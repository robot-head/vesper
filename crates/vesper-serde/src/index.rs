use crate::{Error, ErrorKind, FloatLimits};

pub(crate) const HARD_DEPTH: usize = 128;

/// Parse limits also bound the single token-vector allocation. Recursive descent
/// has an additional hard ceiling of 128 containers, even if max_depth is larger.
/// Work units cover UTF-8 scanning, consumed bytes and duplicate-key comparisons;
/// they are a deterministic resource bound, not elapsed-time measurements.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_depth: usize,
    pub max_tokens: usize,
    pub max_string_bytes: usize,
    pub max_total_string_bytes: usize,
    pub max_work: usize,
    pub max_index_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_depth: 64,
            max_tokens: 65_536,
            max_string_bytes: 256 * 1024,
            max_total_string_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
            max_index_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) next: usize,
    pub(crate) kind: Kind,
    pub(crate) decoded_len: usize,
    pub(crate) escaped: bool,
    pub(crate) depth: usize,
}

/// Validated immutable JSON with a bounded scalar structural index. Construction
/// allocates one Vec, with requested capacity bounded by all three of input
/// length, token count and index-byte allowance. No strings or DOM are allocated.
#[derive(Debug)]
pub struct IndexedJson<'a> {
    /// Entirely valid UTF-8: checked by `core::str` (scalar parser) or by the
    /// stage 1 kernel (SIMD parser). Spans are converted to `&str` on access.
    pub(crate) input: &'a [u8],
    pub(crate) tokens: Vec<Token>,
    pub(crate) work_used: usize,
}
impl<'a> IndexedJson<'a> {
    pub fn parse(input: &'a [u8], limits: Limits) -> Result<Self, Error> {
        if input.len() > limits.max_input_bytes {
            return Err(Error::new(ErrorKind::InputLimit, 0));
        }
        if input.len() > limits.max_work {
            return Err(Error::new(ErrorKind::WorkLimit, 0));
        }
        let text = core::str::from_utf8(input)
            .map_err(|e| Error::new(ErrorKind::Utf8, e.valid_up_to()))?;
        let capacity = input
            .len()
            .min(limits.max_tokens)
            .min(limits.max_index_bytes / size_of::<Token>());
        let mut tokens = Vec::new();
        tokens
            .try_reserve_exact(capacity)
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        let mut parser = Parser {
            text,
            pos: 0,
            tokens,
            capacity,
            limits,
            work: input.len(),
            string_bytes: 0,
        };
        parser.ws()?;
        parser.value(0)?;
        parser.ws()?;
        if parser.pos != input.len() {
            return Err(parser.err(ErrorKind::Syntax));
        }
        Ok(Self {
            input,
            tokens: parser.tokens,
            work_used: parser.work,
        })
    }
    pub fn root(&self) -> Node<'_, 'a> {
        Node {
            doc: self,
            index: 0,
        }
    }
    pub fn cursor(&self) -> Cursor<'_, 'a> {
        Cursor {
            doc: self,
            index: 0,
            end: self.tokens.len(),
        }
    }
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }
    pub fn index_capacity_bytes(&self) -> usize {
        self.tokens.capacity() * size_of::<Token>()
    }
    pub fn work_used(&self) -> usize {
        self.work_used
    }
    /// Whether two documents have identical token tapes and work accounting,
    /// as the scalar and SIMD parsers must on every accepted input.
    pub fn same_tape(&self, other: &IndexedJson<'_>) -> bool {
        self.input == other.input
            && self.tokens == other.tokens
            && self.work_used == other.work_used
    }
    pub(crate) fn span(&self, start: usize, end: usize) -> &'a str {
        core::str::from_utf8(&self.input[start..end]).expect("validated UTF-8 at token boundaries")
    }
}

