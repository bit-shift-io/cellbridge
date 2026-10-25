# CellBridge — Design

**Date:** 2026-10-20
**Status:** Draft for approval
**Topic:** Rootless Android internet-sharing bridge over WireGuard

**Router failover update:** The later approved behaviour in
[`2026-10-25-router-failover-design.md`](2026-10-25-router-failover-design.md)
supersedes D9, the automatic-failover non-goal and the WAN-disable procedure below.
Prefer healthy CellBridge, then USB tether, then WAN.

---

## 1. Intent and why

The phone's cellular data is the **only** internet for the whole home network. Today it
is shared by turning the phone into a Wi-Fi hotspot, joining a second router to that
hotspot, and feeding the second router's output into the Flint 3's WAN. That works, but
it costs the phone its own Wi-Fi connection — so the phone loses access to everything on
the LAN (NAS, printers, local servers) while it is sharing.

CellBridge replaces the hotspot and the second router with one Android app. The phone
stays an ordinary **Wi-Fi client** of the Flint 3, keeping full LAN access, and becomes
the LAN's internet exit. LAN clients' traffic is carried to the phone over a WireGuard
pipe across the existing Wi-Fi link, and the phone re-originates it on its cellular
radio.

The original project docs (`ARCHITECTURE.md`) describe the **opposite** direction — the
phone borrowing the router's internet — and assume `VpnService`/`tun0` and root. This
document supersedes that: the direction is reversed, and the phone captures nothing.

---

## 2. Goals

- **G1** — The phone remains a Wi-Fi client of the Flint 3 with full, normal LAN access.
- **G2** — All LAN-client **TCP and UDP** flows (all ports) reach the internet through
  the phone's **cellular** interface, transparently — no per-device proxy configuration.
- **G3** — **No root** on the phone, and no bootloader unlock / data wipe.
- **G4** — Sustained throughput ≥ 40 Mb/s (the cellular link is the cap, not the app).
- **G5** — Survives screen-off and Doze via a foreground service; reconnects across
  network changes and app restarts.
- **G6** — Comes up automatically after a phone reboot with no user interaction
  (always-on), on a personal, sideloaded Pixel 8.

## 3. Non-goals

- IPv6 (IPv4-only).
- Raw protocols (GRE, ESP/IPsec), ICMP to the internet beyond best-effort, and any
  traffic requiring raw sockets.
- Root, USB tethering, Bluetooth tethering, Wi-Fi Direct, Wi-Fi hotspot.
- `VpnService` / `tun0` — the phone's own traffic is never captured.
- Multiple simultaneous internet exits (multi-WAN) and **automatic failover / backup
  uplinks**. A second phone is supported only as a manually swapped peer — see D10.
- Hiding tethering from the carrier.

## 4. Anti-goals

Things that would make this a failure even though it "works":

- **Breaking the phone's own connectivity** — its internet or its LAN access.
- **Silent partial breakage** — e.g. DNS dies and everything "feels slow" with no clear
  error.
- **Background death** — Android kills the service and the LAN loses internet.
- **Per-device configuration** — LAN clients must never need manual proxy settings.
- **An irreversible router change** — the Flint 3 must be returned to its previous state
  by a single command.

---

## 5. Constraints

- **Router:** stock GL.iNet firmware (OpenWrt-based). SSH access available. UCI/init
  scripts are acceptable; there is no requirement for a GL.iNet GUI plugin.
- **Phone:** Pixel 8, current Android (16), `arm64`, unrooted. Personal app, sideloaded —
  no Play distribution, so Play policy does not constrain permissions. Start-on-boot is
  required.
- **Link:** Wi-Fi LAN between phone and Flint 3. No requirement for the phone to be
  reachable from the WAN.
- **Bandwidth:** cellular ≤ 40 Mb/s.
- **Transport:** WireGuard — kernel module on the router, `boringtun` in the app.

---

## 6. Topology

```
   LAN clients                                   Phone (Wi-Fi client, no root)
   (192.168.8.0/24)                              ┌──────────────────────────────┐
        │                                        │ Flutter UI                   │
        │ internet-bound                         │   └─ Foreground Service       │
        ▼                                        │ Kotlin: net + socket binding │
   ┌──────────────────────┐   WireGuard (UDP)    │ Rust: boringtun + userspace  │
   │ Flint 3 (192.168.8.1)│◄────────────────────►│       IP gateway (smoltcp)   │
   │  • default route     │  over home Wi-Fi     │        │                     │
   │    via wg peer       │                      │        │ cellular-bound      │
   │  • masquerade wg→lan │                      │        ▼  sockets            │
   │  • dnsmasq upstream  │                      └────────┼─────────────────────┘
   └──────────────────────┘                               │
                                                          ▼
                                                  Cellular data → Internet
```

