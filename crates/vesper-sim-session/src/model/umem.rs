//! A bounded UMEM frame model (`umem-v1`) behind the source `XdpNic` effect.
//!
//! A NIC owns `frames` fixed-size chunks identified by `chunk_idx`. Every
//! chunk is in exactly one place, kept in separate containers so that a lost
//! or duplicated chunk is visible rather than assumed away:
//! - the **fill ring** (free, owned by the simulated kernel),
//! - the **RX ring** (holding a received datagram, not yet harvested),
//! - **owned by a source task** as `UmemFrame[RxHarvested]` or
//!   `UmemFrame[TxStaged]`, or inside an `InFlightQuicPkt` lease,
//! - **parked** in the simulator's ACK/PTO wheel.
//!
//! [`Umem::check_conservation`] recounts every container after each step and
//! reports a missing, duplicated or inconsistent chunk as a
//! [`ConservationError`]; the session turns that into an explicit model error.
//!
//! Source values are checked against this state on every consuming call: a
//! frame must name a chunk the calling task holds in the expected typestate,
//! with exactly the recorded offset and length. A forged, duplicated or
//! stale value is a typed [`UmemError`], never success. `ByteSlice` views carry
//! the chunk's generation; any later transition of the chunk makes the view
//! stale.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// QUIC short header: flags byte, 8-byte DCID, 4-byte truncated PN.
pub const SHORT_HEADER_BYTES: u32 = 13;
/// `split_short_header` error codes delivered to source as `Err(code)`.
pub const ERR_TRUNCATED: u32 = 1;
pub const ERR_LONG_HEADER: u32 = 2;
pub const ERR_FIXED_BIT: u32 = 3;
/// Packet numbers are below 2^62 (RFC 9000 §12.3).
pub const MAX_PACKET_NUMBER: u64 = (1 << 62) - 1;

