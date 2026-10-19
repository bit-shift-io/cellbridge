# CellBridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a rootless Android app that keeps the phone a Wi-Fi client of the Flint 3 and turns it into the LAN's internet exit, carrying LAN TCP/UDP traffic over WireGuard to the phone and out its cellular radio.

**Architecture:** The router routes LAN `0.0.0.0/0` into a WireGuard peer (the phone) and masquerades it. On the phone, `boringtun` terminates the tunnel and hands inner IP packets to a `smoltcp` userspace stack that terminates TCP (spliced to real cellular-bound sockets) and to a packet-level UDP forwarder; both re-originate on ordinary sockets that egress cellular via `bindProcessToNetwork`.

**Tech Stack:** Flutter + `flutter_rust_bridge` 2.13, Kotlin (foreground service, `ConnectivityManager`), Rust (`boringtun`, `smoltcp`, `tokio`, `flutter_rust_bridge`), OpenWrt UCI.

**Spec:** `docs/superpowers/specs/2026-10-20-cellbridge-design.md`

## Global Constraints

- **No root, no `VpnService`, no `tun0`.** Everything is ordinary app sockets.
- **Pixel 8, current Android (16), `arm64`**, personal sideloaded build.
- **IPv4 only.** IPv6 is out of scope.
- **Coverage:** all TCP and all UDP ports. ICMP echo is best-effort; raw protocols are out.
- **Fail-closed:** no backup uplink; if the phone is gone the LAN has no internet.
- **Start-on-boot is required** (G6); the app connects with no user interaction after reboot.
- **Values:** interface MTU `1420`; tunnel subnet `10.10.10.0/24`; router `10.10.10.1`; phone `10.10.10.2/24`; router endpoint `192.168.8.1:51820`; throttling target ≥ 40 Mb/s.
- **WireGuard is transport only** — not used for privacy.
- Commits: one per task, `feat:`/`test:`/`chore:` prefixes.

## Review Focus

Inputs and conditions the spec implies but no task's happy-path test exercises. Each is pinned to a test in the task that owns the code:

1. **UDP source-port fidelity** — two LAN clients using the same destination port (e.g. two QUIC clients to `:443`) must both get replies *from that port*. Wrong here breaks all QUIC. *(Task 6, Task 7.)*
2. **UDP flow-table pressure** — a torrent-style client opening thousands of flows must hit bounded memory with oldest-first eviction, never drop an active flow or grow without limit. *(Task 6.)*
3. **Half-closed TCP** — a client that shuts down its write side but keeps reading must keep receiving. *(Task 5.)*
4. **Tunnel loss mid-flow** — Wi-Fi drops while connections are open; every spliced socket must be torn down and the client must see a reset, not a hang. *(Task 8.)*
5. **Unattended boot** — after a phone reboot the bridge must come back with no user interaction. *(Task 11, Task 15.)*

---

## File Structure

```
rust/
  Cargo.toml
  src/
    lib.rs            # module wiring + FRB init
    api.rs            # FRB surface: start_bridge/stop_bridge/status/stats
    config.rs         # BridgeConfig, address parsing
    tunnel.rs         # boringtun Tunn wrapper
    device.rs         # smoltcp phy::Device over in-memory queues
    gateway.rs        # smoltcp Interface (any-IP) + TCP listener + packet demux
    udp.rs            # packet-level UDP forwarder + flow table
    tcp.rs            # TCP splice engine
    stats.rs          # counters
  tests/
    loopback.rs       # full tunnel+gateway+proxy integration
android/app/src/main/kotlin/dev/cellbridge/
  MainActivity.kt
  NetworkBinder.kt    # cellular request + process/socket binding
  BridgeService.kt    # foreground service + notification
  BootReceiver.kt
  ConfigStore.kt
android/app/src/test/kotlin/dev/cellbridge/
  NetworkBinderTest.kt
lib/
  main.dart
  src/bridge.dart         # thin FRB wrapper
  src/dashboard.dart
  src/config_screen.dart
  src/settings_store.dart
test/
  dashboard_test.dart
router/
  cellbridge-setup.sh
  cellbridge-revert.sh
```

