//! Stage 2 of design §20.3: validate the document over the stage 1 structural
//! index and build the subtree-skipping token tape behind [`IndexedJson`].
//!
//! The builder walks structural positions with an explicit bounded stack
//! instead of scanning bytes: strings are delimited by their two quote
//! positions, escapes are visited from the stage 1 escape list only, and a
//! scalar spans from its start to the next structural. Nothing is decoded:
//! numbers are converted and escaped strings unescaped only when the caller
//! asks through [`Node`](crate::Node) or [`Cursor`](crate::Cursor).
//!
//! On every input the scalar parser accepts, this builder produces the same
//! tape, the same `work_used` and the same capacity; it rejects exactly the
//! inputs the scalar parser rejects. Error offsets and, for inputs with
//! several faults, error kinds may differ.
use crate::index::{HARD_DEPTH, Token, check_duplicate, escape};
use crate::simd::{IndexLimits, Isa, StructuralIndex};
use crate::{Error, ErrorKind, IndexedJson, Kind, Limits};

/// Admission and capacity shared by owned and reusable parsing.
struct Plan {
    capacity: usize,
}

fn admit(input: &[u8], limits: &Limits) -> Result<Plan, Error> {
    if input.len() > limits.max_input_bytes {
        return Err(Error::new(ErrorKind::InputLimit, 0));
    }
    if input.len() > limits.max_work {
        return Err(Error::new(ErrorKind::WorkLimit, 0));
    }
    let capacity = input
        .len()
        .min(limits.max_tokens)
        .min(limits.max_index_bytes / size_of::<Token>());
    Ok(Plan { capacity })
}

fn index_limits(input: &[u8], limits: &Limits) -> IndexLimits {
    // A document of T tokens has at most 4T structurals: two per value plus
    // one comma and one colon per value. More means TokenLimit or bad syntax.
    IndexLimits {
        max_input_bytes: limits.max_input_bytes,
        max_structurals: limits
            .max_tokens
            .saturating_mul(4)
            .saturating_add(1)
            .min(input.len()),
    }
}

impl<'a> IndexedJson<'a> {
    /// Parse with the fastest stage 1 path this CPU supports.
    pub fn parse_simd(input: &'a [u8], limits: Limits) -> Result<Self, Error> {
        Self::parse_with_isa(input, limits, Isa::detect())
    }

    /// Parse with an explicit stage 1 path; `IsaUnavailable` if it cannot run.
    pub fn parse_with_isa(input: &'a [u8], limits: Limits, isa: Isa) -> Result<Self, Error> {
        Self::parse_with_index(input, limits, isa, &mut StructuralIndex::new())
    }

    /// As [`parse_with_isa`](Self::parse_with_isa), reusing `index`'s capacity
    /// for stage 1. The token tape is still owned by the returned document.
    pub fn parse_with_index(
        input: &'a [u8],
        limits: Limits,
        isa: Isa,
        index: &mut StructuralIndex,
    ) -> Result<Self, Error> {
        let plan = admit(input, &limits)?;
        index.build(input, isa, index_limits(input, &limits))?;
        let mut tokens = Vec::new();
        tokens
            .try_reserve_exact(plan.capacity)
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        let work_used = build(input, index, &limits, plan.capacity, &mut tokens)?;
        Ok(Self {
            input: whole_str(input)?,
            tokens,
            work_used,
        })
    }
}

/// The stage 1 kernel already rejected invalid UTF-8; this one safe pass
/// (std's validator, very fast on ASCII) yields the `&str` that keeps every
/// token view an O(1) slice without `from_utf8_unchecked`.
fn whole_str(input: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(input).map_err(|e| Error::new(ErrorKind::Utf8, e.valid_up_to()))
}

/// A reusable two-stage parser. The structural index and token tape keep
/// their capacity between documents, so steady-state parsing of documents no
/// larger than earlier ones makes no allocation (an arena-rewind analogue).
#[derive(Debug)]
pub struct SimdParser {
    isa: Isa,
    index: StructuralIndex,
    tokens: Vec<Token>,
}

