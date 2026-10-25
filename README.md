# CellBridge

**CellBridge** is a rootless Android app that lets a phone share its **cellular** internet
with a home LAN while staying an ordinary **Wi-Fi client** of the router — so the phone
keeps full LAN access (NAS, printers, local servers) that a hotspot would take away.

It replaces the usual "phone hotspot → second router → Flint 3 WAN" chain with a single
app. The router routes the LAN's internet traffic into a **WireGuard** pipe to the phone;
the phone re-originates it on its cellular radio.

> **Status:** Rust bridge, Android native service, UI and pre-router test tools
> implemented. Phone-side cellular forwarding is verified on a Pixel 8 with 5G;
> Flint 3 router-cutover verification is pending.
> See [`docs/superpowers/specs/2026-10-20-cellbridge-design.md`](docs/superpowers/specs/2026-10-20-cellbridge-design.md),
> [`docs/superpowers/plans/2026-10-20-cellbridge.md`](docs/superpowers/plans/2026-10-20-cellbridge.md)
> and [the Pixel 8 5G results](docs/testing/2026-10-24-pixel8-5g-tunnel-results.md).

## How it works

```
LAN clients ──▶ Flint 3 ──WireGuard over Wi-Fi──▶ Phone ──▶ Cellular ──▶ Internet
```

- The **router** prefers verified internet through the phone's WireGuard tunnel,
  with masquerading. Existing GL.iNet kmwan provides USB-tether then WAN fallback;
  the router automatically returns to CellBridge when tunnel internet recovers.
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
| **Fallback** | Prefer healthy CellBridge, then USB tether, then WAN; no internet if none works |

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

## Test before changing the router

The maintained `standin_router` Rust binary runs on Fedora using normal UDP
sockets. It exercises the real phone service with TCP, UDP and DNS checks without
changing Fedora routes or configuring Flint 3. The Android service owns the Rust
bridge independently of Flutter; the UI displays its state and diagnostics.

See [the pre-router verification runbook](docs/testing/pre-router-verification.md)
for builds, key exchange, USB-tether preparation and staged device checks.

### Start a Linux WireGuard test peer

From the project directory:

```sh
./linux-wg-peer.sh --peer 'PHONE_PUBLIC_KEY'
```

Use the phone's **Phone public key** from Configuration. The script builds the
peer, creates/reuses an owner-only Linux key, saves the phone public key, and
prints the settings for the app. It chooses the default route's IPv4 source
address; on multi-interface machines, select the shared LAN explicitly with
`--listen 192.168.1.126:51820`.

On later runs, just use `./linux-wg-peer.sh`. Keys live under
`${XDG_STATE_HOME:-$HOME/.local/state}/cellbridge/`, outside the repository.
Keep the terminal open and tap **Test Wi-Fi connection** at the bottom of the
phone's Configuration screen. No SIM, internet, sudo, or kernel WireGuard setup
is needed. Ctrl-C or `quit` stops the peer. Run `./linux-wg-peer.sh --help` for options.

## Hardware

* **Router:** GL.iNet GL-BE9300 (Flint 3), stock firmware, SSH access.
* **Phone:** Google Pixel 8, current Android, unrooted, sideloaded build.
