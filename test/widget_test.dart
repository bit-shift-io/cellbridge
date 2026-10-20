// Smoke tests for the app shell, switching the bridge seam to fakes so no
// Rust library or Android device is needed.

import 'package:cellbridge/main.dart';
import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/demo_bridge.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _IdleBridgeApi implements BridgeApi {
  @override
  Future<void> start(BridgeConfig config) async {}

  @override
  Future<void> stop() async {}

  @override
  BridgeStatus status() => const BridgeStatus(state: BridgeState.idle);

  @override
  Stats stats() => Stats(
    txBytes: BigInt.zero,
    rxBytes: BigInt.zero,
    activeFlows: BigInt.zero,
  );
}

void main() {
  testWidgets('renders the dashboard', (tester) async {
    final original = bridgeApi;
    bridgeApi = _IdleBridgeApi();
    addTearDown(() => bridgeApi = original);

    await tester.pumpWidget(const CellBridgeApp());

    expect(find.text('CellBridge'), findsOneWidget);
    expect(find.byKey(const Key('connect-toggle')), findsOneWidget);
    expect(find.byKey(const Key('status-text')), findsOneWidget);
  });

  testWidgets('demo mode connects and reaches the error UI', (tester) async {
    final api = DemoBridgeApi();
    final original = bridgeApi;
    bridgeApi = api;
    addTearDown(() async {
      await api.stop();
      bridgeApi = original;
    });

    await tester.pumpWidget(CellBridgeApp(demoApi: api));
    await tester.tap(find.byKey(const Key('connect-toggle')));
    await tester.pump();
    expect(find.text('Connecting'), findsOneWidget);

    await tester.pump(const Duration(milliseconds: 1600));
    expect(find.text('Connected'), findsOneWidget);

    await tester.tap(find.byKey(const Key('simulate-error')));
    await tester.pump(const Duration(milliseconds: 600));
    expect(find.byKey(const Key('last-error')), findsOneWidget);
  });
}
