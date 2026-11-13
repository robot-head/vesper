//! The Vesper Warp simulation model (scenario version 3): source-level
//! `XdpNic` frames on the Warp software NIC ([`umem`](super::umem), backed by
//! `vesper-warp-umem`) and adversarial WAN links over [`link`](super::link).
//!
//! A NIC belongs to one process. Its **ingress** link carries datagrams from
//! scenario flows to the NIC's RX ring; its **egress** link carries the
//! replies source submits with `submit_paced_tx`. Flows emit seeded bulk
//! micro-bursts: every packet's kind (valid, unknown CID, truncated, long
//! header), packet number and source address are recorded. A NAT rebinding
//! event changes a flow's source address mid-flow.
//!
//! A submitted reply waits in the EDT launch queue and is posted to the NIC's
//! TX ring at its launch time (`max(edt, now)`), where it enters the egress
//! link, so egress queueing follows launch order. Each parked lease gets a
//! PTO timer at launch plus `pto_ns`. ACKs come back one of two ways:
//! - **timer ACK path** (no `ack_path`): an ACK timer per delivered copy of
//!   the reply, at its egress arrival plus `ack_return_ns`; the model retires
//!   the lease. ACKs cannot be lost or aggregated on this path.
//! - **datagram ACK path** (`ack_path`): the flow's peer records every reply
//!   it receives and aggregates them (an ACK after `ack_every` ack-eliciting
//!   packets or `max_ack_delay_ns` after the first unacknowledged one). Each
//!   ACK frame carries the peer's most recent `max_ranges` ranges and crosses
//!   the `ack_path.link` back to the NIC as a real datagram, where it can be
//!   lost, duplicated, reordered or tail-dropped, and lands in the RX ring
//!   like any other datagram. Leases retire only when source harvests the ACK
//!   and calls `on_ack` (or by PTO); a lost ACK is covered by a later one
//!   carrying the same ranges, or the PTO fires.
//!
//! A delivered egress copy counts against `max_in_flight` until its ACK timer
//! fires (timer path) or the peer receives it (datagram path). Whichever of
//! ACK and PTO comes first returns the frame to the fill ring; later ones are
//! recorded as stale timers. Retransmission is not modelled.
//!
//! Every choice is a [`WarpEvent`] in the model trace and is checked on
//! replay before it takes effect. UMEM conservation is checked after every
//! step.
use super::{
    link::{Fate, Link, LinkSpec, SplitMix64, Transmission, name_seed},
    umem::{
        self, Accounting, Datagram, FrameDesc, HostBug, LeaseDesc, RxDrop, SharedUmem, UmemError,
        UmemParams, ViewDesc,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_NICS: usize = 4;
pub const MAX_LINKS: usize = 8;
pub const MAX_FLOWS: usize = 16;
pub const MAX_WARP_EVENTS: usize = 256;
pub const MAX_FRAMES: u32 = 4096;
/// The software NIC's largest chunk (`vesper_warp_umem::MAX_FRAME_SIZE`).
pub const MAX_FRAME_BYTES: u32 = vesper_warp_umem::MAX_FRAME_SIZE;
pub const MAX_ACK_RANGES: u32 = 64;
pub const MAX_ACK_EVERY: u32 = 1024;
pub const MAX_FLOW_PACKETS: u64 = 1 << 20;
pub const MAX_IN_FLIGHT: u64 = 1 << 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarpSpec {
    pub limits: WarpLimits,
    pub nics: Vec<NicSpec>,
    pub links: Vec<LinkSpec>,
    #[serde(default)]
    pub flows: Vec<FlowSpec>,
    #[serde(default)]
    pub events: Vec<TimedWarpEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarpLimits {
    /// Datagrams in flight on all links together.
    pub max_in_flight: u64,
    /// Leases (submitted, not yet retired) per NIC.
    pub max_leases: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NicSpec {
    pub name: String,
    pub process: String,
    pub frames: u32,
    pub frame_bytes: u32,
    pub headroom: u32,
    pub rx_ring: u32,
    pub ingress: String,
    pub egress: String,
    /// Timer ACK path only: egress arrival to ACK.
    pub ack_return_ns: u64,
    pub pto_ns: u64,
    /// The datagram ACK path; absent means the timer ACK path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_path: Option<AckPathSpec>,
}

/// Peers acknowledge replies with ACK datagrams over a lossy link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckPathSpec {
    /// The link ACK datagrams cross from the peer to this NIC.
    pub link: String,
    /// ACK after this many ack-eliciting packets (RFC 9000 §13.2.2 uses 2).
    pub ack_every: u32,
    /// Or this long after the first unacknowledged one.
    pub max_ack_delay_ns: u64,
    /// Ranges per ACK frame, most recent first.
    pub max_ranges: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    pub name: String,
    pub nic: String,
    pub cid: u64,
    pub src_addr: u32,
    pub src_port: u32,
    pub start_ns: u64,
    pub bursts: u64,
    /// Datagrams sent back to back in each burst.
    pub burst_packets: u32,
    pub burst_interval_ns: u64,
    pub payload_bytes: u32,
    pub first_pn: u64,
    #[serde(default)]
    pub malformed_ppm: u32,
    #[serde(default)]
    pub unknown_cid_ppm: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimedWarpEvent {
    pub at_ns: u64,
    pub event: WarpFault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WarpFault {
    /// The flow's NAT binding changes: later datagrams carry this source.
    NatRebind { flow: String, addr: u32, port: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketKind {
    Valid,
    UnknownCid,
    Truncated,
    LongHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseTimer {
    /// The timer ACK path.
    Ack,
    Pto,
    /// Source harvested an ACK datagram and called `on_ack`.
    SourceAck,
}

/// One Warp model transition or choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WarpEvent {
    Emitted {
        flow: String,
        packet: u64,
        pkt_num: u64,
        kind: PacketKind,
        src_addr: u32,
        src_port: u32,
        len: u32,
        at_ns: u64,
    },
    Transmitted {
        link: String,
        packet: u64,
        duplicate_of: Option<u64>,
        transmission: Transmission,
        at_ns: u64,
    },
    RxArrived {
        nic: String,
        packet: u64,
        chunk: u32,
        at_ns: u64,
    },
    RxDropped {
        nic: String,
        packet: u64,
        reason: RxDrop,
        at_ns: u64,
    },
    Harvested {
        nic: String,
        task: String,
        chunk: u32,
        packet: u64,
        at_ns: u64,
    },
    Recycled {
        nic: String,
        task: String,
        chunk: u32,
        at_ns: u64,
    },
    TurnedAround {
        nic: String,
        task: String,
        chunk: u32,
        tx_len: u32,
        at_ns: u64,
    },
    Submitted {
        nic: String,
        task: String,
        lease: u64,
        chunk: u32,
        pkt_num_full: u64,
        edt_ns: u64,
        launch_ns: u64,
        pto_at_ns: u64,
        at_ns: u64,
    },
    /// The reply was posted to the TX ring, transmitted and completed, and
    /// entered the egress link; one timer ACK per delivered copy, or (on the
    /// datagram ACK path) one peer arrival per delivered copy.
    Launched {
        nic: String,
        lease: u64,
        ack_at_ns: Vec<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        peer_at_ns: Vec<u64>,
        at_ns: u64,
    },
    /// A copy of a reply reached the flow's peer.
    PeerReceived {
        flow: String,
        pkt_num: u64,
        at_ns: u64,
    },
    /// The peer sent an ACK datagram for these inclusive ranges.
    AckSent {
        flow: String,
        packet: u64,
        pkt_num: u64,
        ranges: Vec<(u64, u64)>,
        len: u32,
        at_ns: u64,
    },
    /// Source passed a harvested datagram to `on_ack`.
    AckObserved {
        nic: String,
        task: String,
        chunk: u32,
        /// `Err` code when it was not a valid ACK.
        result: Result<(u64, u64), u32>,
        at_ns: u64,
    },
    Parked {
        nic: String,
        task: String,
        lease: u64,
        at_ns: u64,
    },
    Retired {
        nic: String,
        lease: u64,
        chunk: u32,
        by: LeaseTimer,
        at_ns: u64,
    },
    StaleTimer {
        nic: String,
        lease: u64,
        timer: LeaseTimer,
        at_ns: u64,
    },
    RxWaiting {
        nic: String,
        task: String,
        request: u64,
        timeout_at_ns: u64,
        at_ns: u64,
    },
    RxWoken {
        nic: String,
        request: u64,
        rx_len: u32,
        at_ns: u64,
    },
    RxWaitTimedOut {
        nic: String,
        request: u64,
        at_ns: u64,
    },
    RxWaitStale {
        nic: String,
        request: u64,
        at_ns: u64,
    },
    NatRebound {
        flow: String,
        from: (u32, u32),
        to: (u32, u32),
        at_ns: u64,
    },
}

/// A Warp model-internal timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WarpTimer {
    Burst {
        flow: usize,
        burst: u64,
    },
    Arrive {
        packet: u64,
    },
    Fault(usize),
    Launch {
        nic: String,
        lease: u64,
    },
    Lease {
        nic: String,
        lease: u64,
        timer: LeaseTimer,
    },
    WaitTimeout {
        nic: String,
        request: u64,
    },
    /// A delivered reply copy reaches its flow's peer (datagram ACK path).
    PeerRecv {
        flow: usize,
        pkt_num: u64,
    },
    /// The peer's `max_ack_delay` expires.
    AckDelay {
        flow: usize,
        generation: u64,
    },
}
impl WarpTimer {
    pub fn describe(&self) -> String {
        match self {
            Self::Burst { flow, burst } => format!("warp:flow#{flow}:burst#{burst}"),
            Self::Arrive { packet } => format!("warp:arrive#{packet}"),
            Self::Fault(i) => format!("warp:event#{i}"),
            Self::Launch { nic, lease } => format!("warp:{nic}:launch#{lease}"),
            Self::Lease { nic, lease, timer } => format!("warp:{nic}:{timer:?}#{lease}"),
            Self::WaitTimeout { nic, request } => format!("warp:{nic}:wait#{request}"),
            Self::PeerRecv { flow, pkt_num } => format!("warp:flow#{flow}:peer#{pkt_num}"),
            Self::AckDelay { flow, generation } => {
                format!("warp:flow#{flow}:ack_delay#{generation}")
            }
        }
    }
}

/// The `XdpNic` (and helper) operations a binding may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpOp {
    Harvest,
    View,
    Recycle,
    TurnAround,
    Submit,
    Park,
    SplitShortHeader,
    WaitRx,
    OnAck,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarpError {
    /// A forged, stale or misused value; the operation does not happen.
    Misuse(String),
    /// A budget would be exceeded.
    Budget(String),
    /// A frame is missing or duplicated: the host binding lost track of it.
    Conservation(String),
}

/// What a timer or operation asks the session to do after committing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WarpEffect {
    Spawn(WarpTimer, u64),
    /// Complete the parked `wait_rx` request with this RX occupancy.
    Wake {
        request: u64,
        rx_len: u32,
    },
}

pub(crate) struct OpResult {
    pub events: Vec<WarpEvent>,
    pub effects: Vec<WarpEffect>,
    /// `Some(response)` resumes at once; `None` parks on `park`.
    pub response: Option<Json>,
    pub park: Option<(WarpTimer, u64)>,
}

#[derive(Debug, Clone)]
pub(crate) struct NicState {
    /// Shared by every copy of the model state; see [`SharedUmem`].
    pub umem: SharedUmem,
    pub process: usize,
    pub ingress: String,
    pub egress: String,
    pub ack_return_ns: u64,
    pub pto_ns: u64,
    pub ack_path: Option<AckPathSpec>,
    pub waiters: VecDeque<u64>,
}
/// A flow's peer on the datagram ACK path.
#[derive(Debug, Clone, Default)]
pub(crate) struct PeerState {
    /// Received reply packet numbers as inclusive ranges, largest first.
    pub ranges: Vec<(u64, u64)>,
    /// Ack-eliciting packets since the last ACK.
    pub unacked: u32,
    /// When the oldest unacknowledged one arrived.
    pub first_unacked_ns: u64,
    /// The `max_ack_delay` timer generation, when armed.
    pub armed: Option<u64>,
    pub generation: u64,
}
#[derive(Debug, Clone)]
pub(crate) struct FlowState {
    pub rng: SplitMix64,
    pub src: (u32, u32),
    pub next_pn: u64,
}
#[derive(Debug, Clone)]
pub(crate) struct WarpState {
    pub nics: BTreeMap<String, NicState>,
    pub links: BTreeMap<String, Link>,
    pub flows: Vec<FlowState>,
    pub peers: Vec<PeerState>,
    /// Ingress datagrams in flight, with their NIC.
    pub in_flight: BTreeMap<u64, (String, Datagram)>,
    pub next_packet: u64,
    pub max_in_flight: u64,
    /// Delivered egress copies whose ACK has not fired yet.
    pub egress_in_flight: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicSnapshot {
    pub process: String,
    pub accounting: Accounting,
    pub leases: u64,
    pub waiting: Vec<u64>,
    /// Counters of the software NIC and its driver.
    pub softnic: SoftNicCounters,
}

/// What the `vesper-warp-umem` device and driver counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftNicCounters {
    pub rx_delivered: u64,
    pub rx_dropped_no_buffer: u64,
    pub rx_dropped_ring_full: u64,
    pub tx_consumed: u64,
    pub completions_posted: u64,
    pub harvested: u64,
    pub tx_submitted: u64,
    pub completions: u64,
    pub retired: u64,
    pub leaks_detected: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarpSnapshot {
    pub nics: BTreeMap<String, NicSnapshot>,
    pub in_flight: u64,
    /// The Gilbert–Elliott state of each link.
    pub links_bad: BTreeMap<String, bool>,
}

fn fail<T>(message: impl Into<String>) -> Result<T, String> {
    Err(message.into())
}

/// A typed UMEM error: misuse by source, a budget, or (when the driver
/// refuses a call the adapter validated) a conservation failure.
fn umem_error(nic: &str, e: UmemError) -> WarpError {
    match e {
        UmemError::LeaseLimit { .. } => WarpError::Budget(format!("{nic}: {e:?}")),
        UmemError::Driver(_) => WarpError::Conservation(format!("{nic}: {e:?}")),
        e => WarpError::Misuse(format!("{nic}: {e:?}")),
    }
}

/// Check the Warp section. `crashing` names processes with crash faults.
pub(crate) fn validate(
    spec: &WarpSpec,
    process_index: &BTreeMap<String, usize>,
    crashing: &BTreeSet<String>,
    max_virtual_ns: u64,
) -> Result<(), String> {
    let l = &spec.limits;
    if l.max_in_flight == 0
        || l.max_in_flight > MAX_IN_FLIGHT
        || l.max_leases == 0
        || l.max_leases > u64::from(MAX_FRAMES)
    {
        return fail(format!(
            "warp limits must be positive, in-flight <= {MAX_IN_FLIGHT}, leases <= {MAX_FRAMES}"
        ));
    }
    if spec.nics.is_empty()
        || spec.nics.len() > MAX_NICS
        || spec.links.len() > MAX_LINKS
        || spec.flows.len() > MAX_FLOWS
        || spec.events.len() > MAX_WARP_EVENTS
    {
        return fail("warp needs 1..=4 NICs and at most 8 links, 16 flows, 256 events");
    }
    let mut links = BTreeSet::new();
    for link in &spec.links {
        if link.name.is_empty() || !links.insert(link.name.as_str()) {
            return fail(format!("link names must be unique: {:?}", link.name));
        }
        let ppm = [
            link.loss_ppm,
            link.duplicate_ppm,
            link.reorder.map_or(0, |r| r.ppm),
        ];
        let ge = link.gilbert_elliott.map_or([0; 4], |g| {
            [
                g.good_to_bad_ppm,
                g.bad_to_good_ppm,
                g.loss_good_ppm,
                g.loss_bad_ppm,
            ]
        });
        if ppm.iter().chain(&ge).any(|p| *p > super::link::PPM) {
            return fail(format!("link {}: probabilities are ppm <= 10^6", link.name));
        }
        if link.bottleneck.buffer_packets == 0 || link.bottleneck.rate_bytes_per_sec == 0 {
            return fail(format!(
                "link {}: the bottleneck needs a positive buffer and rate",
                link.name
            ));
        }
    }
    let mut nics: BTreeMap<&str, &NicSpec> = BTreeMap::new();
    for nic in &spec.nics {
        if nic.name.is_empty() || nics.insert(&nic.name, nic).is_some() {
            return fail(format!("NIC names must be unique: {:?}", nic.name));
        }
        if !process_index.contains_key(&nic.process) {
            return fail(format!(
                "NIC {} names unknown process {}",
                nic.name, nic.process
            ));
        }
        if crashing.contains(&nic.process) {
            return fail(format!(
                "umem-softnic-v1 does not model a crash of NIC-owning process {}",
                nic.process
            ));
        }
        if nic.frames == 0 || nic.frames > MAX_FRAMES || nic.pto_ns == 0 {
            return fail(format!(
                "NIC {}: frames 1..={MAX_FRAMES}, pto > 0",
                nic.name
            ));
        }
        params(nic, l.max_leases)
            .validate()
            .map_err(|e| format!("NIC {}: {e}", nic.name))?;
        for link in [&nic.ingress, &nic.egress] {
            if !links.contains(link.as_str()) {
                return fail(format!("NIC {} names unknown link {link}", nic.name));
            }
        }
        if let Some(ack) = &nic.ack_path {
            if !links.contains(ack.link.as_str()) {
                return fail(format!(
                    "NIC {} names unknown ACK link {}",
                    nic.name, ack.link
                ));
            }
            if ack.ack_every == 0
                || ack.ack_every > MAX_ACK_EVERY
                || ack.max_ack_delay_ns == 0
                || ack.max_ranges == 0
                || ack.max_ranges > MAX_ACK_RANGES
            {
                return fail(format!(
                    "NIC {}: ack_every 1..={MAX_ACK_EVERY}, max_ack_delay_ns > 0, max_ranges 1..={MAX_ACK_RANGES}",
                    nic.name
                ));
            }
            if ack_datagram_len(ack.max_ranges) > nic.frame_bytes - nic.headroom {
                return fail(format!(
                    "NIC {}: an ACK datagram with {} ranges must fit a frame after the headroom",
                    nic.name, ack.max_ranges
                ));
            }
        }
    }
    for nic in spec.nics.iter().filter(|n| n.ack_path.is_some()) {
        let mut cids = BTreeSet::new();
        for flow in spec.flows.iter().filter(|f| f.nic == nic.name) {
            if !cids.insert(flow.cid) {
                return fail(format!(
                    "NIC {}: flows on a datagram ACK path need distinct connection IDs",
                    nic.name
                ));
            }
        }
    }
    let mut flows = BTreeSet::new();
    let mut packets = 0u64;
    for flow in &spec.flows {
        if flow.name.is_empty() || !flows.insert(flow.name.as_str()) {
            return fail(format!("flow names must be unique: {:?}", flow.name));
        }
        let nic = nics
            .get(flow.nic.as_str())
            .ok_or_else(|| format!("flow {} names unknown NIC {}", flow.name, flow.nic))?;
        if flow.malformed_ppm > super::link::PPM || flow.unknown_cid_ppm > super::link::PPM {
            return fail(format!("flow {}: probabilities are ppm <= 10^6", flow.name));
        }
        let len = flow
            .payload_bytes
            .checked_add(umem::SHORT_HEADER_BYTES)
            .filter(|len| *len <= nic.frame_bytes - nic.headroom);
        if len.is_none() || flow.bursts == 0 || flow.burst_packets == 0 {
            return fail(format!(
                "flow {}: needs bursts and packets, and datagrams must fit a frame after the headroom",
                flow.name
            ));
        }
        let count = flow
            .bursts
            .checked_mul(u64::from(flow.burst_packets))
            .ok_or("flow packet count overflows")?;
        packets = packets
            .checked_add(count)
            .ok_or("flow packet count overflows")?;
        let last = (flow.bursts - 1)
            .checked_mul(flow.burst_interval_ns)
            .and_then(|t| t.checked_add(flow.start_ns));
        if last.is_none_or(|t| t > max_virtual_ns) {
            return fail(format!(
                "flow {}: its last burst exceeds max_virtual_ns",
                flow.name
            ));
        }
        if flow
            .first_pn
            .checked_add(count)
            .is_none_or(|end| end > umem::MAX_PACKET_NUMBER)
        {
            return fail(format!(
                "flow {}: packet numbers must stay below 2^62",
                flow.name
            ));
        }
    }
    if packets > MAX_FLOW_PACKETS {
        return fail(format!(
            "flows emit {packets} datagrams, more than {MAX_FLOW_PACKETS}"
        ));
    }
    for event in &spec.events {
        if event.at_ns > max_virtual_ns {
            return fail(format!(
                "warp event at {} ns exceeds max_virtual_ns",
                event.at_ns
            ));
        }
        let WarpFault::NatRebind { flow, .. } = &event.event;
        if !flows.contains(flow.as_str()) {
            return fail(format!("NAT rebinding names unknown flow {flow}"));
        }
    }
    Ok(())
}

fn params(nic: &NicSpec, max_leases: u64) -> UmemParams {
    UmemParams {
        frames: nic.frames,
        frame_bytes: nic.frame_bytes,
        headroom: nic.headroom,
        rx_ring: nic.rx_ring,
        max_leases,
    }
}

/// Bytes of an ACK datagram carrying `ranges` ranges.
pub fn ack_datagram_len(ranges: u32) -> u32 {
    umem::SHORT_HEADER_BYTES
        + umem::ACK_FRAME_FIXED_BYTES
        + umem::ACK_RANGE_BYTES * ranges.saturating_sub(1)
}

/// Add `pn` to inclusive ranges kept largest first, merging neighbours, and
/// keep at most `max` ranges (the oldest are forgotten).
fn record_pn(ranges: &mut Vec<(u64, u64)>, pn: u64, max: usize) {
    if ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&pn)) {
        return;
    }
    ranges.push((pn, pn));
    ranges.sort_by_key(|r| std::cmp::Reverse(r.1));
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges.drain(..) {
        match out.last_mut() {
            // Packet numbers are below 2^62, so `hi + 1` cannot overflow.
            Some(last) if hi + 1 >= last.0 => last.0 = last.0.min(lo),
            _ => out.push((lo, hi)),
        }
    }
    out.truncate(max);
    *ranges = out;
}

impl WarpState {
    pub fn new(spec: &WarpSpec, process_index: &BTreeMap<String, usize>, seed: u64) -> Self {
        Self {
            nics: spec
                .nics
                .iter()
                .map(|n| {
                    (
                        n.name.clone(),
                        NicState {
                            umem: SharedUmem::new(params(n, spec.limits.max_leases)),
                            process: process_index[&n.process],
                            ingress: n.ingress.clone(),
                            egress: n.egress.clone(),
                            ack_return_ns: n.ack_return_ns,
                            pto_ns: n.pto_ns,
                            ack_path: n.ack_path.clone(),
                            waiters: VecDeque::new(),
                        },
                    )
                })
                .collect(),
            links: spec
                .links
                .iter()
                .map(|l| (l.name.clone(), Link::new(l.clone(), seed)))
                .collect(),
            flows: spec
                .flows
                .iter()
                .map(|f| FlowState {
                    rng: SplitMix64::new(name_seed(seed ^ 0x5741_5250, &f.name)),
                    src: (f.src_addr, f.src_port),
                    next_pn: f.first_pn,
                })
                .collect(),
            peers: vec![PeerState::default(); spec.flows.len()],
            in_flight: BTreeMap::new(),
            next_packet: 1,
            max_in_flight: spec.limits.max_in_flight,
            egress_in_flight: 0,
        }
    }

    pub fn set_bug(&mut self, bug: Option<HostBug>) {
        for nic in self.nics.values_mut() {
            nic.umem.set_bug(bug);
        }
    }

    /// The step that changed the shared NICs is committed.
    pub fn commit(&self) {
        for nic in self.nics.values() {
            nic.umem.commit();
        }
    }
    /// The step failed: undo its NIC calls (see [`SharedUmem`]).
    pub fn rollback(&self) {
        for nic in self.nics.values() {
            nic.umem.rollback();
        }
    }
    /// `task` finished holding frames: drop their owners (the audit then
    /// reports the leak) and return the conservation error.
    pub fn leak_task(&self, task: &str) -> Result<(), WarpError> {
        for nic in self.nics.values() {
            nic.umem.leak_task(task);
        }
        self.check_conservation()
    }

    /// Initial timers: the first burst of every flow and every timed event.
    pub fn initial_timers(spec: &WarpSpec) -> Vec<(WarpTimer, u64)> {
        let mut timers: Vec<_> = spec
            .flows
            .iter()
            .enumerate()
            .map(|(flow, f)| (WarpTimer::Burst { flow, burst: 0 }, f.start_ns))
            .collect();
        timers.extend(
            spec.events
                .iter()
                .enumerate()
                .map(|(i, e)| (WarpTimer::Fault(i), e.at_ns)),
        );
        timers
    }

    pub fn snapshot(&self, process_names: &[String]) -> WarpSnapshot {
        WarpSnapshot {
            nics: self
                .nics
                .iter()
                .map(|(name, n)| {
                    (
                        name.clone(),
                        NicSnapshot {
                            process: process_names[n.process].clone(),
                            accounting: n.umem.accounting(),
                            leases: n.umem.lease_count() as u64,
                            waiting: n.waiters.iter().copied().collect(),
                            softnic: n.umem.with(|u| {
                                let (nic, drv) = (u.nic_stats(), u.driver_stats());
                                SoftNicCounters {
                                    rx_delivered: nic.rx_delivered,
                                    rx_dropped_no_buffer: nic.rx_dropped_no_buffer,
                                    rx_dropped_ring_full: nic.rx_dropped_ring_full,
                                    tx_consumed: nic.tx_consumed,
                                    completions_posted: nic.completions_posted,
                                    harvested: drv.rx_harvested,
                                    tx_submitted: drv.tx_submitted,
                                    completions: drv.completions,
                                    retired: drv.retired,
                                    leaks_detected: drv.leaks_detected,
                                }
                            }),
                        },
                    )
                })
                .collect(),
            in_flight: self.in_flight.len() as u64 + self.egress_in_flight,
            links_bad: self
                .links
                .iter()
                .map(|(n, l)| (n.clone(), l.in_bad_state()))
                .collect(),
        }
    }

    /// Frames `task` still holds on any NIC.
    pub fn held_by(&self, task: &str) -> Vec<(String, u32)> {
        self.nics
            .iter()
            .flat_map(|(n, s)| s.umem.held_by(task).into_iter().map(|c| (n.clone(), c)))
            .collect()
    }

    pub fn check_conservation(&self) -> Result<(), WarpError> {
        for (name, nic) in &self.nics {
            nic.umem
                .check_conservation()
                .map_err(|e| WarpError::Conservation(format!("{name}: {e:?}")))?;
        }
        Ok(())
    }

    /// Transmit one datagram on a link: draw duplication, then the fate of
    /// the original and of any copy. Returns the transmissions with their
    /// packet ids (the copy gets a fresh id).
    fn send_on_link(
        &mut self,
        link: &str,
        packet: u64,
        now_ns: u64,
        at_ns: u64,
        len: u32,
        events: &mut Vec<WarpEvent>,
    ) -> Result<Vec<(u64, Transmission)>, WarpError> {
        let state = self.links.get_mut(link).expect("validated link");
        let duplicate = state.duplicates();
        let overflow = |_| WarpError::Budget(format!("{link}: virtual clock overflow"));
        let mut out = vec![(
            packet,
            state.transmit(now_ns, at_ns, len).map_err(overflow)?,
        )];
        if duplicate {
            let copy = self.next_packet;
            self.next_packet += 1;
            let state = self.links.get_mut(link).expect("validated link");
            out.push((copy, state.transmit(now_ns, at_ns, len).map_err(overflow)?));
        }
        for (id, transmission) in &out {
            events.push(WarpEvent::Transmitted {
                link: link.into(),
                packet: *id,
                duplicate_of: (*id != packet).then_some(packet),
                transmission: *transmission,
                at_ns,
            });
        }
        Ok(out)
    }

    /// Admit the delivered transmissions of an ingress datagram: each copy
    /// is in flight until it arrives at `nic`. Checked against the budget
    /// before anything is retained.
    fn admit(
        &mut self,
        nic: &str,
        datagram: Datagram,
        sent: Vec<(u64, Transmission)>,
        effects: &mut Vec<WarpEffect>,
    ) -> Result<(), WarpError> {
        for (id, t) in sent {
            if let Fate::Delivered { arrive_at_ns, .. } = t.fate {
                if self.in_flight.len() as u64 + self.egress_in_flight >= self.max_in_flight {
                    return Err(WarpError::Budget(format!(
                        "more than {} datagrams in flight",
                        self.max_in_flight
                    )));
                }
                let copy = Datagram {
                    id,
                    ..datagram.clone()
                };
                self.in_flight.insert(id, (nic.into(), copy));
                effects.push(WarpEffect::Spawn(
                    WarpTimer::Arrive { packet: id },
                    arrive_at_ns,
                ));
            }
        }
        Ok(())
    }

    /// The peer of `flow` sends an ACK datagram for its recorded ranges over
    /// the NIC's ACK link. The ACK uses the flow's packet-number space.
    fn send_ack(
        &mut self,
        spec: &WarpSpec,
        flow: usize,
        now_ns: u64,
        events: &mut Vec<WarpEvent>,
        effects: &mut Vec<WarpEffect>,
    ) -> Result<(), WarpError> {
        let f = &spec.flows[flow];
        let link = self.nics[&f.nic]
            .ack_path
            .as_ref()
            .expect("ACKs imply an ACK path")
            .link
            .clone();
        let peer = &mut self.peers[flow];
        peer.unacked = 0;
        peer.armed = None;
        let ranges = peer.ranges.clone();
        let delay = now_ns.saturating_sub(peer.first_unacked_ns);
        let state = &mut self.flows[flow];
        let pkt_num = state.next_pn;
        if pkt_num > umem::MAX_PACKET_NUMBER {
            return Err(WarpError::Budget(format!(
                "flow {}: packet numbers exhausted",
                f.name
            )));
        }
        state.next_pn += 1;
        let src = state.src;
        let mut head = umem::short_header(0x43, f.cid, pkt_num);
        head.extend(umem::ack_frame_bytes(&ranges, delay));
        let len = head.len() as u32;
        let packet = self.next_packet;
        self.next_packet += 1;
        events.push(WarpEvent::AckSent {
            flow: f.name.clone(),
            packet,
            pkt_num,
            ranges,
            len,
            at_ns: now_ns,
        });
        let sent = self.send_on_link(&link, packet, now_ns, now_ns, len, events)?;
        let datagram = Datagram {
            id: packet,
            src_addr: src.0,
            src_port: src.1,
            len,
            head_hex: umem::hex(&head),
            flow: Some(flow as u32),
        };
        self.admit(&f.nic, datagram, sent, effects)
    }

    /// A Warp timer fired.
    pub fn fire(
        &mut self,
        spec: &WarpSpec,
        timer: &WarpTimer,
        now_ns: u64,
    ) -> Result<(Vec<WarpEvent>, Vec<WarpEffect>), WarpError> {
        let mut events = vec![];
        let mut effects = vec![];
        match timer {
            WarpTimer::Burst { flow, burst } => {
                let f = &spec.flows[*flow];
                for _ in 0..f.burst_packets {
                    let state = &mut self.flows[*flow];
                    let pkt_num = state.next_pn;
                    state.next_pn += 1;
                    let malformed = state.rng.chance(f.malformed_ppm);
                    let which = state.rng.next_u64();
                    let unknown = state.rng.chance(f.unknown_cid_ppm);
                    let (kind, first, dcid) = if malformed {
                        if which.is_multiple_of(2) {
                            (PacketKind::Truncated, 0x43, f.cid)
                        } else {
                            (PacketKind::LongHeader, 0xc3, f.cid)
                        }
                    } else if unknown {
                        let mut other = which;
                        if other == f.cid {
                            other ^= 1;
                        }
                        (PacketKind::UnknownCid, 0x43, other)
                    } else {
                        (PacketKind::Valid, 0x43, f.cid)
                    };
                    let mut head = umem::short_header(first, dcid, pkt_num);
                    let len = if kind == PacketKind::Truncated {
                        head.truncate(7);
                        7
                    } else {
                        f.payload_bytes + umem::SHORT_HEADER_BYTES
                    };
                    let packet = self.next_packet;
                    self.next_packet += 1;
                    let src = state.src;
                    events.push(WarpEvent::Emitted {
                        flow: f.name.clone(),
                        packet,
                        pkt_num,
                        kind,
                        src_addr: src.0,
                        src_port: src.1,
                        len,
                        at_ns: now_ns,
                    });
                    let ingress = self.nics[&f.nic].ingress.clone();
                    let sent =
                        self.send_on_link(&ingress, packet, now_ns, now_ns, len, &mut events)?;
                    let datagram = Datagram {
                        id: packet,
                        src_addr: src.0,
                        src_port: src.1,
                        len,
                        head_hex: umem::hex(&head),
                        flow: Some(*flow as u32),
                    };
                    self.admit(&f.nic, datagram, sent, &mut effects)?;
                }
                if burst + 1 < f.bursts {
                    let next = f.start_ns + (burst + 1) * f.burst_interval_ns;
                    effects.push(WarpEffect::Spawn(
                        WarpTimer::Burst {
                            flow: *flow,
                            burst: burst + 1,
                        },
                        next,
                    ));
                }
            }
            WarpTimer::Arrive { packet } => {
                let (nic, datagram) = self
                    .in_flight
                    .remove(packet)
                    .expect("arrival timers name in-flight datagrams");
                let state = self.nics.get_mut(&nic).expect("validated NIC");
                match state
                    .umem
                    .arrive(datagram)
                    .map_err(|e| umem_error(&nic, e))?
                {
                    Ok(chunk) => {
                        events.push(WarpEvent::RxArrived {
                            nic: nic.clone(),
                            packet: *packet,
                            chunk,
                            at_ns: now_ns,
                        });
                        if let Some(request) = state.waiters.pop_front() {
                            let rx_len = state.umem.rx_len();
                            events.push(WarpEvent::RxWoken {
                                nic,
                                request,
                                rx_len,
                                at_ns: now_ns,
                            });
                            effects.push(WarpEffect::Wake { request, rx_len });
                        }
                    }
                    Err(reason) => events.push(WarpEvent::RxDropped {
                        nic,
                        packet: *packet,
                        reason,
                        at_ns: now_ns,
                    }),
                }
            }
            WarpTimer::Fault(index) => {
                let WarpFault::NatRebind { flow, addr, port } = &spec.events[*index].event;
                let i = spec
                    .flows
                    .iter()
                    .position(|f| f.name == *flow)
                    .expect("validated flow");
                let from = self.flows[i].src;
                self.flows[i].src = (*addr, *port);
                events.push(WarpEvent::NatRebound {
                    flow: flow.clone(),
                    from,
                    to: (*addr, *port),
                    at_ns: now_ns,
                });
            }
            WarpTimer::Launch { nic, lease } => {
                let state = &self.nics[nic];
                let (egress, ack_return) = (state.egress.clone(), state.ack_return_ns);
                let datagram_acks = state.ack_path.is_some();
                let l = state
                    .umem
                    .lease(*lease)
                    .expect("a lease launches before its PTO");
                let parked = l.state == umem::LeaseState::Parked;
                let packet = self.next_packet;
                self.next_packet += 1;
                let sent =
                    self.send_on_link(&egress, packet, now_ns, now_ns, l.tx_len, &mut events)?;
                let arrivals: Vec<u64> = sent
                    .iter()
                    .filter_map(|(_, t)| match t.fate {
                        Fate::Delivered { arrive_at_ns, .. } => Some(arrive_at_ns),
                        _ => None,
                    })
                    .collect();
                let outstanding = self.in_flight.len() as u64 + self.egress_in_flight;
                if outstanding + arrivals.len() as u64 > self.max_in_flight {
                    return Err(WarpError::Budget(format!(
                        "more than {} datagrams in flight",
                        self.max_in_flight
                    )));
                }
                self.egress_in_flight += arrivals.len() as u64;
                let (ack_at_ns, peer_at_ns) = if datagram_acks {
                    let flow = l.flow.expect("every datagram comes from a flow") as usize;
                    for at in &arrivals {
                        effects.push(WarpEffect::Spawn(
                            WarpTimer::PeerRecv {
                                flow,
                                pkt_num: l.pkt_num_full,
                            },
                            *at,
                        ));
                    }
                    (vec![], arrivals)
                } else {
                    let mut acks = vec![];
                    for at in arrivals {
                        acks.push(at.checked_add(ack_return).ok_or_else(|| {
                            WarpError::Budget(format!("{nic}: ACK time overflows"))
                        })?);
                    }
                    if parked {
                        for at in &acks {
                            effects.push(WarpEffect::Spawn(
                                WarpTimer::Lease {
                                    nic: nic.clone(),
                                    lease: *lease,
                                    timer: LeaseTimer::Ack,
                                },
                                *at,
                            ));
                        }
                    }
                    (acks, vec![])
                };
                let state = &self.nics[nic];
                state
                    .umem
                    .launch(*lease, ack_at_ns.clone())
                    .map_err(|e| umem_error(nic, e))?;
                events.push(WarpEvent::Launched {
                    nic: nic.clone(),
                    lease: *lease,
                    ack_at_ns,
                    peer_at_ns,
                    at_ns: now_ns,
                });
            }
            WarpTimer::PeerRecv { flow, pkt_num } => {
                // This copy reached the peer: it no longer counts as in flight.
                self.egress_in_flight = self
                    .egress_in_flight
                    .checked_sub(1)
                    .expect("each delivered egress copy has one peer arrival");
                let f = &spec.flows[*flow];
                let ack = self.nics[&f.nic]
                    .ack_path
                    .clone()
                    .expect("peer timers imply an ACK path");
                let peer = &mut self.peers[*flow];
                record_pn(&mut peer.ranges, *pkt_num, ack.max_ranges as usize);
                if peer.unacked == 0 {
                    peer.first_unacked_ns = now_ns;
                }
                peer.unacked += 1;
                events.push(WarpEvent::PeerReceived {
                    flow: f.name.clone(),
                    pkt_num: *pkt_num,
                    at_ns: now_ns,
                });
                if peer.unacked >= ack.ack_every {
                    self.send_ack(spec, *flow, now_ns, &mut events, &mut effects)?;
                } else if peer.armed.is_none() {
                    peer.generation += 1;
                    peer.armed = Some(peer.generation);
                    let at = now_ns.checked_add(ack.max_ack_delay_ns).ok_or_else(|| {
                        WarpError::Budget(format!("{}: ACK delay overflows", f.name))
                    })?;
                    effects.push(WarpEffect::Spawn(
                        WarpTimer::AckDelay {
                            flow: *flow,
                            generation: peer.generation,
                        },
                        at,
                    ));
                }
            }
            WarpTimer::AckDelay { flow, generation } => {
                let peer = &self.peers[*flow];
                if peer.armed == Some(*generation) && peer.unacked > 0 {
                    self.send_ack(spec, *flow, now_ns, &mut events, &mut effects)?;
                }
            }
            WarpTimer::Lease { nic, lease, timer } => {
                if *timer == LeaseTimer::Ack {
                    // This copy's ACK arrived: it no longer counts as in flight.
                    self.egress_in_flight = self
                        .egress_in_flight
                        .checked_sub(1)
                        .expect("each delivered egress copy has one ACK timer");
                }
                let state = self.nics.get_mut(nic).expect("validated NIC");
                match state.umem.retire(*lease).map_err(|e| umem_error(nic, e))? {
                    Some(l) => events.push(WarpEvent::Retired {
                        nic: nic.clone(),
                        lease: *lease,
                        chunk: l.chunk,
                        by: *timer,
                        at_ns: now_ns,
                    }),
                    None => events.push(WarpEvent::StaleTimer {
                        nic: nic.clone(),
                        lease: *lease,
                        timer: *timer,
                        at_ns: now_ns,
                    }),
                }
            }
            WarpTimer::WaitTimeout { nic, request } => {
                let state = self.nics.get_mut(nic).expect("validated NIC");
                match state.waiters.iter().position(|r| r == request) {
                    Some(i) => {
                        state.waiters.remove(i);
                        events.push(WarpEvent::RxWaitTimedOut {
                            nic: nic.clone(),
                            request: *request,
                            at_ns: now_ns,
                        });
                        effects.push(WarpEffect::Wake {
                            request: *request,
                            rx_len: 0,
                        });
                    }
                    None => events.push(WarpEvent::RxWaitStale {
                        nic: nic.clone(),
                        request: *request,
                        at_ns: now_ns,
                    }),
                }
            }
        }
        self.check_conservation()?;
        Ok((events, effects))
    }

    /// One source operation on `nic` by `task`. `request` is the id a parked
    /// `wait_rx` uses.
    pub fn op(
        &mut self,
        nic: &str,
        op: WarpOp,
        task: &str,
        args: &[Json],
        now_ns: u64,
        request: u64,
    ) -> Result<OpResult, WarpError> {
        let misuse = |e: UmemError| umem_error(nic, e);
        let arg = |i: usize| -> Result<Json, WarpError> {
            args.get(i)
                .cloned()
                .ok_or_else(|| WarpError::Misuse(format!("{nic}: missing argument {i}")))
        };
        let parse = |json: Json| -> Result<FrameDesc, WarpError> {
            serde_json::from_value(json)
                .map_err(|e| WarpError::Misuse(format!("{nic}: frame: {e}")))
        };
        let u64_arg = |i: usize| -> Result<u64, WarpError> {
            arg(i)?
                .as_u64()
                .ok_or_else(|| WarpError::Misuse(format!("{nic}: argument {i} is not a u64")))
        };
        let mut events = vec![];
        let mut effects = vec![];
        let mut park = None;
        let response = {
            let state = self.nics.get_mut(nic).expect("validated NIC");
            let umem = &state.umem;
            match op {
                WarpOp::Harvest => match umem.harvest(task).map_err(misuse)? {
                    None => Some(json!("None")),
                    Some((frame, datagram)) => {
                        events.push(WarpEvent::Harvested {
                            nic: nic.into(),
                            task: task.into(),
                            chunk: frame.chunk_idx,
                            packet: datagram.id,
                            at_ns: now_ns,
                        });
                        Some(json!({ "Some": frame }))
                    }
                },
                WarpOp::View => {
                    let view = umem.view(task, parse(arg(0)?)?).map_err(misuse)?;
                    Some(serde_json::to_value(view).expect("serializable"))
                }
                WarpOp::SplitShortHeader => {
                    let view: ViewDesc = serde_json::from_value(arg(0)?)
                        .map_err(|e| WarpError::Misuse(format!("{nic}: view: {e}")))?;
                    match umem.split_short_header(task, view).map_err(misuse)? {
                        Ok(header) => Some(json!({ "Ok": header })),
                        Err(code) => Some(json!({ "Err": code })),
                    }
                }
                WarpOp::Recycle => {
                    let frame = parse(arg(0)?)?;
                    umem.recycle(task, frame).map_err(misuse)?;
                    events.push(WarpEvent::Recycled {
                        nic: nic.into(),
                        task: task.into(),
                        chunk: frame.chunk_idx,
                        at_ns: now_ns,
                    });
                    Some(Json::Null)
                }
                WarpOp::TurnAround => {
                    let frame = parse(arg(0)?)?;
                    let tx_len = u32::try_from(u64_arg(1)?)
                        .map_err(|_| WarpError::Misuse(format!("{nic}: tx_len exceeds u32")))?;
                    let tx = umem.turn_around(task, frame, tx_len).map_err(misuse)?;
                    events.push(WarpEvent::TurnedAround {
                        nic: nic.into(),
                        task: task.into(),
                        chunk: frame.chunk_idx,
                        tx_len,
                        at_ns: now_ns,
                    });
                    Some(serde_json::to_value(tx).expect("serializable"))
                }
                WarpOp::Park => {
                    let desc: LeaseDesc = serde_json::from_value(arg(0)?)
                        .map_err(|e| WarpError::Misuse(format!("{nic}: lease: {e}")))?;
                    let (lease, retired) = umem.park(task, desc).map_err(misuse)?;
                    events.push(WarpEvent::Parked {
                        nic: nic.into(),
                        task: task.into(),
                        lease,
                        at_ns: now_ns,
                    });
                    match retired {
                        // A source-observed ACK already covered it.
                        Some(chunk) => events.push(WarpEvent::Retired {
                            nic: nic.into(),
                            lease,
                            chunk,
                            by: LeaseTimer::SourceAck,
                            at_ns: now_ns,
                        }),
                        None => {
                            let l = umem.lease(lease).expect("parked lease");
                            let launched_acks: &[u64] = if l.launched { &l.ack_at_ns } else { &[] };
                            for at in launched_acks {
                                effects.push(WarpEffect::Spawn(
                                    WarpTimer::Lease {
                                        nic: nic.into(),
                                        lease,
                                        timer: LeaseTimer::Ack,
                                    },
                                    (*at).max(now_ns),
                                ));
                            }
                            effects.push(WarpEffect::Spawn(
                                WarpTimer::Lease {
                                    nic: nic.into(),
                                    lease,
                                    timer: LeaseTimer::Pto,
                                },
                                l.pto_at_ns.max(now_ns),
                            ));
                        }
                    }
                    Some(Json::Null)
                }
                WarpOp::WaitRx => {
                    let timeout = u64_arg(0)?;
                    let rx_len = umem.rx_len();
                    if rx_len > 0 || timeout == 0 {
                        Some(json!(rx_len))
                    } else {
                        let timeout_at_ns = now_ns.checked_add(timeout).ok_or_else(|| {
                            WarpError::Budget(format!("{nic}: wait timeout overflows"))
                        })?;
                        state.waiters.push_back(request);
                        events.push(WarpEvent::RxWaiting {
                            nic: nic.into(),
                            task: task.into(),
                            request,
                            timeout_at_ns,
                            at_ns: now_ns,
                        });
                        park = Some((
                            WarpTimer::WaitTimeout {
                                nic: nic.into(),
                                request,
                            },
                            timeout_at_ns,
                        ));
                        None
                    }
                }
                WarpOp::OnAck => {
                    let view: ViewDesc = serde_json::from_value(arg(0)?)
                        .map_err(|e| WarpError::Misuse(format!("{nic}: view: {e}")))?;
                    match umem.on_ack(task, view).map_err(misuse)? {
                        Ok((summary, _, retired)) => {
                            events.push(WarpEvent::AckObserved {
                                nic: nic.into(),
                                task: task.into(),
                                chunk: view.chunk_idx,
                                result: Ok((summary.largest_acked, summary.retired)),
                                at_ns: now_ns,
                            });
                            for l in retired {
                                events.push(WarpEvent::Retired {
                                    nic: nic.into(),
                                    lease: l.id,
                                    chunk: l.chunk,
                                    by: LeaseTimer::SourceAck,
                                    at_ns: now_ns,
                                });
                            }
                            Some(json!({ "Ok": summary }))
                        }
                        Err(code) => {
                            events.push(WarpEvent::AckObserved {
                                nic: nic.into(),
                                task: task.into(),
                                chunk: view.chunk_idx,
                                result: Err(code),
                                at_ns: now_ns,
                            });
                            Some(json!({ "Err": code }))
                        }
                    }
                }
                WarpOp::Submit => None,
            }
        };
        let response = if op == WarpOp::Submit {
            let frame = parse(arg(0)?)?;
            let pkt_num_full = u64_arg(1)?;
            let edt_ns = u64_arg(2)?;
            let state = &self.nics[nic];
            state
                .umem
                .check_submit(task, frame, pkt_num_full)
                .map_err(misuse)?;
            let pto = state.pto_ns;
            let launch_ns = edt_ns.max(now_ns);
            let pto_at_ns = launch_ns
                .checked_add(pto)
                .ok_or_else(|| WarpError::Budget(format!("{nic}: PTO time overflows")))?;
            let state = &self.nics[nic];
            let (lease, desc) = state
                .umem
                .submit(task, frame, pkt_num_full, edt_ns, pto_at_ns)
                .map_err(misuse)?;
            events.push(WarpEvent::Submitted {
                nic: nic.into(),
                task: task.into(),
                lease,
                chunk: frame.chunk_idx,
                pkt_num_full,
                edt_ns,
                launch_ns,
                pto_at_ns,
                at_ns: now_ns,
            });
            // The reply enters the egress link at its launch time.
            effects.push(WarpEffect::Spawn(
                WarpTimer::Launch {
                    nic: nic.into(),
                    lease,
                },
                launch_ns,
            ));
            Some(serde_json::to_value(desc).expect("serializable"))
        } else {
            response
        };
        self.check_conservation()?;
        Ok(OpResult {
            events,
            effects,
            response,
            park,
        })
    }
}
