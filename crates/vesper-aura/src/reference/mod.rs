//! The frozen Rust implementation (port wave P1 lane 13, #910): the default
//! build, used without the `vesper-port` feature. It takes bug fixes only and
//! stays the default until the Vesper port in vesper/ reaches performance
//! parity.

mod identity;
mod plan;
mod recording;
mod session;
mod sim_aura;
mod view;

pub use recording::RecordingBackend;
pub use session::{CloseDecision, PreparedUpdate, Session};
pub use sim_aura::{SimAura, audit_view_tree, encode_view_tree};
