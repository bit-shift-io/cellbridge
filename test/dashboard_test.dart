import 'dart:async';

import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/dashboard.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _config = BridgeConfig(
  privateKey: 'private',
  address: '10.7.0.2/24',
  peerPublicKey: 'public',
  endpoint: '192.0.2.1:51820',
  mtu: 1420,
);

/// A [BridgeApi] whose status is scripted by the test.
///
/// The first status read happens when the dashboard is created; later reads
/// come from its poll timer, so a script can model `Connecting` -> `Connected`.
class _ScriptedBridgeApi implements BridgeApi {
  _ScriptedBridgeApi({
    List<BridgeStatus>? statuses,
    BridgeStatus? status,
    Stats? statsValue,
  }) : _statuses =
           statuses ?? [status ?? const BridgeStatus(state: BridgeState.idle)],
       statsValue =
           statsValue ??
           Stats(
             txBytes: BigInt.zero,
             rxBytes: BigInt.zero,
             activeFlows: BigInt.zero,
             txBytesPerSec: BigInt.zero,
             rxBytesPerSec: BigInt.zero,
           );

  final List<BridgeStatus> _statuses;
  int _statusIndex = 0;
  int statusReads = 0;
  Stats statsValue;

  int startCalls = 0;
  int stopCalls = 0;
  Completer<void>? startGate;
  Completer<void>? stopGate;

  @override
  Future<void> start(BridgeConfig config) async {
    startCalls++;
    _statuses
      ..clear()
      ..add(const BridgeStatus(state: BridgeState.connecting));
    _statusIndex = 0;
    final gate = startGate;
    if (gate != null) {
      await gate.future;
    }
    _statuses
      ..clear()
      ..add(const BridgeStatus(state: BridgeState.connected));
    _statusIndex = 0;
  }

  @override
  Future<void> stop() async {
    stopCalls++;
    final gate = stopGate;
    if (gate != null) {
      await gate.future;
    }
    _statuses
      ..clear()
      ..add(const BridgeStatus(state: BridgeState.idle));
    _statusIndex = 0;
  }

  @override
  BridgeStatus status() {
    statusReads++;
    final index = _statusIndex.clamp(0, _statuses.length - 1);
    final result = _statuses[index];
    if (_statusIndex < _statuses.length - 1) {
      _statusIndex++;
    }
    return result;
  }

  @override
  Stats stats() => statsValue;
}

Widget _app({BridgeConfig? Function()? config}) =>
    MaterialApp(home: Dashboard(config: config ?? () => _config));

void _useBridgeApi(BridgeApi api) {
  final original = bridgeApi;
  bridgeApi = api;
  addTearDown(() => bridgeApi = original);
}

String _text(WidgetTester tester, String key) =>
    tester.widget<Text>(find.byKey(Key(key))).data!;

FilledButton _toggle(WidgetTester tester) =>
    tester.widget<FilledButton>(find.byKey(const Key('connect-toggle')));

