//! The Vesper build: the ported modules forward to the Vesper package
//! `@vesper/warp` (`vesper/src/*.ves`) through the generated binding
//! (`port/binding.rs`). Only the representation adapters live here:
//! conversions between the crate's public types and the word types of the
//! export ABI, and the slices a parsed span names.

pub(crate) mod ack;
pub(crate) mod frame;
pub(crate) mod header;
pub(crate) mod packet_number;
pub(crate) mod recovery;
pub(crate) mod stream;
pub(crate) mod varint;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../../port/binding.rs"]
pub(crate) mod binding;

pub(crate) fn packet_type_to(t: crate::PacketType) -> binding::PktType {
    match t {
        crate::PacketType::Initial => binding::PktType::Initial,
        crate::PacketType::ZeroRtt => binding::PktType::ZeroRtt,
        crate::PacketType::Handshake => binding::PktType::Handshake,
        crate::PacketType::OneRtt => binding::PktType::OneRtt,
    }
}
