//! Source `XdpNic` frames backed by the Warp software NIC (`umem-softnic-v1`).
//!
//! Every NIC is a real [`vesper_warp_umem::SoftXsk`]: the application-side
//! AF_XDP driver ([`Xsk`]) over the deterministic software device
//! ([`SoftNic`]), with the four rings (fill, RX, TX, completion), the
//! driver's location table and its leak ledger. A source-visible
//! `UmemFrame[State]` value names a chunk whose Rust owner
//! ([`UmemFrame<RxHarvested>`], [`UmemFrame<TxStaged>`] or
//! [`InFlightQuicPkt`]) this adapter holds on the source task's behalf, so
//! checked Vesper source drives exactly the frame accounting the Rust path
//! uses:
//! - an arrival is [`SoftNic::deliver`]: the device pulls one fill buffer,
//!   writes the datagram bytes into the UMEM and publishes an RX descriptor;
//! - `harvest_rx_single` is [`XdpNic::harvest_rx`] (the RX ring is consumed);
//! - `view_frame_bytes` and the header split read the real UMEM bytes
//!   through [`XdpNic::view_frame_bytes`];
//! - `recycle_to_fill_ring` and `turn_around_tx_in_place` are the driver's
//!   consuming calls;
//! - `submit_paced_tx` hands the staged frame to the EDT launch queue; at its
//!   launch time the frame is posted to the TX ring
//!   ([`XdpNic::submit_paced_tx`]), the device transmits and completes it,
//!   and the completion is harvested, leaving the chunk `InFlight` under its
//!   [`InFlightQuicPkt`] lease;
//! - ACK, source-observed ACK or PTO retirement is [`XdpNic::retire`],
//!   followed by [`Xsk::replenish_fill`].
//!
//! After every step [`Umem::check_conservation`] runs the crate's exact
//! [`UmemAuditor`] (every chunk in exactly one of eleven locations, driver
//! and device sides agreeing, nothing leaked) and then cross-checks the
//! adapter's own record of which task holds which chunk against the driver's
//! table. A dropped Rust owner is never released: the driver quarantines its
//! chunk and the audit fails with `Leaked`.
//!
//! Source values are checked against this state on every consuming call: a
//! frame must name a chunk the calling task holds in the expected typestate,
//! with exactly the owner's offset and length. A forged, duplicated or stale
//! value is a typed [`UmemError`], never success. `ByteSlice` views carry the
//! chunk's generation; any later transition makes the view stale.
//!
//! [`SharedUmem`] journals every mutating call so that a step the session
//! does not commit can be rolled back by replaying the committed journal into
//! a fresh driver (the driver and its linear owners cannot be cloned).
//!
//! Not modelled here: AF_XDP hardware, kernel I/O, NIC launch-time offload
//! (the EDT launch queue is the simulator's), and transmit taking time on the
//! device (a launched frame completes at its launch instant).
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use vesper_warp_umem::{
    DriverLoc, InFlightQuicPkt, Location, RetireOutcome, RxHarvested, SoftNic, SoftNicMode,
    SoftXsk, TxStaged, UmemAuditor, UmemConfig, UmemFrame, XdpNic,
};

/// QUIC short header: flags byte, 8-byte DCID, 4-byte truncated PN.
pub const SHORT_HEADER_BYTES: u32 = 13;
/// `split_short_header` / `on_ack` error codes delivered to source as `Err(code)`.
pub const ERR_TRUNCATED: u32 = 1;
pub const ERR_LONG_HEADER: u32 = 2;
pub const ERR_FIXED_BIT: u32 = 3;
/// `on_ack`: the payload does not start with an ACK frame.
pub const ERR_NOT_ACK: u32 = 4;
/// `on_ack`: the ACK frame is truncated or its ranges underflow.
pub const ERR_BAD_ACK: u32 = 5;
/// Packet numbers are below 2^62 (RFC 9000 §12.3).
pub const MAX_PACKET_NUMBER: u64 = (1 << 62) - 1;
/// ACK frame type (RFC 9000 §19.3).
pub const ACK_FRAME_TYPE: u8 = 0x02;
/// Fixed-width ACK frame: type, largest, delay, range count, first range.
pub const ACK_FRAME_FIXED_BYTES: u32 = 1 + 8 + 8 + 4 + 8;
/// Each additional ACK range: gap and length.
pub const ACK_RANGE_BYTES: u32 = 16;

/// A datagram as it is delivered to the NIC. Only the first bytes (the
/// header, and an ACK frame for ACK datagrams) are materialised in the
/// recording; the rest of `len` is zero padding (PADDING frames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Datagram {
    pub id: u64,
    pub src_addr: u32,
    pub src_port: u32,
    pub len: u32,
    pub head_hex: String,
    /// The scenario flow (peer) that sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<u32>,
}
impl Datagram {
    pub fn head(&self) -> Vec<u8> {
        (0..self.head_hex.len() / 2)
            .map(|i| u8::from_str_radix(&self.head_hex[2 * i..2 * i + 2], 16).unwrap_or(0))
            .collect()
    }
    /// The full datagram bytes: the head, zero-padded to `len`.
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = self.head();
        bytes.resize(self.len as usize, 0);
        bytes
    }
}

/// The source-visible frame descriptor (`UmemFrame[State]` fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameDesc {
    pub chunk_idx: u32,
    pub data_offset: u32,
    pub pkt_len: u32,
}
/// The source-visible `ByteSlice` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewDesc {
    pub chunk_idx: u32,
    pub generation: u64,
    pub offset: u32,
    pub len: u32,
}
/// The source-visible `InFlightQuicPkt` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseDesc {
    pub pkt_num_full: u64,
    pub edt_departure_ns: u64,
    pub held_frame: FrameDesc,
}
/// A parsed short header plus the datagram's source address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortHeader {
    pub first_byte: u8,
    pub dcid: u64,
    pub pkt_num_trunc: u64,
    pub payload_len: u32,
    pub peer_addr: u32,
    pub peer_port: u32,
}
/// What `on_ack` reports to source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckSummary {
    pub largest_acked: u64,
    pub retired: u64,
}
/// `on_ack`: the summary, the parsed frame and the retired leases, or the
/// error code delivered to source.
pub type AckOutcome = Result<(AckSummary, AckFrame, Vec<Lease>), u32>;
/// A parsed ACK frame: inclusive packet-number ranges, largest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckFrame {
    pub dcid: u64,
    pub largest: u64,
    pub ack_delay_ns: u64,
    pub ranges: Vec<(u64, u64)>,
}

/// Where a source-held chunk is, by source typestate.
enum Held {
    Harvested {
        task: String,
        datagram: Datagram,
        frame: UmemFrame<RxHarvested>,
    },
    Staged {
        task: String,
        tx_len: u32,
        origin: Origin,
        frame: UmemFrame<TxStaged>,
    },
    Lease {
        lease: u64,
    },
}

/// Who a reply goes to: the flow and connection ID of the datagram it was
/// turned around from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Origin {
    flow: Option<u32>,
    dcid: Option<u64>,
}

