# CellBridge — On-Device Verification Harness (Stand-In Router)

**Date:** 2026-10-23
**Status:** Approved direction; updated following builder review and user clarification
**Topic:** Prove the phone-to-internet data path on real hardware before touching the Flint 3

---

## 1. Intent and why

The router change (spec §9, plan Task 14) is the first disruptive step against
the Flint 3, and the on-device verification it feeds (plan Task 15) assumes the
router is already cut over. If the phone side is wrong, the LAN loses internet
and the only recovery is the manual revert script.

This document adds the missing stage **before** Task 14: validate the phone's
data path against a **stand-in router** — a program we control, running on the
Fedora machine — so that a platform bug is found while the Flint 3 is still
untouched and the network still works.

It also builds the production Android service (Tasks 10–11) rather than a
throwaway harness. The service is the real deliverable; this document adds the
**seams that make it drivable and observable in a staged test**, and specifies
the stand-in router and the staged procedure.

---

## 2. Relationship to existing documents

- **Supersedes the staging** in spec §11 ("Testing plan") and refines plan
  Task 15 by inserting stages before Task 14. Task 15's later stages
  (throughput, fail-closed, boot, soak) are unchanged.
- **Does not change** spec decisions D1–D10, the topology, or the data path.
- **Tasks 9–11 remain as planned** (Kotlin `NetworkBinder` is done; Tasks 10–11
  are the build). This document adds testability requirements to Task 10/11 and
  one new artifact (the stand-in router).
- Reuses the existing Rust harness concepts (`rust/tests/support/mod.rs`) and the
  public core API (`rust/src/lib.rs`).

---

## 3. Goals

- **V1** — Prove the **production Rust bridge loop** (`bridge::start`) carries
  TCP and UDP to the real internet, on the host, with no phone involved.
- **V2** — Prove the **Android platform steps** work on the phone: request
  cellular, bind the process to cellular, bind the WireGuard socket to Wi-Fi,
  and complete a WireGuard handshake to a peer over the LAN.
- **V3** — Prove **egress is cellular** and that the phone **retains LAN
  access** the whole time.
- **V4** — The app is built so the above is drivable and observable without
  editing code between runs.

## 4. Non-goals

- Replacing Task 14 (the real router configuration) or Task 15 (throughput,
  boot, soak).
- Automatic failover, IPv6, multiple exits (unchanged from spec §3).
- A polished diagnostics UI. The diagnostics surface here is for the test; a
  product diagnostics screen stays in the spec's "left to the builder".

## 5. Anti-goals

- A test that can only run by hand-editing constants between stages.
- A test that changes Fedora's routing or requires `sudo`, so that the machine's
  internet (and the LLM session) survives a failed run.
- A stand-in router that shares so little of the phone's path that a pass means
  nothing.

---

## 6. Topology

```
   Fedora (wired to Flint 3, always)                Phone (device under test)
   ┌────────────────────────────────┐               ┌─────────────────────────┐
   │ stand-in router                │               │ CellBridge app          │
   │  wg responder, 10.10.10.1/24   │  WireGuard    │  BridgeService          │
   │  injects TCP/UDP test flows    │◄─────────────►│  Kotlin: cellular+Wi-Fi │
   │  logs + asserts                │  over the     │  Rust: boringtun+smoltcp│
   │                                │  Flint 3 LAN  │                         │
   │ internet ── usb0 ◄─ USB tether │               │      └─ cellular-bound  │
   │            (phone's cellular)  │               │            sockets      │
   └────────────────────────────────┘               └────────────┬────────────┘
        Fedora's internet comes from the phone's USB tether        ▼
        Flint 3 uplink state is irrelevant to the test          Cellular → Internet
```

- The phone is a **Wi-Fi client** of the Flint 3, with the app running.
- Fedora is **wired to the Flint 3**, so it reaches the phone's Wi-Fi address
  over the LAN with no routing changes of its own.
- Fedora can use USB tethering from the same phone to keep internet access when
  the phone stops providing its hotspot. Flint 3 may have internet or not;
  neither disabling nor changing its uplink is a test prerequisite.
- Nothing on Fedora is asked to route, NAT, or create an interface.

