//! Bridge-level counters and the link state machine.
//!
//! The bridge moves bytes on two paths — the TCP splice engine and the UDP
//! forwarder — and reports them as a single [`Stats`] snapshot. The byte
//! counters are lock-free atomics shared between the data path and whatever
//! thread reads the status, so recording a packet costs one relaxed
//! `fetch_add` and never takes a lock or allocates.
//!
//! The live flow count is *not* a counter: it is supplied to
//! [`StatsCounters::snapshot`] by the structures that own the flows
//! ([`crate::TcpSpliceEngine::flow_count`] and [`crate::UdpForwarder::len`]).
//! A counter that is decremented on every teardown path can drift; deriving the
//! count from the live tables cannot.
//!
//! [`BridgeMonitor`] is the state machine the bridge loop drives: it turns the
//! tunnel's `is_connected()` into `Idle`/`Connecting`/`Connected`/`Error`, and
//! reports [`LinkEvent::Lost`] only once the link has been down for longer than
//! the grace period. A dead uplink therefore fails closed — every flow is torn
//! down — while a transient session expiry that re-handshakes in time is
//! ignored.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A snapshot of what the bridge has moved, how fast, and how many flows are
/// live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Payload bytes sent from clients toward the real destinations (uplink).
    pub tx_bytes: u64,
    /// Payload bytes received from the real destinations toward clients.
    pub rx_bytes: u64,
    /// Live TCP splices plus tracked UDP flows.
    pub active_flows: usize,
    /// Smoothed uplink rate (bytes per second).
    pub tx_bytes_per_sec: u64,
    /// Smoothed downlink rate (bytes per second).
    pub rx_bytes_per_sec: u64,
}

/// Where the bridge is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeState {
    /// Stopped: no tunnel, no flows.
    Idle,
    /// Started, but the WireGuard handshake has not completed yet.
    Connecting,
    /// The handshake completed and a session is live.
    Connected,
    /// The link was lost and its flows were torn down.
    Error,
}

/// The bridge's state plus the last error, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeStatus {
    /// The current lifecycle state.
    pub state: BridgeState,
    /// A human-readable reason, set when the state is [`BridgeState::Error`].
    pub error: Option<String>,
}

/// What an [`BridgeMonitor::observe`] call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEvent {
    /// Nothing changed.
    None,
    /// The link is down but still inside the grace period.
    Down,
    /// The link is up — the first handshake, or a re-handshake after a loss.
    Connected,
    /// The link has been down past the grace period: tear every flow down.
    Lost,
}

/// Time constant of the rate's exponential moving average.
///
/// Long enough to smooth a one-second sample into a steady readout, short
/// enough that a real change shows up within a couple of seconds.
const RATE_TAU_SECS: f64 = 2.0;

/// The previous rate sample, so a smoothed rate can be derived from the
/// cumulative counters without the data path ever taking a lock.
#[derive(Debug, Default)]
struct RateState {
    /// `None` until the first sample primes the window.
    last: Option<Instant>,
    /// Byte counts as of the previous sample.
    tx: u64,
    rx: u64,
    /// Smoothed bytes-per-second rates.
    tx_rate: f64,
    rx_rate: f64,
}

impl RateState {
    /// Fold one sample in and return the smoothed rates.
    ///
    /// A zero interval (`dt == 0`, two reads in the same instant) is not a
    /// sample: the pending delta is kept so the next real interval measures it.
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

/// Payload byte counters shared between the data path and the reader.
///
/// The byte counters are lock-free atomics, so recording a packet never takes a
/// lock. The smoothed rate is derived on read, under a small `Mutex` that only
/// [`StatsCounters::snapshot`] and [`StatsCounters::reset`] ever touch.
#[derive(Debug, Default)]
pub struct StatsCounters {
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
    rate: Mutex<RateState>,
}

impl StatsCounters {
    /// Create a fresh set of zeroed counters.
    pub fn new() -> StatsCounters {
        StatsCounters::default()
    }

