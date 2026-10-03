//! notch-mcp: a stdio MCP server that lets agents push notifications to the
//! user's Android phone through a notch relay.
//!
//! Speaks both MCP eras:
//! - legacy (<= 2025-11-25): `initialize` handshake, then JSON-RPC requests.
//! - modern (2026-07-28): stateless; any request is answerable directly;
//!   `server/discover` advertises versions/capabilities.
//!
//! Usage: `notch-mcp [http://relay-host:port]` (default
//! `http://localhost:8899`, env `NOTCH_RELAY`).

mod relay;
mod tools;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};

use relay::Relay;

const MODERN: &str = "2026-07-28";
const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-11-25", MODERN];

fn server_info() -> Value {
    json!({ "name": tools::SERVER_NAME, "version": tools::SERVER_VERSION })
}

fn capabilities() -> Value {
    json!({ "tools": { "listChanged": false } })
}

/// The one bit of state: did a legacy handshake happen. Modern clients
/// identify per-request via `_meta` and never initialize.
struct Session {
    initialized: bool,
    relay: Relay,
}

impl Session {
    /// Pure request -> response: no IO, fully unit-testable.
    async fn respond(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned()?; // notifications get no reply
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let result: Result<Value, (i32, String)> = match method {
            "server/discover" => Ok(json!({
                "resultType": "complete",
                "protocolVersion": MODERN,
                "supportedVersions": [MODERN],
                "capabilities": capabilities(),
                "serverInfo": server_info(),
                "_meta": { "io.modelcontextprotocol/serverInfo": server_info() }
            })),
            "initialize" => {
                self.initialized = true;
                let asked = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(MODERN);
                if SUPPORTED.contains(&asked) {
                    Ok(json!({
                        "resultType": "complete",
                        "protocolVersion": asked,
                        "capabilities": capabilities(),
                        "serverInfo": server_info()
                    }))
                } else {
                    Err((-32602, format!("unsupported protocolVersion '{asked}'")))
                }
            }
            "tools/list" => Ok(self.with_server_meta(tools::tools_list())),
            "tools/call" => {
                let modern = msg.get("_meta").is_some() || msg.pointer("/params/_meta").is_some();
                if !(self.initialized || modern) {
                    Err((
                        -32002,
                        "send initialize first (legacy) or _meta (MCP 2026-07-28)".into(),
                    ))
                } else {
                    let name = msg
                        .pointer("/params/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let args = msg
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    tools::call_tool(&self.relay, name, &args)
                        .await
                        .map(|v| self.with_server_meta(v))
                }
            }
            "ping" => Ok(json!({ "resultType": "complete" })),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(v) => ok(id, v),
            Err((code, m)) => err(id, code, m),
        })
    }

    /// Modern servers SHOULD identify themselves on every result.
    fn with_server_meta(&self, mut v: Value) -> Value {
        if let Some(obj) = v.as_object_mut()
            && obj.get("resultType").is_some()
        {
            obj.entry("_meta")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap()
                .insert("io.modelcontextprotocol/serverInfo".into(), server_info());
        }
        v
    }
}

struct Out {
    stdout: Stdout,
}

impl Out {
    async fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        // A stdio MCP server MUST NOT put anything but valid messages on
        // stdout; diagnostics go to stderr. A broken pipe means the client
        // is gone: exit (the supervisor restarts us, the protocol is
        // stateless).
        if self.stdout.write_all(line.as_bytes()).await.is_err()
            || self.stdout.flush().await.is_err()
        {
            eprintln!("notch-mcp: stdout closed");
            std::process::exit(1);
        }
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let url = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("NOTCH_RELAY").ok())
        .unwrap_or_else(|| "http://localhost:8899".to_string());
    let relay = Relay::parse(&url)?;
    eprintln!("notch-mcp: relaying to {url}");

    let mut session = Session {
        initialized: false,
        relay,
    };
    let mut out = Out {
        stdout: tokio::io::stdout(),
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    // EOF on stdin is the graceful-shutdown signal (stdio spec).
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(msg) => {
                if let Some(resp) = session.respond(&msg).await {
                    out.send(resp).await;
                }
            }
            Err(e) => {
                out.send(err(Value::Null, -32700, format!("parse error: {e}")))
                    .await
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            initialized: false,
            relay: Relay {
                host: "127.0.0.1".into(),
                port: 1,
            },
        }
    }

    #[tokio::test]
    async fn discover_advertises_modern() {
        let mut s = session();
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":1,"method":"server/discover"}))
            .await
            .unwrap();
        assert_eq!(resp["result"]["supportedVersions"][0], "2026-07-28");
        assert_eq!(resp["result"]["serverInfo"]["name"], "notch");
    }

    #[tokio::test]
    async fn legacy_handshake_then_tools() {
        let mut s = session();
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}))
            .await
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
            .await
            .unwrap();
        assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 2);
        assert!(s.initialized);
    }

    #[tokio::test]
    async fn unsupported_version_rejected() {
        let mut s = session();
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn call_without_handshake_needs_meta() {
        let mut s = session();
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"poll_messages","arguments":{"topic":"x"}}}))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32002);

        // with per-request _meta (modern client skipping discover) it's fine:
        // reaches the relay (unreachable -> -32000, not -32002)
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"poll_messages","arguments":{"topic":"x"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32000);
    }

    #[tokio::test]
    async fn unknown_method_and_notifications() {
        let mut s = session();
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":7,"method":"bogus"}))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        assert!(
            s.respond(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .is_none()
        );
        // a request with an id but no method: unknown method error
        let resp = s.respond(&json!({"jsonrpc":"2.0","id":"a"})).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn results_carry_server_meta() {
        let mut s = session();
        s.initialized = true;
        let resp = s
            .respond(&json!({"jsonrpc":"2.0","id":8,"method":"tools/list"}))
            .await
            .unwrap();
        assert_eq!(
            resp["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "notch"
        );
    }
}
