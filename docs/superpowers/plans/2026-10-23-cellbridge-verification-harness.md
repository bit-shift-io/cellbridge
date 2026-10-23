# CellBridge Native Service and Verification Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the production headless Android bridge and a reusable Fedora WireGuard test peer, proving the data path before router cutover.

**Architecture:** Rust owns forwarding, runtime, lifecycle and diagnostics. Kotlin owns Android network requests, binding and foreground-service lifecycle, calling Rust directly through JNI. Flutter is a channel-backed view/controller on Android and retains FRB on desktop. A userspace Rust peer drives repeatable TCP/UDP checks through the same production bridge.

**Tech Stack:** Rust, tokio, boringtun, smoltcp, JNI, Kotlin Android services, Flutter platform channels, Robolectric.

**Spec:** `docs/superpowers/specs/2026-10-23-cellbridge-verification-harness-design.md`, extending `docs/superpowers/specs/2026-10-20-cellbridge-design.md`.

## Global Constraints

- Pixel 8, Android 16, arm64, no root; no VpnService or Android TUN.
- Tunnel addresses: peer `10.10.10.1/24`, phone `10.10.10.2/24`; MTU `1420`; UDP listen port `51820`.
- Fedora test tool uses ordinary UDP sockets: no sudo, interface creation or route changes.
- Flint 3 uplink state is irrelevant. Cellular selection and binding must be checked explicitly.
- Android service must start without a Flutter engine; there is one process-global Rust bridge.
- Private keys stay out of logs, error messages and command-line arguments; key files are owner-only.
- No commits, router changes or host network changes unless the user requests them. Plan checkpoints mean diff review, not automatic commits.
- USB tethering is a possible Fedora lifeline, subject to an actual device check; do not claim it has been verified.

## Review Focus

1. Startup failure after fd handoff closes the descriptor exactly once, including invalid configuration.
2. Cellular loss or a failed bind never sends proxy traffic through Wi-Fi; stale network-loss callbacks cannot stop a replacement session.
3. UI recreation, duplicate Connect, boot startup and explicit Disconnect operate on one service/session.
4. Forged UDP datagrams cannot replace the test peer's authenticated endpoint; scenario timeout is a clear failure.
5. Multi-segment/retransmitted TCP and unrelated DNS replies cannot produce a false successful result.

## File Structure

- `rust/src/diagnostics.rs`: bounded lifecycle event storage and snapshots.
- `rust/src/native.rs`: persistent native runtime and platform-neutral service entry points.
- `rust/src/android_jni.rs`: Android-only JNI conversion and delegation.
- `rust/src/test_peer/{mod.rs,client.rs,scenario.rs}`: reusable responder, smoltcp client, scenario checks.
- `rust/src/bin/{standin_router.rs,bridge_launcher.rs}`: maintained host CLIs.
- `rust/tests/{test_peer.rs,production_loop.rs,native.rs}`: local deterministic integration tests.
- `android/app/src/main/kotlin/dev/cellbridge/{NativeBridge.kt,BridgeController.kt,BridgeService.kt,BootReceiver.kt,BridgeChannel.kt}`: native facade, network orchestration, service lifecycle and thin UI channel.
- Existing `NetworkBinder.kt`, Android manifest/build settings and `MainActivity.kt`: binding correctness and registration.
- `lib/src/android_bridge.dart`: channel-backed `BridgeApi` with cached status/stats; existing UI interface remains synchronous for reads.
- `docs/testing/pre-router-verification.md`: runnable host and device instructions with honest evidence boundaries.

---

### Task 1: Native Rust lifecycle and descriptor ownership

**Files:** Modify `rust/src/{bridge.rs,config.rs,lib.rs}`, `rust/Cargo.toml`; create `rust/src/{native.rs,diagnostics.rs}`, `rust/tests/native.rs`.

**Interfaces:**
- `native::start(config: BridgeConfig, fd: Option<i32>) -> Result<(), String>`; owns any supplied descriptor immediately.
- `native::stop() -> Result<(), String>`; waits for teardown.
- Existing `bridge::status()` / `bridge::stats()` remain the single source of state.
- `diagnostics::snapshot() -> Vec<DiagnosticEvent>`; timestamp, component, event and sanitized message; bounded at 256 events.
- Config serde representation uses the existing settings map's camelCase field names.

