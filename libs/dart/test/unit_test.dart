import 'dart:convert';
import 'dart:io';
import 'package:notch/notch.dart';
import 'package:test/test.dart';

Message m(
  int seq, {
  List<String> tags = const [],
  bool silent = false,
  String topic = 't',
}) => Message(
  seq: seq,
  id: 'id$seq',
  topic: topic,
  title: 'title$seq',
  tags: tags,
  silent: silent,
);

void main() {
  group('model', () {
    test('wire omits empty fields (tiny payloads)', () {
      final j = jsonEncode(m(1, tags: ['join']).toJson());
      final map = jsonDecode(j) as Map;
      expect(map.keys, containsAll(['seq', 'id', 'topic', 'title']));
      for (final absent in [
        'body',
        'priority',
        'category',
        'icon',
        'silent',
        'ttl',
        'timeout_ms',
        'dedup_key',
        'channel',
      ]) {
        expect(
          map.containsKey(absent),
          isFalse,
          reason: '$absent must be omitted',
        );
      }
      expect(map['tags'], ['join']);
    });

    test('unknown fields are ignored (forward compatible)', () {
      final msg = Message.fromJson({
        'seq': 2,
        'topic': 'x',
        'title': 't',
        'future_field': 42,
      });
      expect(msg.seq, 2);
      expect(msg.priority, Priority.def);
    });

    test('priority round-trips lowercase', () {
      expect(Priority.parse('max'), Priority.max);
      expect(Priority.parse(null), Priority.def);
      expect(m(1).toJson().containsKey('priority'), isFalse); // default omitted
    });

    test('effectiveChannel falls back to topic', () {
      expect(m(1).effectiveChannel, 't');
      final withCh = Message(
        seq: 1,
        id: 'a',
        topic: 't',
        title: 'x',
        channel: 'game',
      );
      expect(withCh.effectiveChannel, 'game');
    });

    test('decodeFrame handles bare and batch frames', () {
      final bare = decodeFrame(jsonEncode(m(5).toJson()));
      expect(bare.isBatch, isFalse);
      expect(bare.messages.single.seq, 5);

      final batch = decodeFrame(
        jsonEncode({
          'messages': [m(6).toJson(), m(7).toJson()],
        }),
      );
      expect(batch.isBatch, isTrue);
      expect(batch.messages.map((x) => x.seq), [6, 7]);
    });
  });

  group('rules', () {
    test('join alerts, leave ignored — the headline scenario', () {
      final engine = RuleEngine([
        Rule.onTags(['leave'], Effect.ignore),
        Rule.onTags(['join'], Effect.alert),
      ]);
      expect(engine.decide(m(1, tags: ['join'])).effect, Effect.alert);
      expect(engine.decide(m(2, tags: ['leave'])).effect, Effect.ignore);
      // first match wins: leave+join -> ignore (leave is listed first)
      expect(
        engine.decide(m(3, tags: ['leave', 'join'])).effect,
        Effect.ignore,
      );
    });

    test('silent rule forces silent effect', () {
      final engine = RuleEngine([
        Rule.onTags(['bg'], Effect.silent),
      ]);
      final d = engine.decide(m(1, tags: ['bg'], silent: false));
      expect(d.effect, Effect.silent);
      expect(d.silent, isTrue);
    });

    test('no match: silent messages stay silent, others alert', () {
      final engine = RuleEngine([
        Rule.onTags(['x'], Effect.alert),
      ]);
      expect(engine.decide(m(1)).effect, Effect.alert);
      expect(engine.decide(m(2, silent: true)).effect, Effect.silent);
    });

    test('tag predicates: all/any/none semantics', () {
      final r = Rule(
        tagsAll: ['a', 'b'],
        tagsAny: ['c'],
        tagsNone: ['d'],
        effect: Effect.alert,
      );
      expect(r.matches(m(1, tags: ['a', 'b', 'c'])), isTrue);
      expect(r.matches(m(2, tags: ['a', 'b'])), isFalse); // any missing
      expect(r.matches(m(3, tags: ['a', 'c'])), isFalse); // all missing
      expect(r.matches(m(4, tags: ['a', 'b', 'c', 'd'])), isFalse); // none hit
    });
  });

  group('backoff', () {
    test('first attempt is immediate', () {
      final b = Backoff();
      expect(b.next(), Duration.zero);
    });

    test('grows exponentially, capped, never under 200ms', () {
      final b = Backoff(
        base: const Duration(seconds: 1),
        cap: const Duration(seconds: 10),
      );
      expect(b.next(), Duration.zero); // first step connects immediately
      final durations = [for (var i = 0; i < 12; i++) b.next()];
      expect(durations.every((d) => d <= const Duration(seconds: 10)), isTrue);
      expect(durations.every((d) => d.inMicroseconds >= 200000), isTrue);
      // after the 1s base window opens, jitter can land anywhere in it:
      expect(
        durations.any((d) => d > const Duration(milliseconds: 200)),
        isTrue,
      );
    });

    test('reset returns to zero', () {
      final b = Backoff()
        ..next()
        ..next();
      b.reset();
      expect(b.next(), Duration.zero);
    });
  });

  group('cursor store', () {
    test('memory store is monotonic', () async {
      final s = MemoryCursorStore();
      await s.save('t', 5);
      await s.save('t', 3); // lower ignored
      expect(await s.load('t'), 5);
      expect(await s.load('other'), 0);
    });

    test('file store persists across instances', () async {
      final path =
          '${Directory.systemTemp.createTempSync('notch').path}/c.json';
      final a = FileCursorStore(path);
      await a.save('t', 7);
      await a.close();
      final b = FileCursorStore(path);
      expect(await b.load('t'), 7);
      await b.save('t', 2); // monotonic across instances
      expect(await b.load('t'), 7);
    });

    test('corrupt file reads as empty (safe replay from 0)', () async {
      final dir = Directory.systemTemp.createTempSync('notch');
      final f = File('${dir.path}/c.json')..writeAsStringSync('not json{{');
      final s = FileCursorStore(f.path);
      expect(await s.load('t'), 0);
    });
  });
}
