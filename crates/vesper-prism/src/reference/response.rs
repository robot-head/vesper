//! The Rust reference build of the response encoder (see
//! `crate::response::ResponseBuilder`).

use crate::response::{
    ConnectionPolicy, EncodedResponse, ResponseContext, ResponseError, ResponseLimits,
};

fn policy_value(policy: ConnectionPolicy) -> &'static [u8] {
    match policy {
        ConnectionPolicy::KeepAlive => b"keep-alive",
        ConnectionPolicy::Close => b"close",
    }
}

#[derive(Debug)]
struct Header {
    name: String,
    value: Vec<u8>,
}

/// Validated response plan. No arbitrary status reason phrase is accepted.
///
/// `representation_length` is checked against `finish`'s actual byte slice,
/// including for HEAD. HEAD then omits the wire payload while emitting that
/// verified length. Statuses 204 and 304 require an empty representation and
/// omit Content-Length; 205 requires empty content and emits Content-Length: 0.
///
/// ```
/// use vesper_prism::response::{ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseLimits};
/// let response = ResponseBuilder::new(200, ResponseContext::Ordinary,
///     ConnectionPolicy::Close, 3, ResponseLimits::default())?
///     .header("Content-Type", b"application/octet-stream")?
///     .finish(&[0, 128, 255])?;
/// let wire = response.into_bytes();
/// assert!(wire.ends_with(&[0, 128, 255]));
/// # Ok::<(), vesper_prism::response::ResponseError>(())
/// ```
///
/// Finalization consumes the builder, preventing accidental reuse:
///
/// ```compile_fail
/// use vesper_prism::response::{ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseLimits};
/// let builder = ResponseBuilder::new(200, ResponseContext::Ordinary,
///     ConnectionPolicy::Close, 0, ResponseLimits::default()).unwrap();
/// let first = builder.finish(b"");
/// let duplicate = builder.finish(b""); // builder was moved
/// ```
#[derive(Debug)]
pub struct ResponseBuilder {
    status: u16,
    context: ResponseContext,
    connection: ConnectionPolicy,
    representation_length: usize,
    limits: ResponseLimits,
    headers: Vec<Header>,
    head_length: usize,
    header_count: usize,
}
impl ResponseBuilder {
    pub fn new(
        status: u16,
        context: ResponseContext,
        connection: ConnectionPolicy,
        representation_length: usize,
        limits: ResponseLimits,
    ) -> Result<Self, ResponseError> {
        if !(MIN_STATUS..=MAX_STATUS).contains(&status) {
            return Err(ResponseError::UnsupportedStatus);
        }
        // A valid 426 requires Upgrade, which this fixed-length subset cannot
        // produce. Informational switching responses are rejected above.
        if status == UPGRADE_REQUIRED {
            return Err(ResponseError::UnsupportedUpgrade);
        }
        if context == ResponseContext::Connect {
            return Err(ResponseError::UnsupportedConnect);
        }
        if body_forbidden(status) && representation_length != 0 {
            return Err(ResponseError::BodyForbidden);
        }
        if representation_length > limits.max_body_bytes {
            return Err(ResponseError::BodyLimit);
        }
        let mut head_length = add(STATUS_LINE_FIXED_LEN, reason(status).len())?;
        let mut header_count = 1;
        if carries_length(status) {
            head_length = add(head_length, b"Content-Length: ".len())?;
            head_length = add(head_length, decimal_length(representation_length))?;
            head_length = add(head_length, CRLF_LEN)?;
            header_count += 1;
        }
        head_length = add(head_length, b"Connection: ".len())?;
        head_length = add(head_length, policy_value(connection).len())?;
        head_length = add(head_length, 2 * CRLF_LEN)?; // Connection CRLF + final empty line.
        let builder = Self {
            status,
            context,
            connection,
            representation_length,
            limits,
            headers: Vec::new(),
            head_length,
            header_count,
        };
        builder.check_sizes(head_length, header_count)?;
        Ok(builder)
    }

    /// Append an ordinary field. Order and repetitions are preserved; names
    /// are emitted in ASCII lowercase. Framing/connection fields are reserved.
    /// Header values retain bytes, including obs-text, but forbid all controls.
    pub fn header(mut self, name: &str, value: &[u8]) -> Result<Self, ResponseError> {
        if name.is_empty() || !name.bytes().all(crate::reference::decoder::token) {
            return Err(ResponseError::InvalidHeaderName);
        }
        if value.iter().any(|byte| *byte < b' ' || *byte == DEL) {
            return Err(ResponseError::InvalidHeaderValue);
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("trailer")
            || name.eq_ignore_ascii_case("te")
        {
            return Err(ResponseError::UnsupportedTransferCoding);
        }
        if name.eq_ignore_ascii_case("upgrade") {
            return Err(ResponseError::UnsupportedUpgrade);
        }
        if [
            "content-length",
            "connection",
            "keep-alive",
            "proxy-connection",
        ]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(ResponseError::ReservedHeader);
        }
        let field_length = add(add(name.len(), value.len())?, 4)?;
        let head_length = add(self.head_length, field_length)?;
        let header_count = add(self.header_count, 1)?;
        self.check_sizes(head_length, header_count)?;
        // All byte/count limits have been checked before retaining copies.
        let mut name_bytes = copy_bytes(name.as_bytes())?;
        name_bytes.make_ascii_lowercase();
        let name = String::from_utf8(name_bytes).expect("validated ASCII token");
        let value = copy_bytes(value)?;
        self.headers
            .try_reserve(1)
            .map_err(|_| ResponseError::AllocationFailed)?;
        self.headers.push(Header { name, value });
        self.head_length = head_length;
        self.header_count = header_count;
        Ok(self)
    }

