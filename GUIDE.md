# CellBridge Developer Guide

## Prerequisites
* Flutter SDK (Latest Stable)
* Rust Toolchain (`cargo`, `rustup`, cross-compilation targets for Android: `aarch64-linux-android`, etc.)
* Android SDK / NDK installed.
* `flutter_rust_bridge_codegen` installed.

## Step-by-Step Implementation Plan

### Step 1: Android Platform Layer (`android/`)
1. Create a `VpnService` subclass in Kotlin (`CellBridgeVpnService.kt`).
2. Implement methods to:
   * Prepare and start the VPN interface (`tun0`, setting appropriate MTU, e.g., 1280, and adding routing rules for `0.0.0.0/0`).
   * Query `ConnectivityManager` to locate the active `TRANSPORT_CELLULAR` network.
   * Bind outgoing UDP sockets using `cellularNetwork.bindSocket(socket)` to force cellular routing.
3. Expose methods via Platform Channels or direct callback structures to pass the TUN file descriptor (`ParcelFileDescriptor`) and socket handle to Rust.

### Step 2: Rust Core (`rust/src/`)
1. Integrate `boringtun` (or a lightweight Rust WireGuard implementation) as a cargo dependency.
2. Define the bridge API via `flutter_rust_bridge`:
   * `fn start_tunnel(tun_fd: i32, config: WireGuardConfig, cellular_socket_fd: i32)`
   * `fn stop_tunnel()`
3. In Rust, read raw IP packets from the TUN file descriptor, pass them to the WireGuard state machine, and write encrypted output to the cellular-bound UDP socket.

### Step 3: Flutter UI (`lib/`)
1. Build a clean, minimalist dashboard featuring:
   * Connection toggle switch.
   * Live connection status indicator (Connected/Disconnected/Connecting).
   * Traffic stats (Bytes sent/received).
2. Invoke `VpnService.prepare()` from Dart before starting the native service.

## Router Configuration (GL-BE9300 / Flint 3)
1. Log into your Flint 3 admin panel (`192.168.8.1`).
2. Navigate to **VPN** -> **WireGuard Server** and start the server.
3. Create a client profile (`cellbridge-phone`).
4. Ensure **Allow Access Local Network** is enabled.
5. Note the `PrivateKey`, `Address`, `PublicKey`, and `Endpoint` (`192.168.8.1:51820`) for your Rust WireGuard client configuration.