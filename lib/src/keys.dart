import 'rust/api.dart' as rust;
import 'rust/keys.dart';

export 'rust/keys.dart' show BridgeKeypair;

/// The key operations the configuration form needs.
///
/// Kept as an interface (like `BridgeApi`) so widget tests can exercise the
/// form without loading the Rust library. The production implementation is
/// [FrbKeyApi].
abstract class KeyApi {
  /// Generates a fresh WireGuard key pair with the OS CSPRNG.
  Future<BridgeKeypair> generate();

  /// Derives the base64 public key for [privateKey].
  ///
  /// Throws when the value is not base64 or is not exactly 32 bytes. The error
  /// never contains the key material.
  Future<String> derive(String privateKey);
}

/// [KeyApi] over the generated `flutter_rust_bridge` bindings.
class FrbKeyApi implements KeyApi {
  const FrbKeyApi();

  @override
  Future<BridgeKeypair> generate() => rust.generateKeypair();

  @override
  Future<String> derive(String privateKey) =>
      rust.derivePublicKey(privateKey: privateKey);
}

/// The current key implementation; injectable for tests and demos.
KeyApi keyApi = const FrbKeyApi();
