# CellBridge router setup and recovery

For GL.iNet Flint 3 / OpenWrt with **fw4/nftables and enabled GL.iNet kmwan**.
The scripts configure a WireGuard tunnel to the phone and add internet-health
failover. **They add to kmwan; they do not replace or disable it.**

## Priority

1. CellBridge, once internet through the tunnel is verified.
2. USB tethering, when attached and working.
3. The existing WAN, including the old hotspot → second router → WAN arrangement.

Other existing kmwan members remain configured. USB and WAN get IPv4 metrics
1 and 2 respectively; kmwan is put in failover mode, not load-balancing mode.
Its existing probe timing is retained. No extra USB polling loop is installed;
kmwan monitors USB when it has an attached network device.

If nothing works there is no internet. Existing connections may reset when paths
change. This supersedes the original fail-closed/no-backup design.

## Before running setup

- Keep the router's current internet source running: setup may download tools.
  The phone can remain a hotspot until setup finishes; join it to the router's
  Wi-Fi afterwards, keeping mobile data enabled.
- Keep an SSH session open **over the LAN**, preferably from a wired computer.
  Network/firewall reloads may briefly interrupt traffic.
- Default setup generates the phone keys and includes them in its QR; no phone
  public-key copy/paste is required. Install the updated app before scanning it.
- Record a baseline from a LAN client: `curl -4 https://ifconfig.me`.
- Substitute your router's LAN address below; do not assume its default address.

## Copy all three scripts

From the repository root:

```sh
scp router/cellbridge-setup.sh router/cellbridge-revert.sh \
    router/cellbridge-failover.sh root@ROUTER_LAN_ADDRESS:/root/
ssh root@ROUTER_LAN_ADDRESS
```

The monitor must be beside setup. Setup checks this before making configuration
changes. Run on the router:

```sh
sh /root/cellbridge-setup.sh
```

Setup installs any missing WireGuard support, `curl`, `ca-bundle`, `ip-full` and
optional `qrencode` before changing routing. Missing mandatory dependencies abort
setup. Setup does **not** disable the WAN. `--keep-wan` is still accepted for
compatibility, but keeping backup uplinks is now the default behaviour.

Options:

| Option | Effect |
|---|---|
| `--phone-public-key KEY` | Optional: use an existing phone-managed key instead of generating/importing phone keys |
| `--dns ADDR` | Repeatable public DNS upstream; default `1.1.1.1` |
| `--wan-iface NAME` | Extra backup UCI interface for snapshot/recovery bookkeeping |
| `--keep-wan` | Compatibility option; backups always stay enabled |
| `--help` | Print usage |

After setup completes, turn off the old phone hotspot if necessary and join the
phone to this router's Wi-Fi as a **client**. Scan the terminal QR using
CellBridge Configuration, save the settings, then connect. If the phone already
has a different key pair, it asks **Overwrite phone keys? — No / Yes**. No or
dismissal cancels the whole import without changing any form fields or saved
settings. Yes fills the new keys and settings; **Save** is still required to
persist them. An empty phone or the same key pair does not need an overwrite
prompt. No internet is needed
to scan/save. The router keeps using backup routing until tunnel internet passes
two consecutive health rounds.

The QR includes the router public key, generated/reused phone private/public key
pair, preshared key, LAN endpoint, address and MTU. Treat the QR, terminal
screenshots and any raw QR payload as private key material. If QR rendering is
unavailable, the raw payload contains the generated private key for manual entry.
Re-running setup reuses phone/router keys and the first backup. A corrupt stored
phone pair stops setup rather than silently rotating it. Older peers without a
preshared key must scan the new settings.

For the original phone-managed-key flow, generate/copy **Phone public key** in
the app and run:

```sh
sh /root/cellbridge-setup.sh --phone-public-key 'PHONE_PUBLIC_KEY'
```

That mode does not generate/store a phone private key or include one in the QR.
Existing public-only QR codes remain supported by the app.

## How failover works

`/etc/init.d/cellbridge-failover` supervises `/usr/sbin/cellbridge-failover` with
procd and starts it after boot. kmwan continues handling physical-uplink health.

- Every round checks HTTPS through `wg0`, with a three-second request timeout.
- It tries `https://1.1.1.1/cdn-cgi/trace`, then `https://1.0.0.1/cdn-cgi/trace`
  if the first fails. One HTTP 200 with a valid TLS certificate is sufficient.
- Two consecutive good rounds prefer CellBridge; three failed rounds withdraw it.
- Rounds are separated by two seconds; worst-case detection is roughly 24 seconds
  plus scheduling delays. Recovery is usually quicker.
- Checks are numeric-IP, proxy-disabled, IPv4 and interface-bound, so WAN DNS,
  proxies or a captive-portal redirect cannot masquerade as tunnel health.
- Checks continue through the tunnel while the LAN uses a backup, enabling
  automatic return to CellBridge. A dedicated probe routing table prevents them
  from falling through to USB/WAN if the tunnel is unavailable.
- A stop/restart removes the monitor's preferred route. Restart begins on backup
  routing until the tunnel has been rechecked.

