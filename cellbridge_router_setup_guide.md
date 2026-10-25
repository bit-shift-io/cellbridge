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
4. After setup finishes, switch the phone from hotspot to the router's Wi-Fi
   client connection, leaving mobile data enabled.
5. Scan the printed setup QR. If it would replace different existing phone keys,
   answer **Yes** to overwrite or **No** to cancel the import without changing
   anything. Save and connect in CellBridge. The router keeps
   backup routing until tunnel internet has passed two consecutive checks.

## Phone settings

| Setting | Value |
|---|---|
| Private key | Generated/reused by setup, imported through QR, stored encrypted by the app |
| Address | `10.10.10.2/24` |
| Peer public key | Router public key printed by setup |
| Preshared key | Generated/reused by setup; included in QR |
| Endpoint | The router's detected LAN address, UDP port `51820` |
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
