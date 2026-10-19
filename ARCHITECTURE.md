# CellBridge Architecture

## 1. Network Topology & Routing Problem
* **The Constraint:** Android forces a choice between Wi-Fi and Cellular. Standard apps bind to whichever network interface is active. Turning on a mobile hotspot drops local Wi-Fi, severing LAN access.
* **The Solution:** CellBridge keeps the phone connected to Wi-Fi (`router.lan` / GL-BE9300 at `192.168.8.1`), spins up a local `tun0` interface via Android's `VpnService`, captures all internet-bound traffic, and pushes it through a UDP socket **explicitly pinned to the cellular interface**, connecting to the Flint 3 router's local WireGuard endpoint (`192.168.8.1:51820`).

## 2. Component Interaction Diagram

```
[Android Apps] ---> (tun0 virtual interface)
                         │
                         ▼
             [Android VpnService (Kotlin)]
                         │
             (Passes FD & Cellular Socket)
                         │
                         ▼
             [Flutter-Rust Bridge]
                         │
                         ▼
             [Rust Core (boringtun)] ---> (Encrypted UDP Packets)
                                                 │
                   ┌─────────────────────────────┘
                   ▼ (Pinned via ConnectivityManager to Cellular)
             [Cellular Data Network] ---> [GL-BE9300 / Flint 3 (WireGuard Server)]
                                                  │
                                                  ▼
                                         [Bridged to Home LAN / WAN]
```

## 3. Data Flow Step-by-Step
1. **App Traffic Generation:** Any app on your phone requests data. Android routes it into the virtual `tun0` interface created by `VpnService`.
2. **Rust Ingestion:** The Rust core (`boringtun`) reads the raw IP packets from `tun0`, packages them, and encrypts them using the WireGuard protocol.
3. **Cellular Pinning:** The encrypted UDP packets are sent out through a socket that the Android platform code explicitly bound to the active `TRANSPORT_CELLULAR` network interface.
4. **Local Transport via Wi-Fi LAN:** The underlying IP packet payload (addressed to `192.168.8.1:51820`) travels over your home Wi-Fi network to the GL-BE9300 router. Because it's just a standard UDP destination on your local subnet, it bypasses Android's default gateway restriction without triggering a loop.
5. **Router Decapsulation & Bridging:** The GL-BE9300 receives the packet on its WireGuard server interface, decrypts it, applies IP Masquerading, and pushes the data out to its WAN or bridges it across your home network.

## 4. Security & Isolation Rules
* **Local LAN Bypass:** Subnet traffic (e.g., `192.168.8.0/24`) bypasses the VPN tunnel entirely, allowing your phone to communicate directly with local home servers, printers, and devices over Wi-Fi.
* **Cellular Exclusivity:** All external internet traffic (`0.0.0.0/0`) is captured by `tun0`, encrypted in Rust, and forced out over mobile data.