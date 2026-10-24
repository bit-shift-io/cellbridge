# Setup QR Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Print a QR code at the end of the router setup script and add an Android scanner that imports the router-side WireGuard values into the app's configuration form.

**Architecture:** The router script generates or reuses a preshared key, installs `qrencode` while the WAN is still up, and renders a versioned JSON payload as a terminal QR with a text fallback. The app parses that payload in pure Dart, then a `mobile_scanner` screen returns the raw value to `ConfigScreen`, which fills every router-side field while leaving the phone private key untouched.

**Tech Stack:** POSIX `sh` / BusyBox ash on OpenWrt, `qrencode`, Flutter/Dart, `mobile_scanner` 7.4.2.

**Spec:** `docs/superpowers/specs/2026-10-24-setup-qr-import-design.md`

## Global Constraints

- Do **not** create git commits: the operator asked for the feature, not for commits.
- The phone private key must never appear in the QR, the router script's output, Dart error messages, or logs.
- Keep the QR payload prefix exactly `CELLBRIDGE1:`.
- `qrencode` must be installed before the WAN is disabled, and setup must never abort because QR rendering failed.
- `router/cellbridge-setup.sh` must remain POSIX `sh` / BusyBox ash (no bashisms).
- Keep all existing configuration-screen behavior: validation, "disconnect before save/test", key derivation, and the existing tests.
- Resolved dependency: `mobile_scanner` 7.4.2. Flutter's `minSdk` is 24; the plugin requires 23.
- The router script reuses an existing peer preshared key; never rotate it on a re-run.
- The app scan fills address, peer public key, preshared key, endpoint, and MTU only. The private key stays as it was.

## Review Focus

- A QR whose `phonePublicKey` does not match the app's current private key must warn clearly instead of silently producing a broken tunnel.
- A foreign or malformed QR must be rejected generically, leave every field unchanged, and never echo the scanned content.
- A missing or uninstallable `qrencode` must degrade to complete text settings, not fail setup.
- Re-running setup must reuse the preshared key so an already-imported phone keeps working.
- Scanning with no private key must fill the router-side fields but say that a key pair and a matching re-run are still required.

---

### Task 1: Router preshared key

**Files:**
- Modify: `router/cellbridge-setup.sh` (validation helpers near the top, `configure_network` around lines 380-418, summary around lines 612-626)
- Modify: `router/README.md` (peer row in "What setup changes", "At the end it prints")

**Interfaces:**
- Consumes: existing `wg` availability from `ensure_wireguard`, existing `msg`/`warn`/`die` helpers.
- Produces: a global `peer_psk` set inside `configure_network`, plus a reusable `is_wireguard_key` helper. Task 2 reads `peer_psk` when building the QR.

- [ ] **Step 1: Add the key-shape helper**

In `router/cellbridge-setup.sh`, near the existing `msg`/`warn`/`die` definitions, add:

```sh
is_wireguard_key() {
	[ "${#1}" -eq 44 ] || return 1
	case "$1" in
	*=) : ;;
	*) return 1 ;;
	esac
	case "${1%?}" in
	*[!A-Za-z0-9+/]*) return 1 ;;
	esac
	return 0
}
```

- [ ] **Step 2: Reuse or generate the preshared key in `configure_network`**

Before the existing `while uci -q delete "network.@wireguard_wg0[0]"` loop, read the old peer's key and choose the effective one:

```sh
	peer_psk=$(uci -q get 'network.@wireguard_wg0[0].preshared_key')
	if is_wireguard_key "$peer_psk"; then
		msg "Reusing the existing peer preshared key"
	else
		peer_psk=$(wg genpsk) || die "wg genpsk failed"
		is_wireguard_key "$peer_psk" ||
			die "generated preshared key has an unexpected shape"
		msg "Generated a new peer preshared key"
	fi
```

After the existing `uci set "network.$peer.public_key=$PHONE_PUB"`, set it on the new peer without putting it on a process argument list, matching how the private key is handled:

```sh
	uci -q batch <<EOF || die "uci: cannot set the preshared key"
set network.$peer.preshared_key='$peer_psk'
EOF
```

