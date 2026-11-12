//! Guest-physical page acceptance (TDX `TDG.MEM.PAGE.ACCEPT`, SEV-SNP
//! `PVALIDATE`) with `UnacceptedGpaRange` linear owners.
//!
//! The host announces memory; the tracker mints one owner per announced
//! range and refuses announcements that overlap accepted or pending pages
//! (the VMM remap/replay attack). Owners can be split and merged; the only
//! exits are [`GpaAcceptanceTracker::accept`] (which zeroes and accepts the
//! pages) and [`GpaAcceptanceTracker::relinquish`] (which returns them to
//! the host unaccepted). Reads of unaccepted pages are typed errors.

use crate::error::{AegisVerifyError, TeeError};
use crate::linear::{Ledger, Obligation};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const PAGE_4K: u64 = 4096;
/// Highest page frame number + 1 (52-bit guest physical addresses).
pub const MAX_PFN: u64 = 1 << 40;

/// Disjoint, non-adjacent half-open page-frame intervals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IntervalSet {
    map: BTreeMap<u64, u64>,
}

impl IntervalSet {
    /// First page in `[s, e)` that lies in the set.
    pub(crate) fn first_overlap(&self, s: u64, e: u64) -> Option<u64> {
        if let Some((&a, &b)) = self.map.range(..=s).next_back()
            && b > s
        {
            return Some(s.max(a));
        }
        self.map.range(s..e).next().map(|(&a, _)| a)
    }

    pub(crate) fn covers(&self, s: u64, e: u64) -> bool {
        matches!(self.map.range(..=s).next_back(), Some((_, &b)) if b >= e)
    }

    /// First page in `[s, e)` not in the set.
    pub(crate) fn first_gap(&self, s: u64, e: u64) -> Option<u64> {
        let mut at = s;
        while at < e {
            match self.map.range(..=at).next_back() {
                Some((_, &b)) if b > at => at = b,
                _ => return Some(at),
            }
        }
        None
    }

    pub(crate) fn insert(&mut self, mut s: u64, mut e: u64) {
        if let Some((&a, &b)) = self.map.range(..=s).next_back()
            && b >= s
        {
            s = a;
            e = e.max(b);
            self.map.remove(&a);
        }
        while let Some((&a, &b)) = self.map.range(s..=e).next() {
            e = e.max(b);
            self.map.remove(&a);
        }
        self.map.insert(s, e);
    }

    pub(crate) fn remove(&mut self, s: u64, e: u64) {
        if let Some((&a, &b)) = self.map.range(..s).next_back()
            && b > s
        {
            self.map.insert(a, s);
            if b > e {
                self.map.insert(e, b);
                return;
            }
        }
        while let Some((&a, &b)) = self.map.range(s..e).next() {
            self.map.remove(&a);
            if b > e {
                self.map.insert(e, b);
            }
        }
    }

    pub(crate) fn pages(&self) -> u64 {
        self.map.iter().map(|(a, b)| b - a).sum()
    }

    pub(crate) fn intervals(&self) -> usize {
        self.map.len()
    }
}

/// Linear owner of a host-announced, not-yet-accepted GPA range.
#[must_use = "an UnacceptedGpaRange must be accepted or relinquished"]
#[derive(Debug)]
pub struct UnacceptedGpaRange {
    gpa_base: u64,
    page_count_4k: u32,
    obligation: Obligation,
}

impl UnacceptedGpaRange {
    pub fn gpa_base(&self) -> u64 {
        self.gpa_base
    }

    pub fn page_count_4k(&self) -> u32 {
        self.page_count_4k
    }

    fn pfns(&self) -> (u64, u64) {
        let s = self.gpa_base / PAGE_4K;
        (s, s + u64::from(self.page_count_4k))
    }

