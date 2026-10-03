//! HTTP API: publish, poll (with long-poll fallback for networks where WS
//! is blocked), health.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use notch_core::{Batch, Message, Publish};
use serde_json::json;
use tokio::time::{Duration, timeout};

use crate::AppState;
use crate::store::MAX_PAGE;

/// `POST /t/{topic}` — publish one message; returns its assigned seq.
pub async fn publish(
    State(state): State<AppState>,
    Path(topic): Path<String>,
    body: String,
) -> Response {
    if !crate::valid_topic(&topic) {
        return (StatusCode::BAD_REQUEST, "invalid topic").into_response();
    }
    let p: Publish = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad json: {e}")).into_response(),
    };
    match state.store.publish(&topic, &p) {
        Ok(pub_) => {
            state.hub.notify(&topic, &pub_.json, &pub_.message.tags);
            (StatusCode::OK, Json(json!({ "seq": pub_.message.seq }))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// `GET /t/{topic}?since=N[&wait=SECS][&all=a,b&any=..&none=..]`
/// Short poll returns immediately; `wait` long-polls until a message arrives
/// or the deadline hits. The response is always a JSON array (possibly empty)
/// — simple for constrained clients: one decode shape.
pub async fn poll(
    State(state): State<AppState>,
    Path(topic): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !crate::valid_topic(&topic) {
        return (StatusCode::BAD_REQUEST, "invalid topic").into_response();
    }
    let since = params
        .get("since")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let wait = params
        .get("wait")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        .min(300);
    let filter = notch_core::Filter::from_query(|k| params.get(k).cloned());

    let mut attempts = 1u8;
    loop {
        let page = match state.store.since(&topic, since, MAX_PAGE) {
            Ok(p) => p,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        };
        let messages: Vec<Message> = page
            .messages
            .iter()
            .filter_map(|m| serde_json::from_str::<Message>(m).ok())
            .filter(|m| filter.matches(&m.tags))
            .collect();
        if !messages.is_empty() {
            // Frame shape matches the WS batch frame: clients share one parser.
            return Json(&Batch { messages }).into_response();
        }
        if attempts > 1 || wait == 0 {
            return Json(&Batch { messages: vec![] }).into_response();
        }
        attempts += 1;
        // Long-poll: re-check the store on every hub wake; on timeout, reply.
        let deadline = Duration::from_secs(wait);
        if timeout(deadline, hub_wait_matching(&state, &topic, since))
            .await
            .is_err()
        {
            return Json(&Batch { messages: vec![] }).into_response();
        }
    }
}

/// Wait until something after `since` exists (spurious wakes re-loop).
async fn hub_wait_matching(state: &AppState, topic: &str, since: u64) {
    loop {
        state.hub.wait().await;
        if let Ok(p) = state.store.since(topic, since, 1)
            && !p.messages.is_empty()
        {
            return;
        }
    }
}

pub async fn health() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> AppState {
        AppState::new(":memory:".into()).unwrap()
    }

    async fn get(state: AppState, uri: &str) -> Response {
        crate::router(state)
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn publish_then_poll() {
        let state = app();
        let res = crate::router(state.clone())
            .oneshot(
                Request::post("/t/alerts")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"hi","tags":["join"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = get(state.clone(), "/t/alerts?since=0").await;
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        // filtered
        let res = get(state.clone(), "/t/alerts?since=0&none=join").await;
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["messages"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn longpoll_returns_on_publish() {
        let state = app();
        let late = {
            let s = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let p: Publish = serde_json::from_str(r#"{"title":"late"}"#).unwrap();
                s.store.publish("t", &p).unwrap();
                s.hub.notify("t", "{}", &[]);
            })
        };
        let res = get(state.clone(), "/t/t?since=0&wait=5").await;
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        late.await.unwrap();
    }

    #[tokio::test]
    async fn longpoll_times_out_empty() {
        let state = app();
        let res = get(state.clone(), "/t/t?since=0&wait=1").await;
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["messages"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_bad_topic_and_body() {
        let state = app();
        let res = crate::router(state.clone())
            .oneshot(
                Request::post("/t/has.space")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = crate::router(state)
            .oneshot(
                Request::post("/t/ok")
                    .header("content-type", "application/json")
                    .body(Body::from("nope"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
