import 'dart:async';
import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'bridge.dart';

/// Cached view of the headless Android service. Never starts Rust via FRB.
class AndroidBridgeApi extends ChangeNotifier
    with WidgetsBindingObserver
    implements BridgeApi {
  AndroidBridgeApi({
    this.channel = const MethodChannel('dev.cellbridge/bridge'),
  });
  final MethodChannel channel;
  Map<String, dynamic> snapshot = const {'state': 'idle'};
  Timer? _timer;
  bool _refreshing = false;
  bool _disposed = false;

  Future<void> initialize() async {
    WidgetsBinding.instance.addObserver(this);
    await refresh();
    _startPolling();
  }

  void _startPolling() {
    _timer?.cancel();
    _timer = Timer.periodic(
      const Duration(milliseconds: 500),
      (_) => unawaited(refresh()),
    );
  }

  Future<void> refresh() async {
    if (_refreshing || _disposed) return;
    _refreshing = true;
    try {
      final json = await channel
          .invokeMethod<String>('snapshot')
          .timeout(const Duration(seconds: 5));
      if (json != null) snapshot = jsonDecode(json) as Map<String, dynamic>;
    } catch (_) {
      snapshot = {
        ...snapshot,
        'state': 'error',
        'error': 'Could not read Android service state',
      };
    } finally {
      _refreshing = false;
      if (!_disposed) notifyListeners();
    }
  }

  Future<void> _command(String method) async {
    final json = await channel
        .invokeMethod<String>(method)
        .timeout(const Duration(seconds: 15));
    if (json != null) snapshot = jsonDecode(json) as Map<String, dynamic>;
    if (!_disposed) notifyListeners();
  }

  @override
  Future<void> start(BridgeConfig config) => _command('start');
  @override
  Future<void> stop() => _command('stop');
  @override
  BridgeStatus status() => BridgeStatus(
    state: switch (snapshot['state']) {
      'connecting' => BridgeState.connecting,
      'connected' => BridgeState.connected,
      'error' => BridgeState.error,
      _ => BridgeState.idle,
    },
    error: snapshot['error'] as String?,
  );
  @override
  Stats stats() => Stats(
    txBytes: BigInt.parse('${snapshot['txBytes'] ?? 0}'),
    rxBytes: BigInt.parse('${snapshot['rxBytes'] ?? 0}'),
    activeFlows: BigInt.parse('${snapshot['activeFlows'] ?? 0}'),
    txBytesPerSec: BigInt.parse('${snapshot['txBytesPerSec'] ?? 0}'),
    rxBytesPerSec: BigInt.parse('${snapshot['rxBytesPerSec'] ?? 0}'),
  );
  Future<String> cellularControl() async =>
      await channel
          .invokeMethod<String>('cellularControl')
          .timeout(const Duration(seconds: 15)) ??
      'No response';
  Future<void> batterySettings() =>
      channel.invokeMethod<void>('batterySettings');

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(refresh());
      _startPolling();
    } else {
      _timer?.cancel();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }
}