    /// Validate the exact representation and consume this builder into wire
    /// bytes. No partial output is returned on an error.
    pub fn finish(self, body: &[u8]) -> Result<EncodedResponse, ResponseError> {
        if body_forbidden(self.status) && !body.is_empty() {
            return Err(ResponseError::BodyForbidden);
        }
        if body.len() != self.representation_length {
            return Err(ResponseError::LengthMismatch {
                expected: self.representation_length,
                actual: body.len(),
            });
        }
        self.check_sizes(self.head_length, self.header_count)?;
        let wire_length = add(self.head_length, self.wire_body_length())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(wire_length)
            .map_err(|_| ResponseError::AllocationFailed)?;
        bytes.extend_from_slice(b"HTTP/1.1 ");
        bytes.extend_from_slice(&[
            b'0' + (self.status / 100) as u8,
            b'0' + ((self.status / 10) % 10) as u8,
            b'0' + (self.status % 10) as u8,
            b' ',
        ]);
        bytes.extend_from_slice(reason(self.status).as_bytes());
        bytes.extend_from_slice(b"\r\n");
        for header in &self.headers {
            bytes.extend_from_slice(header.name.as_bytes());
            bytes.extend_from_slice(b": ");
            bytes.extend_from_slice(&header.value);
            bytes.extend_from_slice(b"\r\n");
        }
        if carries_length(self.status) {
            bytes.extend_from_slice(b"Content-Length: ");
            append_decimal(&mut bytes, self.representation_length);
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(b"Connection: ");
        bytes.extend_from_slice(policy_value(self.connection));
        bytes.extend_from_slice(b"\r\n\r\n");
        if self.context != ResponseContext::Head {
            bytes.extend_from_slice(body);
        }
        debug_assert_eq!(bytes.len(), wire_length);
        Ok(EncodedResponse {
            bytes,
            connection: self.connection,
        })
    }
    fn wire_body_length(&self) -> usize {
        if self.context == ResponseContext::Head {
            0
        } else {
            self.representation_length
        }
    }
    fn check_sizes(&self, head_length: usize, header_count: usize) -> Result<(), ResponseError> {
        if head_length > self.limits.max_head_bytes {
            return Err(ResponseError::HeadLimit);
        }
        if header_count > self.limits.max_headers {
            return Err(ResponseError::HeaderCountLimit);
        }
        if add(head_length, self.wire_body_length())? > self.limits.max_total_bytes {
            return Err(ResponseError::TotalLimit);
        }
        Ok(())
    }
}

fn add(left: usize, right: usize) -> Result<usize, ResponseError> {
    left.checked_add(right).ok_or(ResponseError::LengthOverflow)
}
fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, ResponseError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| ResponseError::AllocationFailed)?;
    owned.extend_from_slice(bytes);
    Ok(owned)
}
/// Status codes this module treats specially (RFC 9110 §15).
const NO_CONTENT: u16 = 204;
const RESET_CONTENT: u16 = 205;
const NOT_MODIFIED: u16 = 304;
const UPGRADE_REQUIRED: u16 = 426;
/// The final statuses a response may carry (1xx are rejected).
const MIN_STATUS: u16 = 200;
const MAX_STATUS: u16 = 599;
/// `HTTP/1.1 NNN ` and the status line's CRLF.
const STATUS_LINE_FIXED_LEN: usize = 15;
const CRLF_LEN: usize = 2;
/// The ASCII delete control byte.
const DEL: u8 = 0x7f;
fn body_forbidden(status: u16) -> bool {
    matches!(status, NO_CONTENT | RESET_CONTENT | NOT_MODIFIED)
}
fn carries_length(status: u16) -> bool {
    !matches!(status, NO_CONTENT | NOT_MODIFIED)
}
fn decimal_length(mut value: usize) -> usize {
    let mut length = 1;
    while value >= 10 {
        value /= 10;
        length += 1;
    }
    length
}
fn append_decimal(bytes: &mut Vec<u8>, mut value: usize) {
    let mut digits = [0u8; usize::BITS as usize];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    bytes.extend_from_slice(&digits[start..]);
}
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => "",
    }
}
