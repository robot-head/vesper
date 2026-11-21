//! HTTP/3 message validation (RFC 9114 §4.1.2, §4.2, §4.3).
//!
//! A request or response whose fields break these rules is malformed: a
//! stream error of type H3_MESSAGE_ERROR. Everything is checked; nothing is
//! normalised into a different message.

use crate::qpack::Field;

/// Words per field in the port's field list: name length, value length,
/// sensitive.
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

// ---------------------------------------------------------------------
// The Vesper build (port wave P3b lane 5, #1067): vesper/src/message.ves
// validates the flattened field list and returns byte ranges.
// ---------------------------------------------------------------------

use crate::qpack::port::{abi::Span, binding as port};

/// The field list as the port takes it: the bytes, and three words per
/// field (name length, value length, where the field begins in the bytes).
/// The buffers are per thread and reused, so a message allocates none.
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
fn text(bytes: &[u8], s: Span) -> String {
    String::from_utf8(bytes[s.lo as usize..s.hi as usize].to_vec()).expect("the port checked UTF-8")
}

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
