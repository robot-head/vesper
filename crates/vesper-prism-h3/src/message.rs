//! HTTP/3 message validation (RFC 9114 §4.1.2, §4.2, §4.3).
//!
//! A request or response whose fields break these rules is malformed: a
//! stream error of type H3_MESSAGE_ERROR. Everything is checked; nothing is
//! normalised into a different message.

use crate::qpack::Field;

/// Every u64 of up to 19 decimal digits fits; u64::MAX has 20.
#[cfg(not(feature = "vesper-port"))]
const MAX_CONTENT_LENGTH_DIGITS: usize = 19;
/// RFC 9110 §15: a status code is three digits, 100 to 599.
#[cfg(not(feature = "vesper-port"))]
const STATUS_DIGITS: usize = 3;
#[cfg(not(feature = "vesper-port"))]
const STATUS_CODES: std::ops::RangeInclusive<u16> = 100..=599;
/// ASCII DEL, never allowed in a pseudo-header value.
#[cfg(not(feature = "vesper-port"))]
const ASCII_DEL: u8 = 0x7f;
/// Words per field in the port's field list: name length, value length,
/// sensitive.
#[cfg(feature = "vesper-port")]
const META_WORDS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageError {
    /// A name with uppercase or non-token characters, or empty.
    InvalidFieldName,
    /// A value with NUL, CR or LF, or leading/trailing whitespace.
    InvalidFieldValue,
    /// A connection-specific field (§4.2), or TE other than "trailers".
    ConnectionSpecificField,
    /// A pseudo-header after a regular field.
    PseudoAfterRegular,
    /// An unknown pseudo-header, a request one in a response or vice versa,
    /// or one in trailers.
    UnknownPseudoHeader,
    DuplicatePseudoHeader,
    MissingPseudoHeader,
    /// An empty `:path`, or `:path`/`:scheme` on CONNECT.
    InvalidPseudoHeader,
    /// `:authority` and `host` disagree.
    AuthorityMismatch,
    /// Invalid or repeated content-length with different values.
    InvalidContentLength,
    /// DATA bytes and content-length disagree (§4.1.2).
    ContentLengthMismatch,
    /// `:status` not three digits, or an informational status as final.
    InvalidStatus,
    /// Plain CONNECT (without `:protocol`) is not supported.
    UnsupportedConnect,
}

/// A validated request head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub scheme: String,
    /// `:authority`, or else the `host` field.
    pub authority: Option<String>,
    pub path: String,
    /// `:protocol` of an extended CONNECT (RFC 9220); `None` otherwise.
    /// The connection refuses it unless it sent
    /// SETTINGS_ENABLE_CONNECT_PROTOCOL.
    pub protocol: Option<String>,
    /// Regular fields, in order.
    pub fields: Vec<Field>,
    pub content_length: Option<u64>,
}

/// A validated response head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub status: u16,
    pub fields: Vec<Field>,
    pub content_length: Option<u64>,
}

#[cfg(not(feature = "vesper-port"))]
fn token(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
}

#[cfg(not(feature = "vesper-port"))]
fn check_regular(f: &Field) -> Result<(), MessageError> {
    if f.name.is_empty() || !f.name.iter().copied().all(token) {
        return Err(MessageError::InvalidFieldName);
    }
    let v = &f.value;
    if v.iter().any(|&b| b == 0 || b == b'\r' || b == b'\n')
        || v.first().is_some_and(|b| matches!(b, b' ' | b'\t'))
        || v.last().is_some_and(|b| matches!(b, b' ' | b'\t'))
    {
        return Err(MessageError::InvalidFieldValue);
    }
    match &f.name[..] {
        b"connection" | b"keep-alive" | b"proxy-connection" | b"transfer-encoding" | b"upgrade" => {
            Err(MessageError::ConnectionSpecificField)
        }
        b"te" if &v[..] != b"trailers" => Err(MessageError::ConnectionSpecificField),
        _ => Ok(()),
    }
}

