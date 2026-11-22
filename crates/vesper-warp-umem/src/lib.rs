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

//!
//! # Two builds
//!
//! The crate is being ported to Vesper (SH6 lane 14, #1409). By default the
//! Rust implementation runs. With the `vesper-port` feature, the rings, the
//! software NIC, the driver and the auditor run in the Vesper build of
//! `vesper/src/` through the generated `port/binding.rs`; the wrappers in
//! `src/port/` keep the UMEM bytes (lent to Vesper in place on every call
//! that touches them) and the linear owners with their drop ledger. The
//! AF_XDP backend is not ported yet and is absent from that build.

#![cfg_attr(
    not(any(all(feature = "af-xdp", target_os = "linux"), feature = "vesper-port")),
    forbid(unsafe_code)
)]
#![cfg_attr(
    any(all(feature = "af-xdp", target_os = "linux"), feature = "vesper-port"),
    deny(unsafe_code)
)]

#[cfg_attr(feature = "vesper-port", path = "port/accounting.rs")]
mod accounting;
mod config;
#[cfg_attr(feature = "vesper-port", path = "port/driver.rs")]
mod driver;
mod error;
mod frame;
#[cfg_attr(feature = "vesper-port", path = "port/ring.rs")]
mod ring;
#[cfg_attr(feature = "vesper-port", path = "port/soft.rs")]
mod soft;
mod trace;
mod wire;

// Generated and committed, kept outside src/ so formatters never reflow it
// (see docs/maintenance/porting-a-crate.md).
#[cfg(feature = "vesper-port")]
#[rustfmt::skip]
#[allow(unsafe_code, missing_docs)]
#[path = "../port/binding.rs"]
mod binding;
#[cfg(feature = "vesper-port")]
#[path = "port/error.rs"]
mod port_error;
#[cfg(feature = "vesper-port")]
pub(crate) use port_error::{AuditError, LocationCounts, PortError};

/// Live Vesper sequence and box allocations of the Vesper build (for leak
/// tests; the Rust allocator never sees them).
#[cfg(feature = "vesper-port")]
#[doc(hidden)]
pub fn port_live_allocations() -> i64 {
    binding::live_allocations()
}

/// The Rust implementation, compiled beside the Vesper build so that one
/// process can benchmark both (tests/abba.rs). Not part of the API.
#[cfg(feature = "vesper-port")]
#[doc(hidden)]
#[allow(dead_code, unreachable_pub)]
pub mod rust_reference;

/// Which implementation this build runs.
pub const IMPLEMENTATION: &str = if cfg!(feature = "vesper-port") {
    "vesper"
} else {
    "rust"
};

#[cfg(all(feature = "af-xdp", target_os = "linux", not(feature = "vesper-port")))]
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
