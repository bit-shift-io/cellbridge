# Verify CellBridge before router cutover

This exercises the production Rust bridge and Android service against a maintained
Fedora test peer. Flint 3 provides the wired/Wi-Fi LAN; its uplink may work or not.
No router script, default-route change, Podman container, or kernel WireGuard setup
is needed for this test.

## What goes where

- Fedora remains cabled to Flint 3.
- The phone joins Flint 3 Wi-Fi as a **client**; mobile data stays enabled.
- The app's WireGuard UDP socket is bound to Wi-Fi; its Rust egress sockets use
  a checked cellular process binding.
- Fedora can get its own internet from the same phone's USB tether. First verify
  that arrangement actually works on the phone; Android chooses tether upstream
  independently of CellBridge. A container cannot fix a missing upstream.
- Flutter displays and controls the native service. The service calls Rust
  directly through JNI and can run without the Activity or Flutter engine.

## 1. Build and prove the host path first

From the repository root:

```sh
cargo build --manifest-path rust/Cargo.toml --bins
cargo test --manifest-path rust/Cargo.toml
flutter test
flutter analyze
```

`rust/tests/test_peer.rs` runs the real bridge with local TCP/UDP fixtures,
including a 12 KB response, an encrypted-packet drop and duplicate delivery.
`rust/tests/cli.rs` starts both real command-line programs as separate processes,
handshakes and completes a TCP request through the production bridge.
These tests need neither internet nor a phone, and change no routes.

Android build on this Fedora installation (JDK 21 is already installed):

```sh
JAVA_HOME=/opt/jbr-21 ./android/gradlew -p android :app:testDebugUnitTest :app:assembleDebug -Ptarget-platform=android-arm64 --console=plain
```

Elsewhere use your installed JDK 21 path. No global Java configuration is changed.
APK: `build/app/outputs/flutter-apk/app-debug.apk`.

The companion host `bridge_launcher --config PATH` reads a JSON file using the
existing settings keys: `privateKey`, `address`, `peerPublicKey`, `endpoint`,
`mtu`, optional `presharedKey`. It calls the same native Rust lifecycle with no
Android fd. Enter `quit`, close stdin, or press Ctrl-C to stop. Protect any file
containing a private key with owner-only permissions. Automated tests create their
own temporary configurations; manual host testing does not require hand-edited
Rust source.

## 2. Prepare Fedora and keys while internet still works

Find Fedora's **Ethernet LAN address**, not its USB tether address:

```sh
ip -4 route get 192.168.8.1
```

Use the `src` address reached over Ethernet; substitute your actual router IP
if different. For example, Fedora might be `192.168.8.100`.

Generate the stand-in router key once:

```sh
mkdir -p "$HOME/.local/state/cellbridge"
./rust/target/debug/standin_router init --key-file "$HOME/.local/state/cellbridge/test-router.key"
```

The command prints its **public** key and creates an owner-only private-key file.
It refuses to overwrite an existing key. Never paste the private key into an
argument or logs. Re-running `serve` prints the same public key.

Install/open the APK when you are ready to test on the phone. Installation is a
separate, explicit step; these build instructions do not install it automatically.
The app's existing Configuration screen generates the phone key and displays a
copyable **Phone public key**. Keep that public key for the peer command.

In the app, save:

| Field | Value |
|---|---|
| Private key | Phone's generated key; leave it on the phone |
| Address | `10.10.10.2/24` |
| Peer public key | Public key printed by Fedora's test peer |
| Endpoint | Fedora **Ethernet** address, e.g. `192.168.8.100:51820` |
| MTU | `1420` |
| Preshared key | Empty |

For diagnostics-only settings changes, disconnect first. Saving while the native
bridge is running is refused.

## 3. Keep Fedora online during the switch

1. Connect the phone to Fedora by USB and enable USB tethering.
2. Confirm Fedora can still access the internet through USB; inspect `ip -4 route`
   and a normal browser request. Do not switch off the working hotspot until you
   have checked this path as far as the phone permits.
3. Join Flint 3 Wi-Fi from the phone. Verify Fedora's internet again, because the
   tether upstream can change when the phone joins Wi-Fi.
4. Fedora's route to the phone/Fedora LAN subnet must remain on Ethernet, while
   Fedora's normal internet can go over USB. The test peer does not alter either.

USB tether and the test tunnel are parallel paths, not a forwarding loop. Fedora
is not configured as Flint's uplink or as a LAN router by this procedure.

If USB internet does not survive the switch, stop and restore your known working
hotspot arrangement; keep the binaries and this runbook available offline.

## 4. Start the persistent test peer and app

Replace `PHONE_PUBLIC_KEY` and the listen IP with real values:

```sh
./rust/target/debug/standin_router serve \
  --key-file "$HOME/.local/state/cellbridge/test-router.key" \
  --peer 'PHONE_PUBLIC_KEY' \
  --listen 192.168.8.100:51820
```