#[cfg(not(feature = "vesper-port"))]
fn utf8(v: &[u8]) -> Result<String, MessageError> {
    String::from_utf8(v.to_vec()).map_err(|_| MessageError::InvalidFieldValue)
}

#[cfg(not(feature = "vesper-port"))]
fn content_length(fields: &[Field]) -> Result<Option<u64>, MessageError> {
    let mut found: Option<u64> = None;
    for f in fields.iter().filter(|f| f.name == b"content-length") {
        if f.value.is_empty()
            || !f.value.iter().all(u8::is_ascii_digit)
            || f.value.len() > MAX_CONTENT_LENGTH_DIGITS
        {
            return Err(MessageError::InvalidContentLength);
        }
        let n: u64 = std::str::from_utf8(&f.value)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(MessageError::InvalidContentLength)?;
        if found.is_some_and(|prev| prev != n) {
            return Err(MessageError::InvalidContentLength);
        }
        found = Some(n);
    }
    Ok(found)
}

#[cfg(not(feature = "vesper-port"))]
/// Splits pseudo-headers (which must come first) from regular fields.
fn split(fields: Vec<Field>) -> Result<(Vec<Field>, Vec<Field>), MessageError> {
    let mut pseudo = Vec::new();
    let mut regular = Vec::new();
    for f in fields {
        if f.name.first() == Some(&b':') {
            if !regular.is_empty() {
                return Err(MessageError::PseudoAfterRegular);
            }
            if pseudo.iter().any(|p: &Field| p.name == f.name) {
                return Err(MessageError::DuplicatePseudoHeader);
            }
            pseudo.push(f);
        } else {
            check_regular(&f)?;
            regular.push(f);
        }
    }
    Ok((pseudo, regular))
}

#[cfg(not(feature = "vesper-port"))]
pub fn parse_request(fields: Vec<Field>) -> Result<RequestHead, MessageError> {
    let (pseudo, fields) = split(fields)?;
    let (mut method, mut scheme, mut authority, mut path, mut protocol) =
        (None, None, None, None, None);
    for p in pseudo {
        let slot = match &p.name[..] {
            b":method" => &mut method,
            b":scheme" => &mut scheme,
            b":authority" => &mut authority,
            b":path" => &mut path,
            b":protocol" => &mut protocol,
            // Includes :status.
            _ => return Err(MessageError::UnknownPseudoHeader),
        };
        if p.value.iter().any(|&b| b <= b' ' || b == ASCII_DEL) {
            return Err(MessageError::InvalidPseudoHeader);
        }
        *slot = Some(utf8(&p.value)?);
    }
    let method = method.ok_or(MessageError::MissingPseudoHeader)?;
    if method.is_empty()
        || !method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err(MessageError::InvalidPseudoHeader);
    }
    // Extended CONNECT (RFC 9220 §3, RFC 8441 §4) carries :protocol,
    // :scheme, :path and :authority; :protocol on any other method is
    // malformed. Plain CONNECT (a TCP tunnel) is not supported.
    match (method == "CONNECT", &protocol) {
        (true, None) => return Err(MessageError::UnsupportedConnect),
        (false, Some(_)) => return Err(MessageError::InvalidPseudoHeader),
        (true, Some(p)) if p.is_empty() => return Err(MessageError::InvalidPseudoHeader),
        _ => {}
    }
    let scheme = scheme.ok_or(MessageError::MissingPseudoHeader)?;
    let path = path.ok_or(MessageError::MissingPseudoHeader)?;
    if path.is_empty() {
        return Err(MessageError::InvalidPseudoHeader);
    }
    let host = fields.iter().find(|f| f.name == b"host");
    if let (Some(a), Some(h)) = (&authority, host)
        && a.as_bytes() != &h.value[..]
    {
        return Err(MessageError::AuthorityMismatch);
    }
    if fields.iter().filter(|f| f.name == b"host").count() > 1 {
        return Err(MessageError::AuthorityMismatch);
    }
    // §4.3.1: http/https require an authority (:authority or Host).
    let authority = match authority {
        Some(a) => Some(a),
        None => host.map(|h| utf8(&h.value)).transpose()?,
    };
    if matches!(scheme.as_str(), "http" | "https") && authority.as_deref().is_none_or(str::is_empty)
    {
        return Err(MessageError::MissingPseudoHeader);
    }
    let content_length = content_length(&fields)?;
    if protocol.is_some() && authority.as_deref().is_none_or(str::is_empty) {
        return Err(MessageError::MissingPseudoHeader);
    }
    Ok(RequestHead {
        method,
        scheme,
        authority,
        path,
        protocol,
        fields,
        content_length,
    })
}