/// The Rust owner behind a lease.
enum LeaseOwner {
    /// Submitted by source, waiting in the EDT launch queue for its launch
    /// time; the staged frame has not reached the TX ring yet.
    Paced(UmemFrame<TxStaged>),
    /// Launched: transmitted and completed by the NIC, owned by the lease.
    Launched(InFlightQuicPkt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Returned to source by `submit_paced_tx`, not yet parked.
    Leased,
    /// Parked in the ACK/PTO wheel.
    Parked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: u64,
    pub task: String,
    pub chunk: u32,
    pub tx_len: u32,
    pub pkt_num_full: u64,
    pub edt_ns: u64,
    pub state: LeaseState,
    /// Whether the reply has been transmitted (entered the egress link).
    pub launched: bool,
    /// Times at which copies of the reply are acknowledged by the timer ACK
    /// path (once launched).
    pub ack_at_ns: Vec<u64>,
    pub pto_at_ns: u64,
    /// The flow the reply goes to.
    pub flow: Option<u32>,
    /// The connection ID of the datagram the reply answers.
    pub dcid: Option<u64>,
    /// A source-observed ACK covered this lease while source still held it;
    /// it retires when parked.
    pub acked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RxDrop {
    /// The NIC had no fill buffer.
    NoFillFrame,
    RxRingFull,
}

/// Counts per location; `total` is the NIC's frame count, and
/// `free + fill + rx + harvested + staged + leased + parked = total`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounting {
    /// Fill ring plus buffers the NIC took from it.
    pub fill: u32,
    pub rx: u32,
    pub harvested: u32,
    pub staged: u32,
    pub leased: u32,
    pub parked: u32,
    pub total: u32,
    /// Driver free list (empty between steps: freed chunks are refilled).
    #[serde(default)]
    pub free: u32,
    /// Leases whose frame has been transmitted and completed (`InFlight`).
    #[serde(default)]
    pub launched: u32,
    /// Chunks the driver quarantined because their owner was dropped (then
    /// `harvested`/`staged`/`leased`/`parked` no longer count them).
    #[serde(default)]
    pub leaked: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConservationError {
    /// The software NIC's own audit (`UmemAuditor::audit`) failed.
    Nic(vesper_warp_umem::ConservationError),
    /// The adapter and the driver disagree about where a chunk is.
    Disagree {
        chunk: u32,
        adapter: &'static str,
        driver: Option<DriverLoc>,
    },
    /// The adapter's RX queue and the device's RX ring differ in length.
    RxRing {
        adapter: u32,
        device: u32,
    },
    LeaseMismatch {
        lease: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UmemError {
    /// The frame does not name a chunk this task holds in this typestate.
    NotHeld {
        chunk: u32,
        expected: &'static str,
    },
    /// The frame's fields differ from the owner's.
    Forged {
        chunk: u32,
    },
    /// The view's chunk changed since the view was taken.
    StaleView {
        chunk: u32,
    },
    TxLenOutOfRange {
        tx_len: u32,
        max: u32,
    },
    PacketNumberOutOfRange {
        pkt_num_full: u64,
    },
    LeaseLimit {
        limit: u64,
    },
    UnknownLease {
        lease: u64,
    },
    /// The driver refused a call the adapter had already validated: the
    /// adapter and the NIC disagree (a host-binding bug).
    Driver(String),
}

/// Deliberately broken host bindings used to show the conservation check
/// catches them. Not part of any scenario.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBug {
    /// `recycle_to_fill_ring` drops the frame owner instead of recycling it.
    DropOnRecycle { occurrence: u64 },
}

/// Geometry and limits of one NIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UmemParams {
    pub frames: u32,
    pub frame_bytes: u32,
    pub headroom: u32,
    pub rx_ring: u32,
    pub max_leases: u64,
}
impl UmemParams {
    /// The software NIC configuration: the scenario's frames, frame size,
    /// headroom and RX ring; fill, TX and completion rings large enough to
    /// hold every frame (so they can never refuse a frame).
    pub fn config(&self) -> UmemConfig {
        let all = self.frames.max(1).checked_next_power_of_two().unwrap_or(0);
        UmemConfig {
            frame_count: self.frames,
            frame_size: self.frame_bytes,
            headroom: self.headroom,
            fill_size: all,
            rx_size: self.rx_ring,
            tx_size: all,
            completion_size: all,
        }
    }
    /// Checked at scenario load.
    pub fn validate(&self) -> Result<(), String> {
        if !self.rx_ring.is_power_of_two() || self.rx_ring > self.frames {
            return Err("the RX ring must be a power of two of at most `frames` entries".into());
        }
        self.config()
            .validate()
            .map_err(|e| format!("software NIC: {e:?}"))
    }
}

pub struct Umem {
    params: UmemParams,
    xsk: SoftXsk,
    auditor: UmemAuditor,
    /// Datagrams on the RX ring, in ring order.
    rx: VecDeque<(u32, Datagram)>,
    held: BTreeMap<u32, Held>,
    owners: BTreeMap<u64, LeaseOwner>,
    leases: BTreeMap<u64, Lease>,
    generations: Vec<u64>,
    next_lease: u64,
    recycles: u64,
    bug: Option<HostBug>,
}

impl std::fmt::Debug for Umem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Umem")
            .field("params", &self.params)
            .field("rx", &self.rx.len())
            .field("held", &self.held.keys().collect::<Vec<_>>())
            .field("leases", &self.leases)
            .finish_non_exhaustive()
    }
}

fn driver(e: impl std::fmt::Debug) -> UmemError {
    UmemError::Driver(format!("{e:?}"))
}

impl Umem {
    /// A fresh NIC: every frame posted to the fill ring, in chunk order.
    /// `params` must have passed [`UmemParams::validate`].
    pub fn new(params: UmemParams) -> Self {
        let config = params.config();
        let mut nic = SoftNic::new(config, SoftNicMode::Scripted).expect("validated geometry");
        // The device takes a fill buffer only when a datagram arrives, so the
        // fill ring stays FIFO and chunk choice is reproducible.
        nic.fill_batch = 0;
        let mut xsk = SoftXsk::new(config, nic).expect("validated geometry");
        let posted = xsk.replenish_fill(params.frames);
        assert_eq!(posted, params.frames, "the fill ring holds every frame");
        Self {
            params,
            xsk,
            auditor: UmemAuditor::new(),
            rx: VecDeque::new(),
            held: BTreeMap::new(),
            owners: BTreeMap::new(),
            leases: BTreeMap::new(),
            generations: vec![0; params.frames as usize],
            next_lease: 1,
            recycles: 0,
            bug: None,
        }
    }
    pub fn set_bug(&mut self, bug: Option<HostBug>) {
        self.bug = bug;
    }
    pub fn rx_len(&self) -> u32 {
        self.rx.len() as u32
    }
    pub fn leases(&self) -> &BTreeMap<u64, Lease> {
        &self.leases
    }
    /// Largest datagram a frame can hold after the headroom.
    pub fn max_datagram(&self) -> u32 {
        self.params.frame_bytes - self.params.headroom
    }
    /// Device statistics of the software NIC.
    pub fn nic_stats(&self) -> vesper_warp_umem::SoftNicStats {
        self.xsk.backend().stats()
    }
    pub fn driver_stats(&self) -> vesper_warp_umem::DriverStats {
        self.xsk.stats()
    }
    fn bump(&mut self, chunk: u32) {
        self.generations[chunk as usize] += 1;
    }
    /// Put freed chunks (retired leases) back on the fill ring.
    fn refill(&mut self) {
        self.xsk.replenish_fill(self.params.frames);
    }

