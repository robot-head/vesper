use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
use std::io::{self, BufRead, Write};
use vesper_serde::{IndexedJson, Kind, Node};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolErrorKind {
    InvalidLimits,
    Io,
    HeaderLimit,
    InvalidHeader,
    MissingLength,
    DuplicateLength,
    InvalidLength,
    BodyLimit,
    Truncated,
    UnsupportedEncoding,
    Utf8,
    Json,
    DuplicateJsonKey,
    JsonLimit,
    InvalidEnvelope,
    InvalidId,
    OutputLimit,
    WorkLimit,
    Allocation,
}
#[derive(Debug)]
pub struct ProtocolError {
    pub kind: ProtocolErrorKind,
    source: Option<io::Error>,
}
impl ProtocolError {
    fn new(kind: ProtocolErrorKind) -> Self {
        Self { kind, source: None }
    }
}
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LSP {:?}", self.kind)?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as _)
    }
}
impl From<io::Error> for ProtocolError {
    fn from(source: io::Error) -> Self {
        Self {
            kind: ProtocolErrorKind::Io,
            source: Some(source),
        }
    }
}
type Result<T> = core::result::Result<T, ProtocolError>;
fn error(kind: ProtocolErrorKind) -> ProtocolError {
    ProtocolError::new(kind)
}
fn bound(n: usize, max: usize, kind: ProtocolErrorKind) -> Result<()> {
    if n > max { Err(error(kind)) } else { Ok(()) }
}
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(error(ProtocolErrorKind::JsonLimit))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimits {
    pub max_body_bytes: usize,
    pub max_reply_bytes: usize,
    pub max_header_bytes: usize,
    pub max_header_line_bytes: usize,
    pub max_headers: usize,
    pub max_json_depth: usize,
    pub max_json_nodes: usize,
    pub max_json_string_bytes: usize,
    pub max_json_total_string_bytes: usize,
    /// Charged input IndexedJson capacity only. Output traversal is node bounded.
    pub max_json_index_bytes: usize,
    pub max_json_work: usize,
    pub max_id_bytes: usize,
    pub max_method_bytes: usize,
}
impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 2 * 1024 * 1024,
            max_reply_bytes: 2 * 1024 * 1024,
            max_header_bytes: 8192,
            max_header_line_bytes: 1024,
            max_headers: 32,
            max_json_depth: 64,
            max_json_nodes: 65_536,
            max_json_string_bytes: 2 * 1024 * 1024,
            max_json_total_string_bytes: 2 * 1024 * 1024,
            max_json_index_bytes: 4 * 1024 * 1024,
            max_json_work: 32 * 1024 * 1024,
            max_id_bytes: 256,
            max_method_bytes: 256,
        }
    }
}
impl ProtocolLimits {
    pub const HARD: Self = Self {
        max_body_bytes: 8 * 1024 * 1024,
        max_reply_bytes: 8 * 1024 * 1024,
        max_header_bytes: 16384,
        max_header_line_bytes: 4096,
        max_headers: 128,
        max_json_depth: 64,
        max_json_nodes: 262_144,
        max_json_string_bytes: 8 * 1024 * 1024,
        max_json_total_string_bytes: 8 * 1024 * 1024,
        max_json_index_bytes: 16 * 1024 * 1024,
        max_json_work: 128 * 1024 * 1024,
        max_id_bytes: 1024,
        max_method_bytes: 1024,
    };
    pub fn validate(self) -> Result<()> {
        let a = [
            self.max_body_bytes,
            self.max_reply_bytes,
            self.max_header_bytes,
            self.max_header_line_bytes,
            self.max_headers,
            self.max_json_depth,
            self.max_json_nodes,
            self.max_json_string_bytes,
            self.max_json_total_string_bytes,
            self.max_json_index_bytes,
            self.max_json_work,
            self.max_id_bytes,
            self.max_method_bytes,
        ];
        let h = Self::HARD;
        let b = [
            h.max_body_bytes,
            h.max_reply_bytes,
            h.max_header_bytes,
            h.max_header_line_bytes,
            h.max_headers,
            h.max_json_depth,
            h.max_json_nodes,
            h.max_json_string_bytes,
            h.max_json_total_string_bytes,
            h.max_json_index_bytes,
            h.max_json_work,
            h.max_id_bytes,
            h.max_method_bytes,
        ];
        if a.iter().zip(b).any(|(a, b)| *a > b) {
            return Err(error(ProtocolErrorKind::InvalidLimits));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
    Integer(i32),
    String(String),
}
impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> core::result::Result<S::Ok, S::Error> {
        match self {
            Self::Integer(n) => s.serialize_i32(*n),
            Self::String(v) => s.serialize_str(v),
        }
    }
}
#[derive(Debug, PartialEq)]
pub enum ResponsePayload {
    Result(Value),
    Error {
        code: i32,
        message: String,
        data: Option<Value>,
    },
}
#[derive(Debug, PartialEq)]
pub enum Envelope {
    Request {
        id: Id,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Response {
        id: Option<Id>,
        payload: ResponsePayload,
    },
}
impl Serialize for Envelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct RpcError<'a> {
            code: i32,
            message: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            data: Option<&'a Value>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", "2.0")?;
        match self {
            Self::Request { id, method, params } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", method)?;
                if let Some(params) = params {
                    map.serialize_entry("params", params)?;
                }
            }
            Self::Notification { method, params } => {
                map.serialize_entry("method", method)?;
                if let Some(params) = params {
                    map.serialize_entry("params", params)?;
                }
            }
            Self::Response { id, payload } => {
                map.serialize_entry("id", id)?;
                match payload {
                    ResponsePayload::Result(value) => map.serialize_entry("result", value)?,
                    ResponsePayload::Error {
                        code,
                        message,
                        data,
                    } => map.serialize_entry(
                        "error",
                        &RpcError {
                            code: *code,
                            message,
                            data: data.as_ref(),
                        },
                    )?,
                }
            }
        }
        map.end()
    }
}