#[cfg(not(feature = "vesper-port"))]
pub fn parse_response(fields: Vec<Field>) -> Result<ResponseHead, MessageError> {
    let (pseudo, fields) = split(fields)?;
    let mut status = None;
    for p in pseudo {
        if p.name != b":status" {
            return Err(MessageError::UnknownPseudoHeader);
        }
        if p.value.len() != STATUS_DIGITS || !p.value.iter().all(u8::is_ascii_digit) {
            return Err(MessageError::InvalidStatus);
        }
        status = Some(
            u16::from(p.value[0] - b'0') * 100
                + u16::from(p.value[1] - b'0') * 10
                + u16::from(p.value[2] - b'0'),
        );
    }
    let status = status.ok_or(MessageError::MissingPseudoHeader)?;
    if !STATUS_CODES.contains(&status) {
        return Err(MessageError::InvalidStatus);
    }
    let content_length = content_length(&fields)?;
    Ok(ResponseHead {
        status,
        fields,
        content_length,
    })
}

#[cfg(not(feature = "vesper-port"))]
/// Trailers carry no pseudo-headers (§4.3).
pub fn check_trailers(fields: &[Field]) -> Result<(), MessageError> {
    for f in fields {
        if f.name.first() == Some(&b':') {
            return Err(MessageError::UnknownPseudoHeader);
        }
        check_regular(f)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// The Vesper build (port wave P3b lane 5, #1067): vesper/src/message.ves
// validates the flattened field list and returns byte ranges.
// ---------------------------------------------------------------------

#[cfg(feature = "vesper-port")]
use crate::qpack::port::{abi::Span, binding as port};

/// The field list as the port takes it: the bytes, and three words per
/// field (name length, value length, where the field begins in the bytes).
/// The buffers are per thread and reused, so a message allocates none.
#[cfg(feature = "vesper-port")]
fn flatten<R>(fields: &[Field], with: impl FnOnce(&[u8], &[u64]) -> R) -> R {
    thread_local! {
        static SCRATCH: std::cell::RefCell<(Vec<u8>, Vec<u64>)> = const {
            std::cell::RefCell::new((Vec::new(), Vec::new()))
        };
    }
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let (bytes, meta) = &mut *scratch;
        bytes.clear();
        meta.clear();
        meta.reserve(fields.len() * META_WORDS);
        for f in fields {
            let start = bytes.len() as u64;
            bytes.extend_from_slice(&f.name);
            bytes.extend_from_slice(&f.value);
            meta.extend_from_slice(&[f.name.len() as u64, f.value.len() as u64, start]);
        }
        with(bytes, meta)
    })
}

/// A range the port checked as UTF-8.
#[cfg(feature = "vesper-port")]
fn text(bytes: &[u8], s: Span) -> String {
    String::from_utf8(bytes[s.lo as usize..s.hi as usize].to_vec()).expect("the port checked UTF-8")
}

#[cfg(feature = "vesper-port")]
pub fn parse_request(mut fields: Vec<Field>) -> Result<RequestHead, MessageError> {
    let (parts, method, scheme, authority, path, protocol) = flatten(&fields, |bytes, meta| {
        let parts = port::h3_parse_request(bytes, meta)?;
        Ok::<_, MessageError>((
            parts,
            text(bytes, parts.method),
            text(bytes, parts.scheme),
            parts.authority.map(|s| text(bytes, s)),
            text(bytes, parts.path),
            parts.protocol.map(|s| text(bytes, s)),
        ))
    })?;
    fields.drain(..parts.pseudo as usize);
    Ok(RequestHead {
        method,
        scheme,
        authority,
        path,
        protocol,
        fields,
        content_length: parts.content_length,
    })
}

