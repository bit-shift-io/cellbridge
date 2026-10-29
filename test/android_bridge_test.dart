import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:cellbridge/src/android_bridge.dart';
import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/dashboard.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('dev.cellbridge/bridge');
  final calls = <String>[];
  String state = 'connected';
  setUp(() {
    calls.clear();
    state = 'connected';
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (call) async {
          calls.add(call.method);
          if (call.method == 'stop') state = 'idle';
          if (call.method == 'start') state = 'connecting';
          return jsonEncode({
            'state': state,
            'error': null,
            'txBytes': '9007199254740993',
            'rxBytes': '5',
            'activeFlows': '1',
            'txBytesPerSec': '3750000',
            'rxBytesPerSec': '5000000',
            'diagnostics': [],
            'binding': {'cellularBindVerified': true},
          });
        });
  });
  tearDown(
    () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, null),
  );

  testWidgets(
    'idle dashboard observes headless service startup without Connect',
    (tester) async {
      state = 'idle';
      final previous = bridgeApi;
      final api = AndroidBridgeApi(channel: channel);
      bridgeApi = api;
      await api.refresh();
      await tester.pumpWidget(MaterialApp(home: Dashboard(config: () => null)));
      expect(find.text('Idle'), findsOneWidget);
      state = 'connected';
      await api.refresh();
      await tester.pump();
      expect(find.text('Connected'), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
      api.dispose();
      bridgeApi = previous;
    },
  );

  test('attaches to existing service and preserves large counters without starting Rust', () async {
    final api = AndroidBridgeApi(channel: channel);
    addTearDown(api.dispose);
    await api.initialize();
    expect(api.status().state, BridgeState.connected);
    expect(api.stats().txBytes, BigInt.parse('9007199254740993'));
    expect(api.stats().txBytesPerSec, BigInt.parse('3750000'));
    expect(api.stats().rxBytesPerSec, BigInt.parse('5000000'));
    expect(calls, ['snapshot']);
    await api.stop();
    expect(api.status().state, BridgeState.idle);
  });

  test('start requests native service without sending credentials or an fd through Flutter', () async {
    final api = AndroidBridgeApi(channel: channel);
    addTearDown(api.dispose);
    await api.start(
      const BridgeConfig(
        privateKey: 'secret',
        address: '10.10.10.2/24',
        peerPublicKey: 'peer',
        endpoint: '127.0.0.1:51820',
        mtu: 1420,
      ),
    );
    expect(calls, ['start']);
    expect(api.status().state, BridgeState.connecting);
  });
}