/// A syntax-validated subtree, suitable for raw forwarding. Cannot be forged
/// from unvalidated bytes. Copying the view keeps input and index borrowed.
#[derive(Clone, Copy, Debug)]
pub struct Node<'d, 'a> {
    doc: &'d IndexedJson<'a>,
    index: usize,
}
impl<'d, 'a> Node<'d, 'a> {
    fn token(self) -> Token {
        self.doc.tokens[self.index]
    }
    pub fn kind(self) -> Kind {
        self.token().kind
    }
    pub(crate) fn stats(self) -> impl Iterator<Item = (Kind, usize, usize)> + 'd {
        let root = self.token();
        self.doc.tokens[self.index..root.next]
            .iter()
            .map(move |t| (t.kind, t.depth - root.depth, t.decoded_len))
    }
    pub fn raw(self) -> &'a str {
        let t = self.token();
        self.doc.span(t.start, t.end)
    }
    pub fn string(self) -> Result<JsonString<'a>, Error> {
        let t = self.token();
        if t.kind != Kind::String {
            return Err(Error::new(ErrorKind::Type, t.start));
        }
        Ok(JsonString {
            raw: self.doc.span(t.start + 1, t.end - 1),
            decoded_len: t.decoded_len,
            escaped: t.escaped,
        })
    }
    pub fn as_u64(self) -> Result<u64, Error> {
        if self.kind() != Kind::Number {
            return Err(Error::new(ErrorKind::Type, self.token().start));
        }
        self.raw()
            .parse()
            .map_err(|_| Error::new(ErrorKind::Number, self.token().start))
    }
    pub fn as_i64(self) -> Result<i64, Error> {
        if self.kind() != Kind::Number {
            return Err(Error::new(ErrorKind::Type, self.token().start));
        }
        self.raw()
            .parse()
            .map_err(|_| Error::new(ErrorKind::Number, self.token().start))
    }
    /// Convert an already validated JSON number to finite scalar binary64.
    ///
    /// Conversion may round, including underflow to signed zero. The original
    /// spelling remains available through `raw`; overflow to infinity fails.
    /// Byte and logical-work admission happen before Rust's scalar conversion.
    pub fn as_f64(self, limits: FloatLimits) -> Result<f64, Error> {
        let token = self.token();
        if token.kind != Kind::Number {
            return Err(Error::new(ErrorKind::Type, token.start));
        }
        let raw = self.raw();
        limits
            .preflight(raw.len())
            .map_err(|e| Error::new(e.kind, token.start))?;
        let value = raw
            .parse::<f64>()
            .map_err(|_| Error::new(ErrorKind::Number, token.start))?;
        if !value.is_finite() {
            return Err(Error::new(ErrorKind::Number, token.start));
        }
        Ok(value)
    }
    pub fn as_bool(self) -> Result<bool, Error> {
        if self.kind() != Kind::Bool {
            return Err(Error::new(ErrorKind::Type, self.token().start));
        }
        Ok(self.raw() == "true")
    }
    pub fn fields(self) -> Result<ObjectFields<'d, 'a>, Error> {
        if self.kind() != Kind::Object {
            return Err(Error::new(ErrorKind::Type, self.token().start));
        }
        Ok(ObjectFields {
            doc: self.doc,
            index: self.index + 1,
            end: self.token().next,
        })
    }
    /// Array traversal needs no parsing and makes no additional allocation.
    pub fn elements(self) -> Result<Cursor<'d, 'a>, Error> {
        if self.kind() != Kind::Array {
            return Err(Error::new(ErrorKind::Type, self.token().start));
        }
        Ok(Cursor {
            doc: self.doc,
            index: self.index + 1,
            end: self.token().next,
        })
    }
    /// Lookup has an explicit independent work allowance. Keys are compared in
    /// decoded Unicode form, with no allocation. Parsing already ruled out duplicates.
    pub fn field(self, key: &str, max_work: usize) -> Result<Option<Node<'d, 'a>>, Error> {
        let mut work = max_work;
        for (name, value) in self.fields()? {
            let mut a = name.chars();
            let mut b = key.chars();
            loop {
                work = work
                    .checked_sub(24)
                    .ok_or(Error::new(ErrorKind::WorkLimit, self.token().start))?;
                match (a.next(), b.next()) {
                    (None, None) => return Ok(Some(value)),
                    (Some(x), Some(y)) if x == y => {}
                    _ => break,
                }
            }
        }
        Ok(None)
    }
}

pub struct ObjectFields<'d, 'a> {
    doc: &'d IndexedJson<'a>,
    index: usize,
    end: usize,
}
impl<'d, 'a> Iterator for ObjectFields<'d, 'a> {
    type Item = (JsonString<'a>, Node<'d, 'a>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.end {
            return None;
        }
        let name = Node {
            doc: self.doc,
            index: self.index,
        }
        .string()
        .expect("validated object key");
        let value = Node {
            doc: self.doc,
            index: self.index + 1,
        };
        self.index = value.token().next;
        Some((name, value))
    }
}

