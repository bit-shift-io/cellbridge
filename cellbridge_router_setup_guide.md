# Flint 3 configuration for CellBridge

The router sends LAN IPv4 internet traffic through a WireGuard tunnel to the phone,
which stays a normal Wi-Fi client with cellular data enabled. A health monitor
prefers CellBridge only when internet actually works through it. GL.iNet kmwan
remains enabled for USB-tether then WAN fallback.

See **[router/README.md](router/README.md)** for the full install, backup,
verification and revert procedure. This supersedes the old fail-closed setup.

## Sequence

1. Keep the current hotspot/WAN internet working while setup installs dependencies.
2. Copy all three scripts from `router/`: setup, revert and failover.
3. Keep LAN SSH open and run `sh /root/cellbridge-setup.sh`. It generates/reuses
   both router and phone key pairs; no phone public-key copy/paste is needed.
   Setup stops before changing anything if UDP 51820 is already used (for
   example by GL.iNet's `wgserver`); choose another port with
   `sh /root/cellbridge-setup.sh --port 51830`.
4. After setup finishes, switch the phone from hotspot to the router's Wi-Fi
   client connection, leaving mobile data enabled.
5. Scan the printed setup QR. If it would replace different existing phone keys,
   answer **Yes** to overwrite or **No** to cancel the import without changing
   anything. Save and connect in CellBridge. The router keeps
   backup routing until tunnel internet has passed two consecutive checks.

## Phone settings

For a main and backup phone, upgrade the first install by rerunning the updated
setup, then add the backup:

```sh
sh /root/cellbridge-setup.sh --label main
sh /root/cellbridge-setup.sh --add-phone --label backup
sh /root/cellbridge-setup.sh --list-phones
```

Scan each QR only on its intended phone. Existing first-phone keys and custom
ports are preserved. `wg1` defaults to `10.10.11.2/24` on the phone and UDP 51830
on the router. If the first phone uses 51830 already, pass an unused `--port`
with `--add-phone`. Up to eight phones are supported. Lower priority numbers
win (`wg0` defaults to 10, `wg1` to 20); set one with
`--phone wg1 --priority 30`. `--phone wg1` also reprints that phone's QR.

| Setting | Value |
|---|---|
| Private key | Generated/reused by setup, imported through QR, stored encrypted by the app |
| Address | `10.10.10.2/24` |
| Peer public key | Router public key printed by setup |
| Preshared key | Generated/reused by setup; included in QR |
| Endpoint | The router's detected LAN address and setup's WireGuard UDP port (default `51820`, or `--port PORT`) |
| MTU | `1420` |

## Backups and recovery

Setup snapshots network, firewall, DNS, kmwan and any previous managed monitor
files/service state under `/etc/cellbridge-backup/`. First backups are retained
across re-runs. Revert stops/removes new tunnel health routing, restores the
original configuration and retains backups if restoration fails.
Generated phone keys are kept in root-only network peer metadata and are covered
by the network snapshot. Treat the QR and raw payload as private key material.

```sh
sh /root/cellbridge-revert.sh
```

The installed monitor's status is available with:

```sh
/usr/sbin/cellbridge-failover status
logread -e cellbridge-failover
```

Test both fallback and automatic return to the tunnel. Existing connections may
reset and failover can take roughly 24 seconds. If no path has internet, an outage
is expected. IPv6 remains outside CellBridge tunnel failover.

With two phones, expect `selected=wg0` normally, `selected=wg1` after main-phone
failure and fresh backup qualification, then `selected=wg0` after recovery.
Qualification adds two successful rounds to withdrawal time. The main reclaims
even during active backup traffic. Both apps may show Connected; check router
status for the selected path.

Idle backup Wi-Fi checks and keepalives run every 25 seconds and use no cellular
data. They do not prove cellular internet works. The router tests cellular
internet before promoting a backup and routinely tests the selected and
higher-preference phones; these HTTPS requests do use cellular data. An idle
limited-plan backup gets no routine HTTPS requests while the main is selected.
