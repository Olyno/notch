/// The two platform channels. No plugins; MethodChannels only.
///
/// - `notch.ui`      (UI engine <-> MainActivity): start/stop service, filesDir,
///                   permission prompts.
/// - `notch.service` (service engine <-> NotchService): bootstrap (filesDir),
///                   post a native notification, update service status.
library;

import 'package:flutter/services.dart';
import 'package:notch/notch.dart';

class UiChannel {
  static const _channel = MethodChannel('notch.ui');

  static Future<void> startService() => _channel.invokeMethod<void>('start');
  static Future<void> stopService() => _channel.invokeMethod<void>('stop');
  static Future<bool> isRunning() async =>
      await _channel.invokeMethod<bool>('running') ?? false;
  static Future<String> filesDir() async =>
      await _channel.invokeMethod<String>('filesDir') ?? '';
  static Future<bool> requestNotifications() async =>
      await _channel.invokeMethod<bool>('requestNotifications') ?? false;
  static Future<bool> requestBatteryExemption() async =>
      await _channel.invokeMethod<bool>('requestBatteryExemption') ?? false;
}

class Notifier {
  static const _channel = MethodChannel('notch.service');

  static Future<String?> bootstrap() => _channel.invokeMethod<String>('bootstrap');

  /// Render [event] as a native notification. Every Android-specific
  /// decision (importance, channels, dedup, timeout) lives in Notifier.kt.
  static Future<void> show(NotchEvent event) {
    final m = event.message;
    return _channel.invokeMethod<void>('post', {
      'seq': m.seq,
      'topic': m.topic,
      'title': m.title,
      'body': m.body,
      'channel': m.effectiveChannel,
      'priority': m.priority.name,
      'silent': event.decision.silent ?? m.silent,
      'category': m.category,
      'icon': m.icon,
      'dedupKey': m.dedupKey,
      'timeoutMs': m.timeoutMs,
      if (m.url.isNotEmpty) 'url': m.url,
    });
  }

  /// Live status for the persistent service notification.
  static Future<void> status(bool connected) =>
      _channel.invokeMethod<void>('status', {'connected': connected});
}
