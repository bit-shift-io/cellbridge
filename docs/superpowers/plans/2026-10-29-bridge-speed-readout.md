# Bridge Speed Readout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the bridge's live down/up internet speed in Mbps — as a `40-30` subtitle on the ongoing notification and as `Down`/`Up` rows on the dashboard — with the rate computed once in Rust.

**Architecture:** `StatsCounters` in the Rust core gains a smoothed, time-based byte rate exposed on `Stats`; the FRB bindings and the Android snapshot JSON carry it; the Kotlin notification and the Flutter dashboard only format it (Mbps = bytes/s × 8 ÷ 10⁶). The byte totals are renamed `Download`/`Upload` on the dashboard.

**Tech Stack:** Rust (boringtun/smoltcp core), flutter_rust_bridge 2.13, Kotlin (Android foreground service), Flutter/Dart.

**Spec:** `docs/superpowers/specs/2026-10-29-bridge-speed-readout-design.md`

## Execution status

- [x] Task 1: Rust EMA and deterministic rate/reset tests.
- [x] Task 2: Native snapshot keys, generated bindings and Dart adapters.
- [x] Task 3: Decimal Mbps formatting.
- [x] Task 4: Five dashboard rows.
- [x] Task 5: Simulated demo rates.
- [x] Task 6: Connected-only notification subtitle.
- [x] Final independent review and verification.

User retains all git operations; commit steps below are not executed.
Bindings and constructor migration moved into Task 1 because generated Rust
constructors otherwise prevented compilation. Android verification uses
`JAVA_HOME=/opt/jbr-21` and `-Dorg.gradle.java.installations.paths=/opt/jbr-21`
because the default Java 25 installation lacks a compiler.

### Review results

Independent review identified a snapshot/reset race, short-screen overflow,
stale notification content on failure, and cross-language decimal tie rounding.
All four were addressed with regression tests observed failing before the fixes.
Snapshot/reset now serialize counters and rate state under the same mutex;
the dashboard scrolls; errors rebuild the notification; both formatters round
exact decimal halves upward. No review findings are deferred.

Final verification: Rust full suite (114 passed, 2 ignored), Flutter (78 passed),
Android unit suite (66 passed), `flutter analyze` (no issues), and debug APK
build succeeded. APK: `build/app/outputs/flutter-apk/app-debug.apk`.

## Global Constraints

- Rate lives in Rust. Skins format only; they never compute a rate.
- Rates are `u64` **bytes per second**. `Stats` keeps `Copy`/`Eq`/`Default`.
- Mbps is **decimal**: `bytesPerSec × 8 ÷ 1_000_000`.
- Formatting: `≥ 10` → whole number; `< 10` → one decimal with a trailing `.0` dropped (this mirrors `formatBytes`).
- Order is always **down then up** (rx then tx).
- The Rust data path (`add_tx`/`add_rx`) stays lock-free; only `snapshot`/`reset` take the rate lock.
- Smoothing is a time-based EMA with `TAU = 2 s` (`alpha = 1 - exp(-dt / TAU)`), so the 500 ms desktop poll and the 1 s Android tick behave the same.
- Test commands: Rust `cargo test --manifest-path rust/Cargo.toml`; Flutter `flutter test`; Kotlin `./gradlew :app:testDebugUnitTest` (from `android/`).

## Review Focus

- **Idle / after stop** — counters and the rate must both read `0`, not a stale peak. (Task 1 reset test; Task 4 rows always render.)
- **Two reads in the same instant** (`dt == 0`) and variable cadence — the pending delta must survive to the next real interval, not be dropped or double-counted. (Task 1 zero-interval test.)
- **Large counters** — u64 rates arrive in Dart as `BigInt`; formatting must not overflow or truncate. (Task 3.)
- **Disconnected / pre-start notification** — no snapshot yet, or state ≠ `connected`: subtitle must be absent and the build must not crash. (Task 6.)
- **Rounding boundaries** — exactly 10 Mbps is `10`, light traffic below 1 Mbps is a decimal like `0.4`, not `0`. (Task 3.)

---

### Task 1: Rust — smoothed byte rate on `StatsCounters`

**Files:**
- Modify: `rust/src/stats.rs`

