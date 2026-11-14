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

use vesper_warp::varint;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Header,
    Skip { remaining: u64 },
    Value { ty: u64, len: usize },
}

/// Incremental capsule decoder for one stream's DATA payload.
#[derive(Debug)]
pub struct CapsuleReader {
    buf: Vec<u8>,
    state: State,
    max_value: usize,
}

impl CapsuleReader {
    /// `max_value` bounds every known capsule's value (a DATAGRAM capsule's
    /// payload, a close reason).
    pub fn new(max_value: usize) -> Self {
        Self {
            buf: Vec::new(),
            state: State::Header,
            max_value,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes held: at most one header or one bounded value.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    pub fn next_capsule(&mut self) -> Result<Option<Capsule>, CapsuleError> {
        loop {
            match self.state {
                State::Header => {
                    let Ok((ty, a)) = varint::decode(&self.buf) else {
                        return Ok(None);
                    };
                    let Ok((len, b)) = varint::decode(&self.buf[a..]) else {
                        return Ok(None);
                    };
                    self.buf.drain(..a + b);
                    match ty {
                        DATAGRAM | CLOSE_WEBTRANSPORT_SESSION | DRAIN_WEBTRANSPORT_SESSION => {
                            let bound = if ty == CLOSE_WEBTRANSPORT_SESSION {
                                4 + MAX_CLOSE_REASON
                            } else {
                                self.max_value
                            };
                            let len = usize::try_from(len).ok().filter(|l| *l <= bound).ok_or(
                                CapsuleError::TooLarge {
                                    capsule_type: ty,
                                    len,
                                },
                            )?;
                            self.state = State::Value { ty, len };
                        }
                        _ => {
                            self.state = State::Skip { remaining: len };
                            return Ok(Some(Capsule::Unknown(ty)));
                        }
                    }
                }
                State::Skip { remaining } => {
                    let n = remaining.min(self.buf.len() as u64) as usize;
                    self.buf.drain(..n);
                    let left = remaining - n as u64;
                    if left > 0 {
                        self.state = State::Skip { remaining: left };
                        return Ok(None);
                    }
                    self.state = State::Header;
                }
                State::Value { ty, len } => {
                    if self.buf.len() < len {
                        return Ok(None);
                    }
                    let value: Vec<u8> = self.buf.drain(..len).collect();
                    self.state = State::Header;
                    return parse(ty, value).map(Some);
                }
            }
        }
    }

    /// The stream ended: an error unless it ended between capsules.
    pub fn finish(&self) -> Result<(), CapsuleError> {
        if self.state == State::Header && self.buf.is_empty() {
            Ok(())
        } else {
            Err(CapsuleError::Truncated)
        }
    }
}

fn parse(ty: u64, value: Vec<u8>) -> Result<Capsule, CapsuleError> {
    Ok(match ty {
        DATAGRAM => Capsule::Datagram(value),
        CLOSE_WEBTRANSPORT_SESSION => {
            if value.len() < 4 {
                return Err(CapsuleError::Malformed { capsule_type: ty });
            }
            let code = u32::from_be_bytes([value[0], value[1], value[2], value[3]]);
            let reason = value[4..].to_vec();
            // The reason is UTF-8 (draft-ietf-webtrans-http3 §5).
            if std::str::from_utf8(&reason).is_err() {
                return Err(CapsuleError::Malformed { capsule_type: ty });
            }
            Capsule::CloseSession { code, reason }
        }
        DRAIN_WEBTRANSPORT_SESSION => {
            if !value.is_empty() {
                return Err(CapsuleError::Malformed { capsule_type: ty });
            }
            Capsule::DrainSession
        }
        _ => unreachable!("only known types are buffered"),
    })
}

/// Appends one capsule.
pub fn write_capsule(out: &mut Vec<u8>, ty: u64, value: &[u8]) {
    put_varint(out, ty);
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Appends a CLOSE_WEBTRANSPORT_SESSION capsule. `reason` must be UTF-8 of
/// at most [`MAX_CLOSE_REASON`] bytes (the caller checks).
pub fn write_close_session(out: &mut Vec<u8>, code: u32, reason: &[u8]) {
    let mut v = Vec::with_capacity(4 + reason.len());
    v.extend_from_slice(&code.to_be_bytes());
    v.extend_from_slice(reason);
    write_capsule(out, CLOSE_WEBTRANSPORT_SESSION, &v);
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
