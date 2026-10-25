# Generated phone keys in setup QR

> **For agentic workers:** Use superpowers:executing-plans inline; test changes before implementation. No live-router access or changes.

**Goal:** Run router setup without copying a phone public key, then scan its QR;
ask Yes/No on the phone before replacing a different existing phone key pair.

**Architecture:** Default setup generates/reuses a phone WireGuard key pair.
Store its private key as a CellBridge metadata option on the existing peer in
root-only `/etc/config/network`, which is already covered by snapshot/revert.
Keep `--phone-public-key` as the existing phone-managed-key option. Extend the
current QR format with optional `phonePrivateKey`; validate the private/public
pair through the existing key API before importing anything. No cancels the
entire import. Yes changes only the form; existing Save persists it securely.

**Tech stack:** POSIX sh, OpenWrt UCI/WireGuard tools, Flutter/Dart, existing
Python router sandbox tests and Flutter widget/parser tests.

**Spec:** Approved chat requirements: script generates phone keys; phone asks
Yes/No to overwrite existing keys. Re-runs reuse keys; existing backup/revert and
router inspection-only constraints remain binding.

## Global constraints

- Do not access or mutate the live router, upload scripts, or install packages there.
- Do not store real credentials, device keys or router details in git.
- Generate independent router and phone keys; never place private keys in process arguments.
- Include the phone private key only in the deliberate QR/QR-payload output, not diagnostic messages.
- Existing public-only QR codes and explicit phone-public-key setup remain supported.
- No import (including keys) is persisted before Save; No/dismissal leaves all fields alone.

## Review focus

- Default re-runs retain the same private/public pair and original backups.
- A corrupt stored generated key aborts rather than silently rotating credentials.
- No, dismiss, invalid private key or mismatched pair leaves every field/store unchanged.
- New phone and identical-key scans do not ask to overwrite unnecessarily.
- Revert removes newly generated metadata or restores previously existing metadata.

## Task 1: Regression tests

Files: `test/setup_qr_test.dart`, `test/config_screen_test.dart`,
`router/tests/test_install.py`.

- [x] Test private-key parsing/type/value errors, public-key presence and legacy compatibility.
- [x] Test fresh import, Yes/No/dismiss/same-key cases, invalid pair and Save persistence.
- [x] Test default generation/QR pairing, re-runs, protected key storage, manual mode and revert.
- [x] Run new tests and observe failures for the missing feature.

## Task 2: Implement and document

Files: `lib/src/setup_qr.dart`, `lib/src/config_screen.dart`,
`router/cellbridge-setup.sh`, `router/cellbridge-revert.sh` (recovery documentation),
`router/README.md`, `cellbridge_router_setup_guide.md`.

- [x] Add optional QR phone private key with private/public consistency validation.
- [x] Add atomic, consent-based form import and a Yes/No overwrite dialog.
- [x] Add generated/reused phone key metadata and optional manual-key mode to setup.
- [x] Update recovery notes and setup instructions; run feature tests to green.

## Task 3: Verify and deliver

- [x] Run Flutter tests/analyze, all router tests, shell syntax and diff checks.
- [x] Review changes; fix meaningful findings with regression coverage.
- [x] Build the arm64 release APK and install if a Pixel is connected.
- [x] Report the build/install state and user-run setup command accurately.

## Verification

- Flutter: 71 tests passed; `flutter analyze` reported no issues.
- Router: all 31 tests passed, including existing namespace routing checks.
- Shell syntax and `git diff --check` passed.
- Arm64 release APK built successfully; updated the connected Pixel, launched
  MainActivity, confirmed it resumed and observed no app error-level startup logs.
- No live-router access or changes performed for this feature.

Independent review found Generate/Save could already be awaiting a key operation
when scanning began. Completer-controlled tests reproduced both races before a
fix. Generate, Save and Scan now hold mutually exclusive busy state throughout
their async lifetimes; Save snapshots the validated form before its first await.
Additional checks cover switching generated keys to manual mode and failed QR
rendering with intentional raw-payload output but no diagnostic/argv leaks.