**Interfaces:**
- Produces: `Stats { tx_bytes: u64, rx_bytes: u64, active_flows: usize, tx_bytes_per_sec: u64, rx_bytes_per_sec: u64 }`
- Produces: `StatsCounters::snapshot(&self, active_flows: usize) -> Stats` (unchanged signature; now fills the two rate fields)
- Produces: `StatsCounters::reset(&self)` (also clears rate state)
- Produces (private, for tests): `StatsCounters::snapshot_at(&self, active_flows: usize, now: Instant) -> Stats`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `rust/src/stats.rs`, and update the existing `a_snapshot_carries_the_live_flow_count` literal to include `tx_bytes_per_sec: 0, rx_bytes_per_sec: 0`.

```rust
#[test]
fn rate_is_zero_on_the_first_sample() {
    let counters = StatsCounters::new();
    counters.add_tx(4000);
    assert_eq!(counters.snapshot_at(0, Instant::now()).tx_bytes_per_sec, 0);
}

#[test]
fn rate_settles_toward_a_steady_flow() {
    let counters = StatsCounters::new();
    let t0 = Instant::now();
    counters.snapshot_at(0, t0); // prime the window
    let mut last = 0;
    for second in 1..=20u64 {
        counters.add_tx(1000); // exactly 1000 B/s
        last = counters.snapshot_at(0, t0 + Duration::from_secs(second)).tx_bytes_per_sec;
    }
    assert!(last <= 1000, "never overshoots the steady rate, got {last}");
    assert!(last > 990, "settles just under 1000 B/s, got {last}");
}

#[test]
fn a_zero_interval_sample_keeps_the_pending_delta() {
    let counters = StatsCounters::new();
    let t0 = Instant::now();
    counters.snapshot_at(0, t0);
    counters.add_tx(2000);
    assert_eq!(counters.snapshot_at(0, t0).tx_bytes_per_sec, 0, "dt == 0 is not a sample");
    // The 2000 bytes are still measured over the full second.
    assert_eq!(counters.snapshot_at(0, t0 + Duration::from_secs(1)).tx_bytes_per_sec, 787);
}

#[test]
fn reset_clears_the_rate() {
    let counters = StatsCounters::new();
    let t0 = Instant::now();
    counters.snapshot_at(0, t0);
    counters.add_tx(2000);
    assert!(counters.snapshot_at(0, t0 + Duration::from_secs(1)).tx_bytes_per_sec > 0);
    counters.reset();
    assert_eq!(counters.snapshot_at(0, t0 + Duration::from_secs(2)), Stats::default());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path rust/Cargo.toml --lib stats`
Expected: FAIL — `StatsCounters` has no `snapshot_at`, and `Stats` has no rate fields.

- [ ] **Step 3: Implement the rate**

In `rust/src/stats.rs`: add `tx_bytes_per_sec`/`rx_bytes_per_sec` to `Stats` (documented as bytes per second, uplink/downlink), add `use std::sync::Mutex;`, and give `StatsCounters` a `rate: Mutex<RateState>` field. `snapshot` becomes `self.snapshot_at(active_flows, Instant::now())`. The EMA body (this is the algorithm the tests pin):

```rust
const RATE_TAU_SECS: f64 = 2.0;

#[derive(Debug, Default)]
struct RateState {
    last: Option<Instant>, // None => first sample
    tx: u64,
    rx: u64,
    tx_rate: f64,
    rx_rate: f64,
}

impl RateState {
    /// Fold one sample in; `dt == 0` leaves the pending delta intact.
    fn sample(&mut self, tx: u64, rx: u64, now: Instant) -> (u64, u64) {
        match self.last {
            None => {
                self.tx = tx;
                self.rx = rx;
                self.last = Some(now);
            }
            Some(prev) => {
                let dt = now.duration_since(prev).as_secs_f64();
                if dt > 0.0 {
                    let inst_tx = tx.saturating_sub(self.tx) as f64 / dt;
                    let inst_rx = rx.saturating_sub(self.rx) as f64 / dt;
                    let alpha = 1.0 - (-dt / RATE_TAU_SECS).exp();
                    self.tx_rate += (inst_tx - self.tx_rate) * alpha;
                    self.rx_rate += (inst_rx - self.rx_rate) * alpha;
                    self.tx = tx;
                    self.rx = rx;
                    self.last = Some(now);
                }
            }
        }
        (round_rate(self.tx_rate), round_rate(self.rx_rate))
    }
}

fn round_rate(rate: f64) -> u64 {
    rate.max(0.0).round() as u64
}
```

`snapshot_at` loads both atomics, locks `self.rate`, calls `sample(tx, rx, now)`, and builds `Stats`. `reset` stores `0` into both atomics and replaces the rate state with `RateState::default()`. Recovers a poisoned lock with `unwrap_or_else(|e| e.into_inner())`, matching the style in `bridge.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path rust/Cargo.toml --lib stats`
Expected: PASS.

