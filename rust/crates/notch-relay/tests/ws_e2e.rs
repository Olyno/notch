//! End-to-end: real HTTP + WS over localhost, exercising the poor-network
//! replay path, server-side filtering, the long-poll fallback and the
//! backpressure guarantee with actual sockets.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use notch_relay::{AppState, router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Next *data* frame as a string, transparently skipping ping/pong control
/// frames. Panics on close/timeout after `secs`.
async fn next_data(ws: &mut Ws, secs: u64) -> String {
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return t.to_string(),
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => continue,
            other => panic!("unexpected ws event: {other:?}"),
        }
    }
}

/// Assert no further data frame arrives within `ms` (control frames allowed).
async fn expect_quiet(ws: &mut Ws, ms: u64) {
    let mut remaining = ms;
    while remaining > 0 {
        let started = std::time::Instant::now();
        match tokio::time::timeout(Duration::from_millis(remaining), ws.next()).await {
            Err(_) => return, // the promise holds: still quiet
            Ok(Some(Ok(Message::Text(t)))) => panic!("unexpected data frame: {t}"),
            Ok(Some(Ok(_))) => {
                remaining = remaining.saturating_sub(started.elapsed().as_millis() as u64)
            }
            Ok(other) => panic!("unexpected ws event: {other:?}"),
        }
    }
}

async fn spawn_relay() -> (SocketAddr, AppState) {
    let state = AppState::new(":memory:".into()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for _ in 0..100 {
        if let Ok(body) = req(addr, "GET", "/health", "").await
            && body.trim() == "ok"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (addr, state)
}

/// Tiny raw HTTP/1.1 client: every request closes, response body to EOF.
async fn req(addr: SocketAddr, method: &str, path: &str, body: &str) -> Result<String, String> {
    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| e.to_string())?;
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    sock.read_to_end(&mut out)
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out).to_string();
    text.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .filter(|b| !b.is_empty())
        .ok_or_else(|| format!("empty response: {text}"))
}

