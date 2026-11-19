//! [`crate::ack`] forwarded to `vesper/src/ack.ves`: the range array is
//! lent in place and the rest of the state is the Vesper `RxState` mirror.

use super::binding as b;
use crate::PnSpace;
use crate::ack::{AckError, Ecn, Received, ReceivedPackets};
use crate::frame::{AckFrame, AckRange, EcnCounts};

fn ack_error(e: b::AckErr) -> AckError {
    match e {
        b::AckErr::Duplicate { pn } => AckError::Duplicate(pn),
        b::AckErr::TooOld { pn } => AckError::TooOld(pn),
        b::AckErr::OutOfRange { pn } => AckError::OutOfRange(pn),
        b::AckErr::ClockWentBackwards => AckError::ClockWentBackwards,
        b::AckErr::BadAckDelayExponent { exponent } => AckError::BadAckDelayExponent(exponent),
    }
}

impl<const N: usize> ReceivedPackets<N> {
    /// `max_ack_delay_ns` is this endpoint's advertised `max_ack_delay`
    /// (ignored for Initial and Handshake, which are acknowledged at once).
    pub fn new(space: PnSpace, max_ack_delay_ns: u64) -> Self {
        assert!(N > 0, "at least one ACK range");
        Self {
            state: b::received_new(space.index() as u8, max_ack_delay_ns),
            ranges: [AckRange {
                smallest: 0,
                largest: 0,
            }; N],
        }
    }

    /// Largest packet number received in this space (the `largest_pn` for
    /// packet-number decoding); unaffected by range eviction or pruning.
    pub fn largest(&self) -> Option<u64> {
        self.state.has_largest.get().then_some(self.state.largest)
    }
    pub fn ranges(&self) -> &[AckRange] {
        &self.ranges[..self.state.len as usize]
    }
    pub fn ecn_counts(&self) -> EcnCounts {
        EcnCounts {
            ect0: self.state.ect0,
            ect1: self.state.ect1,
            ce: self.state.ce,
        }
    }
    /// When an ACK must be sent, if one is owed.
    pub fn ack_deadline(&self) -> Option<u64> {
        self.state
            .has_deadline
            .get()
            .then_some(self.state.deadline_ns)
    }

    /// Would `pn` be rejected as a duplicate (without recording it)?
    pub fn check(&self, pn: u64) -> Result<(), AckError> {
        b::received_check(&self.state, &self.ranges, pn).map_err(ack_error)
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
        let ecn = match ecn {
            Ecn::NotEct => 0,
            Ecn::Ect0 => 1,
            Ecn::Ect1 => 2,
            Ecn::Ce => 3,
        };
        let r = b::received_on_packet(
            &mut self.state,
            &mut self.ranges,
            pn,
            now_ns,
            ack_eliciting,
            ecn,
        )
        .map_err(ack_error)?;
        Ok(Received {
            largest: r.largest.get(),
            out_of_order: r.out_of_order.get(),
        })
    }

    /// The ACK frame to send now, or `None` if nothing was received. The
    /// delay is measured from reception of the largest packet.
    pub fn ack_frame(
        &self,
        now_ns: u64,
        ack_delay_exponent: u8,
    ) -> Result<Option<AckFrame<'_>>, AckError> {
        let Some(ack_delay) =
            b::received_ack_delay(&self.state, now_ns, ack_delay_exponent).map_err(ack_error)?
        else {
            return Ok(None);
        };
        let ecn = self.ecn_counts();
        let ecn = (ecn != EcnCounts::default()).then_some(ecn);
        // The stored ranges satisfy from_ranges' invariants by construction.
        Ok(AckFrame::from_ranges(self.ranges(), ack_delay, ecn).ok())
    }

    /// Call when an ACK frame for this space has been sent.
    pub fn on_ack_sent(&mut self) {
        b::received_on_ack_sent(&mut self.state);
    }

    /// RFC 9000 §13.2.4: the peer acknowledged a packet carrying our ACK
    /// whose largest acknowledged was `largest_in_our_ack`; stop tracking
    /// everything at or below it (later arrivals there are [`AckError::TooOld`]).
    pub fn on_ack_of_ack(&mut self, largest_in_our_ack: u64) {
        b::received_on_ack_of_ack(&mut self.state, &mut self.ranges, largest_in_our_ack);
    }
}