#[cfg(feature = "vesper-port")]
pub fn parse_response(mut fields: Vec<Field>) -> Result<ResponseHead, MessageError> {
    let parts = flatten(&fields, port::h3_parse_response)?;
    fields.drain(..parts.pseudo as usize);
    Ok(ResponseHead {
        status: parts.status,
        fields,
        content_length: parts.content_length,
    })
}

/// Trailers carry no pseudo-headers (§4.3).
#[cfg(feature = "vesper-port")]
pub fn check_trailers(fields: &[Field]) -> Result<(), MessageError> {
    match flatten(fields, port::h3_check_trailers) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(list: &[(&str, &str)]) -> Result<RequestHead, MessageError> {
        parse_request(list.iter().map(|(n, v)| Field::new(*n, *v)).collect())
    }

    #[test]
    fn request_rules() {
        let ok = req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":authority", "a.test"),
            (":path", "/x"),
            ("accept", "*/*"),
        ])
        .unwrap();
        assert_eq!(ok.authority.as_deref(), Some("a.test"));
        assert_eq!(ok.fields.len(), 1);
        // Host instead of :authority.
        let h = req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/"),
            ("host", "b.test"),
        ])
        .unwrap();
        assert_eq!(h.authority.as_deref(), Some("b.test"));
        for (fields, err) in [
            (
                vec![(":method", "GET"), (":path", "/")],
                MessageError::MissingPseudoHeader,
            ),
            (
                vec![(":method", "GET"), (":scheme", "https"), (":path", "/")],
                MessageError::MissingPseudoHeader,
            ),
            (
                vec![(":method", "GET"), (":method", "GET")],
                MessageError::DuplicatePseudoHeader,
            ),
            (
                vec![("accept", "x"), (":method", "GET")],
                MessageError::PseudoAfterRegular,
            ),
            (
                vec![(":protocol", "websocket")],
                MessageError::MissingPseudoHeader,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":protocol", "websocket"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                ],
                MessageError::InvalidPseudoHeader,
            ),
            (
                vec![
                    (":method", "CONNECT"),
                    (":protocol", "webtransport"),
                    (":authority", "a"),
                    (":path", "/"),
                ],
                MessageError::MissingPseudoHeader,
            ),
            (
                vec![(":method", "CONNECT"), (":authority", "a:443")],
                MessageError::UnsupportedConnect,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", ""),
                ],
                MessageError::InvalidPseudoHeader,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("Accept", "x"),
                ],
                MessageError::InvalidFieldName,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("x", " y"),
                ],
                MessageError::InvalidFieldValue,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("connection", "close"),
                ],
                MessageError::ConnectionSpecificField,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("te", "gzip"),
                ],
                MessageError::ConnectionSpecificField,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("host", "b"),
                ],
                MessageError::AuthorityMismatch,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("content-length", "1"),
                    ("content-length", "2"),
                ],
                MessageError::InvalidContentLength,
            ),
            (
                vec![
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "a"),
                    (":path", "/"),
                    ("content-length", "-1"),
                ],
                MessageError::InvalidContentLength,
            ),
        ] {
            assert_eq!(req(&fields), Err(err), "{fields:?}");
        }
    }

    #[test]
    fn response_rules() {
        let r = parse_response(vec![
            Field::new(":status", "204"),
            Field::new("server", "prism"),
        ])
        .unwrap();
        assert_eq!(r.status, 204);
        assert_eq!(
            parse_response(vec![Field::new(":status", "20")]),
            Err(MessageError::InvalidStatus)
        );
        assert_eq!(
            parse_response(vec![Field::new(":path", "/")]),
            Err(MessageError::UnknownPseudoHeader)
        );
        assert_eq!(
            check_trailers(&[Field::new(":status", "200")]),
            Err(MessageError::UnknownPseudoHeader)
        );
    }
}
