/// Minimal abstraction over a WebSocket connection so the client can be
/// tested without `dart:io` and reused on platforms lacking it.
library;

import 'dart:async';

class WsMessage {
  const WsMessage.text(this.text) : isClose = false;
  const WsMessage.close() : text = null, isClose = true;
  final String? text;
  final bool isClose;
}

abstract interface class WsTransport {
  /// Incoming frames: text messages; a terminal close marker on EOF/error.
  Stream<WsMessage> get frames;

  void send(String text);

  Future<void> close();
}

typedef TransportFactory = Future<WsTransport> Function(Uri url);
