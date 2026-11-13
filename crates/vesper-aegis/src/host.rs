//! Host-shared versus private memory, single-fetch snapshots, and Iago
//! validators for values returned across VM exits.
//!
//! The partition is by type: [`HostBounceSlice`] is the only view of
//! shared (unencrypted) memory and offers no `&[u8]`; [`PrivateRegion`] and
//! [`PrivateSlice`] are private (encrypted) guest memory; the only way to
//! write into shared memory is [`BounceAperture::flush_ciphertext_and_release`],
//! which takes a [`CiphertextSlice`] that can only be built from a sealed
//! AEAD output. Misrouting plaintext to the host is therefore a type error:
//!
//! ```compile_fail
//! use vesper_aegis::host::*;
//! let mut aperture = BounceAperture::new(1, 64);
//! let grant = aperture.grant(0).unwrap();
//! let mut region = PrivateRegion::with_capacity(64);
//! let plain = region.alloc(4).unwrap();
//! // A private plaintext slice is not a CiphertextSlice.
//! aperture.flush_ciphertext_and_release(grant, plain);
//! ```
//!
//! ```compile_fail
//! use vesper_aegis::host::*;
//! // There is no constructor from raw bytes.
//! let c = CiphertextSlice::from(&[0u8; 4][..]);
//! ```
//!
//! ```compile_fail
//! use vesper_aegis::host::*;
//! let mut aperture = BounceAperture::new(1, 64);
//! let grant = aperture.grant(0).unwrap();
//! aperture.abort_and_scrub_bounce(grant);
//! // The grant is linear: it was consumed above.
//! aperture.abort_and_scrub_bounce(grant);
//! ```
//!
//! ```compile_fail
//! use vesper_aegis::host::*;
//! let v = UntrustedHostVal::from_host(5u32);
//! // The raw host value is private; it must go through a validator.
//! let n: u32 = v.raw_bits;
//! ```
//!
//! Shared memory is modelled with atomics so that a concurrent
//! "hypervisor" thread can mutate it without undefined behaviour. This is a
//! software model of the protocol, not an encrypted-memory implementation.

use crate::error::TeeError;
use crate::gpa::IntervalSet;
use crate::linear::{Ledger, Obligation};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// A value returned by the untrusted host (VMGEXIT, #VE, RSI host call).
/// The raw bits are private; validators are the only way out.
#[derive(Clone, Copy, Debug)]
pub struct UntrustedHostVal<T: Copy> {
    raw_bits: T,
}

impl<T: Copy> UntrustedHostVal<T> {
    pub fn from_host(raw_bits: T) -> Self {
        UntrustedHostVal { raw_bits }
    }

    /// Run a caller-supplied validator over the raw value.
    pub fn validate<U, E>(self, check: impl FnOnce(T) -> Result<U, E>) -> Result<U, E> {
        check(self.raw_bits)
    }
}

impl UntrustedHostVal<u32> {
    /// A length in `0..=cap`.
    pub fn validate_len(self, cap: u32) -> Result<u32, TeeError> {
        if self.raw_bits > cap {
            return Err(TeeError::HostLengthOutOfRange {
                reported: self.raw_bits.into(),
                cap: cap.into(),
            });
        }
        Ok(self.raw_bits)
    }
}

impl UntrustedHostVal<u64> {
    /// A transferred byte count, which must not exceed what was requested.
    pub fn validate_byte_count(self, requested: u64) -> Result<u64, TeeError> {
        if self.raw_bits > requested {
            return Err(TeeError::HostByteCountExceedsRequest {
                reported: self.raw_bits,
                requested,
            });
        }
        Ok(self.raw_bits)
    }
}

/// Rejects host clocks that go backwards or jump too far (time is `u64`
/// nanoseconds on the host-provided clock being validated).
#[derive(Clone, Debug)]
pub struct HostClockValidator {
    last: Option<u64>,
    max_step: u64,
}

impl HostClockValidator {
    pub fn new(max_step_ns: u64) -> Self {
        HostClockValidator {
            last: None,
            max_step: max_step_ns,
        }
    }

