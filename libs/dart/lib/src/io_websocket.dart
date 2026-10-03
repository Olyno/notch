/// `dart:io` implementation of [WsTransport]. Isolated in its own file so
/// tests and (later) web/non-IO embeds can swap it out.
library;

import 'dart:async';
import 'dart:io' as io;

import 'transport.dart';

/// Connect an IO WebSocket to `ws(s)://…/ws/<topic>?…`.
Future<WsTransport> ioTransport(Uri url) async =>
    _Io(await io.WebSocket.connect(url.toString()));

class _Io implements WsTransport {
  _Io(this._socket);
  final io.WebSocket _socket;

  @override
  Stream<WsMessage> get frames => _socket
      .map((event) => WsMessage.text(event as String))
      .cast<WsMessage>()
      .transform(
        StreamTransformer.fromHandlers(
          handleDone: (sink) {
            sink.add(const WsMessage.close());
            sink.close();
          },
        ),
      );

  @override
  void send(String text) => _socket.add(text);

  @override
  Future<void> close() => _socket.close();
}