    /// A datagram reached the NIC: the device writes it into a fill buffer
    /// and publishes it on the RX ring, or drops it.
    pub fn arrive(&mut self, datagram: Datagram) -> Result<Result<u32, RxDrop>, UmemError> {
        use vesper_warp_umem::DeliverOutcome;
        let outcome = self.xsk.backend_mut().deliver(&datagram.bytes());
        Ok(match outcome {
            DeliverOutcome::Delivered { chunk } => {
                self.bump(chunk);
                self.rx.push_back((chunk, datagram));
                Ok(chunk)
            }
            DeliverOutcome::NoBuffer => Err(RxDrop::NoFillFrame),
            DeliverOutcome::RxRingFull => Err(RxDrop::RxRingFull),
            // Flows and ACK paths are validated to fit a frame at load.
            DeliverOutcome::Oversize => return Err(driver("oversize datagram")),
        })
    }

    pub fn harvest(&mut self, task: &str) -> Result<Option<(FrameDesc, Datagram)>, UmemError> {
        let frame = match self.xsk.harvest_rx() {
            Ok(None) => {
                return match self.rx.is_empty() {
                    true => Ok(None),
                    false => Err(driver("the RX ring is empty but datagrams were delivered")),
                };
            }
            Ok(Some(frame)) => frame,
            Err(rejected) => return Err(driver(rejected)),
        };
        let chunk = frame.chunk_idx();
        let Some((expected, datagram)) = self.rx.pop_front() else {
            // Hold the frame so the audit sees it rather than a leak.
            return Err(driver(format!(
                "unexpected RX descriptor for chunk {chunk}"
            )));
        };
        if expected != chunk {
            return Err(driver(format!(
                "RX ring order: chunk {chunk}, expected {expected}"
            )));
        }
        self.bump(chunk);
        let desc = FrameDesc {
            chunk_idx: chunk,
            data_offset: u32::from(frame.data_offset()),
            pkt_len: u32::from(frame.pkt_len()),
        };
        self.held.insert(
            chunk,
            Held::Harvested {
                task: task.into(),
                datagram: datagram.clone(),
                frame,
            },
        );
        Ok(Some((desc, datagram)))
    }

    fn harvested(
        &self,
        task: &str,
        frame: FrameDesc,
    ) -> Result<(&UmemFrame<RxHarvested>, &Datagram), UmemError> {
        match self.held.get(&frame.chunk_idx) {
            Some(Held::Harvested {
                task: t,
                datagram,
                frame: owner,
            }) if t == task => {
                if frame.data_offset != u32::from(owner.data_offset())
                    || frame.pkt_len != u32::from(owner.pkt_len())
                {
                    return Err(UmemError::Forged {
                        chunk: frame.chunk_idx,
                    });
                }
                Ok((owner, datagram))
            }
            _ => Err(UmemError::NotHeld {
                chunk: frame.chunk_idx,
                expected: "UmemFrame[RxHarvested]",
            }),
        }
    }

    pub fn view(&self, task: &str, frame: FrameDesc) -> Result<ViewDesc, UmemError> {
        let (owner, _) = self.harvested(task, frame)?;
        Ok(ViewDesc {
            chunk_idx: frame.chunk_idx,
            generation: self.generations[frame.chunk_idx as usize],
            offset: u32::from(owner.data_offset()),
            len: u32::from(owner.pkt_len()),
        })
    }

    /// The real UMEM bytes and datagram under a view that is still current.
    fn viewed(&self, task: &str, view: ViewDesc) -> Result<(&[u8], &Datagram), UmemError> {
        let stale = UmemError::StaleView {
            chunk: view.chunk_idx,
        };
        let Some(&generation) = self.generations.get(view.chunk_idx as usize) else {
            return Err(stale);
        };
        match self.held.get(&view.chunk_idx) {
            Some(Held::Harvested {
                task: t,
                datagram,
                frame,
            }) if t == task
                && generation == view.generation
                && view.offset == u32::from(frame.data_offset())
                && view.len == u32::from(frame.pkt_len()) =>
            {
                let bytes = self.xsk.view_frame_bytes(frame).map_err(driver)?;
                Ok((bytes, datagram))
            }
            _ => Err(stale),
        }
    }

    /// Parse the QUIC short header from the frame's UMEM bytes. `Ok(Err(code))`
    /// is a malformed header delivered to source; `Err` is a stale or forged
    /// view.
    pub fn split_short_header(
        &self,
        task: &str,
        view: ViewDesc,
    ) -> Result<Result<ShortHeader, u32>, UmemError> {
        let (bytes, datagram) = self.viewed(task, view)?;
        Ok(
            parse_short_header(bytes).map(|(first_byte, dcid, pn)| ShortHeader {
                first_byte,
                dcid,
                pkt_num_trunc: u64::from(pn),
                payload_len: bytes.len() as u32 - SHORT_HEADER_BYTES,
                peer_addr: datagram.src_addr,
                peer_port: datagram.src_port,
            }),
        )
    }

    pub fn recycle(&mut self, task: &str, frame: FrameDesc) -> Result<(), UmemError> {
        self.harvested(task, frame)?;
        let Some(Held::Harvested { frame: owner, .. }) = self.held.remove(&frame.chunk_idx) else {
            unreachable!("checked above")
        };
        self.bump(frame.chunk_idx);
        self.recycles += 1;
        let dropped = matches!(self.bug, Some(HostBug::DropOnRecycle { occurrence }) if occurrence == self.recycles);
        if dropped {
            // The broken binding forgets the owner; the driver's ledger
            // records the drop and the audit reports the chunk as leaked.
            drop(owner);
            return Ok(());
        }
        self.xsk
            .recycle_to_fill_ring(owner)
            .map_err(|refused| driver(refused.error))
    }

    pub fn turn_around(
        &mut self,
        task: &str,
        frame: FrameDesc,
        tx_len: u32,
    ) -> Result<FrameDesc, UmemError> {
        let (_, datagram) = self.harvested(task, frame)?;
        let max = self.max_datagram();
        if tx_len == 0 || tx_len > max {
            return Err(UmemError::TxLenOutOfRange { tx_len, max });
        }
        let head = datagram.head();
        let origin = Origin {
            flow: datagram.flow,
            dcid: parse_short_header(&head).ok().map(|(_, dcid, _)| dcid),
        };
        let Some(Held::Harvested {
            datagram,
            frame: owner,
            ..
        }) = self.held.remove(&frame.chunk_idx)
        else {
            unreachable!("checked above")
        };
        // tx_len <= frame bytes <= 32 KiB (validated geometry).
        let staged = match self.xsk.turn_around_tx_in_place(owner, tx_len as u16) {
            Ok(staged) => staged,
            Err(refused) => {
                let error = driver(refused.error);
                self.held.insert(
                    frame.chunk_idx,
                    Held::Harvested {
                        task: task.into(),
                        datagram,
                        frame: refused.owner,
                    },
                );
                return Err(error);
            }
        };
        self.bump(frame.chunk_idx);
        let desc = FrameDesc {
            chunk_idx: frame.chunk_idx,
            data_offset: u32::from(staged.data_offset()),
            pkt_len: u32::from(staged.pkt_len()),
        };
        self.held.insert(
            frame.chunk_idx,
            Held::Staged {
                task: task.into(),
                tx_len,
                origin,
                frame: staged,
            },
        );
        Ok(desc)
    }

