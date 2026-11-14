//! HTTP/3 (RFC 9114) and QPACK (RFC 9204) for Vesper Prism (design §17),
//! carried over Vesper Warp QUIC streams.
//!
//! * [`qpack`]: Huffman coding, the static and dynamic tables, encoder and
//!   decoder instruction streams and blocked-stream limits.
//! * [`frame`]: HTTP/3 frame and stream-type codecs.
//! * [`connection`]: a sans-I/O HTTP/3 server connection over any
//!   [`connection::Transport`], with the control stream, SETTINGS, GOAWAY,
//!   the QPACK streams and request streams whose responses are typestate
//!   obligations ([`connection::Responder`]).
//! * [`prism`]: the bridge that serves a [`vesper_prism::service::Service`]
//!   over HTTP/3 with the same handler that answers HTTP/1.1.
//! * [`warp`]: the [`connection::Transport`] for a Warp endpoint connection.
//!
//! * [`session`]: Extended CONNECT (RFC 9220) sessions, WebTransport over
//!   HTTP/3 streams and datagrams, and HTTP Datagrams (RFC 9297).
//! * [`capsule`]: the Capsule protocol (RFC 9297 §3).
//! * [`priority`]: extensible priorities (RFC 9218) and the scheduler.
//!
//! Server push is out of scope and is refused with typed errors. Rust
//! ownership here is host-library discipline: it is not the Vesper source
//! typing rules.

#![forbid(unsafe_code)]

pub mod capsule;
pub mod client;
pub mod connection;
pub mod error;
pub mod frame;
pub mod message;
pub mod priority;
pub mod prism;
pub mod qpack;
pub mod session;
pub mod transport;
pub mod warp;
