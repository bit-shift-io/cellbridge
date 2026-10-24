# Pixel 5a Wi-Fi handshake verification — 2026-10-24

## Setup

- Physical Pixel 5a, Android 14, arm64, connected by USB adb.
- Phone Wi-Fi `192.168.1.109`; Fedora Ethernet `192.168.1.126:51820`.
- Stand-in peer paired with the phone-generated public key; private keys stayed
  in the encrypted app store / owner-only host key file.
- No usable cellular network. These results establish Wi-Fi authentication,
  not cellular internet forwarding or router cutover behavior.

## Observed on device

1. Debug adb override started the real Kotlin/JNI/Rust bridge without cellular.
   Diagnostics correctly showed `egressTestOverride:true`,
   `cellularBindVerified:false`, and Wi-Fi network `101`.
2. Initial handshake failed despite Linux receiving type-1 initiation and
   returning type-2 response. A temporary UDP metadata relay isolated this.
   Android's dual-stack socket returned IPv4-mapped IPv6 source addresses; the
   strict endpoint filter compared these against a plain IPv4 endpoint.
   A real dual-stack-fd regression reproduced the timeout on Linux, then passed
   after canonicalizing source/endpoint IPs while still checking the port.
3. After rebuilding/installing, the direct peer (without relay) returned:
   `{"name":"handshake","passed":true,"message":"authenticated tunnel established"}`.
   The phone dashboard showed **Connected**.
4. Installed the shrunk release APK. With adb override still set to `1`, normal
   **Connect** stayed at **Waiting for cellular and Wi-Fi networks**: the release
   build ignored the debug-only bypass.
5. Removed the adb override, disconnected the service, and tapped the release
   Configuration screen's **Test Wi-Fi connection** button. It displayed
   **Authenticated WireGuard handshake succeeded over Wi-Fi.**
   `adb shell dumpsys activity services dev.cellbridge.cellbridge` showed no
   forwarding service running.
6. Stopped the peer and repeated the button test. It returned a visible failure
   after the bounded timeout, with no forwarding service started.
7. After review fixes, rebuilt/reinstalled release and repeated success with the
   adb setting absent (`null`), and no service running. Left the paired Linux
   peer running for further manual button tests.

## Build and automated checks

- Rust: 103 tests and strict Clippy passed, including dual-stack fd handshake,
  isolated one-shot handshake, timeout, and socket release.
- Android: 52 unit tests, lint and debug APK build passed.
- Flutter: 40 tests and static analysis passed. Widget tests cover current-form testing without saving, result display,
  secret-free failure messages and stale-result invalidation.
- `JAVA_HOME=/opt/jbr-21 flutter build apk --release --target-platform android-arm64`
  produced the tested release APK. Tink's missing compile-time annotation classes
  were supplied as compile-only dependencies so release R8 shrinking succeeds.
- Review fixes: shared lifecycle admission excludes concurrent test/bridge
  startup; changing a form value invalidates the result and discards stale
  completion. Both have regression tests.

Cellular binding/egress, screen-off traffic, reboot recovery and actual Flint
routing remain unverified by this SIM-free check. A later Pixel 8 + 5G run
verified cellular binding, DNS/TCP/HTTP forwarding, screen-off traffic, Wi-Fi
recovery and reboot recovery: see
[the Pixel 8 5G results](2026-10-24-pixel8-5g-tunnel-results.md). Flint routing
remains untested.
