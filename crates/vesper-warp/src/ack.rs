//! Per-packet-number-space received-packet tracking (RFC 9000 §13.2).
//!
//! [`ReceivedPackets`] keeps at most `N` disjoint received ranges in a fixed
//! array (no heap), detects duplicates, and produces ACK frames whose range
//! count is bounded by `N`. When a new range would exceed `N`, the lowest
//! range is forgotten and every packet number at or below it is thereafter
//! reported as [`AckError::TooOld`] rather than risk processing a duplicate
//! (RFC 9000 §13.2.3 permits not acknowledging old ranges; §12.3 requires
//! duplicate suppression, which this conservatively over-approximates).

use crate::frame::{AckFrame, AckRange, EcnCounts};
use crate::{PN_LIMIT, PnSpace};

/// Default ACK range bound.
pub const DEFAULT_MAX_RANGES: usize = 32;
/// RFC 9000 §18.2 default `ack_delay_exponent`.
pub const DEFAULT_ACK_DELAY_EXPONENT: u8 = 3;
/// RFC 9000 §13.2.2: acknowledge at least every second ack-eliciting packet.
pub const ACK_ELICITING_THRESHOLD: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckError {
    /// Already received.
    Duplicate(u64),
    /// At or below the forgotten floor: possibly a duplicate, so dropped.
    TooOld(u64),
    OutOfRange(u64),
    /// Reception time earlier than a previous one.
    ClockWentBackwards,
    /// `ack_delay_exponent` above 20 (RFC 9000 §18.2).
    BadAckDelayExponent(u8),
}

/// The ECN codepoint a packet arrived with (from the IP header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecn {
    NotEct,
    Ect0,
    Ect1,
    Ce,
}

/// What a newly received packet means for acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The packet is the new largest in the space.
    pub largest: bool,
    /// The packet is below the previous largest or leaves a gap: RFC 9000
    /// §13.2.1 asks for an immediate ACK.
    pub out_of_order: bool,
}

#[derive(Debug, Clone)]
pub struct ReceivedPackets<const N: usize = DEFAULT_MAX_RANGES> {
    space: PnSpace,
    /// Descending, disjoint, non-adjacent; `ranges[..len]` are live.
    ranges: [AckRange; N],
    len: usize,
    /// Packet numbers `<= floor` are no longer tracked.
    floor: Option<u64>,
    largest_received_ns: u64,
    last_received_ns: u64,
    ecn: EcnCounts,
    unacked_ack_eliciting: u32,
    ack_deadline_ns: Option<u64>,
    max_ack_delay_ns: u64,
}

impl<const N: usize> ReceivedPackets<N> {
    /// `max_ack_delay_ns` is this endpoint's advertised `max_ack_delay`
    /// (ignored for Initial and Handshake, which are acknowledged at once).
    pub const fn new(space: PnSpace, max_ack_delay_ns: u64) -> Self {
        assert!(N > 0, "at least one ACK range");
        Self {
            space,
            ranges: [AckRange {
                smallest: 0,
                largest: 0,
            }; N],
            len: 0,
            floor: None,
            largest_received_ns: 0,
            last_received_ns: 0,
            ecn: EcnCounts {
                ect0: 0,
                ect1: 0,
                ce: 0,
            },
            unacked_ack_eliciting: 0,
            ack_deadline_ns: None,
            max_ack_delay_ns,
        }
    }

    pub fn largest(&self) -> Option<u64> {
        (self.len > 0).then(|| self.ranges[0].largest)
    }
    pub fn ranges(&self) -> &[AckRange] {
        &self.ranges[..self.len]
    }
    pub const fn ecn_counts(&self) -> EcnCounts {
        self.ecn
    }
    /// When an ACK must be sent, if one is owed.
    pub const fn ack_deadline(&self) -> Option<u64> {
        self.ack_deadline_ns
    }

    /// Would `pn` be rejected as a duplicate (without recording it)?
    pub fn check(&self, pn: u64) -> Result<(), AckError> {
        if pn >= PN_LIMIT {
            return Err(AckError::OutOfRange(pn));
        }
        if self.floor.is_some_and(|f| pn <= f) {
            return Err(AckError::TooOld(pn));
        }
        if self
            .ranges()
            .iter()
            .any(|r| r.smallest <= pn && pn <= r.largest)
        {
            return Err(AckError::Duplicate(pn));
        }
        Ok(())
    }

