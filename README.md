# notch

A tiny self-hosted push-notification system: an agent (via MCP) publishes a
message, a Rust relay stores and streams it, and an Android client renders it
as a native notification — over plain WebSocket, no FCM, no Google dependency.

Inspired by [gotify](https://github.com/gotify/server) and
[ntfy](https://github.com/binwiederhier/ntfy), redesigned from scratch for:

- **Poor networks** — every message gets a monotonic `seq`; clients reconnect
  with a cursor and the relay replays exactly what was missed. Nothing is ever
  lost while the app is alive; the relay keeps messages until acknowledged.
- **Device-specific control** — the Dart library models Android 13+ primitives
  (channels, categories, importance, silent/background tags, `onlyAlertOnce`,
  timeouts, dedup keys). Rules live client-side: *"alert when a player joins,
  ignore when a player leaves"* is a filter on message tags, evaluated before
  the notification is ever built.
- **Size** — single small Rust binary (relay + MCP server), no web UI, no auth
  system, no attachments, no email/SMS. One job, done well.

## Layout

```
rust/               Cargo workspace
  crates/
    notch-core/     shared message model, protocol types, filtering
    notch-relay/    HTTP publish API + WebSocket subscribe + SQLite store
    notch-mcp/      MCP stdio server used by agents to send notifications
libs/dart/          `notch` client SDK (pure Dart, works in CLI and Flutter)
apps/android/       Flutter app: foreground service + native notifications
docs/PROTOCOL.md    wire format (the source of truth)
```

## Quick start

```sh
# relay (stores in ./notch.db, listens on :8899)
cargo run -p notch-relay

# send a test message
curl -X POST localhost:8899/t/alerts \
  -H 'content-type: application/json' \
  -d '{"title":"player joined","tags":["join"],"priority":"high"}'

# agent integration: point your MCP client at notch-mcp (stdio);
# it forwards to the relay over HTTP.
cargo run -p notch-mcp -- http://localhost:8899
```

The Android app is in `apps/android` (`flutter run`). It subscribes to
`ws://<relay>/ws/alerts` and applies the filter rules configured in-app.

## Design notes

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format and the
delivery-guarantee model (seq + cursor + ack + replay).
