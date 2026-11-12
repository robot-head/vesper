//! Vesper Warp timing wheel (design §35.1 item 3, §35.2 `park_for_ack_or_pto`).
//!
//! * [`Wheel`]: a core-local, fixed-capacity hierarchical hashed timing wheel
//!   (Varghese–Lauck) over generic owned payloads, with generation-checked
//!   [`TimerHandle`]s, exact firing (`deadline <= now`), deadline order and
//!   FIFO among equal deadlines. See the [`wheel`] module for the model and
//!   costs.
//! * [`RetransmitLeases`]: a PTO lease table keyed by
//!   `(PacketNumberSpace, packet number)`: ACK removes a lease, PTO fires it
//!   for retransmission.
//! * [`EdtLaunchQueue`]: releases packets whose earliest departure time has
//!   arrived, in non-decreasing EDT order, rejecting submissions beyond a
//!   configurable horizon (a model of `SO_TXTIME`'s horizon).
//! * [`VirtualClock`]: deterministic caller-driven time. No library code reads
//!   a real clock, sleeps or spawns threads.
//!
//! Linear §35 obligations are modelled as Rust owners: a payload enters by a
//! consuming insert and leaves only by cancel, fire or drain; every refusal
//! hands the payload back; a wheel dropped while non-empty is counted in
//! [`leaked_timer_entries`] and panics in debug builds. This is host-library
//! discipline and is not evidence that Vesper's source-level linear typing
//! rules are implemented.
//!
//! ```
//! use vesper_warp_wheel::{Wheel, WheelConfig};
//!
//! let mut wheel = Wheel::new(WheelConfig::with_capacity(16)).unwrap();
//! let late = wheel.insert(5_000, "late").unwrap();
//! let _soon = wheel.insert(2_000, "soon").unwrap();
//! assert!(wheel.poll(1_999).unwrap().is_none()); // never early
//! assert_eq!(wheel.poll(2_000).unwrap().unwrap().payload, "soon");
//! assert_eq!(wheel.cancel(late).unwrap(), "late");
//! assert!(wheel.is_empty());
//! ```
//!
//! This crate is a software scheduling model. It makes no claim about kernel
//! timers, NIC launch-time (EDT/`SO_TXTIME`) offload or interrupt-free pacing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;
pub mod edt;
pub mod leases;
pub mod wheel;

pub use clock::{Clock, ClockError, VirtualClock};
pub use edt::{EdtLaunchQueue, Launch, SubmitError, SubmitErrorKind};
pub use leases::{
    LeaseError, LeaseKey, MAX_PACKET_NUMBER, PacketNumberSpace, ParkError, ParkErrorKind,
    PtoExpired, RetransmitLeases,
};
pub use wheel::{
    AdvanceError, Expired, HandleError, InsertError, InsertErrorKind, MAX_CAPACITY, MAX_TIME_NS,
    TimerHandle, Wheel, WheelConfig, WheelConfigError, leaked_timer_entries,
};
