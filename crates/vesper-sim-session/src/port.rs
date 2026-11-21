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

pub use port_binding::*;

#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