impl SimdParser {
    /// A parser on the fastest available path.
    pub fn new() -> Self {
        Self {
            isa: Isa::detect(),
            index: StructuralIndex::new(),
            tokens: Vec::new(),
        }
    }
    /// A parser on an explicit path, if this CPU can run it.
    pub fn with_isa(isa: Isa) -> Result<Self, Error> {
        if !isa.is_available() {
            return Err(Error::new(ErrorKind::IsaUnavailable, 0));
        }
        Ok(Self {
            isa,
            index: StructuralIndex::new(),
            tokens: Vec::new(),
        })
    }
    pub fn isa(&self) -> Isa {
        self.isa
    }
    /// Retained structural-index and tape capacity in bytes.
    pub fn retained_bytes(&self) -> usize {
        self.index.structurals.capacity() * 4
            + self.index.escapes.capacity() * 4
            + self.tokens.capacity() * size_of::<Token>()
    }

    /// Validate `input` and lend the document to `f`. Views cannot outlive
    /// the call; borrowed `&'a str` values from the input can.
    pub fn parse<'a, R>(
        &mut self,
        input: &'a [u8],
        limits: Limits,
        f: impl FnOnce(&IndexedJson<'a>) -> R,
    ) -> Result<R, Error> {
        let plan = admit(input, &limits)?;
        self.index
            .build(input, self.isa, index_limits(input, &limits))?;
        let mut tokens = core::mem::take(&mut self.tokens);
        tokens.clear();
        if tokens.capacity() < plan.capacity {
            tokens
                .try_reserve_exact(plan.capacity)
                .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        }
        let built = build(input, &self.index, &limits, plan.capacity, &mut tokens);
        let work_used = match built {
            Ok(work) => work,
            Err(e) => {
                self.tokens = tokens;
                return Err(e);
            }
        };
        let text = match whole_str(input) {
            Ok(text) => text,
            Err(e) => {
                self.tokens = tokens;
                return Err(e);
            }
        };
        let doc = IndexedJson {
            input: text,
            tokens,
            work_used,
        };
        let result = f(&doc);
        self.tokens = doc.tokens;
        Ok(result)
    }
}

impl Default for SimdParser {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct Frame {
    token: usize,
    object: bool,
}

struct Builder<'i, 't> {
    input: &'i [u8],
    s: &'i [u32],
    escapes: &'i [u32],
    control: Option<usize>,
    tokens: &'t mut Vec<Token>,
    capacity: usize,
    limits: &'i Limits,
    work: usize,
    string_bytes: usize,
    esc: usize,
}

fn build(
    input: &[u8],
    index: &StructuralIndex,
    limits: &Limits,
    capacity: usize,
    tokens: &mut Vec<Token>,
) -> Result<usize, Error> {
    let mut b = Builder {
        input,
        s: index.structurals(),
        escapes: index.escapes(),
        control: index.first_string_control().map(|c| c as usize),
        tokens,
        capacity,
        limits,
        // The scalar parser charges every byte once up front and once as it
        // is consumed; a complete parse consumes every byte exactly once.
        work: input.len(),
        string_bytes: 0,
        esc: 0,
    };
    b.tick(input.len())?;
    b.document()?;
    if index.unclosed_string() || b.control.is_some_and(|c| c < input.len()) {
        // Unreachable for a successful walk; kept as a typed backstop.
        return Err(Error::new(ErrorKind::Syntax, input.len()));
    }
    Ok(b.work)
}

impl Builder<'_, '_> {
    fn tick(&mut self, amount: usize) -> Result<(), Error> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or(Error::new(ErrorKind::WorkLimit, 0))?;
        if self.work > self.limits.max_work {
            return Err(Error::new(ErrorKind::WorkLimit, 0));
        }
        Ok(())
    }
    fn pos(&self, i: usize) -> usize {
        self.s.get(i).map_or(self.input.len(), |&p| p as usize)
    }
    fn byte(&self, i: usize) -> Option<u8> {
        self.s.get(i).map(|&p| self.input[p as usize])
    }
    fn syntax(&self, i: usize) -> Error {
        Error::new(ErrorKind::Syntax, self.pos(i))
    }
    fn push(&mut self, kind: Kind, depth: usize, start: usize) -> Result<usize, Error> {
        if self.tokens.len() >= self.limits.max_tokens {
            return Err(Error::new(ErrorKind::TokenLimit, start));
        }
        if self.tokens.len() >= self.capacity {
            return Err(Error::new(ErrorKind::AllocationLimit, start));
        }
        let index = self.tokens.len();
        self.tokens.push(Token {
            start,
            end: 0,
            next: index + 1,
            kind,
            decoded_len: 0,
            escaped: false,
            depth,
        });
        Ok(index)
    }

