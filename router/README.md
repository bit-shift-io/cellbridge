# CellBridge router setup and recovery

For GL.iNet Flint 3 / OpenWrt with **fw4/nftables and enabled GL.iNet kmwan**.
The scripts configure a WireGuard tunnel to the phone and add internet-health
failover. **They add to kmwan; they do not replace or disable it.**

## Priority

1. The highest-preference CellBridge phone with verified internet (main first).
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

Setup defaults to UDP **51820**. Before taking a snapshot or editing config it
checks whether another WireGuard interface or UDP service already owns that port
(for example GL.iNet's `wgserver`, which also defaults to 51820). A conflict is a
hard error naming the owner; choose another port with `--port`:

```sh
sh /root/cellbridge-setup.sh --port 51830
```

Re-runs reuse the port already configured on `wg0` unless `--port` is supplied, so
the QR endpoint remains stable. After applying the network reload, setup verifies
the interface has its address, is administratively UP, and has the expected listen
port. An existing address on a DOWN interface is not accepted.

Some GL.iNet firmware versions return exit status 1 from a successful IPv4 kmwan
restart when IPv6 is disabled. Setup and revert handle that specific status/mode
only after checking that the native engine is readable and currently up IPv4
members are registered. Other exit codes or failed runtime checks remain errors;
revert retains its snapshot on failure. No firmware scripts are modified.

Options:

| Option | Effect |
|---|---|
| `--add-phone` | Add the lowest free phone index, up to eight phones; print only its QR |
| `--phone wgN` | Reconfigure/reprint an existing phone; default `wg0` |
| `--priority N` | Set unique preference; lower numbers win, default `10*(N+1)` |
| `--label NAME` | Display token using letters, digits, dot, underscore or dash |
| `--list-phones` | Read-only inventory; creates no backup and generates no keys |
| `--phone-public-key KEY` | Optional: use an existing phone-managed key instead of generating/importing phone keys |
| `--dns ADDR` | Repeatable public DNS upstream; default `1.1.1.1` |
| `--wan-iface NAME` | Extra backup UCI interface for snapshot/recovery bookkeeping |
| `--port PORT` | Target phone's UDP port; default `51820+10*N`, re-runs preserve its port |
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

## Main and backup phones

First upgrade an existing install by copying the updated scripts and rerunning
setup. Existing generated or manual phone keys and custom ports are preserved;
the first phone does not need to rescan its QR. Then add the second phone:

```sh
sh /root/cellbridge-setup.sh --label main
sh /root/cellbridge-setup.sh --add-phone --label backup
sh /root/cellbridge-setup.sh --list-phones
```

Scan the second QR on the second phone and connect both apps. The router selects
the main phone whenever its cellular internet works, and automatically reclaims
it after recovery even during an active backup session. Switching may reset
connections. Both apps can show Connected; router monitor status identifies
which phone actually carries LAN traffic.

Each phone has independent router/phone keys, a preshared key and subnet. `wg0`
uses `10.10.10.1/24` (phone `.2/24`) and UDP 51820 by default; `wg1` uses
`10.10.11.1/24` (phone `.2/24`) and UDP 51830. Reprint/reconfigure a specific
phone with `--phone wg1`. Change preferences with, for example,
`--phone wg1 --priority 30 --label limited`; ordinary reruns preserve them.

`--add-phone` accepts `--port`, `--priority`, `--label` and
`--phone-public-key` for the new phone. If the first phone already uses UDP
51830, select another for the second, for example `--add-phone --port 51840`.
Conflicts stop setup; it does not silently choose other ports or priorities.
A stale/unregistered `wgN`, conflicting tunnel subnet, or disagreement between
UCI and the inventory needs explicit repair before proceeding. An explicitly
targeted, CellBridge-marked interface can recover a missing inventory entry.
There is no individual remove-phone command yet; revert restores all phones.

Adding a phone leaves other phones' interfaces and credentials alone. Monitor
restart can briefly put LAN traffic onto USB/WAN until health is re-verified.
Do the initial legacy upgrade before adding a phone so the original interface
has its ownership marker and Wi-Fi-only keepalive configuration.

## How failover works

`/etc/init.d/cellbridge-failover` supervises `/usr/sbin/cellbridge-failover` with
procd and starts it after boot. kmwan continues handling physical-uplink health.

- Every round checks HTTPS through the selected phone and higher-preference
  phones, with a three-second request timeout per target. This detects main-phone
  cellular recovery even if it never left Wi-Fi.
- It tries `https://1.1.1.1/cdn-cgi/trace`, then `https://1.0.0.1/cdn-cgi/trace`
  if the first fails. One HTTP 200 with a valid TLS certificate is sufficient.
- Two consecutive good rounds prefer CellBridge; three failed rounds withdraw it.
- Rounds are separated by two seconds; worst-case detection is roughly 24 seconds
  plus scheduling delays. Recovery is usually quicker.
- Checks are numeric-IP, proxy-disabled, IPv4 and interface-bound, so WAN DNS,
  proxies or a captive-portal redirect cannot masquerade as tunnel health.
- Lower-preference idle phones get **no routine cellular internet probes**.
  Every 25 seconds the monitor reads local WireGuard last-handshake/receive
  counters. Router-side keepalives also run every 25 seconds over Wi-Fi; neither
  consumes cellular data. These observations are informational, not proof of
  working cellular internet or an acknowledged keepalive.
- After selected-phone failure, a backup must pass two fresh good rounds before
  selection. Startup qualifies phones in preference order. When no phone works,
  checks continue so CellBridge can recover. Qualification adds time to the
  roughly 24-second worst-case withdrawal delay; takeover is not instantaneous.
- HTTPS qualification and active/higher-preference health tests **do** consume
  cellular data. In the two-phone setup, routine recovery tests go through the
  unlimited main phone, not the idle limited-plan backup.
- Each phone has an isolated probe table, preventing checks from falling through
  to another phone, USB or WAN if its tunnel is unavailable.
- A stop/restart removes the monitor's preferred route. Restart begins on backup
  routing until the tunnel has been rechecked.

The tunnel retains `allowed_ips=0.0.0.0/0` but uses `route_allowed_ips=0`.
The monitor owns table **51820** (one selected tunnel default) and probe tables
**51821+10*N** (`51821` for wg0, `51831` for wg1, through `51891` for wg7).
The range 51820–51891 is reserved and checked for foreign entries. An exact
per-phone tunnel-source rule at priority 0 follows the kernel local rule
and precedes vendor local-output routing. Rules 100/101 preserve connected
main-table routes before consulting the preferred tunnel table. This keeps LAN
access and the phone's outer WireGuard endpoint on the LAN. Routes/rules carry
protocol tag 186. Setup and cleanup validate the reserved entries on every run,
including upgrades, and refuse foreign entries or failed routing inspection.
Cleanup filters by the ownership tag as well.

The root-only `/etc/cellbridge-phones.conf` contains six fields per phone:
interface, source, UDP port, probe table, priority and label. It contains no
keys. The monitor validates it before use. A missing file retains legacy wg0
behavior; an empty or malformed file withdraws tunnel routing and logs an error.
UCI holds actual interface settings and keys; setup checks agreement with the
inventory before reconfiguration. Do not treat the file as a shell script.

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
- the phone inventory's original presence/content (when upgrading a legacy
  snapshot, recorded once before publishing the new inventory).