/// A datagram as it sits in a frame. Only the first bytes (the header and
/// at most a few payload bytes) are materialised; `len` is the full length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Datagram {
    pub id: u64,
    pub src_addr: u32,
    pub src_port: u32,
    pub len: u32,
    pub head_hex: String,
}
impl Datagram {
    pub fn head(&self) -> Vec<u8> {
        (0..self.head_hex.len() / 2)
            .map(|i| u8::from_str_radix(&self.head_hex[2 * i..2 * i + 2], 16).unwrap_or(0))
            .collect()
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

/// Where a source-held chunk is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Held {
    Harvested { task: String, datagram: Datagram },
    Staged { task: String, tx_len: u32 },
    Lease { lease: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Returned to source by `submit_paced_tx`, not yet parked.
    Leased,
    /// Parked in the ACK/PTO wheel.
    Parked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub id: u64,
    pub task: String,
    pub chunk: u32,
    pub tx_len: u32,
    pub pkt_num_full: u64,
    pub edt_ns: u64,
    pub state: LeaseState,
    /// Times at which copies of the reply are acknowledged.
    pub ack_at_ns: Vec<u64>,
    pub pto_at_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RxDrop {
    NoFillFrame,
    RxRingFull,
}

/// Counts per location; `total` is the NIC's frame count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounting {
    pub fill: u32,
    pub rx: u32,
    pub harvested: u32,
    pub staged: u32,
    pub leased: u32,
    pub parked: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConservationError {
    Missing { chunk: u32 },
    Duplicated { chunk: u32 },
    Unknown { chunk: u32 },
    LeaseMismatch { lease: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UmemError {
    /// The frame does not name a chunk this task holds in this typestate.
    NotHeld {
        chunk: u32,
        expected: &'static str,
    },
    /// The frame's fields differ from the recorded descriptor.
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
}

/// Deliberately broken host bindings used to show the conservation check
/// catches them. Not part of any scenario.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBug {
    /// `recycle_to_fill_ring` forgets the chunk instead of refilling it.
    DropOnRecycle { occurrence: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Umem {
    frames: u32,
    frame_bytes: u32,
    headroom: u32,
    rx_capacity: u32,
    fill: VecDeque<u32>,
    rx: VecDeque<(u32, Datagram)>,
    held: BTreeMap<u32, Held>,
    generations: Vec<u64>,
    leases: BTreeMap<u64, Lease>,
    next_lease: u64,
    max_leases: u64,
    recycles: u64,
    bug: Option<HostBug>,
}

impl Umem {
    pub fn new(
        frames: u32,
        frame_bytes: u32,
        headroom: u32,
        rx_capacity: u32,
        max_leases: u64,
    ) -> Self {
        Self {
            frames,
            frame_bytes,
            headroom,
            rx_capacity,
            fill: (0..frames).collect(),
            rx: VecDeque::new(),
            held: BTreeMap::new(),
            generations: vec![0; frames as usize],
            leases: BTreeMap::new(),
            next_lease: 1,
            max_leases,
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
        self.frame_bytes - self.headroom
    }
    fn bump(&mut self, chunk: u32) {
        self.generations[chunk as usize] += 1;
    }

    /// A datagram reached the NIC: write it into a fill frame and post it to
    /// the RX ring, or drop it.
    pub fn arrive(&mut self, datagram: Datagram) -> Result<u32, RxDrop> {
        if self.rx.len() as u32 >= self.rx_capacity {
            return Err(RxDrop::RxRingFull);
        }
        let chunk = self.fill.pop_front().ok_or(RxDrop::NoFillFrame)?;
        self.bump(chunk);
        self.rx.push_back((chunk, datagram));
        Ok(chunk)
    }

    pub fn harvest(&mut self, task: &str) -> Option<(FrameDesc, Datagram)> {
        let (chunk, datagram) = self.rx.pop_front()?;
        self.bump(chunk);
        let desc = FrameDesc {
            chunk_idx: chunk,
            data_offset: self.headroom,
            pkt_len: datagram.len,
        };
        self.held.insert(
            chunk,
            Held::Harvested {
                task: task.into(),
                datagram: datagram.clone(),
            },
        );
        Some((desc, datagram))
    }

    fn harvested(&self, task: &str, frame: FrameDesc) -> Result<&Datagram, UmemError> {
        match self.held.get(&frame.chunk_idx) {
            Some(Held::Harvested { task: t, datagram }) if t == task => {
                if frame.data_offset != self.headroom || frame.pkt_len != datagram.len {
                    return Err(UmemError::Forged {
                        chunk: frame.chunk_idx,
                    });
                }
                Ok(datagram)
            }
            _ => Err(UmemError::NotHeld {
                chunk: frame.chunk_idx,
                expected: "UmemFrame[RxHarvested]",
            }),
        }
    }

    pub fn view(&self, task: &str, frame: FrameDesc) -> Result<ViewDesc, UmemError> {
        let datagram = self.harvested(task, frame)?;
        Ok(ViewDesc {
            chunk_idx: frame.chunk_idx,
            generation: self.generations[frame.chunk_idx as usize],
            offset: self.headroom,
            len: datagram.len,
        })
    }

    /// Parse the QUIC short header under a view. `Ok(Err(code))` is a
    /// malformed header delivered to source; `Err` is a stale or forged view.
    pub fn split_short_header(
        &self,
        task: &str,
        view: ViewDesc,
    ) -> Result<Result<ShortHeader, u32>, UmemError> {
        let stale = UmemError::StaleView {
            chunk: view.chunk_idx,
        };
        let Some(&generation) = self.generations.get(view.chunk_idx as usize) else {
            return Err(stale);
        };
        let datagram = match self.held.get(&view.chunk_idx) {
            Some(Held::Harvested { task: t, datagram })
                if t == task
                    && generation == view.generation
                    && view.offset == self.headroom
                    && view.len == datagram.len =>
            {
                datagram
            }
            _ => return Err(stale),
        };
        let head = datagram.head();
        if datagram.len < SHORT_HEADER_BYTES || head.len() < SHORT_HEADER_BYTES as usize {
            return Ok(Err(ERR_TRUNCATED));
        }
        let first = head[0];
        if first & 0x80 != 0 {
            return Ok(Err(ERR_LONG_HEADER));
        }
        if first & 0x40 == 0 {
            return Ok(Err(ERR_FIXED_BIT));
        }
        let dcid = u64::from_be_bytes(head[1..9].try_into().expect("8 bytes"));
        let pn = u32::from_be_bytes(head[9..13].try_into().expect("4 bytes"));
        Ok(Ok(ShortHeader {
            first_byte: first,
            dcid,
            pkt_num_trunc: u64::from(pn),
            payload_len: datagram.len - SHORT_HEADER_BYTES,
            peer_addr: datagram.src_addr,
            peer_port: datagram.src_port,
        }))
    }

    pub fn recycle(&mut self, task: &str, frame: FrameDesc) -> Result<(), UmemError> {
        self.harvested(task, frame)?;
        self.held.remove(&frame.chunk_idx);
        self.bump(frame.chunk_idx);
        self.recycles += 1;
        let dropped = matches!(self.bug, Some(HostBug::DropOnRecycle { occurrence }) if occurrence == self.recycles);
        if !dropped {
            self.fill.push_back(frame.chunk_idx);
        }
        Ok(())
    }

    pub fn turn_around(
        &mut self,
        task: &str,
        frame: FrameDesc,
        tx_len: u32,
    ) -> Result<FrameDesc, UmemError> {
        self.harvested(task, frame)?;
        let max = self.max_datagram();
        if tx_len == 0 || tx_len > max {
            return Err(UmemError::TxLenOutOfRange { tx_len, max });
        }
        self.held.insert(
            frame.chunk_idx,
            Held::Staged {
                task: task.into(),
                tx_len,
            },
        );
        self.bump(frame.chunk_idx);
        Ok(FrameDesc {
            chunk_idx: frame.chunk_idx,
            data_offset: self.headroom,
            pkt_len: tx_len,
        })
    }

    /// Check a `submit_paced_tx` without changing anything.
    pub fn check_submit(
        &self,
        task: &str,
        frame: FrameDesc,
        pkt_num_full: u64,
    ) -> Result<u32, UmemError> {
        let tx_len = match self.held.get(&frame.chunk_idx) {
            Some(Held::Staged { task: t, tx_len }) if t == task => *tx_len,
            _ => {
                return Err(UmemError::NotHeld {
                    chunk: frame.chunk_idx,
                    expected: "UmemFrame[TxStaged]",
                });
            }
        };
        if frame.data_offset != self.headroom || frame.pkt_len != tx_len {
            return Err(UmemError::Forged {
                chunk: frame.chunk_idx,
            });
        }
        if pkt_num_full > MAX_PACKET_NUMBER {
            return Err(UmemError::PacketNumberOutOfRange { pkt_num_full });
        }
        if self.leases.len() as u64 >= self.max_leases {
            return Err(UmemError::LeaseLimit {
                limit: self.max_leases,
            });
        }
        Ok(tx_len)
    }

    /// Move a staged frame into a new lease. Call [`Self::check_submit`]
    /// first; the ACK and PTO times are decided by the caller.
    pub fn submit(
        &mut self,
        task: &str,
        frame: FrameDesc,
        pkt_num_full: u64,
        edt_ns: u64,
        ack_at_ns: Vec<u64>,
        pto_at_ns: u64,
    ) -> Result<(u64, LeaseDesc), UmemError> {
        let tx_len = self.check_submit(task, frame, pkt_num_full)?;
        let id = self.next_lease;
        self.next_lease += 1;
        self.held.insert(frame.chunk_idx, Held::Lease { lease: id });
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
                ack_at_ns,
                pto_at_ns,
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

    /// Park a lease the task holds. Returns its id.
    pub fn park(&mut self, task: &str, desc: LeaseDesc) -> Result<u64, UmemError> {
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
            || desc.held_frame.data_offset != self.headroom
            || desc.held_frame.pkt_len != lease.tx_len
        {
            return Err(UmemError::Forged { chunk });
        }
        lease.state = LeaseState::Parked;
        let id = lease.id;
        self.bump(chunk);
        Ok(id)
    }

    /// ACK or PTO for a parked lease: return its frame to the fill ring.
    /// `None` when the lease was already retired (a stale timer).
    pub fn retire(&mut self, lease: u64) -> Option<Lease> {
        if self.leases.get(&lease)?.state != LeaseState::Parked {
            return None;
        }
        let lease = self.leases.remove(&lease)?;
        self.held.remove(&lease.chunk);
        self.bump(lease.chunk);
        self.fill.push_back(lease.chunk);
        Some(lease)
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

    /// Recount every container: each chunk in exactly one place, and every
    /// lease consistent with the chunk that names it.
    pub fn check_conservation(&self) -> Result<Accounting, ConservationError> {
        let mut seen = BTreeSet::new();
        let mut a = Accounting {
            total: self.frames,
            ..Accounting::default()
        };
        let mut place = |chunk: u32| {
            if chunk >= self.frames {
                return Err(ConservationError::Unknown { chunk });
            }
            if !seen.insert(chunk) {
                return Err(ConservationError::Duplicated { chunk });
            }
            Ok(())
        };
        for chunk in &self.fill {
            place(*chunk)?;
            a.fill += 1;
        }
        for (chunk, _) in &self.rx {
            place(*chunk)?;
            a.rx += 1;
        }
        for (chunk, held) in &self.held {
            place(*chunk)?;
            match held {
                Held::Harvested { .. } => a.harvested += 1,
                Held::Staged { .. } => a.staged += 1,
                Held::Lease { lease } => match self.leases.get(lease) {
                    Some(l) if l.chunk == *chunk => match l.state {
                        LeaseState::Leased => a.leased += 1,
                        LeaseState::Parked => a.parked += 1,
                    },
                    _ => return Err(ConservationError::LeaseMismatch { lease: *lease }),
                },
            }
        }
        for (id, lease) in &self.leases {
            if self.held.get(&lease.chunk) != Some(&Held::Lease { lease: *id }) {
                return Err(ConservationError::LeaseMismatch { lease: *id });
            }
        }
        if let Some(chunk) = (0..self.frames).find(|c| !seen.contains(c)) {
            return Err(ConservationError::Missing { chunk });
        }
        Ok(a)
    }
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
        }
    }

    #[test]
    fn harvest_recycle_reply_and_retire_conserve_frames() {
        let mut u = Umem::new(4, 2048, 256, 2, 4);
        assert_eq!(u.check_conservation().unwrap().fill, 4);
        u.arrive(datagram(1, short_header(0x43, 7, 9), 100))
            .unwrap();
        u.arrive(datagram(2, vec![0x43, 1], 2)).unwrap();
        assert_eq!(u.arrive(datagram(3, vec![], 1)), Err(RxDrop::RxRingFull));
        let (frame, _) = u.harvest("t").unwrap();
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
        // The view is stale once the chunk changed state.
        assert_eq!(
            u.split_short_header("t", view),
            Err(UmemError::StaleView { chunk: 0 })
        );
        assert!(u.recycle("t", frame).is_err());
        let (lease, desc) = u.submit("t", tx, 5, 10, vec![20], 30).unwrap();
        assert_eq!(u.held_by("t"), vec![0]);
        assert_eq!(u.retire(lease), None, "an unparked lease cannot retire");
        u.park("t", desc).unwrap();
        assert!(u.held_by("t").is_empty());
        let a = u.check_conservation().unwrap();
        assert_eq!((a.fill, a.rx, a.parked), (2, 1, 1));
        let (second, _) = u.harvest("t").unwrap();
        let v = u.view("t", second).unwrap();
        assert_eq!(u.split_short_header("t", v).unwrap(), Err(ERR_TRUNCATED));
        u.recycle("t", second).unwrap();
        assert!(u.retire(lease).is_some());
        assert_eq!(u.retire(lease), None);
        assert_eq!(u.check_conservation().unwrap().fill, 4);
    }

    #[test]
    fn header_errors_bounds_and_a_dropping_binding() {
        let mut u = Umem::new(2, 512, 0, 2, 1);
        u.arrive(datagram(1, short_header(0xc3, 1, 1), 50)).unwrap();
        u.arrive(datagram(2, short_header(0x03, 1, 1), 50)).unwrap();
        let (a, _) = u.harvest("t").unwrap();
        let (b, _) = u.harvest("t").unwrap();
        assert_eq!(
            u.split_short_header("t", u.view("t", a).unwrap()).unwrap(),
            Err(ERR_LONG_HEADER)
        );
        assert_eq!(
            u.split_short_header("t", u.view("t", b).unwrap()).unwrap(),
            Err(ERR_FIXED_BIT)
        );
        assert_eq!(
            u.turn_around("t", a, 513),
            Err(UmemError::TxLenOutOfRange {
                tx_len: 513,
                max: 512
            })
        );
        let tx = u.turn_around("t", a, 10).unwrap();
        assert_eq!(
            u.check_submit("t", tx, 1 << 62),
            Err(UmemError::PacketNumberOutOfRange {
                pkt_num_full: 1 << 62
            })
        );
        let (_, desc) = u.submit("t", tx, 1, 0, vec![], 5).unwrap();
        u.set_bug(Some(HostBug::DropOnRecycle { occurrence: 1 }));
        u.recycle("t", b).unwrap();
        assert_eq!(
            u.check_conservation(),
            Err(ConservationError::Missing { chunk: 1 })
        );
        u.park("t", desc).unwrap();
    }
}
