//! Congestion-control adapters between the packet core's RFC 9002 recovery
//! (lane 1) and BBRv3 (lane 2), and from TCP (lane 7) to BBRv3.
//!
//! The wave-four contract fixes one event vocabulary, but each lane defines
//! its own copy of the types, so this module converts between them and adds
//! what neither lane owns:
//!
//! * the per-packet [`PacketDeliveryState`] snapshot BBR returns on send and
//!   needs back on acknowledgement or loss (the contract assigns its
//!   attachment to integration);
//! * batching: recovery emits one `Acked` event per packet, while BBR's
//!   `GenerateRateSample` wants all packets newly acknowledged by one ACK
//!   frame together with that ACK's RTT sample.
//!
//! Both adapters keep their snapshots in buffers sized at construction, so
//! the steady state performs no heap allocation.

use std::collections::VecDeque;
use std::fmt;

use vesper_warp_bbr::{self as bbr, BbrConfig, BbrError, Bbrv3, PacketDeliveryState};
use vesper_warp_tcp::cc as tcp;

use crate::recovery;

/// Lane 1 → lane 2 conversion of a sent packet.
pub const fn bbr_sent(p: recovery::SentPacket) -> bbr::SentPacket {
    bbr::SentPacket {
        pn: p.pn,
        sent_ns: p.sent_ns,
        bytes: p.bytes,
        ack_eliciting: p.ack_eliciting,
        in_flight: p.in_flight,
    }
}

/// Lane 1 → lane 2 conversion of an RTT sample.
pub const fn bbr_rtt(s: recovery::RttSample) -> bbr::RttSample {
    bbr::RttSample {
        latest_ns: s.latest_ns,
        min_ns: s.min_ns,
        smoothed_ns: s.smoothed_ns,
        ack_delay_ns: s.ack_delay_ns,
    }
}

/// Lane 7 → lane 2 conversion of an RTT sample.
pub const fn bbr_rtt_from_tcp(s: tcp::RttSample) -> bbr::RttSample {
    bbr::RttSample {
        latest_ns: s.latest_ns,
        min_ns: s.min_ns,
        smoothed_ns: s.smoothed_ns,
        ack_delay_ns: s.ack_delay_ns,
    }
}

/// Adapter failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcError {
    /// The controller refused or rejected an input.
    Bbr(BbrError),
    /// More packets in flight than the snapshot buffer was sized for.
    TooManyInFlight { capacity: usize },
    /// More packets acknowledged by one ACK than the batch buffer holds.
    AckBatchFull { capacity: usize },
}

impl fmt::Display for CcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bbr(e) => write!(f, "BBRv3: {e}"),
            Self::TooManyInFlight { capacity } => {
                write!(f, "more than {capacity} packets in flight")
            }
            Self::AckBatchFull { capacity } => {
                write!(f, "more than {capacity} packets acknowledged at once")
            }
        }
    }
}
impl std::error::Error for CcError {}

impl From<BbrError> for CcError {
    fn from(e: BbrError) -> Self {
        Self::Bbr(e)
    }
}

/// A delivery-state snapshot keyed by packet number. `None` once taken.
#[derive(Debug, Clone, Copy)]
struct Snapshot {
    pn: u64,
    state: Option<PacketDeliveryState>,
}

/// Snapshots in increasing packet-number order, removed out of order and
/// compacted from the front (the same tombstone discipline as lane 1's
/// sent-packet list, so it never holds more entries than recovery tracks).
#[derive(Debug, Clone)]
struct Snapshots {
    entries: VecDeque<Snapshot>,
    capacity: usize,
}

impl Snapshots {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn pop_tombstones(&mut self) {
        while self.entries.front().is_some_and(|s| s.state.is_none()) {
            self.entries.pop_front();
        }
    }

    fn push(&mut self, pn: u64, state: PacketDeliveryState) -> Result<(), CcError> {
        self.pop_tombstones();
        if self.entries.len() >= self.capacity {
            return Err(CcError::TooManyInFlight {
                capacity: self.capacity,
            });
        }
        debug_assert!(self.entries.back().is_none_or(|b| b.pn < pn));
        self.entries.push_back(Snapshot {
            pn,
            state: Some(state),
        });
        Ok(())
    }

    fn get_mut(&mut self, pn: u64) -> Option<&mut Snapshot> {
        let i = self.entries.partition_point(|s| s.pn < pn);
        self.entries.get_mut(i).filter(|s| s.pn == pn)
    }

    fn take(&mut self, pn: u64) -> Option<PacketDeliveryState> {
        let state = self.get_mut(pn).and_then(|s| s.state.take());
        self.pop_tombstones();
        state
    }