    /// Check a `submit_paced_tx` without changing anything.
    pub fn check_submit(
        &self,
        task: &str,
        frame: FrameDesc,
        pkt_num_full: u64,
    ) -> Result<u32, UmemError> {
        let tx_len = match self.held.get(&frame.chunk_idx) {
            Some(Held::Staged {
                task: t,
                tx_len,
                frame: owner,
                ..
            }) if t == task => {
                if frame.data_offset != u32::from(owner.data_offset()) || frame.pkt_len != *tx_len {
                    return Err(UmemError::Forged {
                        chunk: frame.chunk_idx,
                    });
                }
                *tx_len
            }
            _ => {
                return Err(UmemError::NotHeld {
                    chunk: frame.chunk_idx,
                    expected: "UmemFrame[TxStaged]",
                });
            }
        };
        if pkt_num_full > MAX_PACKET_NUMBER {
            return Err(UmemError::PacketNumberOutOfRange { pkt_num_full });
        }
        if self.leases.len() as u64 >= self.params.max_leases {
            return Err(UmemError::LeaseLimit {
                limit: self.params.max_leases,
            });
        }
        Ok(tx_len)
    }

    /// Move a staged frame into a new lease waiting in the EDT launch queue.
    /// The PTO time is decided by the caller.
    pub fn submit(
        &mut self,
        task: &str,
        frame: FrameDesc,
        pkt_num_full: u64,
        edt_ns: u64,
        pto_at_ns: u64,
    ) -> Result<(u64, LeaseDesc), UmemError> {
        let tx_len = self.check_submit(task, frame, pkt_num_full)?;
        let Some(Held::Staged {
            origin,
            frame: owner,
            ..
        }) = self.held.remove(&frame.chunk_idx)
        else {
            unreachable!("checked above")
        };
        let id = self.next_lease;
        self.next_lease += 1;
        self.held.insert(frame.chunk_idx, Held::Lease { lease: id });
        self.owners.insert(id, LeaseOwner::Paced(owner));
        self.bump(frame.chunk_idx);
        self.leases.insert(
            id,
            Lease {
                id,
                task: task.into(),
                chunk: frame.chunk_idx,
                tx_len,
                pkt_num_full,
                edt_ns,
                state: LeaseState::Leased,
                launched: false,
                ack_at_ns: vec![],
                pto_at_ns,
                flow: origin.flow,
                dcid: origin.dcid,
                acked: false,
            },
        );
        Ok((
            id,
            LeaseDesc {
                pkt_num_full,
                edt_departure_ns: edt_ns,
                held_frame: frame,
            },
        ))
    }

    /// Park a lease the task holds. Returns its id, and whether a
    /// source-observed ACK already covered it (then it retired at once).
    pub fn park(&mut self, task: &str, desc: LeaseDesc) -> Result<(u64, Option<u32>), UmemError> {
        let chunk = desc.held_frame.chunk_idx;
        let not_held = UmemError::NotHeld {
            chunk,
            expected: "InFlightQuicPkt",
        };
        let Some(Held::Lease { lease }) = self.held.get(&chunk) else {
            return Err(not_held);
        };
        let lease = self.leases.get_mut(lease).ok_or(not_held.clone())?;
        if lease.task != task || lease.state != LeaseState::Leased {
            return Err(not_held);
        }
        if lease.pkt_num_full != desc.pkt_num_full
            || lease.edt_ns != desc.edt_departure_ns
            || desc.held_frame.data_offset != self.params.headroom
            || desc.held_frame.pkt_len != lease.tx_len
        {
            return Err(UmemError::Forged { chunk });
        }
        lease.state = LeaseState::Parked;
        let (id, acked) = (lease.id, lease.acked);
        self.bump(chunk);
        let retired = if acked {
            Some(self.retire(id)?.expect("a parked lease retires").chunk)
        } else {
            None
        };
        Ok((id, retired))
    }

    /// The lease's launch time: post its frame to the TX ring, let the device
    /// transmit and complete it, and harvest the completion. Records the
    /// timer-path ACK times.
    pub fn launch(&mut self, lease: u64, ack_at_ns: Vec<u64>) -> Result<(), UmemError> {
        let l = self
            .leases
            .get_mut(&lease)
            .ok_or(UmemError::UnknownLease { lease })?;
        if l.launched {
            return Err(driver(format!("lease {lease} launched twice")));
        }
        l.launched = true;
        l.ack_at_ns = ack_at_ns;
        let (pn, edt) = (l.pkt_num_full, l.edt_ns);
        let Some(LeaseOwner::Paced(frame)) = self.owners.remove(&lease) else {
            return Err(driver(format!("lease {lease} has no paced frame")));
        };
        let inflight = match self.xsk.submit_paced_tx(frame, pn, edt) {
            Ok(inflight) => inflight,
            Err(refused) => {
                let error = driver(refused.error);
                self.owners.insert(lease, LeaseOwner::Paced(refused.owner));
                return Err(error);
            }
        };
        self.owners.insert(lease, LeaseOwner::Launched(inflight));
        // One step consumes the TX descriptor, the next posts its completion.
        let nic = self.xsk.backend_mut();
        nic.step();
        nic.step();
        let report = self.xsk.harvest_completions(u32::MAX);
        if report.completed_leased != 1 || report.invalid != 0 {
            return Err(driver(format!("launch of lease {lease}: {report:?}")));
        }
        Ok(())
    }

    /// Retire a parked lease (ACK, source-observed ACK or PTO): the driver
    /// frees the chunk and it goes back to the fill ring. `None` when the
    /// lease was already retired or is not parked (a stale timer).
    pub fn retire(&mut self, lease: u64) -> Result<Option<Lease>, UmemError> {
        match self.leases.get(&lease) {
            Some(l) if l.state == LeaseState::Parked => {}
            _ => return Ok(None),
        }
        let l = self.leases.remove(&lease).expect("checked");
        self.held.remove(&l.chunk);
        self.bump(l.chunk);
        match self.owners.remove(&lease) {
            Some(LeaseOwner::Launched(inflight)) => match self.xsk.retire(inflight) {
                Ok(RetireOutcome::Freed) => {}
                Ok(RetireOutcome::AwaitingCompletion) => {
                    return Err(driver(format!("lease {lease} retired before completion")));
                }
                Err(refused) => {
                    let error = driver(refused.error);
                    self.owners
                        .insert(lease, LeaseOwner::Launched(refused.owner));
                    return Err(error);
                }
            },
            Some(LeaseOwner::Paced(frame)) => {
                // Never launched (ACK and PTO timers start after launch).
                self.xsk
                    .abandon_tx(frame)
                    .map_err(|refused| driver(refused.error))?;
            }
            None => return Err(UmemError::UnknownLease { lease }),
        }
        self.refill();
        Ok(Some(l))
    }

