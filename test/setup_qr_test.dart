import 'dart:convert';

import 'package:cellbridge/src/setup_qr.dart';
import 'package:flutter_test/flutter_test.dart';

const _peerPublicKey = 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=';
const _presharedKey = 'CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=';
const _phonePublicKey = 'DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD=';

Map<String, Object?> _base() => {
  'address': '10.10.10.2/24',
  'peerPublicKey': _peerPublicKey,
  'presharedKey': _presharedKey,
  'endpoint': '192.168.8.1:51820',
  'mtu': 1420,
  'phonePublicKey': _phonePublicKey,
};

String _encode(Map<String, Object?> value) =>
    '$setupQrPrefix${jsonEncode(value)}';

void main() {
  test('parses a complete setup payload', () {
    final parsed = parseSetupQr(_encode(_base()));

    expect(parsed.address, '10.10.10.2/24');
    expect(parsed.peerPublicKey, _peerPublicKey);
    expect(parsed.presharedKey, _presharedKey);
    expect(parsed.endpoint, '192.168.8.1:51820');
    expect(parsed.mtu, 1420);
    expect(parsed.phonePublicKey, _phonePublicKey);
  });

  test('treats an absent or empty preshared key as null', () {
    final without = _base()..remove('presharedKey');
    expect(parseSetupQr(_encode(without)).presharedKey, isNull);

    final empty = _base()..['presharedKey'] = '';
    expect(parseSetupQr(_encode(empty)).presharedKey, isNull);
  });

  test('treats an absent phone public key as null', () {
    final without = _base()..remove('phonePublicKey');
    expect(parseSetupQr(_encode(without)).phonePublicKey, isNull);
  });

  test('trims surrounding whitespace', () {
    expect(parseSetupQr('  ${_encode(_base())}\n').address, '10.10.10.2/24');
  });

  test('rejects a foreign prefix', () {
    expect(
      () => parseSetupQr('WIREGUARD1:${jsonEncode(_base())}'),
      throwsFormatException,
    );
  });

  test('rejects malformed JSON', () {
    expect(() => parseSetupQr('$setupQrPrefix{'), throwsFormatException);
  });

  test('rejects missing required fields', () {
    for (final key in ['address', 'peerPublicKey', 'endpoint', 'mtu']) {
      final value = _base()..remove(key);
      expect(
        () => parseSetupQr(_encode(value)),
        throwsFormatException,
        reason: key,
      );
    }
  });

  test('rejects wrong field types', () {
    final value = _base()..['mtu'] = '1420';
    expect(() => parseSetupQr(_encode(value)), throwsFormatException);
  });

  test('rejects invalid field values', () {
    final cases = <String, Object?>{
      'address': 'not-a-cidr',
      'peerPublicKey': 'not-a-key',
      'presharedKey': 'not-a-key',
      'endpoint': 'not-a-host-port',
      'mtu': 99999,
      'phonePublicKey': 'not-a-key',
    };

    for (final entry in cases.entries) {
      final value = _base()..[entry.key] = entry.value;
      expect(
        () => parseSetupQr(_encode(value)),
        throwsFormatException,
        reason: entry.key,
      );
    }
  });

  test('error messages never echo the scanned payload', () {
    try {
      parseSetupQr('$setupQrPrefix{"address":"SECRET_MARKER"}');
      fail('expected a FormatException');
    } on FormatException catch (error) {
      expect(error.message, isNot(contains('SECRET_MARKER')));
      expect(error.toString(), isNot(contains('SECRET_MARKER')));
    }
  });
}