    fn len(&self) -> usize {
        self.entries.iter().filter(|s| s.state.is_some()).count()
    }
}

/// BBRv3 driven by lane 1's [`recovery::LossRecovery`] events.
///
/// Per ACK frame: [`QuicBbr::begin_ack`], every event the recovery sink
/// produced in order through [`QuicBbr::on_event`], then
/// [`QuicBbr::end_ack`] with the ACK's RTT sample. Recovery emits ECN-CE
/// and losses before acknowledgements, so BBR sees `EcnCe` before the same
/// ACK's rate sample, as lane 2 requires. Timeout-driven losses go straight
/// through [`QuicBbr::on_event`].
#[derive(Debug, Clone)]
pub struct QuicBbr {
    bbr: Bbrv3,
    snapshots: Snapshots,
    acked: Vec<bbr::Delivered>,
}

impl QuicBbr {
    /// `capacity` bounds packets in flight and packets per ACK; size it like
    /// [`recovery::RecoveryConfig::max_tracked_per_space`].
    pub fn new(config: BbrConfig, now_ns: u64, capacity: usize) -> Result<Self, CcError> {
        Ok(Self {
            bbr: Bbrv3::new(config, now_ns)?,
            snapshots: Snapshots::new(capacity),
            acked: Vec::with_capacity(capacity),
        })
    }

    /// The controller.
    pub const fn controller(&self) -> &Bbrv3 {
        &self.bbr
    }

    /// Packets whose snapshot is still held.
    pub fn snapshots_held(&self) -> usize {
        self.snapshots.len()
    }

    /// Contract outputs.
    pub fn cwnd_bytes(&self) -> u32 {
        self.bbr.cwnd_bytes()
    }
    pub const fn pacing_rate_bytes_per_sec(&self) -> u64 {
        self.bbr.pacing_rate_bytes_per_sec()
    }
    /// §35.3 `advance_bbrv3_edt`: departure time for `bytes` sent at `now_ns`.
    pub fn schedule_edt_ns(&mut self, bytes: u32, now_ns: u64) -> u64 {
        self.bbr.schedule_edt_ns(bytes, now_ns)
    }
    /// The send guard (`bytes_in_flight < cwnd`).
    pub const fn can_send(&self) -> bool {
        self.bbr.can_send()
    }

    /// Records a send. `probe` packets (RFC 9002 PTO probes) bypass the
    /// congestion window. The snapshot is kept for in-flight packets only.
    pub fn on_packet_sent(
        &mut self,
        now_ns: u64,
        packet: recovery::SentPacket,
        probe: bool,
    ) -> Result<(), CcError> {
        if packet.in_flight && self.snapshots.entries.len() >= self.snapshots.capacity {
            self.snapshots.pop_tombstones();
            if self.snapshots.entries.len() >= self.snapshots.capacity {
                return Err(CcError::TooManyInFlight {
                    capacity: self.snapshots.capacity,
                });
            }
        }
        let sent = bbr_sent(packet);
        let state = if probe {
            self.bbr.on_probe_packet_sent(now_ns, sent)?
        } else {
            self.bbr.on_packet_sent(now_ns, sent)?
        };
        if packet.in_flight {
            self.snapshots.push(packet.pn, state)?;
        }
        Ok(())
    }

    /// Starts a new ACK batch.
    pub fn begin_ack(&mut self) {
        self.acked.clear();
    }

    /// One recovery event. `Acked` is buffered until [`QuicBbr::end_ack`];
    /// everything else is applied immediately.
    pub fn on_event(
        &mut self,
        now_ns: u64,
        event: recovery::CongestionEvent,
    ) -> Result<(), CcError> {
        use recovery::CongestionEvent as E;
        match event {
            E::Acked { packet } => {
                let Some(state) = self.snapshots.take(packet.pn) else {
                    return Ok(());
                };
                if self.acked.len() == self.acked.capacity() {
                    return Err(CcError::AckBatchFull {
                        capacity: self.acked.capacity(),
                    });
                }
                self.acked.push(bbr::Delivered {
                    packet: bbr::AckedPacket {
                        pn: packet.pn,
                        sent_ns: packet.sent_ns,
                        bytes: packet.bytes,
                    },
                    state,
                    // Lane 1 never acknowledges a packet it declared lost.
                    previously_lost: false,
                });
                Ok(())
            }
            E::Lost { packet } => {
                let Some(state) = self.snapshots.take(packet.pn) else {
                    return Ok(());
                };
                self.bbr.on_packet_lost(
                    now_ns,
                    bbr::LostPacket {
                        pn: packet.pn,
                        sent_ns: packet.sent_ns,
                        bytes: packet.bytes,
                    },
                    state,
                )?;
                Ok(())
            }
            E::EcnCe { count } => Ok(self.bbr.on_ecn_ce(now_ns, count)?),
            E::PersistentCongestion => Ok(self.bbr.on_persistent_congestion(now_ns)?),
            E::AppLimited => Ok(self.bbr.on_app_limited(now_ns)?),
        }
    }