- [ ] Write lifecycle tests: native start publishes Connecting, duplicate start fails, stop is idempotent, and repeated start/stop works. Use a local UDP peer rather than internet.
- [ ] Add fd tests with Unix datagram sockets: malformed config and duplicate start release the handed-over fd; successful stop releases it. Assert no double-close/reused-descriptor closure.
- [ ] Run `cargo test --test native` in `rust/`, confirm missing interfaces fail.
- [ ] Implement a process-lifetime tokio runtime for native calls. Enter/spawn the existing production lifecycle on it. Acquire socket ownership before fallible validation; do not duplicate forwarding logic or reset diagnostics on every UI read.
- [ ] Add bounded diagnostic events for lifecycle, handshake state transitions, socket/connect errors and teardown. Keep payloads and config values out of errors; do not log every flow packet in production.
- [ ] Run `cargo test --test native` and existing lifecycle tests; inspect the diff.

### Task 2: Maintained userspace peer and TCP/UDP scenario engine

**Files:** Create `rust/src/test_peer/{mod.rs,client.rs,scenario.rs}`, `rust/tests/test_peer.rs`; modify `rust/src/lib.rs`.

**Interfaces:**
- `PeerConfig { private_key: String, peer_public_key: String, listen: SocketAddr, mtu: u16 }`.
- `TestPeer::bind(config: PeerConfig) -> Result<TestPeer, PeerError>` (async).
- `TestPeer::run(self, commands: mpsc::Receiver<Scenario>, results: mpsc::Sender<ScenarioResult>)` (async).
- `Scenario::{Handshake, Tcp { destination, request, expected }, Udp { destination, payload, expected }, Dns { resolver, name }}`; each has a deadline and byte cap.
- `ScenarioResult` includes name, pass/fail, elapsed time, byte counts and sanitized failure reason.

- [ ] Write tests against local fixtures: authenticated handshake, UDP exact-byte roundtrip, DNS rejects wrong transaction ID/response flags, and TCP receives a response larger than one MTU and gracefully closes.
- [ ] Add tests that inject forged datagrams from another UDP address and reorder/retransmit inner TCP packets. Assert authenticated endpoint is unchanged by forgery, stream bytes are correct, timeout fails, and buffers remain bounded.
- [ ] Run `cargo test --test test_peer` and confirm initial failures.
- [ ] Implement responder pumping with `Tunnel` and a persistent UDP socket. Initial selection and endpoint updates require authenticated protocol progress (not merely an Ok result from a no-op datagram). Allow authenticated reconnect after phone socket/network replacement; never implement unauthenticated roaming.
- [ ] Implement TCP initiation using smoltcp Interface/QueueDevice and a default userspace route toward the tunnel. Use a per-scenario deadline (default 15 seconds), 1 MiB response cap and bounded socket buffers. Use numeric IPv4 destinations so host DNS cannot silently bypass the tunnel; DNS checks are explicit tunneled scenarios.
- [ ] Implement UDP checks and DNS construction/validation. Redact payloads by default; packet summaries may include addresses, flags and lengths.
- [ ] Run `cargo test --test test_peer`; inspect the diff.

### Task 3: Host CLIs and production-loop verification

**Files:** Create `rust/src/bin/{standin_router.rs,bridge_launcher.rs}`, `rust/tests/production_loop.rs`; modify `rust/Cargo.toml` and lockfile if necessary.

**Interfaces:**
- `standin_router init --key-file PATH`: atomically creates a private key file, displays only public key, never overwrites an existing key.
- `standin_router serve --key-file PATH --peer PUBLIC_KEY --listen IP:PORT`: persistent peer with commands on stdin (`handshake`, `tcp`, `udp`, `dns`, `quit`); human summaries plus one JSON result per command.
- `bridge_launcher --config PATH`: reads the same camelCase settings JSON, starts `native::start(config, None)`, stops on quit/termination.
- CLI parsers share the typed scenario engine from Task 2; neither creates routes or network interfaces.