    /// Record `bytes` of client payload sent toward a real destination.
    pub fn add_tx(&self, bytes: usize) {
        self.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record `bytes` of payload received from a real destination.
    pub fn add_rx(&self, bytes: usize) {
        self.rx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Zero the byte counters and the rate.
    ///
    /// Used when the bridge stops, so a fresh run starts from zero rather than
    /// inheriting the previous run's totals or rate.
    pub fn reset(&self) {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.rx_bytes.store(0, Ordering::Relaxed);
        *rate = RateState::default();
    }

    /// Snapshot the counters with the caller-supplied live flow count.
    ///
    /// `active_flows` comes from the structures that own the flows, not from a
    /// counter that teardown paths would have to remember to decrement: the
    /// bridge loop passes `TcpSpliceEngine::flow_count() +
    /// UdpForwarder::len()`, which is zero by construction once every flow has
    /// been torn down.
    pub fn snapshot(&self, active_flows: usize) -> Stats {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_locked(active_flows, Instant::now(), &mut rate)
    }

    /// Snapshot the counters against an explicit instant.
    ///
    /// The rate is a function of the wall clock between reads; taking the time
    /// as a parameter keeps that deterministic under test.
    #[cfg(test)]
    fn snapshot_at(&self, active_flows: usize, now: Instant) -> Stats {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_locked(active_flows, now, &mut rate)
    }

    fn snapshot_locked(&self, active_flows: usize, now: Instant, rate: &mut RateState) -> Stats {
        let tx_bytes = self.tx_bytes.load(Ordering::Relaxed);
        let rx_bytes = self.rx_bytes.load(Ordering::Relaxed);
        let (tx_bytes_per_sec, rx_bytes_per_sec) = rate.sample(tx_bytes, rx_bytes, now);
        Stats {
            tx_bytes,
            rx_bytes,
            active_flows,
            tx_bytes_per_sec,
            rx_bytes_per_sec,
        }
    }
}

/// The link state machine the bridge loop drives.
pub struct BridgeMonitor {
    state: BridgeState,
    error: Option<String>,
    grace: Duration,
    down_since: Option<Instant>,
}

impl BridgeMonitor {
    /// Create a monitor that tears flows down once the link has been down for
    /// longer than `grace`.
    ///
    /// The grace period is what keeps a transient session expiry harmless: a
    /// re-handshake that completes inside it never reaches
    /// [`LinkEvent::Lost`].
    pub fn new(grace: Duration) -> BridgeMonitor {
        BridgeMonitor {
            state: BridgeState::Idle,
            error: None,
            grace,
            down_since: None,
        }
    }

    /// Enter [`BridgeState::Connecting`].
    ///
    /// Called when the bridge starts; the state is deliberately *not*
    /// [`BridgeState::Connected`] yet — that only happens once the handshake
    /// completes.
    pub fn start(&mut self) {
        self.state = BridgeState::Connecting;
        self.error = None;
        self.down_since = None;
    }

    /// Return to [`BridgeState::Idle`].
    pub fn stop(&mut self) {
        self.state = BridgeState::Idle;
        self.error = None;
        self.down_since = None;
    }

    /// Record the link's current state and advance the state machine.
    ///
    /// `connected` is `Tunnel::is_connected()` and *only* that: it is the sole
    /// loss signal. A `TunnelError::WireGuard(ConnectionExpired)` out of
    /// `Tunnel::tick` is not one — boringtun hands the session back to the
    /// handshake, and a link that re-handshakes inside `grace` must keep every
    /// flow it has. Treating the expiry itself as loss would drop live
    /// connections on a healthy link every rekey window.
    ///
    /// While the link is up the state becomes [`BridgeState::Connected`]. While
    /// it is down, the down window is timed from the first observation. Past
    /// `grace` the state becomes [`BridgeState::Error`] and [`LinkEvent::Lost`]
    /// is returned exactly once, so the caller tears every flow down — and this
    /// applies to a bridge that never came up as well: a phone that cannot
    /// reach its endpoint must surface an error rather than report
    /// [`BridgeState::Connecting`] forever. Only [`BridgeState::Idle`] is
    /// exempt, because an app that was never started has no uplink to lose.
    pub fn observe(&mut self, connected: bool, now: Instant) -> LinkEvent {
        if connected {
            let recovering = self.down_since.is_some() || self.state != BridgeState::Connected;
            self.down_since = None;
            self.error = None;
            self.state = BridgeState::Connected;
            return if recovering {
                LinkEvent::Connected
            } else {
                LinkEvent::None
            };
        }

        // Never started ([`Self::stop`] leaves the monitor here): there is no
        // uplink to lose, so nothing is timed.
        if self.state == BridgeState::Idle {
            return LinkEvent::None;
        }

        // Already declared lost: hold in Error until the link returns.
        if self.state == BridgeState::Error {
            return LinkEvent::None;
        }

        // Both `Connecting` and `Connected` are timed: a link that never
        // handshook is as dead as one that dropped, and must be reported.
        match self.down_since {
            None => {
                self.down_since = Some(now);
                LinkEvent::Down
            }
            Some(since) if now.saturating_duration_since(since) >= self.grace => {
                self.state = BridgeState::Error;
                self.error = Some(format!(
                    "tunnel lost: no handshake for more than {:?}",
                    self.grace
                ));
                LinkEvent::Lost
            }
            Some(_) => LinkEvent::Down,
        }
    }

    /// The current state and error.
    pub fn status(&self) -> BridgeStatus {
        BridgeStatus {
            state: self.state,
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    fn grace() -> Duration {
        Duration::from_secs(5)
    }

    #[test]
    fn counters_accumulate() {
        let counters = StatsCounters::new();
        counters.add_tx(5);
        counters.add_tx(7);
        counters.add_rx(3);
        counters.add_rx(9);

        let stats = counters.snapshot(0);
        assert_eq!(stats.tx_bytes, 12, "tx counts every payload byte");
        assert_eq!(stats.rx_bytes, 12, "rx counts every payload byte");
    }

    #[test]
    fn a_snapshot_carries_the_live_flow_count() {
        let counters = StatsCounters::new();
        assert_eq!(
            counters.snapshot(4),
            Stats {
                tx_bytes: 0,
                rx_bytes: 0,
                active_flows: 4,
                tx_bytes_per_sec: 0,
                rx_bytes_per_sec: 0,
            }
        );
    }

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
            last = counters
                .snapshot_at(0, t0 + Duration::from_secs(second))
                .tx_bytes_per_sec;
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
        assert_eq!(
            counters.snapshot_at(0, t0).tx_bytes_per_sec,
            0,
            "dt == 0 is not a sample"
        );
        // The 2000 bytes are still measured over the full second.
        assert_eq!(
            counters
                .snapshot_at(0, t0 + Duration::from_secs(1))
                .tx_bytes_per_sec,
            787
        );
    }

    #[test]
    fn reset_clears_the_rate() {
        let counters = StatsCounters::new();
        let t0 = Instant::now();
        counters.snapshot_at(0, t0);
        counters.add_tx(2000);
        assert!(
            counters
                .snapshot_at(0, t0 + Duration::from_secs(1))
                .tx_bytes_per_sec
                > 0
        );
        counters.reset();
        assert_eq!(
            counters.snapshot_at(0, t0 + Duration::from_secs(2)),
            Stats::default()
        );
    }

    #[test]
    fn reset_waits_for_the_rate_transaction_before_zeroing_counters() {
        let counters = std::sync::Arc::new(StatsCounters::new());
        counters.add_tx(4000);
        let guard = counters.rate.lock().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::sync::Arc::clone(&counters);
        let thread = std::thread::spawn(move || {
            worker.reset();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        let during_reset = counters.tx_bytes.load(Ordering::Relaxed);
        drop(guard);
        thread.join().unwrap();
        assert_eq!(during_reset, 4000, "reset must not expose half-reset state");
        assert_eq!(counters.snapshot(0), Stats::default());
    }

    #[test]
    fn waiting_snapshot_reads_counters_after_acquiring_the_rate_lock() {
        let counters = std::sync::Arc::new(StatsCounters::new());
        counters.add_tx(4000);
        let mut guard = counters.rate.lock().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::sync::Arc::clone(&counters);
        let thread = std::thread::spawn(move || done_tx.send(worker.snapshot(0)).unwrap());
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        // Complete a reset while the reader waits for its transaction.
        counters.tx_bytes.store(0, Ordering::Relaxed);
        *guard = RateState::default();
        drop(guard);
        let snapshot = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
        assert_eq!(snapshot, Stats::default(), "reader must not retain pre-reset totals");
    }

    #[test]
    fn reset_zeroes_the_byte_counters() {
        let counters = StatsCounters::new();
        counters.add_tx(5);
        counters.add_rx(5);
        counters.reset();
        assert_eq!(counters.snapshot(0), Stats::default());
    }

    #[test]
    fn a_new_monitor_is_idle() {
        let monitor = BridgeMonitor::new(grace());
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Idle,
                error: None,
            }
        );
    }

    #[test]
    fn starting_connects_only_once_the_handshake_completes() {
        let mut monitor = BridgeMonitor::new(grace());
        let t0 = Instant::now();

        monitor.start();
        assert_eq!(monitor.status().state, BridgeState::Connecting);

        // A link that has not handshook yet is still `Connecting` inside the
        // grace period: the handshake may complete at any moment.
        assert_eq!(monitor.observe(false, t0), LinkEvent::Down);
        assert_eq!(monitor.status().state, BridgeState::Connecting);

        assert_eq!(
            monitor.observe(true, t0 + Duration::from_secs(1)),
            LinkEvent::Connected
        );
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Connected,
                error: None,
            }
        );
    }

    /// The grace period starts at [`BridgeMonitor::start`]: a bridge that never
    /// handshakes must surface `Error`, not sit in `Connecting` forever.
    #[test]
    fn a_bridge_that_never_connects_reports_error_after_grace() {
        let mut monitor = BridgeMonitor::new(grace());
        let t0 = Instant::now();
        monitor.start();

        assert_eq!(monitor.observe(false, t0), LinkEvent::Down);
        assert_eq!(
            monitor.status().state,
            BridgeState::Connecting,
            "still inside the grace period"
        );

        assert_eq!(
            monitor.observe(false, t0 + grace() + Duration::from_secs(1)),
            LinkEvent::Lost,
            "the handshake never completed and the grace period elapsed"
        );
        let status = monitor.status();
        assert_eq!(status.state, BridgeState::Error);
        assert!(
            status.error.is_some(),
            "an unreachable endpoint must carry a reason"
        );

        // A monitor that was never started stays `Idle`: there is no uplink to
        // have lost, and no error to report.
        let mut idle = BridgeMonitor::new(grace());
        assert_eq!(idle.observe(false, t0 + grace() * 10), LinkEvent::None);
        assert_eq!(idle.status().state, BridgeState::Idle);
    }

    #[test]
    fn a_transient_disconnect_within_grace_stays_connected() {
        // boringtun's `ConnectionExpired` makes `is_connected()` false until the
        // re-handshake completes. Inside the grace period that must not tear
        // anything down, or a healthy link would drop its flows every rekey.
        let mut monitor = BridgeMonitor::new(grace());
        let t0 = Instant::now();
        monitor.start();
        monitor.observe(true, t0);

        assert_eq!(
            monitor.observe(false, t0 + Duration::from_secs(1)),
            LinkEvent::Down
        );
        assert_eq!(
            monitor.observe(false, t0 + Duration::from_secs(4)),
            LinkEvent::Down,
            "still inside the grace period"
        );
        assert_eq!(
            monitor.status().state,
            BridgeState::Connected,
            "the link is re-handshaking, not lost"
        );

        assert_eq!(
            monitor.observe(true, t0 + Duration::from_secs(4)),
            LinkEvent::Connected
        );
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Connected,
                error: None,
            }
        );
    }