async fn publish(addr: SocketAddr, topic: &str, json: &str) -> u64 {
    let resp = req(addr, "POST", &format!("/t/{topic}"), json)
        .await
        .unwrap();
    serde_json::from_str::<serde_json::Value>(&resp).unwrap()["seq"]
        .as_u64()
        .unwrap()
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn replay_after_disconnect_delivers_gap() {
    let (addr, _state) = spawn_relay().await;

    // three messages before anyone listens
    for i in 0..3 {
        publish(
            addr,
            "gaps",
            &format!(r#"{{"title":"m{i}","tags":["keep"]}}"#),
        )
        .await;
    }

    // connect with since=0 -> one batch frame replays all three
    let url = format!("ws://{addr}/ws/gaps?since=0");
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let v = parse(&next_data(&mut ws, 5).await);
    let batch = v["messages"].as_array().unwrap();
    assert_eq!(batch.len(), 3, "replay must deliver all missed messages");
    assert_eq!(batch[0]["seq"].as_u64().unwrap(), 1);

    // live message arrives as a bare frame
    publish(addr, "gaps", r#"{"title":"live"}"#).await;
    let v = parse(&next_data(&mut ws, 5).await);
    assert_eq!(v["seq"].as_u64().unwrap(), 4);
    assert!(v.get("messages").is_none(), "live frames are bare messages");

    // the client dies mid-storm (hard close, cursor stuck at 4)
    drop(ws);
    publish(addr, "gaps", r#"{"title":"missed-a"}"#).await;
    publish(addr, "gaps", r#"{"title":"missed-b"}"#).await;

    // reconnect with cursor -> gap replay only, no duplicates of 1..4
    let url = format!("ws://{addr}/ws/gaps?since=4");
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let v = parse(&next_data(&mut ws, 5).await);
    let batch = v["messages"].as_array().unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0]["seq"].as_u64().unwrap(), 5);
    assert_eq!(batch[1]["seq"].as_u64().unwrap(), 6);
}

#[tokio::test]
async fn server_side_tag_filter() {
    let (addr, _state) = spawn_relay().await;
    let url = format!("ws://{addr}/ws/game?since=0&any=join&none=leave");
    let (mut ws, _) = connect_async(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await; // registration lands

    publish(addr, "game", r#"{"title":"A joins","tags":["join"]}"#).await;
    publish(addr, "game", r#"{"title":"A leaves","tags":["leave"]}"#).await;
    publish(addr, "game", r#"{"title":"chat","tags":["chat"]}"#).await;

    let v = parse(&next_data(&mut ws, 5).await);
    assert_eq!(v["seq"].as_u64().unwrap(), 1, "only the join passes");
    // leave/chat must never reach this socket
    expect_quiet(&mut ws, 300).await;
}

#[tokio::test]
async fn filtered_replay_keeps_cursor_consistent() {
    let (addr, _state) = spawn_relay().await;
    for i in 0..5 {
        publish(
            addr,
            "mix",
            &format!(r#"{{"title":"m{i}","tags":["t{}"]}}"#, i % 2),
        )
        .await;
    }
    let url = format!("ws://{addr}/ws/mix?since=0&all=t0");
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let v = parse(&next_data(&mut ws, 5).await);
    let seqs: Vec<u64> = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 3, 5]);
}

#[tokio::test]
async fn longpoll_fallback_when_ws_blocked() {
    let (addr, _state) = spawn_relay().await;
    let late = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        publish(addr, "lp", r#"{"title":"late","tags":["x"]}"#).await
    });
    let v = parse(&req(addr, "GET", "/t/lp?since=0&wait=5", "").await.unwrap());
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["title"].as_str().unwrap(), "late");
    late.await.unwrap();

    // timeout with nothing new returns an empty batch, not an error
    let v = parse(&req(addr, "GET", "/t/lp?since=1&wait=1", "").await.unwrap());
    assert!(v["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn wire_size_is_tiny() {
    // The headline claim: a typical message crosses the wire in ~150 bytes.
    let (addr, _state) = spawn_relay().await;
    publish(addr, "small", r#"{"title":"hi"}"#).await;
    let body = req(addr, "GET", "/t/small?since=0", "").await.unwrap();
    assert!(!body.contains("null"), "empty fields must be omitted");
    assert!(body.len() < 250, "poll body was {} bytes", body.len());
}

#[tokio::test]
async fn backpressure_kills_slow_client_without_stalling_others() {
    let (addr, state) = spawn_relay().await;

    // slow subscriber: socket open, never polled (poor network / dead link)
    let url = format!("ws://{addr}/ws/flood?since=999999");
    let (slow, _resp) = connect_async(&url).await.unwrap();
    // fast subscriber: actively polled by a task for the whole storm
    let (mut fastrx, fast) = connect_async(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(state.hub.count("flood"), 2);

    let got = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let got = got.clone();
        tokio::spawn(async move {
            while let Some(Ok(Message::Text(_))) = fastrx.next().await {
                got.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }

    // flood 2000 fat (~8 KB) messages through the real HTTP path
    let fat = format!(r#"{{"title":"{}"}}"#, "x".repeat(8 * 1024));
    for i in 0..2000u32 {
        let seq = publish(addr, "flood", &fat).await;
        assert_eq!(seq, i as u64 + 1);
    }

    // The blocked slow subscriber must be gone from the hub (queue overflow),
    // the fast one must have kept receiving, and the relay stays responsive.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while state.hub.count("flood") > 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        state.hub.count("flood"),
        1,
        "slow subscriber must be dropped"
    );
    assert!(
        got.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "fast client starved"
    );
    assert_eq!(req(addr, "GET", "/health", "").await.unwrap().trim(), "ok");
    drop(slow);
    drop(fast);
}