    pub fn accept(&mut self, reported: UntrustedHostVal<u64>) -> Result<u64, TeeError> {
        let t = reported.raw_bits;
        if let Some(prev) = self.last {
            if t < prev {
                return Err(TeeError::HostTimeNonMonotonic {
                    previous: prev,
                    reported: t,
                });
            }
            if t - prev > self.max_step {
                return Err(TeeError::HostTimeJump {
                    previous: prev,
                    reported: t,
                    max_step: self.max_step,
                });
            }
        }
        self.last = Some(t);
        Ok(t)
    }
}

/// A host reply to a mapping request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostMmapReply {
    pub addr: u64,
    pub len: u64,
}

/// Validates host mapping replies against a guest-chosen window and the
/// mappings already handed out (the classic Iago `mmap` overlap attack).
#[derive(Clone, Debug)]
pub struct HostMappingTable {
    window_start: u64,
    window_end: u64,
    mapped: IntervalSet,
}

impl HostMappingTable {
    /// Host mappings must lie in `[window_start, window_end)`, page aligned.
    pub fn new(window_start: u64, window_end: u64) -> Self {
        HostMappingTable {
            window_start,
            window_end,
            mapped: IntervalSet::default(),
        }
    }

    pub fn accept_mmap(
        &mut self,
        reply: UntrustedHostVal<HostMmapReply>,
        requested_len: u64,
    ) -> Result<HostMmapReply, TeeError> {
        let HostMmapReply { addr, len } = reply.raw_bits;
        let invalid = TeeError::HostMmapInvalid { addr, len };
        if len == 0
            || len != requested_len
            || !addr.is_multiple_of(4096)
            || !len.is_multiple_of(4096)
        {
            return Err(invalid);
        }
        let end = addr.checked_add(len).ok_or(invalid.clone())?;
        if addr < self.window_start || end > self.window_end {
            return Err(invalid);
        }
        if self.mapped.first_overlap(addr / 4096, end / 4096).is_some() {
            return Err(TeeError::HostMmapOverlap { addr, len });
        }
        self.mapped.insert(addr / 4096, end / 4096);
        Ok(HostMmapReply { addr, len })
    }

    /// Record an unmap the guest itself requested.
    pub fn unmap(&mut self, m: HostMmapReply) {
        self.mapped.remove(m.addr / 4096, (m.addr + m.len) / 4096);
    }
}

/// Shared (unencrypted) SWIOTLB-style memory, visible to the host.
#[derive(Debug)]
struct SharedMemory {
    bytes: Box<[AtomicU8]>,
    slot_cap: u32,
}

/// The host's side of the aperture: it can read and write shared memory
/// at any time, including concurrently with the guest.
#[derive(Clone, Debug)]
pub struct HostSide {
    shared: Arc<SharedMemory>,
}

impl HostSide {
    /// Write into slot `slot` at `offset`; out-of-range bytes are dropped
    /// (the model host cannot write outside the aperture).
    pub fn write(&self, slot: u32, offset: usize, data: &[u8]) {
        let base = slot as usize * self.shared.slot_cap as usize;
        let cap = self.shared.slot_cap as usize;
        for (i, &b) in data.iter().enumerate() {
            let at = offset.saturating_add(i);
            if at < cap
                && let Some(cell) = self.shared.bytes.get(base.saturating_add(at))
            {
                cell.store(b, Ordering::Relaxed);
            }
        }
    }

    pub fn read(&self, slot: u32, offset: usize, out: &mut [u8]) {
        let base = slot as usize * self.shared.slot_cap as usize;
        for (i, dst) in out.iter_mut().enumerate() {
            *dst = self
                .shared
                .bytes
                .get(base.saturating_add(offset).saturating_add(i))
                .map_or(0, |c| c.load(Ordering::Relaxed));
        }
    }
}