    #[test]
    fn sustained_loss_past_grace_reports_lost_once_and_recovers() {
        let mut monitor = BridgeMonitor::new(grace());
        let t0 = Instant::now();
        monitor.start();
        monitor.observe(true, t0);

        assert_eq!(
            monitor.observe(false, t0 + Duration::from_secs(1)),
            LinkEvent::Down
        );
        assert_eq!(
            monitor.observe(false, t0 + grace() + Duration::from_secs(1)),
            LinkEvent::Lost,
            "the grace period elapsed"
        );

        let status = monitor.status();
        assert_eq!(status.state, BridgeState::Error);
        assert!(
            status.error.is_some(),
            "a loss must carry a human-readable reason"
        );

        // Loss is reported exactly once; the state then holds in Error.
        assert_eq!(monitor.observe(false, t0 + grace() * 3), LinkEvent::None);
        assert_eq!(monitor.status().state, BridgeState::Error);

        // When the link returns, the monitor recovers.
        assert_eq!(
            monitor.observe(true, t0 + grace() * 4),
            LinkEvent::Connected
        );
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Connected,
                error: None,
            }
        );
    }

    #[test]
    fn stopping_returns_to_idle_and_clears_the_error() {
        let mut monitor = BridgeMonitor::new(grace());
        let t0 = Instant::now();
        monitor.start();
        monitor.observe(true, t0);
        monitor.observe(false, t0);
        monitor.observe(false, t0 + grace() + Duration::from_secs(1));
        assert_eq!(monitor.status().state, BridgeState::Error);

        monitor.stop();
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Idle,
                error: None,
            }
        );

        // Starting again is a clean slate: Connecting, no stale error.
        monitor.start();
        assert_eq!(monitor.status().state, BridgeState::Connecting);
    }
}
