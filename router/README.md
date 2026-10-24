# CellBridge router scripts

Two POSIX `sh` scripts for the GL.iNet Flint 3 (GL-BE9300, OpenWrt with
**fw4/nftables**) that make the router send the LAN's internet traffic through
a WireGuard tunnel to the phone.

> **Warning: this changes the router's networking and removes its normal
> internet uplink.** The LAN becomes **fail-closed**: when the phone is not
> connected, the LAN has no internet. Open an SSH session **over the LAN**
> (`ssh root@192.168.8.1`) before you start, and keep it open until you have
> verified the tunnel. `cellbridge-revert.sh` is the only way back.

## Topology

```
[LAN client] --+--> [Flint 3: default route via wg0, masquerade]
               |            |
               |            v  WireGuard over Wi-Fi (phone initiates)
               |   [Phone: boringtun -> userspace gateway -> cellular sockets]
               |            |
               |            v
               |        [Internet]
               |
        SSH stays here (br-lan, 192.168.8.1)
```

* The router is the WireGuard **server**: `wg0`, `10.10.10.1/24`, UDP `51820`.
* The phone is the only **peer** (`10.10.10.2/24`) and the **initiator**. It is
  a normal Wi-Fi client of the router, so no static DHCP lease or fixed phone
  LAN address is required.
* The router installs `default dev wg0` and masquerades forwarded traffic, so
  the phone sees every LAN client as `10.10.10.1`.
* There is **no backup uplink**. The old WAN is disabled.

Why the tunnel does not route itself: `allowed_ips 0.0.0.0/0` installs a
default route via `wg0`, but the LAN's connected route
`192.168.8.0/24 dev br-lan` is more specific. The phone's own LAN address (the
WireGuard endpoint) therefore stays reachable through `br-lan`, and the
encapsulated UDP does not recurse into the tunnel. Do not "fix" this by
routing the phone's LAN address into `wg0`.

## Prerequisites

* SSH access to the router over the LAN (`ssh root@192.168.8.1`).
* The phone connected to the router's Wi-Fi as a normal client, with the
  CellBridge app installed.
* The **phone's WireGuard public key**.
* A baseline reading of the LAN's current public IP (see Verification).

### Where the phone's public key comes from

The setup script takes only the phone's **public** key. The phone's **private**
key must never be sent to the router.

The public key is the one belonging to the private key the app uses. At the
time of writing the app has a "Private key" field and a "Peer public key"
field, but **no key generator and no display of the phone's own public key**.
That is an app-side gap. Until it is added, do one of:

1. Generate the pair on a trusted machine and keep the private key on the
   phone:

   ```sh
   wg genkey > phone-private.key
   wg pubkey < phone-private.key > phone-public.key
   ```

   Paste the contents of `phone-private.key` into the app's **Private key**
   field (the Android store encrypts it), and use `phone-public.key` with this
   script. Delete the files from the trusted machine afterwards.

2. Or derive the public key from the private key already configured in the
   app, on the phone or on a trusted machine:

   ```sh
   printf '%s\n' '<the app private key>' | wg pubkey
   ```

The app should eventually show and copy its own public key; that is the
cleanest workflow and does not require the private key to be handled anywhere
else.

## Install the scripts

From the repository root:

```sh
scp router/cellbridge-setup.sh router/cellbridge-revert.sh root@192.168.8.1:/root/
```

## Run setup

**Keep the SSH session open.** The script does not touch the LAN interface or
the LAN firewall input policy, so SSH stays up throughout, but the WAN is
deliberately disabled at the end.

```sh
ssh root@192.168.8.1 \
  'sh /root/cellbridge-setup.sh --phone-public-key "<phone-public-key>"'
```

The public key is 44 base64 characters ending in `=`. The script validates it
and refuses anything else.

Useful options:

| Option | Effect |
| :--- | :--- |
| `--phone-public-key KEY` | phone's public key (required) |
| `--dns ADDR` | upstream resolver(s), repeatable; default `1.1.1.1` |
| `--wan-iface NAME` | extra UCI interface to disable; auto-detection normally finds `wan`/`wan6` |
| `--keep-wan` | configure the tunnel but **do not** disable the WAN |
| `--help` | usage |