---

## 7. Why the test is faithful

- The stand-in router's test traffic originates from **`10.10.10.1`** — the exact
  address the real router masquerades LAN clients to (spec D4). The phone's
  gateway sees precisely what production feeds it, and the source-port rules for
  UDP (spec §8.5) are exercised identically.
- Verify the selected Android network has cellular transport and that process
  binding succeeds before starting Rust. Record the network identity and
  transport in diagnostics. Fetch a public-IP response through the tunnel and
  compare it with a direct cellular-bound control request. IP equality is
  supporting evidence, not sufficient proof by itself (both paths can share
  an upstream). Never infer cellular egress merely from a successful fetch.
- The **hot part of the code is the same**: the phone runs the real
  `bridge::start` loop, the real `boringtun`, the real `smoltcp` gateway and the
  real splices.

The intentional fidelity gap is the stand-in router's userspace client rather
than a kernel TCP stack. Both TCP legs matter; a pass does not establish all
real-client TCP behavior or router NAT/firewall correctness. Use smoltcp for
the tool's client-side TCP so ACKs, retransmission, segmentation and flow control
are implemented by a maintained stack rather than the minimal echo-test client. The
"real `curl` against a kernel WireGuard interface" alternative is recorded in
§13 as a rejected option.

---

## 8. Deliverables

1. **Stand-in router** — a Rust binary that speaks WireGuard as the responder
   and injects scripted TCP/UDP flows (§9).
2. **Service layer** (plan Tasks 10–11) with the testability seams in §10.
3. **Diagnostics surface** — a bounded, readable log of bridge lifecycle events
   (§10.3).
4. **Staged procedure** — the runbook in §11.
5. **Host bridge launcher** — a maintained CLI which calls the production
   Rust start/stop API with a configuration file and no Android socket fd,
   allowing stage 1 to run as documented commands.

---

## 9. The stand-in router

A single Rust binary in the existing crate, `rust/src/bin/standin_router.rs`
(or an `examples/` target — see §13 for the call), built with
`cargo build --bin standin_router`. It speaks WireGuard using the crate's own
`Tunnel` wrapper, so it exercises the same `boringtun` the phone does.

### 9.1 Behaviour

- Binds a UDP socket on `--listen <addr:port>` (default `0.0.0.0:51820`).
- Acts only as the **responder**: it never initiates. The phone initiates
  (spec §9), so the stand-in router, like the real router, learns the peer's
  endpoint from the incoming handshake and needs no knowledge of the phone's IP
  or port.
- Generates and stores its private key in an owner-only key file and displays
  its public key for entry in the app. Accepts the phone public key as an
  argument. Normal diagnostics never print private keys or payloads.
- Tunnel address `10.10.10.1/24`; `--mtu` default `1420`.
- Loop: `recv_from` → `Tunnel::handle_udp` → send `to_network`; `Tunnel::tick`
  on a fixed cadence; drive the scripted scenario's client state machine on
  `to_tunnel`; send `to_network`.
- Logs every inner packet in a short, greppable form (direction, 5-tuple, flags,
  length) and every handshake transition, to stdout.

### 9.2 Scenarios

Selected by a subcommand; each exits `0` on the asserted outcome, non-zero with
a clear message otherwise, and prints a machine-readable summary line.

- **`handshake`** — wait for a completed session; print the time from start and
  exit. Proves only the tunnel.
- **`tcp --host H --port P [--send TEXT] [--expect-regex RE]`** — originate a
  TCP connection from `10.10.10.1:<ephemeral>`, complete the handshake, send
  `TEXT` (default an HTTP/1.0 `GET /` on port 80, or a TLS-less request), read
  the reply, assert it matches, print bytes received.
- **`udp --host H --port P [--send HEX] [--expect-regex RE]`** — send one
  datagram and assert the reply. The default scenario is a DNS query to a public
  resolver, which covers both the UDP forwarder and DNS.
- **`dns --name N`** — a convenience over `udp` against a resolver.

