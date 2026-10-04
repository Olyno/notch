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
  (channels, categories, importance, silent delivery, auto-dismiss timeouts,
  dedup keys, tap deep links to the sender's own app). Rules live client-side:
  *"alert when a player joins, ignore
  when a player leaves"* is a filter on message tags, evaluated before the
  notification is ever built — and opt-in server-side tag filters make the
  ignored messages cost zero bytes on the wire.
- **Size** — two tiny Rust binaries (relay 3.9 MB, MCP server 0.9 MB), no web
  UI, no auth, no attachments, no email/SMS. One job, done well.

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

## Install from binaries (no toolchain needed)

Grab the latest [release](releases). Each archive contains both
daemons (`notch-relay`, `notch-mcp`) plus LICENSE/PROTOCOL; Linux builds are
fully static (musl) — download and run, no dependencies:

**Always run: the relay. That's the whole server.** Everything else is
optional and hangs off it:

```sh
tar xzf notch-linux-x86_64.tar.gz && cd notch-linux-x86_64
./notch-relay                     # :8899, stores ./notch.db — or run as a service below

# NOTCH-MCP IS NOT A DAEMON — never start it yourself. Your agent/IDE spawns
# it as a child process when it loads the notch tools, speaks MCP to it over
# its stdin/stdout (stdio), and notch-mcp turns each tool call into an HTTP
# POST to the relay. Without an MCP client, you don't need it at all:
curl -X POST 127.0.0.1:8899/t/alerts \
  -H 'content-type: application/json' -d '{"title":"hi"}'   # plain HTTP works directly
```

**Run the relay as a service** (`/etc/systemd/system/notch-relay.service`):

```ini
[Unit]
Description=notch relay
After=network.target

[Service]
ExecStart=/opt/notch/notch-relay
Environment=NOTCH_ADDR=0.0.0.0:8899
Environment=NOTCH_DB=/var/lib/notch/notch.db
DynamicUser=yes
StateDirectory=notch
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
sudo mkdir -p /opt/notch && sudo cp notch-relay notch-mcp /opt/notch/
sudo systemctl enable --now notch-relay
```

Defaults if env is unset: `NOTCH_ADDR=0.0.0.0:8899`, `NOTCH_DB=./notch.db`,
MCP target `http://localhost:8899` (also settable via `NOTCH_RELAY` or the
first CLI argument). Open port 8899 (or your reverse proxy) so the phone can
reach the relay.

**Agent (MCP) config** — point any MCP client at the binary:

```json
{
  "mcpServers": {
    "notch": { "command": "/opt/notch/notch-mcp", "args": ["http://127.0.0.1:8899"] }
  }
}
```

**Phone** — install `app-arm64-v8a-release.apk` (modern Pixels/phones) or
`app-armeabi-v7a-release.apk` (older 32-bit) from the release page — via
`adb install app-arm64-v8a-release.apk` or download-and-tap (allow "unknown
apps" once). Open the app, set the relay to `ws://<your-server>:8899/ws/<topic>`,
pick the topic, start the listener. Release APKs are signed with Flutter's
debug key — fine for sideload; a Play/production signing key is a repo secret
TODO.

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

## Tests

```sh
cd rust && cargo test --workspace        # relay + mcp: unit + ws e2e
cd libs/dart && dart test                # SDK: model/rules/backoff/client
./scripts/xtest.sh                       # cross-language: real binaries,
                                         # replay across a relay outage
cd apps/android && flutter test          # settings UI
cd apps/android && flutter run           # needs a device (see below)
```

The xtest script builds the release binaries first; the cross-language suite
proves the two independent implementations agree on the wire: MCP agent ->
relay -> Dart client, including dedup, cursor persistence, server-side tag
filters and replay after the relay dies and restarts on the same origin.

## Design notes

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format and the
delivery-guarantee model (seq + cursor + ack + replay).
