import 'package:cellbridge/src/bridge.dart';
import 'package:flutter_test/flutter_test.dart';

/// Proves the seam the dashboard and tests will build against: the top-level
/// `bridgeApi` can be swapped for a fake without touching Rust.
class _FakeBridgeApi implements BridgeApi {
  BridgeConfig? started;
  bool stopped = false;

  @override
  Future<void> start(BridgeConfig config) async {
    started = config;
  }

  @override
  Future<void> stop() async {
    stopped = true;
  }

  @override
  BridgeStatus status() => const BridgeStatus(state: BridgeState.connected);

  @override
  Stats stats() => Stats(
    txBytes: BigInt.from(0),
    rxBytes: BigInt.from(0),
    activeFlows: BigInt.from(0),
    txBytesPerSec: BigInt.from(0),
    rxBytesPerSec: BigInt.from(0),
  );
}

void main() {
  test('bridgeApi is injectable', () async {
    final original = bridgeApi;
    final fake = _FakeBridgeApi();
    bridgeApi = fake;
    addTearDown(() => bridgeApi = original);

    const config = BridgeConfig(
      privateKey: 'private',
      address: '10.7.0.2/24',
      peerPublicKey: 'public',
      endpoint: '192.0.2.1:51820',
      mtu: 1420,
    );

    await bridgeApi.start(config);
    expect(fake.started, same(config));
    expect(bridgeApi.status().state, BridgeState.connected);
    expect(bridgeApi.stats().activeFlows, BigInt.from(0));

    await bridgeApi.stop();
    expect(fake.stopped, isTrue);
  });
}
