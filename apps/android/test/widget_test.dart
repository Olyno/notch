import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:notch_app/main.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late Directory appDir;

  setUp(() async {
    appDir = await Directory.systemTemp.createTemp('notch-ui');
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(const MethodChannel('notch.ui'), (call) async {
      switch (call.method) {
        case 'filesDir':
          return appDir.path;
        case 'running':
          return false;
        default:
          return null;
      }
    });
  });

  tearDown(() => appDir.deleteSync(recursive: true));

  testWidgets('home renders config form and stopped state', (tester) async {
    // The init path touches real dart:io (config.json); run it for real.
    await tester.runAsync(() async {
      await tester.pumpWidget(const NotchApp());
      await Future<void>.delayed(const Duration(milliseconds: 50));
    });
    await tester.pumpAndSettle();

    expect(find.text('Relay URL'), findsOneWidget);
    expect(find.text('Topic'), findsOneWidget);
    expect(find.text('Listener stopped'), findsOneWidget);
    expect(find.text('Start'), findsOneWidget);
  });

  testWidgets('Start requests permission, persists config, flips badge',
      (tester) async {
    var started = false;
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(const MethodChannel('notch.ui'), (call) async {
      switch (call.method) {
        case 'filesDir':
          return appDir.path;
        case 'running':
          return started;
        case 'requestNotifications':
          return true;
        case 'requestBatteryExemption':
          return true;
        case 'start':
          started = true;
          return true;
        case 'stop':
          started = false;
          return true;
        default:
          return null;
      }
    });

    await tester.runAsync(() async {
      await tester.pumpWidget(const NotchApp());
      await Future<void>.delayed(const Duration(milliseconds: 50));
    });
    await tester.pumpAndSettle();

    await tester.runAsync(() async {
      await tester.tap(find.text('Start'));
      await Future<void>.delayed(const Duration(milliseconds: 50)); // _save + start
    });
    await tester.pumpAndSettle();

    expect(started, isTrue);
    expect(find.text('Listener running'), findsOneWidget);
    expect(find.text('Stop'), findsOneWidget);
    // config was persisted for the service
    expect(File('${appDir.path}/config.json').existsSync(), isTrue);
  });
}
