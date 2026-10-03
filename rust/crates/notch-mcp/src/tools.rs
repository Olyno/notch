//! MCP surface: the two tools agents use, plus their JSON Schemas.

use serde_json::{Value, json};

use crate::relay::Relay;

pub const SERVER_NAME: &str = "notch";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tool definitions, in stable order (spec: deterministic tools/list aids
/// client-side prompt caching).
pub fn tools_list() -> Value {
    json!({
        "resultType": "complete",
        "tools": [
            {
                "name": "send_notification",
                "description": "Send a realtime push notification to the user's Android phone via the notch relay. Optional fields control how the phone presents it: priority maps to channel importance (min/low = silent background, high/max = heads-up + sound), tags drive the user's client-side alert rules (e.g. tag a game event 'join' vs 'leave'), silent=true delivers without sound/vibration, dedup_key replaces a previous notification sharing that key (progress updates), timeout_ms auto-dismisses, channel groups notifications on the phone, icon/category pick the visual style.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "Subscription topic the user's phone listens to (e.g. 'alerts', 'game-server')." },
                        "title": { "type": "string", "minLength": 1, "maxLength": 200, "description": "Notification title." },
                        "body": { "type": "string", "maxLength": 4000, "description": "Notification body text." },
                        "tags": { "type": "array", "items": { "type": "string", "maxLength": 32 }, "maxItems": 16, "description": "Classification labels; the user's phone filters on these to decide whether to alert (e.g. ['join'] vs ['leave'])." },
                        "priority": { "type": "string", "enum": ["min", "low", "default", "high", "max"], "description": "Android importance hint. Default: 'default'." },
                        "silent": { "type": "boolean", "description": "Deliver to the notification shade without sound/vibration/heads-up even at high priority." },
                        "dedup_key": { "type": "string", "maxLength": 64, "description": "Notifications with the same key replace each other instead of stacking (use for 'battery now 80%' style updates)." },
                        "channel": { "type": "string", "maxLength": 64, "description": "Android notification channel override; empty = channel per topic." },
                        "category": { "type": "string", "maxLength": 32, "description": "Android notification category, e.g. 'msg', 'call', 'event', 'workout', 'navigation'." },
                        "icon": { "type": "string", "maxLength": 32, "description": "Logical icon name: info, warn, error, check, message, person, game, timer, battery, network, lock." },
                        "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 86400000, "description": "Auto-dismiss after this many ms; 0 = sticky until tapped. Default: 0." },
                        "ttl": { "type": "integer", "minimum": 1, "maximum": 604800, "description": "Seconds the relay keeps the message for offline phones. Default: 86400." }
                    },
                    "required": ["topic", "title"]
                },
                "outputSchema": {
                    "type": "object",
                    "properties": { "seq": { "type": "integer", "description": "Relay-assigned sequence number; proof the message is stored and queued for delivery." } },
                    "required": ["seq"]
                }
            },
            {
                "name": "poll_messages",
                "description": "Read messages on a topic since a sequence number (the user's relay is the source of truth). Use 'wait' up to 60 for long-polling on slow links.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$" },
                        "since": { "type": "integer", "minimum": 0, "description": "Return messages with seq greater than this. 0 = everything retained." },
                        "any": { "type": "array", "items": { "type": "string" }, "maxItems": 16, "description": "Only messages carrying at least one of these tags." },
                        "none": { "type": "array", "items": { "type": "string" }, "maxItems": 16, "description": "Exclude messages carrying any of these tags." },
                        "wait": { "type": "integer", "minimum": 0, "maximum": 60, "description": "Seconds to hold the request open until a message arrives. 0 = immediate." }
                    },
                    "required": ["topic"]
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "messages": { "type": "array", "description": "Full message objects, oldest first." },
                        "last_seq": { "type": "integer", "description": "Pass as 'since' on the next call." }
                    },
                    "required": ["messages", "last_seq"]
                }
            }
        ],
        // CacheableResult fields (MCP 2026-07-28); legacy clients ignore them.
        "ttlMs": 300000,
        "cacheScope": "public"
    })
}

/// Handle `tools/call`; returns either a CallToolResult or a JSON-RPC error
/// string. Never panics on bad arguments: they surface as MCP tool errors.
pub async fn call_tool(relay: &Relay, name: &str, args: &Value) -> Result<Value, (i32, String)> {
    match name {
        "send_notification" => send(relay, args).await,
        "poll_messages" => poll(relay, args).await,
        _ => Err((-32602, format!("unknown tool '{name}'"))),
    }
}

fn bad(msg: impl Into<String>) -> (i32, String) {
    (-32602, msg.into())
}

