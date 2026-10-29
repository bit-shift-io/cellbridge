// Smoke tests for the app shell, switching the bridge seam to fakes so no
// Rust library or Android device is needed.

import 'package:cellbridge/main.dart';
import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/demo_bridge.dart';
import 'package:cellbridge/src/settings_store.dart';
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
    txBytesPerSec: BigInt.zero,
    rxBytesPerSec: BigInt.zero,
  );
}

/// In-memory settings backend so the whole-app tests never touch a real
/// MethodChannel or the application-support directory.
class _MemoryBackend implements SettingsBackend {
  Map<String, Object?>? record;

  @override
  Future<Map<String, Object?>?> read() async =>
      record == null ? null : Map<String, Object?>.of(record!);

  @override
  Future<void> write(Map<String, Object?> value) async {
    record = Map<String, Object?>.of(value);
  }
}

const _savedConfig = BridgeConfig(
  privateKey: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
  address: '10.7.0.2/24',
  peerPublicKey: 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=',
  endpoint: '203.0.113.1:51820',
  mtu: 1420,
);

/// Installs the idle fake bridge for the duration of a test.
void _useIdleBridgeApi() {
  final original = bridgeApi;
  bridgeApi = _IdleBridgeApi();
  addTearDown(() => bridgeApi = original);
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

    await tester.pump(const Duration(milliseconds: 500));
    expect(api.stats().rxBytesPerSec, greaterThan(BigInt.zero));
    expect(api.stats().txBytesPerSec, greaterThan(BigInt.zero));

    await tester.tap(find.byKey(const Key('simulate-error')));
    await tester.pump(const Duration(milliseconds: 600));
    expect(find.byKey(const Key('last-error')), findsOneWidget);
    expect(api.stats().rxBytesPerSec, BigInt.zero);
    expect(api.stats().txBytesPerSec, BigInt.zero);
  });

  testWidgets('first launch with no saved config opens the config screen', (
    tester,
  ) async {
    _useIdleBridgeApi();
    final store = SettingsStore(backend: _MemoryBackend());

    await tester.pumpWidget(CellBridgeApp(store: store));
    await tester.pumpAndSettle();

    expect(find.text('Configuration'), findsOneWidget);
    expect(find.byKey(const Key('config-private-key')), findsOneWidget);
  });

  testWidgets('a saved config does not auto-open the config screen', (
    tester,
  ) async {
    _useIdleBridgeApi();
    final backend = _MemoryBackend()..record = encodeBridgeConfig(_savedConfig);
    final store = SettingsStore(backend: backend);

    await tester.pumpWidget(
      CellBridgeApp(store: store, initialConfig: _savedConfig),
    );
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('config-private-key')), findsNothing);
    expect(find.byKey(const Key('connect-toggle')), findsOneWidget);
  });

  testWidgets('the config screen is pushed at most once across rebuilds', (
    tester,
  ) async {
    _useIdleBridgeApi();
    final store = SettingsStore(backend: _MemoryBackend());

    await tester.pumpWidget(CellBridgeApp(store: store));
    await tester.pumpAndSettle();

    // Rebuild the root with the same store; the auto-open must not stack a
    // second copy of the config screen on top of the first.
    await tester.pumpWidget(CellBridgeApp(store: store));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('config-private-key')), findsOneWidget);
  });
}
