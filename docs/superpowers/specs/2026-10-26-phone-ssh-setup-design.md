# Phone-driven router setup

Status: user requested direct implementation after requirements discussion.

## Requirements

- Each Android phone can install the bundled router scripts over SSH and register itself without a PC or QR scan.
- Ask for router address/SSH port, username/password and phone priority 1–10 (1 highest). Login details are never persisted; keep only in memory while needed, then discard.
- Show live script feedback and useful copyable error output, excluding passwords, WireGuard keys and machine-readable import payloads.
- Assume router internet is available. An initial bounded check runs before setup mutations. If unavailable, notify the user and wait for Retry; retry rechecks before continuing.
- Preserve other phones, existing backups and the manual/QR setup path.

## Implementation calls

- Per user direction, SSH and provisioning orchestration live in the Rust core (`russh` with ring crypto); Flutter is the skin. Android hands Rust an independently owned, connected Wi-Fi TCP descriptor under the existing process-wide bridge/test guard. No process-wide network rebinding. Setup requires the bridge to be disconnected.
- Embed the actual scripts from `router/` with Rust `include_str!`, not duplicated source files or Flutter assets. Upload through SSH stdin (Dropbear need not provide SFTP), into a unique private staging directory. SSH credentials are used only for authentication; the router scripts never receive phone private keys.
- Generate/preserve a phone identity locally in Android encrypted preferences before remote mutation. Prefer an existing configured phone's key. Send only its public key; router registration looks up that key before allocating a new index. A router-wide lock serializes registration, and partial-install retry reuses its existing interface/key allocation.
- Add app-only script options for registration by public key, machine-readable output and a standalone internet check. Existing CLI priorities remain compatible; app registration accepts 1–10 and explains occupied priorities. Existing out-of-range CLI priorities are not silently changed.
- Script internet check exits with a dedicated retryable code when offline. App waits without a running mutating command; Retry reruns the initial check. Connection loss requires login again if session ended; identity remains so retry does not duplicate registration.
- SSH host key is verified before sending the password: an unpinned handshake returns a fingerprint without authenticating, Flutter confirms and pins it in encrypted storage, then opens a new Wi-Fi connection. Changed keys fail clearly. Store no SSH username/password. Router hostname is only session input; encrypted host pins are not login credentials.
- Capture stdout/stderr independently, parse the framed result only from stdout, require successful exit and matching phone public key, then construct/validate/save through the existing settings store. Keep diagnostics and result separate. No automatic bridge start; successful setup saves settings and returns to Configuration.
- Initial router internet probe uses existing curl or BusyBox wget with bounded numeric HTTPS requests; missing tools / unsupported firmware are actionable errors, not silent success. Root OpenWrt with enabled kmwan remains the supported router target.
- No automatic rollback on SSH loss. Preserve original snapshots; report that setup may have progressed and that Retry reuses this phone. Cancellation closes SSH and clears credentials; user may inspect/revert from a PC.

## Checks

Router sandbox tests: public-key idempotency, priority conflict/range, clean machine result without secret logs, partial retry, concurrent lock, initial offline check without mutations. Rust tests exercise a real local SSH server, pre-auth host-key verification, script uploads, framing/redaction and offline pause. Flutter tests cover live UI, error output, retry and direct import. Android tests cover exclusive ownership and the encrypted persistence allowlist. Run full Flutter/router/Rust/native suites, analyze, release APK build and shell syntax checks. The user authorized the connected Android phone for local testing; live-router provisioning remains user-run.
