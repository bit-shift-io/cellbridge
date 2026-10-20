// Storage for the bridge configuration.
//
// The WireGuard *private key* must never sit in plaintext on Android, so the
// storage story is deliberately split:
//
//  * Android uses [MethodChannelSettingsBackend], which delegates to the native
//    `dev.cellbridge/settings` channel backed by `EncryptedSharedPreferences`.
//    The native side is not implemented in this file (see the Android service
//    task); until it lands the channel call simply fails, and that failure is
//    surfaced rather than downgraded to an unencrypted store.
//  * Desktop and tests use [FileSettingsBackend] (a JSON file under the app
//    support directory, written owner-only where the platform allows) or an
//    injected in-memory fake. This is a *development* path, not the Android
//    storage story.
//
// [SettingsStore] itself stays thin: validate, serialise, delegate.

import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'bridge.dart';

/// The key/value store a [SettingsStore] persists through.
///
/// Implementations return the whole record as a string-keyed map, or `null`
/// when nothing has been saved yet. Backends are injectable so widget tests can
/// use an in-memory fake and never touch a channel or the filesystem.
abstract class SettingsBackend {
  /// Reads the stored record, or `null` when the store is empty.
  Future<Map<String, Object?>?> read();

  /// Replaces the stored record with [value].
  Future<void> write(Map<String, Object?> value);
}

/// The channel name the Android `ConfigStore` listens on.
const MethodChannel androidSettingsChannel = MethodChannel(
  'dev.cellbridge/settings',
);

/// Android backend: delegates to native `EncryptedSharedPreferences`.
///
/// The private key is encrypted at rest by the native side. The Kotlin
/// implementation belongs to the Android service task; until it exists the
/// channel throws `MissingPluginException`, which propagates to the caller.
class MethodChannelSettingsBackend implements SettingsBackend {
  const MethodChannelSettingsBackend([this.channel = androidSettingsChannel]);

  final MethodChannel channel;

  @override
  Future<Map<String, Object?>?> read() async {
    final result = await channel.invokeMethod<Object?>('read');
    if (result == null) {
      return null;
    }
    if (result is! Map) {
      throw StateError('settings channel returned ${result.runtimeType}');
    }
    return <String, Object?>{
      for (final entry in result.entries) entry.key.toString(): entry.value,
    };
  }

  @override
  Future<void> write(Map<String, Object?> value) =>
      channel.invokeMethod<void>('write', value);
}

/// Desktop/dev backend: a JSON file under the application-support directory.
///
/// This is a development convenience only. It writes the record to disk in
/// plaintext (owner-only permissions where the platform allows) and must not be
/// used as the Android storage story.
class FileSettingsBackend implements SettingsBackend {
  FileSettingsBackend({Future<Directory> Function()? directory})
    : _directory = directory ?? getApplicationSupportDirectory;

  final Future<Directory> Function() _directory;

  static const _fileName = 'settings.json';

  Future<File> _file() async {
    final directory = await _directory();
    return File('${directory.path}${Platform.pathSeparator}$_fileName');
  }

  @override
  Future<Map<String, Object?>?> read() async {
    final file = await _file();
    if (!await file.exists()) {
      return null;
    }
    final text = await file.readAsString();
    if (text.trim().isEmpty) {
      return null;
    }
    final Object? decoded;
    try {
      decoded = jsonDecode(text);
    } on FormatException {
      // Never let jsonDecode's message (which quotes the source) escape: the
      // source may hold the private key.
      throw const FormatException('settings file is not valid JSON');
    }
    if (decoded is! Map) {
      throw const FormatException('settings file is not a JSON object');
    }
    return <String, Object?>{
      for (final entry in decoded.entries) entry.key.toString(): entry.value,
    };
  }

  @override
  Future<void> write(Map<String, Object?> value) async {
    final file = await _file();
    await file.parent.create(recursive: true);
    await file.writeAsString(jsonEncode(value), flush: true);
    await _restrictPermissions(file);
  }

