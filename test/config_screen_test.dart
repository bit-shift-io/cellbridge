// Settings persistence and config-form tests.
//
// Every test injects an in-memory [SettingsBackend]; nothing here touches a
// MethodChannel or the real application-support directory.

import 'package:cellbridge/src/bridge.dart';
import 'package:cellbridge/src/config_screen.dart';
import 'package:cellbridge/src/settings_store.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Base64-shaped 32-byte WireGuard keys. The value is irrelevant here; only
/// the shape matters to validation.
const _privateKey = 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=';
const _peerPublicKey = 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=';
const _presharedKey = 'CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=';

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

Future<void> _pumpScreen(
  WidgetTester tester, {
  required SettingsStore store,
  BridgeConfig? initialConfig,
  bool running = false,
  ValueChanged<BridgeConfig>? onSaved,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: ConfigScreen(
        store: store,
        initialConfig: initialConfig,
        bridgeRunning: () => running,
        onSaved: onSaved,
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
  await tester.ensureVisible(find.byKey(const Key('config-save')));
  await tester.tap(find.byKey(const Key('config-save')));
  await tester.pumpAndSettle();
}

void main() {
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
