//! The records that cross the Vesper port boundary (port/src/lib.ves), and
//! the generated binding. Compiled only under `vesper-port`.

/// `LinkSpec` without its name, with the options flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkParams {
    pub base_delay_ns: u64,
    pub has_gilbert_elliott: bool,
    pub good_to_bad_ppm: u32,
    pub bad_to_good_ppm: u32,
    pub loss_good_ppm: u32,
    pub loss_bad_ppm: u32,
    pub loss_ppm: u32,
    pub duplicate_ppm: u32,
    pub has_reorder: bool,
    pub reorder_ppm: u32,
    pub reorder_extra_delay_ns: u64,
    pub buffer_packets: u32,
    pub rate_bytes_per_sec: u64,
}

/// `Result<Transmission, LinkError>`, flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmitOutcome {
    TimeOverflow,
    Lost(bool, u8),
    Delivered(bool, u64, u64, u64),
}

/// `Result<Sent, NetError>` from `Network::send`, without the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    TooLarge(u64),
    InFlightFull,
    InboxFull,
    InFlight(u64, u64),
    Dropped(u64, u64, u8),
}

/// `Option<Arrival>` from `Network::arrive`, without the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArriveOutcome {
    Unknown,
    Delivered(u64),
    Queued,
    Dropped(u8),
}

/// `Result<u64, WalError>` from the WAL state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalResult {
    Ok(u64),
    NotRecovered,
    AlreadyRecovered,
    FlushInFlight,
    NoFlushInFlight,
    RecordTooLarge(u64, u64),
    LogFull(u64, u64),
}

/// `CrashApplied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCrash {
    pub volatile_lost: u64,
    pub in_flight_records: u64,
    pub persisted_records: u64,
    pub torn_bytes: u64,
}

pub use port_binding::*;

#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