    /// Source observed a datagram under `view`: if it carries an ACK frame,
    /// retire every parked, launched lease of the ACK's connection whose
    /// packet number it covers. Leases source still holds are marked and
    /// retire when parked. Returns the summary and the retired leases.
    pub fn on_ack(&mut self, task: &str, view: ViewDesc) -> Result<AckOutcome, UmemError> {
        let (bytes, _) = self.viewed(task, view)?;
        let ack = match parse_ack(bytes) {
            Ok(ack) => ack,
            Err(code) => return Ok(Err(code)),
        };
        let covered: Vec<u64> = self
            .leases
            .values()
            .filter(|l| {
                l.launched
                    && !l.acked
                    && l.dcid == Some(ack.dcid)
                    && ack
                        .ranges
                        .iter()
                        .any(|(lo, hi)| (*lo..=*hi).contains(&l.pkt_num_full))
            })
            .map(|l| l.id)
            .collect();
        let mut retired = vec![];
        for id in covered {
            let lease = self.leases.get_mut(&id).expect("listed");
            lease.acked = true;
            if lease.state == LeaseState::Parked
                && let Some(l) = self.retire(id)?
            {
                retired.push(l);
            }
        }
        let summary = AckSummary {
            largest_acked: ack.largest,
            retired: retired.len() as u64,
        };
        Ok(Ok((summary, ack, retired)))
    }

    /// Chunks a task still holds (frames or unparked leases).
    pub fn held_by(&self, task: &str) -> Vec<u32> {
        self.held
            .iter()
            .filter(|(_, h)| match h {
                Held::Harvested { task: t, .. } | Held::Staged { task: t, .. } => t == task,
                Held::Lease { lease } => self
                    .leases
                    .get(lease)
                    .is_some_and(|l| l.task == task && l.state == LeaseState::Leased),
            })
            .map(|(c, _)| *c)
            .collect()
    }

    /// A task finished while holding frames: its owners are dropped, as a
    /// source program that loses a linear value would. The driver's ledger
    /// records each drop and the next audit reports them as leaked.
    pub fn leak_task(&mut self, task: &str) {
        for chunk in self.held_by(task) {
            match self.held.remove(&chunk) {
                Some(Held::Harvested { frame, .. }) => drop(frame),
                Some(Held::Staged { frame, .. }) => drop(frame),
                Some(Held::Lease { lease }) => {
                    self.leases.remove(&lease);
                    drop(self.owners.remove(&lease));
                }
                None => {}
            }
        }
    }

    /// Counts for reports. Unlike [`Self::check_conservation`] this does not
    /// fail on leaked chunks; it reports them.
    pub fn accounting(&mut self) -> Accounting {
        let Ok(counts) = self.auditor.counts(&mut self.xsk) else {
            return Accounting {
                total: self.params.frames,
                ..Accounting::default()
            };
        };
        let mut a = Accounting {
            total: self.params.frames,
            free: counts.get(Location::Free),
            fill: counts.get(Location::FillRing) + counts.get(Location::NicRx),
            rx: counts.get(Location::RxRing),
            leaked: counts.get(Location::Leaked),
            launched: counts.get(Location::InFlight),
            ..Accounting::default()
        };
        for held in self.held.values() {
            match held {
                Held::Harvested { .. } => a.harvested += 1,
                Held::Staged { .. } => a.staged += 1,
                Held::Lease { lease } => match self.leases.get(lease).map(|l| l.state) {
                    Some(LeaseState::Leased) => a.leased += 1,
                    Some(LeaseState::Parked) => a.parked += 1,
                    None => {}
                },
            }
        }
        a
    }

    /// The software NIC's exact audit, then the adapter's cross-check: each
    /// chunk the adapter attributes to a task or lease is where the driver
    /// says it is, and the RX queue matches the device's RX ring.
    pub fn check_conservation(&mut self) -> Result<Accounting, ConservationError> {
        let audit = self
            .auditor
            .audit(&mut self.xsk)
            .map_err(ConservationError::Nic)?;
        let mut a = Accounting {
            total: self.params.frames,
            free: audit.get(Location::Free),
            fill: audit.get(Location::FillRing) + audit.get(Location::NicRx),
            rx: audit.get(Location::RxRing),
            ..Accounting::default()
        };
        if a.rx != self.rx.len() as u32 {
            return Err(ConservationError::RxRing {
                adapter: self.rx.len() as u32,
                device: a.rx,
            });
        }
        let disagree = |chunk, adapter, driver| ConservationError::Disagree {
            chunk,
            adapter,
            driver,
        };
        for (chunk, _) in &self.rx {
            let loc = self.xsk.location(*chunk);
            if loc != Some(DriverLoc::FillPosted) {
                return Err(disagree(*chunk, "rx ring", loc));
            }
        }
        for (chunk, held) in &self.held {
            let loc = self.xsk.location(*chunk);
            let (ok, what) = match held {
                Held::Harvested { frame, .. } => {
                    a.harvested += 1;
                    (
                        loc == Some(DriverLoc::AppRx) && frame.chunk_idx() == *chunk,
                        "harvested",
                    )
                }
                Held::Staged { frame, .. } => {
                    a.staged += 1;
                    (
                        loc == Some(DriverLoc::AppTx) && frame.chunk_idx() == *chunk,
                        "staged",
                    )
                }
                Held::Lease { lease } => {
                    let Some(l) = self.leases.get(lease).filter(|l| l.chunk == *chunk) else {
                        return Err(ConservationError::LeaseMismatch { lease: *lease });
                    };
                    match l.state {
                        LeaseState::Leased => a.leased += 1,
                        LeaseState::Parked => a.parked += 1,
                    }
                    match self.owners.get(lease) {
                        Some(LeaseOwner::Paced(f)) => (
                            !l.launched && loc == Some(DriverLoc::AppTx) && f.chunk_idx() == *chunk,
                            "paced lease",
                        ),
                        Some(LeaseOwner::Launched(p)) => {
                            a.launched += 1;
                            (
                                l.launched
                                    && loc == Some(DriverLoc::InFlight)
                                    && p.chunk_idx() == *chunk,
                                "launched lease",
                            )
                        }
                        None => return Err(ConservationError::LeaseMismatch { lease: *lease }),
                    }
                }
            };
            if !ok {
                return Err(disagree(*chunk, what, loc));
            }
        }
        for (id, lease) in &self.leases {
            if !matches!(self.held.get(&lease.chunk), Some(Held::Lease { lease }) if lease == id) {
                return Err(ConservationError::LeaseMismatch { lease: *id });
            }
        }
        // Every chunk the driver gives the application is one the adapter
        // attributes to a task or lease.
        let app =
            audit.get(Location::AppRx) + audit.get(Location::AppTx) + audit.get(Location::InFlight);
        if app != self.held.len() as u32 {
            return Err(ConservationError::Disagree {
                chunk: u32::MAX,
                adapter: "held count",
                driver: None,
            });
        }
        Ok(a)
    }
}

