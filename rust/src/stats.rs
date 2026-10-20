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
use std::time::{Duration, Instant};

/// A snapshot of what the bridge has moved and how many flows are live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Payload bytes sent from clients toward the real destinations (uplink).
    pub tx_bytes: u64,
    /// Payload bytes received from the real destinations toward clients.
    pub rx_bytes: u64,
    /// Live TCP splices plus tracked UDP flows.
    pub active_flows: usize,
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

/// Lock-free payload byte counters shared between the data path and the reader.
#[derive(Debug, Default)]
pub struct StatsCounters {
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
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

    /// Zero the byte counters.
    ///
    /// Used when the bridge stops, so a fresh run starts from zero rather than
    /// inheriting the previous run's totals.
    pub fn reset(&self) {
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.rx_bytes.store(0, Ordering::Relaxed);
    }

    /// Snapshot the counters with the caller-supplied live flow count.
    ///
    /// `active_flows` comes from the structures that own the flows, not from a
    /// counter that teardown paths would have to remember to decrement: the
    /// bridge loop passes `TcpSpliceEngine::flow_count() +
    /// UdpForwarder::len()`, which is zero by construction once every flow has
    /// been torn down.
    pub fn snapshot(&self, active_flows: usize) -> Stats {
        Stats {
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            active_flows,
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
    /// it is down and the bridge was connected, the down window is timed; past
    /// `grace` the state becomes [`BridgeState::Error`] and
    /// [`LinkEvent::Lost`] is returned exactly once, so the caller tears every
    /// flow down.
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

        // A link that is down before it ever came up is not a loss: there is
        // nothing to tear down, and a misconfigured endpoint must read as
        // Connecting rather than Error.
        if self.state != BridgeState::Connected && self.state != BridgeState::Error {
            return LinkEvent::None;
        }

        // Already declared lost: hold in Error until the link returns.
        if self.state == BridgeState::Error {
            return LinkEvent::None;
        }

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
            }
        );
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

        // A link that never comes up is *not* a loss: there was nothing
        // connected to lose, so the state stays Connecting however long it is
        // down (a bogus endpoint must report Connecting, not Error).
        assert_eq!(monitor.observe(false, t0 + grace() * 4), LinkEvent::None);
        assert_eq!(monitor.status().state, BridgeState::Connecting);

        assert_eq!(monitor.observe(true, t0), LinkEvent::Connected);
        assert_eq!(
            monitor.status(),
            BridgeStatus {
                state: BridgeState::Connected,
                error: None,
            }
        );
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
        assert_eq!(monitor.observe(true, t0 + grace() * 4), LinkEvent::Connected);
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