    fn document(&mut self) -> Result<(), Error> {
        let mut stack = [Frame {
            token: 0,
            object: false,
        }; HARD_DEPTH];
        let mut depth = 0usize;
        let mut i = 0usize;
        // `i` is at a value. Each pass parses one value, then unwinds closed
        // containers until it reaches the next value or the end.
        loop {
            match self.byte(i) {
                Some(open @ (b'{' | b'[')) => {
                    let object = open == b'{';
                    if depth >= self.limits.max_depth || depth >= HARD_DEPTH {
                        return Err(Error::new(ErrorKind::DepthLimit, self.pos(i)));
                    }
                    let kind = if object { Kind::Object } else { Kind::Array };
                    let token = self.push(kind, depth, self.pos(i))?;
                    i += 1;
                    let close = if object { b'}' } else { b']' };
                    if self.byte(i) == Some(close) {
                        self.tokens[token].end = self.pos(i) + 1;
                        i += 1;
                    } else {
                        stack[depth] = Frame { token, object };
                        depth += 1;
                        if object {
                            i = self.key(i, token, depth)?;
                        }
                        continue;
                    }
                }
                Some(b'"') => i = self.string(i, depth)?,
                Some(b'n' | b't' | b'f' | b'-' | b'0'..=b'9') => i = self.scalar(i, depth)?,
                _ => return Err(self.syntax(i)),
            }
            // A value just ended at structural `i`; close finished containers.
            loop {
                if depth == 0 {
                    return if i == self.s.len() {
                        Ok(())
                    } else {
                        Err(self.syntax(i))
                    };
                }
                let frame = stack[depth - 1];
                let close = if frame.object { b'}' } else { b']' };
                match self.byte(i) {
                    Some(b',') => {
                        i += 1;
                        if frame.object {
                            i = self.key(i, frame.token, depth)?;
                        }
                        break;
                    }
                    Some(b) if b == close => {
                        self.tokens[frame.token].end = self.pos(i) + 1;
                        self.tokens[frame.token].next = self.tokens.len();
                        depth -= 1;
                        i += 1;
                    }
                    _ => return Err(self.syntax(i)),
                }
            }
        }
    }

    /// Parse `"key" :` at `i` inside object token `object`; return the index
    /// of the member value.
    fn key(&mut self, i: usize, object: usize, depth: usize) -> Result<usize, Error> {
        if self.byte(i) != Some(b'"') {
            return Err(self.syntax(i));
        }
        let key = self.tokens.len();
        let next = self.string(i, depth)?;
        let at = self.pos(next - 1) + 1;
        check_duplicate(
            self.tokens,
            self.input,
            object + 1,
            key,
            &mut self.work,
            self.limits.max_work,
            at,
        )?;
        if self.byte(next) != Some(b':') {
            return Err(self.syntax(next));
        }
        Ok(next + 1)
    }