---

## Task 1: Scaffold Flutter + Rust + FRB round-trip

**Files:**
- Create: `rust/Cargo.toml`, `rust/src/lib.rs`, `rust/src/api.rs`, `flutter_rust_bridge.yaml`, `pubspec.yaml`, `lib/main.dart`, `lib/src/bridge.dart`

**Interfaces:**
- Produces: `pub fn ping() -> String` (Rust); `String ping()` (Dart, generated).

- [ ] **Step 1: Create the Flutter app and Rust crate**

Run `flutter create --platforms=android --org dev.cellbridge .` in the repo root, then `cargo new --lib rust`. Set `rust/Cargo.toml` to `crate-type = ["cdylib", "staticlib"]` and add `flutter_rust_bridge = "=2.13.0"`, `boringtun`, `smoltcp`, `tokio`, `bytes`.

- [ ] **Step 2: Add the FRB surface**

`rust/src/api.rs`:

```rust
pub fn ping() -> String { "pong".to_string() }
```

`rust/src/lib.rs`: `mod api; pub use api::*;` plus `#[frb(init)] fn init() { flutter_rust_bridge::setup_default_user_utils(); }`.

- [ ] **Step 3: Generate bindings and build**

Run: `flutter_rust_bridge_codegen generate && flutter build apk --debug`
Expected: build succeeds; `lib/src/rust/` contains generated `ping`.

- [ ] **Step 4: Verify round-trip on device**

In `lib/src/bridge.dart` add `String ping() => RustLib.instance.api.crateApiPing();`. In `main.dart` show the result. Run `flutter run -d <pixel>`, confirm the screen shows `pong`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: scaffold flutter + rust + frb round-trip"
```

---

## Task 2: `Tunnel` — boringtun wrapper

**Files:**
- Create: `rust/src/config.rs`, `rust/src/tunnel.rs`
- Test: `rust/src/tunnel.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `BridgeConfig { private_key: String, address: String, peer_public_key: String, preshared_key: Option<String>, endpoint: String, mtu: u16 }`
- Produces: `Tunnel::new(&BridgeConfig) -> Result<Tunnel, TunnelError>`, `Tunnel::is_connected(&self) -> bool`, `Tunnel::encapsulate(&mut self, ip: &[u8]) -> Result<Vec<Vec<u8>>, TunnelError>`, `Tunnel::handle_udp(&mut self, dg: &[u8]) -> Result<Vec<Vec<u8>>, TunnelError>`, `Tunnel::tick(&mut self) -> Result<(Vec<Vec<u8>>, Option<Duration>), TunnelError>`
- Consumes: nothing.

- [ ] **Step 1: Write the failing test**

Two `Tunnel`s (one as "phone", one as "router") with swapped keys. Drive the handshake by passing each `tick`/`handle_udp` output into the other until `is_connected()`. Then `encapsulate` a packet on one and assert `handle_udp` on the other returns the same plaintext.

```rust
#[test]
fn handshake_and_roundtrip() {
    // ... build peer + server tunnels from generated keys ...
    assert_eq!(server.handle_udp(&phone.encapsulate(&ip).unwrap()[0]).unwrap()[0], ip);
    assert!(phone.is_connected() && server.is_connected());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path rust/Cargo.toml handshake_and_roundtrip`
Expected: FAIL (not implemented).

- [ ] **Step 3: Implement `Tunnel` over `boringtun::noise::Tunn`**

`new` parses the base64 keys and constructs `Tunn::new(static_private, peer_public, psk, None, index, None)`. `encapsulate` calls `tunn.encapsulate(packet, out)`; `handle_udp` calls `tunn.decapsulate(None, dg, out)` mapping `WriteToTunnel::V4/V6` to packets and discarding `WriteToNetwork`/`None`. `tick` calls `tunn.update_timers(out)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path rust/Cargo.toml handshake_and_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add rust/src/tunnel.rs rust/src/config.rs && git commit -m "feat: boringtun tunnel wrapper"
```