The tunnel retains `allowed_ips=0.0.0.0/0` but uses `route_allowed_ips=0`.
The monitor owns tables **51820** (healthy tunnel default) and **51821** (isolated
probes). An exact tunnel-source rule at priority 0 follows the kernel local rule
and precedes vendor local-output routing. Rules 100/101 preserve connected
main-table routes before consulting the preferred tunnel table. This keeps LAN
access and the phone's outer WireGuard endpoint on the LAN. Routes/rules carry
protocol tag 186. Setup and cleanup validate the reserved entries on every run,
including upgrades, and refuse foreign entries or failed routing inspection.
Cleanup filters by the ownership tag as well.

The fallback path itself remains subject to kmwan's existing health checks.
There is no second custom WAN/USB monitor: the tunnel is withdrawn when unhealthy
whether or not a backup works.

## Changes and backups

Setup creates an owner-only `/etc/cellbridge-backup/` **before configuration
changes**. It saves:

- `/etc/config/network`, `/etc/config/firewall`, `/etc/config/dhcp`;
- `/etc/config/kmwan`, including original mode, priorities and member settings;
- any previous CellBridge monitor/init files, boot-enabled state and running state;
- presence markers for files originally absent and recovery bookkeeping.

Copies and metadata are published atomically under a versioned snapshot format,
so an interrupted copy cannot become a reusable partial backup. A completed backup
is never repaired using subsequently modified configuration. Missing required
files/metadata or an incomplete snapshot cause setup/revert to stop and preserve
the snapshot for explicit repair. Do not delete an incomplete snapshot unless you
have established that its originals are safely recoverable.

| Area | Change |
|---|---|
| Network | `wg0`, `10.10.10.1/24`, UDP 51820, MTU 1420; one phone peer |
| Routing | Health-controlled policy tables instead of an unconditional tunnel default |
| Firewall | `vpn` zone for `wg0`, masquerade, MSS clamping, `lan → vpn` forwarding |
| DNS | Public resolvers via whichever IPv4 path is selected |
| kmwan | Keep enabled; failover mode; USB metric 1, WAN metric 2 |
| Service | Install/enable the CellBridge procd monitor |

In generated-key mode, `network.<phone-peer>.cellbridge_phone_private_key` stores
the phone private key as metadata ignored by netifd. `/etc/config/network` is
restricted to root-only access before key material is committed. Its existing
snapshot covers this option too: revert removes new keys or restores any original
peer metadata along with the rest of the original network configuration.

Upgrading an older fail-closed setup re-enables only backup interfaces that were
enabled in the original network snapshot. Originally user-disabled interfaces are
preserved. Dependencies installed by setup are left installed by revert.

## Verify after installation

On the router:

```sh
wg show wg0
/usr/sbin/cellbridge-failover status
ip -4 rule show
ip -4 route show table 51820
ip -4 route show table 51821
cat /proc/gl-kmwan/config
logread -e cellbridge-failover
```

Expected with working CellBridge: a recent WireGuard handshake, `path=tunnel`,
and a default via `wg0` in table 51820. Table 51821 contains the tunnel probe
default and an unreachable guard. Main-table physical-uplink routes stay present.

From a LAN client, verify browsing and DNS:

```sh
curl -4 https://ifconfig.me
nslookup example.com
```

The public IP alone is not proof of the selected path: the old hotspot and
CellBridge may share the same cellular exit. Use monitor status, routes and
WireGuard transfer counters too. ICMP forwarding through CellBridge is best-effort;
do not make ping success a requirement for a working tunnel.

Test fallback by stopping CellBridge in the app, making the old hotspot/WAN or USB
upstream available, and waiting for `path=backup`. Verify LAN internet. Then restore
CellBridge, check `path=tunnel` returns, and verify internet again. With both USB and
WAN working, verify native kmwan selects USB. Check phone LAN/NAS access as well.

## Revert

Over LAN SSH:

```sh
sh /root/cellbridge-revert.sh
```

Revert stops/disables the monitor and removes its exact rules/tables before
restoring configuration files byte-for-byte, restarting services and restoring
any previous managed monitor/service state. Enabled/stopped and disabled/running
are restored independently. It deletes the backup only after required service
reloads succeed. Failure keeps it for retry. A small restore receipt prevents a
second revert from deleting a restored older monitor; re-running revert is harmless.

With no snapshot, targeted cleanup removes CellBridge artifacts and re-enables
detected WANs; it cannot reconstruct lost original kmwan/DNS settings. An incomplete
or damaged snapshot is retained and is **not** treated as permission to guess.
Do not delete `/etc/cellbridge-backup/` while CellBridge is installed.

## Limits and local checks

- IPv4 only. Existing IPv6 uplinks are unchanged and may use WAN independently.
- A phone must use cellular as the upstream when USB tethering; sharing the
  router's own Wi-Fi connection back into it does not provide another internet path.
- VPN policy changes or other routing managers added later may interact with the
  reserved policy tables/rules and require rechecking.
- The router's multi-WAN UI should retain USB before WAN if priorities are edited.
- Local tests cover UCI/procd command boundaries and real Linux policy routing in
  disposable, rootless network namespaces. Actual hardware failover, boot and
  recovery must still be verified by the person running setup. Namespace tests
  skip on hosts that prohibit unprivileged user/network namespaces.

```sh
python -m unittest discover -s router/tests -v
sh -n router/cellbridge-setup.sh
sh -n router/cellbridge-revert.sh
sh -n router/cellbridge-failover.sh
```
