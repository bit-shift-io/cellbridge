# CellBridge Architecture

> This is a summary. The authoritative document is
> [`docs/superpowers/specs/2026-10-20-cellbridge-design.md`](docs/superpowers/specs/2026-10-20-cellbridge-design.md).

## 1. The problem

The phone's cellular data is the only internet for the home network. The usual workaround
turns the phone into a **Wi-Fi hotspot**, which drops its Wi-Fi client connection — so the
phone loses access to everything on the LAN while it is sharing.

## 2. The approach

Keep the phone a **Wi-Fi client** of the Flint 3 (full LAN access retained) and make it the
LAN's **internet exit**, reached over a WireGuard pipe across the existing Wi-Fi link.

```
   LAN clients                                   Phone (Wi-Fi client, no root)
   (192.168.8.0/24)                              ┌──────────────────────────────┐
        │                                        │ Flutter UI                   │
        │ internet-bound                         │   └─ Foreground Service       │
        ▼                                        │ Kotlin: net + socket binding │
   ┌──────────────────────┐   WireGuard (UDP)    │ Rust: boringtun + userspace  │
   │ Flint 3 (192.168.8.1)│◄────────────────────►│       IP gateway (smoltcp)   │
   │  default route via   │  over home Wi-Fi     │        │                     │
   │  the phone's peer    │                      │        ▼ cellular-bound      │
   │  masquerade, MSS     │                      │          sockets             │
   └──────────────────────┘                      └────────┼─────────────────────┘
                                                          ▼
                                                  Cellular data → Internet
```

## 3. Components

1. **Flutter UI (`lib/`)** — dashboard and WireGuard configuration; a view over Rust state.
2. **Android platform layer (`android/`)** — foreground service with start-on-boot, plus
   the `ConnectivityManager` work an unrooted Rust process cannot do: request a cellular
   network, bind the process to it, and bind the WireGuard socket to Wi-Fi.
3. **Rust core (`rust/`)** — `boringtun` for the tunnel; `smoltcp` as a userspace IPv4
   stack that terminates TCP; a packet-level UDP forwarder; splicing to real sockets.
4. **Router (`router/`)** — OpenWrt config that routes the LAN's internet into the phone's
   peer and masquerades it.

## 4. Data flow

1. A LAN client sends internet-bound traffic; the router's default route points it into
   the `wg0` tunnel, masqueraded to the phone's tunnel address.
2. `boringtun` on the phone decrypts it into inner IP packets.
3. **TCP** is terminated by `smoltcp` and spliced to a real cellular-bound socket.
   **UDP** is rewritten at the packet level, preserving the client's destination port.
4. Replies travel back out the tunnel, the router de-NATs, and the client receives them.

## 5. Key properties

* **No root, no `VpnService`, no `tun0`** — the phone only ever uses ordinary app sockets.
* **The hard part is on the phone** — a userspace TCP/IP gateway, because Android will not
  NAT another device's traffic without root.
* **Fail-closed** — there is no backup uplink; the phone being absent means no internet.
* **Transport, not privacy** — WireGuard is used because the router speaks it natively.
