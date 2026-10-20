# CellBridge

**CellBridge** is a rootless Android app that lets a phone share its **cellular** internet
with a home LAN while staying an ordinary **Wi-Fi client** of the router — so the phone
keeps full LAN access (NAS, printers, local servers) that a hotspot would take away.

It replaces the usual "phone hotspot → second router → Flint 3 WAN" chain with a single
app. The router routes the LAN's internet traffic into a **WireGuard** pipe to the phone;
the phone re-originates it on its cellular radio.

> **Status:** design and implementation plan complete; implementation not started.
> See [`docs/superpowers/specs/2026-10-20-cellbridge-design.md`](docs/superpowers/specs/2026-10-20-cellbridge-design.md)
> and [`docs/superpowers/plans/2026-10-20-cellbridge.md`](docs/superpowers/plans/2026-10-20-cellbridge.md).

## How it works

```
LAN clients ──▶ Flint 3 ──WireGuard over Wi-Fi──▶ Phone ──▶ Cellular ──▶ Internet
```

- The **router** treats the phone as its upstream: the LAN's `0.0.0.0/0` is routed into
  the WireGuard peer and masqueraded.
- The **phone** terminates the tunnel in Rust (`boringtun`) and runs a userspace IPv4
  gateway (`smoltcp`) that terminates TCP and rewrites UDP, re-originating every flow on
  ordinary sockets bound to the cellular network.

Because the phone originates the connections itself, **no root is required** and no
`VpnService`/`tun0` is involved.

## Capabilities and limits

| | |
|---|---|
| **Works** | All TCP and UDP ports — browsing, QUIC, DNS, games, VoIP, torrents |
| **Best-effort** | ICMP echo to the internet |
| **Out of scope** | Raw protocols (GRE/ESP), IPv6, root, USB/Bluetooth tethering |
| **Fail-closed** | If the phone is absent the LAN has no internet — by design |

## Project structure

* `lib/` — Flutter UI (dashboard, configuration).
* `android/` — Kotlin platform layer: foreground service, `ConnectivityManager`, socket binding.
* `rust/` — Rust core: `boringtun` tunnel, `smoltcp` gateway, TCP/UDP proxy engine.
* `router/` — OpenWrt (Flint 3) setup and revert scripts.
* `docs/` — the design spec and implementation plan.

## Run the dashboard on Linux (no phone required)

The dashboard runs against a simulated backend, so UI work needs neither an
Android device nor a router:

```sh
flutter run -d linux --dart-define=CELLBRIDGE_DEMO=true
```

The demo shows `Connecting` for about 1.5 seconds, then `Connected` with
counters and active flows that keep changing. Use the bug icon in the app bar
to simulate losing the uplink and see the error state. Without the
`CELLBRIDGE_DEMO` define the app initializes the real Rust bridge instead.

## Hardware

* **Router:** GL.iNet GL-BE9300 (Flint 3), stock firmware, SSH access.
* **Phone:** Google Pixel 8, current Android, unrooted, sideloaded build.
