import 'rust/frb_generated.dart';

// Thin wrapper around the `flutter_rust_bridge`-generated Rust API. The rest
// of the app talks to Rust only through this file, so regenerating the
// bindings cannot ripple into UI code.

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