`is_wireguard_key` has already excluded quotes and other non-base64 characters.

- [ ] **Step 3: Print the preshared key in the phone settings block**

In the `Phone WireGuard settings:` block, add a `PresharedKey` line after `PublicKey`:

```sh
msg "  PresharedKey = $peer_psk"
```

- [ ] **Step 4: Update `router/README.md`**

In the "Peer" row of "What setup changes", document `preshared_key` as generated once and reused. In "At the end it prints", add the preshared key to the list of phone settings.

- [ ] **Step 5: Verify the script parses**

Run: `sh -n router/cellbridge-setup.sh`
Expected: no output, exit status 0.

- [ ] **Step 6: Verify on the router (manual)**

Run the script once with a real phone public key (use `--keep-wan` for the rehearsal), then:

```sh
uci -q get network.@wireguard_wg0[0].preshared_key
wg show wg0 preshared-keys
```

Expected: a 44-character base64 key in both places. Re-run the script and confirm the same key is printed; the phone's existing configuration must still handshake.

---

### Task 2: Router QR output

**Files:**
- Modify: `router/cellbridge-setup.sh` (`ensure_qrencode` near `ensure_wireguard`, call after `ensure_wireguard`, QR block after the phone settings summary)
- Modify: `router/README.md` ("At the end it prints", prerequisites/limits)

**Interfaces:**
- Consumes: `peer_psk` from Task 1; existing `router_pub`, `lan_ip`, `PHONE_ADDR`, `WG_PORT`, `WG_MTU`, `PHONE_PUB`.
- Produces: the `CELLBRIDGE1:` payload consumed by the app's parser (Task 3).

- [ ] **Step 1: Add `ensure_qrencode`**

Next to `ensure_wireguard`, add:

```sh
ensure_qrencode() {
	command -v qrencode >/dev/null 2>&1 && return 0
	warn "qrencode not found; trying to install it now"
	opkg update >/dev/null 2>&1 || true
	opkg install qrencode >/dev/null 2>&1 || true
	command -v qrencode >/dev/null 2>&1
}
```

Call it immediately after `ensure_wireguard`:

```sh
if ! ensure_qrencode; then
	warn "qrencode is unavailable; the phone settings will be printed as text"
fi
```

This placement is before the WAN is disabled, which is the only time `opkg` can work.

- [ ] **Step 2: Build the payload and render the QR**

After the existing `Phone WireGuard settings:` block (after the `AllowedIPs` line, before the WAN-state block), add:

```sh
	if [ -z "$router_pub" ]; then
		warn "cannot render the phone QR: the router public key is unavailable"
	else
		qr_payload=$(printf 'CELLBRIDGE1:{"address":"%s","peerPublicKey":"%s","presharedKey":"%s","endpoint":"%s:%s","mtu":%s,"phonePublicKey":"%s"}' \
			"$PHONE_ADDR" "$router_pub" "$peer_psk" "$lan_ip" "$WG_PORT" "$WG_MTU" "$PHONE_PUB")

		msg "Scan this QR code in the CellBridge app:"
		qr_art=
		if command -v qrencode >/dev/null 2>&1; then
			qr_art=$(printf '%s' "$qr_payload" | qrencode -t ANSIUTF8 -m 2 2>/dev/null) || qr_art=
			if [ -z "$qr_art" ]; then
				qr_art=$(printf '%s' "$qr_payload" | qrencode -t UTF8 -m 2 2>/dev/null) || qr_art=
			fi
		fi
		if [ -n "$qr_art" ]; then
			msg "$qr_art"
		else
			warn "could not render a QR code; use the text settings above"
			msg "CellBridge setup payload:"
			msg "$qr_payload"
		fi
	fi
```

The values are all script-owned or validated: `PHONE_ADDR` is a constant, keys are base64, `lan_ip` is numeric, and `WG_MTU`/`WG_PORT` are numeric. No JSON escaping is needed.

- [ ] **Step 3: Update `router/README.md`**