/// `(first_byte, dcid, truncated pn)` of a short header, or the error code.
fn parse_short_header(bytes: &[u8]) -> Result<(u8, u64, u32), u32> {
    if bytes.len() < SHORT_HEADER_BYTES as usize {
        return Err(ERR_TRUNCATED);
    }
    let first = bytes[0];
    if first & 0x80 != 0 {
        return Err(ERR_LONG_HEADER);
    }
    if first & 0x40 == 0 {
        return Err(ERR_FIXED_BIT);
    }
    let dcid = u64::from_be_bytes(bytes[1..9].try_into().expect("8 bytes"));
    let pn = u32::from_be_bytes(bytes[9..13].try_into().expect("4 bytes"));
    Ok((first, dcid, pn))
}

/// Parse a datagram whose payload starts with a fixed-width ACK frame
/// (RFC 9000 §19.3 fields, big-endian integers instead of varints).
pub fn parse_ack(bytes: &[u8]) -> Result<AckFrame, u32> {
    let (_, dcid, _) = parse_short_header(bytes)?;
    let payload = &bytes[SHORT_HEADER_BYTES as usize..];
    if payload.first() != Some(&ACK_FRAME_TYPE) {
        return Err(ERR_NOT_ACK);
    }
    let mut at = 1usize;
    let mut take = |n: usize| -> Result<&[u8], u32> {
        let end = at.checked_add(n).ok_or(ERR_BAD_ACK)?;
        let field = payload.get(at..end).ok_or(ERR_BAD_ACK)?;
        at = end;
        Ok(field)
    };
    let u64_at = |b: &[u8]| u64::from_be_bytes(b.try_into().expect("8 bytes"));
    let largest = u64_at(take(8)?);
    let ack_delay_ns = u64_at(take(8)?);
    let count = u32::from_be_bytes(take(4)?.try_into().expect("4 bytes"));
    let first = u64_at(take(8)?);
    if largest > MAX_PACKET_NUMBER {
        return Err(ERR_BAD_ACK);
    }
    let mut hi = largest;
    let mut lo = hi.checked_sub(first).ok_or(ERR_BAD_ACK)?;
    // Each additional range needs 16 bytes; reject counts the datagram cannot hold.
    if u64::from(count) * u64::from(ACK_RANGE_BYTES) > payload.len() as u64 {
        return Err(ERR_BAD_ACK);
    }
    let mut ranges = vec![(lo, hi)];
    for _ in 0..count {
        let gap = u64_at(take(8)?);
        let len = u64_at(take(8)?);
        // RFC 9000 §19.3.1: the next largest is smallest - gap - 2.
        hi = lo
            .checked_sub(gap)
            .and_then(|v| v.checked_sub(2))
            .ok_or(ERR_BAD_ACK)?;
        lo = hi.checked_sub(len).ok_or(ERR_BAD_ACK)?;
        ranges.push((lo, hi));
    }
    Ok(AckFrame {
        dcid,
        largest,
        ack_delay_ns,
        ranges,
    })
}

/// Encode an ACK frame for inclusive ranges given largest first, disjoint
/// and separated by at least one missing packet number.
pub fn ack_frame_bytes(ranges: &[(u64, u64)], ack_delay_ns: u64) -> Vec<u8> {
    let mut out = vec![ACK_FRAME_TYPE];
    let (lo0, hi0) = ranges[0];
    out.extend_from_slice(&hi0.to_be_bytes());
    out.extend_from_slice(&ack_delay_ns.to_be_bytes());
    out.extend_from_slice(&(ranges.len() as u32 - 1).to_be_bytes());
    out.extend_from_slice(&(hi0 - lo0).to_be_bytes());
    let mut prev_lo = lo0;
    for &(lo, hi) in &ranges[1..] {
        out.extend_from_slice(&(prev_lo - hi - 2).to_be_bytes());
        out.extend_from_slice(&(hi - lo).to_be_bytes());
        prev_lo = lo;
    }
    out
}

/// Build the bytes of a datagram's materialised head.
pub fn short_header(first_byte: u8, dcid: u64, pkt_num: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(SHORT_HEADER_BYTES as usize);
    out.push(first_byte);
    out.extend_from_slice(&dcid.to_be_bytes());
    // Truncation to the low 32 bits is the wire encoding (4-byte PN field).
    out.extend_from_slice(&(pkt_num as u32).to_be_bytes());
    out
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One journaled mutating call on a [`Umem`].
#[derive(Debug, Clone)]
enum UmemOp {
    Arrive(Datagram),
    Harvest(String),
    Recycle(String, FrameDesc),
    TurnAround(String, FrameDesc, u32),
    Submit(String, FrameDesc, u64, u64, u64),
    Park(String, LeaseDesc),
    Launch(u64, Vec<u64>),
    Retire(u64),
    OnAck(String, ViewDesc),
    LeakTask(String),
}

#[derive(Debug)]
struct Journal {
    umem: Umem,
    log: Vec<UmemOp>,
    committed: usize,
}

impl Journal {
    fn apply(umem: &mut Umem, op: &UmemOp) {
        // Results are ignored: replaying the same calls in the same order
        // reproduces the same state, failures included.
        let _ = match op {
            UmemOp::Arrive(d) => umem.arrive(d.clone()).map(|_| ()),
            UmemOp::Harvest(t) => umem.harvest(t).map(|_| ()),
            UmemOp::Recycle(t, f) => umem.recycle(t, *f),
            UmemOp::TurnAround(t, f, len) => umem.turn_around(t, *f, *len).map(|_| ()),
            UmemOp::Submit(t, f, pn, edt, pto) => umem.submit(t, *f, *pn, *edt, *pto).map(|_| ()),
            UmemOp::Park(t, d) => umem.park(t, *d).map(|_| ()),
            UmemOp::Launch(l, acks) => umem.launch(*l, acks.clone()),
            UmemOp::Retire(l) => umem.retire(*l).map(|_| ()),
            UmemOp::OnAck(t, v) => umem.on_ack(t, *v).map(|_| ()),
            UmemOp::LeakTask(t) => {
                umem.leak_task(t);
                Ok(())
            }
        };
    }
}

/// A [`Umem`] shared by every copy of the model state, with a journal of its
/// mutating calls.
///
/// The session steps the model copy-check-commit: it clones the model state,
/// applies a step to the clone, checks it and only then replaces the
/// committed state. The software NIC and its linear owners cannot be cloned,
/// so clones share one `Umem`; [`SharedUmem::commit`] marks the journal as
/// committed and [`SharedUmem::rollback`] rebuilds the `Umem` from scratch by
/// replaying the committed calls into a fresh driver. Rollback happens only
/// when a step fails, so its cost does not affect normal runs.
#[derive(Clone)]
pub struct SharedUmem(Rc<RefCell<Journal>>);

impl std::fmt::Debug for SharedUmem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.borrow().umem.fmt(f)
    }
}

