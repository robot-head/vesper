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
    pub const QPACK_DECOMPRESSION_FAILED: Self = Self(0x200);
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
            0x200 => "QPACK_DECOMPRESSION_FAILED",
            0x201 => "QPACK_ENCODER_STREAM_ERROR",
            0x202 => "QPACK_DECODER_STREAM_ERROR",
            _ => return None,
        })
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
