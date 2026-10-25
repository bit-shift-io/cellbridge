import 'dart:convert';

import 'settings_store.dart';

/// Prefix that identifies a CellBridge setup QR payload.
const String setupQrPrefix = 'CELLBRIDGE1:';

/// Router settings and an optional generated phone key imported from a setup QR.
class SetupQrPayload {
  const SetupQrPayload({
    required this.address,
    required this.peerPublicKey,
    required this.endpoint,
    required this.mtu,
    this.presharedKey,
    this.phonePublicKey,
    this.phonePrivateKey,
  });

  final String address;
  final String peerPublicKey;
  final String? presharedKey;
  final String endpoint;
  final int mtu;
  final String? phonePublicKey;
  final String? phonePrivateKey;
}

/// Parses the text encoded in a setup QR code.
///
/// Every failure is a generic [FormatException]; the scanned content is never
/// echoed in the message.
SetupQrPayload parseSetupQr(String raw) {
  final text = raw.trim();
  if (!text.startsWith(setupQrPrefix)) {
    _invalid();
  }

  final Object? decoded;
  try {
    decoded = jsonDecode(text.substring(setupQrPrefix.length));
  } on FormatException {
    _invalid();
  }
  if (decoded is! Map) {
    _invalid();
  }
  final map = decoded.cast<String, Object?>();

  String requiredString(String key) {
    final value = map[key];
    if (value is! String || value.isEmpty) {
      _invalid();
    }
    return value;
  }

  String? optionalString(String key) {
    final value = map[key];
    if (value == null) {
      return null;
    }
    if (value is! String) {
      _invalid();
    }
    return value.isEmpty ? null : value;
  }

  final address = requiredString('address');
  final peerPublicKey = requiredString('peerPublicKey');
  final endpoint = requiredString('endpoint');
  final mtuValue = map['mtu'];
  if (mtuValue is! int) {
    _invalid();
  }
  final presharedKey = optionalString('presharedKey');
  final phonePublicKey = optionalString('phonePublicKey');
  // If supplied, an empty/null/wrong-type private key is a broken provisioned
  // payload, not permission to silently keep using an unrelated phone key.
  final phonePrivateKey = map.containsKey('phonePrivateKey')
      ? requiredString('phonePrivateKey').trim()
      : null;

  if (validateAddress(address) != null ||
      validatePeerPublicKey(peerPublicKey) != null ||
      validatePresharedKey(presharedKey) != null ||
      validateEndpoint(endpoint) != null ||
      validateMtuNumber(mtuValue) != null ||
      (phonePrivateKey != null &&
          (validatePrivateKey(phonePrivateKey) != null ||
              phonePublicKey == null)) ||
      (phonePublicKey != null &&
          validatePeerPublicKey(phonePublicKey) != null)) {
    _invalid();
  }

  return SetupQrPayload(
    address: address,
    peerPublicKey: peerPublicKey,
    presharedKey: presharedKey,
    endpoint: endpoint,
    mtu: mtuValue,
    phonePublicKey: phonePublicKey,
    phonePrivateKey: phonePrivateKey,
  );
}

Never _invalid() =>
    throw const FormatException('not a CellBridge setup QR code');
