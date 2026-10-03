/// Cross-language integration: the REAL Rust relay binary and the REAL
/// notch-mcp binary feed the Dart client over real sockets. This is the
/// contract test for docs/PROTOCOL.md between two independent
/// implementations: agent -> MCP(stdio) -> relay(HTTP) -> phone(WS).
///
/// Requires release binaries:  cargo build --release  (in ../rust)
/// Run:  dart test -t xlang
@Tags(['xlang'])
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:notch/notch.dart';
import 'package:notch/notch_io.dart';
import 'package:test/test.dart';

/// Release binaries live in the sibling `rust/target/release`; `dart test`
/// runs with cwd = the package root (`libs/dart`).
String get releaseDir => '${Directory.current.path}/../../rust/target/release/';

bool get xlangReady =>
    File('${releaseDir}notch-relay').existsSync() &&
    File('${releaseDir}notch-mcp').existsSync();

File binary(String name) {
  final f = File('$releaseDir$name');
  if (!f.existsSync())
    throw StateError('missing $name — cargo build --release');
  return f;
}

/// Ask the OS for a free TCP port (bind-then-release).
Future<int> freePort() async {
  final s = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final port = s.port;
  await s.close();
  return port;
}

void main() {
  late Directory tmp;
  late String dbPath;
  late File cursorFile;
  late int relayPort;
  Process? relayProc;
  String relayUrl = '';

  /// Spawn the relay on [relayPort] (stable across restarts, like a real
  /// deployment) and wait for its "listening" line.
  Future<void> startRelay() async {
    final p = await Process.start(
      binary('notch-relay').path,
      [],
      environment: {'NOTCH_DB': dbPath, 'NOTCH_ADDR': '127.0.0.1:$relayPort'},
    );
    relayProc = p;
    final completer = Completer<void>();
    p.stdout.transform(utf8.decoder).transform(const LineSplitter()).listen((
      line,
    ) {
      if (line.contains('listening on') && !completer.isCompleted)
        completer.complete();
    });
    p.stderr.drain();
    await completer.future.timeout(
      const Duration(seconds: 5),
      onTimeout: () => throw StateError('relay failed to bind :$relayPort'),
    );
    relayUrl = 'http://127.0.0.1:$relayPort';
  }

  /// One-shot publish through the real MCP server over stdio (modern
  /// stateless client: identifies itself per request via _meta).
  Future<int> mcpSend(String topic, Map<String, Object?> args) async {
    final p = await Process.start(binary('notch-mcp').path, [relayUrl]);
    p.stderr.drain();
    final req = {
      'jsonrpc': '2.0',
      'id': 1,
      'method': 'tools/call',
      'params': {
        '_meta': {
          'io.modelcontextprotocol/protocolVersion': '2026-07-28',
          'io.modelcontextprotocol/clientCapabilities': <String, Object?>{},
        },
        'name': 'send_notification',
        'arguments': {'topic': topic, ...args},
      },
    };
    p.stdin.writeln(jsonEncode(req));
    await p.stdin.flush();
    final line = await p.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .first
        .timeout(const Duration(seconds: 5));
    await p.stdin.close();
    final resp = jsonDecode(line) as Map<String, Object?>;
    expect(resp['error'], isNull, reason: 'mcp tool call failed: $resp');
    return ((resp['result'] as Map)['structuredContent'] as Map)['seq'] as int;
  }

  setUp(() async {
    if (!xlangReady) return;
    tmp = await Directory.systemTemp.createTemp('notch-x');
    dbPath = '${tmp.path}/notch.db';
    cursorFile = File('${tmp.path}/cursor.json');
    relayPort = await freePort();
    await startRelay();
  });

  tearDown(() async {
    relayProc?.kill(ProcessSignal.sigkill);
    relayProc = null;
    if (xlangReady) await tmp.delete(recursive: true);
  });

  test(
    'agent (mcp) -> relay -> dart client: rules decide alert vs silence',
    () async {
      final cursors = FileCursorStore(cursorFile.path);
      final client = NotchClient(
        topic: 'game',
        relayUrl: Uri.parse(relayUrl),
        rules: [
          Rule.onTags(['leave'], Effect.ignore),
          Rule.onTags(['join'], Effect.alert),
        ],
        cursors: cursors,
        transport: ioTransport,
        // the headline "join not leave" with zero bandwidth for leaves:
        noneTags: ['leave'],
      );
      final events = <NotchEvent>[];
      client.events.listen(events.add);
      await client.start();
      await Future<void>.delayed(const Duration(milliseconds: 200));

      final s1 = await mcpSend('game', {
        'title': 'Alice joins',
        'tags': ['join'],
      });
      await mcpSend('game', {
        'title': 'Bob leaves',
        'tags': ['leave'],
      });
      final s3 = await mcpSend('game', {
        'title': 'Server restart',
        'tags': ['admin'],
        'priority': 'high',
        'icon': 'warn',
        'dedup_key': 'restart',
      });
      expect(s3, greaterThan(s1));
      await Future<void>.delayed(const Duration(milliseconds: 300));

      // leave never crossed the wire (server filter) and never alerts
      expect(events.map((e) => e.message.seq), [s1, s3]);
      expect(events[0].decision.effect, Effect.alert);
      expect(events[1].message.priority, Priority.high);
      expect(events[1].message.dedupKey, 'restart');

      // cursor persisted to disk = the phone can crash and resume anywhere
      await cursors.close();
      expect(jsonDecode(await cursorFile.readAsString())['game'], s3);
      await client.stop();
    },
    skip: !xlangReady ? 'needs cargo build --release (see header)' : false,
  );

  test(
    'relay dies and restarts on the same origin: client replays the gap',
    () async {
      final cursors = FileCursorStore(cursorFile.path);
      final client = NotchClient(
        topic: 'chat',
        relayUrl: Uri.parse(relayUrl),
        rules: const [],
        cursors: cursors,
        transport: ioTransport,
        backoff: Backoff(
          base: const Duration(milliseconds: 100),
          cap: const Duration(milliseconds: 500),
        ),
      );
      final events = <NotchEvent>[];
      client.events.listen(events.add);
      await client.start();
      await Future<void>.delayed(const Duration(milliseconds: 200));

      final before = await mcpSend('chat', {'title': 'before the storm'});
      await Future<void>.delayed(const Duration(milliseconds: 300));
      expect(events.map((e) => e.message.seq), [before]);

      // ---- the relay itself goes down (worst network: nothing works) ----
      relayProc!.kill(ProcessSignal.sigkill);
      await relayProc!.exitCode;
      // the client notices and starts its backoff ladder; the operator
      // restarts the relay on the SAME port with the SAME SQLite file.
      await startRelay();
      final after = await mcpSend('chat', {'title': 'during the storm'});
      final last = await mcpSend('chat', {'title': 'after recovery'});

      // the client reconnects on its own; replay must deliver both misses
      final deadline = DateTime.now().add(const Duration(seconds: 5));
      while (events.length < 3 && DateTime.now().isBefore(deadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }
      final got = events.map((e) => e.message.seq).toList();
      expect(
        got,
        containsAllInOrder([before, after, last]),
        reason: 'client got $got',
      );
      // exactly-once across the whole outage: no duplicates
      expect(got.toSet().length, got.length);
      await cursors.close();
      await client.stop();
    },
    skip: !xlangReady ? 'needs cargo build --release' : false,
  );

  test(
    'client restart re-reads cursor from disk: no duplicates, no gaps',
    () async {
      // pre-seed the cursor file: this "phone" already processed up to seq 2
      cursorFile.writeAsStringSync('{"chat":2}');
      await mcpSend('chat', {'title': 'already seen'}); // seq 1
      await mcpSend('chat', {'title': 'already seen 2'}); // seq 2
      final s3 = await mcpSend('chat', {
        'title': 'fresh',
      }); // seq 3 -> must show

      final cursors = FileCursorStore(cursorFile.path);
      final client = NotchClient(
        topic: 'chat',
        relayUrl: Uri.parse(relayUrl),
        rules: const [],
        cursors: cursors,
        transport: ioTransport,
      );
      final events = <NotchEvent>[];
      client.events.listen(events.add);
      await client.start();
      await Future<void>.delayed(const Duration(milliseconds: 400));

      expect(events.map((e) => e.message.seq), [
        s3,
      ], reason: 'replay must skip what the cursor already covers');
      await cursors.close();
      await client.stop();
    },
    skip: !xlangReady ? 'needs cargo build --release' : false,
  );
}
