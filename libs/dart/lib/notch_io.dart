/// `dart:io` WebSocket transport for [NotchClient] (VM, server, Flutter).
///
///     import 'package:notch/notch.dart';
///     import 'package:notch/notch_io.dart';
///
///     final client = NotchClient(
///       topic: 'alerts',
///       relayUrl: Uri.parse('http://192.168.1.10:8899'),
///       rules: [Rule.onTags(['leave'], Effect.ignore)],
///       cursors: FileCursorStore('/data/notch.json'),
///       transport: ioTransport,
///     );
library;

export 'src/io_websocket.dart' show ioTransport;
export 'src/transport.dart' show TransportFactory, WsMessage, WsTransport;