- [ ] Write a production-loop integration test which starts the real native bridge against TestPeer and local TCP/UDP servers, sends nontrivial payloads, verifies replies, stops and restarts. Test a destination refusal and timeout as failures.
- [ ] Add CLI tests for help, invalid command/config, refusing key overwrite, owner-only permissions, sanitized errors and machine-readable failing results.
- [ ] Run `cargo test --test production_loop` and CLI parser tests; confirm initial failures.
- [ ] Implement both CLIs, secure key-file loading and shutdown. Ensure Ctrl-C/EOF does not leave a background process. Add only dependencies needed for serialization/JNI/CLI signal handling, preserving existing versions.
- [ ] Run `cargo test` and `cargo build --bins` in `rust/`. Execute a host-only TCP/UDP smoke run against deterministic local fixtures and save the command/result evidence.

### Task 4: Direct Android-to-Rust boundary

**Files:** Create `rust/src/android_jni.rs`, `android/app/src/main/kotlin/dev/cellbridge/NativeBridge.kt`; modify `rust/src/lib.rs`, `rust/Cargo.toml`, Android native-library packaging if required.

**Interfaces:** Kotlin `BridgeFacade` has `start(configJson: String, fd: Int)`, `stop()`, `snapshotJson(): String`. `NativeBridge` implements it using JNI exports under `dev.cellbridge.NativeBridge`; load the existing cellbridge shared library.

- [ ] Add boundary tests for config roundtrip, malformed field types, invalid fd, errors without key material and shared lifecycle snapshots. Reuse native lifecycle tests rather than a second mock-only implementation.
- [ ] Run Rust boundary tests before implementation and confirm failures.
- [ ] Implement Android-only JNI exports: convert strings/config, acquire fd ownership before parsing, delegate to Task 1 runtime, map failures to bounded exceptions/results, prevent Rust panics crossing JNI. Avoid passing Android objects into Rust's forwarding loop.
- [ ] Verify symbol names match Kotlin declarations and build the Android arm64 library/APK through the project's existing Rust builder. Verify the library is available when started from Kotlin before FRB initialization.
- [ ] Run host tests; record device JNI loading as pending until a phone is attached.

### Task 5: Production service, boot lifecycle and network recovery

**Files:** Create `android/app/src/main/kotlin/dev/cellbridge/{BridgeController.kt,BridgeService.kt,BootReceiver.kt}` and corresponding tests; modify `NetworkBinder.kt`, `AndroidManifest.xml`, Android build settings as needed.

**Interfaces:**
- `BridgeController.start(config: Map<String, Any?>)`, `stop()`, `snapshot()`; inject network operations, BridgeFacade and executor for deterministic tests.
- `BridgeService.start(context)` / `stop(context)`; owns one controller and reads existing `ConfigStore`.
- Network abstraction supports request/observe, transport-qualified snapshots, checked cellular process binding, Wi-Fi fd handoff and unbinding.

- [ ] Write Robolectric tests: notification on startup, boot starts service with saved config but not without one, duplicate Connect/recreated UI keeps one bridge, explicit stop does not reconnect, and all requests/wakelocks/fds are released after failure.
- [ ] Test missing cellular, bind returning false, replacement Wi-Fi/cellular identities and stale onLost callbacks. Assert Rust stops before unbinding and restarts only after valid replacement binding, with no default-network fallback.
- [ ] Run `./gradlew :app:testDebugUnitTest` in `android/`; confirm new tests fail before implementing.
- [ ] Implement serialized controller lifecycle away from the main thread. Wait asynchronously for network callbacks rather than treating initial unavailability as permanent failure. Stop old flows on transport loss; retry only while a run is requested.
- [ ] Implement foreground service notification and stop action, boot receiver, wake lock scoped to forwarding, and required INTERNET/ACCESS_NETWORK_STATE/CHANGE_NETWORK_STATE/FOREGROUND_SERVICE/FOREGROUND_SERVICE_SPECIAL_USE/WAKE_LOCK/RECEIVE_BOOT_COMPLETED permissions. Check current target API foreground-service rules; declare `specialUse` and its subtype property. Handle notification permission/battery exemption through the UI without blocking headless networking unnecessarily.
- [ ] Preserve encrypted-store behavior: ordinary BOOT_COMPLETED after unlock can read it; do not claim pre-unlock Direct Boot support. Handle unavailable/corrupt configuration as a visible startup error, never plaintext fallback.
- [ ] Run Android unit tests, build the APK and inspect manifest declarations.

