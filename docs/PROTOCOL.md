# notch wire protocol v0

Two transports, one JSON message model (`notch-core::Message`). The Rust
implementation (`notch-relay`, `notch-mcp`) and the Dart mirror
(`libs/dart/lib/src/model.dart`) are the reference; this document is kept in
sync with them.

## Message

```json
{
  "seq": 42,                    // relay-assigned, monotonic per topic
  "id": "7f3a9c0d1e2b3c4d",     // 16-hex fnv-1a over topic+seq+title+body
  "topic": "alerts",
  "title": "player joined",
  "body": "Alice entered the lobby",
  "tags": ["join"],             // the only field filters/rules run on
  "priority": "high",           // min | low | default | high | max
  "category": "msg",            // Android notification category hint
  "icon": "person",             // logical icon name, client maps to a drawable
  "silent": false,              // deliver but do not alert
  "ttl": 3600,                  // seconds the relay may keep it (default 86400)
  "timeout_ms": 60000,          // client auto-dismiss, 0 = sticky until tapped
  "dedup_key": "lobby",         // same key replaces, not stacks
  "channel": "game",            // Android channel override (default: topic)
  "ts": 1727900000              // unix seconds, relay-assigned
}
```

- Unknown fields are ignored by both decoders (forward compatible).
- Empty/default optional fields are **omitted on the wire** (serde
  `skip_serializing_if` / Dart conditional map): a typical message is
  ~120–150 bytes end to end.

## Publish — HTTP `POST /t/{topic}`

Body: JSON with any Message fields except `seq`/`id`/`topic`/`ts`.
Topic must match `[A-Za-z0-9_-]{1,64}`. Response `200 {"seq":42}`; bad topic
or JSON → `400`. No auth in v0 — bind the relay to a trusted network
(`x-notch-token` style shared secret is reserved for v1). Plaintext HTTP is
allowed on purpose; TLS belongs to a reverse proxy if one is needed.

## Subscribe — WebSocket `GET /ws/{topic}?since={seq}[&all=…&any=…&none=…]`

`since` is optional (default 0). When given, the relay streams every message
with `seq > since` **before** switching to live mode. This is the
poor-network guarantee, in three moves:

1. The client persists the last processed `seq` durably *before* acking
   (`FileCursorStore`; atomic rename).
2. On any disconnect it reconnects with exponential backoff + full jitter
   (0.5 s → 1 s → 2 s … cap 30 s, never < 200 ms).
3. The server replays the gap from the store. A client may see the same
   message twice (it died after processing but before persisting its
   cursor); it dedups by `seq`, so at-least-once transport + client
   idempotence = exactly-once notifications.

Frame shapes (both JSON, one WS text frame each):

- **Live:** one bare Message object (smallest possible payload).
- **Replay:** pages of up to 200 messages wrapped as
  `{"messages":[ … ]}` — fewer frames over flaky links, one client rebuild
  instead of hundreds.

**Server-side filtering (bandwidth):** `all`/`any`/`none` are comma-separated
tag predicates, e.g. `?any=join&none=leave`. `none` beats `any`; `all`
requires every tag. The relay applies them before sending: a device that
only cares about joins never pays bytes for leaves. Filters never change
`since` semantics — the client's cursor still advances over everything the
server *scanned*, and local rules remain the source of truth for alerting.

**Keepalive:** the server pings every 25 s (first ping deferred one full
interval); pongs from the client reset the liveness clock; silence for >2
intervals + 5 s ⇒ the server drops the socket and the client reconnects with
its cursor. Pings ride the same TCP connection as data — no extra channels,
no extra RTT bursts.

**Backpressure:** each live subscriber gets a bounded queue (400 messages ≈
2 replay pages). A subscriber that cannot drain it (dead link, backgrounded
phone with frozen UI) is killed: its sender is dropped, its socket closed,
and it is removed from the hub immediately. Reconnect + replay makes the
kill lossless. A write that cannot flush within 25 s (half-open TCP) also
ends the connection. The relay itself never blocks on a slow client.

**Acknowledgement:** the client sends `{"ack":N}` after durably persisting
cursor N. The relay prunes a message once the **global ack floor** (min
acked over *live* cursors — ones seen within 2 h) passes it AND it is older
than `min_retention` (600 s), or it exceeds its TTL (default 24 h), or the
topic is over the 5000-message cap. Stale cursors stop pinning the floor,
and cursors older than 7 days are GC'd — a phone that dies never grows the
store. That is the whole poor-network story in one mechanism:
*retain-until-acked, replay-on-reconnect, release-when-dead*.

## HTTP poll (fallback when WS is blocked)

`GET /t/{topic}?since=N[&wait=SECS][&all=…&any=…&none=…]` returns
`{"messages":[ … ]}` — the same shape as the WS replay frame, so constrained
clients share one parser. `wait` (cap 300 s) long-polls: the server holds the
request until a message arrives or the deadline passes (then an empty array,
HTTP 200 — never 204, one response shape).

## MCP server

`notch-mcp` speaks MCP over stdio (newline-delimited JSON-RPC 2.0) and
supports both protocol eras:

- **legacy** (`2025-06-18`, `2025-11-25`): `initialize` handshake, then
  requests.
- **modern** (`2026-07-28`, stateless): `server/discover` probe supported;
  any request answering directly when it carries per-request `_meta`;
  results include `io.modelcontextprotocol/serverInfo`.

Tools:

- `send_notification { topic, title, body?, tags?, priority?, category?,
  silent?, dedup_key?, channel?, icon?, timeout_ms?, ttl? }`
  → `structuredContent { seq }`
- `poll_messages { topic, since?, any?, none?, wait? }`
  → `structuredContent { messages, last_seq }`

`tools/list` is deterministic (stable order) and carries `ttlMs`/`cacheScope`.
The server forwards to the relay over plain HTTP and stores nothing itself:
an agent needs zero network stack beyond its MCP transport. Stdout carries
only valid MCP messages; diagnostics go to stderr; stdin EOF exits cleanly.

## Optimization budget (non-network)

- Relay: tokio multi-thread; one SQLite connection in WAL mode behind a
  parking_lot mutex; messages serialized once at publish and shipped as the
  stored raw JSON string on every fan-out (no re-serialization). Release
  binary 3.9 MB; RSS target < 15 MB with 100 idle subscribers.
- Store: append-only `(topic, seq)` PK index; housekeeping (expiry, acked,
  cap, cursor GC, floor re-age) runs once a minute.
- Client SDK: hand-rolled `dart:convert` JSON; no runtime dependencies
  outside `dart:io`/`dart:convert`; zero reflection/codegen.
- Flutter app: one background isolate owns the socket; notifications are
  handed to the platform channel and rendered natively (Android 13+
  channels/importance). Silent messages never wake the UI.

## Not implemented in v0 (deliberate)

Attachments, email/SMS, web UI, auth, read-receipts beyond ack floors,
WS permessage-deflate negotiation (tungstenite side), HTTP content-encoding.
The protocol leaves the space for them (extra fields ignored, headers
reserved) without breaking anything.
