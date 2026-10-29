# Bridge speed readout — design

**Date:** 2026-10-29
**Status:** approved and implemented

## Intent

Surface how fast the bridge is moving internet traffic — down and up — in
megabits per second, in two places:

1. The ongoing notification's subtitle, as a bare `40-30` (40 down / 30 up),
   shown only while connected.
2. The dashboard, as two new rows, plus a rename of the existing byte totals.

The Rust core owns the numbers; Kotlin and Flutter are skins that format them.
This is deliberate: the GUI may be replaced, and the calculation belongs with
the counters that produce it.

## Goals

- A live down/up speed, in Mbps, for the traffic the bridge forwards.
- One definition of "speed", in Rust, read by both the notification and the
  dashboard.
- The rename: the byte totals become `Download` (received, internet → LAN) and
  `Upload` (sent, LAN → internet). Speeds use `Down` and `Up`.

## Non-goals

- The phone's **total** cellular usage. This is bridge-forwarded traffic only —
  what crosses the tunnel — not whatever the phone itself browses.
- History, graphs, peaks, or averages.
- Any change to the forwarding path, WireGuard, or the tunnel.

## What exists today

- `rust/src/stats.rs` — `StatsCounters` holds two lock-free `AtomicU64`
  payload counters and produces a `Stats { tx_bytes, rx_bytes, active_flows }`
  on read. `tx` is uplink (LAN → internet), `rx` is downlink (internet → LAN).
- `rust/src/native.rs` — `snapshot_json()` turns `bridge::stats()` into the JSON
  the Android service caches. Keys are strings.
- `rust/src/api.rs` — `stats()` exposes `Stats` over flutter_rust_bridge (2.13).
- `android/.../BridgeService.kt` — ticks once a second, calls
  `controller.refresh()`, then `publish()`, which rebuilds the notification.
  The title is the status word; there is deliberately no subtitle today.
- `lib/src/dashboard.dart` — reads `Stats` on a 500 ms poll and renders `In`,
  `Out`, `Active flows`.
- `lib/src/format.dart` — `formatBytes`, the existing 1024-based formatter.

## Approaches

**A. Compute the speed in each skin.** The notification and the dashboard each
keep their own last reading and derive delta-bytes ÷ delta-time.
*Rejected:* the user's goal is a Rust core with replaceable skins, so the
definition must not live in the skins.

**B. Rust owns the rate (chosen).** `StatsCounters` also tracks a smoothed byte
rate, exposed on `Stats` and in the snapshot JSON. Both skins just read and
format it.

## Design

### Rust: rate on `StatsCounters`

`Stats` gains two fields, in bytes per second:

- `tx_bytes_per_sec` — uplink rate
- `rx_bytes_per_sec` — downlink rate

They stay `u64` so `Stats` keeps `Copy`/`Eq`/`Default`.

`StatsCounters` gains a `Mutex<RateState>` touched **only** by `snapshot()` and
`reset()`; the data path (`add_tx`/`add_rx`) stays lock-free. `RateState` holds
the previous sample (instant + byte counts) and the two smoothed rates (f64).

On `snapshot()`:

```
dt = now - last_instant
inst = (bytes_now - bytes_last) / dt
alpha = 1 - exp(-dt / TAU)          // time-based EMA, cadence-independent
rate += (inst - rate) * alpha
return rates rounded to u64
```

`TAU` is a little over a second (start at 2 s): smooth enough that a 1 s sample
does not flicker, responsive enough to feel live. `reset()` clears the rate
state along with the byte counters, so a fresh run starts at zero.

The cadence differs per platform — the Android service samples ~1 s via
`snapshot_json()`, the desktop dashboard ~500 ms via `stats()` — and there is
only one caller per platform, so the EMA window is well-defined on each.

### Rust: expose it

- `native.rs::snapshot_json()` adds `"txBytesPerSec"` / `"rxBytesPerSec"`
  (strings, matching the existing keys).
- `Stats` crosses FRB unchanged in shape; bindings are regenerated with
  `flutter_rust_bridge_codegen generate` (2.13), producing Dart
  `txBytesPerSec` / `rxBytesPerSec`.

### Formatting (skins only)

Rust returns a unit-neutral number; each skin formats it identically:

- `mbps = bytesPerSec × 8 ÷ 1_000_000` (decimal megabits, the networking unit).
- `≥ 10` → whole number; `< 10` → one decimal with a trailing `.0` dropped —
  mirroring `formatBytes`' existing convention.
- Exact decimal halves round upward in both skins, without binary floating-point
  ambiguity. Dart uses integer arithmetic; Kotlin uses decimal arithmetic.
- Notification subtitle: `${down}-${up}` (rx rate, then tx rate), e.g. `40-30`;
  `null` unless the state is `connected`.
- Dashboard rows: `Down` and `Up` render `40 Mbps` / `30 Mbps`.

### Skins

**Notification (`BridgeService.kt`).** `notification(status)` becomes
`notification(snapshot)`; a pure `notificationSubtitle(snapshot)` returns the
`40-30` string or `null`. The title is unchanged. The `onCreate` pre-start
notification passes no snapshot and so has no subtitle.

**Dashboard (`dashboard.dart`).** Five rows, in order:

| Label | Value |
|-------|-------|
| Download | `formatBytes(rxBytes)` |
| Upload | `formatBytes(txBytes)` |
| Down | `formatMbps(rxBytesPerSec) Mbps` |
| Up | `formatMbps(txBytesPerSec) Mbps` |
| Active flows | count |

`format.dart` gains `formatMbps(BigInt bytesPerSec)`. `AndroidBridgeApi.stats()`
reads the two new snapshot keys (defaulting to 0 when absent).

## Calls I made

- **Mbps is decimal (10⁶).** The standard meaning of "megabits per second".
- **Sub-10 Mbps shows one decimal** (`3.4`), matching `formatBytes`, so real but
  slow traffic does not read as `0`.
- **`down-up` order** in the notification, and `Down` above `Up` on the
  dashboard (rx then tx), matching the `40-30` example.
- **Speeds shown whenever the app is open**, including `0 Mbps` while idle —
  only the notification hides its subtitle when disconnected.
- **TAU = 2 s** for the smoothing window.
- **Test keys renamed** with the labels: `bytes-in`/`bytes-out` become
  `bytes-download`/`bytes-upload`; speeds use `speed-down`/`speed-up`.

## Affected files

- `rust/src/stats.rs`, `rust/src/native.rs`, `rust/src/api.rs` (docs)
- `rust/src/frb_generated.rs` and `lib/src/rust/**` (regenerated)
- `rust/src/bridge.rs` (tests constructing `Stats`)
- `android/.../BridgeService.kt`
- `lib/src/dashboard.dart`, `lib/src/format.dart`, `lib/src/android_bridge.dart`,
  `lib/src/demo_bridge.dart`
- Tests: `rust` unit tests, `BridgeServiceTest.kt`, `test/dashboard_test.dart`,
  `test/format_test.dart`, `test/android_bridge_test.dart`, and any other
  `Stats(...)` constructor in tests.

## Verification

- Rust: unit tests for the EMA (zero on first sample, settles toward a steady
  rate, clears on reset).
- Kotlin: `notificationSubtitle` is the `40-30` pair only when connected;
  `formatMbps` rounding.
- Flutter: `formatMbps` cases; dashboard renders the five rows and the renamed
  keys.
- `flutter_rust_bridge_codegen generate`, `cargo test`, `flutter test`, and the
  Android unit tests.
