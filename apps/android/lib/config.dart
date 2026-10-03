/// App configuration: relay address, topic, and the local alert rules.
/// Persisted as one small JSON file inside the app's files dir (path handed
/// over by a trivial platform channel — no plugin dependencies).
library;

import 'dart:convert';
import 'dart:io';

import 'package:notch/notch.dart';

class AppConfig {
  AppConfig({
    this.relay = 'http://192.168.1.10:8899',
    this.topic = 'alerts',
    this.rules = const [],
    this.serverFilterNone = true,
  });

  final String relay;
  final String topic;
  final List<Rule> rules;

  /// Push the rule's ignores to the relay so filtered-out messages never
  /// cost bytes on the wire (docs/PROTOCOL.md). Disable if rules are complex.
  final bool serverFilterNone;

  static AppConfig fromJson(Map<String, Object?> j) => AppConfig(
    relay: j['relay'] as String? ?? 'http://localhost:8899',
    topic: j['topic'] as String? ?? 'alerts',
    rules: (j['rules'] as List? ?? [])
        .cast<Map<Object?, Object?>>()
        .map((r) => Rule(
              tagsAll: (r['all'] as List?)?.cast<String>(),
              tagsAny: (r['any'] as List?)?.cast<String>(),
              tagsNone: (r['none'] as List?)?.cast<String>(),
              effect: Effect.values.firstWhere(
                (e) => e.name == r['effect'],
                orElse: () => Effect.alert,
              ),
            ))
        .toList(),
    serverFilterNone: j['server_filter_none'] as bool? ?? true,
  );

  Map<String, Object?> toJson() => {
    'relay': relay,
    'topic': topic,
    'server_filter_none': serverFilterNone,
    'rules': rules
        .map((r) => {
              'all': r.tagsAll,
              'any': r.tagsAny,
              'none': r.tagsNone,
              'effect': r.effect.name,
            })
        .toList(),
  };

  /// Tag names the server should never send us (first ignore-rule).
  List<String>? get noneTags {
    if (!serverFilterNone) return null;
    final first = rules.where((r) => r.effect == Effect.ignore).firstOrNull;
    final tags = <String>{...?first?.tagsNone, ...?first?.tagsAll};
    return tags.isEmpty ? null : tags.toList();
  }

  static File file(String dir) => File('$dir/config.json');

  static Future<AppConfig> load(String dir) async {
    final f = file(dir);
    if (!await f.exists()) return AppConfig();
    try {
      return AppConfig.fromJson(
        (jsonDecode(await f.readAsString()) as Map).cast<String, Object?>(),
      );
    } catch (_) {
      return AppConfig(); // corrupt config = defaults, never crash the service
    }
  }

  Future<void> save(String dir) =>
      file(dir).writeAsString(const JsonEncoder.withIndent('  ').convert(toJson()));
}
