//! The frozen Rust safetensors header reader (bug fixes only until
//! self-hosting Stage 2). The package vesper-tensor reads headers with the
//! Vesper port (vesper/src/text.ves); this seed copy, which the compiler
//! links, keeps the Rust reader. Moved here from src/json.rs by SH6 lane 11
//! (#1406).
use crate::json::{Value, BAD, CONTROL_END, MAX_DEPTH};
use crate::Error;

pub(crate) fn parse(bytes: &[u8]) -> Result<Value, Error> {
    let mut p = Parser { bytes, at: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.at != bytes.len() {
        return Err(Error::Malformed("trailing bytes after JSON header"));
    }
    Ok(v)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}
/// UTF-16 surrogate halves (Unicode §3.8); a `\u` escape pair encodes one
/// supplementary-plane code point (RFC 8259 §7).
const HIGH_SURROGATES: std::ops::Range<u32> = 0xd800..0xdc00;
const LOW_SURROGATES: std::ops::Range<u32> = 0xdc00..0xe000;
/// The first code point outside the Basic Multilingual Plane.
const SUPPLEMENTARY_BASE: u32 = 0x1_0000;

impl Parser<'_> {
    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.at) {
            self.at += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn eat(&mut self, b: u8) -> Result<(), Error> {
        if self.peek() == Some(b) {
            self.at += 1;
            Ok(())
        } else {
            Err(BAD)
        }
    }
    fn literal(&mut self, word: &[u8]) -> Result<(), Error> {
        if self.bytes.get(self.at..self.at + word.len()) == Some(word) {
            self.at += word.len();
            Ok(())
        } else {
            Err(BAD)
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::Limit("JSON nesting"));
        }
        match self.peek().ok_or(BAD)? {
            b'{' => {
                self.at += 1;
                let mut fields: Vec<(String, Value)> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Value::Object(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    if !seen.insert(key.clone()) {
                        return Err(Error::Malformed("duplicate JSON key"));
                    }
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    fields.push((key, v));
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Object(fields));
                        }
                        _ => return Err(BAD),
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Array(items));
                        }
                        _ => return Err(BAD),
                    }
                }
            }
            b'"' => Ok(Value::String(self.string()?)),
            b't' => self.literal(b"true").map(|_| Value::Bool(true)),
            b'f' => self.literal(b"false").map(|_| Value::Bool(false)),
            b'n' => self.literal(b"null").map(|_| Value::Null),
            b'0'..=b'9' => self.int(),
            b'-' => Err(Error::Unsupported("negative JSON number".into())),
            _ => Err(BAD),
        }
    }
    fn int(&mut self) -> Result<Value, Error> {
        let start = self.at;
        while let Some(b'0'..=b'9') = self.peek() {
            self.at += 1;
        }
        let digits = &self.bytes[start..self.at];
        if digits.len() > 1 && digits[0] == b'0' {
            return Err(Error::Malformed("JSON number with a leading zero"));
        }
        if let Some(b'.' | b'e' | b'E') = self.peek() {
            return Err(Error::Unsupported("non-integer JSON number".into()));
        }
        let mut v: u64 = 0;
        for d in digits {
            v = v
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(d - b'0')))
                .ok_or(Error::Overflow)?;
        }
        Ok(Value::Int(v))
    }
    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.bytes.get(self.at..self.at + 4).ok_or(BAD)?;
        let s = std::str::from_utf8(digits).map_err(|_| BAD)?;
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(BAD);
        }
        self.at += 4;
        u32::from_str_radix(s, 16).map_err(|_| BAD)
    }
    fn string(&mut self) -> Result<String, Error> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            let b = self.peek().ok_or(BAD)?;
            self.at += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = self.peek().ok_or(BAD)?;
                    self.at += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if HIGH_SURROGATES.contains(&hi) {
                                self.literal(b"\\u")?;
                                let lo = self.hex4()?;
                                if !LOW_SURROGATES.contains(&lo) {
                                    return Err(Error::Malformed("unpaired JSON surrogate"));
                                }
                                SUPPLEMENTARY_BASE
                                    + ((hi - HIGH_SURROGATES.start) << 10)
                                    + (lo - LOW_SURROGATES.start)
                            } else if LOW_SURROGATES.contains(&hi) {
                                return Err(Error::Malformed("unpaired JSON surrogate"));
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or(BAD)?
                        }
                        _ => return Err(BAD),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                0x00..CONTROL_END => {
                    return Err(Error::Malformed("control character in JSON string"))
                }
                _ => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| Error::Malformed("JSON string is not UTF-8"))
    }
}
