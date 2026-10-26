# Multi-phone CellBridge priority

Status: approved. Implementation and verification are tracked in
`docs/superpowers/plans/2026-10-26-multi-phone-priority.md`.

## Intent

CellBridge currently supports one phone. The user has two phones that come and
go: the main phone has unlimited data, and the second phone has a limited plan.
The main phone must always be the router's internet path when it is present and
healthy. The second phone is a fallback. When the main phone returns, the router
must switch back to it even if the second phone is carrying traffic. Two phones
are the realistic target, but the design should be index-based so it can grow to
N.

The user chose the router-side priority model: all phones stay connected, and
the router decides which tunnel to use. Phones do not coordinate with each
other.

## Goals

- One WireGuard interface per phone, with independent keys, preshared key,
  tunnel subnet, UDP port, and health checks.
- Strict priority: the highest-priority healthy phone is used. The main phone
  reclaims as soon as it is healthy again.
- The backup phone stays connected but idle. It uses only WireGuard handshake
  and keepalive traffic over Wi-Fi while the main phone is selected; routine
  idle checks must consume no cellular data. Candidate qualification before
  takeover does use a small amount of cellular data.
- Adding a second phone must not rotate or disturb the first phone's keys,
  preshared key, port, or configuration.
- Re-running setup for one phone must leave the other phones alone.
- Revert must remove every phone configured by CellBridge and restore the
  original router configuration.
- The phone app needs no changes for this feature. Each phone scans its own QR
  and connects exactly as it does today.
- Existing single-phone installs upgrade without re-scanning the existing
  phone's QR or changing its keys.

## Non-goals and anti-goals

- No load balancing or bandwidth aggregation across phones.
- No automatic detection of which phone has an unlimited plan. Priority is
  configured explicitly.
- No phone-to-phone coordination, presence detection, or app-side "standby"
  state.
- No seamless session migration. Switching paths may reset active connections.
- No IPv6 support.
- No router web UI. Setup remains a CLI script.
- No change to the USB-tether and WAN fallback behavior owned by GL.iNet kmwan.
- Do not delete or modify unrelated WireGuard interfaces, peers, firewall
  zones, policy rules, or routing tables.
- Do not store real credentials, device keys, or site-specific details in the
  repository. All live-router work remains user-run.

## Constraints

- Router: GL.iNet Flint 3, stock OpenWrt with fw4/nftables and GL.iNet kmwan.
- Router scripts are POSIX sh / BusyBox ash only.
- The phone remains a normal Wi-Fi client; no root, no `VpnService`.
- WireGuard interfaces cannot share one `allowed_ips=0.0.0.0/0` peer set
  unambiguously. Separate phones therefore need separate WireGuard interfaces.
- The router is the WireGuard server and the phone is the initiator.
- The existing health monitor owns policy routing tagged `proto 186`; foreign
  entries must never be flushed or reused.
- The user runs setup, revert, and verification on the real router. Local work
  is limited to scripts and sandbox/namespace tests.

## Decided behavior

- The main phone is the highest-priority phone. By default the first phone
  configured (`wg0`) is the main phone. Priority is explicit and can be changed.
- The main phone always takes over when it has passed health checks, even if
  the backup is mid-stream.
- Two phones are the target. The data model and scripts support up to 8
  (indices 0 through 7); raising the cap is a constant change, not a redesign.
- All phones remain connected. The backup phone is not told to disconnect.
- Idle Wi-Fi-only checks run every 25 seconds, within the user's requested
  10–30 second range. Wi-Fi liveness is distinct from cellular internet health.
- Lower priority number means preferred. Defaults are `10` for `wg0`, `20` for
  `wg1`, and so on. Priorities must be unique.

## Architecture

```
LAN clients
    |
  Flint 3
    |-- policy rule 100: main table, suppress prefix 0 (LAN and phone endpoints)
    |-- policy rule 101: active tunnel table (one selected phone)
    |-- policy rules 0: per-phone probe tables
    |
    |-- wg0 (10.10.10.1/24, UDP 51820) --- Wi-Fi --- phone A (main, priority 10)
    `-- wg1 (10.10.11.1/24, UDP 51830) --- Wi-Fi --- phone B (backup, priority 20)
