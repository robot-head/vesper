//! Bounded HTTP/1.1 request framing for the Prism host prototype.
//!
//! Only origin-form requests (and `OPTIONS *`) with no body or a single
//! `Content-Length` are supported. Transfer codings, upgrades, CONNECT,
//! expectations, HTTP/1.0 and absolute-form targets fail closed. This is not a
//! server, source-language linearity implementation, TLS stack or SIMD parser.
//! All parse errors poison the decoder: the caller must close the connection,
//! not search the remaining bytes for another request boundary.

#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    fmt,
    net::{Ipv4Addr, Ipv6Addr},
};

/// Logical byte/count limits, checked before retaining input or copying bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_head_bytes: usize,
    pub max_headers: usize,
    pub max_body_bytes: usize,
    /// Includes the current request and all queued pipelined bytes.
    pub max_buffered_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_head_bytes: 16 * 1024,
            max_headers: 100,
            max_body_bytes: 1024 * 1024,
            max_buffered_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    AllocationFailed,
    BufferedLimit,
    HeadLimit,
    HeaderCountLimit,
    BodyLimit,
    InvalidLineEnding,
    InvalidRequestLine,
    UnsupportedVersion,
    InvalidTarget,
    UnsupportedMethod,
    InvalidHeader,
    InvalidHost,
    MissingHost,
    DuplicateHost,
    DuplicateContentLength,
    InvalidContentLength,
    AmbiguousFraming,
    UnsupportedTransferCoding,
    UnsupportedUpgrade,
    UnsupportedExpectation,
    InvalidConnection,
    UnexpectedEof,
    InputEnded,
    Poisoned,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP/1.1 framing error: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Header names are ASCII lowercase; values retain bytes after edge OWS trim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead {
    /// Case is preserved: HTTP methods are case-sensitive.
    pub method: String,
    /// Validated ASCII origin-form or `OPTIONS *`; no URI decoding is done.
    pub target: String,
    pub headers: Vec<Header>,
    pub content_length: usize,
}
impl RequestHead {
    /// ASCII case-insensitive lookup. Repeated non-framing fields remain separate.
    pub fn headers<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.headers
            .iter()
            .filter(move |header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_slice())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub head: RequestHead,
    pub body: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedRequest {
    pub request: Request,
    /// Wire bytes removed for this request; queued pipeline bytes are untouched.
    pub consumed: usize,
}

struct Pending {
    head: RequestHead,
    head_bytes: usize,
}

/// Incremental, connection-local decoder with an explicitly bounded byte queue.
///
/// `push` is all-or-nothing. `next_request` returns `Ok(None)` for incomplete
/// input and an owned request only when its entire fixed-length body is present.
/// Head bytes are scanned once and parsed once; body waits do not reparse heads.
/// A ring queue avoids repeatedly moving a large pipelined suffix.
///
/// ```
/// use vesper_prism::{Decoder, Limits};
/// let mut decoder = Decoder::new(Limits::default())?;
/// decoder.push(b"POST /events HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\n\r\na")?;
/// assert!(decoder.next_request()?.is_none());
/// decoder.push(b"bc")?;
/// let decoded = decoder.next_request()?.unwrap();
/// assert_eq!(decoded.request.body, b"abc");
/// assert_eq!(decoder.buffered_bytes(), 0);
/// # Ok::<(), vesper_prism::Error>(())
/// ```
pub struct Decoder {
    limits: Limits,
    buffer: VecDeque<u8>,
    scan: usize,
    line_start: usize,
    after_cr: bool,
    pending: Option<Pending>,
    ended: bool,
    poisoned: bool,
}
impl Decoder {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        if limits.max_head_bytes < 4
            || limits.max_headers == 0
            || limits
                .max_head_bytes
                .checked_add(limits.max_body_bytes)
                .is_none_or(|size| size > limits.max_buffered_bytes)
        {
            return Err(Error::InvalidLimits);
        }
        let mut buffer = VecDeque::new();
        buffer
            .try_reserve_exact(limits.max_buffered_bytes)
            .map_err(|_| Error::AllocationFailed)?;
        Ok(Self {
            limits,
            buffer,
            scan: 0,
            line_start: 0,
            after_cr: false,
            pending: None,
            ended: false,
            poisoned: false,
        })
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
    /// Unconsumed bytes, possibly split at the ring's wrap point.
    pub fn buffered_slices(&self) -> (&[u8], &[u8]) {
        self.buffer.as_slices()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.ended {
            return Err(Error::InputEnded);
        }
        if bytes.len() > self.limits.max_buffered_bytes - self.buffer.len() {
            return self.fail(Error::BufferedLimit);
        }
        self.buffer.extend(bytes);
        Ok(())
    }
    /// Mark transport EOF. Already complete pipelined requests can still drain.
    /// A subsequent `next_request` reports `UnexpectedEof` for any partial frame.
    pub fn end_input(&mut self) {
        self.ended = true;
    }
    pub fn next_request(&mut self) -> Result<Option<DecodedRequest>, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.pending.is_none() {
            if let Some(head_bytes) = self.scan_head()? {
                let bytes = self.buffer.range(..head_bytes).copied().collect::<Vec<_>>();
                match parse_head(&bytes, self.limits) {
                    Ok(head) => self.pending = Some(Pending { head, head_bytes }),
                    Err(error) => return self.fail(error),
                }
            } else {
                return self.incomplete();
            }
        }
        let pending = self.pending.as_ref().expect("head was parsed");
        // The limits constructor checked max_head + max_body for overflow.
        let consumed = pending.head_bytes + pending.head.content_length;
        if self.buffer.len() < consumed {
            return self.incomplete();
        }
        let body = self
            .buffer
            .range(pending.head_bytes..consumed)
            .copied()
            .collect();
        let pending = self.pending.take().expect("head was parsed");
        self.buffer.drain(..consumed);
        self.scan = 0;
        self.line_start = 0;
        self.after_cr = false;
        Ok(Some(DecodedRequest {
            request: Request {
                head: pending.head,
                body,
            },
            consumed,
        }))
    }
    fn incomplete(&mut self) -> Result<Option<DecodedRequest>, Error> {
        if self.ended && !self.buffer.is_empty() {
            self.fail(Error::UnexpectedEof)
        } else {
            Ok(None)
        }
    }
    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.poisoned = true;
        Err(error)
    }
    fn scan_head(&mut self) -> Result<Option<usize>, Error> {
        while self.scan < self.buffer.len() {
            if self.scan == self.limits.max_head_bytes {
                return self.fail(Error::HeadLimit);
            }
            let byte = self.buffer[self.scan];
            let index = self.scan;
            self.scan += 1;
            if self.after_cr {
                if byte != b'\n' {
                    return self.fail(Error::InvalidLineEnding);
                }
                self.after_cr = false;
                if index - 1 == self.line_start {
                    if self.line_start == 0 {
                        return self.fail(Error::InvalidRequestLine);
                    }
                    return Ok(Some(self.scan));
                }
                self.line_start = self.scan;
            } else if byte == b'\r' {
                self.after_cr = true;
            } else if byte == b'\n' {
                return self.fail(Error::InvalidLineEnding);
            }
        }
        // No extra byte could fit, so this head can never become complete.
        if self.scan == self.limits.max_head_bytes {
            return self.fail(Error::HeadLimit);
        }
        Ok(None)
    }
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
fn trim_ows(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}
fn valid_target(method: &[u8], target: &[u8]) -> bool {
    if target == b"*" {
        return method == b"OPTIONS";
    }
    if !target.starts_with(b"/") || target.starts_with(b"//") {
        return false;
    }
    let mut index = 0;
    while index < target.len() {
        let byte = target[index];
        if byte == b'%' {
            if target.get(index + 1).is_none_or(|b| !b.is_ascii_hexdigit())
                || target.get(index + 2).is_none_or(|b| !b.is_ascii_hexdigit())
            {
                return false;
            }
            index += 3;
        } else if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?".contains(&byte) {
            index += 1;
        } else {
            return false;
        }
    }
    true
}
fn valid_port(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && bytes
            .iter()
            .try_fold(0u16, |port, byte| {
                port.checked_mul(10)?.checked_add(u16::from(byte - b'0'))
            })
            .is_some()
}
fn valid_host(bytes: &[u8]) -> bool {
    if bytes.first() == Some(&b'[') {
        let Some(end) = bytes.iter().position(|b| *b == b']') else {
            return false;
        };
        let Ok(address) = std::str::from_utf8(&bytes[1..end]) else {
            return false;
        };
        if address.parse::<Ipv6Addr>().is_err() {
            return false;
        }
        return bytes.len() == end + 1
            || (bytes.get(end + 1) == Some(&b':') && valid_port(&bytes[end + 2..]));
    }
    let mut parts = bytes.split(|b| *b == b':');
    let mut host = parts.next().unwrap_or_default();
    if let Some(port) = parts.next()
        && (!valid_port(port) || parts.next().is_some())
    {
        return false;
    }
    // Avoid alternate numeric-host interpretations such as 127.1, integer IP
    // addresses and octal-looking IPv4 components. No DNS/IP normalization is
    // performed on accepted bytes.
    if host
        .iter()
        .all(|byte| byte.is_ascii_digit() || *byte == b'.')
    {
        return std::str::from_utf8(host).is_ok_and(|host| host.parse::<Ipv4Addr>().is_ok());
    }
    if host.last() == Some(&b'.') {
        host = &host[..host.len() - 1];
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split(|b| *b == b'.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.first().is_some_and(u8::is_ascii_alphanumeric)
                && label.last().is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        })
}
fn parse_head(bytes: &[u8], limits: Limits) -> Result<RequestHead, Error> {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let line = lines.next().ok_or(Error::InvalidRequestLine)?;
    let line = line.strip_suffix(b"\r").ok_or(Error::InvalidLineEnding)?;
    let mut words = line.split(|byte| *byte == b' ');
    let method = words.next().unwrap_or_default();
    let target = words.next().ok_or(Error::InvalidRequestLine)?;
    let version = words.next().ok_or(Error::InvalidRequestLine)?;
    if method.is_empty()
        || !method.iter().copied().all(token)
        || target.is_empty()
        || words.next().is_some()
    {
        return Err(Error::InvalidRequestLine);
    }
    if version != b"HTTP/1.1" {
        return Err(Error::UnsupportedVersion);
    }
    if method.eq_ignore_ascii_case(b"CONNECT") {
        return Err(Error::UnsupportedMethod);
    }
    if !valid_target(method, target) {
        return Err(Error::InvalidTarget);
    }
    let mut headers = Vec::new();
    let mut host = false;
    let mut content_length = None;
    let mut transfer_coding = false;
    let mut upgrade = false;
    let mut expectation = false;
    for line in lines {
        if line == b"\r" {
            break;
        }
        if headers.len() == limits.max_headers {
            return Err(Error::HeaderCountLimit);
        }
        let line = line.strip_suffix(b"\r").ok_or(Error::InvalidLineEnding)?;
        let colon = line
            .iter()
            .position(|b| *b == b':')
            .ok_or(Error::InvalidHeader)?;
        let name = &line[..colon];
        let value = &line[colon + 1..];
        if name.is_empty()
            || !name.iter().copied().all(token)
            || value.iter().any(|b| (*b < 32 && *b != b'\t') || *b == 127)
        {
            return Err(Error::InvalidHeader);
        }
        let name = name
            .iter()
            .map(u8::to_ascii_lowercase)
            .map(char::from)
            .collect::<String>();
        let value = trim_ows(value);
        match name.as_str() {
            "host" => {
                if host {
                    return Err(Error::DuplicateHost);
                }
                if !valid_host(value) {
                    return Err(Error::InvalidHost);
                }
                host = true;
            }
            "content-length" => {
                if content_length.is_some() {
                    return Err(Error::DuplicateContentLength);
                }
                if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                    return Err(Error::InvalidContentLength);
                }
                let length = value
                    .iter()
                    .try_fold(0usize, |length, byte| {
                        length
                            .checked_mul(10)?
                            .checked_add(usize::from(byte - b'0'))
                    })
                    .ok_or(Error::InvalidContentLength)?;
                if length > limits.max_body_bytes {
                    return Err(Error::BodyLimit);
                }
                content_length = Some(length);
            }
            "transfer-encoding" => transfer_coding = true,
            "upgrade" => upgrade = true,
            "expect" => expectation = true,
            "connection" => {
                for option in value.split(|b| *b == b',').map(trim_ows) {
                    if option.is_empty() || !option.iter().copied().all(token) {
                        return Err(Error::InvalidConnection);
                    }
                    if option.eq_ignore_ascii_case(b"upgrade") {
                        upgrade = true;
                    }
                    if [b"content-length".as_slice(), b"transfer-encoding", b"host"]
                        .iter()
                        .any(|name| option.eq_ignore_ascii_case(name))
                    {
                        return Err(Error::InvalidConnection);
                    }
                }
            }
            _ => {}
        }
        headers.push(Header {
            name,
            value: value.to_vec(),
        });
    }
    if !host {
        return Err(Error::MissingHost);
    }
    if transfer_coding && content_length.is_some() {
        return Err(Error::AmbiguousFraming);
    }
    if transfer_coding {
        return Err(Error::UnsupportedTransferCoding);
    }
    if upgrade {
        return Err(Error::UnsupportedUpgrade);
    }
    if expectation {
        return Err(Error::UnsupportedExpectation);
    }
    Ok(RequestHead {
        method: String::from_utf8(method.to_vec()).expect("ASCII token"),
        target: String::from_utf8(target.to_vec()).expect("ASCII target"),
        headers,
        content_length: content_length.unwrap_or(0),
    })
}