---

## Task 3: `QueueDevice` — smoltcp device over queues

**Files:**
- Create: `rust/src/device.rs`
- Test: `rust/src/device.rs`

**Interfaces:**
- Produces: `QueueDevice::new(mtu: usize) -> Self`, `push_rx(&mut self, pkt: Vec<u8>)`, `drain_tx(&mut self) -> Vec<Vec<u8>>`, `rx_len(&self) -> usize`, `mtu(&self) -> usize`, and `impl smoltcp::phy::Device for QueueDevice`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn receives_pushed_and_collects_transmitted() {
    let mut d = QueueDevice::new(1420);
    d.push_rx(vec![0x45, 0x00, /* ... min valid IPv4 ... */]);
    assert_eq!(d.rx_len(), 1);
    assert_eq!(d.mtu(), 1420);
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --manifest-path rust/Cargo.toml receives_pushed`
- [ ] **Step 3: Implement** the two queues and the `Device`/`RxToken`/`TxToken` impls; `RxToken::consume` pops, `TxToken::consume` pushes.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: smoltcp queue device`

---

## Task 4: `Gateway` — smoltcp TCP listener in any-IP mode

**Files:**
- Create: `rust/src/gateway.rs`
- Test: `rust/src/gateway.rs`

**Interfaces:**
- Produces: `Gateway::new(address: Ipv4Addr, prefix_len: u8, mtu: usize) -> Gateway`, `Gateway::push_rx(&mut self, pkt: Vec<u8>)`, `Gateway::poll(&mut self, now: Instant) -> Vec<GatewayEvent>`, `Gateway::take_tx(&mut self) -> Vec<Vec<u8>>`, `Gateway::poll_delay(&self, now: Instant) -> Option<Duration>`
- Produces: `enum GatewayEvent { TcpAccepted { handle: SocketHandle, client: SocketAddr, dst: SocketAddr } }`
- Produces: `Gateway::tcp_recv(&mut self, handle: SocketHandle, buf: &mut [u8]) -> Result<usize, smoltcp::socket::tcp::RecvError>`, `Gateway::tcp_send(&mut self, handle: SocketHandle, data: &[u8]) -> Result<usize, smoltcp::socket::tcp::SendError>`, `Gateway::tcp_close(&mut self, handle: SocketHandle)`
- Consumes: `QueueDevice` (Task 3).

- [ ] **Step 1: Write the failing test**

Feed a hand-built TCP SYN to `1.1.1.1:80`. Assert `take_tx()` yields one packet that is a SYN-ACK, that its destination is the client and source is `1.1.1.1:80`, and that it advertises an MSS of `mtu - 40`.

```rust
#[test]
fn accepts_syn_to_arbitrary_destination() {
    // gateway 10.10.10.2/24, mtu 1420
    let events = g.poll(Instant::now());
    assert_eq!(events.len(), 1);
    let (client, dst) = expect_tcp_accepted(&events[0]);
    assert_eq!(dst.to_string(), "1.1.1.1:80");
    assert_eq!(mss_of(&g.take_tx()[0]), 1380);
}
```

- [ ] **Step 2: Run to verify it fails** — expected FAIL.
- [ ] **Step 3: Implement** `Gateway`: build an `Interface` with the given address, call `iface.set_any_ip(true)`, add a `tcp::Socket` in `Listen` state, and add an `icmp::Socket`. In `poll`, run `iface.poll` and drain the listen socket's `accept()` into `GatewayEvent`s.
- [ ] **Step 4: Run to verify it passes** — expected PASS (MSS assertion included).
- [ ] **Step 5: Commit** — `feat: smoltcp gateway with any-ip tcp listener`

---

## Task 5: TCP splice engine

**Files:**
- Create: `rust/src/tcp.rs`
- Test: `rust/src/tcp.rs`

**Interfaces:**
- Produces: `TcpSpliceEngine::new()`, `TcpSpliceEngine::on_accept(&mut self, handle: SocketHandle, client: SocketAddr, dst: SocketAddr)`, `TcpSpliceEngine::pump(&mut self, gw: &mut Gateway) -> Vec<IoOp>`, `TcpSpliceEngine::on_readable(&mut self, handle: SocketHandle) -> Vec<IoOp>`, `TcpSpliceEngine::closed_handles(&self) -> Vec<SocketHandle>`
- Consumes: `Gateway` (Task 4).

Design note for the implementer: `pump` drains `gateway → smoltcp recv → real socket write` and, for each handle whose real socket is readable, `real socket read → gateway.tcp_send`. Real sockets are non-blocking and owned per handle.

- [ ] **Step 1: Write the failing test**

Against a local `tokio` TCP echo server: inject a SYN, complete the handshake by feeding gateway TX back into gateway RX, send `"hello"`, and assert `"hello"` arrives back on the smoltcp side. Then close the client's write half and assert the echo of a final message still arrives (half-close).

```rust
#[tokio::test]
async fn splices_and_preserves_half_close() { /* ... */ }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --manifest-path rust/Cargo.toml splices_and_preserves_half_close`
- [ ] **Step 3: Implement** the splice: on accept, `TcpStream::connect(dst)`; loop copying between `Gateway::tcp_recv/tcp_send` and the stream; map EOF on one side to `tcp_close`/`shutdown`, and reset both on error.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: tcp splice engine`

---

## Task 6: UDP packet-level forwarder

**Files:**
- Create: `rust/src/udp.rs`
- Test: `rust/src/udp.rs`

**Interfaces:**
- Produces: `UdpForwarder::new(max_flows: usize, idle_timeout: Duration)`, `UdpForwarder::handle_outbound(&mut self, ip_packet: &[u8]) -> Option<OutboundUdp>`, `UdpForwarder::handle_reply(&mut self, flow: FlowId, payload: &[u8]) -> Vec<u8>` (the reply IP packet), `UdpForwarder::expire(&mut self, now: Instant)`, `UdpForwarder::len(&self) -> usize`
- Produces: `struct OutboundUdp { flow: FlowId, dst: SocketAddr, payload: Vec<u8> }`
- Produces: `pub fn build_udp_reply(src: SocketAddr, dst: SocketAddr, payload: &[u8]) -> Vec<u8>` and `pub fn parse_udp(pkt: &[u8]) -> Option<ParsedUdp>`.
- Consumes: nothing (pure packet manipulation).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn reply_source_port_equals_client_destination_port() {
    let reply = build_udp_reply("1.1.1.1:443".parse().unwrap(), "10.10.10.1:53000".parse().unwrap(), b"x");
    assert_eq!(parse_udp(&reply).unwrap().src.port(), 443);
}

#[test]
fn table_is_bounded_by_evicting_oldest() {
    let mut f = UdpForwarder::new(2, Duration::from_secs(30));
    // insert 3 distinct flows
    assert_eq!(f.len(), 2);
}
```

- [ ] **Step 2: Run to verify they fail** — expected FAIL.
- [ ] **Step 3: Implement** IP/UDP parsing, checksum computation (IPv4 header + UDP, including the pseudo-header), the 5-tuple flow table (`HashMap<FlowId, Flow>` + insertion-ordered eviction), and idle expiry.
- [ ] **Step 4: Run to verify they pass** — expected PASS.
- [ ] **Step 5: Commit** — `feat: packet-level udp forwarder`

---

## Task 7: Full loopback integration

**Files:**
- Create: `rust/tests/loopback.rs`
- Modify: `rust/src/gateway.rs` (demux UDP away from smoltcp)

**Interfaces:**
- Consumes: `Tunnel` (2), `Gateway` (4), `TcpSpliceEngine` (5), `UdpForwarder` (6).
- Produces: `Gateway::route(&mut self, pkt: Vec<u8>) -> Routed` with `enum Routed { Tcp, Udp, Dropped }`.

- [ ] **Step 1: Write the failing test**

Bring up two `Tunnel`s, a `Gateway`, the splice engine, and the UDP forwarder. Inject a TCP SYN into the tunnel and drive bytes to a local echo server; then inject a UDP datagram to a local UDP echo server. Assert both round-trip **through the tunnel** (encapsulate on one side, `handle_udp` on the other), and that the UDP reply's source port equals the destination port used.

```rust
#[tokio::test]
async fn tcp_and_udp_roundtrip_through_tunnel() { /* ... */ }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --manifest-path rust/Cargo.toml tcp_and_udp_roundtrip_through_tunnel`
- [ ] **Step 3: Implement** the demux in `Gateway`: inspect the IP protocol field before handing to smoltcp; UDP packets go to `UdpForwarder`, everything else to smoltcp. Wire `take_tx` output back into `Tunnel::encapsulate`.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: end-to-end rust loopback integration`

---

## Task 8: Stats, status, and teardown on tunnel loss

**Files:**
- Create: `rust/src/stats.rs`
- Modify: `rust/src/gateway.rs`, `rust/src/tcp.rs`, `rust/src/udp.rs`
- Test: `rust/src/stats.rs`, `rust/tests/loopback.rs`

**Interfaces:**
- Produces: `Stats { tx_bytes: u64, rx_bytes: u64, active_flows: usize }`, `enum BridgeState { Idle, Connecting, Connected, Error }`, `struct BridgeStatus { state: BridgeState, error: Option<String> }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn counters_accumulate() { /* tx/rx increment by payload size */ }

#[tokio::test]
async fn tunnel_loss_tears_down_all_flows() {
    // start a TCP + UDP flow, then stop driving the tunnel past the timeout,
    // assert TcpSpliceEngine::closed_handles() covers every handle and active_flows == 0
}
```

- [ ] **Step 2: Run to verify they fail** — expected FAIL.
- [ ] **Step 3: Implement** counters threaded through the splice and forwarder; on `Tunnel::is_connected() == false` for longer than the handshake grace period, tear down all handles and set `BridgeState::Error`.
- [ ] **Step 4: Run to verify they pass** — expected PASS.
- [ ] **Step 5: Commit** — `feat: stats, status and tunnel-loss teardown`

---

## Task 9: Kotlin `NetworkBinder`

**Files:**
- Create: `android/app/src/main/kotlin/dev/cellbridge/NetworkBinder.kt`
- Test: `android/app/src/test/kotlin/dev/cellbridge/NetworkBinderTest.kt`

**Interfaces:**
- Produces: `class NetworkBinder(cm: ConnectivityManager)` with `fun cellularNetwork(): Network?`, `fun wifiNetwork(): Network?`, `fun bindProcess(cellular: Network)`, `fun bindWifiSocket(network: Network): Int` (returns a raw fd), `fun onAvailable(network: Network)`, `fun onLost(network: Network)`.
- Produces: `interface NetworkEvents { fun onCellularChanged(up: Boolean); fun onWifiChanged(up: Boolean) }`

- [ ] **Step 1: Write the failing test (Robolectric)**

With a Robolectric `ShadowConnectivityManager` exposing one `TRANSPORT_CELLULAR` and one `TRANSPORT_WIFI` network, assert `cellularNetwork()` and `wifiNetwork()` select the right ones, and that with no cellular network `cellularNetwork()` returns null.

- [ ] **Step 2: Run to verify it fails** — `./gradlew :app:testDebugUnitTest --tests '*NetworkBinderTest'`
- [ ] **Step 3: Implement** using `activeNetwork`/`allNetworks` filtered by `NetworkCapabilities.hasTransport`, `requestNetwork(NetworkRequest)` for cellular, `bindProcessToNetwork`, and for the Wi-Fi socket: `DatagramSocket()` → `network.bindSocket(sock)` → `ParcelFileDescriptor.fromDatagramSocket(sock).fd`. **Do not** use `getFileDescriptor$()` reflection.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: kotlin network binder`

---

## Task 10: `BridgeService` + boot receiver

**Files:**
- Create: `android/app/src/main/kotlin/dev/cellbridge/BridgeService.kt`, `android/app/src/main/kotlin/dev/cellbridge/BootReceiver.kt`, `android/app/src/main/kotlin/dev/cellbridge/ConfigStore.kt`
- Modify: `android/app/src/main/AndroidManifest.xml`

**Interfaces:**
- Consumes: `NetworkBinder` (Task 9), generated `startBridge`/`stopBridge` (Task 11).
- Produces: `BridgeService.start(context)`, `BridgeService.stop(context)`.

- [ ] **Step 1: Write the failing test**

A Robolectric test asserting that starting `BridgeService` posts a notification, and that a `BOOT_COMPLETED` broadcast to `BootReceiver` starts the service when a config exists and does nothing when none does.

- [ ] **Step 2: Run to verify it fails** — expected FAIL.
- [ ] **Step 3: Implement** the foreground service with a `specialUse` FGS type and its manifest property (chosen over `dataSync`, which recent Android forbids starting from `BOOT_COMPLETED`), the notification, `RECEIVE_BOOT_COMPLETED` + `BOOT_COMPLETED` receiver, a `PARTIAL_WAKE_LOCK` held only while forwarding, and `ConfigStore` over `EncryptedSharedPreferences`.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: foreground bridge service and boot receiver`

---

## Task 11: FRB lifecycle wiring

**Files:**
- Modify: `rust/src/api.rs`, `android/.../BridgeService.kt`, `lib/src/bridge.dart`
- Test: `rust/src/api.rs`

**Interfaces:**
- Produces: `#[frb] pub async fn start_bridge(cfg: BridgeConfig, wg_socket_fd: i32) -> Result<(), String>`, `pub async fn stop_bridge() -> Result<(), String>`, `pub fn status() -> BridgeStatus`, `pub fn stats() -> Stats`
- Consumes: `Tunnel`, `Gateway`, `TcpSpliceEngine`, `UdpForwarder`, `Stats` (Tasks 2–8).

- [ ] **Step 1: Write the failing test**

A Rust test that `start_bridge` with a bogus endpoint reports `BridgeState::Connecting` (not a panic) and that `stop_bridge` returns the state to `Idle` and all counters to zero.

- [ ] **Step 2: Run to verify it fails** — expected FAIL.
- [ ] **Step 3: Implement** a `tokio` runtime task owning the tunnel pump (wrapping `wg_socket_fd` with `UdpSocket::from_raw_fd`), the gateway poll loop, the splice `pump`, UDP expiry, and a select on the gateway delay; `BridgeService` calls `NetworkBinder.bindProcess(cellular)` and `bindWifiSocket(wifi)` and passes the fd in.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: frb bridge lifecycle`

---

## Task 12: Dashboard

**Files:**
- Create: `lib/src/dashboard.dart`, `lib/src/bridge.dart`
- Modify: `lib/main.dart`
- Test: `test/dashboard_test.dart`

**Interfaces:**
- Consumes: `start_bridge`, `stop_bridge`, `status`, `stats` (Task 11).

- [ ] **Step 1: Write the failing widget test**

With a mocked `BridgeApi` returning `BridgeState.Connecting` then `Connected`, assert the dashboard shows **Connecting** then **Connected**, that the toggle is disabled while `Connecting`, and that stats render as human-readable byte counts.

- [ ] **Step 2: Run to verify it fails** — `flutter test test/dashboard_test.dart`
- [ ] **Step 3: Implement** the dashboard: connect/disconnect toggle, status text, bytes-in/out, active-flow count, last error.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: dashboard ui`

---

## Task 13: Config screen + settings store

**Files:**
- Create: `lib/src/config_screen.dart`, `lib/src/settings_store.dart`
- Modify: `lib/main.dart`
- Test: `test/config_screen_test.dart`

**Interfaces:**
- Produces: `SettingsStore.load() -> BridgeConfig?`, `SettingsStore.save(BridgeConfig)`.

- [ ] **Step 1: Write the failing test**

Assert `SettingsStore` round-trips a `BridgeConfig` (including the optional preshared key), that an empty store returns null, and that the config screen refuses to save with a missing private key or endpoint.

- [ ] **Step 2: Run to verify it fails** — expected FAIL.
- [ ] **Step 3: Implement** the form and persistence.
- [ ] **Step 4: Run to verify it passes** — expected PASS.
- [ ] **Step 5: Commit** — `feat: config screen and settings store`

---

## Task 14: Router setup + revert scripts

**Files:**
- Create: `router/cellbridge-setup.sh`, `router/cellbridge-revert.sh`, `router/README.md`

**Interfaces:**
- Produces: an idempotent setup script and a revert script; no code interfaces.

- [ ] **Step 1: Write the failing verification**

Before the script exists, from a LAN client: `ping 1.1.1.1` succeeds via the current WAN. This is the "before" state the tests compare against.

- [ ] **Step 2: Confirm the baseline** — record the LAN client's public IP with `curl -s ifconfig.me`.

- [ ] **Step 3: Implement `cellbridge-setup.sh`**

Create `network.wg0` (`proto=wireguard`, tunnel address `10.10.10.1/24`, listen port `51820`, MTU `1420`); add the phone peer with `allowed_ips '0.0.0.0/0'` and `route_allowed_ips '1'`; create a `vpn` firewall zone for `wg0` with `masquerade` and `mssfix` on and forwarding from `lan`; point `dnsmasq` upstream at a public resolver; disable the old WAN uplink. Make every step idempotent.

- [ ] **Step 4: Implement `cellbridge-revert.sh`** and verify restoration

Run: `ssh root@192.168.8.1 'sh /root/cellbridge-revert.sh'`, then from the LAN client confirm `curl -s ifconfig.me` returns the **original** public IP and the internet works.

- [ ] **Step 5: Commit** — `feat: router setup and revert scripts`

---

## Task 15: On-device end-to-end, throughput, boot, soak

**Files:**
- Create: `docs/superpowers/notes/2026-10-20-e2e-results.md`

**Interfaces:** none (verification task).

- [ ] **Step 1: End-to-end correctness**

With the phone on the Flint 3 Wi-Fi and the app connected, from a LAN client assert: `curl -s ifconfig.me` equals the **phone's cellular IP**; a website loads; `dig` resolves; a QUIC flow works (e.g. a Chromium page over HTTP/3).

- [ ] **Step 2: Throughput**

Run an `iperf3` download through the bridge; record the result. Expected: ≥ 40 Mb/s, or the cellular link's actual ceiling.

- [ ] **Step 3: Fail-closed behaviour**

Kill the app; confirm the LAN client loses internet (expected) and regains it when the app returns.

- [ ] **Step 4: Unattended boot**

Reboot the phone; without touching it, confirm the LAN client has internet again within a minute.

- [ ] **Step 5: Soak**

Leave it running several hours under mixed load; check for memory growth, leaked flows, and reconnect after a Wi-Fi drop.

- [ ] **Step 6: Commit results**

```bash
git add docs/superpowers/notes/2026-10-20-e2e-results.md && git commit -m "test: e2e verification results"
```

---

## Self-Review Notes

- Spec coverage: M0→Task 1; M1→Tasks 9–11; M2→Tasks 2, 7, 11; M3→Tasks 4, 5; M4→Tasks 6, 7; M5→Task 14; M6→Tasks 8, 15. G1–G6 covered. All five Review Focus items have owning tests.
- Deviation from the spec's milestone order: the router script is Task 14 rather than M5-as-last, so it can be validated with the stock WireGuard app before the phone app is finished. The spec's §8.5 UDP section was corrected during planning (packet-level rewriting, not `smoltcp` sockets) because per-flow `smoltcp` UDP sockets cannot preserve a client's destination port once two clients share it — see the spec's current text and Review Focus item 1.