```

Each phone gets its own WireGuard interface. The interface has:

- `proto=wireguard`, `listen_port=<port>`, `mtu=1420`, `nohostroute=1`;
- one tunnel address, for example `10.10.11.1/24`;
- exactly one peer with the phone's public key, preshared key, generated
  private-key metadata, `allowed_ips=0.0.0.0/0`, `route_allowed_ips=0`, and
  `persistent_keepalive=25`;
- `cellbridge_managed=1` as UCI metadata (netifd ignores it) so revert can
  prove ownership before deleting anything;
- membership in the shared `vpn` firewall zone (masquerade and MSS clamping);
- an entry in the phone configuration file used by the health monitor.

`persistent_keepalive` is an empty encrypted WireGuard packet sent over the
phone's Wi-Fi link, not inner IP traffic, so it consumes no cellular data. It
helps keep the idle phone's session warm. It does not prove cellular internet
works or guarantee that a returning phone already has a live session.

All `wgN` interfaces share one `vpn` firewall zone and one `lan -> vpn`
forwarding rule. The zone masquerades every interface in it.

## Allocation

For index `N` (0-based, `wg0` is 0):

| Item | Value |
|---|---|
| Interface | `wgN` |
| Router tunnel source | `10.10.(10+N).1` |
| Phone tunnel address | `10.10.(10+N).2/24` |
| UDP port | `51820 + 10*N` by default |
| Probe table | `51821 + 10*N` |
| Default priority | `10 * (N+1)` |

Examples:

| Phone | Interface | Source | Phone address | Port | Probe table | Priority |
|---|---|---|---|---|---|---|
| main | wg0 | 10.10.10.1 | 10.10.10.2/24 | 51820 | 51821 | 10 |
| backup | wg1 | 10.10.11.1 | 10.10.11.2/24 | 51830 | 51831 | 20 |

`--port` and `--priority` override the defaults for the target phone. The setup
script validates that the proposed port is free and that the proposed subnet
does not already exist in routing or UCI. An explicit port for `wg0` preserves
the existing re-run behavior: the configured port is reused unless `--port` is
given.

Builder call: keep index-based allocation predictable. If the computed port,
subnet, or default priority collides, stop before edits rather than silently
selecting a different index or value. Port and priority conflicts can be resolved
with `--port` and `--priority`; a subnet conflict needs operator repair because
custom tunnel subnets are outside this version's CLI.

The active tunnel table is always `51820`. The probe table is per phone. The
reserved range is `51820` through `51891` (active table plus eight probe
tables). These tables and the owned policy rules remain tagged `proto 186`.

## Routing and health selection

The monitor reads the phone configuration file and separates Wi-Fi tunnel
liveness from end-to-end cellular health:

- Every 25 seconds, inspect idle peers' local WireGuard handshake/receive
  state. Router-side keepalives run over Wi-Fi. Reading state and exchanging
  these keepalives use no cellular data. A sent keepalive is not an
  acknowledgement; stale handshakes alone must not be treated as proof that
  a phone has disconnected. Report idle liveness as last-seen/unknown, not
  verified cellular health. No new app responder is required.
- Probe the selected phone's internet every health round.
- Also probe phones with higher preference than the selected phone each health
  round so the main phone can reclaim after cellular service returns, including
  when it never left Wi-Fi. These HTTPS checks use cellular data on those
  preferred phones; in the user's two-phone setup this is the unlimited main
  phone. Wi-Fi state alone cannot detect cellular recovery.
- Do not routinely HTTPS-probe phones below the selected phone in preference.
  Their cellular health is unverified while idle. When the selected phone is
  withdrawn, probe fallback candidates immediately and require two consecutive
  good rounds before selecting one. Do not reuse stale idle health counters.
- At startup, or when no phone is selected, qualify candidates in priority
  order. Probe the highest-preference candidate until it passes two good rounds
  or fails a round, then try the next candidate. Once a phone is selected,
  resume the selected/preferred probing schedule above. This avoids testing
  the limited backup unnecessarily when the main phone works.

For each phone that needs an internet check:

- Prepare the phone's probe table:
  - `unreachable default metric 32767 proto 186`
  - `default dev wgN src 10.10.(10+N).1 metric 10 proto 186`
- Add one source-specific probe rule per phone at priority 0:
  `from 10.10.(10+N).1/32 lookup <probe_table> proto 186`.
  Multiple rules at priority 0 are unambiguous because each matches a different
  source. They must precede vendor local-output rules.
- Probe HTTPS through that phone's interface, for example
  `curl --ipv4 --noproxy '*' --interface wgN ...`, with the existing numeric
  HTTPS targets, certificate validation, and three-second timeout. The
  source-specific rule keeps the probe in the phone's probe table even when a
  different phone is active. The unreachable guard prevents probes falling
  through to USB or WAN.
- Keep per-phone good/bad counters: two consecutive good rounds make a phone
  healthy; three consecutive failed rounds make it unhealthy.

Run scheduled checks independently and in parallel where applicable. After the
probes, select the highest-preference freshly qualified phone, retaining the
selected phone until its three-failure threshold is reached. An untested idle
phone is not eligible merely because its Wi-Fi tunnel is present. Maintain
exactly one default route in
the shared active table `51820`:

- If a phone is selected and the active route is missing or points at another
  phone, replace it with
  `default dev wgN src 10.10.(10+N).1 metric 10 proto 186`.
- If no phone is healthy, flush the owned active route so kmwan's USB/WAN
  backup selection applies.
- Log a line when the selected phone changes.

Because there is one active table, the existing rule `101: from all lookup
51820` and rule `100: lookup main suppress_prefixlength 0` stay unchanged. The
main phone reclaims as soon as it becomes healthy: the monitor replaces the
active route with `wg0`, and the router switches back. Connections may reset.

If the backup phone is idle, its probe table remains available and its tunnel
stays configured, but it carries neither LAN traffic nor routine HTTPS probes.
Its keepalives and handshakes stay on Wi-Fi and use no cellular data.

Keep the existing two-second sleep between health rounds. Each round can also
spend up to six seconds trying two HTTPS targets. Three failed rounds therefore
take roughly 24 seconds plus scheduling delays to withdraw a selected phone;
fallback qualification adds two successful rounds. Do not promise a fixed
four-second takeover. Higher-preference recovery requires two good rounds.

## Phone configuration file

Setup writes a root-only file, `/etc/cellbridge-phones.conf`, atomically. The
monitor reads it. It contains no private keys.

```
# interface source port probe_table priority label
wg0 10.10.10.1 51820 51821 10 main
wg1 10.10.11.1 51830 51831 20 backup
```

The label is a display-only token with no whitespace; use `-` when none is
wanted. The monitor validates the file strictly before using it: interface name
`wg` followed by digits, IPv4 source, port 1-65535, probe table in the reserved
range, unique integer priority, and a safe label. Invalid configuration means
no tunnel route is installed; the monitor logs the problem and leaves backup
routing alone.

If the file is absent, the monitor falls back to a single implicit entry for
`wg0` (`10.10.10.1`, port `51820`, probe table `51821`, priority `10`). This
keeps an existing single-phone install working if only the monitor is updated.
Setup writes the explicit file on its next run.

Builder call: UCI is authoritative for interface configuration, keys, source,
and port; this file is authoritative for priorities and labels and supplies the
monitor's validated routing inputs. Setup checks agreement before edits. A
conflicting source/port or a listed but missing UCI interface causes a clear
repair error, never automatic key regeneration. The running monitor treats a
missing interface as unavailable while continuing with other candidates.
An explicit `--phone wgN` may restore a missing file entry for an interface
marked `cellbridge_managed=1`, preserving its UCI keys and port. It refuses an
unmarked interface except for the validated legacy `wg0` upgrade described below.
An unrelated UCI tunnel absent from the file is never probed or adopted.

## Setup CLI

The existing script keeps its current behavior for one phone and gains:

| Option | Effect |
|---|---|
| `--add-phone` | Add the next free phone, generate/reuse its keys and preshared key, print its QR, and print the full phone list |
| `--phone wgN` | Target an existing phone for reconfiguration or QR reprint; default target is `wg0` |
| `--priority N` | Set the target phone's priority; lower is preferred; must be unique |
| `--label NAME` | Set the target phone's display label |
| `--list-phones` | Read-only list of configured phones and priorities |
| `--port PORT` | Set the target phone's UDP port (existing meaning for `wg0`) |
| `--phone-public-key KEY` | Manual-key mode for the target phone (existing meaning) |

`--dns`, `--wan-iface`, and `--keep-wan` remain global. `--add-phone` refuses to
run when the cap is reached or when a stale `wgN` exists in UCI but not in the
phone configuration file; it must not silently adopt or overwrite a foreign or
half-configured interface.

`--add-phone` and `--phone` are mutually exclusive. With `--add-phone`,
`--port`, `--priority`, `--label`, and `--phone-public-key` all apply to the
new phone, including manual-key mode.

Adding a phone:

- chooses the lowest free index;
- creates only that `wgN` interface and peer;
- generates or reuses that phone's private key, public key, and preshared key;
- adds `wgN` to the existing `vpn` firewall zone without dropping other members;
- adds or updates only that phone's line in the phone configuration file;
- leaves every other phone's interface, keys, port, and firewall membership
  untouched;
- restarts the monitor so the new phone is available as a candidate;
- prints only the new phone's QR plus a summary of all phones.

Re-running setup for an existing phone (`--phone wgN`, or the default `wg0`)
reuses its keys, preshared key, port, priority, and label unless an explicit
option changes the corresponding value, reconciles that interface, and leaves
the other phones alone. Changing priority or label updates only the phone
configuration file and restarts the monitor.

Builder call: adding a phone may briefly withdraw the active route during the
monitor restart, allowing USB/WAN fallback until health is re-verified. This is
a maintenance interruption, not a promise of uninterrupted LAN sessions. It
must not reconfigure or restart other phones' interfaces. Ownership metadata
and keepalive are added to legacy `wg0` during its explicit upgrade, not during
an unrelated add-phone operation.

`--remove-phone` is not required for the first implementation. Revert removes
all phones. A future removal command can reuse the same per-phone cleanup.

## Migration and backward compatibility

- An existing single-phone install upgrades by re-running setup without
  `--add-phone`. Setup reuses the existing `wg0` keys, preshared key, and port,
  writes the new phone configuration file with `wg0` at priority 10, installs
  the updated monitor, and starts it.
- The explicit legacy upgrade also stamps `cellbridge_managed=1` and adds
  router-side keepalive; neither changes the phone's QR or credentials.
  Require installation provenance (`/etc/cellbridge-installed` or a valid
  existing CellBridge snapshot), the expected WireGuard tunnel address, and a
  single peer with the existing CellBridge routing shape before adopting an
  unmarked legacy `wg0`. Preserve custom ports and manual-key installations.
  An address match alone does not establish ownership.
- The existing phone's QR does not need to be re-scanned.
- The updated monitor supports an absent phone configuration file by using the
  implicit `wg0` entry, so a monitor-only update is safe.
- The setup preflight validates the phone configuration file and every reserved
  table/rule for every configured phone before changing anything.
- The second phone is added with `--add-phone`, then scans its own QR and
  connects. No app update is required.

## Firewall and DNS

- All `wgN` interfaces join the existing `vpn` zone. The zone keeps
  `masq=1` and `mtu_fix=1`; the existing `lan -> vpn` forwarding is shared.
- Setup adds each interface to the zone's network list without removing
  existing members.
- The LAN zone input policy still governs the phones' UDP handshakes. The setup
  warning must mention every configured port, not only 51820.
- DNS behavior is unchanged: public resolvers follow whichever IPv4 path is
  selected.

## Revert and cleanup

- The monitor stops before configuration is restored.
- Cleanup removes every owned probe table, probe rule, active route, and active
  rule, including tables/rules for all configured phones. It must validate
  ownership first and never flush foreign entries.
- The phone configuration file is removed.
- The original network/firewall snapshot is immutable. Restoring it removes
  subsequently added phones, their peers and private-key metadata, and their
  firewall memberships, while restoring any originally present interfaces.
- Builder call: no-snapshot targeted cleanup requires `cellbridge_managed=1`
  on each interface before removing its peers and firewall membership. Never
  infer ownership of `wg1` or later from an address pattern. For pre-marker
  `wg0` only, allow the same provenance-plus-configuration legacy predicate
  used by setup. Without sufficient proof, report the interface and leave it
  intact. Preserve unrelated zone members and forwarding rules; remove shared
  firewall objects only when their own CellBridge ownership is established.
- Remove the phone configuration file only after routing cleanup has used its
  entries. Routing cleanup must also recognize owned allocations left by a
  partial install, without depending exclusively on a valid file.
- Revert must be safe to run more than once.

## Failure behavior

- Selected phone unhealthy: qualify fallback candidates and select the best
  one after two good rounds; use USB/WAN routing during qualification.
- Main phone returns: after two good rounds it is selected again.
- All phones unhealthy: no active tunnel default; kmwan uses USB tether, then
  WAN. If neither works, there is no internet.
- Selected phone leaves Wi-Fi or loses cellular internet: its HTTPS probes
  fail and it is withdrawn after three failed rounds. Idle Wi-Fi observations
  are informational; they do not certify or disqualify cellular connectivity.
- Main phone remains on Wi-Fi while cellular service recovers: preferred-phone
  HTTPS checks detect recovery and it reclaims after two good rounds.
- A malformed phone configuration file: no tunnel route is installed; the
  monitor logs an error and backup routing continues. Setup's preflight should
  catch this before installation.
- A reserved table or rule contains a foreign entry: setup and monitor refuse
  to proceed rather than flushing it.

## Security

- Each phone has its own WireGuard private/public key pair and preshared key.
  Compromising one phone's credentials does not affect the other.
- Router private keys remain root-only in `/etc/config/network`; generated
  phone private keys remain in per-peer metadata covered by the snapshot.
- The phone configuration file contains no secrets and is root-only.
- QR codes and printed payloads remain private key material.
- The monitor never evaluates data from the phone configuration file as shell
  code; all values are validated and used as fixed command arguments.

## Known limitations

- Both phones' apps show "Connected" while their tunnel is up. The backup
  phone does not know the router is preferring the main phone. The router's
  monitor status is authoritative. A future enhancement could expose the
  selected phone to the app, but that needs a router control channel.
- Switching phones resets active connections.
- The backup phone uses only idle tunnel traffic while not selected, but it is
  still connected; the user cannot make it disconnect automatically from the
  phone side.
- Wi-Fi-only checks cannot certify cellular service. Candidate qualification
  and active/preferred-phone HTTPS checks consume cellular data. Status must
  distinguish idle/unverified from internet-healthy.
- Strict priority only; no load balancing.
- IPv4 only.

## Testing

Local work must not touch the live router.

- Extend the router Python tests for CLI parsing, index/port/subnet
  allocation, per-phone key reuse, priority ordering, idempotent re-runs,
  firewall membership, phone configuration file writing/validation, and revert
  cleanup.
- Extend the monitor tests for multiple probe tables/rules, parallel probing,
  priority selection, main-phone reclaim, backup idle behavior, all-unhealthy
  behavior, malformed configuration, missing interfaces, ownership checks, and
  status output.
- Verify no HTTPS request is issued through a lower-preference idle phone,
  Wi-Fi checks run every 25 seconds, stale handshake state is not mistaken for
  cellular failure, fallback qualification resets old counters, and main-phone
  cellular recovery is detected without a Wi-Fi reconnection.
- Extend the network-namespace routing tests to simulate two tunnel interfaces
  and verify that the main phone's table is preferred when healthy and the
  backup is used only when the main is withdrawn.
- Keep the existing single-phone tests passing, including the implicit `wg0`
  fallback when the phone configuration file is absent.
- Run `sh -n` on all router scripts and the existing test suites.
- The user runs the real setup, adds the second phone, and verifies on the
  router with `wg show`, the monitor status, policy rules, and route tables.

## What is left to the builder

- Exact shell function decomposition, atomic file handling, and ordering of
  setup steps so a failure leaves a recoverable state.
- Exact CLI parsing, validation messages, and help text.
- Exact status file format details, as long as it reports the selected phone
  and each phone's priority, health, and counters.
- Exact parallel-probe implementation in POSIX sh.
- Exact strict-validation and ownership-check helpers for the reserved tables
  and rules.
- Test harness details for fake UCI and network namespaces.
- Documentation updates to `router/README.md`,
  `cellbridge_router_setup_guide.md`, and the architecture summary.
- Whether to implement `--remove-phone` now or later. It is not required for
  the first build.
- Whether to expose the selected phone anywhere outside the monitor status.