Copies and metadata are published atomically under a versioned snapshot format,
so an interrupted copy cannot become a reusable partial backup. A completed backup
is never repaired using subsequently modified configuration. Missing required
files/metadata or an incomplete snapshot cause setup/revert to stop and preserve
the snapshot for explicit repair. Do not delete an incomplete snapshot unless you
have established that its originals are safely recoverable.
Multi-phone snapshots use version 3, which requires phone-inventory presence
metadata. Genuine version-2 snapshots can be extended once; missing version-3
metadata is damage and is never reconstructed from the installed inventory.

| Area | Change |
|---|---|
| Network | Up to eight `wgN` interfaces, separate /24 subnets and configurable UDP ports, MTU 1420; one peer per phone |
| Routing | Health-controlled policy tables instead of an unconditional tunnel default |
| Firewall | Shared `vpn` zone for configured phones, masquerade, MSS clamping, `lan → vpn` forwarding |
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
ip link show wg0
wg show wg0 listen-port
wg show wg0 latest-handshakes
wg show wg0 endpoints
wg show wg0 transfer
/usr/sbin/cellbridge-failover status
ip -4 rule show
ip -4 route show table 51820
ip -4 route show table 51821
ip -4 route show table 51831
cat /proc/gl-kmwan/config
logread -e cellbridge-failover
```

`wg0` must show `UP` and a listener on the port in the QR. A DOWN interface with
an address is not ready. Expected with working CellBridge: a recent WireGuard
handshake, `path=tunnel`,
`selected=wg0` (or `wg1` when using the second phone),
and a default via that interface in table 51820. Table 51821 contains wg0's probe
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
For two phones, stop the main phone, wait for `selected=wg1`, then restore the
main and verify `selected=wg0`. Also test cellular loss while the main remains
on Wi-Fi. Check the lower-priority phone is reported `health=unverified` while
idle; its Wi-Fi counters do not certify cellular service.

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
No-snapshot interface removal requires `cellbridge_managed=1`, or proven legacy
wg0 installation provenance plus the expected tunnel/peer shape. An address
match alone never authorizes deleting a tunnel. Shared/foreign firewall entries
are retained. Cleanup inspects all eight routing allocations, including partial
installs without a readable phone inventory.
Before no-snapshot cleanup deletes its UCI ownership evidence, it saves an
owner-only `/etc/cellbridge-cleanup` receipt. Failed cleanup retains that receipt
so a retry can finish firewall, service and device removal. Setup refuses to run
until that cleanup is finished. Do not remove the receipt to bypass a failed
revert.

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