`--keep-wan` is a configuration rehearsal, not a fallback:
`allowed_ips 0.0.0.0/0` still installs a default route via `wg0`, so the old
WAN is no longer guaranteed to be the preferred path. Connect the phone, confirm
`wg show wg0` reports a handshake and that LAN traffic really exits through the
phone, then re-run **without** `--keep-wan` to fail closed. A `--keep-wan` run
does not undo a WAN that an earlier run disabled.

The script is idempotent. It reuses the existing `wg0` private key and the
existing snapshot, reconciles the peer to exactly one entry, and can be re-run
on a partially configured router.

### What setup changes

| Area | Change |
| :--- | :--- |
| `/etc/config/network` | creates/reconciles `network.wg0`: `proto=wireguard`, `addresses=10.10.10.1/24`, `listen_port=51820`, `mtu=1420`, `nohostroute=1`; generates the router private key if absent |
| Peer | exactly one `wireguard_wg0` peer: `public_key=<phone>`, `preshared_key=<generated once and reused>`, `allowed_ips=0.0.0.0/0`, `route_allowed_ips=1` (installs the default route) |
| `/etc/config/firewall` | zone `vpn` over `wg0` with `input=ACCEPT`, `output=ACCEPT`, `forward=REJECT`, `masq=1`, `mtu_fix=1`; forwarding `lan -> vpn` |
| `/etc/config/dhcp` | dnsmasq `noresolv=1` and `server=<--dns>` (default `1.1.1.1`), so DNS goes through the tunnel instead of a dead WAN resolver |
| WAN | `disabled=1` on the detected WAN interface(s), normally `wan` and `wan6` |

Before any change, setup copies `/etc/config/network`, `/etc/config/firewall`
and `/etc/config/dhcp` into `/etc/cellbridge-backup/`, records the detected WAN
interfaces and the original dnsmasq upstream, and creates a
`snapshot-complete` marker. It never overwrites an existing snapshot.

Before the WAN is disabled, the script checks that WireGuard works by creating
and deleting a throwaway kernel interface. If `wg` or kernel support is
missing it tries `wireguard-tools` / `kmod-wireguard` via `opkg` while the WAN
is still up; if that fails it stops with the WAN left enabled.

At the end it prints:

* the router's `wg0` public key,
* the endpoint `192.168.8.1:51820` (the detected LAN address),
* the tunnel addresses (`10.10.10.1/24` and `10.10.10.2/24`),
* the peer preshared key (generated on the first run and reused on re-runs),
* the phone's WireGuard settings to enter in the app,
* a terminal QR code the app can scan to fill those settings.

The phone's private key is never accepted, printed or stored by the script.
The preshared key is generated on the router and is included in the phone
settings and the QR, so the app does not need to generate one. Treat
screenshots of this terminal as sensitive.

Upgrade note: older setup versions created the peer without a preshared key.
The first run after upgrading generates one and adds it to the peer, so any
phone configured by the old script must be updated with the new `PresharedKey`
(scan the new QR or enter it) or its handshake will fail. Later runs reuse the
same key.

The QR is rendered with `qrencode`, which setup installs automatically while
the WAN is still up. If the package is unavailable or rendering fails, setup
prints the text settings plus the raw `CELLBRIDGE1:` payload instead and keeps
going.

## Run revert

```sh
ssh root@192.168.8.1 'sh /root/cellbridge-revert.sh'
```

It prints what it restored and the current WAN state.

* If `/etc/cellbridge-backup/` contains all three config files, revert copies
  them back (and verifies them byte-for-byte), reloads network/firewall/dnsmasq,
  removes the state directory, and re-enables the WAN. The previous
  `wg0`/DNS/WAN state is restored exactly.
* If no usable snapshot exists, it removes what setup added (`wg0` and its
  peers, the `vpn` zone, the `lan -> vpn` forwarding, the public resolver
  override) and re-enables the detected WAN interface(s).
* If nothing from CellBridge is found, it says so and changes nothing.

Revert is idempotent: running it twice is safe, and it is safe on a router where
setup never ran. It keeps a broken/incomplete snapshot and says so rather than
pretending the restore succeeded.

## Verify

Run these from a **LAN client** unless stated otherwise.

### 1. Before setup

```sh
curl -s ifconfig.me
ping -c 3 1.1.1.1
```