Document that setup prints a QR for the app, that `qrencode` is installed automatically while the WAN is up, and that a text fallback plus the raw `CELLBRIDGE1:` payload is printed if rendering fails. Mention that the preshared key is in the QR, so screenshots of the terminal are sensitive.

- [ ] **Step 4: Verify the script parses**

Run: `sh -n router/cellbridge-setup.sh`
Expected: no output, exit status 0.

- [ ] **Step 5: Verify on the router (manual)**

Run with `--keep-wan`, scan the terminal QR with a phone camera or a QR reader, and confirm the decoded text starts with `CELLBRIDGE1:` and contains `address`, `peerPublicKey`, `presharedKey`, `endpoint`, `mtu`, and `phonePublicKey`. Temporarily run with a `PATH` that hides `qrencode` (for example, `PATH=/bin:/sbin` on a shell where `qrencode` is in `/usr/bin`) and confirm the script still finishes and prints the text fallback plus payload.

---

### Task 3: Setup payload parser

**Files:**
- Create: `lib/src/setup_qr.dart`
- Test: `test/setup_qr_test.dart`

**Interfaces:**
- Consumes: `validateAddress`, `validatePeerPublicKey`, `validatePresharedKey`, `validateEndpoint`, `validateMtuNumber` from `lib/src/settings_store.dart`.
- Produces:
  - `const String setupQrPrefix = 'CELLBRIDGE1:';`
  - `class SetupQrPayload` with `String address`, `String peerPublicKey`, `String? presharedKey`, `String endpoint`, `int mtu`, `String? phonePublicKey`.
  - `SetupQrPayload parseSetupQr(String raw)`, throwing `FormatException` with a generic message.

- [ ] **Step 1: Write the failing parser tests**

Create `test/setup_qr_test.dart`:

