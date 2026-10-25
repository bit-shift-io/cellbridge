# Setup QR import — design

Date: 2026-10-24
Status: awaiting approval

**Update (2026-10-26):** The approved generated-phone-key flow in
[`../plans/2026-10-26-generated-phone-qr.md`](../plans/2026-10-26-generated-phone-qr.md)
supersedes this document's phone-private-key exclusion. Default router setup now
generates/reuses the phone pair and includes it in the QR; the app asks Yes/No
before replacing different existing keys. Explicit phone-managed keys and these
older public-only QR payloads remain supported.

## Intent

After `router/cellbridge-setup.sh` runs, the operator must currently retype the
phone-side WireGuard settings (address, router public key, endpoint, MTU,
preshared key) on a phone keyboard. That is the painful part of setup.

Make the router script print a QR code containing every phone-side value it
knows, and give the app a scanner that imports those values into the
configuration form.

## Goals

- The router script prints a terminal QR code after a successful run.
- The app can scan that QR and fill the configuration form.
- The QR carries as much of the app's configuration as the router can know:
  address, peer (router) public key, preshared key, endpoint, MTU, and the phone
  public key the router was configured with (for validation).
- The router script generates a WireGuard preshared key when none exists, reuses
  an existing one on re-runs, and includes it in the QR and text output.
- The script installs `qrencode` while the WAN is still up, and falls back to
  the existing text output if the package is unavailable or rendering fails.
- The app still never receives the phone private key from the router.
- The router, as the server, owns preshared-key generation. The app does not add
  PSK generation; its existing optional PSK field is simply filled by the scan.

## Non-goals

- Carrying the phone private key in the QR. The router never sees it and must
  not start seeing it.
- Changing the Rust bridge, the settings record format, or the Android native
  settings store. `presharedKey` is already supported end to end.
- Adding QR output to `linux-wg-peer.sh` / the Rust `standin_router`. That test
  peer keeps its current text output.
- Uninstalling `qrencode` from `cellbridge-revert.sh`. It is an ordinary
  dependency, not part of the router configuration snapshot.
- iOS support. The scanner ships on Android only, matching the rest of the app.

## Anti-goals

- Making setup fail because the QR cannot be rendered. The text settings remain
  the authoritative fallback.
- Putting key material in logs, error messages, or analytics.
- Silently overwriting the private key or a running tunnel.
- Requiring the operator to install packages by hand.

## Constraints

- `router/cellbridge-setup.sh` is POSIX `sh` / BusyBox ash, runs as root on the
  router, and disables the WAN near the end. Anything that needs the network
  (including `opkg`) must happen before that point.
- The script's existing guarantees stay: the phone private key is never accepted
  or printed; the script is idempotent; an existing `wg0` private key is reused.
- The app is Flutter, Android-first, with injectable seams for widget tests
  (`KeyApi`, `testConnection`). The scanner must follow that pattern so tests do
  not need a camera or the plugin.
- The terminal QR must survive SSH from the router to the operator's computer.
  The assistant concluded that `qrencode -t ANSIUTF8` is the best default, with
  `-t UTF8` as a fallback.

## The flow

1. In the app, the operator taps **Generate key pair** and copies the displayed
   phone public key. (This already exists.)
2. The operator runs the router script with `--phone-public-key`.
3. The script configures `wg0`, generates or reuses a preshared key, and prints
   the settings plus a QR.
4. In the app, the operator taps **Scan router QR**, points the camera at the
   terminal, and the router-side fields fill in.
5. The operator reviews the form and taps **Save**.

The private key is already present from step 1 and is never touched by the scan.
On a fresh app with no private key, scanning still fills the router-side fields;
the app tells the operator that a private key is still needed, and that the
router must be given the matching public key if it does not already match the
QR's `phonePublicKey`.

## QR payload

A single-line, versioned text value. The prefix makes the format recognisable
and lets the app reject unrelated QR codes.

