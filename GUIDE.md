# CellBridge Developer Guide

## Prerequisites

* Flutter SDK (stable), 3.47 or newer.
* Rust toolchain with the Android targets: `aarch64-linux-android` (and
  `x86_64-linux-android` for emulators).
* Android SDK + NDK; a Pixel 8 (or another arm64 device) for on-device testing.
* `flutter_rust_bridge_codegen` 2.13.
* SSH access to the Flint 3.

## Getting started

Implementation follows the plan:
[`docs/superpowers/plans/2026-10-20-cellbridge.md`](docs/superpowers/plans/2026-10-20-cellbridge.md).
Build it task by task — the plan carries the exact files, interfaces, test assertions and
commits.

The shape of the work:

### 1. Rust core (`rust/`) — Tasks 2–8

* `tunnel.rs` — the `boringtun` WireGuard state machine.
* `device.rs` / `gateway.rs` — a `smoltcp` IPv4 stack in **any-IP** mode that terminates TCP.
* `tcp.rs` — splice between `smoltcp` sockets and real, cellular-bound sockets.
* `udp.rs` — packet-level UDP forwarding that preserves the client's destination port.
* The core is **host-testable**: `cargo test --manifest-path rust/Cargo.toml`.

### 2. Android platform layer (`android/`) — Tasks 9–11

* `NetworkBinder.kt` — request a `TRANSPORT_CELLULAR` network, `bindProcessToNetwork`, and
  bind the WireGuard socket to **Wi-Fi** (using `ParcelFileDescriptor.fromDatagramSocket`).
* `BridgeService.kt` — foreground service (`specialUse` type) plus a `BOOT_COMPLETED` receiver.
* Pass the WireGuard socket fd into Rust. Never use `getFileDescriptor$()` reflection.

### 3. Flutter UI (`lib/`) — Tasks 12–13

* Dashboard: connect/disconnect, status, bytes in/out, active flows.
* Config: WireGuard keys, tunnel address, endpoint.

### 4. Router (`router/`) — Task 14

* `cellbridge-setup.sh` and `cellbridge-revert.sh` — see
  [`cellbridge_router_setup_guide.md`](cellbridge_router_setup_guide.md).

## Testing

* Rust: `cargo test --manifest-path rust/Cargo.toml` (unit tests + `tests/loopback.rs`).
* Kotlin: `./gradlew :app:testDebugUnitTest`.
* Flutter: `flutter test`.
* End-to-end, throughput, boot and soak: plan Task 15.
