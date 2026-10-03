/// Message model — the Dart mirror of `notch-core` (rust/crates/notch-core).
/// Keep both in sync; unknown JSON fields are ignored on decode (forward
/// compatible), and empty optional fields are omitted on encode (tiny wire).
library;

import 'dart:convert';

enum Priority {
  min('min'),
  low('low'),
  def('default'),
  high('high'),
  max('max');

  const Priority(this.wire);
  final String wire;

  static Priority parse(String? s) => Priority.values.firstWhere(
    (p) => p.wire == s,
    orElse: () => Priority.def,
  );
}

class Message {
  Message({
    required this.seq,
    required this.id,
    required this.topic,
    required this.title,
    this.body = '',
    this.tags = const [],
    this.priority = Priority.def,
    this.category = '',
    this.icon = '',
    this.silent = false,
    this.ttl = 0,
    this.timeoutMs = 0,
    this.dedupKey = '',
    this.channel = '',
    this.ts = 0,
  });

  final int seq;
  final String id;
  final String topic;
  final String title;
  final String body;
  final List<String> tags;
  final Priority priority;
  final String category;
  final String icon;
  final bool silent;
  final int ttl;
  final int timeoutMs;
  final String dedupKey;
  final String channel;
  final int ts;

  factory Message.fromJson(Map<String, Object?> j) => Message(
    seq: (j['seq'] as num?)?.toInt() ?? 0,
    id: j['id'] as String? ?? '',
    topic: j['topic'] as String? ?? '',
    title: j['title'] as String? ?? '',
    body: j['body'] as String? ?? '',
    tags: (j['tags'] as List?)?.cast<String>() ?? const [],
    priority: Priority.parse(j['priority'] as String?),
    category: j['category'] as String? ?? '',
    icon: j['icon'] as String? ?? '',
    silent: j['silent'] as bool? ?? false,
    ttl: (j['ttl'] as num?)?.toInt() ?? 0,
    timeoutMs: (j['timeout_ms'] as num?)?.toInt() ?? 0,
    dedupKey: j['dedup_key'] as String? ?? '',
    channel: j['channel'] as String? ?? '',
    ts: (j['ts'] as num?)?.toInt() ?? 0,
  );

  Map<String, Object?> toJson() => {
    'seq': seq,
    'id': id,
    'topic': topic,
    'title': title,
    if (body.isNotEmpty) 'body': body,
    if (tags.isNotEmpty) 'tags': tags,
    if (priority != Priority.def) 'priority': priority.wire,
    if (category.isNotEmpty) 'category': category,
    if (icon.isNotEmpty) 'icon': icon,
    if (silent) 'silent': true,
    if (ttl != 0) 'ttl': ttl,
    if (timeoutMs != 0) 'timeout_ms': timeoutMs,
    if (dedupKey.isNotEmpty) 'dedup_key': dedupKey,
    if (channel.isNotEmpty) 'channel': channel,
    'ts': ts,
  };

  /// The Android channel this message belongs to: explicit override, else
  /// one channel per topic (Android-13-friendly grouping).
  String get effectiveChannel => channel.isEmpty ? topic : channel;

  @override
  bool operator ==(Object other) =>
      other is Message && other.seq == seq && other.id == id;

  @override
  int get hashCode => Object.hash(seq, id);

  @override
  String toString() => 'Message($topic#$seq "$title" tags=$tags)';
}

/// Decode one WS text frame: either a bare message or `{"messages":[..]}`.
({List<Message> messages, bool isBatch}) decodeFrame(String rawJson) {
  final j = (jsonDecode(rawJson) as Map).cast<String, Object?>();
  final batch = j['messages'];
  if (batch is List) {
    return (
      messages: batch
          .whereType<Map<Object?, Object?>>()
          .map((m) => Message.fromJson(m.cast<String, Object?>()))
          .toList(),
      isBatch: true,
    );
  }
  return (messages: [Message.fromJson(j)], isBatch: false);
}
