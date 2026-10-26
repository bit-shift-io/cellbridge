# Multi-phone Priority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support up to eight independent phone tunnels with strict preference and zero-cellular-data idle backup checks.

**Architecture:** Keep the three deployable POSIX shell scripts. The monitor exposes a validated `phones` command for setup/listing, maintains per-phone state in its private run directory, and isolates HTTPS traffic in per-phone probe tables. Setup generalizes its existing key/snapshot handling; revert restores the immutable originals or deletes explicitly owned artifacts.

**Tech Stack:** POSIX sh/BusyBox ash, OpenWrt UCI/procd, iproute2, WireGuard, Python unittest, rootless Linux network namespaces.

**Spec:** `docs/superpowers/specs/2026-10-26-multi-phone-priority-design.md`

## Global Constraints

- Up to 8 phones, indices 0 through 7; interface `wgN`, source `10.10.(10+N).1`, phone `10.10.(10+N).2/24`.
- UDP default `51820 + 10*N`; probe table `51821 + 10*N`; default priority `10*(N+1)`; unique priorities.
- Shared active table `51820`, exact source rules priority `0`, connected/main rule `100`, active rule `101`; protocol `186`.
- Wi-Fi-only idle checks every 25 seconds; HTTPS only for selected/preferred phones and fallback qualification.
- Two good health rounds qualify, three failed rounds withdraw; sleep two seconds between rounds; HTTPS max time three seconds per target.
- No live-router operations, app changes, dependencies, commits, or credential capture during implementation.
- Preserve existing keys, custom ports, priority/labels, original immutable snapshots, unrelated routing/configuration.

## Review Focus

- A failed higher-priority phone must not prevent lower candidates from qualifying, nor block its own later cellular recovery; cover in monitor scenarios.
- Stale idle counters and sent keepalives do not prove internet health; cover no-idle-HTTPS and fresh qualification scenarios.
- Foreign entries anywhere in the reserved allocations must stop mutation, even with missing/malformed phone configuration; cover ownership/validation tests.
- A manual-key legacy install must retain credentials on a normal rerun and be explicitly marked during migration; cover migration/key tests.
- Snapshot restore and no-snapshot cleanup must preserve unrelated interfaces and shared firewall members across repeated revert; cover multi-phone recovery tests.

---

### Task 1: Multi-phone monitor and configuration contract

**Files:** Modify `router/cellbridge-failover.sh`, `router/tests/test_failover.py`.

**Interfaces:** `load_phones()` validates `/etc/cellbridge-phones.conf` (test override `CELLBRIDGE_PHONES_FILE`) and emits canonical six-field records via `phones`; absent file emits implicit wg0. `check`/`cleanup` inspect all eight fixed allocations independently of that file. `run` consumes canonical records; status preserves `path`, `successes`, `failures` and adds `selected` plus each phone's priority, health/counters and Wi-Fi observations.

- [ ] Add scenarios asserting wg0 preference, wg1 fallback only after fresh two-round qualification, wg0 cellular recovery without Wi-Fi reconnection, no idle wg1 HTTPS, and guard/rule creation for both phones.
- [ ] Add validation/ownership cases asserting rejection of duplicate fields, wrong index/address/table mappings, empty/malformed files, foreign reserved routes/rules, and inspection failures without deletion.
- [ ] Run `python -m unittest discover -s router/tests -p test_failover.py -v` and observe new failures.
- [ ] Implement strict parser, allocation-aware routing ownership, exact cleanup, per-phone qualification/counters, parallel scheduled HTTPS checks, periodic local WireGuard observations, atomic status and child-process shutdown.
- [ ] Run the monitor tests to PASS, including existing single-phone regressions.

### Task 2: Setup CLI, allocation and upgrade

**Files:** Modify `router/cellbridge-setup.sh`, `router/tests/test_install.py`.

**Interfaces:** Setup consumes monitor `phones` and `check`; writes root-only atomic six-field config; per-target `WG_IF`, addresses, port, priority/label and dynamic peer type replace wg0-specific selectors. `--list-phones` is read-only; `--add-phone` and `--phone` are exclusive. UCI marker `cellbridge_managed=1`, router keepalive 25, generated/private metadata remain owner-only.

- [ ] Add sandbox cases asserting independent wg0/wg1 keys/QR/subnets/ports, preserve wg0 byte-for-byte on add, rerun priority/label/key reuse, list read-only, custom-port collision refusal, cap/stale/foreign interface refusal, config/UCI disagreement, manual-key legacy migration.
- [ ] Run `python -m unittest discover -s router/tests -p test_install.py -v` and observe new failures.
- [ ] Generalize CLI/key/peer/readiness functions and implement preflight allocation, proven ownership, subnet/port uniqueness, existing metadata preservation and atomic config writing.
- [ ] Keep global configuration/reloads out of add-phone and metadata-only updates; install/update monitor while preserving immutable backups. Preserve generated or manual phone keys on rerun.
- [ ] Run installation and monitor tests to PASS.

### Task 3: Multi-phone recovery and ownership

**Files:** Modify `router/cellbridge-revert.sh`, `router/tests/test_install.py`.

**Interfaces:** Revert uses monitor's full-range routing cleanup before removing the phone file; enumerates owned interfaces before restore, removes leftover kernel links only when absent in originals; no-snapshot deletion requires markers or proven legacy wg0. Shared firewall objects use their own ownership markers.

- [ ] Add tests asserting exact original restore after two phones, repeated revert, owned-only no-snapshot cleanup preserving a foreign wg2 and shared zone/forwarding, custom-port manual legacy ownership and damaged snapshot refusal.
- [ ] Run installation tests and observe new failures.
- [ ] Implement enumeration, exact snapshot restore of configuration-file presence, and guarded targeted cleanup. Preserve files/snapshots on failure and remove phone configuration only after cleanup.
- [ ] Run installation and monitor tests to PASS.

### Task 4: Real routing verification and operator documentation

**Files:** Modify `router/tests/test_kernel_routing.py`, `router/README.md`, `cellbridge_router_setup_guide.md`, `ARCHITECTURE.md` and this plan's progress.

**Interfaces:** Namespace tests use dummy wg0/wg1 and local stub HTTPS, never public internet. Operator docs deploy the same three scripts and show `--add-phone`, `--phone`, `--priority`, `--label`, `--list-phones`, monitor status and both probe tables.

- [ ] Add a namespace scenario asserting wg0 → wg1 → wg0 selection, isolated source-specific probes, LAN endpoint preservation and exact full-range cleanup.
- [ ] Run the namespace tests; implement any routing fixes their failures expose.
- [ ] Document limits, idle Wi-Fi versus cellular health, fresh candidate qualification delay, preferred-phone recovery checks, collision/repair behavior and user-run hardware checks.
- [ ] Run `python -m unittest discover -s router/tests -v`, `sh -n` for all three scripts and `git diff --check`; review changed code against the spec and fix findings.
- [ ] Mark completed tasks and report test evidence and any unavailable checks.

## Execution

The user explicitly requested both planning and building in this session. Execute inline, preserving the pre-existing theme/tile changes; no commits unless requested. Plan self-review: all design sections map to the four tasks, and all five review-focus conditions have owning tests above.
