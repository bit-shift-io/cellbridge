# Router failover implementation plan

> **For agentic workers:** Use superpowers:executing-plans inline. No router changes during implementation.

**Goal:** Prefer healthy tunnel internet with USB then WAN fallback.

**Architecture:** Keep kmwan managing physical uplinks; add a supervised HTTPS
tunnel monitor and independent probe/preferred routing tables.

**Tech stack:** POSIX sh, OpenWrt procd/UCI, ip-full, curl; Python unittest.

**Spec:** `docs/superpowers/specs/2026-10-25-router-failover-design.md`

## Global constraints

- Live-router inspection only; the user performs installation.
- No credentials, device dumps or site-specific router details in git.
- Preserve kmwan and keep physical WAN uplinks enabled.
- Do not commit changes unless requested.

## Review focus

- Probes cannot accidentally succeed via the fallback path.
- Recovery must work with the tunnel's preferred route withdrawn.
- A brief failure must not flap routes; service stop must restore backup routing.
- Failed route operations must not be reported as successful tunnel selection.
- Revert must restore kmwan configuration and clean up partial installations.

## Task 1: Tunnel health monitor

Files: `router/cellbridge-failover.sh`, `router/tests/test_failover.py`.
Interface: monitor `run` and `cleanup` commands; tables 51820/51821 and rules
priority 0 (exact tunnel-source probe), 100/101 (connected routes/tunnel).

- [x] Write subprocess tests for failover/recovery, transient failures, isolated
  probes, secondary-target success, route failure and shutdown cleanup.
- [x] Run `python -m unittest discover -s router/tests -v`; observe missing monitor.
- [x] Implement the monitor; run the tests to green.

## Task 2: Setup/revert integration

Files: setup/revert scripts and tests. Consume the monitor from Task 1, copying
it from beside setup into `/usr/sbin/cellbridge-failover`; install procd init.

- [x] Add executable sandbox checks for installation preflight and revert cleanup.
- [x] Install dependencies before changes, snapshot optional kmwan configuration,
  preserve WAN, adjust USB/WAN priorities, disable automatic WireGuard routes,
  install/start monitor, and remove it on revert.
- [x] Check all scripts with `sh -n` and shellcheck when available; run unit tests.

## Task 3: Documentation and final verification

- [x] Update router instructions, setup guide and project summary for new behaviour.
- [x] Review diff, run all router checks, and check no private inspection material
  entered the working tree. Report that device cutover remains untested.

## Verification and review

`python -m unittest discover -s router/tests -v`: 24 passed, including two
real Linux routing tests in rootless network namespaces. Shell syntax checks
and `git diff --check` passed. Shellcheck was not installed.

Independent review identified snapshot-completeness/provenance, partial-helper,
routing-ownership, reload-failure and original-service-state issues. Regression
tests reproduced them before fixes; the final suite covers their resolution.
Snapshot format 2 requires complete metadata, original enabled/running state and
atomic payload publication. Incomplete/damaged snapshots require explicit repair.
Owned routes/rules use protocol 186 and are validated on every installation and
cleanup. A restore receipt protects restored older monitors on repeated revert.

The namespace guard test accounts for Linux synthesizing an on-link route when
an output device is explicitly bound: that route must stay on wg0, whereas an
unbound lookup with the probe source must hit the unreachable guard. Both are
checked against the real kernel, including an earlier vendor local-output rule.

No live-router changes, uploads or cutover testing were performed. Firmware
kmwan hotplug behaviour, on-device procd timing, boot and actual internet cutover
remain user-run verification. Work is left uncommitted as requested by default.
