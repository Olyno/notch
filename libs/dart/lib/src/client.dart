/// The client: one topic, one socket, endless resilience.
///
/// Delivery contract (docs/PROTOCOL.md): the transport is at-least-once
/// (the cursor persists *before* the ack, replay can re-deliver), the client
/// makes it exactly-once by deduping on `seq`.
library;

import 'dart:async';
import 'dart:convert';

import 'cursor_store.dart';
import 'model.dart';
import 'reconnect.dart';
import 'rules.dart';
import 'transport.dart';

/// A decided message: what arrived + what the rules said to do about it.
class NotchEvent {
  NotchEvent(this.message, this.decision);
  final Message message;
  final Decision decision;
}

class NotchClient {
  NotchClient({
    required this.topic,
    required Uri relayUrl,
    required this.rules,
    required this.cursors,
    TransportFactory? transport,
    Backoff? backoff,

    /// Server-side tag filters: spend zero bandwidth on messages the local
    /// rules would ignore anyway. Opt-in — rules remain the source of truth.
    List<String>? anyTags,
    List<String>? noneTags,
    List<String>? allTags,
  }) : _url = relayUrl,
       _transport = transport,
       _backoff = backoff ?? Backoff(),
       _any = anyTags,
       _none = noneTags,
       _all = allTags,
       _engine = RuleEngine(rules);

  final String topic;
  final Uri _url;
  final List<Rule> rules;
  final CursorStore cursors;
  final TransportFactory? _transport;
  final Backoff _backoff;
  final RuleEngine _engine;
  final List<String>? _any;
  final List<String>? _none;
  final List<String>? _all;

  final _events = StreamController<NotchEvent>.broadcast();
  final _status = StreamController<bool>.broadcast();

  WsTransport? _live;
  StreamSubscription<WsMessage>? _sub;
  Timer? _retry;
  bool _running = false;
  bool _connected = false;
  int _lastSeq = 0; // highest seq already dispatched to listeners

  /// Decided messages, in seq order. Each message appears exactly once.
  Stream<NotchEvent> get events => _events.stream;

  /// Connection state (true = live socket), for UI badges / battery-aware
  /// scheduling (e.g. WorkManager fallback while false).
  Stream<bool> get connectionStatus => _status.stream;

  /// The subscribe URL currently in use (diagnostics/tests).
  Uri get subscribeUrl => _subscribeUrl();

  bool get isConnected => _connected;

  Future<void> start() async {
    if (_running) return;
    _running = true;
    _lastSeq = await cursors.load(topic);
    _connect();
  }

  void _connect() {
    final factory = _transport;
    if (!_running || factory == null) return;
    unawaited(
      factory(_subscribeUrl()).then((t) {
        if (!_running) {
          unawaited(t.close());
          return;
        }
        _live = t;
        _setConnected(true);
        _backoff.reset();
        _sub = t.frames.listen(_onFrame, onError: (_) => _onDisconnect(t));
      }, onError: (Object _) => _onDisconnect(null)),
    );
  }

  Uri _subscribeUrl() {
    final q = <String, String>{'since': '$_lastSeq'};
    if (_any != null && _any.isNotEmpty) q['any'] = _any.join(',');
    if (_none != null && _none.isNotEmpty) q['none'] = _none.join(',');
    if (_all != null && _all.isNotEmpty) q['all'] = _all.join(',');
    // http->ws, https->wss; the path is always /ws/<topic>. dart:io's
    // WebSocket.connect does not apply default ports, so fill them in.
    final secure = _url.scheme == 'https' || _url.scheme == 'wss';
    return Uri(
      scheme: secure ? 'wss' : 'ws',
      host: _url.host,
      port: _url.hasPort ? _url.port : (secure ? 443 : 80),
      path: '/ws/$topic',
      queryParameters: q,
    );
  }

  void _onFrame(WsMessage msg) {
    if (msg.isClose) {
      _onDisconnect(_live);
      return;
    }
    final decoded = decodeFrame(msg.text!);
    var progressed = false;
    for (final m in decoded.messages) {
      // At-least-once transport: dedup is what makes notifications
      // exactly-once.
      if (m.seq <= _lastSeq) continue;
      if (m.topic.isNotEmpty && m.topic != topic) continue; // relay guard
      final decision = _engine.decide(m);
      // The cursor advances over EVERY delivered message, ignores included:
      // "seen and chose not to alert" is still "seen".
      _lastSeq = m.seq;
      if (decision.effect != Effect.ignore) {
        _events.add(NotchEvent(m, decision));
      }
      progressed = true;
    }
    if (progressed) _persist();
  }

  void _persist() {
    final seq = _lastSeq;
    unawaited(
      cursors
          .save(topic, seq)
          .then((_) {
            // Ack only once the cursor is durable: a crash between write and ack
            // just replays (dedup catches it); an ack before the write could
            // lose a message.
            if (seq == _lastSeq) _live?.send(jsonEncode({'ack': seq}));
          })
          .catchError((Object _) {}),
    );
  }

  void _onDisconnect(WsTransport? t) {
    if (t != null && identical(t, _live)) {
      unawaited(t.close().catchError((_) {}));
      _live = null;
      unawaited(_sub?.cancel());
      _sub = null;
      _setConnected(false);
    }
    if (!_running) return;
    _retry?.cancel();
    _retry = Timer(_backoff.next(), _connect);
  }

  void _setConnected(bool v) {
    if (_connected == v) return;
    _connected = v;
    _status.add(v);
  }

  Future<void> stop() async {
    _running = false;
    _retry?.cancel();
    final t = _live;
    _live = null;
    await _sub?.cancel();
    _sub = null;
    if (t != null) await t.close();
    _setConnected(false);
    await cursors.close();
    await _events.close();
    await _status.close();
  }
}
