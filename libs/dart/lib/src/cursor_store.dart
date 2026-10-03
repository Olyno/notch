/// Persistent delivery cursors: the poor-network guarantee only works if
/// "the last seq I actually processed" survives process death.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

abstract interface class CursorStore {
  /// Last processed seq for [topic], 0 if never.
  Future<int> load(String topic);

  Future<void> save(String topic, int seq);

  Future<void> close();
}

/// In-memory (tests, ephemeral embeds).
class MemoryCursorStore implements CursorStore {
  final Map<String, int> _cursors = {};

  @override
  Future<int> load(String topic) async => _cursors[topic] ?? 0;

  @override
  Future<void> save(String topic, int seq) async {
    if (seq > (_cursors[topic] ?? 0)) _cursors[topic] = seq;
  }

  @override
  Future<void> close() async {}
}

/// Single JSON file, atomic write via rename (the on-device implementation
/// of "the cursor survives everything"). Flutter apps can pass a path under
/// `getApplicationSupportDirectory()`; the service isolate owns the file.
class FileCursorStore implements CursorStore {
  FileCursorStore(this.path) : _file = File(path);

  final String path;
  final File _file;
  Map<String, int>? _cache;
  Future<void> _pending = Future.value();

  @override
  Future<int> load(String topic) async {
    final cache = _cache ??= await _read();
    return cache[topic] ?? 0;
  }

  @override
  Future<void> save(String topic, int seq) {
    // Serialize writes; drop-if-lower keeps acks monotonic under races.
    return _pending = _pending.then((_) async {
      final cache = _cache ??= await _read();
      if (seq <= (cache[topic] ?? 0)) return;
      cache[topic] = seq;
      final tmp = File('$path.tmp');
      await tmp.writeAsString(jsonEncode(cache));
      await tmp.rename(path); // atomic on POSIX; best-effort elsewhere
    });
  }

  Future<Map<String, int>> _read() async {
    try {
      final raw = await _file.readAsString();
      final map = (jsonDecode(raw) as Map).cast<String, Object?>();
      return map.map((k, v) => MapEntry(k, (v as num).toInt()));
    } catch (_) {
      return {}; // missing/corrupt file = no history; replay from 0 is safe
    }
  }

  @override
  Future<void> close() async {
    await _pending;
    _cache = null;
  }
}