Record the public IP as `BEFORE_IP`. This is the old WAN's address.

### 2. On the router, after setup

```sh
wg show wg0
```

Expected: the phone peer, `allowed ips: 0.0.0.0/0`, and
`listening port: 51820`. `latest handshake` only appears after the phone app
connects; the `endpoint` will be the phone's LAN address (it can change when the
phone reconnects, which is fine).

```sh
ip -4 addr show wg0        # inet 10.10.10.1/24
ip route                   # default dev wg0; 192.168.8.0/24 dev br-lan
```

The `br-lan` route must still be present. That is what keeps the phone's
endpoint reachable and the tunnel from recursing.

### 3. From the LAN client, with the phone connected

```sh
curl -s ifconfig.me
```

Expected: this now returns the **phone's cellular public IP**, not
`BEFORE_IP`. Confirm the phone's cellular IP independently (for example in a
terminal app on the phone with Wi-Fi temporarily off, or from the carrier's
account page). A `BEFORE_IP` result means the default route did not move into
the tunnel.

```sh
ping -c 3 1.1.1.1
nslookup example.com
```

DNS must resolve: the router's dnsmasq forwards to `1.1.1.1` over the tunnel.

If `curl` works but `nslookup` does not, check `logread | grep dnsmasq` and the
dnsmasq upstream from `uci show dhcp | grep -E 'server|noresolv'`.

### 4. Handshake and traffic

On the router:

```sh
wg show wg0 latest-handshakes
wg show wg0 transfer
```

A fresh `latest handshake` (seconds, not hours) and growing transfer counters
mean the phone is carrying the LAN's traffic. `No handshake` means the phone is
not connected, the keys do not match, or UDP 51820 is blocked between the
phone and the router.

## Recovery

**The LAN is fail-closed by design.** If the phone is away, asleep, rebooted
without the app, or off the Wi-Fi, there is no internet until it returns. This
is intentional (D9): there is no fallback route.

SSH over the LAN (`192.168.8.1`) keeps working in this state. To restore the
old WAN:

```sh
ssh root@192.168.8.1 'sh /root/cellbridge-revert.sh'
```

If you cannot SSH:

1. Connect a computer to a LAN port of the router.
2. Give it a static address in `192.168.8.0/24` (for example
   `192.168.8.2/24`); a gateway is not needed to reach `192.168.8.1`.
3. `ssh root@192.168.8.1`, then run the revert script.

Do not expect a reboot to recover the WAN: the configuration persists. Do not
delete `/etc/cellbridge-backup/` while CellBridge is configured; it is the only
exact record of the previous state.

## Assumptions and limits

* Written for **fw4/nftables** (`masq`, `mtu_fix`, zones, forwardings). If the
  firmware only has fw3, the UCI options are the same and the script falls back
  to `/etc/init.d/firewall reload`, but this combination was not tested.
* WAN detection looks for interface names matching `wan*`, `wwan*`,
  `usbwan*` and `*_wan`, and prints what it found. On unusual firmware, name
  the interface explicitly with `--wan-iface`.
* WireGuard availability is tested by creating a kernel interface, so both
  `kmod-wireguard` and a built-in kernel implementation work. Package install
  uses `opkg` (the package manager on GL.iNet's current firmware); if the
  firmware uses `apk`, install `wireguard-tools` manually **before** running
  setup.
* A stock GL.iNet firmware has no firewall zone named `vpn`. If one already
  exists, setup reuses it and turns on masquerade/MSS clamping; revert without
  a snapshot cannot restore those option values.
* The phone's handshake arrives on `br-lan`, so the **LAN** zone's input policy
  governs UDP 51820. Stock firmware accepts input on `lan`. Setup reads the
  policy and warns if it is not `ACCEPT`; it never changes the LAN zone,
  because that is the operator's SSH path.
* IPv6 is out of scope. `wan6` is disabled, and IPv6-only destinations will
  fail.
* QR output needs `qrencode`. Setup installs it via `opkg` before the WAN is
  disabled, but if the feed is unreachable the script prints the text settings
  and raw payload instead. A re-run after the WAN is already disabled cannot
  install it.
* The script does not configure the phone's app directly; it prints the values
  and a QR code that the app scans. The phone must be a Wi-Fi client of this
  router, not a hotspot.
