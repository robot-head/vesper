//! §23.1.2 / §23.2 `SecretSlot[K]`: long-lived secret bytes in page-aligned,
//! locked, core-dump-excluded memory, reachable only through closure
//! projections and consumed only by [`SecretSlot::shred`].

use core::fmt;

use crate::obligation::{ObligationKind, record_leak};
use crate::sys::{LockedPages, Protection};

/// What to do when the OS refuses to lock the slot's pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockPolicy {
    /// Fail with [`SlotError::MlockFailed`] unless the pages are locked.
    Require,
    /// Keep the slot and report the failure in [`SlotProtection`].
    BestEffort,
}

/// Why a slot could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotError {
    /// `K == 0`.
    ZeroSize,
    /// The OS refused the page mapping.
    MapFailed {
        /// OS error code, when there is one.
        os_error: Option<i32>,
    },
    /// `mlock`/`VirtualLock` failed under [`LockPolicy::Require`]; on Linux
    /// this is usually `RLIMIT_MEMLOCK` (`EPERM`/`ENOMEM`/`EAGAIN`).
    MlockFailed {
        /// OS error code, when there is one.
        os_error: Option<i32>,
    },
}

impl fmt::Display for SlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotError::ZeroSize => f.write_str("secret slot of zero bytes"),
            SlotError::MapFailed { os_error } => {
                write!(f, "secret slot mapping failed (os error {os_error:?})")
            }
            SlotError::MlockFailed { os_error } => {
                write!(
                    f,
                    "secret slot mlock failed (os error {os_error:?}); check RLIMIT_MEMLOCK"
                )
            }
        }
    }
}

impl std::error::Error for SlotError {}

/// The protections actually in force for one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotProtection {
    /// `mlock` (Unix) or `VirtualLock` (Windows).
    pub mlock: Protection,
    /// `MADV_DONTDUMP` (Linux/Android only; unavailable elsewhere).
    pub dontdump: Protection,
    /// The slot starts on a page boundary.
    pub page_aligned: bool,
    /// Bytes mapped (a whole number of pages).
    pub mapped_bytes: usize,
}

/// Proof-of-wipe returned by [`SecretSlot::shred`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct ShredReceipt {
    /// Bytes zero-filled (the whole mapping, not only `K`).
    pub wiped_bytes: usize,
    /// A volatile read-back found every mapped byte zero.
    pub verified_zero: bool,
}

/// `K` secret bytes in their own locked pages.
///
/// Linear: not `Clone`, not `Copy`. The only exit is [`SecretSlot::shred`].
/// A slot dropped any other way is still wiped, but counted as a leak by
/// [`crate::LeakAuditor`].
#[must_use = "a SecretSlot must be consumed with shred()"]
pub struct SecretSlot<const K: usize> {
    pages: Option<LockedPages>,
}

impl<const K: usize> fmt::Debug for SecretSlot<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretSlot<{K}>(<secret>)")
    }
}

impl<const K: usize> SecretSlot<K> {
    /// A zero-filled slot.
    pub fn new(policy: LockPolicy) -> Result<Self, SlotError> {
        if K == 0 {
            return Err(SlotError::ZeroSize);
        }
        let pages = LockedPages::new(K).map_err(|e| SlotError::MapFailed {
            os_error: match e {
                region::Error::SystemCall(io) => io.raw_os_error(),
                _ => None,
            },
        })?;
        if policy == LockPolicy::Require
            && let Protection::Failed { os_error } = pages.mlock
        {
            // `pages` is wiped and freed here; it never held a secret.
            return Err(SlotError::MlockFailed { os_error });
        }
        Ok(Self { pages: Some(pages) })
    }

    /// A slot initialised in place by `init`, so the secret is written
    /// straight into locked memory.
    pub fn from_fn(policy: LockPolicy, init: impl FnOnce(&mut [u8; K])) -> Result<Self, SlotError> {
        let mut slot = Self::new(policy)?;
        slot.with_mut(init);
        Ok(slot)
    }

    /// Move `src` into a new slot and zeroize `src`.
    pub fn from_bytes(policy: LockPolicy, src: &mut [u8; K]) -> Result<Self, SlotError> {
        let r = Self::from_fn(policy, |dst| dst.copy_from_slice(src));
        zeroize::Zeroize::zeroize(src);
        r
    }

    fn pages(&self) -> &LockedPages {
        // Invariant: `pages` is `Some` until `shred` takes it, and `shred`
        // consumes `self`, so every live slot has pages.
        match &self.pages {
            Some(p) => p,
            None => unreachable!("SecretSlot used after shred"),
        }
    }

    fn pages_mut(&mut self) -> &mut LockedPages {
        match &mut self.pages {
            Some(p) => p,
            None => unreachable!("SecretSlot used after shred"),
        }
    }

    /// Read-only projection (`in` access). The borrow cannot escape `f`.
    pub fn with<R>(&self, f: impl FnOnce(&[u8; K]) -> R) -> R {
        let bytes: &[u8; K] = match self.pages().bytes().try_into() {
            Ok(b) => b,
            Err(_) => unreachable!("slot length is K"),
        };
        f(bytes)
    }

    /// Mutable projection (`inout` access). The borrow cannot escape `f`.
    pub fn with_mut<R>(&mut self, f: impl FnOnce(&mut [u8; K]) -> R) -> R {
        let bytes: &mut [u8; K] = match self.pages_mut().bytes_mut().try_into() {
            Ok(b) => b,
            Err(_) => unreachable!("slot length is K"),
        };
        f(bytes)
    }

    /// The protections in force.
    pub fn protection(&self) -> SlotProtection {
        let p = self.pages();
        SlotProtection {
            mlock: p.mlock,
            dontdump: p.dontdump,
            page_aligned: p.is_page_aligned(),
            mapped_bytes: p.mapped_len(),
        }
    }

    /// The terminal consumer: volatile zero-fill of the whole mapping, a
    /// compiler and hardware fence, a volatile read-back, then unlock and
    /// unmap. Register scrubbing (§23.1.2) is not performed: Rust offers no
    /// portable way to do it, so it is a recorded gap.
    pub fn shred(mut self) -> ShredReceipt {
        let mut pages = match self.pages.take() {
            Some(p) => p,
            None => unreachable!("SecretSlot shredded twice"),
        };
        pages.wipe();
        let receipt = ShredReceipt {
            wiped_bytes: pages.mapped_len(),
            verified_zero: pages.all_zero(),
        };
        drop(pages);
        receipt
    }
}

impl<const K: usize> Drop for SecretSlot<K> {
    fn drop(&mut self) {
        if self.pages.is_some() {
            // Dropped without `shred`: `LockedPages::drop` still wipes, but
            // the missed obligation is counted, never silent.
            record_leak(ObligationKind::SecretSlot);
        }
    }
}
