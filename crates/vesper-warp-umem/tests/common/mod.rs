//! Shared test harness: a deterministic RNG and an echo application that
//! drives every `XdpNic` path against the software NIC.
#![allow(dead_code, reason = "each test binary uses a subset")]

use vesper_warp_umem::{
    DescError, Fault, InFlightQuicPkt, RetireOutcome, SoftXsk, TxStaged, UmemError, UmemFrame,
    XdpNic,
};

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// A packet whose first byte is 0xFF is "malformed" at the application
/// level (as if header split or CID lookup failed) and is recycled.
pub const MALFORMED: u8 = 0xFF;

#[derive(Default, Debug)]
pub struct EchoStats {
    pub harvested: u64,
    pub rejected: u64,
    /// OutOfUmem, HeadroomOverlap, OverLength, ZeroLength.
    pub rejected_kinds: [u64; 4],
    pub malformed_recycled: u64,
    pub replied: u64,
    pub backpressured: u64,
    pub retired_early: u64,
    pub retired_late: u64,
}

/// The application half: harvest, recycle malformed, turn around and reply,
/// retire leases (some before their completion), replenish the fill ring.
pub struct Echo {
    pub leases: Vec<InFlightQuicPkt>,
    pub staged: Vec<UmemFrame<TxStaged>>,
    pub next_pn: u64,
    pub stats: EchoStats,
}

impl Echo {
    pub fn new(capacity: usize) -> Self {
        Self {
            leases: Vec::with_capacity(capacity),
            staged: Vec::with_capacity(capacity),
            next_pn: 0,
            stats: EchoStats::default(),
        }
    }

    /// One application step. `check` runs after every individual operation.
    pub fn step(&mut self, xsk: &mut SoftXsk, rng: &mut Rng, check: &mut dyn FnMut(&mut SoftXsk)) {
        // Retry staged frames first (backpressure from a full tx ring).
        while let Some(frame) = self.staged.pop() {
            match self.submit(xsk, frame, rng) {
                Ok(()) => check(xsk),
                Err(frame) => {
                    self.staged.push(frame);
                    break;
                }
            }
        }
        for _ in 0..rng.below(8) + 1 {
            match xsk.harvest_rx() {
                Ok(Some(mut frame)) => {
                    self.stats.harvested += 1;
                    let first = xsk.view_frame_bytes(&frame).unwrap()[0];
                    if first == MALFORMED {
                        xsk.recycle_to_fill_ring(frame).unwrap();
                        self.stats.malformed_recycled += 1;
                    } else {
                        xsk.rx_bytes_mut(&mut frame).unwrap()[0] ^= 0x80;
                        let len = frame.pkt_len().clamp(1, 1200);
                        let tx = xsk.turn_around_tx_in_place(frame, len).unwrap();
                        if let Err(tx) = self.submit(xsk, tx, rng) {
                            self.staged.push(tx);
                        }
                    }
                }
                Ok(None) => break,
                Err(rejected) => {
                    assert_eq!(rejected.recycled, rejected.chunk.is_some());
                    self.stats.rejected += 1;
                    let kind = match rejected.reason {
                        DescError::OutOfUmem { .. } => 0,
                        DescError::HeadroomOverlap { .. } => 1,
                        DescError::OverLength { .. } => 2,
                        DescError::ZeroLength => 3,
                        other => panic!("the software NIC never produces {other:?}"),
                    };
                    self.stats.rejected_kinds[kind] += 1;
                }
            }
            check(xsk);
        }
        xsk.harvest_completions(rng.below(16) as u32 + 1);
        check(xsk);
        // Retire a random subset: early ones (before completion) are freed
        // by their completion later.
        let mut i = 0;
        while i < self.leases.len() {
            if rng.chance(40) {
                let lease = self.leases.swap_remove(i);
                match xsk.retire(lease).unwrap() {
                    RetireOutcome::Freed => self.stats.retired_late += 1,
                    RetireOutcome::AwaitingCompletion => self.stats.retired_early += 1,
                }
                check(xsk);
            } else {
                i += 1;
            }
        }
        // Occasionally originate a fresh packet.
        if rng.chance(20)
            && let Ok(mut fresh) = xsk.alloc_tx(64)
        {
            xsk.tx_bytes_mut(&mut fresh).unwrap()[0] = 0x42;
            if rng.chance(10) {
                xsk.abandon_tx(fresh).unwrap();
            } else if let Err(fresh) = self.submit(xsk, fresh, rng) {
                self.staged.push(fresh);
            }
            check(xsk);
        }
        xsk.replenish_fill(rng.below(32) as u32 + 1);
        check(xsk);
    }

    fn submit(
        &mut self,
        xsk: &mut SoftXsk,
        frame: UmemFrame<TxStaged>,
        rng: &mut Rng,
    ) -> Result<(), UmemFrame<TxStaged>> {
        let pn = self.next_pn;
        match xsk.submit_paced_tx(frame, pn, 1_000 * pn + rng.below(1000)) {
            Ok(lease) => {
                assert_eq!(lease.pkt_num_full(), pn);
                self.next_pn += 1;
                self.stats.replied += 1;
                self.leases.push(lease);
                Ok(())
            }
            Err(refused) => {
                assert_eq!(refused.error, UmemError::TxRingFull);
                self.stats.backpressured += 1;
                Err(refused.into_owner())
            }
        }
    }

    /// Consumes every owner the application still holds.
    pub fn drain(&mut self, xsk: &mut SoftXsk) {
        for frame in self.staged.drain(..) {
            xsk.abandon_tx(frame).unwrap();
        }
        for lease in self.leases.drain(..) {
            xsk.retire(lease).unwrap();
        }
    }
}

pub fn random_fault(rng: &mut Rng) -> Fault {
    match rng.below(7) {
        0 => Fault::RxOutOfRange,
        1 => Fault::RxHeadroomOverlap,
        2 => Fault::RxOverLength,
        3 => Fault::RxZeroLength,
        4 => Fault::CompletionOutOfRange,
        5 => Fault::StallTx(rng.below(5) as u32 + 1),
        _ => Fault::StallCompletion(rng.below(5) as u32 + 1),
    }
}

pub fn random_packet(rng: &mut Rng, max: usize) -> Vec<u8> {
    let len = rng.below(max as u64) as usize + 1;
    let mut packet: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    packet[0] = if rng.chance(25) { MALFORMED } else { 0x01 };
    packet
}
