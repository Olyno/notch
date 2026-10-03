//! Drive the real notch-mcp binary over stdio like a real MCP client:
//! fake relay on a random port, JSON-RPC lines in, JSON-RPC lines out.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Fake relay answering any request with `{"seq":99}`; counts requests.
async fn fake_relay() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits2 = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let hits = hits2.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let Ok(n) = sock.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"seq\":99}",
                    )
                    .await;
            });
        }
    });
    (format!("http://{addr}"), hits)
}

struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: BufReader<tokio::process::ChildStdout>,
}

impl Client {
    async fn spawn(relay_url: &str) -> Client {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_notch-mcp"))
            .arg(relay_url)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let stdin = cmd.stdin.take().unwrap();
        let stdout = cmd.stdout.take().unwrap();
        Client {
            child: cmd,
            stdin,
            lines: BufReader::new(stdout),
        }
    }

    async fn ask(&mut self, id: u64, method: &str, params: Option<Value>) -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
        // Skip unrelated lines (e.g. a parse-error response with null id).
        loop {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), self.lines.read_line(&mut line))
                .await
                .expect("server response")
                .unwrap();
            let v: Value = serde_json::from_str(&line).expect("valid JSON-RPC line on stdout");
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }

    async fn shutdown(self) {
        // dropping stdin closes the pipe -> server sees EOF -> exits
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("server must exit on stdin EOF")
            .unwrap();
    }
}

#[tokio::test]
async fn legacy_session_full_roundtrip() {
    let (url, hits) = fake_relay().await;
    let mut c = Client::spawn(&url).await;

    // initialize
    let r = c
        .ask(
            1,
            "initialize",
            Some(json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}})),
        )
        .await;
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "notch");

    // initialized notification: no response line must be produced, so the
    // next request's response is well-formed:
    c.stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();

    // tools/list
    let r = c.ask(2, "tools/list", None).await;
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "send_notification");

    // tools/call send_notification -> relay hit
    let r = c
        .ask(
            3,
            "tools/call",
            Some(json!({"name":"send_notification","arguments":{"topic":"alerts","title":"hi","tags":["join"],"priority":"high"}})),
        )
        .await;
    assert_eq!(r["result"]["structuredContent"]["seq"], 99);
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);

    c.shutdown().await;
}

#[tokio::test]
async fn modern_stateless_flow() {
    let (url, hits) = fake_relay().await;
    let mut c = Client::spawn(&url).await;

    // discover (the modern probe)
    let r = c.ask(1, "server/discover", None).await;
    assert!(
        r["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );

    // straight to tools/call with per-request _meta: must work without any
    // handshake (MCP 2026-07-28 is stateless)
    let r = c
        .ask(
            2,
            "tools/call",
            Some(json!({
                "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"},
                "name": "send_notification",
                "arguments": {"topic": "alerts", "title": "modern"}
            })),
        )
        .await;
    assert_eq!(r["result"]["structuredContent"]["seq"], 99);
    assert_eq!(
        r["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "notch"
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);

    c.shutdown().await;
}

#[tokio::test]
async fn garbage_line_gets_parse_error_not_death() {
    let (url, _hits) = fake_relay().await;
    let mut c = Client::spawn(&url).await;
    c.stdin.write_all(b"this is not json\n").await.unwrap();
    let r = c.ask(9, "tools/list", None).await; // after garbage, still alive
    assert_eq!(
        r["id"], 9,
        "the server must recover and answer the next request"
    );
    c.shutdown().await;
}

#[tokio::test]
async fn relay_down_surfaces_as_tool_error() {
    // point at a closed port
    let mut c = Client::spawn("http://127.0.0.1:1").await;
    let r = c
        .ask(
            1,
            "tools/call",
            Some(json!({"name":"send_notification","arguments":{"topic":"alerts","title":"x"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}})),
        )
        .await;
    assert_eq!(r["error"]["code"], -32000);
    assert!(r["error"]["message"].as_str().unwrap().contains("relay"));
    c.shutdown().await;
}