/// A transactional tape cursor. The root cursor contains one value; array
/// cursors are scoped to that array's children. Failed reads never advance.
#[derive(Clone, Debug)]
pub struct Cursor<'d, 'a> {
    doc: &'d IndexedJson<'a>,
    index: usize,
    end: usize,
}
impl<'d, 'a> Cursor<'d, 'a> {
    pub fn position(&self) -> usize {
        self.index
    }
    pub fn peek(&self) -> Option<Node<'d, 'a>> {
        if self.index >= self.end {
            return None;
        }
        self.doc.tokens.get(self.index).map(|_| Node {
            doc: self.doc,
            index: self.index,
        })
    }
    pub fn read_raw(&mut self) -> Result<Node<'d, 'a>, Error> {
        let node = self
            .peek()
            .ok_or(Error::new(ErrorKind::End, self.doc.input.len()))?;
        self.index = node.token().next;
        Ok(node)
    }
    pub fn skip(&mut self) -> Result<(), Error> {
        self.read_raw().map(|_| ())
    }
    pub fn read_str<'s>(&mut self, scratch: &'s mut [u8]) -> Result<JsonStr<'a, 's>, Error> {
        let node = self
            .peek()
            .ok_or(Error::new(ErrorKind::End, self.doc.input.len()))?;
        let result = node.string()?.decode(scratch)?;
        self.index = node.token().next;
        Ok(result)
    }
    /// Read a finite binary64 number. Every failure preserves the cursor, so
    /// callers can retry admission or read the same validated value as raw JSON.
    pub fn read_f64(&mut self, limits: FloatLimits) -> Result<f64, Error> {
        let node = self
            .peek()
            .ok_or(Error::new(ErrorKind::End, self.doc.input.len()))?;
        let result = node.as_f64(limits)?;
        self.index = node.token().next;
        Ok(result)
    }
}

/// Validated string content. Raw spelling is available without unescaping.
#[derive(Clone, Copy, Debug)]
pub struct JsonString<'a> {
    raw: &'a str,
    decoded_len: usize,
    escaped: bool,
}
impl<'a> JsonString<'a> {
    pub fn raw_content(self) -> &'a str {
        self.raw
    }
    pub fn decoded_len(self) -> usize {
        self.decoded_len
    }
    pub fn is_escaped(self) -> bool {
        self.escaped
    }
    pub fn equals(self, text: &str) -> bool {
        self.chars().eq(text.chars())
    }
    fn chars(self) -> DecodedChars<'a> {
        DecodedChars {
            raw: self.raw,
            pos: 0,
        }
    }
    /// Preflight ensures insufficient scratch leaves every byte untouched.
    /// An unescaped string needs no scratch, even when it is non-ASCII UTF-8.
    pub fn decode<'s>(self, scratch: &'s mut [u8]) -> Result<JsonStr<'a, 's>, Error> {
        if !self.escaped {
            return Ok(JsonStr::Borrowed(self.raw));
        }
        if scratch.len() < self.decoded_len {
            return Err(Error::new(ErrorKind::ScratchTooSmall, 0));
        }
        let mut pos = 0;
        for ch in self.chars() {
            let mut bytes = [0; 4];
            let n = ch.encode_utf8(&mut bytes).len();
            scratch[pos..pos + n].copy_from_slice(&bytes[..n]);
            pos += n;
        }
        Ok(JsonStr::Scratch(
            core::str::from_utf8(&scratch[..pos]).expect("validated Unicode scalars"),
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum JsonStr<'a, 's> {
    Borrowed(&'a str),
    Scratch(&'s str),
}
impl JsonStr<'_, '_> {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(s) => s,
            Self::Scratch(s) => s,
        }
    }
}

struct DecodedChars<'a> {
    raw: &'a str,
    pos: usize,
}
impl Iterator for DecodedChars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        let ch = self.raw.get(self.pos..)?.chars().next()?;
        self.pos += ch.len_utf8();
        if ch != '\\' {
            return Some(ch);
        }
        Some(escape(self.raw.as_bytes(), &mut self.pos).expect("validated escape"))
    }
}