void main() {
  testWidgets('all five rows remain accessible on a short screen', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 480);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    _useBridgeApi(_ScriptedBridgeApi());
    await tester.pumpWidget(_app());
    expect(tester.takeException(), isNull);
    await tester.ensureVisible(find.byKey(const Key('active-flows')));
    expect(find.byKey(const Key('active-flows')).hitTestable(), findsOneWidget);
    expect(find.text('Download'), findsOneWidget);
    expect(find.text('Upload'), findsOneWidget);
    expect(find.text('0 Mbps'), findsNWidgets(2));
  });
  testWidgets('shows Connecting then Connected as status is polled', (
    tester,
  ) async {
    _useBridgeApi(
      _ScriptedBridgeApi(
        statuses: const [
          BridgeStatus(state: BridgeState.connecting),
          BridgeStatus(state: BridgeState.connected),
        ],
      ),
    );

    await tester.pumpWidget(_app());
    expect(_text(tester, 'status-text'), 'Connecting');

    await tester.pump(const Duration(milliseconds: 500));
    expect(_text(tester, 'status-text'), 'Connected');
  });

  testWidgets('disables the toggle while Connecting', (tester) async {
    _useBridgeApi(
      _ScriptedBridgeApi(
        status: const BridgeStatus(state: BridgeState.connecting),
      ),
    );

    await tester.pumpWidget(_app());

    expect(_text(tester, 'status-text'), 'Connecting');
    expect(_toggle(tester).onPressed, isNull);
    expect(find.text('Disconnect'), findsNothing);
  });

  testWidgets('renders human-readable stats', (tester) async {
    _useBridgeApi(
      _ScriptedBridgeApi(
        status: const BridgeStatus(state: BridgeState.connected),
        statsValue: Stats(
          txBytes: BigInt.from(999),
          rxBytes: BigInt.from(1024),
          activeFlows: BigInt.from(3),
          txBytesPerSec: BigInt.from(5000000), // 40 Mbps up
          rxBytesPerSec: BigInt.from(3750000), // 30 Mbps down
        ),
      ),
    );

    await tester.pumpWidget(_app());

    expect(_text(tester, 'bytes-download'), '1 KiB');
    expect(_text(tester, 'bytes-upload'), '999 B');
    expect(_text(tester, 'speed-down'), '30 Mbps');
    expect(_text(tester, 'speed-up'), '40 Mbps');
    expect(_text(tester, 'active-flows'), '3');
  });

  testWidgets('shows the error text from an Error status', (tester) async {
    _useBridgeApi(
      _ScriptedBridgeApi(
        status: const BridgeStatus(
          state: BridgeState.error,
          error: 'tunnel lost: handshake timeout',
        ),
      ),
    );

    await tester.pumpWidget(_app());

    expect(_text(tester, 'status-text'), 'Error');
    expect(find.text('tunnel lost: handshake timeout'), findsOneWidget);
  });

  testWidgets('does not offer stop while a start is in flight', (tester) async {
    final api = _ScriptedBridgeApi();
    api.startGate = Completer<void>();
    _useBridgeApi(api);

    await tester.pumpWidget(_app());
    await tester.tap(find.byKey(const Key('connect-toggle')));
    await tester.pump();

    expect(api.startCalls, 1);
    expect(_text(tester, 'status-text'), 'Connecting');
    expect(_toggle(tester).onPressed, isNull);
    expect(find.text('Disconnect'), findsNothing);
    expect(api.stopCalls, 0);

    api.startGate!.complete();
    await tester.pump();

    expect(_text(tester, 'status-text'), 'Connected');
    expect(find.text('Disconnect'), findsOneWidget);
    expect(_toggle(tester).onPressed, isNotNull);
  });

  testWidgets('shows a missing configuration without calling start', (
    tester,
  ) async {
    final api = _ScriptedBridgeApi();
    _useBridgeApi(api);

    await tester.pumpWidget(_app(config: () => null));
    await tester.tap(find.byKey(const Key('connect-toggle')));
    await tester.pump();

    expect(api.startCalls, 0);
    expect(find.byKey(const Key('last-error')), findsOneWidget);
  });

  testWidgets('does not poll while Idle', (tester) async {
    final api = _ScriptedBridgeApi();
    _useBridgeApi(api);

    await tester.pumpWidget(_app());
    expect(api.statusReads, 1);

    await tester.pump(const Duration(seconds: 2));
    expect(api.statusReads, 1);
  });

  testWidgets('stops polling when the widget is not visible', (tester) async {
    final api = _ScriptedBridgeApi(
      status: const BridgeStatus(state: BridgeState.connecting),
    );
    _useBridgeApi(api);

    Widget app({required bool visible}) =>
        TickerMode(enabled: visible, child: _app());

    await tester.pumpWidget(app(visible: true));
    await tester.pump(const Duration(milliseconds: 500));
    expect(api.statusReads, greaterThan(1));

    await tester.pumpWidget(app(visible: false));
    final readsWhileHidden = api.statusReads;
    await tester.pump(const Duration(seconds: 2));
    expect(api.statusReads, readsWhileHidden);
  });
}
