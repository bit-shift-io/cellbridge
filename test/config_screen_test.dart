// Settings persistence and config-form tests.
//
// Every test injects an in-memory [SettingsBackend]; nothing here touches a
// MethodChannel or the real application-support directory.

import 'dart:convert';
import 'dart:async';

import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/config_screen.dart';
import 'package:cellbridge/src/keys.dart';
import 'package:cellbridge/src/settings_store.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

/// RFC 7748 §6.1 (Alice's key pair), base64 of the raw bytes. Not a project
/// key: the same vector is asserted in `rust/tests/keys.rs`.
const _rfcPrivateKey = 'dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=';
const _rfcPublicKey = 'hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=';

/// Base64-shaped 32-byte WireGuard keys. The value is irrelevant here; only
/// the shape matters to validation.
const _privateKey = 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=';
const _peerPublicKey = 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=';
const _presharedKey = 'CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=';
const _phonePublicKey = 'DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD=';

String _qrPayload({
  String address = '10.10.10.2/24',
  String peerPublicKey = _peerPublicKey,
  String presharedKey = _presharedKey,
  String endpoint = '192.168.8.1:51820',
  int mtu = 1420,
  String? phonePublicKey = _phonePublicKey,
}) =>
    'CELLBRIDGE1:${jsonEncode({'address': address, 'peerPublicKey': peerPublicKey, 'presharedKey': presharedKey, 'endpoint': endpoint, 'mtu': mtu, 'phonePublicKey': ?phonePublicKey})}';

const _config = BridgeConfig(
  privateKey: _privateKey,
  address: '10.7.0.2/24',
  peerPublicKey: _peerPublicKey,
  presharedKey: _presharedKey,
  endpoint: '203.0.113.1:51820',
  mtu: 1420,
);

class _MemoryBackend implements SettingsBackend {
  Map<String, Object?>? record;

  @override
  Future<Map<String, Object?>?> read() async =>
      record == null ? null : Map<String, Object?>.of(record!);

  @override
  Future<void> write(Map<String, Object?> value) async {
    record = Map<String, Object?>.of(value);
  }
}

SettingsStore _storeOn(_MemoryBackend backend) =>
    SettingsStore(backend: backend);

/// Stand-in for the Rust key functions.
class _FakeKeyApi implements KeyApi {
  _FakeKeyApi({this.pair, Map<String, String>? derivations})
    : derivations = derivations ?? const {};

  final BridgeKeypair? pair;
  final Map<String, String> derivations;
  int generateCalls = 0;
  final List<String> deriveCalls = [];

  @override
  Future<BridgeKeypair> generate() async {
    generateCalls++;
    return pair!;
  }

  @override
  Future<String> derive(String privateKey) async {
    deriveCalls.add(privateKey);
    final publicKey = derivations[privateKey];
    if (publicKey == null) {
      throw const FormatException('unrecognised test key');
    }
    return publicKey;
  }
}

Future<void> _pumpScreen(
  WidgetTester tester, {
  required SettingsStore store,
  BridgeConfig? initialConfig,
  bool running = false,
  ValueChanged<BridgeConfig>? onSaved,
  KeyApi? keyApi,
  Future<void> Function(BridgeConfig)? testConnection,
  Future<String?> Function(BuildContext)? scanSetupQr,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: ConfigScreen(
        store: store,
        initialConfig: initialConfig,
        bridgeRunning: () => running,
        onSaved: onSaved,
        keyApi: keyApi,
        testConnection: testConnection,
        scanSetupQr: scanSetupQr,
      ),
    ),
  );
}

String _field(WidgetTester tester, String key) =>
    tester.widget<TextFormField>(find.byKey(Key(key))).controller!.text;

/// Finds rendered *messages* (`Text` widgets) containing [text].
///
/// [find.textContaining] alone also matches the input fields' `EditableText`,
/// which legitimately holds whatever the user typed; the requirement is that no
/// displayed message echoes key material.
Finder _messagesContaining(String text) => find.byWidgetPredicate(
  (widget) => widget is Text && (widget.data ?? '').contains(text),
);