fn hex4(text: &[u8], pos: &mut usize) -> Result<u32, Error> {
    let mut n = 0;
    for _ in 0..4 {
        let b = *text
            .get(*pos)
            .ok_or(Error::new(ErrorKind::Escape, *pos))?;
        *pos += 1;
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return Err(Error::new(ErrorKind::Escape, *pos - 1)),
        };
        n = n * 16 + u32::from(d);
    }
    Ok(n)
}
pub(crate) fn escape(text: &[u8], pos: &mut usize) -> Result<char, Error> {
    let b = *text
        .get(*pos)
        .ok_or(Error::new(ErrorKind::Escape, *pos))?;
    *pos += 1;
    Ok(match b {
        b'"' => '"',
        b'\\' => '\\',
        b'/' => '/',
        b'b' => '\x08',
        b'f' => '\x0c',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'u' => {
            let hi = hex4(text, pos)?;
            let code = if (0xd800..=0xdbff).contains(&hi) {
                if text.get(*pos..pos.saturating_add(2)) != Some(b"\\u") {
                    return Err(Error::new(ErrorKind::Escape, *pos));
                }
                *pos += 2;
                let lo = hex4(text, pos)?;
                if !(0xdc00..=0xdfff).contains(&lo) {
                    return Err(Error::new(ErrorKind::Escape, *pos - 4));
                }
                0x10000 + ((hi - 0xd800) << 10) + lo - 0xdc00
            } else {
                hi
            };
            char::from_u32(code).ok_or(Error::new(ErrorKind::Escape, *pos))?
        }
        _ => return Err(Error::new(ErrorKind::Escape, *pos - 1)),
    })
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    capacity: usize,
    limits: Limits,
    work: usize,
    string_bytes: usize,
}
impl Parser<'_> {
    fn err(&self, kind: ErrorKind) -> Error {
        Error::new(kind, self.pos)
    }
    fn tick(&mut self, amount: usize) -> Result<(), Error> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or(self.err(ErrorKind::WorkLimit))?;
        if self.work > self.limits.max_work {
            return Err(self.err(ErrorKind::WorkLimit));
        }
        Ok(())
    }
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }
    fn step(&mut self) -> Result<(), Error> {
        self.tick(1)?;
        self.pos += 1;
        Ok(())
    }
    fn ws(&mut self) -> Result<(), Error> {
        while matches!(self.byte(), Some(b' ' | b'\r' | b'\n' | b'\t')) {
            self.step()?;
        }
        Ok(())
    }
    fn expect(&mut self, b: u8) -> Result<(), Error> {
        if self.byte() != Some(b) {
            return Err(self.err(ErrorKind::Syntax));
        }
        self.step()
    }
    fn push(&mut self, kind: Kind, depth: usize) -> Result<usize, Error> {
        if self.tokens.len() >= self.limits.max_tokens {
            return Err(self.err(ErrorKind::TokenLimit));
        }
        if self.tokens.len() >= self.capacity {
            return Err(self.err(ErrorKind::AllocationLimit));
        }
        let index = self.tokens.len();
        self.tokens.push(Token {
            start: self.pos,
            end: 0,
            next: index + 1,
            kind,
            decoded_len: 0,
            escaped: false,
            depth,
        });
        Ok(index)
    }
    fn value(&mut self, depth: usize) -> Result<(), Error> {
        let kind = match self.byte() {
            Some(b'n') => Kind::Null,
            Some(b't' | b'f') => Kind::Bool,
            Some(b'-' | b'0'..=b'9') => Kind::Number,
            Some(b'"') => Kind::String,
            Some(b'[') => Kind::Array,
            Some(b'{') => Kind::Object,
            _ => return Err(self.err(ErrorKind::Syntax)),
        };
        if matches!(kind, Kind::Array | Kind::Object)
            && (depth >= self.limits.max_depth || depth >= HARD_DEPTH)
        {
            return Err(self.err(ErrorKind::DepthLimit));
        }
        let index = self.push(kind, depth)?;
        match kind {
            Kind::Null => self.literal(b"null")?,
            Kind::Bool => {
                if self.byte() == Some(b't') {
                    self.literal(b"true")?;
                } else {
                    self.literal(b"false")?;
                }
            }
            Kind::Number => self.number()?,
            Kind::String => self.string(index)?,
            Kind::Array | Kind::Object => self.container(index, depth, kind == Kind::Object)?,
        }
        self.tokens[index].end = self.pos;
        self.tokens[index].next = self.tokens.len();
        Ok(())
    }
    fn literal(&mut self, literal: &[u8]) -> Result<(), Error> {
        for &b in literal {
            self.expect(b)?;
        }
        Ok(())
    }
    fn number(&mut self) -> Result<(), Error> {
        if self.byte() == Some(b'-') {
            self.step()?;
        }
        match self.byte() {
            Some(b'0') => self.step()?,
            Some(b'1'..=b'9') => {
                self.step()?;
                while matches!(self.byte(), Some(b'0'..=b'9')) {
                    self.step()?;
                }
            }
            _ => return Err(self.err(ErrorKind::Number)),
        }
        if self.byte() == Some(b'.') {
            self.step()?;
            if !matches!(self.byte(), Some(b'0'..=b'9')) {
                return Err(self.err(ErrorKind::Number));
            }
            while matches!(self.byte(), Some(b'0'..=b'9')) {
                self.step()?;
            }
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.step()?;
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.step()?;
            }
            if !matches!(self.byte(), Some(b'0'..=b'9')) {
                return Err(self.err(ErrorKind::Number));
            }
            while matches!(self.byte(), Some(b'0'..=b'9')) {
                self.step()?;
            }
        }
        Ok(())
    }
    fn string(&mut self, index: usize) -> Result<(), Error> {
        self.expect(b'"')?;
        let mut decoded_len: usize = 0;
        let mut escaped = false;
        loop {
            match self.byte() {
                Some(b'"') => {
                    self.step()?;
                    break;
                }
                Some(b'\\') => {
                    escaped = true;
                    self.step()?;
                    let old = self.pos;
                    let ch = escape(self.text.as_bytes(), &mut self.pos)?;
                    self.tick(self.pos - old)?;
                    decoded_len = decoded_len
                        .checked_add(ch.len_utf8())
                        .ok_or(self.err(ErrorKind::StringLimit))?;
                }
                Some(0..=0x1f) | None => return Err(self.err(ErrorKind::Syntax)),
                Some(_) => {
                    self.step()?;
                    decoded_len = decoded_len
                        .checked_add(1)
                        .ok_or(self.err(ErrorKind::StringLimit))?;
                }
            }
            if decoded_len > self.limits.max_string_bytes {
                return Err(self.err(ErrorKind::StringLimit));
            }
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(decoded_len)
            .ok_or(self.err(ErrorKind::StringLimit))?;
        if self.string_bytes > self.limits.max_total_string_bytes {
            return Err(self.err(ErrorKind::StringLimit));
        }
        self.tokens[index].decoded_len = decoded_len;
        self.tokens[index].escaped = escaped;
        Ok(())
    }
    fn duplicate(&mut self, start: usize, current: usize) -> Result<(), Error> {
        let at = self.pos;
        check_duplicate(
            &self.tokens,
            self.text.as_bytes(),
            start,
            current,
            &mut self.work,
            self.limits.max_work,
            at,
        )
    }
    fn container(&mut self, index: usize, depth: usize, object: bool) -> Result<(), Error> {
        self.step()?;
        self.ws()?;
        let close = if object { b'}' } else { b']' };
        if self.byte() == Some(close) {
            return self.step();
        }
        loop {
            if object {
                if self.byte() != Some(b'"') {
                    return Err(self.err(ErrorKind::Syntax));
                }
                let key = self.tokens.len();
                self.value(depth + 1)?;
                self.duplicate(index + 1, key)?;
                self.ws()?;
                self.expect(b':')?;
                self.ws()?;
            }
            self.value(depth + 1)?;
            self.ws()?;
            if self.byte() == Some(close) {
                return self.step();
            }
            self.expect(b',')?;
            self.ws()?;
        }
    }
}

