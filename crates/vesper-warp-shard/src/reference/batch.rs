//! The Rust build of the batch update (frozen; the Vesper build is
//! `vesper/src/batch.ves`).

use crate::batch::{ConnEvent, ConnEventDesc, MAX_BATCH};
use crate::error::BatchError;
use crate::reference::table::{ConnTable, NO_STAGE, Stage};
use crate::simd::{Kernels, LANES};
use crate::table::PN_LIMIT;
use std::hash::BuildHasher;

#[derive(Clone, Copy)]
struct Agg {
    index: u32,
    first: usize,
    sent: u64,
    released: u64,
    max_pn: Option<u64>,
    cwnd: Option<u32>,
}

const AGG0: Agg = Agg {
    index: 0,
    first: 0,
    sent: 0,
    released: 0,
    max_pn: None,
    cwnd: None,
};

fn check_event(i: usize, event: ConnEvent) -> Result<(), BatchError> {
    match event {
        ConnEvent::PacketReceived { pn } if pn >= PN_LIMIT => {
            Err(BatchError::PacketNumberRange { index: i, pn })
        }
        _ => Ok(()),
    }
}

impl<S: BuildHasher> ConnTable<S> {
    /// Applies a batch with the table's vector kernels.
    pub fn apply_batch(&mut self, batch: &[ConnEventDesc]) -> Result<(), BatchError> {
        self.apply_batch_with(self.kernels, batch)
    }

    /// Scalar reference for [`ConnTable::apply_batch`].
    pub fn apply_batch_scalar(&mut self, batch: &[ConnEventDesc]) -> Result<(), BatchError> {
        if batch.len() > MAX_BATCH {
            return Err(BatchError::TooLong { len: batch.len() });
        }
        let mut aggs = [AGG0; MAX_BATCH];
        let mut n = 0;
        for (i, d) in batch.iter().enumerate() {
            self.check(d.slot)
                .map_err(|_| BatchError::StaleHandle { index: i })?;
            check_event(i, d.event)?;
            let a = match aggs[..n].iter().position(|a| a.index == d.slot.index()) {
                Some(p) => p,
                None => {
                    aggs[n] = Agg {
                        index: d.slot.index(),
                        first: i,
                        ..AGG0
                    };
                    n += 1;
                    n - 1
                }
            };
            let a = &mut aggs[a];
            // Sums of at most 64 u32 values stay below 2^38: no overflow.
            match d.event {
                ConnEvent::PacketReceived { pn } => {
                    a.max_pn = Some(a.max_pn.map_or(pn, |m| m.max(pn)))
                }
                ConnEvent::PacketSent { bytes } => a.sent += u64::from(bytes),
                ConnEvent::BytesAcked { bytes } | ConnEvent::BytesLost { bytes } => {
                    a.released += u64::from(bytes)
                }
                ConnEvent::CwndUpdate { cwnd_bytes } => a.cwnd = Some(cwnd_bytes),
            }
        }
        let mut results = [0u32; MAX_BATCH];
        for (a, r) in aggs[..n].iter().zip(results.iter_mut()) {
            let (t, l) = (a.index as usize / LANES, a.index as usize % LANES);
            let total = u64::from(self.hot[t].bytes_in_flight[l]) + a.sent;
            let net = total
                .checked_sub(a.released)
                .ok_or(BatchError::BytesInFlightUnderflow { index: a.first })?;
            *r = u32::try_from(net)
                .map_err(|_| BatchError::BytesInFlightOverflow { index: a.first })?;
        }
        for (a, &r) in aggs[..n].iter().zip(results.iter()) {
            let (t, l) = (a.index as usize / LANES, a.index as usize % LANES);
            let hot = &mut self.hot[t];
            hot.bytes_in_flight[l] = r;
            if let Some(pn) = a.max_pn {
                // pn < 2^62 checked above, so pn + 1 does not overflow.
                hot.largest_rx_pn_p1[l] = hot.largest_rx_pn_p1[l].max(pn + 1);
            }
            if let Some(c) = a.cwnd {
                hot.cwnd_bytes[l] = c;
            }
        }
        Ok(())
    }