Use the existing smoltcp dependency for the tool's TCP client, with bounded
buffers, response sizes and deadlines. Keep deterministic local TCP/UDP fixtures
for automated verification; public endpoints are manual integration checks.
DNS success requires checking the transaction ID, response flags and answer,
not just receiving bytes. Authenticate a received datagram before learning or
updating the peer endpoint, including a fresh authenticated handshake after a
phone restart or Wi-Fi change. A persistent session supports running checks
without restarting the peer. The maintained interactive CLI reports each check's
pass/fail as JSON; command failure does not terminate the persistent process.

### 9.3 Constraints

- **No root, no interface, no routing change.** The binary only opens a normal
  UDP socket and writes packets into the tunnel. Running it must not perturb
  Fedora's network.

---

## 10. Service testability seams

The goal is that every stage in §11 is a *command*, not a code edit, and that
the platform logic is unit-testable without a device.

### 10.1 `BridgeController` (extracted orchestration)

Today the start sequence — "register network callbacks, find cellular and
Wi-Fi, bind the process to cellular, bind a Wi-Fi socket, start Rust, and undo
all of it on stop" — has no home; it will otherwise be buried in
`BridgeService`. Extract it into a plain Kotlin class:

```
class BridgeController(
    private val binder: NetworkBinder,
    private val bridge: BridgeFacade,   // start/stop bridge, Rust-backed
    private val config: () -> BridgeConfig?,
)
```

- `start()` performs the sequence and returns a typed result describing each
  binding outcome (cellular found? Wi-Fi found? fd obtained? bridge started?),
  rather than throwing on the first miss.
- `stop()` reverses it.
- `BridgeService`, the boot receiver, and tests all call `BridgeController`.
  The boot receiver starts the service, not a separate controller instance.
  `BridgeFacade` is a thin interface over a dedicated Rust JNI entry point so
  a Robolectric test uses a fake and never loads Rust. The existing FRB bindings
  are Dart bindings, not Kotlin bindings.

### 10.2 The fd handoff (made explicit)

Kotlin creates the Wi-Fi socket and passes its detached fd directly to Rust
through JNI. Rust owns the bridge runtime and all forwarding state. The native
service owns lifecycle even when no Flutter engine or Activity exists. Flutter
uses a platform channel to request start/stop and read status, stats and
diagnostics; desktop retains its existing FRB path. Both native entry points
delegate to the same production Rust core and process-global lifecycle.

Define fd ownership on every error path: Kotlin owns an untransferred fd;
Rust owns it from JNI handoff onward, including failures during validation or
startup. Stop waits for Rust teardown before releasing process binding and
network requests. Check bindProcessToNetwork's Boolean result. Missing cellular
must never fall back to Wi-Fi egress. Network loss/replacement tears down old
flows and rebinds before restarting. Keep Android operations injectable for tests.

The service includes notification, required manifest declarations, boot receiver,
and configuration loading now. UI recreation must attach to existing state
without launching another bridge. Production lifecycle is tested through the
service in stages 2–4; it is not deferred to a UI-only harness.

### 10.3 Diagnostics surface

Add a bounded, in-memory event log (ring buffer, newest last) fed by the
controller and the Rust bridge:

- Controller events: cellular/Wi-Fi availability, bind results, fd obtained.
- Bridge events: state transitions and the last error, from the existing
  `BridgeStatus`.

Expose it two ways: Android `logcat` (tagged `CellBridge`), and a read-only
accessor the UI can render (a text panel on the config/diagnostics screen is
enough). This is what makes stage 1 — "did the tunnel come up?" — legible
instead of a silent `Connecting`.

### 10.4 No new config seam

Pointing the app at the stand-in router needs **no new code**: the config screen
already accepts private key, address, peer public key, endpoint and MTU, and
`SettingsStore`/`ConfigStore` already persist it. The test sets
`endpoint = <fedora-LAN-ip>:51820`, `peerPublicKey = <stand-in-router pubkey>`,
`address = 10.10.10.2/24`.

---

## 11. Staged procedure

Each stage has a single success signal. A stage is not started until the
previous one passes.

### Stage 0 — Rust core, host loopback (existing, no hardware)

`cargo test` in `rust/`. Confirms the harness-based loopback still passes.

