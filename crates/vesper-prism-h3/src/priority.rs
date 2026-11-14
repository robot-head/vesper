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

use vesper_warp::varint;

use crate::frame::{FrameError, put_varint, write_frame};

/// PRIORITY_UPDATE for a request stream and for a push (RFC 9218 §7.2).
pub const PRIORITY_UPDATE_REQUEST: u64 = 0xf0700;
pub const PRIORITY_UPDATE_PUSH: u64 = 0xf0701;
/// SETTINGS_NO_RFC7540_PRIORITIES (RFC 9218 §2.1).
pub const SETTINGS_NO_RFC7540_PRIORITIES: u64 = 0x09;

pub const DEFAULT_URGENCY: u8 = 3;

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
            urgency: urgency.min(7),
            incremental,
        }
    }

    /// Parses a `priority` field value or a PRIORITY_UPDATE value.
    ///
    /// A value that is not a valid Structured Field Dictionary is ignored
    /// as a whole (RFC 9218 §4, RFC 8941 §4.2): the result is the default.
    /// Within a valid dictionary, unknown members are ignored, and so are
    /// `u` outside 0..=7 or not an integer and `i` that is not a boolean
    /// (§4.1, §4.2). A member that appears twice takes its last value
    /// (RFC 8941 §3.2).
    pub fn parse(value: &[u8]) -> Self {
        let mut p = Self::default();
        let Some(members) = parse_dictionary(value) else {
            return p;
        };
        for (key, item) in members {
            match (key.as_slice(), item) {
                (b"u", Item::Integer(u)) if (0..=7).contains(&u) => p.urgency = u as u8,
                (b"u", _) => {}
                (b"i", Item::Boolean(b)) => p.incremental = b,
                _ => {}
            }
        }
        p
    }

    /// The field value, with default parameters omitted (an empty string
    /// is the default priority).
    pub fn serialize(&self) -> String {
        let mut s = String::new();
        if self.urgency != DEFAULT_URGENCY {
            s.push_str(&format!("u={}", self.urgency));
        }
        if self.incremental {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push('i');
        }
        s
    }
}

/// A Structured Field bare item, reduced to what priorities need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Integer(i64),
    Boolean(bool),
    /// Any other valid item (decimal, string, token, byte sequence, inner
    /// list): ignored by the priority parameters.
    Other,
}

fn ows(v: &[u8], at: &mut usize) {
    while *at < v.len() && matches!(v[*at], b' ' | b'\t') {
        *at += 1;
    }
}

fn sp(v: &[u8], at: &mut usize) {
    while *at < v.len() && v[*at] == b' ' {
        *at += 1;
    }
}

fn key(v: &[u8], at: &mut usize) -> Option<Vec<u8>> {
    let start = *at;
    match v.get(*at) {
        Some(c) if c.is_ascii_lowercase() || *c == b'*' => *at += 1,
        _ => return None,
    }
    while let Some(c) = v.get(*at) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-.*".contains(c) {
            *at += 1;
        } else {
            break;
        }
    }
    Some(v[start..*at].to_vec())
}

/// RFC 8941 §4.2.3: a bare item.
fn bare_item(v: &[u8], at: &mut usize) -> Option<Item> {
    let c = *v.get(*at)?;
    if c == b'-' || c.is_ascii_digit() {
        let start = *at;
        if c == b'-' {
            *at += 1;
        }
        let digits = *at;
        while v.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        let int_digits = *at - digits;
        if int_digits == 0 {
            return None;
        }
        if v.get(*at) == Some(&b'.') {
            if int_digits > 12 {
                return None;
            }
            *at += 1;
            let frac = *at;
            while v.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
            if *at == frac || *at - frac > 3 {
                return None;
            }
            return Some(Item::Other);
        }
        if int_digits > 15 {
            return None;
        }
        let n: i64 = std::str::from_utf8(&v[start..*at]).ok()?.parse().ok()?;
        return Some(Item::Integer(n));
    }
    match c {
        b'"' => {
            *at += 1;
            loop {
                let c = *v.get(*at)?;
                *at += 1;
                match c {
                    b'\\' => {
                        let e = *v.get(*at)?;
                        if e != b'"' && e != b'\\' {
                            return None;
                        }
                        *at += 1;
                    }
                    b'"' => return Some(Item::Other),
                    0x20..=0x7e => {}
                    _ => return None,
                }
            }
        }
        b'?' => {
            *at += 1;
            let b = match v.get(*at)? {
                b'0' => false,
                b'1' => true,
                _ => return None,
            };
            *at += 1;
            Some(Item::Boolean(b))
        }
        b':' => {
            *at += 1;
            loop {
                let c = *v.get(*at)?;
                *at += 1;
                match c {
                    b':' => return Some(Item::Other),
                    c if c.is_ascii_alphanumeric() || b"+/=".contains(&c) => {}
                    _ => return None,
                }
            }
        }
        c if c.is_ascii_alphabetic() || c == b'*' => {
            *at += 1;
            while let Some(c) = v.get(*at) {
                if c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~:/".contains(c) {
                    *at += 1;
                } else {
                    break;
                }
            }
            Some(Item::Other)
        }
        _ => None,
    }
}