Future<void> _save(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('config-save')));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('editing during a connection test discards its stale success', (
    tester,
  ) async {
    final pending = Completer<void>();
    await _pumpScreen(
      tester,
      store: _storeOn(_MemoryBackend()),
      initialConfig: _config,
      testConnection: (_) => pending.future,
    );
    await tester.ensureVisible(find.byKey(const Key('config-test-connection')));
    await tester.tap(find.byKey(const Key('config-test-connection')));
    await tester.pump();
    await tester.enterText(
      find.byKey(const Key('config-endpoint')),
      '192.168.1.99:51820',
    );
    pending.complete();
    await tester.pumpAndSettle();
    expect(
      find.text('Authenticated WireGuard handshake succeeded over Wi-Fi.'),
      findsNothing,
    );
  });
  testWidgets(
    'connection test uses form values without saving and shows authenticated result',
    (tester) async {
      final backend = _MemoryBackend();
      BridgeConfig? tested;
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
        testConnection: (config) async {
          tested = config;
        },
      );
      await tester.enterText(
        find.byKey(const Key('config-endpoint')),
        '192.168.1.126:51820',
      );
      await tester.ensureVisible(
        find.byKey(const Key('config-test-connection')),
      );
      await tester.tap(find.byKey(const Key('config-test-connection')));
      await tester.pumpAndSettle();
      expect(tested?.endpoint, '192.168.1.126:51820');
      expect(backend.record, isNull);
      expect(
        find.text('Authenticated WireGuard handshake succeeded over Wi-Fi.'),
        findsOneWidget,
      );
    },
  );

  testWidgets(
    'connection test failure is visible without exposing exception details',
    (tester) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
        testConnection: (_) async {
          throw StateError('secret key contents');
        },
      );
      await tester.ensureVisible(
        find.byKey(const Key('config-test-connection')),
      );
      await tester.tap(find.byKey(const Key('config-test-connection')));
      await tester.pumpAndSettle();
      expect(
        find.text(
          'Wi-Fi connection test failed. Check Wi-Fi, endpoint and paired keys.',
        ),
        findsOneWidget,
      );
      expect(_messagesContaining('secret key contents'), findsNothing);
    },
  );
  group('SettingsStore', () {
    test('round-trips a config including the preshared key', () async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);

      await store.save(_config);

      expect(await store.load(), _config);
    });

    test('round-trips a config without a preshared key', () async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);
      const config = BridgeConfig(
        privateKey: _privateKey,
        address: '10.7.0.2/24',
        peerPublicKey: _peerPublicKey,
        endpoint: '203.0.113.1:51820',
        mtu: 1420,
      );

      await store.save(config);

      expect(await store.load(), config);
      expect(backend.record!.containsKey('presharedKey'), isFalse);
    });

    test('an empty store returns null', () async {
      final store = _storeOn(_MemoryBackend());

      expect(await store.load(), isNull);
    });

    test('rejects an invalid config before writing', () async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);

      await expectLater(
        store.save(
          const BridgeConfig(
            privateKey: '',
            address: '10.7.0.2/24',
            peerPublicKey: _peerPublicKey,
            endpoint: '203.0.113.1:51820',
            mtu: 1420,
          ),
        ),
        throwsArgumentError,
      );
      expect(backend.record, isNull);
    });
  });

  group('ConfigScreen', () {
    testWidgets('prefills the current config', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
      );

      expect(_field(tester, 'config-private-key'), _privateKey);
      expect(_field(tester, 'config-address'), '10.7.0.2/24');
      expect(_field(tester, 'config-peer-public-key'), _peerPublicKey);
      expect(_field(tester, 'config-preshared-key'), _presharedKey);
      expect(_field(tester, 'config-endpoint'), '203.0.113.1:51820');
      expect(_field(tester, 'config-mtu'), '1420');
    });

    testWidgets('refuses to save with a missing private key', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
      );

      await tester.enterText(find.byKey(const Key('config-private-key')), '');
      await _save(tester);

      expect(backend.record, isNull);
      expect(find.text('Private key is required.'), findsOneWidget);
    });

    testWidgets('refuses to save with a missing endpoint', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
      );

      await tester.enterText(find.byKey(const Key('config-endpoint')), '');
      await _save(tester);

      expect(backend.record, isNull);
      expect(find.text('Endpoint is required.'), findsOneWidget);
    });

    testWidgets('refuses malformed keys, endpoints and MTUs', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
      );

      await tester.enterText(
        find.byKey(const Key('config-private-key')),
        'not-a-key',
      );
      await tester.enterText(
        find.byKey(const Key('config-endpoint')),
        '203.0.113.1',
      );
      await tester.enterText(find.byKey(const Key('config-mtu')), 'nope');
      await _save(tester);

      expect(backend.record, isNull);
      expect(find.textContaining('base64'), findsOneWidget);
      expect(find.textContaining('host:port'), findsOneWidget);
      // The bad key material itself must never be echoed back.
      expect(_messagesContaining('not-a-key'), findsNothing);
    });

    testWidgets('refuses an out-of-range MTU', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
      );

      await tester.enterText(find.byKey(const Key('config-mtu')), '99999');
      await _save(tester);

      expect(backend.record, isNull);
      expect(find.textContaining('between'), findsOneWidget);
    });

    testWidgets('a valid save persists and can be read back', (tester) async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);
      BridgeConfig? saved;
      await _pumpScreen(
        tester,
        store: store,
        onSaved: (config) => saved = config,
      );

      await tester.enterText(
        find.byKey(const Key('config-private-key')),
        _privateKey,
      );
      await tester.enterText(
        find.byKey(const Key('config-address')),
        '10.7.0.2/24',
      );
      await tester.enterText(
        find.byKey(const Key('config-peer-public-key')),
        _peerPublicKey,
      );
      await tester.enterText(
        find.byKey(const Key('config-preshared-key')),
        _presharedKey,
      );
      await tester.enterText(
        find.byKey(const Key('config-endpoint')),
        '203.0.113.1:51820',
      );
      await tester.enterText(find.byKey(const Key('config-mtu')), '1420');
      await _save(tester);

      expect(saved, _config);
      expect(await store.load(), _config);
      expect(find.text('Configuration saved.'), findsOneWidget);
    });

    testWidgets('cancel leaves the stored config untouched', (tester) async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);
      await store.save(_config);
      await _pumpScreen(tester, store: store, initialConfig: _config);

      await tester.enterText(
        find.byKey(const Key('config-endpoint')),
        '198.51.100.9:51820',
      );
      await tester.tap(find.byKey(const Key('config-cancel')));
      await tester.pumpAndSettle();

      expect(await store.load(), _config);
    });

    testWidgets('refuses to save while the bridge is running', (tester) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
        running: true,
      );

      await _save(tester);

      expect(backend.record, isNull);
      expect(find.textContaining('Disconnect'), findsOneWidget);
    });

    testWidgets('surfaces a storage failure instead of failing silently', (
      tester,
    ) async {
      final store = SettingsStore(backend: _FailingBackend());
      await _pumpScreen(tester, store: store, initialConfig: _config);

      await _save(tester);

      expect(find.textContaining('Could not save'), findsOneWidget);
      expect(_messagesContaining(_privateKey), findsNothing);
    });

    testWidgets('generating fills the private key and shows its public key', (
      tester,
    ) async {
      final generatedPrivate = base64Encode(List<int>.filled(32, 7));
      final generatedPublic = base64Encode(List<int>.filled(32, 9));
      final keys = _FakeKeyApi(
        pair: BridgeKeypair(
          privateKey: generatedPrivate,
          publicKey: generatedPublic,
        ),
      );
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        keyApi: keys,
      );

      await tester.tap(find.byKey(const Key('config-generate-keypair')));
      await tester.pumpAndSettle();

      expect(keys.generateCalls, 1);
      expect(_field(tester, 'config-private-key'), generatedPrivate);
      expect(_field(tester, 'config-public-key'), generatedPublic);
      expect(find.textContaining('Phone public key'), findsWidgets);
      expect(find.textContaining('Add this as the peer'), findsOneWidget);
      // The private key is not rendered as a message, only in its obscured
      // input field.
      expect(_messagesContaining(generatedPrivate), findsNothing);
    });

    testWidgets('pasting a valid private key derives the RFC public key', (
      tester,
    ) async {
      final keys = _FakeKeyApi(derivations: {_rfcPrivateKey: _rfcPublicKey});
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        keyApi: keys,
      );

      await tester.enterText(
        find.byKey(const Key('config-private-key')),
        _rfcPrivateKey,
      );
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pumpAndSettle();

      expect(keys.deriveCalls, contains(_rfcPrivateKey));
      expect(_field(tester, 'config-public-key'), _rfcPublicKey);
    });

    testWidgets('a malformed private key shows a neutral hint, not an error', (
      tester,
    ) async {
      final keys = _FakeKeyApi();
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        keyApi: keys,
      );

      await tester.enterText(
        find.byKey(const Key('config-private-key')),
        'not-a-key',
      );
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pumpAndSettle();

      expect(
        find.text('Enter a valid private key to see its public key.'),
        findsOneWidget,
      );
      expect(find.textContaining('must be a base64'), findsNothing);
      expect(keys.deriveCalls, isEmpty);
    });

    testWidgets('copying the public key writes it to the clipboard', (
      tester,
    ) async {
      final calls = <MethodCall>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        calls.add(call);
        return null;
      });
      addTearDown(
        () => messenger.setMockMethodCallHandler(SystemChannels.platform, null),
      );

      final keys = _FakeKeyApi(derivations: {_rfcPrivateKey: _rfcPublicKey});
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        keyApi: keys,
      );

      await tester.enterText(
        find.byKey(const Key('config-private-key')),
        _rfcPrivateKey,
      );
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-copy-public-key')));
      await tester.pump();

      final clipboardCall = calls.firstWhere(
        (call) => call.method == 'Clipboard.setData',
      );
      expect(
        (clipboardCall.arguments as Map<Object?, Object?>)['text'],
        _rfcPublicKey,
      );
      expect(find.text('Phone public key copied.'), findsOneWidget);
    });

    testWidgets('scan fills router settings and keeps the private key', (
      tester,
    ) async {
      final backend = _MemoryBackend();
      await _pumpScreen(
        tester,
        store: _storeOn(backend),
        initialConfig: _config,
        keyApi: _FakeKeyApi(derivations: {_privateKey: _phonePublicKey}),
        scanSetupQr: (_) async => _qrPayload(),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();

      expect(_field(tester, 'config-private-key'), _privateKey);
      expect(_field(tester, 'config-address'), '10.10.10.2/24');
      expect(_field(tester, 'config-peer-public-key'), _peerPublicKey);
      expect(_field(tester, 'config-preshared-key'), _presharedKey);
      expect(_field(tester, 'config-endpoint'), '192.168.8.1:51820');
      expect(_field(tester, 'config-mtu'), '1420');
      expect(backend.record, isNull);
      expect(
        find.text('Router settings loaded. Review, then Save.'),
        findsOneWidget,
      );
    });

    testWidgets('a scanned config saves with the original private key', (
      tester,
    ) async {
      final backend = _MemoryBackend();
      final store = _storeOn(backend);
      await _pumpScreen(
        tester,
        store: store,
        initialConfig: _config,
        keyApi: _FakeKeyApi(derivations: {_privateKey: _phonePublicKey}),
        scanSetupQr: (_) async => _qrPayload(),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();
      await _save(tester);

      expect(await store.load(), isNotNull);
      expect(backend.record!['privateKey'], _privateKey);
      expect(backend.record!['address'], '10.10.10.2/24');
      expect(backend.record!['peerPublicKey'], _peerPublicKey);
      expect(backend.record!['presharedKey'], _presharedKey);
      expect(backend.record!['endpoint'], '192.168.8.1:51820');
      expect(backend.record!['mtu'], 1420);
    });

    testWidgets('scan clears a stale connection-test result', (tester) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
        keyApi: _FakeKeyApi(derivations: {_privateKey: _phonePublicKey}),
        testConnection: (_) async {},
        scanSetupQr: (_) async => _qrPayload(),
      );
      await tester.pumpAndSettle();

      await tester.ensureVisible(
        find.byKey(const Key('config-test-connection')),
      );
      await tester.tap(find.byKey(const Key('config-test-connection')));
      await tester.pumpAndSettle();
      expect(
        find.text('Authenticated WireGuard handshake succeeded over Wi-Fi.'),
        findsOneWidget,
      );

      await tester.ensureVisible(find.byKey(const Key('config-scan-qr')));
      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();
      expect(
        find.text('Authenticated WireGuard handshake succeeded over Wi-Fi.'),
        findsNothing,
      );
    });

    testWidgets('an invalid QR leaves the form alone and shows no payload', (
      tester,
    ) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
        keyApi: _FakeKeyApi(derivations: {_privateKey: _phonePublicKey}),
        scanSetupQr: (_) async => 'CELLBRIDGE1:{"address":"SECRET_MARKER"}',
      );
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();

      expect(
        find.text('That QR code is not a CellBridge setup code.'),
        findsOneWidget,
      );
      expect(_field(tester, 'config-address'), _config.address);
      expect(_field(tester, 'config-endpoint'), _config.endpoint);
      expect(_messagesContaining('SECRET_MARKER'), findsNothing);
    });

    testWidgets('a different phone key from the QR produces a warning', (
      tester,
    ) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
        keyApi: _FakeKeyApi(derivations: {_privateKey: _phonePublicKey}),
        scanSetupQr: (_) async => _qrPayload(phonePublicKey: _peerPublicKey),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();

      expect(
        find.textContaining('expects a different phone key'),
        findsOneWidget,
      );
    });

    testWidgets('scan warns when the phone key cannot be derived', (
      tester,
    ) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
        keyApi: _FakeKeyApi(),
        scanSetupQr: (_) async => _qrPayload(phonePublicKey: _peerPublicKey),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();

      expect(
        find.textContaining('could not verify the phone key'),
        findsOneWidget,
      );
    });

    testWidgets(
      'scan verifies against the current private key, not a stale one',
      (tester) async {
        final newPrivateKey = base64Encode(List<int>.filled(32, 5));
        final newPublicKey = base64Encode(List<int>.filled(32, 6));
        await _pumpScreen(
          tester,
          store: _storeOn(_MemoryBackend()),
          initialConfig: _config,
          keyApi: _FakeKeyApi(
            derivations: {
              _privateKey: _phonePublicKey,
              newPrivateKey: newPublicKey,
            },
          ),
          scanSetupQr: (_) async => _qrPayload(phonePublicKey: newPublicKey),
        );
        await tester.pumpAndSettle();

        await tester.enterText(
          find.byKey(const Key('config-private-key')),
          newPrivateKey,
        );
        await tester.tap(find.byKey(const Key('config-scan-qr')));
        await tester.pumpAndSettle();

        expect(
          find.text('Router settings loaded. Review, then Save.'),
          findsOneWidget,
        );
        expect(find.textContaining('different phone key'), findsNothing);
      },
    );

    testWidgets('scanning with no private key asks for a key pair', (
      tester,
    ) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        scanSetupQr: (_) async => _qrPayload(),
      );

      await tester.tap(find.byKey(const Key('config-scan-qr')));
      await tester.pumpAndSettle();

      expect(_field(tester, 'config-private-key'), '');
      expect(
        find.textContaining('Generate a key pair and re-run router setup'),
        findsOneWidget,
      );
    });

    testWidgets('no scan button without a scanner callback', (tester) async {
      await _pumpScreen(
        tester,
        store: _storeOn(_MemoryBackend()),
        initialConfig: _config,
      );

      expect(find.byKey(const Key('config-scan-qr')), findsNothing);
    });
  });
}

class _FailingBackend implements SettingsBackend {
  @override
  Future<Map<String, Object?>?> read() async => null;

  @override
  Future<void> write(Map<String, Object?> value) async {
    throw StateError('settings channel unavailable');
  }
}
