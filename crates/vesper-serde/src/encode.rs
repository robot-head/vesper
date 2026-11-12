use crate::{Error, ErrorKind, Kind, Node, add};

/// Supported host encoding slice. Numbers are exact integer values; floating
/// point conversion, schema derives, streams and binary archives are deferred.
/// Raw nodes can only come from a fully validated IndexedJson.
#[derive(Clone, Copy, Debug)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    String(&'a str),
    Array(&'a [Value<'a>]),
    Object(&'a [(&'a str, Value<'a>)]),
    Raw(Node<'a, 'a>),
}

#[derive(Clone, Copy, Debug)]
pub struct EncodeLimits {
    pub max_output_bytes: usize,
    pub max_depth: usize,
    pub max_tokens: usize,
    pub max_string_bytes: usize,
    pub max_total_string_bytes: usize,
    pub max_work: usize,
}
impl Default for EncodeLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: 1024 * 1024,
            max_depth: 64,
            max_tokens: 65_536,
            max_string_bytes: 256 * 1024,
            max_total_string_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}

/// Checks all values, duplicate keys, limits and arithmetic without allocation.
/// The deterministic work allowance covers both preflight and writing.
pub fn encoded_size(value: &Value<'_>, limits: EncodeLimits) -> Result<usize, Error> {
    let mut check = Check {
        limits,
        work: 0,
        tokens: 0,
        strings: 0,
    };
    let size = check.value(value, 0)?;
    check.charge(size)?; // One additional unit per byte covers the write pass.
    if size > limits.max_output_bytes {
        return Err(err(ErrorKind::OutputLimit));
    }
    Ok(size)
}