- [ ] **Step 5: Run the whole Rust suite**

Run: `cargo test --manifest-path rust/Cargo.toml`
Expected: PASS (including `stopping zeroes the counters`, which still equals `Stats::default()`).

- [ ] **Step 6: Commit**

```bash
git add rust/src/stats.rs
git commit -m "feat(rust): smoothed down/up byte rate on Stats"
```

---

### Task 2: Rust surface + regenerated bindings + Dart call sites

**Files:**
- Modify: `rust/src/native.rs`
- Modify: `rust/src/api.rs`
- Modify: `rust/tests/native.rs`
- Regenerated: `rust/src/frb_generated.rs`, `lib/src/rust/*.dart`
- Modify: `lib/src/dashboard.dart`, `lib/src/android_bridge.dart`, `lib/src/demo_bridge.dart`
- Modify: `test/widget_test.dart`, `test/bridge_api_test.dart`, `test/dashboard_test.dart`, `test/android_bridge_test.dart`

**Interfaces:**
- Consumes: `Stats.tx_bytes_per_sec` / `rx_bytes_per_sec` (Task 1).
- Produces: Dart `Stats.txBytesPerSec` / `rxBytesPerSec` (`BigInt`), from regenerated FRB bindings.
- Produces: Android snapshot JSON keys `"txBytesPerSec"` / `"rxBytesPerSec"` (decimal strings).

- [ ] **Step 1: Add the keys to the snapshot and a test**

In `rust/src/native.rs`, extend the `serde_json::json!` in `snapshot_json()`:

```rust
"txBytesPerSec": stats.tx_bytes_per_sec.to_string(),
"rxBytesPerSec": stats.rx_bytes_per_sec.to_string(),
```

In `rust/tests/native.rs`, in `native_json_boundary_rejects_malformed_settings_and_keeps_snapshot_readable`, add:

```rust
assert_eq!(snapshot["txBytesPerSec"], "0");
assert_eq!(snapshot["rxBytesPerSec"], "0");
```

- [ ] **Step 2: Regenerate the bindings**

Run: `flutter pub get && flutter_rust_bridge_codegen generate`
Expected: `lib/src/rust/stats.dart`'s `Stats` now has `txBytesPerSec` and `rxBytesPerSec`, and `rust/src/frb_generated.rs` round-trips them.

- [ ] **Step 3: Update every Dart `Stats(` call site**

The generator makes the two fields required, so each constructor must name them. Use real values where the source has them:

- `lib/src/android_bridge.dart` — `stats()` reads the new snapshot keys:
  ```dart
  txBytesPerSec: BigInt.parse('${snapshot['txBytesPerSec'] ?? 0}'),
  rxBytesPerSec: BigInt.parse('${snapshot['rxBytesPerSec'] ?? 0}'),
  ```
- `lib/src/dashboard.dart` — `_stats` field initializer gets `txBytesPerSec: BigInt.zero, rxBytesPerSec: BigInt.zero`.
- `lib/src/demo_bridge.dart` — `stats()` gets `txBytesPerSec: BigInt.zero, rxBytesPerSec: BigInt.zero` (made live in Task 5).
- `test/widget_test.dart`, `test/bridge_api_test.dart`, `test/dashboard_test.dart` (two literals) — add `txBytesPerSec: BigInt.zero, rxBytesPerSec: BigInt.zero`.

- [ ] **Step 4: Prove the Android adapter reads the keys**

In `test/android_bridge_test.dart`, add `'txBytesPerSec': '3750000', 'rxBytesPerSec': '5000000'` to the mocked `jsonEncode` map, and in `attaches to existing service...` add:

```dart
expect(api.stats().txBytesPerSec, BigInt.parse('3750000'));
expect(api.stats().rxBytesPerSec, BigInt.parse('5000000'));
```

- [ ] **Step 5: Run everything**

Run: `cargo test --manifest-path rust/Cargo.toml && flutter test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(rust): expose bridge byte rates over frb and the snapshot"
```

---

### Task 3: `formatMbps`

**Files:**
- Modify: `lib/src/format.dart`
- Modify: `test/format_test.dart`

**Interfaces:**
- Produces: `String formatMbps(BigInt bytesPerSec)` — decimal Mbps, `≥ 10` whole, `< 10` one decimal with a trailing `.0` dropped.

- [ ] **Step 1: Write the failing tests**

Add to `test/format_test.dart`:

