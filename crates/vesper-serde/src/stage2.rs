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
//! tape, the same `work_used` and the same capacity; on every input it
//! rejects, it reports the same error kind at the same offset, including
//! when several limits are exceeded at once (see [`Builder`]).
use crate::index::{HARD_DEPTH, IndexPlan, Token, check_duplicate, escape, escape_with};
use crate::simd::{IndexLimits, Isa, StructuralIndex};
use crate::{Error, ErrorKind, IndexedJson, Kind, Limits};

/// Admission and the `max_index_bytes` split shared by owned and reusable
/// parsing (and, through [`IndexPlan`], by the scalar parser).
fn admit(input: &[u8], limits: &Limits) -> Result<(IndexPlan, IndexLimits), Error> {
    if input.len() > limits.max_input_bytes {
        return Err(Error::new(ErrorKind::InputLimit, 0));
    }
    if input.len() > limits.max_work {
        return Err(Error::new(ErrorKind::WorkLimit, 0));
    }
    let plan = IndexPlan::new(input.len(), limits);
    let index = IndexLimits {
        max_input_bytes: limits.max_input_bytes,
        max_entries: plan.entries,
    };
    Ok((plan, index))
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
        let (plan, index_limits) = admit(input, &limits)?;
        index.build(input, isa, index_limits)?;
        let mut tokens = Vec::new();
        tokens
            .try_reserve_exact(plan.capacity)
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        let work_used = build(input, index, &limits, plan.capacity, &mut tokens)?;
        Ok(Self {
            input: whole_str(input)?,
            tokens,
            work_used,
            index_entries: index.structurals().len() + index.escapes().len(),
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
        self.index.capacity_bytes() + self.tokens.capacity() * size_of::<Token>()
    }

    /// Validate `input` and lend the document to `f`. Views cannot outlive
    /// the call; borrowed `&'a str` values from the input can.
    pub fn parse<'a, R>(
        &mut self,
        input: &'a [u8],
        limits: Limits,
        f: impl FnOnce(&IndexedJson<'a>) -> R,
    ) -> Result<R, Error> {
        let (plan, index_limits) = admit(input, &limits)?;
        self.index.build(input, self.isa, index_limits)?;
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
            index_entries: self.index.structurals().len() + self.index.escapes().len(),
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

/// Walks the structural index in document order, the order in which the
/// scalar parser meets every byte, so that the first error reported is the
/// scalar parser's first error. Work is not charged byte by byte: the scalar
/// parser charges `len` up front, one unit per byte it consumes and the
/// duplicate-key comparisons, so its `WorkLimit` fires when it steps the
/// byte at the *frontier* `max_work - len - dup`. Each error candidate
/// records how far the scalar parser had stepped when it would detect it,
/// and becomes `WorkLimit` at the frontier if that lies earlier.
struct Builder<'i, 't> {
    input: &'i [u8],
    s: &'i [u32],
    escapes: &'i [u32],
    control: Option<usize>,
    /// The first stage 1 entry not in the index, when it was truncated.
    truncated: Option<usize>,
    tokens: &'t mut Vec<Token>,
    capacity: usize,
    limits: &'i Limits,
    /// Duplicate-key work so far.
    dup: usize,
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
        truncated: index.truncated_at().map(|p| p as usize),
        tokens,
        capacity,
        limits,
        dup: 0,
        string_bytes: 0,
        esc: 0,
    };
    b.document()?;
    // The scalar parser charges every byte once up front and once as it is
    // consumed; a complete parse consumes every byte exactly once.
    b.check_frontier(input.len())?;
    if b.truncated.is_some()
        || index.unclosed_string()
        || b.control.is_some_and(|c| c < input.len())
    {
        // Unreachable for a successful walk; kept as a typed backstop.
        return Err(Error::new(ErrorKind::Syntax, input.len()));
    }
    Ok(2 * input.len() + b.dup)
}

impl Builder<'_, '_> {
    /// The first byte whose step would exceed `max_work`.
    fn frontier(&self) -> usize {
        // `admit` guarantees len <= max_work; dup checks keep len+at+dup in it.
        (self.limits.max_work - self.input.len()).saturating_sub(self.dup)
    }
    /// `e` as the scalar parser reports it, given it had stepped every byte
    /// before `stepped` when detecting it.
    #[cold]
    fn fail(&self, e: Error, stepped: usize) -> Error {
        let frontier = self.frontier();
        if frontier < stepped {
            Error::new(ErrorKind::WorkLimit, self.work_offset(frontier))
        } else {
            e
        }
    }
    fn err(&self, kind: ErrorKind, at: usize) -> Error {
        self.fail(Error::new(kind, at), at)
    }
    fn check_frontier(&self, stepped: usize) -> Result<(), Error> {
        if self.frontier() < stepped {
            return Err(Error::new(
                ErrorKind::WorkLimit,
                self.work_offset(self.frontier()),
            ));
        }
        Ok(())
    }
    /// Where the scalar parser reports running out of work at byte `p`: at
    /// `p`, unless `p` is inside an escape body, whose bytes it charges in
    /// one step after decoding, reporting just past the escape.
    #[cold]
    fn work_offset(&self, p: usize) -> usize {
        let k = self.escapes.partition_point(|&e| (e as usize) < p);
        let decode_end = |e: usize| {
            let mut pos = e + 1;
            escape(self.input, &mut pos).ok().map(|_| pos)
        };
        for back in [2usize, 1] {
            // The containing escape is the last one before `p`, or the one
            // before that when the last is the low half of a surrogate pair.
            if let Some(&e) = k.checked_sub(back).and_then(|j| self.escapes.get(j))
                && let Some(end) = decode_end(e as usize)
                && end > p
            {
                return end;
            }
        }
        p
    }

    /// Position of structural `i`; past the index, the truncation point or
    /// the end of input.
    fn pos(&self, i: usize) -> usize {
        match self.s.get(i) {
            Some(&p) => p as usize,
            None => self.truncated.unwrap_or(self.input.len()),
        }
    }
    /// Byte at structural `i`, including the first entry cut by truncation.
    fn byte(&self, i: usize) -> Option<u8> {
        match self.s.get(i) {
            Some(&p) => Some(self.input[p as usize]),
            None if i == self.s.len() => self.truncated.map(|p| self.input[p]),
            None => None,
        }
    }
    /// Charge structural `i`'s index entry: it fails exactly at truncation.
    fn entry(&self, i: usize) -> Result<(), Error> {
        if i >= self.s.len()
            && let Some(p) = self.truncated
        {
            return Err(self.err(ErrorKind::AllocationLimit, p));
        }
        Ok(())
    }
    fn syntax(&self, i: usize) -> Error {
        self.err(ErrorKind::Syntax, self.pos(i))
    }
    /// An operator at structural `i` that the grammar expects here.
    fn op(&self, i: usize, b: u8) -> Result<(), Error> {
        if self.byte(i) != Some(b) {
            return Err(self.syntax(i));
        }
        self.entry(i)
    }
    fn push(&mut self, kind: Kind, depth: usize, start: usize) -> Result<usize, Error> {
        if self.tokens.len() >= self.limits.max_tokens {
            return Err(self.err(ErrorKind::TokenLimit, start));
        }
        if self.tokens.len() >= self.capacity {
            return Err(self.err(ErrorKind::AllocationLimit, start));
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
                        return Err(self.err(ErrorKind::DepthLimit, self.pos(i)));
                    }
                    let kind = if object { Kind::Object } else { Kind::Array };
                    let token = self.push(kind, depth, self.pos(i))?;
                    self.entry(i)?;
                    i += 1;
                    let close = if object { b'}' } else { b']' };
                    if self.byte(i) == Some(close) {
                        self.entry(i)?;
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
                    return if i == self.s.len() && self.truncated.is_none() {
                        Ok(())
                    } else {
                        Err(self.syntax(i))
                    };
                }
                let frame = stack[depth - 1];
                let close = if frame.object { b'}' } else { b']' };
                match self.byte(i) {
                    Some(b',') => {
                        self.entry(i)?;
                        i += 1;
                        if frame.object {
                            i = self.key(i, frame.token, depth)?;
                        }
                        break;
                    }
                    Some(b) if b == close => {
                        self.entry(i)?;
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
        self.check_frontier(at)?;
        let before = self.input.len() + at + self.dup;
        let mut work = before;
        check_duplicate(
            self.tokens,
            self.input,
            object + 1,
            key,
            &mut work,
            self.limits.max_work,
            at,
        )?;
        self.dup += work - before;
        self.op(next, b':')?;
        Ok(next + 1)
    }

    /// A string at structural `i`. The scalar parser meets, in byte order:
    /// ordinary bytes (one decoded byte each), escapes (entry, decode,
    /// decoded length), control bytes (an error) and the closing quote
    /// (entry), checking `max_string_bytes` after each unit.
    fn string(&mut self, i: usize, depth: usize) -> Result<usize, Error> {
        let open = self.pos(i);
        let token = self.push(Kind::String, depth, open)?;
        self.entry(i)?;
        let closed = self.byte(i + 1) == Some(b'"') && i + 1 < self.s.len();
        // How far the units run: the closing quote, the truncation point
        // (the next entry, inside or closing this string) or the end.
        let bound = if closed {
            self.pos(i + 1)
        } else {
            self.truncated.unwrap_or(self.input.len())
        };
        let max = self.limits.max_string_bytes;
        let control = self.control.filter(|&c| c > open && c < bound);
        let no_escape = self
            .escapes
            .get(self.esc)
            .is_none_or(|&e| e as usize >= bound);
        if closed && control.is_none() && no_escape && bound - open - 1 <= max {
            // Common case: a closed string of ordinary bytes within limits.
            return self.finish_string(token, bound, bound - open - 1, false, i);
        }
        let mut decoded = 0usize;
        let mut escaped = false;
        let mut at = open + 1;
        loop {
            let next_escape = loop {
                match self.escapes.get(self.esc) {
                    Some(&e) if (e as usize) < at => self.esc += 1, // low half, done
                    Some(&e) if (e as usize) < bound => break Some(e as usize),
                    _ => break None,
                }
            };
            let event = match (next_escape, control) {
                (Some(e), Some(c)) => e.min(c),
                (Some(e), None) => e,
                (None, Some(c)) => c,
                (None, None) => bound,
            };
            // Ordinary bytes up to the event.
            let run = event - at;
            if decoded + run > max {
                let limit = at + (max - decoded) + 1;
                return Err(self.err(ErrorKind::StringLimit, limit));
            }
            decoded += run;
            if event == bound {
                break;
            }
            if Some(event) == control {
                return Err(self.err(ErrorKind::Syntax, event));
            }
            // An escape: its entry is present (it is before any truncation).
            self.esc += 1;
            escaped = true;
            let mut pos = event + 1;
            let truncated = self.truncated;
            let ch = escape_with(self.input, &mut pos, &mut |second| match truncated {
                Some(p) if second >= p => Err(Error::new(ErrorKind::AllocationLimit, p)),
                _ => Ok(()),
            })
            .map_err(|e| self.fail(e, event + 1))?;
            decoded += ch.len_utf8();
            at = pos;
            if decoded > max {
                return Err(self.err(ErrorKind::StringLimit, at));
            }
        }
        if !closed {
            return Err(match self.truncated {
                Some(p) => self.err(ErrorKind::AllocationLimit, p),
                None => self.err(ErrorKind::Syntax, self.input.len()),
            });
        }
        self.finish_string(token, bound, decoded, escaped, i)
    }

    /// After the closing quote at `close`: the total string budget, then
    /// the token's fields.
    #[inline]
    fn finish_string(
        &mut self,
        token: usize,
        close: usize,
        decoded: usize,
        escaped: bool,
        i: usize,
    ) -> Result<usize, Error> {
        let bound = close;
        self.string_bytes = self
            .string_bytes
            .checked_add(decoded)
            .ok_or(self.err(ErrorKind::StringLimit, bound + 1))?;
        if self.string_bytes > self.limits.max_total_string_bytes {
            return Err(self.err(ErrorKind::StringLimit, bound + 1));
        }
        let t = &mut self.tokens[token];
        t.end = bound + 1;
        t.decoded_len = decoded;
        t.escaped = escaped;
        Ok(i + 2)
    }

    fn scalar(&mut self, i: usize, depth: usize) -> Result<usize, Error> {
        let start = self.pos(i);
        let token = self.push(Kind::Null, depth, start)?;
        self.entry(i)?;
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
                    return Err(self.err(ErrorKind::Syntax, start + at));
                }
                (kind, literal.len())
            }
            _ => (
                Kind::Number,
                number(text).map_err(|at| self.err(ErrorKind::Number, start + at))?,
            ),
        };
        if len != text.len() {
            return Err(self.err(ErrorKind::Syntax, start + len));
        }
        let t = &mut self.tokens[token];
        t.kind = kind;
        t.end = end;
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