impl SharedUmem {
    pub fn new(params: UmemParams) -> Self {
        Self(Rc::new(RefCell::new(Journal {
            umem: Umem::new(params),
            log: vec![],
            committed: 0,
        })))
    }
    fn run<T>(&self, op: UmemOp, f: impl FnOnce(&mut Umem) -> T) -> T {
        let mut j = self.0.borrow_mut();
        j.log.push(op);
        f(&mut j.umem)
    }
    /// Read-only access.
    pub fn with<T>(&self, f: impl FnOnce(&Umem) -> T) -> T {
        f(&self.0.borrow().umem)
    }
    pub fn commit(&self) {
        let mut j = self.0.borrow_mut();
        j.committed = j.log.len();
    }
    /// Discard every call since the last commit.
    pub fn rollback(&self) {
        let mut j = self.0.borrow_mut();
        if j.log.len() == j.committed {
            return;
        }
        let committed = j.committed;
        j.log.truncate(committed);
        let params = j.umem.params;
        let bug = j.umem.bug;
        let mut fresh = Umem::new(params);
        fresh.set_bug(bug);
        for op in &j.log {
            Journal::apply(&mut fresh, op);
        }
        j.umem = fresh;
    }
    /// Uncommitted journal entries (tests).
    pub fn pending(&self) -> usize {
        let j = self.0.borrow();
        j.log.len() - j.committed
    }