    /// Records a successfully opened packet. Call only after decryption
    /// succeeded (RFC 9000 §12.3), and drop the packet on any error.
    pub fn on_packet_received(
        &mut self,
        pn: u64,
        now_ns: u64,
        ack_eliciting: bool,
        ecn: Ecn,
    ) -> Result<Received, AckError> {
        self.check(pn)?;
        if now_ns < self.last_received_ns {
            return Err(AckError::ClockWentBackwards);
        }
        self.last_received_ns = now_ns;
        let previous = self.largest();
        let largest = previous.is_none_or(|l| pn > l);
        let out_of_order = match previous {
            None => false,
            Some(l) => pn < l || pn > l + 1,
        };
        if largest {
            self.largest_received_ns = now_ns;
        }
        self.insert(pn);
        match ecn {
            Ecn::NotEct => {}
            Ecn::Ect0 => self.ecn.ect0 += 1,
            Ecn::Ect1 => self.ecn.ect1 += 1,
            Ecn::Ce => self.ecn.ce += 1,
        }
        if ack_eliciting {
            self.unacked_ack_eliciting += 1;
            let immediate = self.space != PnSpace::ApplicationData
                || out_of_order
                || ecn == Ecn::Ce
                || self.unacked_ack_eliciting >= ACK_ELICITING_THRESHOLD;
            let deadline = if immediate {
                now_ns
            } else {
                now_ns.saturating_add(self.max_ack_delay_ns)
            };
            self.ack_deadline_ns = Some(self.ack_deadline_ns.map_or(deadline, |d| d.min(deadline)));
        }
        Ok(Received {
            largest,
            out_of_order,
        })
    }

    fn insert(&mut self, pn: u64) {
        // Index of the first range whose largest is below pn.
        let i = self
            .ranges()
            .iter()
            .position(|r| r.largest < pn)
            .unwrap_or(self.len);
        let joins_above = i > 0 && self.ranges[i - 1].smallest == pn + 1;
        let joins_below = i < self.len && self.ranges[i].largest + 1 == pn;
        match (joins_above, joins_below) {
            (true, true) => {
                self.ranges[i - 1].smallest = self.ranges[i].smallest;
                self.ranges.copy_within(i + 1..self.len, i);
                self.len -= 1;
            }
            (true, false) => self.ranges[i - 1].smallest = pn,
            (false, true) => self.ranges[i].largest = pn,
            (false, false) => {
                if self.len == N {
                    // Forget the lowest range (possibly the new one).
                    let lowest = if i == N {
                        pn
                    } else {
                        let dropped = self.ranges[N - 1].largest;
                        self.ranges.copy_within(i..N - 1, i + 1);
                        self.ranges[i] = AckRange {
                            smallest: pn,
                            largest: pn,
                        };
                        dropped
                    };
                    self.floor = Some(self.floor.map_or(lowest, |f| f.max(lowest)));
                } else {
                    self.ranges.copy_within(i..self.len, i + 1);
                    self.ranges[i] = AckRange {
                        smallest: pn,
                        largest: pn,
                    };
                    self.len += 1;
                }
            }
        }
    }

    /// The ACK frame to send now, or `None` if nothing was received. The
    /// delay is measured from reception of the largest packet.
    pub fn ack_frame(
        &self,
        now_ns: u64,
        ack_delay_exponent: u8,
    ) -> Result<Option<AckFrame<'_>>, AckError> {
        if ack_delay_exponent > 20 {
            return Err(AckError::BadAckDelayExponent(ack_delay_exponent));
        }
        if self.len == 0 {
            return Ok(None);
        }
        let delay_us = now_ns.saturating_sub(self.largest_received_ns) / 1_000;
        let ack_delay = (delay_us >> ack_delay_exponent).min(crate::varint::MAX);
        let ecn = (self.ecn != EcnCounts::default()).then_some(self.ecn);
        // The stored ranges satisfy from_ranges' invariants by construction.
        Ok(AckFrame::from_ranges(self.ranges(), ack_delay, ecn).ok())
    }

    /// Call when an ACK frame for this space has been sent.
    pub fn on_ack_sent(&mut self) {
        self.unacked_ack_eliciting = 0;
        self.ack_deadline_ns = None;
    }

    /// RFC 9000 §13.2.4: the peer acknowledged a packet carrying our ACK
    /// whose largest acknowledged was `largest_in_our_ack`; stop tracking
    /// everything at or below it (later arrivals there are [`AckError::TooOld`]).
    pub fn on_ack_of_ack(&mut self, largest_in_our_ack: u64) {
        let x = largest_in_our_ack;
        if let Some(j) = self.ranges().iter().position(|r| r.smallest <= x) {
            if self.ranges[j].largest > x {
                self.ranges[j].smallest = x + 1;
                self.len = j + 1;
            } else {
                self.len = j;
            }
        }
        self.floor = Some(
            self.floor
                .map_or(largest_in_our_ack, |f| f.max(largest_in_our_ack)),
        );
    }
}
