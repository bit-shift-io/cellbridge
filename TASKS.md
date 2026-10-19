# CellBridge Task Tracker & Roadmap

## Phase 1: Skeleton & Project Scaffolding
- [ ] Initialize Flutter project with Rust integration (`flutter_rust_bridge`).
- [ ] Configure `pubspec.yaml` and `Cargo.toml` with necessary dependencies (`boringtun`, `tokio`, etc.).
- [ ] Set up basic UI layout in Flutter with start/stop toggle state.

## Phase 2: Android Native Plumbing (`VpnService` & Socket Binding)
- [ ] Implement `CellBridgeVpnService.kt` in Android Java/Kotlin.
- [ ] Implement network query logic using `ConnectivityManager` to target `TRANSPORT_CELLULAR`.
- [ ] Implement explicit socket binding to force cellular transport interface.
- [ ] Expose TUN file descriptor passing mechanism to Rust.

## Phase 3: Rust Core & WireGuard Tunneling
- [ ] Set up `flutter_rust_bridge` bindings for lifecycle control.
- [ ] Integrate `boringtun` packet processing loop with the TUN file descriptor.
- [ ] Wire up UDP socket communication with the pinned cellular socket handle.
- [ ] Validate handshake with the GL-BE9300 OpenWrt WireGuard server.

## Phase 4: Testing & Hardening
- [ ] Verify local LAN accessibility (`router.lan` devices reachable while connected).
- [ ] Verify internet traffic routing exclusively through mobile data.
- [ ] Handle background lifecycle events and Android Doze mode gracefully with a Foreground Service notification.