Keep this terminal open. Press Connect in the app and allow the notification
permission if requested. The native service starts and waits for both networks.
It reports missing networks or failed binding, and retries while a connection is
requested. Open **Diagnostics** to inspect binding identities and errors.

If there is no handshake, check:

- The public keys are paired correctly and Fedora's endpoint is its LAN address.
- `ss -lunp` shows UDP 51820 listening on the selected address.
- Fedora's firewall permits inbound UDP 51820 from this LAN on the Ethernet
  interface. The tool does not change firewall rules; any temporary rule must be
  deliberately scoped and removed afterwards.
- Logcat: `adb logcat -s CellBridge`.
- A read-only packet capture on Ethernet, if needed, can establish whether phone
  datagrams reach Fedora before investigating tunnel keys.

## 5. Run checks through the tunnel

Enter these commands in the running test-peer terminal, **one at a time**:

```text
handshake
dns 1.1.1.1:53 example.com
http 1.1.1.1:80 one.one.one.one /cdn-cgi/trace
```

Each emits a JSON result with `passed`, elapsed time, received bytes and a reason.
An HTTP fetch also returns the response text explicitly, useful for egress-IP
evidence. Public services can change or be blocked; a failed remote service is
not automatically a bridge bug. Use an endpoint you control to isolate that case.

For a small known echo endpoint you control:

```text
tcp PUBLIC_IPV4:PORT pong ping
udp PUBLIC_IPV4:PORT pong ping
```

The fields after destination are expected reply and sent request respectively;
they are single text tokens. TCP matches the expected byte sequence in the full
response; UDP requires the exact reply. DNS validates its transaction, question,
response flags and a complete IPv4 answer. Requests time out after 15 seconds;
TCP response storage is capped at 1 MiB. Payloads are not logged by default.

The peer stays alive between checks, servicing handshake timers. `quit`, EOF or
Ctrl-C stops it. A scenario failure is a JSON `passed:false` result, not a process
exit, because this is an interactive persistent tool.

The `handshake` check reports that the peer has an authenticated session; it is
not a standalone liveness probe for a phone that has subsequently disconnected.
Use a new TCP/UDP/DNS request to verify current connectivity after reconnects.

## 6. Verify the two important properties explicitly

### Cellular egress

In Diagnostics, require:

- `binding.cellularBindVerified` is `true`.
- Cellular and Wi-Fi network identities are present and distinct.
- The tunnel fetch returns an actual public-internet response.

Tap **Check cellular public IPv4**. This is a separate request explicitly bound
to the cellular network; compare its address with `ip=` in the tunneled trace
response (or another public-IP endpoint you control). Matching addresses support
the result but are not proof by themselves; both paths could share an upstream.
Successful checked cellular binding plus working tunnel traffic is the primary
evidence. Flint 3 can have internet throughout this test.

### LAN access while remaining a Wi-Fi client

While repeating tunnel checks, open the phone browser to Flint's LAN address
and a NAS/server's numeric LAN address. Confirm they remain accessible and the
phone remains joined to the Flint Wi-Fi as a client.

CellBridge's **own process** is bound to cellular; an in-app LAN probe would need
an explicit Wi-Fi-bound socket. Browser/NAS access in other apps is not affected
by this process-local binding. Do not infer all-app LAN access just from a
successful tunnel handshake.

## 7. Production lifecycle checks before router cutover

- Close the Flutter Activity normally: tunnel checks should still pass, and the
  notification should remain. Reopening should attach to the same service.
- Turn the screen off and repeat checks after several minutes.
- Disconnect in the app or notification: tunnel traffic must stop. Reconnect and
  verify recovery without restarting the peer.
- Briefly leave/rejoin Wi-Fi: old flows stop and the service rebinds to current
  networks. New checks should pass after handshake recovery.
- When prepared for the temporary USB/internet interruption, reboot and unlock
  the phone. The boot receiver loads saved config and starts the native service
  without opening Flutter. Encrypted settings are read after unlock; pre-unlock
  Direct Boot is not implemented. Android force-stop is different from closing
  the Activity and can suppress boot/background startup until the app is opened.
- In battery optimisation settings, exempt CellBridge if needed for your device's
  background behavior. The app provides a settings shortcut.

Only then move to real-router testing. These checks do **not** establish Flint's
NAT, firewall, LAN default route, DNS forwarding, MSS clamping or revert behavior;
those require the separate router cutover/revert procedure. Real-kernel clients,
QUIC, throughput and longer soak remain additional verification.

## Implementation-session evidence

Verified on Fedora during implementation:

- Rust baseline: 86 tests passed before changes.
- New automated host tests cover headless lifecycle, fd cleanup, peer scenarios,
  packet loss/replay, DNS validation, CLI key-file protection and subprocess use.
- Android unit tests and arm64 APK build pass with JDK 21; JNI exports are present
  in the packaged library.
- Flutter unit/widget tests and static analysis pass.

Not yet verified on the physical phone: Wi-Fi/cellular concurrency, USB tether
upstream selection, headless native startup, screen-off behavior, boot recovery
and concurrent LAN access. Record those results separately when executed.
