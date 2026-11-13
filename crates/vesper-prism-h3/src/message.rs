//! HTTP/3 message validation (RFC 9114 §4.1.2, §4.2, §4.3).
//!
//! A request or response whose fields break these rules is malformed: a
//! stream error of type H3_MESSAGE_ERROR. Everything is checked; nothing is
//! normalised into a different message.

use crate::qpack::Field;

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
    /// CONNECT and extended CONNECT are not supported.
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

fn token(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
}

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

fn utf8(v: &[u8]) -> Result<String, MessageError> {
    String::from_utf8(v.to_vec()).map_err(|_| MessageError::InvalidFieldValue)
}

fn content_length(fields: &[Field]) -> Result<Option<u64>, MessageError> {
    let mut found: Option<u64> = None;
    for f in fields.iter().filter(|f| f.name == b"content-length") {
        if f.value.is_empty() || !f.value.iter().all(u8::is_ascii_digit) || f.value.len() > 19 {
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

pub fn parse_request(fields: Vec<Field>) -> Result<RequestHead, MessageError> {
    let (pseudo, fields) = split(fields)?;
    let (mut method, mut scheme, mut authority, mut path) = (None, None, None, None);
    for p in pseudo {
        let slot = match &p.name[..] {
            b":method" => &mut method,
            b":scheme" => &mut scheme,
            b":authority" => &mut authority,
            b":path" => &mut path,
            // Includes :protocol (extended CONNECT, RFC 9220) and :status.
            _ => return Err(MessageError::UnknownPseudoHeader),
        };
        if p.value.iter().any(|&b| b <= b' ' || b == 0x7f) {
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
    if method == "CONNECT" {
        return Err(MessageError::UnsupportedConnect);
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
    Ok(RequestHead {
        method,
        scheme,
        authority,
        path,
        fields,
        content_length,
    })
}

pub fn parse_response(fields: Vec<Field>) -> Result<ResponseHead, MessageError> {
    let (pseudo, fields) = split(fields)?;
    let mut status = None;
    for p in pseudo {
        if p.name != b":status" {
            return Err(MessageError::UnknownPseudoHeader);
        }
        if p.value.len() != 3 || !p.value.iter().all(u8::is_ascii_digit) {
            return Err(MessageError::InvalidStatus);
        }
        status = Some(
            u16::from(p.value[0] - b'0') * 100
                + u16::from(p.value[1] - b'0') * 10
                + u16::from(p.value[2] - b'0'),
        );
    }
    let status = status.ok_or(MessageError::MissingPseudoHeader)?;
    if !(100..=599).contains(&status) {
        return Err(MessageError::InvalidStatus);
    }
    let content_length = content_length(&fields)?;
    Ok(ResponseHead {
        status,
        fields,
        content_length,
    })
}

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
                MessageError::UnknownPseudoHeader,
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
