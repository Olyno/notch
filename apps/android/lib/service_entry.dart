/// Headless entrypoint: runs INSIDE the foreground service's FlutterEngine
/// (NotchService.kt executes this via DartExecutor.DartEntrypoint).
///
/// Owns the realtime pipeline: bootstrap -> config -> NotchClient ->
/// native notifications over the service channel.
library;

import 'package:notch/notch.dart';
import 'package:notch/notch_io.dart';

import 'config.dart';
import 'notify_bridge.dart';

@pragma('vm:entry-point')
Future<void> serviceMain() async {
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
    } catch (_) {
      // The notifier dying must never take the socket down.
    }
  });

  await client.start();
}
