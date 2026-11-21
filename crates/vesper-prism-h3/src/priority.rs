//! Extensible priorities for HTTP/3 (RFC 9218).
//!
//! A request carries its priority in the `priority` field (a Structured
//! Field Dictionary, RFC 8941) and a client may change it later with a
//! PRIORITY_UPDATE frame on its control stream (§7.2). Two parameters are
//! defined: urgency `u` (0 highest to 7 lowest, default 3) and incremental
//! `i` (default false).
//!
//! The scheduler order used by [`crate::connection::Core::flush`] follows
//! §10: lower urgency values first; within one urgency, non-incremental
//! responses one at a time in stream order, then incremental responses
//! sharing the remaining capacity round-robin in fixed quanta.

use crate::frame::FrameError;

/// PRIORITY_UPDATE for a request stream and for a push (RFC 9218 §7.2).
pub const PRIORITY_UPDATE_REQUEST: u64 = 0xf0700;
pub const PRIORITY_UPDATE_PUSH: u64 = 0xf0701;
/// SETTINGS_NO_RFC7540_PRIORITIES (RFC 9218 §2.1).
pub const SETTINGS_NO_RFC7540_PRIORITIES: u64 = 0x09;

pub const DEFAULT_URGENCY: u8 = 3;
/// The least urgent level (RFC 9218 §4.1: urgency is 0 to 7).
pub const MAX_URGENCY: u8 = 7;

/// Words per stream handed to the port's scheduler: id, urgency,
/// incremental.
const STREAM_WORDS: usize = 3;

/// One response's priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Priority {
    /// 0 (most urgent) to 7.
    pub urgency: u8,
    pub incremental: bool,
}

impl Default for Priority {
    fn default() -> Self {
        Self {
            urgency: DEFAULT_URGENCY,
            incremental: false,
        }
    }
}

impl Priority {
    pub fn new(urgency: u8, incremental: bool) -> Self {
        Self {
            urgency: urgency.min(MAX_URGENCY),
            incremental,
        }
    }

    /// Parses a `priority` field value (the Vesper build).
    pub fn parse(value: &[u8]) -> Self {
        port::h3_priority_parse(value)
    }

    /// The field value (the Vesper build).
    pub fn serialize(&self) -> String {
        String::from_utf8(port::h3_priority_serialize(*self)).expect("the value is ASCII")
    }
}

/// A parsed PRIORITY_UPDATE frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriorityUpdate {
    /// True for a push (0xf0701), false for a request stream.
    pub push: bool,
    pub element_id: u64,
    pub priority: Priority,
}

impl PriorityUpdate {
    /// Parses a PRIORITY_UPDATE payload (the Vesper build).
    pub fn parse(frame_type: u64, payload: &[u8]) -> Result<Self, FrameError> {
        port::h3_priority_update_parse(frame_type, payload)
    }

    /// The PRIORITY_UPDATE frame (the Vesper build).
    pub fn encode(&self, out: &mut Vec<u8>) {
        crate::frame::written(out, port::h3_priority_update_encode(*self));
    }
}

/// The order in which streams with queued bytes are offered to the
/// transport (RFC 9218 §10), in the Vesper build.
pub fn schedule(streams: &[(u64, Priority)], rotate: usize) -> Vec<Vec<u64>> {
    let mut flat = Vec::with_capacity(streams.len() * STREAM_WORDS);
    for (id, p) in streams {
        flat.extend_from_slice(&[*id, u64::from(p.urgency), u64::from(p.incremental)]);
    }
    let words = port::h3_schedule(&flat, rotate as u64);
    let mut groups = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let n = words[at] as usize;
        groups.push(words[at + 1..at + 1 + n].to_vec());
        at += 1 + n;
    }
    groups
}

use crate::qpack::port::binding as port;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9218_examples() {
        assert_eq!(Priority::parse(b"u=5, i"), Priority::new(5, true));
        assert_eq!(Priority::parse(b"u=0"), Priority::new(0, false));
        assert_eq!(Priority::parse(b"i"), Priority::new(3, true));
        assert_eq!(Priority::parse(b"i=?0, u=1"), Priority::new(1, false));
        assert_eq!(Priority::parse(b""), Priority::default());
        // Out of range or wrong type: the parameter is ignored.
        assert_eq!(Priority::parse(b"u=8, i"), Priority::new(3, true));
        assert_eq!(Priority::parse(b"u=-1"), Priority::default());
        assert_eq!(Priority::parse(b"u=1.5"), Priority::default());
        assert_eq!(Priority::parse(b"u=\"2\""), Priority::default());
        assert_eq!(Priority::parse(b"i=1"), Priority::default());
        // Unknown members and parameters are ignored.
        assert_eq!(
            Priority::parse(b"foo=(a b);x=1, u=2;q, bar=:AAEC:, i"),
            Priority::new(2, true)
        );
        // Last value wins.
        assert_eq!(Priority::parse(b"u=1, u=6"), Priority::new(6, false));
        // Invalid dictionaries are ignored as a whole.
        for bad in [
            &b"u=1,"[..],
            b"U=1",
            b"u=1 i",
            b"u==1",
            b"u=1, i=?2",
            b"u=\"unterminated",
        ] {
            assert_eq!(Priority::parse(bad), Priority::default(), "{bad:?}");
        }
    }

    #[test]
    fn serialize_round_trips() {
        for u in 0..8 {
            for i in [false, true] {
                let p = Priority::new(u, i);
                assert_eq!(Priority::parse(p.serialize().as_bytes()), p);
            }
        }
        assert_eq!(Priority::default().serialize(), "");
        assert_eq!(Priority::new(1, true).serialize(), "u=1, i");
    }

    #[test]
    fn priority_update_frames() {
        let mut wire = Vec::new();
        PriorityUpdate {
            push: false,
            element_id: 8,
            priority: Priority::new(1, true),
        }
        .encode(&mut wire);
        // Type 0xf0700 is a 4-byte varint.
        assert_eq!(&wire[..4], &[0x80, 0x0f, 0x07, 0x00]);
        let payload = &wire[5..];
        let u = PriorityUpdate::parse(PRIORITY_UPDATE_REQUEST, payload).unwrap();
        assert_eq!(u.element_id, 8);
        assert_eq!(u.priority, Priority::new(1, true));
        assert_eq!(
            PriorityUpdate::parse(PRIORITY_UPDATE_REQUEST, &[]),
            Err(FrameError::Malformed)
        );
        assert_eq!(
            PriorityUpdate::parse(PRIORITY_UPDATE_REQUEST, &[4, b'u', b'=', 0xff]),
            Err(FrameError::Malformed)
        );
    }

    #[test]
    fn schedule_orders_by_urgency_then_incremental_round_robin() {
        let s = [
            (0, Priority::new(3, false)),
            (4, Priority::new(1, true)),
            (8, Priority::new(1, true)),
            (12, Priority::new(1, false)),
            (16, Priority::new(7, false)),
            (20, Priority::new(3, false)),
        ];
        assert_eq!(
            schedule(&s, 0),
            vec![vec![12], vec![4, 8], vec![0], vec![20], vec![16]]
        );
        assert_eq!(schedule(&s, 1)[1], vec![8, 4]);
    }
}