/// A quarantined bounce aperture of fixed-size host-shared slots.
#[derive(Debug)]
pub struct BounceAperture {
    shared: Arc<SharedMemory>,
    granted: Vec<bool>,
    ledger: Arc<Ledger>,
}

/// Linear lease over one host-shared bounce slot.
#[must_use = "a HostBounceGrant must be flushed or scrubbed"]
#[derive(Debug)]
pub struct HostBounceGrant {
    slot_idx: u32,
    byte_cap: u32,
    obligation: Obligation,
}

impl HostBounceGrant {
    pub fn slot_idx(&self) -> u32 {
        self.slot_idx
    }

    pub fn byte_cap(&self) -> u32 {
        self.byte_cap
    }
}

/// Read-only view of a granted shared slot. It never yields `&[u8]`: the
/// only read is a one-pass copy into private memory.
pub struct HostBounceSlice<'a> {
    cells: &'a [AtomicU8],
}

impl HostBounceSlice<'_> {
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Copy the first `dst.len()` bytes, loading each shared byte exactly once.
    fn copy_once(&self, dst: &mut [u8]) {
        for (d, c) in dst.iter_mut().zip(self.cells) {
            *d = c.load(Ordering::Relaxed);
        }
    }
}

impl BounceAperture {
    pub fn new(slots: u32, slot_cap: u32) -> Self {
        let n = slots as usize * slot_cap as usize;
        BounceAperture {
            shared: Arc::new(SharedMemory {
                bytes: (0..n).map(|_| AtomicU8::new(0)).collect(),
                slot_cap,
            }),
            granted: vec![false; slots as usize],
            ledger: Ledger::new(),
        }
    }

    pub fn host_side(&self) -> HostSide {
        HostSide {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Lease slot `slot_idx` for one host exchange.
    pub fn grant(&mut self, slot_idx: u32) -> Result<HostBounceGrant, TeeError> {
        match self.granted.get_mut(slot_idx as usize) {
            Some(g) if !*g => {
                *g = true;
                Ok(HostBounceGrant {
                    slot_idx,
                    byte_cap: self.shared.slot_cap,
                    obligation: Obligation::new(&self.ledger),
                })
            }
            _ => Err(TeeError::BadBounceSlot { slot_idx }),
        }
    }

    fn cells(&self, grant: &HostBounceGrant) -> Result<HostBounceSlice<'_>, TeeError> {
        if grant.obligation.issuer() != self.ledger.issuer {
            return Err(TeeError::ForeignGrant);
        }
        let cap = self.shared.slot_cap as usize;
        let base = grant.slot_idx as usize * cap;
        Ok(HostBounceSlice {
            cells: &self.shared.bytes[base..base + cap],
        })
    }

    /// The shared view of a granted slot.
    pub fn shared_view(&self, grant: &HostBounceGrant) -> Result<HostBounceSlice<'_>, TeeError> {
        self.cells(grant)
    }