  /// Best-effort `chmod 0600`, skipped on platforms without POSIX modes.
  Future<void> _restrictPermissions(File file) async {
    if (kIsWeb || Platform.isWindows) {
      return;
    }
    try {
      await Process.run('chmod', ['600', file.path]);
    } on ProcessException {
      // A filesystem without chmod (or without a `chmod` binary) is not fatal
      // for this development-only path.
    }
  }
}

/// Picks the backend for the current platform.
///
/// Android gets the encrypted native store; everything else gets the JSON
/// development file.
SettingsBackend createSettingsBackend() {
  if (!kIsWeb && Platform.isAndroid) {
    return const MethodChannelSettingsBackend();
  }
  return FileSettingsBackend();
}

/// Persists and loads [BridgeConfig] values, delegating the bytes to a
/// [SettingsBackend].
class SettingsStore {
  const SettingsStore({required this.backend});

  final SettingsBackend backend;

  /// Loads the saved configuration, or `null` when nothing is stored.
  Future<BridgeConfig?> load() async {
    final record = await backend.read();
    if (record == null) {
      return null;
    }
    return decodeBridgeConfig(record);
  }

  /// Validates and saves [config].
  ///
  /// Throws [ArgumentError] — without echoing any key material — when [config]
  /// is invalid. The backend is left untouched in that case.
  Future<void> save(BridgeConfig config) async {
    final validation = validateBridgeConfig(config);
    if (!validation.isValid) {
      throw ArgumentError(
        validation.firstError ?? 'invalid bridge configuration',
      );
    }
    await backend.write(encodeBridgeConfig(config));
  }
}

/// Serialises [config] to a plain map. The preshared key is omitted entirely
/// when unset, so an absent key round-trips as `null`.
Map<String, Object?> encodeBridgeConfig(BridgeConfig config) => {
  'privateKey': config.privateKey,
  'address': config.address,
  'peerPublicKey': config.peerPublicKey,
  if (config.presharedKey != null) 'presharedKey': config.presharedKey,
  'endpoint': config.endpoint,
  'mtu': config.mtu,
};

/// Rebuilds a [BridgeConfig] from a stored record.
///
/// Throws [FormatException] (with no field values in the message) when a
/// required field is missing or the wrong type.
BridgeConfig decodeBridgeConfig(Map<String, Object?> record) {
  String string(String key) {
    final value = record[key];
    if (value is! String) {
      throw FormatException('settings record is missing "$key"');
    }
    return value;
  }

  final mtu = record['mtu'];
  if (mtu is! int) {
    throw const FormatException('settings record is missing "mtu"');
  }
  final presharedKey = record['presharedKey'];

  return BridgeConfig(
    privateKey: string('privateKey'),
    address: string('address'),
    peerPublicKey: string('peerPublicKey'),
    presharedKey: presharedKey is String && presharedKey.isNotEmpty
        ? presharedKey
        : null,
    endpoint: string('endpoint'),
    mtu: mtu,
  );
}

// ---------------------------------------------------------------------------
// Validation
//
// Kept here (rather than in the UI) because [SettingsStore.save] is the
// enforcement point; the config screen reuses the same rules for field errors.
// ---------------------------------------------------------------------------

/// Smallest MTU the WireGuard tunnel accepts, per the task brief.
const int minMtu = 1280;

/// Largest MTU the WireGuard tunnel accepts, per the task brief.
const int maxMtu = 1500;

/// A per-field validation result for a bridge configuration.
class BridgeConfigValidation {
  const BridgeConfigValidation({
    this.privateKey,
    this.address,
    this.peerPublicKey,
    this.presharedKey,
    this.endpoint,
    this.mtu,
  });

  final String? privateKey;
  final String? address;
  final String? peerPublicKey;
  final String? presharedKey;
  final String? endpoint;
  final String? mtu;

  bool get isValid =>
      privateKey == null &&
      address == null &&
      peerPublicKey == null &&
      presharedKey == null &&
      endpoint == null &&
      mtu == null;

  /// The first error message in field order, for callers that need one line.
  String? get firstError =>
      privateKey ?? address ?? peerPublicKey ?? presharedKey ?? endpoint ?? mtu;
}

