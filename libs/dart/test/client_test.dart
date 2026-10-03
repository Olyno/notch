/// Client behavior against fake transports: exactly-once under replay,
/// dedup, cursor persistence, ack-after-persist, reconnect with cursor,
/// rules gating dispatch.
library;

import 'dart:async';
import 'dart:convert';

import 'package:notch/notch.dart';
import 'package:test/test.dart';

/// Programmable transport: the test decides when frames arrive, when the
/// socket dies, and records everything the client sends.
class FakeTransport implements WsTransport {
  final _frames = StreamController<WsMessage>.broadcast();
  final sent = <String>[];
  var closed = false;

  void emit(Message m) => _frames.add(WsMessage.text(jsonEncode(m.toJson())));

  void emitBatch(List<Message> ms) => _frames.add(
    WsMessage.text(
      jsonEncode({'messages': ms.map((m) => m.toJson()).toList()}),
    ),
  );

  void emitDuplicate(Message m) => emit(m); // same seq twice

  void kill() {
    _frames.add(const WsMessage.close());
  }

  void failConnect() => _frames.addError(StateError('nope'));

  StreamSink<WsMessage> get sink => _frames.sink;

  @override
  Stream<WsMessage> get frames => _frames.stream;

  @override
  void send(String text) => sent.add(text);

  @override
  Future<void> close() async => closed = true;
}

Message msg(int seq, {List<String> tags = const [], String topic = 'game'}) =>
    Message(seq: seq, id: 'id$seq', topic: topic, title: 't$seq', tags: tags);

void main() {
  late List<Rule> rules;
  late NotchClient client;
  late Uri capturedUrl;
  late FakeTransport transport;
  late MemoryCursorStore cursors;
  final events = <NotchEvent>[];

  NotchClient buildClient({
    List<Rule>? overrideRules,
    CursorStore? overrideCursors,
    Duration? zeroBackoff,
  }) {
    final c = NotchClient(
      topic: 'game',
      relayUrl: Uri.parse('http://relay.test:8899'),
      rules: overrideRules ?? rules,
      cursors: overrideCursors ?? cursors,
      transport: (url) async {
        capturedUrl = url;
        return transport;
      },
      backoff: Backoff(
        base: const Duration(milliseconds: 1),
        cap: const Duration(milliseconds: 5),
        min: const Duration(milliseconds: 1),
      ),
    );
    c.events.listen(events.add);
    return c;
  }

  setUp(() {
    rules = [
      Rule.onTags(['leave'], Effect.ignore),
    ];
    cursors = MemoryCursorStore();
    transport = FakeTransport();
    events.clear();
  });

  tearDown(() async {
    await client.stop();
  });

  test('subscribe url carries since + filter params', () async {
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);
    expect(capturedUrl.scheme, 'ws');
    expect(capturedUrl.host, 'relay.test');
    expect(capturedUrl.port, 8899);
    expect(capturedUrl.path, '/ws/game');
    expect(capturedUrl.queryParameters['since'], '0');
  });

  test('https relay -> wss with default port 443', () {
    final c = NotchClient(
      topic: 'x',
      relayUrl: Uri.parse('https://relay.test'),
      rules: const [],
      cursors: MemoryCursorStore(),
      transport: (_) async => FakeTransport(),
    );
    expect(c.subscribeUrl.scheme, 'wss');
    expect(c.subscribeUrl.port, 443);
  });

  test('live messages dispatch once; ignores suppress events', () async {
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);

    transport.emit(msg(1)); // game joined
    transport.emit(msg(2, tags: ['leave'])); // ignored by rule
    transport.emit(msg(3)); // another join
    await Future.delayed(Duration.zero);

    expect(events.map((e) => e.message.seq), [1, 3]);
    expect(await cursors.load('game'), 3); // cursor advances over ignores too
    // one ack per frame: the cursor persisted after each page
    expect(transport.sent, ['{"ack":1}', '{"ack":2}', '{"ack":3}']);
  });

  test('duplicate replay after crash does not double-notify', () async {
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);

    transport.emit(msg(1));
    transport.emitDuplicate(msg(1)); // replay: same seq again
    transport.emitBatch([msg(2), msg(2)]); // batch containing a dup
    await Future.delayed(Duration.zero);

    expect(events.map((e) => e.message.seq), [1, 2]);
    expect(await cursors.load('game'), 2);
  });

  test('reconnect resumes from persisted cursor', () async {
    await cursors.save('game', 41); // cursor survived a process death
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);

    // server replays from 42 on the new connection
    transport.emitBatch([msg(42), msg(43)]);
    await Future.delayed(Duration.zero);
    expect(events.map((e) => e.message.seq), [42, 43]);

    // socket dies mid-frame-set; client reconnects and the new URL
    // must carry the advanced cursor
    transport.kill();
    await Future.delayed(const Duration(milliseconds: 30));
    expect(capturedUrl.queryParameters['since'], '43');
  });

  test('connect failures retry with backoff and keep going', () async {
    var attempts = 0;
    final good = FakeTransport();
    client = NotchClient(
      topic: 'game',
      relayUrl: Uri.parse('http://relay.test:8899'),
      rules: rules,
      cursors: cursors,
      backoff: Backoff(
        base: const Duration(milliseconds: 2),
        cap: const Duration(milliseconds: 8),
        min: const Duration(milliseconds: 1),
      ),
      transport: (_) async {
        attempts++;
        if (attempts < 3) throw Exception('network down ($attempts)');
        return good;
      },
    );
    client.events.listen(events.add);
    await client.start();

    await Future.delayed(const Duration(milliseconds: 120));
    expect(attempts, greaterThanOrEqualTo(3));
    expect(client.isConnected, isTrue);

    good.emit(msg(1));
    await Future.delayed(Duration.zero);
    expect(events.length, 1);
  });

  test('messages from a different topic are dropped defensively', () async {
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);
    transport.emit(msg(1, topic: 'other'));
    transport.emit(msg(2));
    await Future.delayed(Duration.zero);
    expect(events.map((e) => e.message.seq), [2]);
    // cursor did NOT advance over the wrong-topic message
    expect(await cursors.load('game'), 2);
  });

  test('batch replay frames arrive as one page of events', () async {
    client = buildClient();
    await client.start();
    await Future.delayed(Duration.zero);
    transport.emitBatch([
      msg(1),
      msg(2),
      msg(3, tags: ['leave']),
      msg(4),
    ]);
    await Future.delayed(Duration.zero);
    expect(events.map((e) => e.message.seq), [1, 2, 4]);
    expect(transport.sent.last, '{"ack":4}');
  });
}
