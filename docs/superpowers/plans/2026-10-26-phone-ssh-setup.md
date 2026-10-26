# Phone SSH Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Track steps with checkboxes.

**Goal:** Configure each phone and a fresh router directly from the app using temporary SSH credentials and visible script output.

**Architecture:** Extend existing router setup with idempotent public-key registration and machine results. Rust runs russh, embeds the real scripts, sanitizes progress and validates results. Android provides a connected Wi-Fi TCP descriptor and encrypted identity/host pins; Flutter renders polled progress and imports settings.

**Tech Stack:** Flutter/Dart, Rust/russh/Tokio/ring, Android Kotlin/EncryptedSharedPreferences/JNI, OpenWrt POSIX sh.

**Spec:** `docs/superpowers/specs/2026-10-26-phone-ssh-setup-design.md`.

## Global Constraints

- SSH username/password never persisted; errors/logs exclude login and tunnel secrets.
- App priority 1–10, 1 highest; existing router CLI priorities preserved.
- No duplicate phone on retry; private phone identity persists encrypted before remote mutation.
- All four bundled scripts are sourced directly from `router/` via Rust `include_str!`; no real-router changes during local verification.
- Working router internet is assumed; offline state waits for explicit Retry before mutations.
- Build on current main as user requested; no commits unless requested.

## Review Focus

- Output framing across arbitrary chunks cannot expose keys or accept an unsuccessful result.
- SSH loss after registration must not cause duplicate allocation on retry.
- Host-key confirmation precedes authentication; no silent host-key changes.
- Cancel/dispose and authentication failure must release Wi-Fi binding and credentials.
- Existing manual/QR imports and router recovery remain operational.

### Task 1: Router provisioning contract

Files: `router/cellbridge-setup.sh`, new `router/cellbridge-provision.sh`, `router/tests/test_install.py`.

Interface: provision wrapper `check-internet` returns 75 if offline; `register PUBLIC_KEY PRIORITY` runs serialized public-key matching and calls setup with app output. Setup app output emits a single `CELLBRIDGE1:` payload on stdout only after success, with no private phone key; diagnostic output excludes secrets.

- [ ] Add failing sandbox tests for registration/retry, priority conflicts, secret-free output and internet pause.
- [ ] Implement wrapper locking/preflight and setup registration/structured output options while preserving existing CLI defaults.
- [ ] Run sandbox tests and shell syntax checks to pass.

### Task 2: Rust provisioning core and Wi-Fi/identity support

Files: new `rust/src/provisioning.rs`, Rust JNI/native entries and Cargo dependencies, Android `RouterSetupChannel.kt`, `NativeBridge.kt`, native channel registration and native tests.

Interface: Rust `Request`, `start`, `snapshot_json`, `cancel` own the one-shot async SSH job. Snapshot has state, bounded sanitized logs, fingerprint and validated result. Android exclusively leases bridge/test ownership, transfers the Wi-Fi socket, and exposes identity/pin storage only. No login credentials in state serialization.

- [ ] Add failing tests for framing, identity reuse, failed exits, offline retry, host mismatch and lease release.
- [ ] Add russh dependency and embedded scripts; implement real SSH stdin uploads and streaming commands with no PTY/SFTP requirement.
- [ ] Implement native ownership lease and encrypted metadata allowlist; test both.
- [ ] Run targeted tests and analyze to pass.

### Task 3: App integration and end-to-end verification

Files: new `lib/src/router_setup_api.dart`, `lib/src/router_setup_screen.dart`, `lib/src/config_screen.dart`, `lib/main.dart`, Flutter widget tests, router README and setup guide.

Interface: Setup router button opens login/priority form and live output; success validates/saves matching phone settings, returns config to parent without auto-starting.

- [ ] Add failing UI tests for form validation, live output, waiting/retry, error feedback and imported settings.
- [ ] Implement screen/lifecycle and Configuration entry point; preserve manual/QR flow.
- [ ] Update docs, run full Flutter/router/Android tests, analyze and release APK build.
- [ ] Request one fresh read-only feature review, fix verified findings, record final evidence.

User opted out of further design questions and requested implementation. Execute inline without another approval pause.

## Additional UI changes

- App launcher icon now uses `assets/app_icon.svg`: legacy `ic_launcher.png` and
  `ic_launcher_round.png` (rendered with librsvg — Inkscape drops the gradient
  circle), an adaptive `mipmap-anydpi-v26/ic_launcher(.round).xml`, and
  `ic_launcher_foreground.png` (glyph only) with `ic_launcher_background` colour.
  Verified on the connected phone's app drawer.
- Ongoing notification title is now the status itself ("Connected" /
  "Disconnected"); the old "CellBridge" title and the content-text subtitle are
  gone. `notificationTitle()` is unit-tested.

## Final verification

- `cargo test`: Rust core incl. real local russh-server tests (`rust/src/provisioning.rs`) — pass.
- Real-router read-only interop (russh ↔ Dropbear, fingerprint, password auth, exec) — pass. No router configuration was changed.
- On connected Pixel 5a: app reached SSH connect → host-key confirmation dialog → authentication → script upload → internet check → "Waiting for router internet" (Android socket handoff, channel and UI states).
- `flutter test` 73 pass, `flutter analyze` clean; Android `:app:testDebugUnitTest` pass; ARM64 release APK builds.
- Router `python -m unittest` 76/76, `sh -n` all scripts, `git diff --check` clean.

## Architectural amendment

User explicitly directed Rust for the core, Flutter for the skin. Removed the unfinished Dart SSH controller/dependency and implemented SSH, upload/orchestration, result framing and log redaction in Rust. JNI follows the existing headless Android bridge pattern. Android does not change global process binding for provisioning.
