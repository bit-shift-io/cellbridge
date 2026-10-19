# CellBridge Architecture

## 1. Network Topology & Routing Problem
* **The Constraint:** Android forces a choice between Wi-Fi and Cellular. Standard apps bind to whichever network interface is default. Turning on a mobile hotspot drops local Wi-Fi, severing LAN access.
* **The Solution:** CellBridge keeps the phone connected to Wi-Fi (`router.lan`), spins up a local `tun0` interface via Android's `VpnService`, captures all internet-bound traffic, and pushes it through a UDP socket **explicitly pinned to the cellular interface**, connecting to the Flint 3 router's local IP address (`192.168.8.1:51820`).

## 2. Component Interaction