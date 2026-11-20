//! The endpoint's per-packet assembly in Rust (see `crate::imp::packet`).

use crate::ack::ReceivedPackets;
use crate::endpoint::WarpError;
use crate::frame::Frame;
use crate::header::HeaderParser;
use crate::packet_number::{PN_MAX_LEN, PnLen, encoded_len};

/// A short header's largest size: the first byte, a 20-byte DCID and a
/// 4-byte packet number.
pub(crate) const MAX_SHORT_HEADER: usize = 1 + crate::header::MAX_CID_LEN + PN_MAX_LEN;
const TAG_LEN: usize = vesper_warp_crypto::TAG_LEN;

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
    // Frames go after a maximal header and move once the PN length is known.
    let mut at = MAX_SHORT_HEADER;
    let mut has_ack = false;
    if with_ack {
        let ack = received
            .ack_frame(now_ns, exponent)
            .map_err(|_| WarpError::Config("bad ack_delay_exponent"))?;
        if let Some(ack) = ack {
            at += Frame::Ack(ack).encode(&mut scratch[at..])?;
            has_ack = true;
        }
    }
    let data_len = app.len();
    if at + data_len > scratch.len() {
        return Err(WarpError::PacketTooLarge { len: at + data_len });
    }
    scratch[at..at + data_len].copy_from_slice(app);
    let payload_len = at + data_len - MAX_SHORT_HEADER;
    if payload_len == 0 {
        return Err(WarpError::Config("empty packet"));
    }
    let shortest = encoded_len(pn, largest_acked)
        .map_err(|_| WarpError::PacketNumbersExhausted)?
        .bytes();
    // The header-protection sample needs pn_len + payload >= 4: use a
    // longer PN encoding rather than padding (padding would put
    // ACK-only packets in flight).
    let pn_bytes = shortest.max(4usize.saturating_sub(payload_len));
    let pn_len = PnLen::new(pn_bytes as u8).expect("1..=4 bytes");
    let header_len = parser.write_short_to_peer(
        &mut scratch[..MAX_SHORT_HEADER],
        dcid,
        false,
        key_phase,
        pn,
        pn_len,
    )?;
    scratch.copy_within(MAX_SHORT_HEADER..MAX_SHORT_HEADER + payload_len, header_len);
    let plain_len = header_len + payload_len;
    if plain_len + TAG_LEN > max_datagram {
        return Err(WarpError::PacketTooLarge {
            len: plain_len + TAG_LEN,
        });
    }
    Ok((plain_len, header_len, has_ack))
}
