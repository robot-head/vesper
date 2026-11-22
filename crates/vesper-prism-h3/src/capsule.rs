//! The Capsule protocol (RFC 9297 §3) and the capsules this crate uses.
//!
//! Once an extended CONNECT stream has a 2xx response, the payload of its
//! DATA frames is a sequence of capsules: a type varint, a length varint and
//! that many bytes. A capsule can span DATA frames, and one DATA frame can
//! hold several capsules. [`CapsuleReader`] is incremental like
//! [`crate::frame::FrameReader`]: bytes go in as DATA payload arrives and
//! complete capsules come out. Unknown capsule types are skipped without
//! buffering (§3.2); a known capsule longer than its bound is an error
//! before any of it is retained.
//!
//! Known types:
//! * DATAGRAM (0x00, RFC 9297 §3.5): an HTTP Datagram carried on the stream.
//! * CLOSE_WEBTRANSPORT_SESSION (0x2843) and DRAIN_WEBTRANSPORT_SESSION
//!   (0x78ae) from draft-ietf-webtrans-http3.

#[cfg(test)]
use crate::frame::put_varint;

pub const DATAGRAM: u64 = 0x00;
pub const CLOSE_WEBTRANSPORT_SESSION: u64 = 0x2843;
pub const DRAIN_WEBTRANSPORT_SESSION: u64 = 0x78ae;

/// Longest CLOSE_WEBTRANSPORT_SESSION reason (draft-ietf-webtrans-http3 §5).
pub const MAX_CLOSE_REASON: usize = 1024;

/// A decoded capsule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capsule {
    /// An HTTP Datagram payload (context and all: RFC 9297 leaves its
    /// interpretation to the upgraded protocol).
    Datagram(Vec<u8>),
    CloseSession {
        code: u32,
        reason: Vec<u8>,
    },
    DrainSession,
    /// An unknown type; its value was skipped.
    Unknown(u64),
}

/// A capsule that does not parse, or one larger than the reader accepts.
/// Either way the stream is malformed (H3_DATAGRAM_ERROR for a DATAGRAM
/// capsule, H3_MESSAGE_ERROR otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleError {
    TooLarge {
        capsule_type: u64,
        len: u64,
    },
    Malformed {
        capsule_type: u64,
    },
    /// The stream ended inside a capsule.
    Truncated,
}

// ---------------------------------------------------------------------
// The Vesper build (port wave P3b lane 5, #1067): vesper/src/capsule.ves.
// ---------------------------------------------------------------------

use crate::qpack::port::binding as port;

/// Incremental capsule decoder for one stream's DATA payload (the Vesper
/// build).
pub struct CapsuleReader {
    core: port::CapsuleReaderCore,
}

impl std::fmt::Debug for CapsuleReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapsuleReader")
            .field("buffered", &self.buffered())
            .finish_non_exhaustive()
    }
}

