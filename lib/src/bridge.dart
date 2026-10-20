import 'rust/api.dart' as rust;
import 'rust/config.dart';
import 'rust/frb_generated.dart';
import 'rust/stats.dart';

// The single seam between Rust and the rest of the app. Everything above this
// file (dashboard, settings, widget tests, the Linux demo) talks to the
// `BridgeApi` interface, so it can be mocked without a running Rust library.

export 'rust/config.dart' show BridgeConfig;
export 'rust/stats.dart' show BridgeState, BridgeStatus, Stats;

/// Initializes the Rust library and registers its message handlers.
///
/// Must complete before any other bridge call.
Future<void> initBridge() => RustLib.init();

/// Round-trip smoke test: the Rust core replies `"pong"`.
Future<String> ping() =>
    // The low-level `api` member is `@internal` by design; this wrapper is the
    // intended single point of contact with it.
    // ignore: invalid_use_of_internal_member
    RustLib.instance.api.crateApiPing();

/// The bridge operations the UI depends on.
///
/// Kept as an interface so widget tests and the Linux demo can inject a fake
/// implementation; the production implementation is [FrbBridgeApi].
abstract class BridgeApi {
  /// Start the bridge with [config].
  ///
  /// Throws if the bridge is already running or the configuration is invalid.
  Future<void> start(BridgeConfig config);

  /// Stop the bridge. Safe to call when it is not running.
  Future<void> stop();

  /// The current lifecycle state, readable synchronously from the UI.
  BridgeStatus status();

  /// The current byte and flow counters.
  Stats stats();
}

/// [BridgeApi] over the generated `flutter_rust_bridge` bindings.
class FrbBridgeApi implements BridgeApi {
  /// [wgSocketFd] is the Wi-Fi-bound WireGuard socket prepared by the Android
  /// service. Ownership of the descriptor transfers to Rust; the caller must
  /// detach it first. Leave it `null` on desktop, where Rust binds its own
  /// socket.
  const FrbBridgeApi({this.wgSocketFd});

  /// Android-provided socket descriptor, if any.
  final int? wgSocketFd;

  @override
  Future<void> start(BridgeConfig config) =>
      rust.startBridge(cfg: config, wgSocketFd: wgSocketFd);

  @override
  Future<void> stop() => rust.stopBridge();

  @override
  BridgeStatus status() => rust.status();

  @override
  Stats stats() => rust.stats();
}

/// The current bridge implementation.
///
/// Settable so the Android service can supply the Wi-Fi-bound socket descriptor
/// (`bridgeApi = FrbBridgeApi(wgSocketFd: fd)`) and so widget tests or the Linux
/// demo can inject a fake. The default is the production, fd-less desktop path.
BridgeApi bridgeApi = const FrbBridgeApi();