    /// Applies a batch with explicit kernels.
    pub fn apply_batch_with(
        &mut self,
        kernels: Kernels,
        batch: &[ConnEventDesc],
    ) -> Result<(), BatchError> {
        if batch.len() > MAX_BATCH {
            return Err(BatchError::TooLong { len: batch.len() });
        }
        let mut n = 0usize;
        let outcome = self
            .scatter(batch, &mut n)
            .and_then(|()| self.stage_compute(kernels, n));
        if outcome.is_ok() {
            self.stage_commit(kernels, n);
        }
        for s in 0..n {
            let tile = self.stages[s].tile as usize;
            self.tile_stage[tile] = NO_STAGE;
        }
        outcome
    }

    fn scatter(&mut self, batch: &[ConnEventDesc], n: &mut usize) -> Result<(), BatchError> {
        for (i, d) in batch.iter().enumerate() {
            let (t, l) = self
                .check(d.slot)
                .map_err(|_| BatchError::StaleHandle { index: i })?;
            check_event(i, d.event)?;
            let s = match self.tile_stage[t] {
                NO_STAGE => {
                    let s = *n;
                    self.stages[s] = Stage {
                        // Tile count is below 2^23, fits u32.
                        tile: t as u32,
                        ..Stage::EMPTY
                    };
                    // s < MAX_BATCH = 64 < NO_STAGE.
                    self.tile_stage[t] = s as u8;
                    *n += 1;
                    s
                }
                s => usize::from(s),
            };
            let st = &mut self.stages[s];
            let bit = 1u16 << l;
            if st.touched & bit == 0 {
                st.touched |= bit;
                // i < MAX_BATCH = 64 fits u8.
                st.first[l] = i as u8;
            }
            // Sums of at most 64 u32 values stay below 2^38: no overflow.
            match d.event {
                ConnEvent::PacketReceived { pn } => {
                    st.max_pn_p1[l] = st.max_pn_p1[l].max(pn + 1);
                }
                ConnEvent::PacketSent { bytes } => st.sent[l] += u64::from(bytes),
                ConnEvent::BytesAcked { bytes } | ConnEvent::BytesLost { bytes } => {
                    st.released[l] += u64::from(bytes)
                }
                ConnEvent::CwndUpdate { cwnd_bytes } => {
                    st.cwnd[l] = cwnd_bytes;
                    st.cwnd_set |= bit;
                }
            }
        }
        Ok(())
    }

    fn stage_compute(&mut self, k: Kernels, n: usize) -> Result<(), BatchError> {
        const U32_MAX: [u64; LANES] = [u32::MAX as u64; LANES];
        let mut worst: Option<BatchError> = None;
        let mut worst_first = usize::MAX;
        for s in 0..n {
            let tile = self.stages[s].tile as usize;
            let mut t = [0u64; LANES];
            for (dst, &src) in t.iter_mut().zip(&self.hot[tile].bytes_in_flight) {
                *dst = u64::from(src);
            }
            let st = &mut self.stages[s];
            // bif < 2^32 and sent < 2^38, so the add cannot wrap.
            k.add_u64(&mut t, &st.sent);
            let under = k.gt_mask_u64(&st.released, &t) & st.touched;
            // Lanes in `under` wrap here; they are rejected below.
            k.sub_u64(&mut t, &st.released);
            let over = k.gt_mask_u64(&t, &U32_MAX) & st.touched & !under;
            let mut bad = under | over;
            while bad != 0 {
                let l = bad.trailing_zeros() as usize;
                let first = usize::from(st.first[l]);
                if first < worst_first {
                    worst_first = first;
                    worst = Some(if under & (1 << l) != 0 {
                        BatchError::BytesInFlightUnderflow { index: first }
                    } else {
                        BatchError::BytesInFlightOverflow { index: first }
                    });
                }
                bad &= bad - 1;
            }
            st.result_bif = t;
        }
        worst.map_or(Ok(()), Err)
    }

    fn stage_commit(&mut self, k: Kernels, n: usize) {
        for s in 0..n {
            let st = &self.stages[s];
            let hot = &mut self.hot[st.tile as usize];
            for l in 0..LANES {
                if st.touched & (1 << l) != 0 {
                    // Range-checked in stage_compute.
                    hot.bytes_in_flight[l] = st.result_bif[l] as u32;
                }
                if st.cwnd_set & (1 << l) != 0 {
                    hot.cwnd_bytes[l] = st.cwnd[l];
                }
            }
            k.max_u64(&mut hot.largest_rx_pn_p1, &st.max_pn_p1);
        }
    }
}
