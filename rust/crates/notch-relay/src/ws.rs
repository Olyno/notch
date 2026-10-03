//! WebSocket subscription endpoint: replay-on-connect (poor-network
//! guarantee), live push with server-side tag filtering, keepalive pings and
//! backpressure-driven disconnect. See docs/PROTOCOL.md.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use notch_core::Filter;
use serde_json::Value;

use crate::AppState;
use crate::hub::KEEPALIVE;
use crate::store::MAX_PAGE;

pub async fn handle(
    ws: WebSocketUpgrade,
    Path(topic): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    let since = params
        .get("since")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let filter = Filter::from_query(|k| params.get(k).cloned());
    let topic_ok = crate::valid_topic(&topic);
    ws.on_upgrade(move |mut socket| async move {
        if !topic_ok {
            let _ = socket.close().await;
            return;
        }
        run(socket, state, topic, since, filter).await;
    })
}

fn tags_of(v: &Value) -> Vec<String> {
    v.get("tags")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A half-open write must never wedge the task: a frame that cannot flush
/// within the keepalive window ends the connection (the client reconnects
/// and replays from its cursor).
const SEND_TIMEOUT: Duration = KEEPALIVE;

async fn send_pending(
    sink: &mut SplitSink<WebSocket, Message>,
    pending: &mut Option<String>,
) -> bool {
    let Some(s) = pending.take() else { return true };
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Text(s.into()))).await,
        Ok(Ok(()))
    )
}

async fn run(socket: WebSocket, state: AppState, topic: String, since: u64, filter: Filter) {
    // Register BEFORE replay: anything published mid-replay lands in the
    // bounded queue and may duplicate a replayed row; the client dedups by
    // seq, so at-least-once transport + idempotent client = exactly-once.
    let (sub, mut rx) = state
        .hub
        .add(topic.clone(), filter.clone(), state.store.clone());
    let (mut sink, mut stream): (SplitSink<WebSocket, Message>, SplitStream<WebSocket>) =
        socket.split();
    let mut cursor = since;
    let mut broken = false;

    // 1. Gap replay in pages; each non-empty page is one batch frame.
    while !broken {
        let page = match state.store.since(&topic, cursor, MAX_PAGE) {
            Ok(p) => p,
            Err(_) => {
                broken = true;
                break;
            }
        };
        let scanned = page.messages.len();
        if scanned == 0 {
            break;
        }
        cursor = page.last_scanned;
        let kept: Vec<&str> = page
            .messages
            .iter()
            .filter(|m| {
                filter.is_empty()
                    || filter.matches(&tags_of(&serde_json::from_str(m).unwrap_or(Value::Null)))
            })
            .map(String::as_str)
            .collect();
        if !kept.is_empty() {
            let frame = format!(r#"{{"messages":[{}]}}"#, kept.join(","));
            if sink.send(Message::Text(frame.into())).await.is_err() {
                broken = true;
            }
        }
        if scanned < MAX_PAGE {
            break;
        }
    }

    // 2. Live: single writer path (one pending frame), reads for acks/pongs,
    // ping on an idle tick. A dead-but-open client is evicted by queue
    // overflow (hub flags it dead and removes it); we stop without draining
    // the queue so eviction is prompt.
    if !broken {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        let mut last_seen = Instant::now();
        let mut pending: Option<String> = None;
        loop {
            if sub.is_dead() {
                break;
            }
            let has_pending = pending.is_some();
            tokio::select! {
                biased;
                ok = send_pending(&mut sink, &mut pending), if has_pending => {
                    if !ok { break }
                }
                j = rx.recv(), if !has_pending => {
                    match j {
                        Some(s) if !sub.is_dead() => pending = Some(s),
                        _ => break,
                    }
                }
                inbound = stream.next() => match inbound {
                    Some(Ok(Message::Text(t))) => {
                        last_seen = Instant::now();
                        sub.handle_inbound(t.as_str());
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                        last_seen = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {} // binary frames: ignore
                },
                _ = ticker.tick(), if !has_pending => {
                    if last_seen.elapsed() > KEEPALIVE * 2 + Duration::from_secs(5) {
                        break; // no inbound activity for >2 intervals
                    }
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
    state.hub.remove(&sub);
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_of_missing() {
        assert!(tags_of(&Value::Null).is_empty());
        assert_eq!(tags_of(&serde_json::json!({"tags": ["a"]})), ["a"]);
    }
}