    pub fn set_bug(&self, bug: Option<HostBug>) {
        self.0.borrow_mut().umem.set_bug(bug);
    }
    pub fn arrive(&self, d: Datagram) -> Result<Result<u32, RxDrop>, UmemError> {
        self.run(UmemOp::Arrive(d.clone()), |u| u.arrive(d))
    }
    pub fn harvest(&self, task: &str) -> Result<Option<(FrameDesc, Datagram)>, UmemError> {
        self.run(UmemOp::Harvest(task.into()), |u| u.harvest(task))
    }
    pub fn view(&self, task: &str, frame: FrameDesc) -> Result<ViewDesc, UmemError> {
        self.with(|u| u.view(task, frame))
    }
    pub fn split_short_header(
        &self,
        task: &str,
        view: ViewDesc,
    ) -> Result<Result<ShortHeader, u32>, UmemError> {
        self.with(|u| u.split_short_header(task, view))
    }
    pub fn recycle(&self, task: &str, frame: FrameDesc) -> Result<(), UmemError> {
        self.run(UmemOp::Recycle(task.into(), frame), |u| {
            u.recycle(task, frame)
        })
    }
    pub fn turn_around(
        &self,
        task: &str,
        frame: FrameDesc,
        tx_len: u32,
    ) -> Result<FrameDesc, UmemError> {
        self.run(UmemOp::TurnAround(task.into(), frame, tx_len), |u| {
            u.turn_around(task, frame, tx_len)
        })
    }
    pub fn check_submit(&self, task: &str, frame: FrameDesc, pn: u64) -> Result<u32, UmemError> {
        self.with(|u| u.check_submit(task, frame, pn))
    }
    pub fn submit(
        &self,
        task: &str,
        frame: FrameDesc,
        pn: u64,
        edt_ns: u64,
        pto_at_ns: u64,
    ) -> Result<(u64, LeaseDesc), UmemError> {
        self.run(
            UmemOp::Submit(task.into(), frame, pn, edt_ns, pto_at_ns),
            |u| u.submit(task, frame, pn, edt_ns, pto_at_ns),
        )
    }
    pub fn park(&self, task: &str, desc: LeaseDesc) -> Result<(u64, Option<u32>), UmemError> {
        self.run(UmemOp::Park(task.into(), desc), |u| u.park(task, desc))
    }
    pub fn launch(&self, lease: u64, ack_at_ns: Vec<u64>) -> Result<(), UmemError> {
        self.run(UmemOp::Launch(lease, ack_at_ns.clone()), |u| {
            u.launch(lease, ack_at_ns)
        })
    }
    pub fn retire(&self, lease: u64) -> Result<Option<Lease>, UmemError> {
        self.run(UmemOp::Retire(lease), |u| u.retire(lease))
    }
    pub fn on_ack(&self, task: &str, view: ViewDesc) -> Result<AckOutcome, UmemError> {
        self.run(UmemOp::OnAck(task.into(), view), |u| u.on_ack(task, view))
    }
    pub fn leak_task(&self, task: &str) {
        self.run(UmemOp::LeakTask(task.into()), |u| u.leak_task(task))
    }
    pub fn check_conservation(&self) -> Result<Accounting, ConservationError> {
        self.0.borrow_mut().umem.check_conservation()
    }
    pub fn accounting(&self) -> Accounting {
        self.0.borrow_mut().umem.accounting()
    }
    pub fn lease(&self, lease: u64) -> Option<Lease> {
        self.with(|u| u.leases().get(&lease).cloned())
    }
    pub fn lease_count(&self) -> usize {
        self.with(|u| u.leases().len())
    }
    pub fn rx_len(&self) -> u32 {
        self.with(|u| u.rx_len())
    }
    pub fn held_by(&self, task: &str) -> Vec<u32> {
        self.with(|u| u.held_by(task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datagram(id: u64, head: Vec<u8>, len: u32) -> Datagram {
        Datagram {
            id,
            src_addr: 1,
            src_port: 2,
            len,
            head_hex: hex(&head),
            flow: Some(0),
        }
    }
    fn params(frames: u32, rx_ring: u32, max_leases: u64) -> UmemParams {
        UmemParams {
            frames,
            frame_bytes: 2048,
            headroom: 256,
            rx_ring,
            max_leases,
        }
    }

    #[test]
    fn harvest_recycle_reply_and_retire_conserve_frames_on_the_soft_nic() {
        let mut u = Umem::new(params(4, 2, 4));
        assert_eq!(u.check_conservation().unwrap().fill, 4);
        u.arrive(datagram(1, short_header(0x43, 7, 9), 100))
            .unwrap()
            .unwrap();
        u.arrive(datagram(2, vec![0x43, 1], 2)).unwrap().unwrap();
        assert_eq!(
            u.arrive(datagram(3, vec![1], 1)).unwrap(),
            Err(RxDrop::RxRingFull)
        );
        let (frame, _) = u.harvest("t").unwrap().unwrap();
        assert_eq!((frame.chunk_idx, frame.data_offset), (0, 256));
        let view = u.view("t", frame).unwrap();
        let hdr = u.split_short_header("t", view).unwrap().unwrap();
        assert_eq!((hdr.dcid, hdr.pkt_num_trunc, hdr.payload_len), (7, 9, 87));
        assert_eq!(
            u.view("other", frame),
            Err(UmemError::NotHeld {
                chunk: 0,
                expected: "UmemFrame[RxHarvested]"
            })
        );
        let forged = FrameDesc {
            pkt_len: 99,
            ..frame
        };
        assert_eq!(u.recycle("t", forged), Err(UmemError::Forged { chunk: 0 }));
        let tx = u.turn_around("t", frame, 64).unwrap();
        assert_eq!(
            u.split_short_header("t", view),
            Err(UmemError::StaleView { chunk: 0 })
        );
        assert!(u.recycle("t", frame).is_err());
        let (lease, desc) = u.submit("t", tx, 5, 10, 30).unwrap();
        assert_eq!(u.held_by("t"), vec![0]);
        assert_eq!(
            u.retire(lease).unwrap(),
            None,
            "an unparked lease cannot retire"
        );
        u.park("t", desc).unwrap();
        assert!(u.held_by("t").is_empty());
        let a = u.check_conservation().unwrap();
        assert_eq!((a.fill, a.rx, a.parked, a.launched), (2, 1, 1, 0));
        u.launch(lease, vec![]).unwrap();
        let a = u.check_conservation().unwrap();
        assert_eq!((a.parked, a.launched), (1, 1));
        assert_eq!(u.driver_stats().completions, 1);
        let (second, _) = u.harvest("t").unwrap().unwrap();
        let v = u.view("t", second).unwrap();
        assert_eq!(u.split_short_header("t", v).unwrap(), Err(ERR_TRUNCATED));
        u.recycle("t", second).unwrap();
        assert!(u.retire(lease).unwrap().is_some());
        assert_eq!(u.retire(lease).unwrap(), None);
        let a = u.check_conservation().unwrap();
        assert_eq!((a.fill, a.free), (4, 0));
        assert_eq!(u.nic_stats().rx_delivered, 2);
    }

    #[test]
    fn header_errors_bounds_and_a_dropping_binding() {
        let mut u = Umem::new(params(2, 2, 1));
        u.arrive(datagram(1, short_header(0xc3, 1, 1), 50))
            .unwrap()
            .unwrap();
        u.arrive(datagram(2, short_header(0x03, 1, 1), 50))
            .unwrap()
            .unwrap();
        let (a, _) = u.harvest("t").unwrap().unwrap();
        let (b, _) = u.harvest("t").unwrap().unwrap();
        assert_eq!(
            u.split_short_header("t", u.view("t", a).unwrap()).unwrap(),
            Err(ERR_LONG_HEADER)
        );
        assert_eq!(
            u.split_short_header("t", u.view("t", b).unwrap()).unwrap(),
            Err(ERR_FIXED_BIT)
        );
        assert_eq!(
            u.turn_around("t", a, 1793),
            Err(UmemError::TxLenOutOfRange {
                tx_len: 1793,
                max: 1792
            })
        );
        let tx = u.turn_around("t", a, 10).unwrap();
        assert_eq!(
            u.check_submit("t", tx, 1 << 62),
            Err(UmemError::PacketNumberOutOfRange {
                pkt_num_full: 1 << 62
            })
        );
        let (_, desc) = u.submit("t", tx, 1, 0, 5).unwrap();
        u.set_bug(Some(HostBug::DropOnRecycle { occurrence: 1 }));
        u.recycle("t", b).unwrap();
        match u.check_conservation() {
            Err(ConservationError::Nic(vesper_warp_umem::ConservationError::Leaked {
                count: 1,
                first_chunk: Some(1),
            })) => {}
            other => panic!("{other:?}"),
        }
        u.park("t", desc).unwrap();
    }

    #[test]
    fn ack_frames_round_trip_and_retire_covered_leases() {
        let ranges = [(10, 12), (5, 7), (0, 1)];
        let mut head = short_header(0x43, 7, 99);
        head.extend(ack_frame_bytes(&ranges, 25));
        let ack = parse_ack(&head).unwrap();
        assert_eq!((ack.dcid, ack.largest, ack.ack_delay_ns), (7, 12, 25));
        assert_eq!(ack.ranges, ranges);
        // Underflowing gaps, truncation and non-ACK payloads are typed codes.
        let mut bad = head.clone();
        bad[13 + 1 + 8 + 8 + 4 + 7] = 13; // first range 13 > largest 12
        assert_eq!(parse_ack(&bad), Err(ERR_BAD_ACK));
        assert_eq!(parse_ack(&head[..30]), Err(ERR_BAD_ACK));
        let mut pad = short_header(0x43, 7, 1);
        pad.push(0);
        assert_eq!(parse_ack(&pad), Err(ERR_NOT_ACK));

        let mut u = Umem::new(params(4, 4, 4));
        for (id, pn) in [(1, 1), (2, 2)] {
            u.arrive(datagram(id, short_header(0x43, 7, pn), 64))
                .unwrap()
                .unwrap();
            let (f, _) = u.harvest("t").unwrap().unwrap();
            let tx = u.turn_around("t", f, 64).unwrap();
            let (lease, desc) = u.submit("t", tx, 10 + pn, 0, 100).unwrap();
            u.launch(lease, vec![]).unwrap();
            if pn == 1 {
                u.park("t", desc).unwrap();
            }
        }
        // An ACK for 11..=12 from connection 7: lease 1 is parked and
        // retires; lease 2 is still held by source and retires when parked.
        let mut head = short_header(0x43, 7, 3);
        head.extend(ack_frame_bytes(&[(11, 12)], 0));
        let len = head.len() as u32;
        u.arrive(datagram(3, head, len)).unwrap().unwrap();
        let (f, _) = u.harvest("t").unwrap().unwrap();
        let view = u.view("t", f).unwrap();
        let (summary, _, retired) = u.on_ack("t", view).unwrap().unwrap();
        assert_eq!((summary.largest_acked, summary.retired), (12, 1));
        assert_eq!(retired[0].id, 1);
        u.recycle("t", f).unwrap();
        let lease2 = *u.leases().keys().next().unwrap();
        let desc = LeaseDesc {
            pkt_num_full: 12,
            edt_departure_ns: 0,
            held_frame: FrameDesc {
                chunk_idx: u.leases()[&lease2].chunk,
                data_offset: 256,
                pkt_len: 64,
            },
        };
        let (_, retired) = u.park("t", desc).unwrap();
        assert!(retired.is_some());
        let a = u.check_conservation().unwrap();
        assert_eq!((a.fill, a.leased + a.parked), (4, 0));
    }

    #[test]
    fn rollback_replays_the_committed_journal() {
        let u = SharedUmem::new(params(4, 4, 4));
        u.arrive(datagram(1, short_header(0x43, 7, 1), 64))
            .unwrap()
            .unwrap();
        u.commit();
        let (frame, _) = u.harvest("t").unwrap().unwrap();
        u.turn_around("t", frame, 64).unwrap();
        assert_eq!(u.pending(), 2);
        u.rollback();
        assert_eq!(u.pending(), 0);
        // The harvested frame is back on the RX ring and can be harvested again.
        assert_eq!(u.rx_len(), 1);
        let a = u.check_conservation().unwrap();
        assert_eq!((a.rx, a.harvested, a.staged), (1, 0, 0));
        assert_eq!(u.harvest("t").unwrap().unwrap().0, frame);
    }

    #[test]
    fn a_leaking_task_fails_the_soft_nic_audit() {
        let mut u = Umem::new(params(4, 4, 4));
        u.arrive(datagram(1, short_header(0x43, 7, 1), 64))
            .unwrap()
            .unwrap();
        u.harvest("t").unwrap().unwrap();
        u.leak_task("t");
        match u.check_conservation() {
            Err(ConservationError::Nic(vesper_warp_umem::ConservationError::Leaked {
                count: 1,
                ..
            })) => {}
            other => panic!("{other:?}"),
        }
    }
}
