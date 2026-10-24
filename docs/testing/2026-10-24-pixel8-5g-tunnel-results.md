# Pixel 8 5G full-tunnel verification — 2026-10-24

## Setup

- Physical Pixel 8, Android 17, arm64, installed from the release APK.
- Phone on Wi-Fi `spud_5` (`192.168.1.157`) and later `spud_2`
  (`192.168.1.80`); Vodafone LTE network `100`.
- Fedora peer on Ethernet `192.168.1.126:51820`, paired with the phone's
  generated public key. The phone's Wi-Fi socket was bound to network `113`.
- A second phone supplied USB-tethered internet to Fedora during the test.
- No adb egress override: diagnostics reported
  `egressTestOverride:false` and `cellularBindVerified:true`.

## Results

| Check | Result |
|---|---|
| Configuration handshake test | **Passed**: "Authenticated WireGuard handshake succeeded over Wi-Fi." |
| Bridge start | **Connected**; cellular network `100`, Wi-Fi network `113` |
| DNS (`example.com` via `1.1.1.1:53`) | **Passed**, 61-byte response |
| TCP echo (`tcpbin.com:4242`) | **Passed**, 4-byte response |
| HTTP (`portquiz.net:80`) | **Passed**, 2,631-byte response |
| Egress IP via `ifconfig.me/ip` over the tunnel | `120.20.1.62` |
| Direct cellular control IP | `120.20.1.62` |
| Fedora direct egress IP (second phone) | `120.21.25.199` |
| Screen off | DNS still **passed** |
| Wi-Fi disabled | Service entered `error`; DNS timed out |
| Wi-Fi restored | Service auto-recovered to `connected`; DNS passed |
| Reboot, before first unlock | Service correctly absent (no `BOOT_COMPLETED` yet) |
| Reboot, after unlock | Service auto-started and reconnected; DNS passed |
| App swiped from recents | Task gone, foreground service stayed up; DNS passed |
| Disconnect / Connect in UI | Service stopped; DNS timed out. Reconnected; DNS passed |

The tunneled egress IP matching the direct cellular control IP, while differing
from Fedora's egress, is direct evidence that the peer's tunnel traffic was
re-originated on the phone's cellular network rather than leaking through Wi-Fi.

## Follow-up observation

Some public HTTP endpoints (`example.com`, Cloudflare-backed hosts, and
`httpbin.org`) closed without returning a response to the test client, while
`portquiz.net` and `ifconfig.me` returned full responses. The test client closes
its write side immediately after sending the request. This did not prevent
successful TCP/HTTP forwarding, but it is worth revisiting if LAN clients use
half-close before reading a response.

## Not covered

- The actual Flint 3 router cutover, its routing/NAT/firewall rules and rollback.
- Long-duration throughput, battery and thermal behavior.
- Real LAN-client traffic from a router instead of the stand-in peer.