- The **phone is the exit**. It originates every internet connection itself.
- The **router is the gateway**. Its LAN default route points at the phone's tunnel IP.
- LAN → router → tunnel → phone → cellular → internet, and back.

---

## 7. Key decisions

**D1 — WireGuard is transport only, not privacy.** It is used purely as a packet pipe.
Chosen because the Flint 3 supports it natively and it is a solved, well-tuned tunnel.
*(User approved dropping the "VPN" framing; this is the transport of least work.)*

**D2 — The phone forwards in userspace; it does not NAT packets.** Rootless, an app
cannot create raw sockets or run kernel NAT, nor can it create a real `tun0` uplink.
The only viable model is to **terminate each flow and re-originate it** on a normal,
cellular-bound socket. The kernel then NATs for free. *(Decided with the user.)*

**D3 — TCP is spliced; UDP is mapped per flow.** *(Decided with the user.)* See §8.5.

**D4 — The router masquerades LAN traffic into the tunnel.** *(My call.)* Without this,
the phone would receive packets addressed to `192.168.8.x` — addresses it can also reach
*directly* over Wi-Fi — creating route ambiguity and a potential loop. With masquerade,
the phone only ever sees a single gateway address on the tunnel.

**D5 — Cellular egress is bound explicitly; so is the tunnel socket.** *(My call.)*
Android will mark the Wi-Fi network as "no internet" (because its uplink *is* the phone),
so the app must not rely on the default network. Outbound proxy sockets are pinned to a
`TRANSPORT_CELLULAR` network; the WireGuard UDP socket is pinned to the **Wi-Fi** network.
Binding everything explicitly removes any dependence on Android's default-network choice.

**D6 — No `VpnService`, no TUN.** *(My call.)* The app never observes the phone's own
traffic, so the VPN machinery is unnecessary. This removes the VPN permission prompt and
a large class of lifecycle problems.

**D7 — IPv4 only, initially.** *(My call.)* Carriers increasingly hand out IPv6, but the
LAN and the target traffic are overwhelmingly IPv4. Revisit only if needed.

**D8 — The phone's own traffic should egress cellular directly, not hairpin.** *(My call.)*
Because the bridge makes the phone's Wi-Fi *appear* to have internet, Android may mark it
the default network, and the phone's own traffic would then travel
Wi-Fi → router → tunnel → back into the phone → cellular: functional but wasteful. The
app pins its own sockets explicitly (D5), and the design treats "keep the phone's own
traffic off the hairpin" as a stated default the builder must preserve. LAN access is
unaffected: the `192.168.8.0/24` link route stays on Wi-Fi regardless of the default
network, so the NAS/printers remain reachable.

