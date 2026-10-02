# notch wire protocol v0

Two transports, one JSON message model (`notch-core::Message`).

## Message

```json
{
  "seq": 42,                    // assigned by relay, monotonic per topic
  "id": "7f3a…",                // 8-hex blake3 of body, client-side dedup
  "topic": "alerts",
  "title": "player joined",
  "body": "Alice entered the lobby",
  "tags": ["join"],             // client rules filter on these
  "priority": "high",           // min | low | default | high | max
  "category": "msg",            // Android notification category
  "icon": "person_add",         // optional logical icon name (client maps)
  "silent": false,              // deliver but do not alert (badge/list only)
  "ttl": 3600,                  // seconds the relay may keep it, default 86400
  "timeout_ms": 60000,          // client auto-dismiss, 0 = sticky until tapped
  "dedup_key": "lobby",         // replaces prior notification with same key
  "channel": "game",            // Android notification channel override
  "ts": 1727900000              // unix seconds, relay-assigned
}
```

Unknown fields are ignored by clients (forward compatible). Empty/`null`
optional fields are **omitted on the wire** — serde `skip_serializing_if`,
the payload for a typical message is ~120 bytes.

## Publish — HTTP `POST /t/{topic}`

Body: JSON with any Message fields except `seq`/`id`/`topic`/`ts`.
Headers: nothing required. Response `200` → `{"seq":42}`.

Optimizations:
- `Content-Encoding: zstd`/`br` accepted for fat payloads (rare; text is small).
- Plaintext HTTP is allowed on purpose (LAN/self-host). TLS via reverse proxy.
- No auth in v0; a shared-secret header (`x-notch-token`) check is reserved.

## Subscribe — WebSocket `GET /ws/{topic}?since={seq}`

`since` is optional: if given, the relay streams every message with
`seq > since` **before** switching to live mode. This is the poor-network
guarantee:

1. Client stores last seen `seq` persistently (SQLite/shared prefs).
2. On disconnect, reconnects with exponential backoff + full jitter
   (1s → 2 → 4 … cap 30s, ±50% jitter).
3. Server replays the gap from the store. The client may see the *same*
   message twice (if it died before persisting its cursor); it dedups by
   `seq` (and `id`), so at-least-once transport + client idempotence =
   exactly-once notifications.

Live frames are one message JSON per WS text frame.

**Batching:** if the client was offline long, the relay replays in pages of
up to 200 messages wrapped in `{"type":"batch","messages":[…]}` frames, so a
flaky link transfers fewer frames and the client renders one rebuild instead
of hundreds.

**Filtering server-side (bandwidth):** subscribe may carry `all` / `any` /
`none` tag predicates — `GET /ws/alerts?since=7&any=join&none=leave`.
The relay evaluates them against `tags` before sending; a device that only
cares about joins never pays bytes for leaves. Filters are an optimization:
they remove messages from the stream but MUST NOT change `since` semantics —
the cursor is the highest *sent* seq, and the client's own rules remain the
source of truth for alerting.

**Keepalive:** server sends WS ping every 25s; client pong is automatic.
If no pong within 2 intervals, server drops the connection; client
reconnects with its cursor. Pings ride the same TCP connection as data —
no separate heartbeat channel, no extra RTT bursts.

**Compression:** WS permessage-deflate is requested by clients; text JSON
compresses ~3.5:1 (repeated field names dominate small messages).

**Acknowledgement:** client sends `{"ack":42}` whenever it persists cursor
42. The relay prunes a message from the store once **all** current
subscribers have acked past it AND it exceeds `min_retention` (default 10 min,
default TTL 24 h, hard cap 5000 messages/topic). Offline subscribers are
served from the store when they return within retention — that is the whole
poor-network story in one mechanism: *retain-until-acked, replay-on-reconnect*.

HTTP fallback (for networks where WS is blocked, e.g. some captive
portals): `GET /t/{topic}?since=42` returns up to 200 missed messages as a
JSON array; clients long-poll it with `?since=42&wait=60` (server holds the
request open until a message arrives, `204` on timeout).

## MCP server

`notch-mcp` speaks MCP over stdio (JSON-RPC 2.0, spec 2025-06-18) and exposes
one tool:

- `send_notification { topic, title, body, tags?, priority?, category?,
  silent?, dedup_key?, channel?, icon?, timeout_ms?, ttl? } → { seq }`
  plus `poll_messages { topic, since?, any?, none?, limit? } → { messages,
  last_seq }` for agents that want to read.

It forwards to the relay over HTTP and does no storage itself. An agent
therefore needs zero network stack beyond its MCP transport.

## Optimization budget (non-network)

- Relay: one thread per core (tokio), SQLite in WAL mode, single writer task;
  messages are `Arc<[u8]>` (serialized once at publish, cloned per subscriber).
  Static RSS target < 15 MB with 100 idle subscribers.
- Store is append-only per topic with seq index (`topic, seq` PK); pruning is
  a `DELETE … WHERE seq <= global_ack_floor AND ts < now()-ttl` every 60s.
- Client SDK: no reflection/codegen JSON, hand-rolled serde; reconnect logic
  is < 200 lines; zero dependencies outside `dart:io`/`dart:convert`.
- Flutter app: single isolate for networking; notifications handed to
  platform channel; `silent` + `min` priority messages never wake the UI.