fn key_string<'a>(input: &'a [u8], t: &Token, at: usize) -> Result<JsonString<'a>, Error> {
    let raw = core::str::from_utf8(&input[t.start + 1..t.end - 1])
        .map_err(|_| Error::new(ErrorKind::Utf8, at))?;
    Ok(JsonString {
        raw,
        decoded_len: t.decoded_len,
        escaped: t.escaped,
    })
}

/// Reject `current` if it equals an earlier key of the object whose first key
/// token is `start`. Both parsers share this, so their work totals agree.
pub(crate) fn check_duplicate(
    tokens: &[Token],
    input: &[u8],
    start: usize,
    current: usize,
    work: &mut usize,
    max_work: usize,
    at: usize,
) -> Result<(), Error> {
    let err = |kind| Error::new(kind, at);
    let tick = |work: &mut usize, amount: usize| -> Result<(), Error> {
        *work = work.checked_add(amount).ok_or(err(ErrorKind::WorkLimit))?;
        if *work > max_work {
            return Err(err(ErrorKind::WorkLimit));
        }
        Ok(())
    };
    let mut previous = start;
    while previous < current {
        tick(work, 1)?;
        let mut a = key_string(input, &tokens[previous], at)?.chars();
        let mut b = key_string(input, &tokens[current], at)?.chars();
        let mut units = 0usize;
        let equal = loop {
            units = units.checked_add(24).ok_or(err(ErrorKind::WorkLimit))?;
            // Enforce while comparing, rather than after an unbounded key scan.
            if units > max_work - *work {
                return Err(err(ErrorKind::WorkLimit));
            }
            match (a.next(), b.next()) {
                (None, None) => break true,
                (Some(x), Some(y)) if x == y => {}
                _ => break false,
            }
        };
        tick(work, units)?;
        if equal {
            return Err(err(ErrorKind::DuplicateKey));
        }
        previous = tokens[previous + 1].next;
    }
    Ok(())
}
