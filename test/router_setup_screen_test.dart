import 'package:cellbridge/src/keys.dart';
import 'package:cellbridge/src/router_setup_screen.dart';
import 'package:cellbridge/src/router_setup_api.dart';
import 'package:cellbridge/src/settings_store.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const private = 'dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=';
const public = 'hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=';

class MemorySettings implements SettingsBackend {
  Map<String, Object?>? value;
  @override Future<Map<String, Object?>?> read() async => value;
  @override Future<void> write(Map<String, Object?> record) async { value = record; }
}
class Keys implements KeyApi {
  @override Future<BridgeKeypair> generate() async => const BridgeKeypair(privateKey: private, publicKey: public);
  @override Future<String> derive(String privateKey) async => public;
}
class FakeSetup implements RouterSetupApi {
  String? identity;
  int starts = 0;
  int cancels = 0;
  String mode = 'waitingForInternet';
  @override Future<String> gateway() async => '192.168.8.1';
  @override Future<String?> readIdentity() async => identity;
  @override Future<void> saveIdentity(String key) async { identity = key; }
  @override Future<String?> hostPin(String endpoint) async => 'SHA256:known';
  @override Future<void> saveHostPin(String endpoint, String fingerprint) async {}
  @override Future<void> start(Map<String, Object?> input) async {
    expect(input['password'], 'temporary-password');
    expect(input['phonePublicKey'], public);
    expect(identity, private, reason: 'identity must persist before remote mutation');
    starts++;
  }
  @override Future<Map<String, dynamic>> snapshot() async => {
    'state': mode,
    'logs': ['Installing dependencies', if (mode == 'waitingForInternet') 'Restore router internet, then Retry'],
    if (mode == 'error') 'error': 'priority is already used; choose another priority',
    if (mode == 'success') 'result': {
      'address': '10.10.10.2/24', 'peerPublicKey': public,
      'presharedKey': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      'endpoint': '192.168.8.1:51820', 'mtu': 1420, 'phonePublicKey': public,
    },
  };
  @override Future<void> cancel() async { cancels++; }
}

Future<void> open(WidgetTester tester, FakeSetup api, MemorySettings settings) async {
  await tester.pumpWidget(MaterialApp(home: RouterSetupScreen(
    api: api, keyApi: Keys(), store: SettingsStore(backend: settings), bridgeRunning: () => false,
  )));
  await tester.pumpAndSettle();
  await tester.enterText(find.byKey(const Key('router-password')), 'temporary-password');
  await tester.tap(find.text('Set up this phone'));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
}

void main() {
  testWidgets('offline setup displays live output and retries with the same identity', (tester) async {
    final api = FakeSetup(); final settings = MemorySettings();
    await open(tester, api, settings);
    expect(find.text('Waiting for router internet'), findsOneWidget);
    expect(find.textContaining('Installing dependencies'), findsOneWidget);
    expect(settings.value, isNull);
    api.mode = 'success';
    await tester.tap(find.text('Retry'));
    await tester.pump(); await tester.pump(const Duration(milliseconds: 600));
    expect(api.starts, 2);
    expect(settings.value?['privateKey'], private);
    expect(settings.value?['endpoint'], '192.168.8.1:51820');
    expect(find.text('Router configured. Settings saved.'), findsOneWidget);
    final passwordField = tester.widget<TextFormField>(find.byKey(const Key('router-password')));
    expect(passwordField.controller!.text, isEmpty);
    await tester.pumpWidget(const SizedBox()); await tester.pump();
    expect(api.cancels, greaterThan(0));
  });

  testWidgets('script failure retains useful output without importing settings', (tester) async {
    final api = FakeSetup()..mode = 'error'; final settings = MemorySettings();
    await open(tester, api, settings);
    expect(find.textContaining('priority is already used'), findsOneWidget);
    expect(find.textContaining('Installing dependencies'), findsOneWidget);
    expect(settings.value, isNull);
    expect(find.text('Copy output'), findsOneWidget);
    await tester.pumpWidget(const SizedBox()); await tester.pump();
  });
}