/// Performs one capacity check after complete preflight. Every error leaves all
/// output bytes unchanged, and the borrowed input value remains owned by caller.
/// Bytes beyond the returned encoded length remain unchanged on success too.
pub fn encode_into(
    value: &Value<'_>,
    output: &mut [u8],
    limits: EncodeLimits,
) -> Result<usize, Error> {
    let size = encoded_size(value, limits)?;
    if output.len() < size {
        return Err(err(ErrorKind::OutputTooSmall));
    }
    let mut writer = Writer {
        out: &mut output[..size],
        pos: 0,
    };
    writer.value(value);
    debug_assert_eq!(writer.pos, size);
    Ok(size)
}
fn err(kind: ErrorKind) -> Error {
    Error::new(kind, 0)
}
struct Check {
    limits: EncodeLimits,
    work: usize,
    tokens: usize,
    strings: usize,
}
impl Check {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.work = add(self.work, n)?;
        if self.work > self.limits.max_work {
            return Err(err(ErrorKind::WorkLimit));
        }
        Ok(())
    }
    fn token(&mut self) -> Result<(), Error> {
        self.charge(1)?;
        self.tokens = add(self.tokens, 1)?;
        if self.tokens > self.limits.max_tokens {
            return Err(err(ErrorKind::TokenLimit));
        }
        Ok(())
    }
    fn string_bytes(&mut self, n: usize) -> Result<(), Error> {
        if n > self.limits.max_string_bytes {
            return Err(err(ErrorKind::StringLimit));
        }
        self.strings = add(self.strings, n)?;
        if self.strings > self.limits.max_total_string_bytes {
            return Err(err(ErrorKind::StringLimit));
        }
        Ok(())
    }
    fn string(&mut self, text: &str) -> Result<usize, Error> {
        self.string_bytes(text.len())?;
        self.charge(text.len())?;
        let mut size = 2;
        for b in text.bytes() {
            size = add(
                size,
                match b {
                    b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
                    0..=31 => 6,
                    _ => 1,
                },
            )?;
        }
        Ok(size)
    }
    fn depth(&self, depth: usize) -> Result<(), Error> {
        if depth >= self.limits.max_depth || depth >= 128 {
            return Err(err(ErrorKind::DepthLimit));
        }
        Ok(())
    }
    fn value(&mut self, value: &Value<'_>, depth: usize) -> Result<usize, Error> {
        if let Value::Raw(node) = value {
            self.charge(node.raw().len())?;
            for (kind, relative_depth, bytes) in node.stats() {
                self.token()?;
                if matches!(kind, Kind::Array | Kind::Object) {
                    self.depth(add(depth, relative_depth)?)?;
                }
                if kind == Kind::String {
                    self.string_bytes(bytes)?;
                }
            }
            return Ok(node.raw().len());
        }
        self.token()?;
        Ok(match value {
            Value::Null => 4,
            Value::Bool(b) => {
                if *b {
                    4
                } else {
                    5
                }
            }
            Value::U64(n) => digits(*n),
            Value::I64(n) => add(usize::from(*n < 0), digits(n.unsigned_abs()))?,
            Value::String(text) => self.string(text)?,
            Value::Array(values) => {
                self.depth(depth)?;
                let mut size = 2;
                for (i, value) in values.iter().enumerate() {
                    size = add(size, usize::from(i != 0))?;
                    size = add(size, self.value(value, depth + 1)?)?;
                }
                size
            }
            Value::Object(fields) => {
                self.depth(depth)?;
                let mut size = 2;
                for (i, (key, value)) in fields.iter().enumerate() {
                    self.token()?; // Object key is itself a string token.
                    for (previous, _) in &fields[..i] {
                        self.charge(add(1, previous.len().min(key.len()))?)?;
                        if previous == key {
                            return Err(err(ErrorKind::DuplicateKey));
                        }
                    }
                    size = add(size, usize::from(i != 0))?;
                    size = add(size, add(self.string(key)?, 1)?)?;
                    size = add(size, self.value(value, depth + 1)?)?;
                }
                size
            }
            Value::Raw(_) => unreachable!("handled before token accounting"),
        })
    }
}
fn digits(mut n: u64) -> usize {
    let mut digits = 1;
    while n >= 10 {
        digits += 1;
        n /= 10;
    }
    digits
}
struct Writer<'a> {
    out: &'a mut [u8],
    pos: usize,
}
impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        self.out[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
    }
    fn byte(&mut self, byte: u8) {
        self.out[self.pos] = byte;
        self.pos += 1;
    }
    fn integer(&mut self, mut n: u64) {
        let mut buffer = [0; 20];
        let mut i = 20;
        loop {
            i -= 1;
            buffer[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.bytes(&buffer[i..]);
    }
    fn string(&mut self, text: &str) {
        self.byte(b'"');
        for b in text.bytes() {
            match b {
                b'"' => self.bytes(b"\\\""),
                b'\\' => self.bytes(b"\\\\"),
                b'\n' => self.bytes(b"\\n"),
                b'\r' => self.bytes(b"\\r"),
                b'\t' => self.bytes(b"\\t"),
                8 => self.bytes(b"\\b"),
                12 => self.bytes(b"\\f"),
                0..=31 => {
                    self.bytes(b"\\u00");
                    self.byte(b"0123456789abcdef"[usize::from(b >> 4)]);
                    self.byte(b"0123456789abcdef"[usize::from(b & 15)]);
                }
                _ => self.byte(b),
            }
        }
        self.byte(b'"');
    }
    fn value(&mut self, value: &Value<'_>) {
        match value {
            Value::Null => self.bytes(b"null"),
            Value::Bool(b) => self.bytes(if *b { b"true" } else { b"false" }),
            Value::U64(n) => self.integer(*n),
            Value::I64(n) => {
                if *n < 0 {
                    self.byte(b'-');
                }
                self.integer(n.unsigned_abs());
            }
            Value::String(s) => self.string(s),
            Value::Raw(node) => self.bytes(node.raw().as_bytes()),
            Value::Array(values) => {
                self.byte(b'[');
                for (i, value) in values.iter().enumerate() {
                    if i != 0 {
                        self.byte(b',');
                    }
                    self.value(value);
                }
                self.byte(b']');
            }
            Value::Object(fields) => {
                self.byte(b'{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i != 0 {
                        self.byte(b',');
                    }
                    self.string(key);
                    self.byte(b':');
                    self.value(value);
                }
                self.byte(b'}');
            }
        }
    }
}