    /// Delivers the batched acknowledgements with the ACK's RTT sample.
    pub fn end_ack(
        &mut self,
        now_ns: u64,
        rtt: Option<recovery::RttSample>,
    ) -> Result<(), CcError> {
        if self.acked.is_empty() && rtt.is_none() {
            return Ok(());
        }
        let result = self.bbr.on_ack(now_ns, &self.acked, rtt.map(bbr_rtt));
        self.acked.clear();
        Ok(result?)
    }

    /// The application ran out of data.
    pub fn on_app_limited(&mut self, now_ns: u64) -> Result<(), CcError> {
        Ok(self.bbr.on_app_limited(now_ns)?)
    }
}

/// A transmission's snapshot on the TCP side.
#[derive(Debug, Clone, Copy)]
struct TcpSnapshot {
    pn: u64,
    sent_ns: u64,
    state: PacketDeliveryState,
    /// Bytes not yet acknowledged (TCP can acknowledge a segment in parts).
    remaining: u32,
    /// Reported lost: its bytes already left BBR's flight.
    lost: bool,
}

/// Counters for inputs the TCP adapter could not apply. The
/// [`tcp::CongestionController`] trait has no error channel, so failures
/// are counted here instead of panicking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TcpBbrStats {
    /// Inputs BBR rejected.
    pub bbr_errors: u64,
    /// Snapshots dropped because the buffer was full (oldest first).
    pub evicted: u64,
    /// Acknowledgements or losses for transmissions with no snapshot.
    pub unknown: u64,
}

/// BBRv3 behind lane 7's [`tcp::CongestionController`] trait.
///
/// Differences from the QUIC adapter, all forced by the TCP interface:
/// * TCP reports its RTT sample *after* that ACK's `Acked` events, so each
///   acknowledgement is delivered to BBR with the most recent sample seen
///   so far (one ACK late), and that sample is consumed once.
/// * Every acknowledgement is its own one-packet BBR batch.
/// * TCP gates sends on its own window (`pipe < cwnd`, plus forced
///   retransmissions and SYNs), so sends enter BBR without BBR's send
///   guard; otherwise the two flight counts would diverge.
/// * Partial acknowledgements are tracked per transmission and a
///   transmission acknowledged after being reported lost is passed as
///   `previously_lost`.
#[derive(Debug, Clone)]
pub struct TcpBbr {
    bbr: Bbrv3,
    snapshots: VecDeque<TcpSnapshot>,
    capacity: usize,
    rtt: Option<bbr::RttSample>,
    now_ns: u64,
    stats: TcpBbrStats,
    last_error: Option<BbrError>,
}

impl TcpBbr {
    /// `capacity` bounds outstanding transmissions with a snapshot; size it
    /// to the connection's `max_in_flight_segments` or more.
    pub fn new(config: BbrConfig, now_ns: u64, capacity: usize) -> Result<Self, BbrError> {
        Ok(Self {
            bbr: Bbrv3::new(config, now_ns)?,
            snapshots: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            rtt: None,
            now_ns,
            stats: TcpBbrStats::default(),
            last_error: None,
        })
    }

    /// TCP defaults: [`BbrConfig::quic`] with the TCP offload budget.
    pub fn tcp_config(smss: u32) -> BbrConfig {
        BbrConfig {
            offload: bbr::OffloadModel::Tcp,
            ..BbrConfig::quic(smss)
        }
    }

    pub const fn controller(&self) -> &Bbrv3 {
        &self.bbr
    }
    pub const fn stats(&self) -> TcpBbrStats {
        self.stats
    }
    pub const fn last_error(&self) -> Option<BbrError> {
        self.last_error
    }

    fn clock(&mut self, now_ns: u64) -> u64 {
        self.now_ns = self.now_ns.max(now_ns);
        self.now_ns
    }

    fn note(&mut self, r: Result<(), BbrError>) {
        if let Err(e) = r {
            self.stats.bbr_errors += 1;
            self.last_error = Some(e);
        }
    }