    /// Split into `[base, base + at)` and the rest. On a bad split point the
    /// owner is returned.
    pub fn split(self, at: u32) -> Result<(Self, Self), (Self, TeeError)> {
        if at == 0 || at >= self.page_count_4k {
            let e = TeeError::BadSplit {
                at,
                page_count_4k: self.page_count_4k,
            };
            return Err((self, e));
        }
        let tail = UnacceptedGpaRange {
            gpa_base: self.gpa_base + u64::from(at) * PAGE_4K,
            page_count_4k: self.page_count_4k - at,
            obligation: self.obligation.sibling(),
        };
        let head = UnacceptedGpaRange {
            gpa_base: self.gpa_base,
            page_count_4k: at,
            obligation: self.obligation.sibling(),
        };
        self.obligation.discharge();
        Ok((head, tail))
    }

    /// Merge with an adjacent range from the same tracker (either order).
    /// On failure both owners are returned.
    pub fn merge(self, other: Self) -> Result<Self, (Self, Self, TeeError)> {
        if self.obligation.issuer() != other.obligation.issuer() {
            return Err((self, other, TeeError::ForeignOwner));
        }
        let (lo, hi) = if self.gpa_base <= other.gpa_base {
            (self, other)
        } else {
            (other, self)
        };
        let count = u64::from(lo.page_count_4k) + u64::from(hi.page_count_4k);
        if lo.pfns().1 != hi.pfns().0 || count > u64::from(u32::MAX) {
            return Err((lo, hi, TeeError::NotAdjacent));
        }
        let merged = UnacceptedGpaRange {
            gpa_base: lo.gpa_base,
            page_count_4k: count as u32,
            obligation: lo.obligation.sibling(),
        };
        lo.obligation.discharge();
        hi.obligation.discharge();
        Ok(merged)
    }
}

/// Proof that a GPA range was accepted and zeroed by this tracker. It is a
/// receipt, not an owner: accepted private memory stays accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivatePageSlab {
    pub gpa_base: u64,
    pub page_count_4k: u32,
    issuer: u64,
}

impl PrivatePageSlab {
    /// Merge two adjacent slabs from the same tracker.
    pub fn merge(self, other: PrivatePageSlab) -> Result<PrivatePageSlab, TeeError> {
        if self.issuer != other.issuer {
            return Err(TeeError::ForeignOwner);
        }
        let (lo, hi) = if self.gpa_base <= other.gpa_base {
            (self, other)
        } else {
            (other, self)
        };
        let end = lo.gpa_base + u64::from(lo.page_count_4k) * PAGE_4K;
        let count = u64::from(lo.page_count_4k) + u64::from(hi.page_count_4k);
        if end != hi.gpa_base || count > u64::from(u32::MAX) {
            return Err(TeeError::NotAdjacent);
        }
        Ok(PrivatePageSlab {
            gpa_base: lo.gpa_base,
            page_count_4k: count as u32,
            issuer: lo.issuer,
        })
    }
}

/// The hardware page-acceptance operation. `accept_4k` must accept and
/// zero exactly one 4 KiB page or fail with a backend status code.
pub trait PageAcceptor {
    fn accept_4k(&mut self, gpa: u64) -> Result<(), u64>;
}

/// Tracks accepted 4 KiB private GPA frames to block VMM replay attacks.
#[derive(Debug)]
pub struct GpaAcceptanceTracker {
    accepted: IntervalSet,
    pending: IntervalSet,
    ledger: Arc<Ledger>,
}

impl Default for GpaAcceptanceTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn check_range(gpa_base: u64, page_count_4k: u64) -> Result<(u64, u64), TeeError> {
    let bad = TeeError::UnalignedOrZeroGpaRange {
        gpa_base,
        page_count_4k,
    };
    if page_count_4k == 0 || !gpa_base.is_multiple_of(PAGE_4K) {
        return Err(bad);
    }
    let s = gpa_base / PAGE_4K;
    match s.checked_add(page_count_4k) {
        Some(e) if e <= MAX_PFN => Ok((s, e)),
        _ => Err(bad),
    }
}

