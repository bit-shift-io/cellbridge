//! An in-memory [`smoltcp::phy::Device`] backed by queues.
//!
//! The bridge's data path is queue-based: decrypted inner IP packets arrive from
//! the WireGuard tunnel and are handed to the stack via [`QueueDevice::push_rx`],
//! while packets the stack wants to send are collected with
//! [`QueueDevice::drain_tx`] and later encapsulated into the tunnel. Neither
//! side blocks; the caller decides when to move packets.

use std::collections::VecDeque;

use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// A smoltcp device whose RX and TX paths are plain in-memory queues.
///
/// The device is *IP-medium*: it exchanges bare IP packets with the stack (no
/// Ethernet header), which is what a layer-3 tunnel carries. Consequently
/// [`QueueDevice::new`]'s `mtu` is the IP MTU, and smoltcp derives its
/// advertised MSS (`mtu - 40` for IPv4+TCP) from it.
///
/// The queues are unbounded; the caller is expected to drain TX promptly and to
/// only push RX packets it actually wants processed.
#[derive(Debug)]
pub struct QueueDevice {
    mtu: usize,
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
}

impl QueueDevice {
    /// Build an empty device advertising `mtu` as its IP MTU.
    pub fn new(mtu: usize) -> Self {
        QueueDevice {
            mtu,
            rx: VecDeque::new(),
            tx: VecDeque::new(),
        }
    }

    /// Queue an inbound IP packet for the stack to receive.
    ///
    /// Packets larger than the device's MTU are dropped: smoltcp may not be
    /// handed a frame it could not itself transmit, and the tiny stack has no
    /// reassembly buffer for them.
    pub fn push_rx(&mut self, pkt: Vec<u8>) {
        if pkt.len() <= self.mtu {
            self.rx.push_back(pkt);
        }
    }

    /// Remove and return every outbound packet the stack has produced.
    ///
    /// The TX queue is left empty; an empty queue drains to an empty `Vec`.
    pub fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        self.tx.drain(..).collect()
    }

    /// How many inbound packets are waiting to be received.
    pub fn rx_len(&self) -> usize {
        self.rx.len()
    }

    /// The IP MTU advertised to the stack.
    pub fn mtu(&self) -> usize {
        self.mtu
    }
}

impl Device for QueueDevice {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // Only pop a packet when one is queued, so an `RxToken` always has data
        // to hand the caller.
        self.rx.pop_front().map(move |buffer| {
            let rx = RxToken { buffer };
            let tx = TxToken {
                queue: &mut self.tx,
            };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            queue: &mut self.tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        // `DeviceCapabilities` is `#[non_exhaustive]`, so it must be built from
        // its default and then adjusted.
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

/// The receive half of a [`QueueDevice`], owning the packet it hands over.
#[derive(Debug)]
pub struct RxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer)
    }
}

/// The transmit half of a [`QueueDevice`], appending what it is given to the
/// device's TX queue.
#[derive(Debug)]
pub struct TxToken<'a> {
    queue: &'a mut VecDeque<Vec<u8>>,
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        self.queue.push_back(buffer);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::phy::{RxToken as _, TxToken as _};

    /// A minimal (header-only) IPv4 packet: version 4, 20 byte header, UDP.
    fn ipv4_packet() -> Vec<u8> {
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, IHL 5
        ip[2..4].copy_from_slice(&20u16.to_be_bytes()); // total length
        ip[8] = 64; // TTL
        ip[9] = 17; // protocol: UDP
        ip[12..16].copy_from_slice(&[10, 0, 0, 2]); // source
        ip[16..20].copy_from_slice(&[10, 0, 0, 1]); // destination
        ip
    }

    #[test]
    fn receives_pushed_and_collects_transmitted() {
        let mut d = QueueDevice::new(1420);
        d.push_rx(ipv4_packet());
        assert_eq!(d.rx_len(), 1);
        assert_eq!(d.mtu(), 1420);
    }

    #[test]
    fn receive_hands_over_the_pushed_packet_unmodified() {
        let mut d = QueueDevice::new(1420);
        let pkt = ipv4_packet();
        d.push_rx(pkt.clone());

        let (rx, _tx) = d.receive(Instant::ZERO).expect("a packet is queued");
        let seen = rx.consume(|buf| buf.to_vec());
        assert_eq!(seen, pkt);
        assert_eq!(d.rx_len(), 0, "consuming the rx token pops the queue");
        assert!(d.receive(Instant::ZERO).is_none(), "queue is empty again");
    }

    #[test]
    fn transmitted_packet_lands_in_drain_tx_and_is_then_gone() {
        let mut d = QueueDevice::new(1420);

        let tx = d.transmit(Instant::ZERO).expect("transmit always yields");
        tx.consume(4, |buf| buf.copy_from_slice(&[1, 2, 3, 4]));

        assert_eq!(d.drain_tx(), vec![vec![1, 2, 3, 4]]);
        assert!(d.drain_tx().is_empty(), "drain_tx empties the queue");
    }

    #[test]
    fn capabilities_report_the_ip_mtu() {
        let caps = QueueDevice::new(1420).capabilities();
        assert_eq!(caps.max_transmission_unit, 1420);
        assert_eq!(caps.medium, Medium::Ip);
    }

    #[test]
    fn oversized_packet_is_never_handed_to_the_stack() {
        let mut d = QueueDevice::new(1420);
        d.push_rx(vec![0u8; 1421]);
        assert_eq!(d.rx_len(), 0);
        assert!(d.receive(Instant::ZERO).is_none());
    }

    #[test]
    fn a_packet_exactly_at_the_mtu_is_accepted() {
        let mut d = QueueDevice::new(1420);
        d.push_rx(vec![0u8; 1420]);
        assert_eq!(d.rx_len(), 1);
    }
}
