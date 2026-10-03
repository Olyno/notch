/// Client SDK for the notch notification relay.
///
/// A [NotchClient] subscribes to one topic over WebSocket, survives flaky
/// networks (persistent cursor + replay + dedup), and applies local [Rule]s
/// before any notification is ever built — so "alert when a player joins,
/// ignore when a player leaves" costs zero alerts *and* zero bandwidth
/// (server-side tag filters are opt-in).
library;

export 'src/client.dart';
export 'src/cursor_store.dart';
export 'src/model.dart';
export 'src/reconnect.dart';
export 'src/rules.dart';
export 'src/transport.dart';
