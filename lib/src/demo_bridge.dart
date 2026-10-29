import 'dart:async';
import 'dart:math';

import 'bridge.dart';

/// A fake [BridgeApi] for running the dashboard on a desktop with no phone.
///
/// It simulates a believable session: `Idle` -> `Connecting` (~1.5 s) ->
/// `Connected`, with counters that grow while connected and a flow count that
/// drifts up and down. [triggerError] makes the error UI reachable.
class DemoBridgeApi implements BridgeApi {
  DemoBridgeApi({
    this.connectDelay = const Duration(milliseconds: 1500),
    this.tickInterval = const Duration(milliseconds: 500),
    Random? random,
  }) : _random = random ?? Random();

  /// How long the simulated handshake takes.
  final Duration connectDelay;

  /// How often the simulated counters change.
  final Duration tickInterval;

  final Random _random;

  BridgeState _state = BridgeState.idle;
  String? _error;
  BigInt _tx = BigInt.zero;
  BigInt _rx = BigInt.zero;
  BigInt _txRate = BigInt.zero;
  BigInt _rxRate = BigInt.zero;
  int _flows = 0;
  Timer? _ticker;
  int _generation = 0;

  @override
  Future<void> start(BridgeConfig config) async {
    if (_state != BridgeState.idle) {
      throw StateError('the demo bridge is already running');
    }

    final generation = ++_generation;
    _state = BridgeState.connecting;
    _error = null;
    _tx = BigInt.zero;
    _rx = BigInt.zero;
    _txRate = BigInt.zero;
    _rxRate = BigInt.zero;
    _flows = 0;

    await Future<void>.delayed(connectDelay);
    if (generation != _generation || _state != BridgeState.connecting) {
      return;
    }

    _state = BridgeState.connected;
    _flows = 4;
    _ticker = Timer.periodic(tickInterval, (_) => _tick());
  }

  @override
  Future<void> stop() async {
    _generation++;
    _ticker?.cancel();
    _ticker = null;
    _state = BridgeState.idle;
    _error = null;
    _tx = BigInt.zero;
    _rx = BigInt.zero;
    _txRate = BigInt.zero;
    _rxRate = BigInt.zero;
    _flows = 0;
  }

  /// Simulates losing the uplink, as the Rust core eventually would.
  void triggerError([String message = 'demo: uplink lost']) {
    if (_state == BridgeState.idle) {
      return;
    }
    _generation++;
    _ticker?.cancel();
    _ticker = null;
    _state = BridgeState.error;
    _error = message;
    _txRate = BigInt.zero;
    _rxRate = BigInt.zero;
    _flows = 0;
  }

  void _tick() {
    final rxDelta = BigInt.from(20 * 1024 + _random.nextInt(180 * 1024));
    final txDelta = BigInt.from(8 * 1024 + _random.nextInt(90 * 1024));
    _rx += rxDelta;
    _tx += txDelta;
    _rxRate =
        rxDelta *
        BigInt.from(1000000) ~/
        BigInt.from(tickInterval.inMicroseconds);
    _txRate =
        txDelta *
        BigInt.from(1000000) ~/
        BigInt.from(tickInterval.inMicroseconds);
    final drift = _random.nextInt(5) - 2;
    _flows = (_flows + drift).clamp(1, 24).toInt();
  }

  @override
  BridgeStatus status() => BridgeStatus(state: _state, error: _error);

  @override
  Stats stats() => Stats(
    txBytes: _tx,
    rxBytes: _rx,
    activeFlows: BigInt.from(_flows),
    txBytesPerSec: _txRate,
    rxBytesPerSec: _rxRate,
  );
}
