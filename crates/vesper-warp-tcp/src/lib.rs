//! Sans-IO RFC 9293 TCP endpoint for Vesper Warp (design §35, "QUIC/TCP").
//!
//! - [`wire`]: header/options parse and serialize in place, checksums over
//!   IPv4/IPv6 pseudo-headers.
//! - [`seq`]: modular sequence arithmetic.
//! - [`conn`]: the connection state machine (LISTEN … TIME-WAIT), windows
//!   with RFC 7323 scaling, zero-window probing, bounded out-of-order
//!   reassembly, RFC 6298 RTO with Karn's rule, fast retransmit/recovery,
//!   RFC 6675 SACK loss recovery and RFC 5961 mitigations.
//! - [`cc`]: the congestion-control boundary shared with the QUIC/BBRv3
//!   lanes, and a NewReno (RFC 5681) controller.
//! - [`harness`]: a deterministic seeded two-endpoint harness with a lossy,
//!   reordering, duplicating link, for tests.
//!
//! There is no kernel, TUN or raw-socket I/O here and no interoperability
//! claim with real stacks: the harness validates this implementation against
//! itself and hand-built fixtures only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cc;
pub mod conn;
pub mod harness;
pub mod rtt;
pub mod seq;
pub mod wire;

pub use cc::{CongestionController, CongestionEvent, NewReno};
pub use conn::{
    CloseReason, Config, Connection, Event, Received, Released, SendView, State, Stats, TcpError,
    Transmit,
};
pub use seq::Seq;
pub use wire::{Flags, IpPair, Segment, TcpOptions, WireError};