**D9 — Fail-closed: there is no backup uplink.** *(User's requirement.)* If the phone is
unreachable the LAN simply has no internet until the phone returns. The old uplink is
removed and deliberately not kept as a fallback. Recovery is the phone coming back, or
the manual revert script.

**D10 — A second phone is a swapped peer, not a live backup.** *(My call.)* WireGuard
lets only one peer own `0.0.0.0/0` on an interface, so two phones cannot both serve the
default route at the same time. A second Pixel can run the same app with its own tunnel
address; changing which phone is the exit is a config change, not automatic failover.

---

## 8. Phone application architecture

Three layers, as the repo already implies.

### 8.1 Flutter UI (`lib/`)

- **Dashboard:** connect/disconnect toggle, status (Idle / Connecting / Connected /
  Error), live bytes-in/bytes-out, current uplink, last error.
- **Config:** WireGuard private key, tunnel address, peer public key, endpoint
  (`192.168.8.1:51820`), and optional preshared key.
- **Diagnostics:** log view, active flow count, registered/unregistered state.
- Requests battery-optimization exemption and starts/stops the foreground service.

The UI is a **view over Rust state**; it owns no networking logic.

### 8.2 Android platform layer (`android/`, Kotlin)

Its whole job is the things an unrooted Rust process cannot do:

- **`ConnectivityManager`** — find a `TRANSPORT_CELLULAR` network and the current Wi-Fi
  network. Call `requestNetwork(TRANSPORT_CELLULAR)` so a cellular network is brought up
  and held even while Wi-Fi is connected.
- **Socket binding** — `connectivityManager.bindProcessToNetwork(cellularNetwork)` so
  sockets created by Rust default to cellular; then bind the WireGuard UDP socket
  explicitly to the Wi-Fi network (per-socket binding overrides the process default).
- **Foreground service + boot** — `startForeground()` with a live notification and a
  `BOOT_COMPLETED` receiver, so the bridge comes up unattended after a reboot (G6). A
  `WakeLock` only while actively forwarding. The foreground-service **type** must be one
  Android permits to start from `BOOT_COMPLETED` — recent Android disallows some types
  (e.g. `dataSync`) — so the builder must choose accordingly (`specialUse` /
  `connectedDevice` are the likely fits for a sideloaded app).
- **Battery-optimization exemption** — request it; the app is sideloaded, so Play policy
  does not apply.
- **Network callbacks** — on loss/change of Wi-Fi or cellular, notify Rust so it can
  rebind sockets and re-handshake.
- **Config storage** — keys and settings (EncryptedSharedPreferences).

### 8.3 Rust core (`rust/`, `flutter_rust_bridge`)

- **`boringtun`** — the WireGuard state machine. Encapsulates inner IP packets into UDP
  to the router and decapsulates the reverse. Provides the raw inner IP stream.
- **`smoltcp`** — a userspace IPv4 TCP/IP stack, driven by a custom `smoltcp::phy::Device`
  whose RX queue is filled from `boringtun` and whose TX queue is drained back into
  `boringtun`. Because the router masquerades only the **source** (D4), packets arrive
  addressed to real public IPs, so the stack runs in **any-IP / transparent mode**
  (`Interface::set_any_ip(true)`) and accepts connections to arbitrary destinations. The
  interface address (the phone's tunnel IP) is used only as the source for packets the
  stack itself originates.
- **Proxy engine** — TCP: for each connection surfaced by `smoltcp`, opens a real TCP
  socket and splices. UDP: rewrites datagrams at the IP-packet level, below `smoltcp`
  (detail in §8.5).
- **`tokio`** — one async runtime driving: the UDP↔`boringtun` pump, the `smoltcp`
  poll/timer loop, and the per-flow splices.

### 8.4 Socket binding detail (the constraint that shapes everything)

Rootless Android cannot use `SO_BINDTODEVICE` (needs `CAP_NET_RAW`). Therefore:

1. Kotlin calls `bindProcessToNetwork(cellularNetwork)` before starting Rust. All sockets
   Rust opens thereafter egress cellular with no per-socket work.
2. Kotlin creates the WireGuard UDP socket (or binds it) to the **Wi-Fi** network so the
   tunnel traverses the LAN, not cellular.

This is why the platform layer exists at all, and why the design does **not** need a
per-flow Kotlin round trip.

### 8.5 Flow handling

**TCP:**
1. `smoltcp` receives a SYN from the router gateway address; we `listen`/`accept`.
2. On the first payload (or immediately on accept), open a TCP socket to the original
   destination and `connect()`.
3. Splice bytes both directions between the `smoltcp` socket and the real socket.
4. Propagate half-close and RST correctly; tear down both ends together.

**UDP (handled at the packet level, below `smoltcp`):**
1. The gateway parses the IP/UDP headers itself. A reply's **source port must equal the
   destination port the client used** (443 for QUIC, 53 for DNS), which per-flow
   `smoltcp` sockets cannot express once two clients share a destination port — so UDP is
   rewritten directly instead of being terminated.
2. The first datagram of a new 5-tuple `(client_addr, client_port, dst_addr, dst_port)`
   opens a real, connected UDP socket to the destination and records the mapping.
3. Replies are re-encapsulated as a UDP/IPv4 packet with `src = dst_addr:dst_port`,
   `dst = client_addr:client_port`, both checksums recomputed, and pushed back into the
   tunnel.
4. Entries expire after an idle timeout; the table is bounded with oldest-first eviction.

**ICMP:** echo requests addressed to the phone's tunnel IP are answered locally by
`smoltcp`. Echo to arbitrary internet hosts is best-effort and may be dropped.

### 8.6 MTU and MSS

- WireGuard interface MTU on the router: `1420` (conservative over Wi-Fi).
- `smoltcp` advertises an MSS derived from its interface MTU so clients never send
  segments that would have to be fragmented inside the tunnel.
- Router-side MSS clamping on the wg zone as a second line of defence.

### 8.7 DNS

There is **no** special DNS path: DNS is UDP/53 and is covered by the generic UDP flow
handling. The router's `dnsmasq` is pointed at a public resolver; those queries are
proxied like any other UDP.

---

## 9. Router configuration (Flint 3)

Delivered as one idempotent UCI script plus a matching revert script.

- Create a WireGuard interface (`wg0`) with the router's tunnel address
  (e.g. `10.10.10.1/24`) and listen port `51820`.
- Add the phone as the **only** peer, with `allowed_ips '0.0.0.0/0'` and
  `route_allowed_ips '1'` so the interface installs the **default route** via the tunnel.
  The phone's LAN IP stays reachable via `br-lan` (its `/24` route is more specific).
- The **phone initiates** the handshake, so the router learns/refreshes the peer endpoint
  on its own; a fixed phone LAN IP is therefore *not* required. Android's per-SSID MAC
  randomization does not break this. A DHCP reservation is a nicety, not a dependency.
- A dedicated firewall zone for `wg0`: `input=ACCEPT`, `output=ACCEPT`,
  forwarding from `lan`, **masquerade ON** (D4), **MSS clamping ON**.
- Point `dnsmasq` upstream at a public resolver.
- **Disable the old WAN uplink** so the tunnel is genuinely the only path, and do **not**
  keep a fallback route — the LAN is fail-closed by design (D9).
- A second phone, if used, is provisioned as its own peer and swapped in manually (D10).
- A **revert** script restores the previous WAN/firewall/DNS state and removes `wg0`.
  This is the only automated recovery, and it is manual.

---

## 10. Failure modes and edge cases

| Case | Handling |
|---|---|
| Wi-Fi flagged "no internet" | Explicit socket binding (D5) — never rely on default network. |
| Cellular not up while on Wi-Fi | `requestNetwork(TRANSPORT_CELLULAR)` holds it open. |
| Wi-Fi or cellular network change | Network callbacks → rebind + re-handshake. |
| WireGuard handshake fails | Exponential backoff; surfaced in UI, not a silent hang. |
| MTU black hole | MSS clamping on both ends (§8.6). |
| UDP flow-table growth | Bounded table + idle expiry; oldest evicted. |
| Doze / screen off | Foreground service + notification; wake lock only while forwarding. |
| Phone off the LAN / asleep / rebooting | LAN loses internet by design (D9/D10). No automatic fallback; recovers when the phone returns. |
| Router reboot / DHCP change | Phone re-initiates and the router relearns the endpoint; auto-reconnect on the phone. |
| App killed / phone rebooted | Required start-on-boot (G6); state restored from config. |
| LAN clients using IPv6 | Out of scope; clients fall back to IPv4 or lose those flows. |

---

## 11. Testing plan

- **Unit (Rust):** flow table, MSS computation, packet codec, splice half-close.
- **Unit (Kotlin):** network selection, binding decisions.
- **Integration (Rust):** `boringtun` + `smoltcp` loopback — inject known IP packets,
  assert correct re-origination against a local test server; UDP mapping correctness.
- **WAN-side correctness:** verify a LAN client's **public IP** equals the phone's
  cellular IP; verify DNS resolves; verify a UDP flow (e.g. QUIC to a known host) works.
- **Throughput:** saturate the cellular link; confirm ≥ 40 Mb/s through the bridge.
- **Soak:** multiple hours for leaks, reconnect after Wi-Fi drops, Doze survival.
- **Boot:** reboot the phone and confirm the bridge reconnects unattended (G6).
- **Revert:** confirm the router script's revert fully restores prior connectivity.

---

## 12. Milestones

- **M0 — Scaffolding:** Flutter project + `flutter_rust_bridge` + Rust crate; builds for
  `arm64` on a real device.
- **M1 — Platform plumbing:** cellular request + process/interface binding; foreground
  service + boot receiver; config storage; a trivial echo proves cellular-bound sockets
  work.
- **M2 — Tunnel up:** `boringtun` handshakes with the Flint 3; inner-packet transport
  proven with a test payload.
- **M3 — TCP gateway:** `smoltcp` + TCP splice; a LAN client can browse via the tunnel.
- **M4 — UDP gateway:** per-flow UDP mapping; DNS and QUIC verified.
- **M5 — Router packaging:** UCI script + revert; default route, masquerade, MSS clamp,
  dnsmasq, old WAN disabled.
- **M6 — Hardening:** reconnect, Doze, throughput, soak.

---

## 13. Left to the builder

- Exact internal Rust module layout and the `smoltcp` device wiring.
- Choice of logging/tracing stack.
- Notification copy, UI styling, and the precise diagnostics screen.
- WireGuard key generation flow in the app.
- Which public resolver to use for `dnsmasq`.

## 14. Calls I made (check these)

- **D4** masquerade on the router, **D5** socket-binding strategy, **D6** no `VpnService`,
  **D7** IPv4-only, MTU `1420`, tunnel subnet `10.10.10.0/24`.
- **D10** — a second phone is a manually swapped peer, not automatic failover.
- Disabling IPv6 on the LAN (no IPv6 in use today).
- Treating WireGuard purely as transport.
- Not building a GL.iNet GUI plugin — a script is enough.
- Start-on-boot means always-on: the app connects with no user interaction after reboot.
