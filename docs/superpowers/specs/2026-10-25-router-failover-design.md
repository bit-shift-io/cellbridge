# CellBridge internet-health failover

This supersedes the original design's D9 and automatic-failover non-goal.

## Behaviour

Prefer working CellBridge IPv4 internet, then USB tethering, then WAN. Keep
the old hotspot arrangement usable without SSH or a manual revert. If none
has internet, an outage is acceptable. Switching may reset active connections.

## Approach

Add to GL.iNet's kmwan rather than replace it. It continues to monitor and
select physical uplinks, including USB only when attached. Configure its IPv4
USB and WAN members with metrics 1 and 2 respectively, preserving other members
and the monitor's existing timing. Do not add an always-running USB probe.

A procd-supervised shell monitor tests HTTPS over wg0, independently of the
currently selected default route. Two successful rounds select the tunnel;
three failed rounds withdraw it. Each round tries two numeric-IP HTTPS targets,
accepting HTTP 200 with normal certificate validation. Each request has a
three-second timeout; rounds are separated by two seconds. Typical failover
is within about 24 seconds, excluding scheduling delays.

WireGuard keeps allowed_ips=0.0.0.0/0 but route_allowed_ips=0. A private routing
table supplies the preferred default only while the monitor considers internet
healthy. Another table and a source rule keep probes in the tunnel even while
falling back; an unreachable default prevents probes from escaping via WAN.
Connected main-table routes precede the preferred tunnel table, preserving LAN
access and the phone's outer WireGuard endpoint. Reserved tables and rules must
be checked for conflicts before installation and cleanup on every run. Tag owned
routes/rules with protocol 186 and never flush foreign entries. Existing VPN policy rules may
need separate consideration if changed after installation.

When tunnel internet fails, leave selection to existing physical-uplink routing.
There is no benefit to a second custom WAN check: an unusable tunnel is withdrawn
whether or not a backup works. kmwan retains responsibility for backup health.

## Installation and recovery

Setup downloads any missing tools before routing changes, installs the monitor
and init service, and leaves WAN enabled. Snapshot network, firewall, DNS and,
when present, kmwan configuration. Re-runs preserve original snapshots and keys.
Snapshots have explicit version/completion metadata; incomplete or damaged
snapshots require repair and cannot be recaptured from changed live files.
Original boot-enabled and running service states are independent. A successful
restore receipt makes repeated revert leave restored older monitors untouched.
Recover missing installed helpers using the companion supplied beside revert.
Revert stops the monitor and removes only its routes/rules before restoring the
snapshots, including kmwan priorities. Normal stop/crash restart starts on backup
routing until the tunnel has passed checks again.

IPv4 only; existing IPv6 routing is not made part of tunnel failover. Existing
IPv6 uplinks may continue independently. Test results must distinguish host-side
simulation, real Linux namespace routing tests and actual device cutover verification.

## Execution constraints

All live-router access in this session is read-only inspection. No uploads,
package installation, UCI writes, service calls or route changes on the device.
Do not persist credentials, device configuration dumps or site-specific details
in the repository. The user installs/runs the resulting local scripts.
