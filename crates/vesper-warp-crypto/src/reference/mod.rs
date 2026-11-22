//! The Rust packet protection the Vesper port replaced (the last Rust
//! commit's `protect`, `retry`, `schedule`, `ticket` and `update`), kept
//! test-only as the reference of `crate::differential`. It shares the
//! crate's `Cipher`, `CryptoError` and constants.

#![allow(dead_code)]

pub mod protect;
pub mod retry;
pub mod schedule;
pub mod ticket;
pub mod update;

use crate::{CryptoError, PACKET_NUMBER_LIMIT};

/// Checks `pn < 2^62`.
pub(crate) fn check_pn(pn: u64) -> Result<(), CryptoError> {
    if pn < PACKET_NUMBER_LIMIT {
        Ok(())
    } else {
        Err(CryptoError::PacketNumberOutOfRange(pn))
    }
}
