//! The endpoint's per-packet assembly forwarded to
//! `vesper/src/packet.ves`; the endpoint's tables, keys and UMEM frames
//! stay Rust.

use super::binding::{self as b, PacketShortPlainError as E};
use super::frame::frame_error;
use super::header::header_error;
use crate::ack::ReceivedPackets;
use crate::endpoint::WarpError;
use crate::header::HeaderParser;

/// `WarpEndpoint::write_plaintext`: the plain text of the 1-RTT packet
/// number `pn` to `dcid` in `scratch[..plain_len]`, an ACK frame first when
/// `with_ack` and one is owed, then `app`; returns (plain length, header
/// length, carries an ACK).
#[allow(clippy::too_many_arguments)]
pub(crate) fn short_plaintext<const N: usize>(
    scratch: &mut [u8],
    received: &ReceivedPackets<N>,
    with_ack: bool,
    now_ns: u64,
    exponent: u8,
    app: &[u8],
    pn: u64,
    largest_acked: Option<u64>,
    parser: &HeaderParser,
    dcid: &[u8],
    key_phase: bool,
    max_datagram: usize,
) -> Result<(usize, usize, bool), WarpError> {
    let out = b::packet_short_plain(
        scratch,
        &received.state,
        &received.ranges,
        with_ack,
        now_ns,
        exponent,
        app,
        pn,
        largest_acked,
        dcid,
        parser.dcid_len as u64,
        key_phase,
        max_datagram as u64,
    )
    .map_err(|e| match e {
        E::AckErr(_) => WarpError::Config("bad ack_delay_exponent"),
        E::FrameErr(e) => WarpError::Frame(frame_error(e)),
        E::HdrErr(e) => WarpError::Header(header_error(e)),
        E::PnErr(_) => WarpError::PacketNumbersExhausted,
        E::PlainErr(b::PlainErr::TooLarge { len }) => {
            WarpError::PacketTooLarge { len: len as usize }
        }
        E::PlainErr(b::PlainErr::Empty) => WarpError::Config("empty packet"),
    })?;
    Ok((
        out.plain_len as usize,
        out.header_len as usize,
        out.has_ack.get(),
    ))
}