```
CELLBRIDGE1:{"address":"10.10.10.2/24","peerPublicKey":"<router public key>","presharedKey":"<psk>","endpoint":"192.168.8.1:51820","mtu":1420,"phonePublicKey":"<phone public key>"}
```

- `address`, `peerPublicKey`, `endpoint`, `mtu` are required.
- `presharedKey` may be absent or empty; the app treats that as "no PSK".
- `phonePublicKey` is optional for forward/backward tolerance but is always
  emitted by this version. The app uses it to warn when the current private key
  does not derive the key the router expects.
- Unknown fields are ignored. A different prefix or a different version is
  rejected with a generic message.
- The payload is built by the script with `printf`; all values are already
  validated or generated by the script itself and contain no JSON metacharacters
  (`address` is a constant, keys are base64, endpoint is a numeric
  `ip:port`, MTU is numeric).

Approximate size is 250 bytes, comfortably within a medium-EC QR code.

## Router script changes (`router/cellbridge-setup.sh`)

### Preshared key

- Before the existing "delete all peers" loop in `configure_network`, read the
  current first peer's `preshared_key` from UCI.
- If it is a valid 44-character base64 key, reuse it.
- Otherwise generate one with `wg genpsk` (WireGuard is already ensured) and use
  it.
- Set `network.<peer>.preshared_key` on the peer that is re-added.
- Keep the key out of command-line arguments; pass it through `uci set`, as the
  script does for the private key today.
- Add `PresharedKey` to the printed phone settings and to the QR payload.

Reusing the PSK is what makes a re-run safe for a phone that already imported
it.

### `qrencode`

- Add `ensure_qrencode` and call it next to `ensure_wireguard`, before the WAN is
  disabled.
- It returns immediately if `qrencode` is on `PATH`. Otherwise it runs
  `opkg update` and `opkg install qrencode`, tolerating failures, and returns
  non-zero if the binary is still missing.
- Do not disable the WAN or abort because of this. If installation fails, warn
  and continue to the text fallback.

### QR rendering

At the end of the summary, after the router public key and LAN IP are known:

- Build the payload from `PHONE_ADDR`, the router public key, the PSK,
  `$lan_ip:$WG_PORT`, `$WG_MTU`, and `$PHONE_PUB`.
- If `qrencode` is available, print a short instruction and render with
  `qrencode -t ANSIUTF8 -m 2`; if that mode fails, retry with `-t UTF8 -m 2`.
- If `qrencode` is missing or both renderings fail, print the complete text
  settings (including the PSK) under a clear "scan unavailable" heading, plus
  the raw `CELLBRIDGE1:` payload so it can be rendered elsewhere.
- The QR is printed in addition to, not instead of, the existing text settings.
- If the script runs with the WAN already disabled and `qrencode` is missing,
  the fallback is used without attempting an install (the `opkg` attempt simply
  fails harmlessly).

### Documentation

Update `router/README.md` and the script's header/usage text to mention the QR,
the generated preshared key, and the `qrencode` dependency.

## App changes

### New payload parser

Add `lib/src/setup_qr.dart`:

- A `SetupQrPayload` value type holding `address`, `peerPublicKey`,
  `presharedKey`, `endpoint`, `mtu`, and `phonePublicKey`.
- `SetupQrPayload parseSetupQr(String raw)`.
- It verifies the `CELLBRIDGE1:` prefix, parses JSON, checks types, and reuses
  the existing validators in `settings_store.dart` for address, peer public
  key, preshared key, endpoint, and MTU.
- All failures throw a single project exception (or `FormatException`) whose
  message never includes the scanned content. UI shows "That QR code is not a
  CellBridge setup code." for every failure.
- Pure Dart, no Flutter imports, so it is unit-testable.

### Scanner screen

Add `lib/src/qr_scan_screen.dart`:

- A full-screen `MobileScanner` view titled "Scan router QR" with a short
  instruction.