/// Reads a single bounded CRLF header line using fill_buf/consume. There is no
/// unbounded read_line/read_until. EOF is clean only at a fresh frame boundary.
fn header_line<R: BufRead>(
    reader: &mut R,
    total: &mut usize,
    limits: ProtocolLimits,
) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if line.is_empty() && *total == 0 {
                Ok(None)
            } else {
                Err(error(ProtocolErrorKind::Truncated))
            };
        }
        let remaining = limits
            .max_header_line_bytes
            .saturating_sub(line.len())
            .min(limits.max_header_bytes.saturating_sub(*total));
        let scan = &buffer[..buffer.len().min(remaining.saturating_add(1))];
        let count = scan
            .iter()
            .position(|b| *b == b'\n')
            .map_or(scan.len(), |i| i + 1);
        let next = add(line.len(), count)?;
        let all = add(*total, count)?;
        bound(
            next,
            limits.max_header_line_bytes,
            ProtocolErrorKind::HeaderLimit,
        )?;
        bound(all, limits.max_header_bytes, ProtocolErrorKind::HeaderLimit)?;
        line.try_reserve(count)
            .map_err(|_| error(ProtocolErrorKind::Allocation))?;
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        *total = all;
        if line.last() == Some(&b'\n') {
            if !line.ends_with(b"\r\n") {
                return Err(error(ProtocolErrorKind::InvalidHeader));
            }
            return Ok(Some(line));
        }
    }
}
fn content_type(value: &str) -> Result<()> {
    let mut parts = value.split(';');
    let mime = parts.next().unwrap_or("").trim();
    if !mime.eq_ignore_ascii_case("application/vscode-jsonrpc")
        && !mime.eq_ignore_ascii_case("application/json")
    {
        return Err(error(ProtocolErrorKind::UnsupportedEncoding));
    }
    let mut charset = false;
    for part in parts {
        let (k, v) = part
            .trim()
            .split_once('=')
            .ok_or(error(ProtocolErrorKind::InvalidHeader))?;
        if !k.trim().eq_ignore_ascii_case("charset") || charset {
            return Err(error(ProtocolErrorKind::InvalidHeader));
        }
        charset = true;
        let raw = v.trim();
        let value = if raw.starts_with('"') {
            raw.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .ok_or(error(ProtocolErrorKind::InvalidHeader))?
        } else {
            raw
        };
        if !value.eq_ignore_ascii_case("utf-8") && !value.eq_ignore_ascii_case("utf8") {
            return Err(error(ProtocolErrorKind::UnsupportedEncoding));
        }
    }
    Ok(())
}
/// A framing error does not promise resynchronization. The caller should stop
/// that stream. Successfully returned bodies can be rejected independently by
/// parse_envelope while the next frame boundary remains known.
pub fn read_frame<R: BufRead>(reader: &mut R, limits: ProtocolLimits) -> Result<Option<Vec<u8>>> {
    limits.validate()?;
    let (mut total, mut count, mut length, mut typed) = (0, 0, None, false);
    loop {
        let Some(line) = header_line(reader, &mut total, limits)? else {
            return Ok(None);
        };
        if line == b"\r\n" {
            break;
        }
        count += 1;
        bound(count, limits.max_headers, ProtocolErrorKind::HeaderLimit)?;
        let line = &line[..line.len() - 2];
        if !line.iter().all(|b| matches!(b, 0x20..=0x7e | b'\t')) {
            return Err(error(ProtocolErrorKind::InvalidHeader));
        }
        let line =
            std::str::from_utf8(line).map_err(|_| error(ProtocolErrorKind::InvalidHeader))?;
        let (name, value) = line
            .split_once(':')
            .ok_or(error(ProtocolErrorKind::InvalidHeader))?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(error(ProtocolErrorKind::InvalidHeader));
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(error(ProtocolErrorKind::DuplicateLength));
            }
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(error(ProtocolErrorKind::InvalidLength));
            }
            let mut n = 0usize;
            for byte in value.bytes() {
                n = n
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(usize::from(byte - b'0')))
                    .ok_or(error(ProtocolErrorKind::InvalidLength))?;
                bound(n, limits.max_body_bytes, ProtocolErrorKind::BodyLimit)?;
            }
            length = Some(n);
        } else if name.eq_ignore_ascii_case("content-type") {
            if typed {
                return Err(error(ProtocolErrorKind::InvalidHeader));
            }
            content_type(value)?;
            typed = true;
        } else if name.eq_ignore_ascii_case("content-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err(error(ProtocolErrorKind::UnsupportedEncoding));
        }
    }
    let length = length.ok_or(error(ProtocolErrorKind::MissingLength))?;
    let mut body = Vec::new();
    body.try_reserve_exact(length)
        .map_err(|_| error(ProtocolErrorKind::Allocation))?;
    body.resize(length, 0);
    reader.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            error(ProtocolErrorKind::Truncated)
        } else {
            e.into()
        }
    })?;
    Ok(Some(body))
}
fn indexed_error(e: vesper_serde::Error) -> ProtocolError {
    error(match e.kind {
        vesper_serde::ErrorKind::DuplicateKey => ProtocolErrorKind::DuplicateJsonKey,
        vesper_serde::ErrorKind::Utf8 => ProtocolErrorKind::Utf8,
        vesper_serde::ErrorKind::InputLimit
        | vesper_serde::ErrorKind::DepthLimit
        | vesper_serde::ErrorKind::TokenLimit
        | vesper_serde::ErrorKind::StringLimit
        | vesper_serde::ErrorKind::WorkLimit
        | vesper_serde::ErrorKind::AllocationLimit => ProtocolErrorKind::JsonLimit,
        vesper_serde::ErrorKind::AllocationFailed => ProtocolErrorKind::Allocation,
        _ => ProtocolErrorKind::Json,
    })
}
fn get<'d, 'a>(
    root: Node<'d, 'a>,
    name: &str,
    limits: ProtocolLimits,
) -> Result<Option<Node<'d, 'a>>> {
    root.field(name, limits.max_json_work)
        .map_err(indexed_error)
}
fn node_id(node: Node<'_, '_>, nullable: bool, limits: ProtocolLimits) -> Result<()> {
    match node.kind() {
        Kind::Null if nullable => Ok(()),
        Kind::Number => {
            node.raw()
                .parse::<i32>()
                .map_err(|_| error(ProtocolErrorKind::InvalidId))?;
            Ok(())
        }
        Kind::String => bound(
            node.string().map_err(indexed_error)?.decoded_len(),
            limits.max_id_bytes,
            ProtocolErrorKind::InvalidId,
        ),
        _ => Err(error(ProtocolErrorKind::InvalidId)),
    }
}
fn preflight_envelope(index: &IndexedJson<'_>, limits: ProtocolLimits) -> Result<()> {
    let root = index.root();
    if root.kind() != Kind::Object {
        return Err(error(ProtocolErrorKind::InvalidEnvelope));
    }
    let version = get(root, "jsonrpc", limits)?.ok_or(error(ProtocolErrorKind::InvalidEnvelope))?;
    if version.kind() != Kind::String || !version.string().map_err(indexed_error)?.equals("2.0") {
        return Err(error(ProtocolErrorKind::InvalidEnvelope));
    }
    for (name, _) in root.fields().map_err(indexed_error)? {
        if !["jsonrpc", "id", "method", "params", "result", "error"]
            .iter()
            .any(|s| name.equals(s))
        {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
    }
    let id = get(root, "id", limits)?;
    let method = get(root, "method", limits)?;
    let params = get(root, "params", limits)?;
    let result = get(root, "result", limits)?;
    let rpc_error = get(root, "error", limits)?;
    if let Some(method) = method {
        if method.kind() != Kind::String {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
        let len = method.string().map_err(indexed_error)?.decoded_len();
        if len == 0 || len > limits.max_method_bytes {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
        if result.is_some() || rpc_error.is_some() {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
        if let Some(id) = id {
            node_id(id, false, limits)?;
        }
        if params.is_some_and(|p| !matches!(p.kind(), Kind::Object | Kind::Array)) {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
    } else {
        if params.is_some() || result.is_some() == rpc_error.is_some() {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        }
        node_id(id.ok_or(error(ProtocolErrorKind::InvalidId))?, true, limits)?;
        if let Some(rpc_error) = rpc_error {
            if rpc_error.kind() != Kind::Object {
                return Err(error(ProtocolErrorKind::InvalidEnvelope));
            }
            for (name, _) in rpc_error.fields().map_err(indexed_error)? {
                if !["code", "message", "data"].iter().any(|s| name.equals(s)) {
                    return Err(error(ProtocolErrorKind::InvalidEnvelope));
                }
            }
            let code =
                get(rpc_error, "code", limits)?.ok_or(error(ProtocolErrorKind::InvalidEnvelope))?;
            if code.kind() != Kind::Number || code.raw().parse::<i32>().is_err() {
                return Err(error(ProtocolErrorKind::InvalidEnvelope));
            }
            if get(rpc_error, "message", limits)?.is_none_or(|n| n.kind() != Kind::String) {
                return Err(error(ProtocolErrorKind::InvalidEnvelope));
            }
        }
    }
    Ok(())
}
fn take_id(value: Value) -> Result<Id> {
    match value {
        Value::String(s) => Ok(Id::String(s)),
        Value::Number(n) => n
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(Id::Integer)
            .ok_or(error(ProtocolErrorKind::InvalidId)),
        _ => Err(error(ProtocolErrorKind::InvalidId)),
    }
}
/// Validates decoded-equivalent duplicate keys everywhere BEFORE serde_json can
/// materialize/overwrite an object. The accepted envelope is an explicit strict
/// JSON-RPC2/LSP integer-ID profile; batches, extensions and null request IDs fail.
pub fn parse_envelope(bytes: &[u8], limits: ProtocolLimits) -> Result<Envelope> {
    limits.validate()?;
    bound(
        bytes.len(),
        limits.max_body_bytes,
        ProtocolErrorKind::BodyLimit,
    )?;
    let index = IndexedJson::parse(
        bytes,
        vesper_serde::Limits {
            max_input_bytes: limits.max_body_bytes,
            max_depth: limits.max_json_depth,
            max_tokens: limits.max_json_nodes,
            max_string_bytes: limits.max_json_string_bytes,
            max_total_string_bytes: limits.max_json_total_string_bytes,
            max_work: limits.max_json_work,
            max_index_bytes: limits.max_json_index_bytes,
        },
    )
    .map_err(indexed_error)?;
    preflight_envelope(&index, limits)?;
    drop(index);
    let value: Value = serde_json::from_slice(bytes).map_err(|_| error(ProtocolErrorKind::Json))?;
    let Value::Object(mut map) = value else {
        return Err(error(ProtocolErrorKind::InvalidEnvelope));
    };
    if let Some(Value::String(method)) = map.remove("method") {
        let params = map.remove("params");
        return Ok(if let Some(id) = map.remove("id") {
            Envelope::Request {
                id: take_id(id)?,
                method,
                params,
            }
        } else {
            Envelope::Notification { method, params }
        });
    }
    let id = match map.remove("id") {
        Some(Value::Null) => None,
        Some(value) => Some(take_id(value)?),
        None => return Err(error(ProtocolErrorKind::InvalidId)),
    };
    let payload = if let Some(result) = map.remove("result") {
        ResponsePayload::Result(result)
    } else {
        let Some(Value::Object(mut err)) = map.remove("error") else {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        };
        let code = err
            .remove("code")
            .and_then(|v| v.as_i64())
            .and_then(|n| i32::try_from(n).ok())
            .ok_or(error(ProtocolErrorKind::InvalidEnvelope))?;
        let Some(Value::String(message)) = err.remove("message") else {
            return Err(error(ProtocolErrorKind::InvalidEnvelope));
        };
        ResponsePayload::Error {
            code,
            message,
            data: err.remove("data"),
        }
    };
    Ok(Envelope::Response { id, payload })
}
pub fn read_message<R: BufRead>(
    reader: &mut R,
    limits: ProtocolLimits,
) -> Result<Option<Envelope>> {
    read_frame(reader, limits)?
        .map(|body| parse_envelope(&body, limits))
        .transpose()
}

struct JsonBudget {
    limits: ProtocolLimits,
    nodes: usize,
    strings: usize,
    remaining: usize,
}
impl JsonBudget {
    fn work(&mut self, n: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or(error(ProtocolErrorKind::WorkLimit))?;
        Ok(())
    }
    fn node(&mut self, _depth: usize) -> Result<()> {
        self.nodes = add(self.nodes, 1)?;
        bound(
            self.nodes,
            self.limits.max_json_nodes,
            ProtocolErrorKind::JsonLimit,
        )?;
        self.work(1)
    }
    fn text(&mut self, s: &str) -> Result<()> {
        bound(
            s.len(),
            self.limits.max_json_string_bytes,
            ProtocolErrorKind::JsonLimit,
        )?;
        self.strings = add(self.strings, s.len())?;
        bound(
            self.strings,
            self.limits.max_json_total_string_bytes,
            ProtocolErrorKind::JsonLimit,
        )?;
        self.work(s.len())
    }
    fn key(&mut self, s: &str, depth: usize) -> Result<()> {
        self.node(depth)?;
        self.text(s)
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<()> {
        bound(
            add(self.nodes, 1)?,
            self.limits.max_json_nodes,
            ProtocolErrorKind::JsonLimit,
        )?;
        bound(1, self.remaining, ProtocolErrorKind::WorkLimit)?;
        let mut stack = Vec::new();
        stack
            .try_reserve(1)
            .map_err(|_| error(ProtocolErrorKind::Allocation))?;
        stack.push((value, depth));
        while let Some((value, depth)) = stack.pop() {
            self.node(depth)?;
            if matches!(value, Value::Array(_) | Value::Object(_)) {
                bound(
                    depth,
                    self.limits.max_json_depth,
                    ProtocolErrorKind::JsonLimit,
                )?;
            }
            match value {
                Value::String(s) => self.text(s)?,
                Value::Array(values) => {
                    bound(
                        add(self.nodes, add(stack.len(), values.len())?)?,
                        self.limits.max_json_nodes,
                        ProtocolErrorKind::JsonLimit,
                    )?;
                    self.work(values.len())?;
                    stack
                        .try_reserve(values.len())
                        .map_err(|_| error(ProtocolErrorKind::Allocation))?;
                    stack.extend(values.iter().map(|v| (v, depth + 1)));
                }
                Value::Object(values) => {
                    bound(
                        add(
                            self.nodes,
                            add(
                                stack.len(),
                                values
                                    .len()
                                    .checked_mul(2)
                                    .ok_or(error(ProtocolErrorKind::JsonLimit))?,
                            )?,
                        )?,
                        self.limits.max_json_nodes,
                        ProtocolErrorKind::JsonLimit,
                    )?;
                    self.work(values.len())?;
                    stack
                        .try_reserve(values.len())
                        .map_err(|_| error(ProtocolErrorKind::Allocation))?;
                    for (k, v) in values {
                        self.key(k, depth + 1)?;
                        stack.push((v, depth + 1));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn id(&mut self, id: &Option<&Id>) -> Result<()> {
        self.node(2)?;
        if let Some(Id::String(s)) = id {
            bound(
                s.len(),
                self.limits.max_id_bytes,
                ProtocolErrorKind::InvalidId,
            )?;
            self.text(s)?;
        }
        Ok(())
    }
}
fn validate_output(envelope: &Envelope, limits: ProtocolLimits) -> Result<JsonBudget> {
    limits.validate()?;
    bound(1, limits.max_json_depth, ProtocolErrorKind::JsonLimit)?;
    let mut b = JsonBudget {
        limits,
        nodes: 0,
        strings: 0,
        remaining: limits.max_json_work,
    };
    b.node(1)?;
    b.key("jsonrpc", 2)?;
    b.node(2)?;
    b.text("2.0")?;
    match envelope {
        Envelope::Request { id, method, params } => {
            b.key("id", 2)?;
            b.id(&Some(id))?;
            check_method(method, limits)?;
            b.key("method", 2)?;
            b.node(2)?;
            b.text(method)?;
            if let Some(params) = params {
                if !matches!(params, Value::Object(_) | Value::Array(_)) {
                    return Err(error(ProtocolErrorKind::InvalidEnvelope));
                }
                b.key("params", 2)?;
                b.value(params, 2)?;
            }
        }
        Envelope::Notification { method, params } => {
            check_method(method, limits)?;
            b.key("method", 2)?;
            b.node(2)?;
            b.text(method)?;
            if let Some(params) = params {
                if !matches!(params, Value::Object(_) | Value::Array(_)) {
                    return Err(error(ProtocolErrorKind::InvalidEnvelope));
                }
                b.key("params", 2)?;
                b.value(params, 2)?;
            }
        }
        Envelope::Response { id, payload } => {
            b.key("id", 2)?;
            b.id(&id.as_ref())?;
            match payload {
                ResponsePayload::Result(result) => {
                    b.key("result", 2)?;
                    b.value(result, 2)?;
                }
                ResponsePayload::Error { message, data, .. } => {
                    b.key("error", 2)?;
                    bound(2, limits.max_json_depth, ProtocolErrorKind::JsonLimit)?;
                    b.node(2)?;
                    b.key("code", 3)?;
                    b.node(3)?;
                    b.key("message", 3)?;
                    b.node(3)?;
                    b.text(message)?;
                    if let Some(data) = data {
                        b.key("data", 3)?;
                        b.value(data, 3)?;
                    }
                }
            }
        }
    }
    Ok(b)
}
fn check_method(method: &str, limits: ProtocolLimits) -> Result<()> {
    if method.is_empty() || method.len() > limits.max_method_bytes {
        Err(error(ProtocolErrorKind::InvalidEnvelope))
    } else {
        Ok(())
    }
}
struct Output {
    bytes: Vec<u8>,
    budget: JsonBudget,
    failure: Option<ProtocolErrorKind>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.bytes.len().checked_add(bytes.len());
        let failure = if next.is_none_or(|n| n > self.budget.limits.max_reply_bytes) {
            Some(ProtocolErrorKind::OutputLimit)
        } else if self.budget.work(bytes.len()).is_err() {
            Some(ProtocolErrorKind::WorkLimit)
        } else if self.bytes.try_reserve(bytes.len()).is_err() {
            Some(ProtocolErrorKind::Allocation)
        } else {
            None
        };
        if let Some(kind) = failure {
            self.failure = Some(kind);
            return Err(io::Error::other("bounded LSP output"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Full validation/serialization completes before external output. I/O errors in
/// write_message may still leave partial stream bytes, so terminate that stream.
pub fn encode_frame(envelope: &Envelope, limits: ProtocolLimits) -> Result<Vec<u8>> {
    let budget = validate_output(envelope, limits)?;
    let mut output = Output {
        bytes: Vec::new(),
        budget,
        failure: None,
    };
    if serde_json::to_writer(&mut output, envelope).is_err() {
        return Err(error(output.failure.unwrap_or(ProtocolErrorKind::Json)));
    }
    let digits = output
        .bytes
        .len()
        .checked_ilog10()
        .map_or(1, |n| n as usize + 1);
    let header_bytes = 20 + digits; // literal prefix16 + CRLFCRLF4
    bound(
        header_bytes,
        limits.max_header_bytes,
        ProtocolErrorKind::HeaderLimit,
    )?;
    bound(
        header_bytes - 2,
        limits.max_header_line_bytes,
        ProtocolErrorKind::HeaderLimit,
    )?;
    bound(1, limits.max_headers, ProtocolErrorKind::HeaderLimit)?;
    let header = format!("Content-Length: {}\r\n\r\n", output.bytes.len());
    let total = add(header_bytes, output.bytes.len())?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(total)
        .map_err(|_| error(ProtocolErrorKind::Allocation))?;
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&output.bytes);
    Ok(frame)
}
pub fn write_message<W: Write>(
    writer: &mut W,
    envelope: &Envelope,
    limits: ProtocolLimits,
) -> Result<()> {
    let frame = encode_frame(envelope, limits)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}