```dart
group('formatMbps', () {
  test('renders whole numbers at and above ten', () {
    expect(formatMbps(BigInt.from(5000000)), '40'); // 40 Mbps
    expect(formatMbps(BigInt.from(1250000)), '10'); // exactly 10 Mbps
  });
  test('renders one decimal below ten, dropping a trailing .0', () {
    expect(formatMbps(BigInt.from(425000)), '3.4');
    expect(formatMbps(BigInt.from(125000)), '1');
  });
  test('zero is zero', () {
    expect(formatMbps(BigInt.zero), '0');
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `flutter test test/format_test.dart`
Expected: FAIL — `formatMbps` is not defined.

- [ ] **Step 3: Implement `formatMbps` in `lib/src/format.dart`**

```dart
String formatMbps(BigInt bytesPerSec) {
  final mbps = bytesPerSec.toDouble() * 8 / 1e6;
  return mbps >= 10
      ? mbps.toStringAsFixed(0)
      : mbps.toStringAsFixed(1).replaceFirst(RegExp(r'\.0$'), '');
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `flutter test test/format_test.dart`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/src/format.dart test/format_test.dart
git commit -m "feat(ui): formatMbps with the formatBytes precision rule"
```

---

### Task 4: Dashboard — five rows

**Files:**
- Modify: `lib/src/dashboard.dart`
- Modify: `test/dashboard_test.dart`

**Interfaces:**
- Consumes: `formatMbps` (Task 3); `Stats.txBytesPerSec` / `rxBytesPerSec` (Task 2).

- [ ] **Step 1: Update the failing test**

In `test/dashboard_test.dart`, in `renders human-readable stats`, set the stats and rename/extend the assertions:

```dart
statsValue: Stats(
  txBytes: BigInt.from(999),
  rxBytes: BigInt.from(1024),
  activeFlows: BigInt.from(3),
  txBytesPerSec: BigInt.from(5000000), // 40 Mbps up
  rxBytesPerSec: BigInt.from(3750000), // 30 Mbps down
),
```

```dart
expect(_text(tester, 'bytes-download'), '1 KiB');
expect(_text(tester, 'bytes-upload'), '999 B');
expect(_text(tester, 'speed-down'), '30 Mbps');
expect(_text(tester, 'speed-up'), '40 Mbps');
expect(_text(tester, 'active-flows'), '3');
```

- [ ] **Step 2: Run to verify it fails**

Run: `flutter test test/dashboard_test.dart`
Expected: FAIL — the new keys are absent.

- [ ] **Step 3: Render the five rows in `lib/src/dashboard.dart`**

Replace the stats card's three rows with five, each separated by `const Divider(height: 1)`:

```dart
_StatRow(label: 'Download', value: formatBytes(_stats.rxBytes), valueKey: 'bytes-download'),
_StatRow(label: 'Upload', value: formatBytes(_stats.txBytes), valueKey: 'bytes-upload'),
_StatRow(label: 'Down', value: '${formatMbps(_stats.rxBytesPerSec)} Mbps', valueKey: 'speed-down'),
_StatRow(label: 'Up', value: '${formatMbps(_stats.txBytesPerSec)} Mbps', valueKey: 'speed-up'),
_StatRow(label: 'Active flows', value: '${_stats.activeFlows}', valueKey: 'active-flows'),
```

- [ ] **Step 4: Run to verify it passes**

Run: `flutter test test/dashboard_test.dart`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add lib/src/dashboard.dart test/dashboard_test.dart
git commit -m "feat(ui): dashboard Download/Upload plus Down/Up speed rows"
```

---

### Task 5: Demo bridge — live simulated rates

**Files:**
- Modify: `lib/src/demo_bridge.dart`

**Interfaces:**
- Consumes: `Stats.txBytesPerSec` / `rxBytesPerSec` (Task 2).

- [ ] **Step 1: Compute rates from the demo tick**

Add two fields (`_rxRate`/`_txRate`, both `BigInt.zero`) and have `_tick()` turn its
random per-direction byte increment into a bytes-per-second rate:

```dart
void _tick() {
  final rxDelta = BigInt.from(20 * 1024 + _random.nextInt(180 * 1024));
  final txDelta = BigInt.from(8 * 1024 + _random.nextInt(90 * 1024));
  _rx += rxDelta;
  _tx += txDelta;
  _rxRate = (rxDelta * 1000) ~/ tickInterval.inMilliseconds;
  _txRate = (txDelta * 1000) ~/ tickInterval.inMilliseconds;
  final drift = _random.nextInt(5) - 2;
  _flows = (_flows + drift).clamp(1, 24).toInt();
}
```

Reset `_rxRate`/`_txRate` to `BigInt.zero` wherever `_rx`/`_tx` are reset (`start()` and `stop()`), and expose them in `stats()`:

```dart
Stats(txBytes: _tx, rxBytes: _rx, activeFlows: BigInt.from(_flows),
      txBytesPerSec: _txRate, rxBytesPerSec: _rxRate);
```

- [ ] **Step 2: Run the demo smoke test**

Run: `flutter test test/widget_test.dart`
Expected: PASS (`demo mode connects and reaches the error UI` still passes).

- [ ] **Step 3: Commit**

```bash
git add lib/src/demo_bridge.dart
git commit -m "feat(demo): simulated down/up rates for desktop demo"
```

---

### Task 6: Notification subtitle — the `40-30` pair

**Files:**
- Modify: `android/app/src/main/kotlin/dev/cellbridge/BridgeService.kt`
- Modify: `android/app/src/test/kotlin/dev/cellbridge/BridgeServiceTest.kt`

**Interfaces:**
- Consumes: snapshot JSON `txBytesPerSec` / `rxBytesPerSec` (Task 2).
- Produces: `internal fun notificationSubtitle(snapshot: JSONObject?): String?` and `internal fun formatMbps(raw: String): String`.

- [ ] **Step 1: Write the failing tests**

In `BridgeServiceTest.kt`, keep the title test and add (with `import org.json.JSONObject`):

```kotlin
@Test fun notification_subtitle_is_the_speed_pair_only_when_connected() {
    val connected = JSONObject().put("state", "connected")
        .put("rxBytesPerSec", "5000000").put("txBytesPerSec", "3750000")
    assertEquals("40-30", notificationSubtitle(connected))
    assertNull(notificationSubtitle(JSONObject().put("state", "connecting")))
    assertNull(notificationSubtitle(JSONObject().put("state", "error")))
    assertNull(notificationSubtitle(null))
}

@Test fun format_mbps_uses_the_shared_rule() {
    assertEquals("40", formatMbps("5000000"))
    assertEquals("10", formatMbps("1250000"))
    assertEquals("3.4", formatMbps("425000"))
    assertEquals("0", formatMbps("0"))
    assertEquals("0", formatMbps("not-a-number"))
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `./gradlew :app:testDebugUnitTest --tests 'dev.cellbridge.BridgeServiceTest'` (from `android/`)
Expected: FAIL — `notificationSubtitle`/`formatMbps` are unresolved.

- [ ] **Step 3: Implement in `BridgeService.kt`**

Add `import java.util.Locale`, then next to `notificationTitle`:

```kotlin
/** The ongoing notification's subtitle: the bare `down-up` speed, or null. */
internal fun notificationSubtitle(snapshot: JSONObject?): String? {
    if (snapshot?.optString("state") != "connected") return null
    val down = formatMbps(snapshot.optString("rxBytesPerSec", "0"))
    val up = formatMbps(snapshot.optString("txBytesPerSec", "0"))
    return "$down-$up"
}

internal fun formatMbps(raw: String): String {
    val mbps = (raw.toLongOrNull() ?: 0L) * 8.0 / 1_000_000.0
    val text = if (mbps >= 10) "%.0f".format(Locale.US, mbps) else "%.1f".format(Locale.US, mbps)
    return text.removeSuffix(".0")
}
```

Change `notification(status: String)` to `notification(snapshot: JSONObject?)`: title stays `notificationTitle(snapshot?.optString("state") ?: "idle")`, and apply `.setContentText(subtitle)` only when `notificationSubtitle(snapshot)` is non-null. Update the two callers: `onCreate` passes `notification(null)`; `publish()` passes `notification(snapshot)`.

- [ ] **Step 4: Run to verify it passes**

Run: `./gradlew :app:testDebugUnitTest --tests 'dev.cellbridge.BridgeServiceTest'` (from `android/`)
Expected: PASS.

- [ ] **Step 5: Full sweep, then commit**

Run: `cargo test --manifest-path rust/Cargo.toml && flutter test && (cd android && ./gradlew :app:testDebugUnitTest)`
Expected: PASS.

```bash
git add android/app/src/main/kotlin/dev/cellbridge/BridgeService.kt \
        android/app/src/test/kotlin/dev/cellbridge/BridgeServiceTest.kt
git commit -m "feat(android): down-up speed subtitle on the ongoing notification"
```