```dart
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
    expect(
      parseSetupQr('  ${_encode(_base())}\n').address,
      '10.10.10.2/24',
    );
  });

  test('rejects a foreign prefix', () {
    expect(
      () => parseSetupQr('WIREGUARD1:${jsonEncode(_base())}'),
      throwsFormatException,
    );
  });

  test('rejects malformed JSON', () {
    expect(
      () => parseSetupQr('${setupQrPrefix}{'),
      throwsFormatException,
    );
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
      parseSetupQr('${setupQrPrefix}{"address":"SECRET_MARKER"}');
      fail('expected a FormatException');
    } on FormatException catch (error) {
      expect(error.message, isNot(contains('SECRET_MARKER')));
      expect(error.toString(), isNot(contains('SECRET_MARKER')));
    }
  });
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `flutter test test/setup_qr_test.dart`
Expected: FAIL because `package:cellbridge/src/setup_qr.dart` does not exist.

- [ ] **Step 3: Implement `lib/src/setup_qr.dart`**

Use `jsonDecode`, require a string map, require the prefix, trim the raw value, and validate with the existing helpers. Every failure throws:

```dart
const FormatException('not a CellBridge setup QR code')
```

Return `null` for an absent or empty `presharedKey`/`phonePublicKey`. Reject a `phonePublicKey` that is present but not a valid WireGuard key. Ignore unknown JSON fields.

- [ ] **Step 4: Run the parser tests**

Run: `flutter test test/setup_qr_test.dart`
Expected: all tests pass.

- [ ] **Step 5: Run analysis**

Run: `flutter analyze`
Expected: no issues.

---

### Task 4: Scanner screen, dependency, Android permission

**Files:**
- Create: `lib/src/qr_scan_screen.dart`
- Modify: `pubspec.yaml`
- Modify: `android/app/src/main/AndroidManifest.xml`
- Modify: `pubspec.lock` (via `flutter pub get`)

**Interfaces:**
- Produces: `class QrScanScreen extends StatefulWidget` with `const QrScanScreen({super.key})`; the route pops the first non-empty barcode `rawValue` as a `String`.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Add the dependency**

Run: `flutter pub add mobile_scanner:^7.4.2`
Expected: `mobile_scanner 7.4.2` in `pubspec.yaml` and `pubspec.lock`.

- [ ] **Step 2: Create the scanner screen**

Create `lib/src/qr_scan_screen.dart` with:

- A `Scaffold` titled `Scan router QR`.
- A `MobileScanner` filling the body, with `onDetect: (capture) { ... }`.
- An instruction text over the bottom of the preview.
- A `bool _handled` guard: return while true; ignore empty `capture.barcodes` and empty/`null` `rawValue`; set `_handled = true` and `Navigator.of(context).pop(value)` on the first usable value.
- No custom controller, torch, or zoom in this version.

Use the `mobile_scanner` 7.4.2 callback shape:

```dart
void _onDetect(BarcodeCapture capture) {
  if (_handled || capture.barcodes.isEmpty) return;
  final value = capture.barcodes.first.rawValue;
  if (value == null || value.isEmpty) return;
  _handled = true;
  Navigator.of(context).pop(value);
}
```

- [ ] **Step 3: Declare camera support**

In `android/app/src/main/AndroidManifest.xml`, inside `<manifest>` and before `<application>`, add:

```xml
<uses-permission android:name="android.permission.CAMERA"/>
<uses-feature android:name="android.hardware.camera" android:required="false"/>
```

The plugin also declares these; the explicit entries keep the app's intent visible and are harmless.

- [ ] **Step 4: Verify the project builds**

Run: `flutter pub get`
Expected: resolves without errors.

Run: `flutter analyze`
Expected: no issues.

Run: `flutter build apk --debug`
Expected: build succeeds, confirming the plugin's native code and minSdk are compatible.

- [ ] **Step 5: Device check**

Install the debug APK, open a route that shows `QrScanScreen` (temporarily reachable or after Task 5), and confirm the camera permission prompt appears and a QR decode pops the screen.

---

### Task 5: Config-screen import and wiring

**Files:**
- Modify: `lib/src/config_screen.dart` (constructor, state, build)
- Modify: `lib/main.dart` (`_openConfig`)
- Modify: `test/config_screen_test.dart` (`_pumpScreen` and new tests)

**Interfaces:**
- Consumes: `SetupQrPayload parseSetupQr(String)` from Task 3; `QrScanScreen` from Task 4.
- Produces:
  - `ConfigScreen` constructor parameter `Future<String?> Function(BuildContext context)? scanSetupQr`.
  - A `config-scan-qr` button visible only when the callback is non-null.

- [ ] **Step 1: Extend the test harness and write failing widget tests**

In `test/config_screen_test.dart`:

Add constants near the existing keys:

```dart
const _phonePublicKey = 'DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD=';

String _qrPayload({
  String address = '10.10.10.2/24',
  String peerPublicKey = _peerPublicKey,
  String presharedKey = _presharedKey,
  String endpoint = '192.168.8.1:51820',
  int mtu = 1420,
  String? phonePublicKey = _phonePublicKey,
}) =>
    'CELLBRIDGE1:${jsonEncode({
      'address': address,
      'peerPublicKey': peerPublicKey,
      'presharedKey': presharedKey,
      'endpoint': endpoint,
      'mtu': mtu,
      if (phonePublicKey != null) 'phonePublicKey': phonePublicKey,
    })}';
```

Add `Future<String?> Function(BuildContext)? scanSetupQr` to `_pumpScreen` and pass it to `ConfigScreen`.

Add these tests inside the existing `group('ConfigScreen', ...)`:

```dart
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

  await tester.ensureVisible(find.byKey(const Key('config-test-connection')));
  await tester.tap(find.byKey(const Key('config-test-connection')));
  await tester.pumpAndSettle();
  expect(
    find.text('Authenticated WireGuard handshake succeeded over Wi-Fi.'),
    findsOneWidget,
  );

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

  expect(find.textContaining('expects a different phone key'), findsOneWidget);
});

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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `flutter test test/config_screen_test.dart`
Expected: FAIL because `ConfigScreen` has no `scanSetupQr` parameter and no `config-scan-qr` key.

- [ ] **Step 3: Implement the `ConfigScreen` changes**

In `lib/src/config_screen.dart`:

