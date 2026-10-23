# CellBridge Task Tracker & Roadmap

Full detail — files, interfaces, test assertions, commits — lives in the plan:
[`docs/superpowers/plans/2026-10-20-cellbridge.md`](docs/superpowers/plans/2026-10-20-cellbridge.md).
This file tracks progress only.

## Phase A — Rust core (host-testable)

- [ ] Task 1: Scaffold Flutter + Rust + FRB round-trip
- [ ] Task 2: `Tunnel` — boringtun wrapper
- [ ] Task 3: `QueueDevice` — smoltcp device over queues
- [ ] Task 4: `Gateway` — smoltcp TCP listener in any-IP mode
- [ ] Task 5: TCP splice engine
- [ ] Task 6: UDP packet-level forwarder
- [ ] Task 7: Full loopback integration
- [ ] Task 8: Stats, status, and teardown on tunnel loss

## Phase B — Android platform layer

- [ ] Task 9: Kotlin `NetworkBinder`
- [x] Task 10: `BridgeService` + boot receiver (build/unit verified; device pending)
- [x] Task 11: Native Rust lifecycle wiring via JNI; Flutter service controls

## Phase C — Flutter UI

- [ ] Task 12: Dashboard
- [ ] Task 13: Config screen + settings store

## Phase D — Router

- [ ] Task 14: Router setup + revert scripts

## Phase E — Verification

- [x] Maintained Fedora stand-in router and host production-loop checks
- [ ] Physical-device pre-router verification — see `docs/testing/pre-router-verification.md`

- [ ] Task 15: On-device end-to-end, throughput, boot, soak