/// Validates every field of [config].
BridgeConfigValidation validateBridgeConfig(BridgeConfig config) =>
    BridgeConfigValidation(
      privateKey: validatePrivateKey(config.privateKey),
      address: validateAddress(config.address),
      peerPublicKey: validatePeerPublicKey(config.peerPublicKey),
      presharedKey: validatePresharedKey(config.presharedKey),
      endpoint: validateEndpoint(config.endpoint),
      mtu: validateMtuNumber(config.mtu),
    );

/// Errors for a required WireGuard private key.
String? validatePrivateKey(String value) => _requiredKey(value, 'Private key');

/// Errors for a required peer public key.
String? validatePeerPublicKey(String value) =>
    _requiredKey(value, 'Peer public key');

/// Errors for the optional preshared key. A blank value means "not used".
String? validatePresharedKey(String? value) {
  final trimmed = value?.trim() ?? '';
  if (trimmed.isEmpty) {
    return null;
  }
  if (!_isWireGuardKey(trimmed)) {
    return 'Preshared key must be a base64-encoded 32-byte key.';
  }
  return null;
}

/// Errors for the interface address.
String? validateAddress(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) {
    return 'Address is required.';
  }
  if (!_isCidr(trimmed)) {
    return 'Address must be in CIDR form, e.g. 10.7.0.2/24.';
  }
  return null;
}

/// Errors for the peer endpoint.
String? validateEndpoint(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) {
    return 'Endpoint is required.';
  }
  if (!_isHostPort(trimmed)) {
    return 'Endpoint must be in host:port form.';
  }
  return null;
}

/// Errors for the MTU as entered in the form.
String? validateMtu(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) {
    return 'MTU is required.';
  }
  final parsed = int.tryParse(trimmed);
  if (parsed == null) {
    return 'MTU must be a number between $minMtu and $maxMtu.';
  }
  return validateMtuNumber(parsed);
}

/// Errors for an already-parsed MTU value.
String? validateMtuNumber(int value) {
  if (value < minMtu || value > maxMtu) {
    return 'MTU must be a number between $minMtu and $maxMtu.';
  }
  return null;
}

String? _requiredKey(String value, String label) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) {
    return '$label is required.';
  }
  if (!_isWireGuardKey(trimmed)) {
    return '$label must be a base64-encoded 32-byte key.';
  }
  return null;
}

/// A WireGuard key is 32 bytes encoded as 44 base64 characters: exactly 43
/// base64-alphabet characters and a single `=` pad.
///
/// This is a *shape* check rather than a canonical decode. Dart's
/// `base64.decode` rejects non-canonical trailing bits, which would reject
/// perfectly shape-valid text; the WireGuard core is the authority on whether a
/// key is usable.
final RegExp _wireGuardKeyPattern = RegExp(r'^[A-Za-z0-9+/]{43}=$');

bool _isWireGuardKey(String value) => _wireGuardKeyPattern.hasMatch(value);

/// IPv4 CIDR such as `10.7.0.2/24`.
bool _isCidr(String value) {
  final parts = value.split('/');
  if (parts.length != 2) {
    return false;
  }
  final octets = parts[0].split('.');
  if (octets.length != 4) {
    return false;
  }
  for (final octet in octets) {
    final parsed = int.tryParse(octet);
    if (parsed == null || parsed < 0 || parsed > 255) {
      return false;
    }
  }
  final prefix = int.tryParse(parts[1]);
  return prefix != null && prefix >= 0 && prefix <= 32;
}

/// `host:port`, with IPv6 hosts wrapped in brackets. The port must be numeric
/// and in range.
bool _isHostPort(String value) {
  final RegExp pattern;
  if (value.startsWith('[')) {
    pattern = RegExp(r'^\[[0-9a-fA-F:]+\]:(\d{1,5})$');
  } else {
    pattern = RegExp(r'^[^\s:]+:(\d{1,5})$');
  }
  final match = pattern.firstMatch(value);
  if (match == null) {
    return false;
  }
  final port = int.tryParse(match.group(1)!);
  return port != null && port >= 1 && port <= 65535;
}
