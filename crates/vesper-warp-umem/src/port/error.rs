//! The port's error types: `UmemError` and `ConservationError` without their
//! `&'static str` payloads, which do not cross the Vesper boundary, and their
//! conversions to the crate's errors.

use crate::accounting::ConservationError;
use crate::driver::DriverLoc;
use crate::error::{RingError, UmemError};
use crate::trace::{LOCATIONS, Location};

/// `UmemError` with `InvalidConfig` as a message code ([`config_message`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortError {
    InvalidConfig(u32),
    WrongLocation {
        chunk: u32,
    },
    TxRingFull,
    NoFreeFrame,
    LengthOutOfFrame {
        offset: u32,
        len: u32,
        frame_size: u32,
    },
    HeadroomOverlap {
        offset: u32,
        headroom: u32,
    },
    PacketNumberOutOfRange {
        pn: u64,
    },
    Ring(RingError),
}

/// `XdpUmemAccounting::counts` as a struct, in `Location` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationCounts {
    pub free: u32,
    pub fill: u32,
    pub nic_rx: u32,
    pub rx: u32,
    pub app_rx: u32,
    pub app_tx: u32,
    pub tx: u32,
    pub nic_tx: u32,
    pub comp: u32,
    pub in_flight: u32,
    pub leaked: u32,
}

/// `ConservationError` with the side and the device location as codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditError {
    TotalMismatch {
        expected: u32,
        actual: u64,
    },
    Leaked {
        count: u32,
        first_chunk: Option<u32>,
    },
    SideMismatch {
        side: u32,
        driver: u32,
        device: u32,
    },
    Duplicate {
        chunk: u32,
    },
    Missing {
        chunk: u32,
    },
    Misplaced {
        chunk: u32,
        device: u32,
        driver: DriverLoc,
    },
}

/// The messages of `UmemConfig::validate` and `Xsk::new`, by the codes of
/// `vesper/src/config.ves`.
pub(crate) fn config_message(code: u32) -> &'static str {
    match code {
        0 => "frame_count must be in 1..=2^20",
        1 => "frame_size must be a power of two in 2048..=32768",
        2 => "headroom leaves fewer than 64 payload bytes",
        3 => "ring sizes must be powers of two up to 2^20",
        4 => "fill ring must be able to hold every frame",
        5 => "UMEM size overflows",
        _ => "backend UMEM or ring sizes differ from the config",
    }
}

impl From<PortError> for UmemError {
    fn from(e: PortError) -> Self {
        match e {
            PortError::InvalidConfig(code) => UmemError::InvalidConfig(config_message(code)),
            PortError::WrongLocation { chunk } => UmemError::WrongLocation { chunk },
            PortError::TxRingFull => UmemError::TxRingFull,
            PortError::NoFreeFrame => UmemError::NoFreeFrame,
            PortError::LengthOutOfFrame {
                offset,
                len,
                frame_size,
            } => UmemError::LengthOutOfFrame {
                offset,
                len,
                frame_size,
            },
            PortError::HeadroomOverlap { offset, headroom } => {
                UmemError::HeadroomOverlap { offset, headroom }
            }
            PortError::PacketNumberOutOfRange { pn } => UmemError::PacketNumberOutOfRange { pn },
            PortError::Ring(e) => UmemError::Ring(e),
        }
    }
}

fn side_name(side: u32) -> &'static str {
    match side {
        0 => "free list",
        1 => "kernel receive side",
        _ => "kernel transmit side",
    }
}

fn location(index: u32) -> Location {
    LOCATIONS
        .get(index as usize)
        .copied()
        .unwrap_or(Location::Free)
}

impl From<AuditError> for ConservationError {
    fn from(e: AuditError) -> Self {
        match e {
            AuditError::TotalMismatch { expected, actual } => {
                ConservationError::TotalMismatch { expected, actual }
            }
            AuditError::Leaked { count, first_chunk } => {
                ConservationError::Leaked { count, first_chunk }
            }
            AuditError::SideMismatch {
                side,
                driver,
                device,
            } => ConservationError::SideMismatch {
                side: side_name(side),
                driver,
                device,
            },
            AuditError::Duplicate { chunk } => ConservationError::Duplicate { chunk },
            AuditError::Missing { chunk } => ConservationError::Missing { chunk },
            AuditError::Misplaced {
                chunk,
                device,
                driver,
            } => ConservationError::Misplaced {
                chunk,
                device: location(device),
                driver,
            },
        }
    }
}
