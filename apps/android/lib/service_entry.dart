/// Headless entrypoint: runs INSIDE the foreground service's FlutterEngine
/// (NotchService.kt executes this via DartExecutor.DartEntrypoint).
///
/// Owns the realtime pipeline: bootstrap -> config -> NotchClient ->
/// native notifications over the service channel.
library;

import 'package:flutter/widgets.dart';
import 'package:notch/notch.dart';
import 'package:notch/notch_io.dart';

import 'config.dart';
import 'notify_bridge.dart';

@pragma('vm:entry-point')
Future<void> serviceMain() async {
  // Background engines start WITHOUT the binding: MethodChannel throws
  // "Binding has not yet been initialized" until this runs.
  WidgetsFlutterBinding.ensureInitialized();

  final dir = await Notifier.bootstrap();
  if (dir == null) return; // service not ready yet: nothing to do

  final config = await AppConfig.load(dir);
  final client = NotchClient(
    topic: config.topic,
    relayUrl: Uri.parse(config.relay),
    rules: config.rules,
    cursors: FileCursorStore('$dir/cursors.json'),
    transport: ioTransport,
    noneTags: config.noneTags,
  );

  client.connectionStatus.listen((connected) {
    Notifier.status(connected);
  });

  client.events.listen((event) async {
    try {
      await Notifier.show(event);
    } catch (e) {
      // The notifier dying must never take the socket down — but it must
      // not die silently either. debugPrint keeps logcat as the sink.
      debugPrint('notch: notify failed: $e');
    }
  });

  await client.start();
}