### Task 6: Thin Flutter service control and diagnostics

**Files:** Create `android/app/src/main/kotlin/dev/cellbridge/BridgeChannel.kt`, `lib/src/android_bridge.dart` and channel tests; modify `MainActivity.kt`, `lib/src/bridge.dart`, `lib/main.dart`, minimal dashboard diagnostics rendering if needed.

**Interfaces:** Channel `dev.cellbridge/bridge`: `start`, `stop`, `snapshot`; snapshot contains status, decimal byte-counter strings, active flows, bounded diagnostics and transport-binding metadata. Android `BridgeApi` keeps synchronous status/stats reads from a cached snapshot, refreshed while attached; start/stop await acknowledgement and refresh.

- [ ] Write channel/widget tests proving Android Connect requests service startup, resume attaches to existing state, large counters survive serialization, errors are displayed and desktop/demo behavior remains unchanged.
- [ ] Run focused Flutter and Kotlin tests to confirm failures.
- [ ] Implement service control channel and Android adapter selection. Android start never calls FRB start; desktop retains it. Key generation may retain FRB because it does not own bridge lifecycle. Native callback/network logic never moves into Flutter.
- [ ] Expose a bounded text diagnostic view using existing UI patterns, plus logcat tag CellBridge. Show cellular/Wi-Fi binding outcomes and actionable failures.
- [ ] Run `flutter analyze`, `flutter test`, Android unit tests and `flutter build apk --debug`.

### Task 7: Pre-router runbook and evidence

**Files:** Create `docs/testing/pre-router-verification.md`; update `README.md`, `TASKS.md` to match verified implementation status.

- [ ] Document host build, key exchange, persistent tool commands and deterministic host production-loop check; then Fedora LAN endpoint configuration in the phone's existing config screen.
- [ ] Document checking Fedora's routes and USB tether connectivity before switching the phone from hotspot to Wi-Fi client. USB upstream choice is controlled by Android and must be observed. Fedora remains wired to Flint; tunnel packets must traverse that LAN path. Firewall issues are diagnosed and any host firewall change is explicit, scoped and user-approved.
- [ ] Document device checks through the real service: handshake; TCP/UDP/DNS replies; cellular network identity and checked binding; egress-IP response alongside cellular-bound control; concurrent phone browser access to Flint/NAS and service-bound Wi-Fi probe if needed. A process bound to cellular does not automatically retain LAN routing for its own unbound sockets, so in-app LAN probes must bind to Wi-Fi explicitly.
- [ ] Include stop/start, screen-off, UI closure and reboot checks against the Fedora peer before cutover where practical. Separate those from later real-router NAT/firewall/default-route/revert verification and throughput/soak.
- [ ] Run all relevant host checks once after the final code change; record exact commands, results and any missing toolchain/device blockers. Do not present a device result without physical execution evidence.

## Self-review

- V1: Tasks 1–3; V2: Tasks 4–6; V3: Task 5 binding evidence plus Task 7 device checks; V4: Tasks 3, 6–7.
- Review-focus ownership: fd Task 1/4; transport loss Task 5; duplicate lifecycle Tasks 5–6; peer authentication Task 2; TCP/DNS fidelity Task 2.
- Existing Task 11's Rust loop is already implemented: extend it for native ownership, do not rebuild it.
- Public internet tests are manual; default automated tests use local fixtures and work offline.
- Full Android/device verification depends on actual installed SDK/toolchain and attached phone. Completion reports must distinguish build/unit evidence from real-device evidence.