- Import `setup_qr.dart`.
- Add the constructor parameter and field:

  ```dart
  final Future<String?> Function(BuildContext context)? scanSetupQr;
  ```

- Add `bool _scanning = false;`.
- Add `Future<void> _scanSetupQr()`:
  - Return if `_saving || _scanning`.
  - `setState(() => _scanning = true);`
  - `final raw = await widget.scanSetupQr!(context);`
  - If `!mounted` or `raw == null`, return.
  - `try { final payload = parseSetupQr(raw); ... } on FormatException { _showMessage('That QR code is not a CellBridge setup code.'); }`
  - On success, set `_address.text`, `_peerPublicKey.text`, `_presharedKey.text` (empty for null), `_endpoint.text`, and `_mtu.text`. Do not touch `_privateKey`.
  - Choose the message:
    - If `_privateKey.text.trim().isEmpty`: `Router settings loaded. Generate a key pair and re-run router setup with its public key.`
    - Else if `payload.phonePublicKey != null && _publicKey.text.trim().isNotEmpty && payload.phonePublicKey != _publicKey.text.trim()`: `Router settings loaded, but this router expects a different phone key. Re-run router setup with the public key shown above.`
    - Else: `Router settings loaded. Review, then Save.`
  - In `finally`, if mounted, `setState(() => _scanning = false);`.
  - The controller listeners already invalidate the connection-test result; call `_invalidateTestResult()` once after setting the fields if needed.

- In `build`, before the private-key field, add when `widget.scanSetupQr != null`:

  ```dart
  OutlinedButton.icon(
    key: const Key('config-scan-qr'),
    onPressed: _saving || _scanning ? null : _scanSetupQr,
    icon: const Icon(Icons.qr_code_scanner),
    label: Text(_scanning ? 'Opening camera…' : 'Scan router QR'),
  ),
  ```

- [ ] **Step 4: Wire the production scanner**

In `lib/main.dart`, import `qr_scan_screen.dart` and add to the `ConfigScreen` construction in `_openConfig`:

```dart
scanSetupQr: defaultTargetPlatform == TargetPlatform.android
    ? (context) => Navigator.of(context).push<String>(
          MaterialPageRoute(builder: (_) => const QrScanScreen()),
        )
    : null,
```

- [ ] **Step 5: Run the widget tests**

Run: `flutter test test/config_screen_test.dart`
Expected: all tests pass, including the existing ones.

- [ ] **Step 6: Run analysis and the whole test suite**

Run: `flutter analyze`
Expected: no issues.

Run: `flutter test`
Expected: all tests pass.

---

### Task 6: End-to-end verification

**Files:**
- No new files.

**Interfaces:**
- Consumes: everything from Tasks 1-5.

- [ ] **Step 1: Verify the router script syntax**

Run: `sh -n router/cellbridge-setup.sh`
Expected: exit status 0.

- [ ] **Step 2: Verify the app suite**

Run: `flutter analyze`
Expected: no issues.

Run: `flutter test`
Expected: all tests pass.

- [ ] **Step 3: Build and install the Android app**

Run: `flutter build apk --debug`
Expected: build succeeds.

- [ ] **Step 4: Run the full flow on the real router**

1. Build the debug APK and install it.
2. Open Configuration, tap **Generate key pair**, and copy the phone public key.
3. On the router, run `sh cellbridge-setup.sh --phone-public-key '<key>' --keep-wan`.
4. Confirm the terminal QR is scannable and the text settings include `PresharedKey`.
5. In the app, tap **Scan router QR** and confirm every router-side field fills; the private key does not change.
6. Tap **Save**, then **Test Wi-Fi connection**. Expected: authenticated handshake.
7. Re-run setup without `--keep-wan` and confirm the same preshared key is reused and the connection test still passes.

- [ ] **Step 5: Verify the fallback**

On a router where `qrencode` is not installed and cannot be installed (for example, after the WAN is disabled), run setup again and confirm:

- the script completes,
- the text settings include every field including the preshared key,
- the raw `CELLBRIDGE1:` payload is printed,
- no error aborts the configuration.

Do not commit the result; leave the changes in the working tree.