/// RFC 8941 §4.2.3.2: parameters (parsed and dropped).
fn parameters(v: &[u8], at: &mut usize) -> Option<()> {
    while v.get(*at) == Some(&b';') {
        *at += 1;
        sp(v, at);
        key(v, at)?;
        if v.get(*at) == Some(&b'=') {
            *at += 1;
            bare_item(v, at)?;
        }
    }
    Some(())
}

/// An item or inner list with its parameters.
fn member_value(v: &[u8], at: &mut usize) -> Option<Item> {
    if v.get(*at) == Some(&b'(') {
        *at += 1;
        loop {
            sp(v, at);
            if v.get(*at) == Some(&b')') {
                *at += 1;
                parameters(v, at)?;
                return Some(Item::Other);
            }
            bare_item(v, at)?;
            parameters(v, at)?;
            match v.get(*at) {
                Some(b' ') | Some(b')') => {}
                _ => return None,
            }
        }
    }
    let item = bare_item(v, at)?;
    parameters(v, at)?;
    Some(item)
}

/// RFC 8941 §4.2.2. `None` if the value is not a valid dictionary.
fn parse_dictionary(v: &[u8]) -> Option<Vec<(Vec<u8>, Item)>> {
    let mut at = 0;
    sp(v, &mut at);
    let mut out = Vec::new();
    while at < v.len() {
        let k = key(v, &mut at)?;
        let item = if v.get(at) == Some(&b'=') {
            at += 1;
            member_value(v, &mut at)?
        } else {
            parameters(v, &mut at)?;
            Item::Boolean(true)
        };
        out.push((k, item));
        // Bounded: each member consumes at least one byte.
        ows(v, &mut at);
        if at >= v.len() {
            break;
        }
        if v[at] != b',' {
            return None;
        }
        at += 1;
        ows(v, &mut at);
        if at >= v.len() {
            // A trailing comma.
            return None;
        }
    }
    Some(out)
}

/// A parsed PRIORITY_UPDATE frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriorityUpdate {
    /// True for a push (0xf0701), false for a request stream.
    pub push: bool,
    pub element_id: u64,
    pub priority: Priority,
}

impl PriorityUpdate {
    pub fn parse(frame_type: u64, payload: &[u8]) -> Result<Self, FrameError> {
        let (element_id, n) = varint::decode(payload).map_err(|_| FrameError::Malformed)?;
        let value = &payload[n..];
        // The field value is ASCII (RFC 8941 §4.2).
        if value.iter().any(|b| !(0x20..=0x7e).contains(b)) {
            return Err(FrameError::Malformed);
        }
        Ok(Self {
            push: frame_type == PRIORITY_UPDATE_PUSH,
            element_id,
            priority: Priority::parse(value),
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut payload = Vec::new();
        put_varint(&mut payload, self.element_id);
        payload.extend_from_slice(self.priority.serialize().as_bytes());
        let ty = if self.push {
            PRIORITY_UPDATE_PUSH
        } else {
            PRIORITY_UPDATE_REQUEST
        };
        write_frame(out, ty, &payload);
    }
}

/// The order in which streams with queued bytes are offered to the
/// transport (RFC 9218 §10).
///
/// `streams` are `(stream_id, priority)` pairs. Returns groups in urgency
/// order. A group of one stream is a non-incremental stream, served until
/// it is blocked or empty before the next group. A group of several
/// streams holds the incremental streams of one urgency, which take turns
/// in quanta; `rotate` picks which of them goes first.
pub fn schedule(streams: &[(u64, Priority)], rotate: usize) -> Vec<Vec<u64>> {
    let mut by_urgency: [Vec<(u64, bool)>; 8] = Default::default();
    for (id, p) in streams {
        by_urgency[usize::from(p.urgency.min(7))].push((*id, p.incremental));
    }
    let mut groups = Vec::new();
    for mut g in by_urgency {
        if g.is_empty() {
            continue;
        }
        g.sort_by_key(|(id, inc)| (*inc, *id));
        // Non-incremental streams each form their own group, in order.
        let split = g.iter().position(|(_, inc)| *inc).unwrap_or(g.len());
        for (id, _) in &g[..split] {
            groups.push(vec![*id]);
        }
        let mut inc: Vec<u64> = g[split..].iter().map(|(id, _)| *id).collect();
        if !inc.is_empty() {
            let r = rotate % inc.len();
            inc.rotate_left(r);
            groups.push(inc);
        }
    }
    groups
}

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
