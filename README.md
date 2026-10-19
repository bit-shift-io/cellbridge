# CellBridge

**CellBridge** is an advanced, high-performance Android application built with **Flutter** and a **Rust core** (`flutter_rust_bridge`). It solves a classic Android networking constraint: allowing an Android device to stay connected to home Wi-Fi (retaining full local LAN access) while seamlessly tunneling all cellular data internet traffic directly into an OpenWrt router (**GL.iNet GL-BE9300 / Flint 3**) via an embedded WireGuard client.

## Core Architecture
1. **Flutter Frontend:** Provides the UI control panel, system VPN permission requests (`VpnService`), and manages the Android Foreground Service lifecycle.
2. **Android Platform Layer (Kotlin):** Establishes the virtual TUN interface (`tun0`), queries Android's `ConnectivityManager` to find the active **Cellular** network interface, and explicitly binds outgoing UDP sockets to mobile data to avoid routing loops.
3. **Rust Core (`flutter_rust_bridge` + `boringtun`):** Handles raw IP packet ingestion from `tun0`, WireGuard encryption/decapsulation, and streaming data over the local network to the Flint 3 router.
4. **Router Endpoint (OpenWrt / Flint 3):** Acts as the WireGuard server gateway (`router.lan`), ingesting cellular data and routing it upstream while bridging local LAN traffic.

## Project Structure
* `android/`: Native Kotlin implementation for `VpnService` and network socket binding.
* `lib/`: Flutter UI and state management.
* `rust/`: Rust core library wrapping `boringtun` and managing packet flow.
* `docs/`: Technical specifications and developer guidelines.