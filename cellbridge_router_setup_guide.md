# GL.iNet GL-BE9300 (Flint 3) Configuration for CellBridge

In this setup the Flint 3 does **not** use a normal WAN uplink. The phone — a Wi-Fi
**client** of the Flint 3 — is the internet exit, and the LAN's traffic is routed into a
WireGuard tunnel to the phone.

> The router side is delivered as scripts, not manual UI steps: see
> `router/cellbridge-setup.sh` and `router/cellbridge-revert.sh`. GL.iNet's
> WireGuard-**server** UI is built for the opposite direction (clients borrowing the
> router's internet) and cannot express "route the LAN out through a peer".

## 1. Prerequisites

* SSH access to the Flint 3 (`ssh root@192.168.8.1`).
* A WireGuard key pair for the phone — the app generates and holds the private key.
* The old WAN uplink identified, so it can be disabled.

## 2. What the setup script configures

| Setting | Value | Why |
| :--- | :--- | :--- |
| Interface | `wg0`, proto `wireguard` | the tunnel |
| Tunnel address | `10.10.10.1/24` | the router's end of the tunnel |
| Listen port | `51820` | the phone connects here |
| MTU | `1420` | avoids fragmentation over Wi-Fi |
| Peer | the phone, `allowed_ips 0.0.0.0/0`, `route_allowed_ips 1` | installs the **default route** via the tunnel |
| Firewall zone | `vpn` for `wg0`, **masquerade ON**, **MSS clamping ON** | LAN clients appear to the phone as a single address |
| dnsmasq | upstream set to a public resolver | DNS is carried through the tunnel like any other UDP |
| Old WAN | **disabled** | the tunnel is the only path (fail-closed) |

The **phone initiates** the handshake, so the router learns the peer's endpoint by itself.
No static lease or fixed phone IP is required.

## 3. Reverting

`router/cellbridge-revert.sh` restores the previous WAN, firewall and DNS state and removes
`wg0`. This is the **only** recovery path and it is manual — if the phone is away, the LAN
has no internet by design.

## 4. Values the phone app needs

| Setting | Example | Notes |
| :--- | :--- | :--- |
| `PrivateKey` | *generated in the app* | never leaves the phone |
| `Address` | `10.10.10.2/24` | the phone's tunnel IP |
| `PublicKey` | the router's `wg0` public key | printed by the setup script |
| `Endpoint` | `192.168.8.1:51820` | the router on the LAN |
| `MTU` | `1420` | |

## 5. Traffic flow

```
[LAN client] ──▶ [Flint 3: default route via wg0, masquerade]
                        │
                        ▼  WireGuard over Wi-Fi
              [Phone: boringtun → userspace gateway → cellular sockets]
                        │
                        ▼
                    [Internet]
```