async fn send(relay: &Relay, args: &Value) -> Result<Value, (i32, String)> {
    let topic = args
        .get("topic")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("topic required"))?;
    if !notch_core::valid_topic(topic) {
        return Err(bad("topic must match [A-Za-z0-9_-]{1,64}"));
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("title required"))?;
    let mut msg = json!({ "title": title });
    for key in ["body", "category", "icon", "dedup_key", "channel"] {
        if let Some(v) = args.get(key).and_then(Value::as_str) {
            msg[key] = json!(v);
        }
    }
    if let Some(tags) = args.get("tags").and_then(Value::as_array) {
        msg["tags"] = json!(tags);
    }
    if let Some(p) = args.get("priority").and_then(Value::as_str) {
        if !matches!(p, "min" | "low" | "default" | "high" | "max") {
            return Err(bad(format!("invalid priority '{p}'")));
        }
        msg["priority"] = json!(p);
    }
    for key in ["silent"] {
        if let Some(v) = args.get(key).and_then(Value::as_bool) {
            msg[key] = json!(v);
        }
    }
    for key in ["timeout_ms", "ttl"] {
        if let Some(v) = args.get(key).and_then(Value::as_u64) {
            msg[key] = json!(v);
        }
    }
    let body = msg.to_string();
    let resp = relay
        .publish(topic, &body)
        .await
        .map_err(|e| (-32000, format!("relay publish failed: {e}")))?;
    let seq: Value =
        serde_json::from_str(&resp).map_err(|e| (-32000, format!("bad relay response: {e}")))?;
    Ok(json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": format!("sent to '{topic}' (seq {})", seq["seq"]) }],
        "structuredContent": { "seq": seq["seq"] },
        "isError": false
    }))
}

async fn poll(relay: &Relay, args: &Value) -> Result<Value, (i32, String)> {
    let topic = args
        .get("topic")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("topic required"))?;
    if !notch_core::valid_topic(topic) {
        return Err(bad("topic must match [A-Za-z0-9_-]{1,64}"));
    }
    let since = args.get("since").and_then(Value::as_u64).unwrap_or(0);
    let wait = args
        .get("wait")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(60);
    let mut path = format!("/t/{topic}?since={since}");
    for key in ["any", "none"] {
        if let Some(list) = args.get(key).and_then(Value::as_array) {
            let csv = list
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            if !csv.is_empty() {
                path.push_str(&format!("&{key}={csv}"));
            }
        }
    }
    if wait > 0 {
        path.push_str(&format!("&wait={wait}"));
    }
    let resp = relay
        .poll(&path)
        .await
        .map_err(|e| (-32000, format!("relay poll failed: {e}")))?;
    let v: Value =
        serde_json::from_str(&resp).map_err(|e| (-32000, format!("bad relay response: {e}")))?;
    let messages = v
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_seq = messages
        .iter()
        .filter_map(|m| m.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(since);
    Ok(json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": format!("{last_seq}") }],
        "structuredContent": { "messages": messages, "last_seq": last_seq },
        "isError": false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_valid_json_schema_shapes() {
        let v = tools_list();
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
        }
    }

    #[tokio::test]
    async fn unknown_tool_rejected() {
        let relay = Relay {
            host: "127.0.0.1".into(),
            port: 1,
        };
        let err = call_tool(&relay, "nope", &json!({})).await.unwrap_err();
        assert_eq!(err.0, -32602);
    }

    #[tokio::test]
    async fn bad_topic_and_priority_rejected_without_network() {
        let relay = Relay {
            host: "127.0.0.1".into(),
            port: 1,
        };
        assert!(
            call_tool(
                &relay,
                "send_notification",
                &json!({"topic": "a b", "title": "t"})
            )
            .await
            .is_err()
        );
        assert!(
            call_tool(
                &relay,
                "send_notification",
                &json!({"topic": "a", "title": "t", "priority": "urgent"})
            )
            .await
            .is_err()
        );
        assert!(
            call_tool(&relay, "poll_messages", &json!({"topic": "a b"}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn send_serializes_android_fields() {
        // fake relay echoes the request body
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req = String::from_utf8_lossy(&buf).into_owned();
            let body = req.split_once("\r\n\r\n").unwrap().1;
            assert_eq!(
                body,
                r#"{"channel":"game","silent":true,"tags":["join"],"timeout_ms":5000,"title":"hi"}"#,
            );
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"seq\":12}").await;
        });
        let relay = Relay {
            host: addr.ip().to_string(),
            port: addr.port(),
        };
        let out = call_tool(
            &relay,
            "send_notification",
            &json!({"topic": "g", "title": "hi", "channel": "game", "tags": ["join"], "silent": true, "timeout_ms": 5000}),
        )
        .await
        .unwrap();
        assert_eq!(out["structuredContent"]["seq"], 12);
        assert_eq!(out["resultType"], "complete");
    }
}