    fn find(&self, pn: u64) -> Option<usize> {
        let i = self.snapshots.partition_point(|s| s.pn < pn);
        (self.snapshots.get(i)?.pn == pn).then_some(i)
    }
}

impl tcp::CongestionController for TcpBbr {
    /// BBR's SMSS is fixed at construction; an MSS change is ignored.
    fn on_mss(&mut self, _smss: u32) {}

    fn on_sent(&mut self, p: tcp::SentPacket) {
        let now = self.now_ns;
        let sent = bbr::SentPacket {
            pn: p.pn,
            sent_ns: p.sent_ns,
            bytes: p.bytes,
            ack_eliciting: p.ack_eliciting,
            in_flight: p.in_flight,
        };
        match self.bbr.on_probe_packet_sent(now, sent) {
            Ok(state) if p.in_flight => {
                if self.snapshots.len() == self.capacity {
                    self.snapshots.pop_front();
                    self.stats.evicted += 1;
                }
                self.snapshots.push_back(TcpSnapshot {
                    pn: p.pn,
                    sent_ns: p.sent_ns,
                    state,
                    remaining: p.bytes,
                    lost: false,
                });
            }
            Ok(_) => {}
            Err(e) => self.note(Err(e)),
        }
    }

    fn on_rtt_sample(&mut self, sample: tcp::RttSample) {
        self.rtt = Some(bbr_rtt_from_tcp(sample));
    }

    fn on_event(&mut self, event: tcp::CongestionEvent, now_ns: u64) {
        let now = self.clock(now_ns);
        match event {
            tcp::CongestionEvent::Acked { packet, .. } => {
                let Some(i) = self.find(packet.pn) else {
                    self.stats.unknown += 1;
                    return;
                };
                let snap = self.snapshots[i];
                let bytes = packet.bytes.min(snap.remaining);
                let delivered = bbr::Delivered {
                    packet: bbr::AckedPacket {
                        pn: packet.pn,
                        // TCP stamps a transmission with its departure time.
                        sent_ns: packet.sent_ns.min(now),
                        bytes,
                    },
                    state: snap.state,
                    previously_lost: snap.lost,
                };
                let rtt = self.rtt.take();
                let r = self.bbr.on_ack(now, &[delivered], rtt);
                self.note(r);
                let remaining = snap.remaining - bytes;
                if remaining == 0 {
                    self.snapshots.remove(i);
                } else {
                    self.snapshots[i].remaining = remaining;
                }
            }
            tcp::CongestionEvent::Lost { packet, .. } => {
                let Some(i) = self.find(packet.pn) else {
                    self.stats.unknown += 1;
                    return;
                };
                let snap = self.snapshots[i];
                if snap.lost {
                    return;
                }
                self.snapshots[i].lost = true;
                let lost = bbr::LostPacket {
                    pn: packet.pn,
                    sent_ns: packet.sent_ns.min(now),
                    bytes: snap.remaining,
                };
                let r = self.bbr.on_packet_lost(now, lost, snap.state);
                self.note(r);
            }
            tcp::CongestionEvent::EcnCe { count } => {
                let r = self.bbr.on_ecn_ce(now, count);
                self.note(r);
            }
            tcp::CongestionEvent::PersistentCongestion => {
                // An RTO: TCP marks every outstanding segment lost without
                // reporting each one and later retransmits them under new
                // transmission ids, so the old ids would never be resolved.
                // Report them lost here, then the RTO itself.
                for i in 0..self.snapshots.len() {
                    let snap = self.snapshots[i];
                    if snap.lost {
                        continue;
                    }
                    self.snapshots[i].lost = true;
                    let lost = bbr::LostPacket {
                        pn: snap.pn,
                        sent_ns: snap.sent_ns.min(now),
                        bytes: snap.remaining,
                    };
                    let r = self.bbr.on_packet_lost(now, lost, snap.state);
                    self.note(r);
                }
                let r = self.bbr.on_persistent_congestion(now);
                self.note(r);
            }
            tcp::CongestionEvent::AppLimited => {
                let r = self.bbr.on_app_limited(now);
                self.note(r);
            }
        }
    }

    fn cwnd_bytes(&self) -> u32 {
        self.bbr.cwnd_bytes()
    }

    fn pacing_rate_bytes_per_sec(&self) -> u64 {
        self.bbr.pacing_rate_bytes_per_sec()
    }

    fn schedule_edt_ns(&mut self, bytes: u32, now_ns: u64) -> u64 {
        let now = self.clock(now_ns);
        self.bbr.schedule_edt_ns(bytes, now)
    }
}