    fn string(&mut self, i: usize, depth: usize) -> Result<usize, Error> {
        let open = self.pos(i);
        // An unclosed string runs to the end; its escapes are still checked
        // first, in byte order, as the scalar parser does.
        let closed = self.byte(i + 1) == Some(b'"');
        let close = if closed {
            self.pos(i + 1)
        } else {
            self.input.len()
        };
        let token = self.push(Kind::String, depth, open)?;
        let mut decoded = close - open - 1;
        let mut escaped = false;
        let mut consumed = open + 1;
        while let Some(&e) = self.escapes.get(self.esc) {
            let e = e as usize;
            if e >= close {
                break;
            }
            self.esc += 1;
            if e < consumed {
                // The low half of a surrogate pair, already decoded.
                continue;
            }
            escaped = true;
            if let Some(c) = self.control
                && c < e
            {
                return Err(Error::new(ErrorKind::Syntax, c));
            }
            let mut pos = e + 1;
            let ch = escape(self.input, &mut pos)?;
            decoded = decoded - (pos - e) + ch.len_utf8();
            consumed = pos;
        }
        if let Some(c) = self.control
            && c < close
        {
            return Err(Error::new(ErrorKind::Syntax, c));
        }
        if !closed {
            return Err(Error::new(ErrorKind::Syntax, self.input.len()));
        }
        if decoded > self.limits.max_string_bytes {
            let at = self.limit_offset(open, close);
            return Err(Error::new(ErrorKind::StringLimit, at));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(decoded)
            .ok_or(Error::new(ErrorKind::StringLimit, close + 1))?;
        if self.string_bytes > self.limits.max_total_string_bytes {
            return Err(Error::new(ErrorKind::StringLimit, close + 1));
        }
        let t = &mut self.tokens[token];
        t.end = close + 1;
        t.decoded_len = decoded;
        t.escaped = escaped;
        Ok(i + 2)
    }

    /// Error path only: the offset just past the unit (byte or escape) at
    /// which the decoded length first exceeds `max_string_bytes`.
    fn limit_offset(&self, open: usize, close: usize) -> usize {
        let mut pos = open + 1;
        let mut decoded = 0usize;
        while pos < close {
            if self.input[pos] == b'\\' {
                let mut next = pos + 1;
                decoded += escape(self.input, &mut next).map_or(1, char::len_utf8);
                pos = next;
            } else {
                decoded += 1;
                pos += 1;
            }
            if decoded > self.limits.max_string_bytes {
                return pos;
            }
        }
        close
    }

    fn scalar(&mut self, i: usize, depth: usize) -> Result<usize, Error> {
        let start = self.pos(i);
        let mut end = self.pos(i + 1);
        while end > start && matches!(self.input[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
            end -= 1;
        }
        let text = &self.input[start..end];
        let (kind, len) = match text[0] {
            b'n' | b't' | b'f' => {
                let (kind, literal): (Kind, &[u8]) = match text[0] {
                    b'n' => (Kind::Null, b"null"),
                    b't' => (Kind::Bool, b"true"),
                    _ => (Kind::Bool, b"false"),
                };
                if let Some(at) = (0..literal.len()).find(|&k| text.get(k) != Some(&literal[k])) {
                    return Err(Error::new(ErrorKind::Syntax, start + at));
                }
                (kind, literal.len())
            }
            _ => (
                Kind::Number,
                number(text).map_err(|at| Error::new(ErrorKind::Number, start + at))?,
            ),
        };
        if len != text.len() {
            return Err(Error::new(ErrorKind::Syntax, start + len));
        }
        let token = self.push(kind, depth, start)?;
        self.tokens[token].end = end;
        Ok(i + 1)
    }
}

/// JSON number grammar; returns the length of the longest valid prefix or the
/// offset of the byte that broke the grammar.
fn number(text: &[u8]) -> Result<usize, usize> {
    let mut p = 0;
    let at = |p: usize| text.get(p).copied();
    let digits = |mut p: usize| {
        while matches!(at(p), Some(b'0'..=b'9')) {
            p += 1;
        }
        p
    };
    if at(p) == Some(b'-') {
        p += 1;
    }
    match at(p) {
        Some(b'0') => p += 1,
        Some(b'1'..=b'9') => p = digits(p + 1),
        _ => return Err(p),
    }
    if at(p) == Some(b'.') {
        p += 1;
        if !matches!(at(p), Some(b'0'..=b'9')) {
            return Err(p);
        }
        p = digits(p);
    }
    if matches!(at(p), Some(b'e' | b'E')) {
        p += 1;
        if matches!(at(p), Some(b'+' | b'-')) {
            p += 1;
        }
        if !matches!(at(p), Some(b'0'..=b'9')) {
            return Err(p);
        }
        p = digits(p);
    }
    Ok(p)
}
