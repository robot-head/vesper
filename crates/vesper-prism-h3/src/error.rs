//! HTTP/3 (RFC 9114 §8.1) and QPACK (RFC 9204 §6) application error codes.

use std::fmt;

/// An HTTP/3 or QPACK application error code, as carried in
/// CONNECTION_CLOSE, RESET_STREAM and STOP_SENDING.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Code(pub u64);

impl Code {
    pub const NO_ERROR: Self = Self(0x100);
    pub const GENERAL_PROTOCOL_ERROR: Self = Self(0x101);
    pub const INTERNAL_ERROR: Self = Self(0x102);
    pub const STREAM_CREATION_ERROR: Self = Self(0x103);
    pub const CLOSED_CRITICAL_STREAM: Self = Self(0x104);
    pub const FRAME_UNEXPECTED: Self = Self(0x105);
    pub const FRAME_ERROR: Self = Self(0x106);
    pub const EXCESSIVE_LOAD: Self = Self(0x107);
    pub const ID_ERROR: Self = Self(0x108);
    pub const SETTINGS_ERROR: Self = Self(0x109);
    pub const MISSING_SETTINGS: Self = Self(0x10a);
    pub const REQUEST_REJECTED: Self = Self(0x10b);
    pub const REQUEST_CANCELLED: Self = Self(0x10c);
    pub const REQUEST_INCOMPLETE: Self = Self(0x10d);
    pub const MESSAGE_ERROR: Self = Self(0x10e);
    pub const CONNECT_ERROR: Self = Self(0x10f);
    pub const VERSION_FALLBACK: Self = Self(0x110);
    /// RFC 9297 §2.1.1: a malformed HTTP Datagram or DATAGRAM capsule.
    pub const DATAGRAM_ERROR: Self = Self(0x33);
    pub const QPACK_DECOMPRESSION_FAILED: Self = Self(0x200);
    /// draft-ietf-webtrans-http3 §9.5: a stream for a session that is
    /// unknown and could not be buffered.
    pub const WT_BUFFERED_STREAM_REJECTED: Self = Self(0x3994_bd84);
    /// draft-ietf-webtrans-http3 §9.5: the stream's session is gone.
    pub const WT_SESSION_GONE: Self = Self(0x170d_7b68);
    /// The first code of the WebTransport application range (§4.4).
    pub const WT_APPLICATION_FIRST: u64 = 0x52e4_a40f_a8db;
    pub const QPACK_ENCODER_STREAM_ERROR: Self = Self(0x201);
    pub const QPACK_DECODER_STREAM_ERROR: Self = Self(0x202);

    /// The RFC name of a registered code.
    pub fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0x100 => "H3_NO_ERROR",
            0x101 => "H3_GENERAL_PROTOCOL_ERROR",
            0x102 => "H3_INTERNAL_ERROR",
            0x103 => "H3_STREAM_CREATION_ERROR",
            0x104 => "H3_CLOSED_CRITICAL_STREAM",
            0x105 => "H3_FRAME_UNEXPECTED",
            0x106 => "H3_FRAME_ERROR",
            0x107 => "H3_EXCESSIVE_LOAD",
            0x108 => "H3_ID_ERROR",
            0x109 => "H3_SETTINGS_ERROR",
            0x10a => "H3_MISSING_SETTINGS",
            0x10b => "H3_REQUEST_REJECTED",
            0x10c => "H3_REQUEST_CANCELLED",
            0x10d => "H3_REQUEST_INCOMPLETE",
            0x10e => "H3_MESSAGE_ERROR",
            0x10f => "H3_CONNECT_ERROR",
            0x110 => "H3_VERSION_FALLBACK",
            0x33 => "H3_DATAGRAM_ERROR",
            0x3994_bd84 => "WT_BUFFERED_STREAM_REJECTED",
            0x170d_7b68 => "WT_SESSION_GONE",
            0x200 => "QPACK_DECOMPRESSION_FAILED",
            0x201 => "QPACK_ENCODER_STREAM_ERROR",
            0x202 => "QPACK_DECODER_STREAM_ERROR",
            _ => return None,
        })
    }

    /// The HTTP/3 code that carries WebTransport application error `n` on
    /// RESET_STREAM and STOP_SENDING (draft-ietf-webtrans-http3 §4.4):
    /// `first + n + floor(n / 0x1e)`, skipping the reserved grease codes.
    pub const fn webtransport(n: u32) -> Self {
        let n = n as u64;
        Self(Self::WT_APPLICATION_FIRST + n + n / 0x1e)
    }

    /// The WebTransport application error a code carries, if it is in the
    /// WebTransport range and not a reserved code.
    pub const fn webtransport_code(self) -> Option<u32> {
        let last = Self::webtransport(u32::MAX).0;
        if self.0 < Self::WT_APPLICATION_FIRST || self.0 > last || self.is_reserved() {
            return None;
        }
        let shifted = self.0 - Self::WT_APPLICATION_FIRST;
        Some((shifted - shifted / 0x1f) as u32)
    }

    /// Reserved "grease" codes `0x1f * N + 0x21` (RFC 9114 §8.1).
    pub const fn is_reserved(self) -> bool {
        self.0 >= 0x21 && (self.0 - 0x21).is_multiple_of(0x1f)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "0x{:x}", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webtransport_codes_skip_grease() {
        for n in [0u32, 1, 0x1d, 0x1e, 0x1f, 1000, u32::MAX - 1, u32::MAX] {
            let c = Code::webtransport(n);
            assert!(!c.is_reserved(), "{n}");
            assert_eq!(c.webtransport_code(), Some(n), "{n}");
        }
        assert_eq!(Code::webtransport(0).0, 0x52e4_a40f_a8db);
        assert_eq!(Code::webtransport(u32::MAX).0, 0x52e5_ac98_3162);
        assert_eq!(Code::NO_ERROR.webtransport_code(), None);
        // Every code in the range is either reserved or maps back.
        for c in 0x52e4_a40f_a8db..0x52e4_a40f_a8db + 200 {
            let code = Code(c);
            match code.webtransport_code() {
                Some(n) => assert_eq!(Code::webtransport(n), code),
                None => assert!(code.is_reserved()),
            }
        }
    }
}