impl GpaAcceptanceTracker {
    pub fn new() -> Self {
        GpaAcceptanceTracker {
            accepted: IntervalSet::default(),
            pending: IntervalSet::default(),
            ledger: Ledger::new(),
        }
    }

    /// Host announces `page_count_4k` pages at `gpa_base` (hotplug or
    /// remap). Rejects ranges that are unaligned, empty, out of range,
    /// already accepted (double accept), or overlap accepted or pending
    /// pages.
    pub fn announce(
        &mut self,
        gpa_base: u64,
        page_count_4k: u32,
    ) -> Result<UnacceptedGpaRange, TeeError> {
        let (s, e) = check_range(gpa_base, page_count_4k.into())?;
        if self.accepted.covers(s, e) {
            return Err(TeeError::HypervisorGpaDoubleAccept {
                gpa_page_4k: gpa_base,
            });
        }
        if let Some(p) = self
            .accepted
            .first_overlap(s, e)
            .into_iter()
            .chain(self.pending.first_overlap(s, e))
            .min()
        {
            return Err(TeeError::OverlappingGpaRange {
                gpa_page_4k: p * PAGE_4K,
            });
        }
        self.pending.insert(s, e);
        Ok(UnacceptedGpaRange {
            gpa_base,
            page_count_4k,
            obligation: Obligation::new(&self.ledger),
        })
    }

    /// Accept and zero every page of `range` through `backend`, consuming
    /// the owner. If the backend fails at page `k`, pages before `k` stay
    /// accepted (they were accepted in hardware) and the owner of the
    /// remaining pages is returned with the error.
    pub fn accept(
        &mut self,
        range: UnacceptedGpaRange,
        backend: &mut impl PageAcceptor,
    ) -> Result<PrivatePageSlab, (UnacceptedGpaRange, TeeError)> {
        if range.obligation.issuer() != self.ledger.issuer {
            return Err((range, TeeError::ForeignOwner));
        }
        let (s, e) = range.pfns();
        for pfn in s..e {
            if let Err(code) = backend.accept_4k(pfn * PAGE_4K) {
                let err = TeeError::AcceptBackend {
                    gpa_page_4k: pfn * PAGE_4K,
                    code,
                };
                if pfn == s {
                    return Err((range, err));
                }
                self.pending.remove(s, pfn);
                self.accepted.insert(s, pfn);
                let rest = UnacceptedGpaRange {
                    gpa_base: pfn * PAGE_4K,
                    page_count_4k: (e - pfn) as u32,
                    obligation: range.obligation.sibling(),
                };
                range.obligation.discharge();
                return Err((rest, err));
            }
        }
        self.pending.remove(s, e);
        self.accepted.insert(s, e);
        let slab = PrivatePageSlab {
            gpa_base: range.gpa_base,
            page_count_4k: range.page_count_4k,
            issuer: self.ledger.issuer,
        };
        range.obligation.discharge();
        Ok(slab)
    }

    /// Give an unaccepted range back to the host without accepting it.
    pub fn relinquish(
        &mut self,
        range: UnacceptedGpaRange,
    ) -> Result<(), (UnacceptedGpaRange, TeeError)> {
        if range.obligation.issuer() != self.ledger.issuer {
            return Err((range, TeeError::ForeignOwner));
        }
        let (s, e) = range.pfns();
        self.pending.remove(s, e);
        range.obligation.discharge();
        Ok(())
    }

    /// Check that `[gpa, gpa + len)` lies entirely in accepted private pages.
    pub fn check_access(&self, gpa: u64, len: u64) -> Result<(), TeeError> {
        if len == 0 {
            return Ok(());
        }
        let last = gpa
            .checked_add(len - 1)
            .ok_or(TeeError::UnacceptedPageAccess {
                gpa_page_4k: gpa & !0xFFF,
            })?;
        let (s, e) = (gpa / PAGE_4K, last / PAGE_4K + 1);
        match self.accepted.first_gap(s, e) {
            None => Ok(()),
            Some(p) => Err(TeeError::UnacceptedPageAccess {
                gpa_page_4k: p * PAGE_4K,
            }),
        }
    }