- Pops the first decoded barcode's raw value.
- Ignores further detections after the first one; a cancel/back button pops
  `null`.
- No torch/zoom controls in the first version.

Add `mobile_scanner` to `pubspec.yaml` (a current release compatible with the
project's Flutter SDK; `flutter pub add mobile_scanner` pins it).

### Config screen integration

`ConfigScreen` gains an optional injected scanner:

```dart
final Future<String?> Function(BuildContext context)? scanSetupQr;
```

- When non-null, show a **Scan router QR** button near the top of the form.
- On press: call the callback, parse the result, then set the address, peer
  public key, preshared key, endpoint, and MTU controllers. Leave the private
  key untouched. Cancel the pending public-key derivation debounce only if
  needed; the private key did not change, so no re-derivation is required.
- Invalidate any old connection-test result.
- Show "Router settings loaded. Review, then Save." on success and the generic
  failure message on any parse error.
- If `phonePublicKey` is present and the current private key's derived public key
  is known and differs, show a warning that the router expects a different phone
  key and the script must be re-run. Do not block filling or saving; the
  existing connection test is the authority.
- If the private key is empty after a successful scan, show a message that a
  private key is still required.
- `_save` and `_testConnection` keep their existing "disconnect first" guards.
  Scanning itself is allowed while the bridge runs; it only fills the form.

`main.dart` passes the production callback on Android:

```dart
scanSetupQr: (context) => Navigator.of(context).push<String>(
  MaterialPageRoute(builder: (_) => const QrScanScreen()),
),
```

On other platforms it passes `null`, so the button is absent.

### Android

- Add `<uses-permission android:name="android.permission.CAMERA" />` to
  `android/app/src/main/AndroidManifest.xml`.
- Add `<uses-feature android:name="android.hardware.camera" android:required="false" />`
  so the app remains installable on camera-less devices.
- No Kotlin changes expected; the plugin owns the runtime permission request.

## Testing

Dart unit tests (`test/setup_qr_test.dart`):

- Parses a full payload, including PSK and phone key.
- Accepts an absent/empty PSK.
- Rejects a wrong prefix, malformed JSON, missing required fields, wrong types,
  and invalid field values.
- Error messages never contain the scanned payload or key material.

Widget tests (`test/config_screen_test.dart`):

- Injected scanner returning a valid payload fills all router-side fields and
  leaves the private key unchanged.
- The connection-test result is cleared after a scan.
- A scanner returning garbage shows the generic message and leaves fields
  unchanged.
- The default (no injected scanner) still renders the form without a scan
  button.
- A saved scanned config round-trips through `SettingsStore`, including the PSK.

Manual/device checks:

- Router script with `qrencode` installed renders a scannable QR over SSH.
- Router script without `qrencode` prints the text fallback and the payload.
- Re-running setup reuses the PSK (the phone's existing config still passes the
  Wi-Fi connection test).
- Phone camera permission prompt appears, a successful scan fills the form, and
  `Test Wi-Fi connection` succeeds after Save.

Run `flutter analyze` and `flutter test` before claiming completion.

## Security notes

- The QR carries the preshared key and the router's public key, but not the
  phone private key. Someone who photographs the QR still cannot complete the
  handshake without the phone private key.
- The preshared key is nevertheless secret material. The script's text fallback
  prints it because the operator needs it; the README tells the operator to run
  setup in private and not share screenshots of the terminal.
- Scanned values are never logged. Parse errors are generic.
- The camera feed is handled by `mobile_scanner` on-device; nothing is uploaded.

## Calls left to the builder

- Exact `mobile_scanner` version and any Android `minSdk` adjustment it forces.
- Exact wording and layout of the scan button, scanner screen, and warnings.
- Whether to attempt `ANSIUTF8`, then `UTF8`, or pick one.
- Exact validator reuse and exception type for the parser.
- Whether the warning for a mismatched `phonePublicKey` compares against the
  already-derived field or derives again.