impl CapsuleReader {
    /// `max_value` bounds every known capsule's value.
    pub fn new(max_value: usize) -> Self {
        Self {
            core: port::CapsuleReaderCore::new(max_value as u64),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.core.push(bytes);
    }

    /// Bytes held: at most one header or one bounded value.
    pub fn buffered(&self) -> usize {
        self.core.buffered() as usize
    }

    pub fn next_capsule(&mut self) -> Result<Option<Capsule>, CapsuleError> {
        use crate::qpack::port::abi::CapsuleOut;
        let out = self.core.next()?;
        let bytes = || self.core.as_ref().out().as_slice().to_vec();
        Ok(Some(match out {
            CapsuleOut::None => return Ok(None),
            CapsuleOut::Datagram => Capsule::Datagram(bytes()),
            CapsuleOut::CloseSession(code) => Capsule::CloseSession {
                code,
                reason: bytes(),
            },
            CapsuleOut::DrainSession => Capsule::DrainSession,
            CapsuleOut::Unknown(t) => Capsule::Unknown(t),
        }))
    }

    /// The stream ended: an error unless it ended between capsules.
    pub fn finish(&self) -> Result<(), CapsuleError> {
        if self.core.at_boundary() {
            Ok(())
        } else {
            Err(CapsuleError::Truncated)
        }
    }
}

/// Appends one capsule.
pub fn write_capsule(out: &mut Vec<u8>, ty: u64, value: &[u8]) {
    crate::frame::written(out, port::h3_record_header(ty, value.len() as u64));
    out.extend_from_slice(value);
}

/// Appends a CLOSE_WEBTRANSPORT_SESSION capsule. `reason` must be UTF-8 of
/// at most [`MAX_CLOSE_REASON`] bytes (the caller checks).
pub fn write_close_session(out: &mut Vec<u8>, code: u32, reason: &[u8]) {
    crate::frame::written(out, port::h3_close_session(code, reason));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsules_round_trip_split_anywhere() {
        let mut wire = Vec::new();
        write_capsule(&mut wire, DATAGRAM, b"\x00hello");
        write_capsule(&mut wire, 0x17, &[9; 40]);
        write_capsule(&mut wire, DRAIN_WEBTRANSPORT_SESSION, b"");
        write_close_session(&mut wire, 0xdead_beef, b"bye");
        for split in 0..wire.len() {
            let mut r = CapsuleReader::new(64);
            let mut got = Vec::new();
            for part in [&wire[..split], &wire[split..]] {
                r.push(part);
                while let Some(c) = r.next_capsule().unwrap() {
                    got.push(c);
                }
            }
            assert!(r.finish().is_ok(), "split {split}");
            assert_eq!(
                got,
                [
                    Capsule::Datagram(b"\x00hello".to_vec()),
                    Capsule::Unknown(0x17),
                    Capsule::DrainSession,
                    Capsule::CloseSession {
                        code: 0xdead_beef,
                        reason: b"bye".to_vec()
                    },
                ],
                "split {split}"
            );
        }
    }

    #[test]
    fn bad_capsules_are_typed_errors() {
        let mut r = CapsuleReader::new(8);
        let mut wire = Vec::new();
        write_capsule(&mut wire, DATAGRAM, &[0; 9]);
        r.push(&wire);
        assert_eq!(
            r.next_capsule(),
            Err(CapsuleError::TooLarge {
                capsule_type: DATAGRAM,
                len: 9
            })
        );
        let mut r = CapsuleReader::new(8);
        let mut wire = Vec::new();
        write_capsule(&mut wire, CLOSE_WEBTRANSPORT_SESSION, &[0; 3]);
        r.push(&wire);
        assert!(matches!(
            r.next_capsule(),
            Err(CapsuleError::Malformed { .. })
        ));
        let mut r = CapsuleReader::new(8);
        let mut wire = Vec::new();
        write_close_session(&mut wire, 1, &[0xff, 0xfe]);
        r.push(&wire);
        assert!(matches!(
            r.next_capsule(),
            Err(CapsuleError::Malformed { .. })
        ));
        let mut r = CapsuleReader::new(8);
        let mut wire = Vec::new();
        write_capsule(&mut wire, DRAIN_WEBTRANSPORT_SESSION, &[1]);
        r.push(&wire);
        assert!(matches!(
            r.next_capsule(),
            Err(CapsuleError::Malformed { .. })
        ));
        // A huge unknown capsule is skipped without buffering.
        let mut r = CapsuleReader::new(8);
        let mut wire = Vec::new();
        put_varint(&mut wire, 0x4000);
        put_varint(&mut wire, 1 << 40);
        r.push(&wire);
        assert_eq!(r.next_capsule(), Ok(Some(Capsule::Unknown(0x4000))));
        r.push(&[0; 4096]);
        assert_eq!(r.next_capsule(), Ok(None));
        assert_eq!(r.buffered(), 0);
        assert_eq!(r.finish(), Err(CapsuleError::Truncated));
    }
}