### Stage 1 — Production loop vs the stand-in router (host only, no phone)

Run the **real** `bridge::start` on Fedora with `wg_socket_fd = None`, endpoint
pointed at the stand-in router running on `127.0.0.1` (or the LAN). The
stand-in router runs `tcp` and `udp` scenarios; the bridge's own sockets egress
Fedora's real internet.

- Success: the `tcp` and `udp` scenarios report the expected replies.
- This is the highest-value pre-device check: it validates the **production
  loop**, not just the harness, and isolates Rust-core bugs from Android bugs.
- Prerequisite: Fedora has normal internet (before the phone changes state).
- Launch the bridge using the maintained host bridge CLI (§8), not a source edit.
  Automated coverage uses local deterministic fixtures and the same production
  bridge lifecycle; public-internet checks are explicitly separate.

### Stage 2 — On-device handshake

The phone is a Wi-Fi client of the Flint 3; stand-in router runs on Fedora's LAN
address; app configured per §10.4; connect.

- Success: `standin_router handshake` exits 0 **and** the app shows `Connected`.
- Proves V2: cellular request, process binding, Wi-Fi socket binding, handshake.

### Stage 3 — On-device egress round trip

Stand-in router runs `tcp` (a fetch) and `udp`/`dns`.

- Success: both scenarios pass, with replies coming from the public internet.
- Verify cellular transport selection and successful binding in diagnostics;
  record the tunnel public-IP result alongside a cellular-bound control request.
  Flint 3's internet status does not gate this stage (§7).

### Stage 4 — LAN retention

While stage 3 traffic is flowing, from the phone confirm it still reaches the
Flint 3 (`192.168.8.1`) and a LAN host.

- Success: LAN reachability holds throughout the egress test (V3, retention
  half). This is the property the whole project exists for.

### Stage 5 — Later hardening (plan Task 15, unchanged)

Throughput, fail-closed behaviour, unattended boot, soak. These follow the
router cutover (Task 14) and are out of scope here.

### Operating note — staying online

Fedora's internet during stages 2–5 is the phone's **USB tether**. Enable and
verify the tether **before** the phone leaves hotspot mode. USB tethering uses
no Wi-Fi radio, so it coexists with the phone being a Wi-Fi client. Caveat to
verify on the Pixel: with the Flint Wi-Fi carrying no internet, Android must
fall back to cellular as the tether uplink; this is expected but unconfirmed.

---

## 12. Success criteria (summary)

| Stage | Signal | Proves |
|---|---|---|
| 0 | `cargo test` passes | Core data path |
| 1 | Stand-in router `tcp`+`udp` pass against real `bridge::start` on host | Production loop, TCP+UDP egress |
| 2 | `handshake` exits 0 and app shows `Connected` | Android binding + tunnel |
| 3 | Stand-in router `tcp`+`udp` pass on-device | Cellular egress, full path |
| 4 | LAN host reachable during stage 3 | Wi-Fi client retention |

---

## 13. Calls I made (check these)

- **Stand-in router is a repo Rust binary**, not kernel WireGuard on Fedora.
  Rejected the kernel option because it needs `sudo`, changes host interfaces,
  and is harder to script/inspect; the simulated client is an acceptable
  fidelity gap (§7). *Revisit if stage 3 passes suspiciously easily and we want
  a real kernel stack on the Fedora leg.*
- **Binary location/flag details** (`bin/` vs `examples/`, exact flags) are left
  to the builder; use smoltcp for client TCP rather than promoting the minimal
  echo-test client into a general-purpose TCP implementation.
- **USB tether** as Fedora's internet during the test, accepted as the way to
  keep the machine (and the LLM session) online.
- **Diagnostics is a log + optional panel**, not a full screen.
- Stage 1 runs the **real** `bridge::start`, not the test harness's `step`.

## 14. Left to the builder

- Exact `BridgeController` API and UI channel shapes; fd handoff stays native
  Kotlin → Rust, not through Flutter.
- Whether the diagnostics ring buffer lives in Rust, Kotlin, or both.
- The userspace client module boundary and host launcher CLI ergonomics.
- Stand-in router CLI ergonomics and log format.