    /// `TeeHardware::snapshot_from_host_bounce`: validate the host-reported
    /// length against the slot capacity, then copy that many bytes exactly
    /// once into `dest`. Everything after this reads only the private copy.
    pub fn snapshot_from_host_bounce<'r>(
        &self,
        bounce: &HostBounceGrant,
        untrusted_len: UntrustedHostVal<u32>,
        dest: &'r mut PrivateRegion,
    ) -> Result<PrivateSlice<'r>, TeeError> {
        let view = self.cells(bounce)?;
        let len = untrusted_len.validate_len(bounce.byte_cap)? as usize;
        let mut buf = dest.alloc(len)?;
        view.copy_once(buf.as_mut_slice());
        Ok(buf)
    }

    /// Copy only sealed ciphertext out to the slot, scrub the rest of the
    /// slot, and release the grant.
    pub fn flush_ciphertext_and_release(
        &mut self,
        bounce: HostBounceGrant,
        sealed: CiphertextSlice<'_>,
    ) -> Result<(), (HostBounceGrant, TeeError)> {
        let cells = match self.cells(&bounce) {
            Ok(c) => c.cells,
            Err(e) => return Err((bounce, e)),
        };
        let parts = [&sealed.nonce[..], sealed.ciphertext, &sealed.tag[..]];
        let total: usize = parts.iter().map(|p| p.len()).sum();
        if total > cells.len() {
            let cap = cells.len();
            return Err((bounce, TeeError::CiphertextTooLarge { len: total, cap }));
        }
        let mut it = cells.iter();
        for part in parts {
            for (&b, c) in part.iter().zip(it.by_ref()) {
                c.store(b, Ordering::Relaxed);
            }
        }
        for c in it {
            c.store(0, Ordering::Relaxed);
        }
        self.release(bounce);
        Ok(())
    }

    /// Scrub the whole slot and release the grant (error paths).
    pub fn abort_and_scrub_bounce(&mut self, bounce: HostBounceGrant) {
        if let Ok(view) = self.cells(&bounce) {
            for c in view.cells {
                c.store(0, Ordering::Relaxed);
            }
            self.release(bounce);
        } else {
            // A foreign grant cannot be released here; it stays armed and is
            // counted as leaked by its own aperture when dropped.
            drop(bounce);
        }
    }

    fn release(&mut self, bounce: HostBounceGrant) {
        if let Some(g) = self.granted.get_mut(bounce.slot_idx as usize) {
            *g = false;
        }
        bounce.obligation.discharge();
    }

    pub fn is_granted(&self, slot_idx: u32) -> bool {
        self.granted
            .get(slot_idx as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Auditor: grants dropped without flush or scrub.
    pub fn leaked_grants(&self) -> u64 {
        self.ledger.leaked()
    }
}

/// Private (encrypted guest) memory: a fixed-capacity bump region,
/// allocated once and zeroized on reset.
#[derive(Debug)]
pub struct PrivateRegion {
    bytes: Box<[u8]>,
    used: usize,
}

/// A slice of private memory borrowed from a [`PrivateRegion`].
#[derive(Debug)]
pub struct PrivateSlice<'r> {
    bytes: &'r mut [u8],
}

impl<'r> PrivateSlice<'r> {
    pub fn as_slice(&self) -> &[u8] {
        self.bytes
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.bytes
    }

    pub fn into_mut(self) -> &'r mut [u8] {
        self.bytes
    }
}

impl PrivateRegion {
    pub fn with_capacity(cap: usize) -> Self {
        PrivateRegion {
            bytes: vec![0u8; cap].into_boxed_slice(),
            used: 0,
        }
    }

    pub fn alloc(&mut self, len: usize) -> Result<PrivateSlice<'_>, TeeError> {
        let free = self.bytes.len() - self.used;
        if len > free {
            return Err(TeeError::PrivateRegionExhausted { need: len, free });
        }
        let start = self.used;
        self.used += len;
        Ok(PrivateSlice {
            bytes: &mut self.bytes[start..start + len],
        })
    }

    /// Zeroize and reuse (end of `Region::scoped`).
    pub fn reset(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.bytes[..self.used]);
        self.used = 0;
    }

    pub fn used(&self) -> usize {
        self.used
    }
}

/// AEAD output: the only thing allowed to cross into shared memory.
#[derive(Debug)]
pub struct SealedFrame<'a> {
    pub(crate) nonce: [u8; 12],
    pub(crate) ciphertext: &'a [u8],
    pub(crate) tag: [u8; 16],
}

/// Borrowed view of a [`SealedFrame`] for egress.
#[derive(Clone, Copy, Debug)]
pub struct CiphertextSlice<'a> {
    nonce: [u8; 12],
    ciphertext: &'a [u8],
    tag: [u8; 16],
}

impl<'a> CiphertextSlice<'a> {
    pub fn from_sealed(frame: &SealedFrame<'a>) -> Self {
        CiphertextSlice {
            nonce: frame.nonce,
            ciphertext: frame.ciphertext,
            tag: frame.tag,
        }
    }

    pub fn len(&self) -> usize {
        12 + self.ciphertext.len() + 16
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}