    /// The design's raw API (§33.5): accept a range given as plain numbers,
    /// rejecting any page already accepted. Prefer [`Self::announce`] and
    /// [`Self::accept`], which also track pending pages and owners.
    pub fn accept_gpa_range(
        &mut self,
        gpa_base: u64,
        page_count_4k: u32,
    ) -> Result<(), AegisVerifyError> {
        let (s, e) = check_range(gpa_base, page_count_4k.into())
            .map_err(|_| AegisVerifyError::UnalignedOrZeroGpaRange)?;
        if let Some(p) = self
            .accepted
            .first_overlap(s, e)
            .into_iter()
            .chain(self.pending.first_overlap(s, e))
            .min()
        {
            return Err(AegisVerifyError::HypervisorGpaDoubleAccept {
                gpa_page_4k: p * PAGE_4K,
            });
        }
        self.accepted.insert(s, e);
        Ok(())
    }

    pub fn is_accepted(&self, gpa: u64) -> bool {
        let p = gpa / PAGE_4K;
        self.accepted.covers(p, p + 1)
    }

    pub fn is_pending(&self, gpa: u64) -> bool {
        let p = gpa / PAGE_4K;
        self.pending.covers(p, p + 1)
    }

    pub fn accepted_pages(&self) -> u64 {
        self.accepted.pages()
    }

    /// Number of maximal accepted intervals (adjacent accepts merge).
    pub fn accepted_intervals(&self) -> usize {
        self.accepted.intervals()
    }

    /// Auditor: owners dropped without a consuming call.
    pub fn leaked_ranges(&self) -> u64 {
        self.ledger.leaked()
    }
}

/// Software model of guest memory for tests and simulators: each page is
/// unaccepted (host-controlled stale bytes) until accepted, which zeroes
/// it; accepting twice fails like `TDX_PAGE_ALREADY_ACCEPTED`. It models
/// the protocol only; no hardware page state is touched.
#[derive(Debug, Default)]
pub struct SimulatedGuestMemory {
    pages: BTreeMap<u64, Box<[u8; PAGE_4K as usize]>>,
    accepted: IntervalSet,
    /// Fail the next accept at this GPA with this code (fault injection).
    pub fail_at: Option<(u64, u64)>,
}

/// Status code returned for a second accept of the same page.
pub const SIM_PAGE_ALREADY_ACCEPTED: u64 = 0xC000_0B0A;

impl SimulatedGuestMemory {
    /// Host writes stale bytes into an unaccepted page.
    pub fn host_write(&mut self, gpa: u64, byte: u8) {
        let page = self
            .pages
            .entry(gpa / PAGE_4K)
            .or_insert_with(|| Box::new([0; PAGE_4K as usize]));
        page[(gpa % PAGE_4K) as usize] = byte;
    }

    /// Guest read, gated by the tracker.
    pub fn guest_read(&self, tracker: &GpaAcceptanceTracker, gpa: u64) -> Result<u8, TeeError> {
        tracker.check_access(gpa, 1)?;
        Ok(self
            .pages
            .get(&(gpa / PAGE_4K))
            .map_or(0, |p| p[(gpa % PAGE_4K) as usize]))
    }
}

impl PageAcceptor for SimulatedGuestMemory {
    fn accept_4k(&mut self, gpa: u64) -> Result<(), u64> {
        if let Some((at, code)) = self.fail_at
            && at == gpa
        {
            self.fail_at = None;
            return Err(code);
        }
        let p = gpa / PAGE_4K;
        if self.accepted.covers(p, p + 1) {
            return Err(SIM_PAGE_ALREADY_ACCEPTED);
        }
        self.accepted.insert(p, p + 1);
        if let Some(page) = self.pages.get_mut(&p) {
            page.fill(0);
        }
        Ok(())
    }
}
