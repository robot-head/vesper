//! Vesper Warp UMEM: linear AF_XDP frames, the four-ring model, a
//! deterministic software NIC and frame-conservation accounting (design §35.1
//! item 1, §35.2 `XdpNic`/`UmemFrame`, §35.4 theorem 1, §35.5
//! `XdpUmemAccounting`).
//!
//! What this crate is:
//! * [`XdpDescWire`]: the 16-byte `struct xdp_desc`, declared through
//!   `vesper-ffi`'s checked wire layouts.
//! * [`UmemFrame`] typestate owners ([`RxHarvested`], [`TxStaged`],
//!   [`Submitted`]) and [`InFlightQuicPkt`] leases: non-Clone, `#[must_use]`,
//!   consumed only by [`XdpNic`] calls; dropped owners are detected, counted
//!   and quarantined, never recycled.
//! * [`Ring`]: an SPSC ring with libxdp's cached-index, wrapping-`u32`,
//!   batch reserve/submit/peek/release semantics.
//! * [`Xsk`]: the application-side driver implementing [`XdpNic`], over any
//!   [`XskBackend`]: the [`SoftNic`] model or (feature `af-xdp`, Linux) a
//!   real AF_XDP socket.
//! * [`XdpUmemAccounting`] and [`UmemAuditor`]: exact per-location
//!   conservation, cross-checked chunk by chunk.
//!
//! What it is not: Rust ownership here is host-library discipline, not an
//! implementation of the Vesper source typing rules for `linear`. The
//! software NIC validates the modelled protocol only; it says nothing about
//! NIC zero-copy, DMA isolation or line rate.

#![cfg_attr(not(all(feature = "af-xdp", target_os = "linux")), forbid(unsafe_code))]
#![cfg_attr(all(feature = "af-xdp", target_os = "linux"), deny(unsafe_code))]

mod accounting;
mod config;
mod driver;
mod error;
mod frame;
mod ring;
mod soft;
mod trace;
mod wire;

#[cfg(all(feature = "af-xdp", target_os = "linux"))]
pub mod af_xdp;

pub use accounting::{ConservationError, UmemAuditor, XdpUmemAccounting, audit_driver};
pub use config::{
    MAX_FRAME_SIZE, MAX_FRAMES, MAX_RING_SIZE, MIN_FRAME_PAYLOAD, MIN_FRAME_SIZE, UmemConfig,
};
pub use driver::{
    CompletionReport, DriverCensus, DriverLoc, DriverStats, LeaseMark, MAX_PACKET_NUMBER_EXCLUSIVE,
    RetireOutcome, XdpNic, Xsk, XskBackend,
};
pub use error::{DescError, Refused, RingError, RxRejected, UmemError};
pub use frame::{FrameState, InFlightQuicPkt, RxHarvested, Submitted, TxStaged, UmemFrame};
pub use ring::{ConsumerRing, ProducerRing, Ring, pop_one, push_one};
pub use soft::{
    DeliverOutcome, Fault, KernelCensus, SoftNic, SoftNicMode, SoftNicStats, SoftStepReport,
    SoftXsk,
};
pub use trace::{LOCATIONS, Location, Transition, TransitionKind};
pub use wire::{XDP_DESC_BYTES, XDP_DESC_WIRE, XdpDesc, XdpDescBytes, XdpDescWire};
