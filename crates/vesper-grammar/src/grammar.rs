//! Executable acyclic byte-FIRST grammar subset, not a source or general PEG
//! compiler. FIRST/nullable information is derived from actual rules. Choices
//! require nonnullable, disjoint alternatives; FOLLOW-dependent epsilon choices
//! are explicitly unsupported. All rule references, including unreachable ones,
//! are validated. Shared DAG rules are legal; recursion is not.
pub mod codec;

use crate::framing::{self, Remainder};
#[cfg(vesper_port)]
use crate::port;
use crate::{Error, ErrorKind, Result, add, bound, charge, mul};
use core::ops::Range;

pub const HARD_MAX_REPETITIONS: usize = 4096;
/// The depth ceiling whatever `Limits::max_depth` says.
const HARD_MAX_DEPTH: usize = 64;
/// FIRST sets are 256-bit byte sets in four `u64` words.
#[cfg_attr(vesper_port, allow(dead_code))]
const SET_WORD_BITS: usize = u64::BITS as usize;
/// The port's per-rule metadata (vesper/src/grammar.ves): the four FIRST-set
/// words, then nullable, color and height.
#[cfg_attr(not(vesper_port), allow(dead_code))]
const PORT_META_WORDS: usize = 7;
#[cfg_attr(not(vesper_port), allow(dead_code))]
const PORT_META_NULLABLE: usize = 4;

#[derive(Clone, Debug)]
pub struct Branch {
    pub id: u16,
    pub rule: usize,
}
#[derive(Clone, Debug)]
pub enum Rule {
    Empty,
    Byte(u8),
    Literal(Vec<u8>),
    Uleb128,
    /// Canonical length + opaque payload.
    Blob,
    /// Canonical length + exact child-rule payload, enabling bounded nesting.
    Frame(usize),
    /// Three-byte big-endian length plus an exact child-rule payload.
    U24Frame(usize),
    Sequence(Vec<usize>),
    Choice(Vec<Branch>),
    /// Exactly `count` child parses; no count bytes are present on the wire.
    Repeat {
        rule: usize,
        count: usize,
    },
    /// A canonical unsigned ULEB128 count followed by that many child parses.
    Counted(usize),
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_rules: usize,
    pub max_edges: usize,
    pub max_literal_bytes: usize,
    pub max_total_literal_bytes: usize,
    pub max_depth: usize,
    pub max_input_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_events: usize,
    pub max_metadata_bytes: usize,
    pub max_capture_bytes: usize,
    pub max_work: usize,
    /// Bounds each fixed or wire-supplied repetition count. Zero permits zero
    /// counts only; values above HARD_MAX_REPETITIONS are invalid settings.
    pub max_repetitions: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rules: 256,
            max_edges: 1024,
            max_literal_bytes: 4096,
            max_total_literal_bytes: 65_536,
            max_depth: 32,
            max_input_bytes: 1024 * 1024,
            max_frame_bytes: 1024 * 1024,
            max_events: 4096,
            max_metadata_bytes: 64 * 1024,
            max_capture_bytes: 256 * 1024,
            max_work: 4 * 1024 * 1024,
            max_repetitions: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Meta {
    first: [u64; 4],
    nullable: bool,
    color: u8,
    height: usize,
}
#[cfg(not(vesper_port))]
impl Meta {
    fn contains(self, byte: u8) -> bool {
        self.first[usize::from(byte) / SET_WORD_BITS] & (1 << (byte as usize % SET_WORD_BITS)) != 0
    }
    fn union(&mut self, other: Self) {
        for (a, b) in self.first.iter_mut().zip(other.first) {
            *a |= b;
        }
    }
}

#[derive(Debug)]
pub struct Grammar<'g> {
    rules: &'g [Rule],
    root: usize,
    #[cfg(not(vesper_port))]
    meta: Vec<Meta>,
    /// The port's metadata: seven words per rule (vesper/src/grammar.ves).
    #[cfg(vesper_port)]
    meta: Vec<u64>,
    limits: Limits,
    /// The rules flattened for the port (`port::flatten_rules`).
    #[cfg(vesper_port)]
    flat: Vec<u64>,
    #[cfg(vesper_port)]
    lits: Vec<u8>,
}
impl<'g> Grammar<'g> {
    /// Borrows rule owners, and allocates one bounded metadata vector. Invalid
    /// rules never consume or modify the caller's rule definitions.
    #[cfg(not(vesper_port))]
    pub fn validate(rules: &'g [Rule], root: usize, limits: Limits) -> Result<Self> {
        if limits.max_repetitions > HARD_MAX_REPETITIONS {
            return Err(Error::new(ErrorKind::Rule, 0));
        }
        if rules.is_empty() || root >= rules.len() {
            return Err(Error::new(ErrorKind::Rule, root));
        }
        bound(rules.len(), limits.max_rules, ErrorKind::CountLimit)?;
        let mut work = limits.max_work;
        charge(&mut work, rules.len())?;
        let mut edges = 0;
        let mut literals = 0;
        for (id, rule) in rules.iter().enumerate() {
            let (single, sequence, branches): (Option<usize>, &[usize], &[Branch]) = match rule {
                Rule::Frame(child) | Rule::U24Frame(child) | Rule::Counted(child) => {
                    (Some(*child), &[], &[])
                }
                Rule::Repeat { rule, count } => {
                    if *count > limits.max_repetitions {
                        return Err(Error::new(ErrorKind::CountLimit, id));
                    }
                    (Some(*rule), &[], &[])
                }
                Rule::Sequence(children) => (None, children, &[]),
                Rule::Choice(branches) => {
                    if branches.is_empty() {
                        return Err(Error::new(ErrorKind::Rule, id));
                    }
                    (None, &[], branches)
                }
                Rule::Literal(bytes) => {
                    bound(bytes.len(), limits.max_literal_bytes, ErrorKind::InputLimit)?;
                    literals = add(literals, bytes.len())?;
                    bound(
                        literals,
                        limits.max_total_literal_bytes,
                        ErrorKind::InputLimit,
                    )?;
                    charge(&mut work, bytes.len())?;
                    (None, &[], &[])
                }
                _ => (None, &[], &[]),
            };
            let count = add(
                add(usize::from(single.is_some()), sequence.len())?,
                branches.len(),
            )?;
            edges = add(edges, count)?;
            bound(edges, limits.max_edges, ErrorKind::CountLimit)?;
            charge(&mut work, count)?;
            for (i, branch) in branches.iter().enumerate() {
                for previous in &branches[..i] {
                    charge(&mut work, 1)?;
                    if previous.id == branch.id {
                        return Err(Error::new(ErrorKind::DuplicateBranch, id));
                    }
                }
            }
            if single
                .into_iter()
                .chain(sequence.iter().copied())
                .chain(branches.iter().map(|b| b.rule))
                .any(|child| child >= rules.len())
            {
                return Err(Error::new(ErrorKind::Rule, id));
            }
        }
        let bytes = mul(rules.len(), size_of::<Meta>())?;
        bound(bytes, limits.max_metadata_bytes, ErrorKind::AllocationLimit)?;
        let mut meta = Vec::new();
        meta.try_reserve_exact(rules.len())
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        meta.resize(rules.len(), Meta::default());
        for id in 0..rules.len() {
            build(
                id,
                rules,
                &mut meta,
                &mut work,
                limits.max_depth.min(HARD_MAX_DEPTH),
                1,
            )?;
        }
        Ok(Self {
            rules,
            root,
            meta,
            limits,
        })
    }
    #[cfg(vesper_port)]
    pub fn validate(rules: &'g [Rule], root: usize, limits: Limits) -> Result<Self> {
        let (flat, lits) = port::flatten_rules(rules);
        let lim = port::grammar_limits(limits, size_of::<Meta>());
        let meta =
            port::vesper::gr_validate(&flat, &lits, root as u64, &lim).map_err(port::error)?;
        Ok(Self {
            rules,
            root,
            meta,
            limits,
            flat,
            lits,
        })
    }
    /// The port's view of this grammar: flattened rules, literal bytes,
    /// metadata and the root.
    #[cfg(vesper_port)]
    pub(crate) fn port_parts(&self) -> (&[u64], &[u8], &[u64], u64) {
        (&self.flat, &self.lits, &self.meta, self.root as u64)
    }
    #[cfg(vesper_port)]
    pub(crate) fn port_limits(&self) -> [u64; 14] {
        port::grammar_limits(self.limits, size_of::<Meta>())
    }
    #[cfg(not(vesper_port))]
    pub fn nullable(&self, rule: usize) -> Option<bool> {
        self.meta.get(rule).map(|m| m.nullable)
    }
    #[cfg(vesper_port)]
    pub fn nullable(&self, rule: usize) -> Option<bool> {
        (rule < self.rules.len())
            .then(|| self.meta[rule * PORT_META_WORDS + PORT_META_NULLABLE] != 0)
    }
    #[cfg(not(vesper_port))]
    pub fn first_contains(&self, rule: usize, byte: u8) -> Option<bool> {
        self.meta.get(rule).map(|m| m.contains(byte))
    }
    #[cfg(vesper_port)]
    pub fn first_contains(&self, rule: usize, byte: u8) -> Option<bool> {
        (rule < self.rules.len()).then(|| {
            self.meta[rule * PORT_META_WORDS + usize::from(byte) / SET_WORD_BITS]
                & (1 << (byte as usize % SET_WORD_BITS))
                != 0
        })
    }
    #[cfg(not(vesper_port))]
    pub fn parse_prefix<'a>(&self, input: &'a [u8]) -> Result<Parsed<'a>> {
        bound(
            input.len(),
            self.limits.max_input_bytes,
            ErrorKind::InputLimit,
        )?;
        let capacity = self
            .limits
            .max_events
            .min(self.limits.max_work)
            .min(self.limits.max_capture_bytes / size_of::<Capture>());
        let mut captures = Vec::new();
        captures
            .try_reserve_exact(capacity)
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        let mut run = Run {
            grammar: self,
            input,
            pos: 0,
            end: input.len(),
            work: self.limits.max_work,
            captures,
            capacity,
        };
        run.rule(self.root, 1)?;
        Ok(Parsed {
            captures: run.captures,
            tail: Remainder::at(input, run.pos),
        })
    }
    #[cfg(vesper_port)]
    pub fn parse_prefix<'a>(&self, input: &'a [u8]) -> Result<Parsed<'a>> {
        let (flat, lits, meta, root) = self.port_parts();
        let words =
            port::vesper::gr_parse_prefix(flat, lits, meta, root, &self.port_limits(), input)
                .map_err(port::error)?;
        let (pos, captures) = words
            .split_last()
            .expect("the port returns the end position");
        Ok(Parsed {
            captures: port::captures(captures),
            tail: Remainder::at(input, *pos as usize),
        })
    }
    pub fn parse_exact(&self, input: &[u8]) -> Result<Vec<Capture>> {
        let parsed = self.parse_prefix(input)?;
        parsed.tail.assert_exact_eof().map_err(|e| e.error)?;
        Ok(parsed.captures)
    }
}
#[cfg(not(vesper_port))]
fn build(
    id: usize,
    rules: &[Rule],
    meta: &mut [Meta],
    work: &mut usize,
    max_depth: usize,
    depth: usize,
) -> Result<Meta> {
    charge(work, 1)?;
    if meta[id].color == 1 {
        return Err(Error::new(ErrorKind::RecursiveRule, id));
    }
    bound(depth, max_depth, ErrorKind::DepthLimit)?;
    if meta[id].color == 2 {
        return Ok(meta[id]);
    }
    meta[id].color = 1;
    let mut result = Meta {
        color: 2,
        height: 1,
        ..Meta::default()
    };
    match &rules[id] {
        Rule::Empty => result.nullable = true,
        Rule::Byte(byte) => {
            result.first[usize::from(*byte) / SET_WORD_BITS] |=
                1 << (*byte as usize % SET_WORD_BITS)
        }
        Rule::Literal(bytes) => {
            if let Some(byte) = bytes.first() {
                result.first[usize::from(*byte) / SET_WORD_BITS] |=
                    1 << (*byte as usize % SET_WORD_BITS);
            } else {
                result.nullable = true;
            }
        }
        Rule::Uleb128 | Rule::Blob => result.first = [u64::MAX; 4],
        Rule::Frame(child) | Rule::U24Frame(child) => {
            let child = build(*child, rules, meta, work, max_depth, depth + 1)?;
            result.first = [u64::MAX; 4];
            result.height = add(child.height, 1)?;
        }
        Rule::Repeat { rule, count } => {
            // Even zero-count/unreachable children participate in reference,
            // cycle and full DAG-height validation.
            let child = build(*rule, rules, meta, work, max_depth, depth + 1)?;
            result.height = add(child.height, 1)?;
            result.nullable = *count == 0 || child.nullable;
            if *count != 0 {
                result.union(child);
            }
        }
        Rule::Counted(child) => {
            let child = build(*child, rules, meta, work, max_depth, depth + 1)?;
            result.first = [u64::MAX; 4];
            result.height = add(child.height, 1)?;
        }
        Rule::Sequence(children) => {
            result.nullable = true;
            for &child in children {
                let child = build(child, rules, meta, work, max_depth, depth + 1)?;
                if result.nullable {
                    result.union(child);
                }
                result.nullable &= child.nullable;
                result.height = result.height.max(add(child.height, 1)?);
            }
        }
        Rule::Choice(branches) => {
            for branch in branches {
                let child = build(branch.rule, rules, meta, work, max_depth, depth + 1)?;
                if child.nullable {
                    return Err(Error::new(ErrorKind::NullableChoice, id));
                }
                if result
                    .first
                    .iter()
                    .zip(child.first)
                    .any(|(a, b)| *a & b != 0)
                {
                    return Err(Error::new(ErrorKind::AmbiguousChoice, id));
                }
                result.union(child);
                result.height = result.height.max(add(child.height, 1)?);
            }
        }
    }
    bound(result.height, max_depth, ErrorKind::DepthLimit)?;
    meta[id] = result;
    Ok(result)
}

// Decode within the caller's current frame and charge every header byte.
#[cfg(not(vesper_port))]
fn decode_u24(input: &[u8], work: &mut usize) -> Result<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..3 {
        charge(work, 1)?;
        let byte = *input
            .get(i)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, i))?;
        value = (value << 8) | u64::from(byte);
    }
    Ok((value, 3))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
    pub rule: usize,
    pub branch: Option<u16>,
    pub range: Range<usize>,
    pub number: Option<u64>,
    pub payload: Option<Range<usize>>,
}
#[derive(Debug)]
pub struct Parsed<'a> {
    pub captures: Vec<Capture>,
    pub tail: Remainder<'a>,
}
#[cfg(not(vesper_port))]
struct Run<'g, 'r, 'a> {
    grammar: &'g Grammar<'r>,
    input: &'a [u8],
    pos: usize,
    end: usize,
    work: usize,
    captures: Vec<Capture>,
    capacity: usize,
}
#[cfg(not(vesper_port))]
impl Run<'_, '_, '_> {
    fn rule(&mut self, id: usize, depth: usize) -> Result<()> {
        charge(&mut self.work, 1)?;
        bound(
            depth,
            self.grammar.limits.max_depth.min(HARD_MAX_DEPTH),
            ErrorKind::DepthLimit,
        )?;
        bound(
            add(self.captures.len(), 1)?,
            self.grammar.limits.max_events,
            ErrorKind::CountLimit,
        )?;
        if self.captures.len() >= self.capacity {
            return Err(Error::new(ErrorKind::AllocationLimit, self.pos));
        }
        let event = self.captures.len();
        self.captures.push(Capture {
            rule: id,
            branch: None,
            range: self.pos..self.pos,
            number: None,
            payload: None,
        });
        match &self.grammar.rules[id] {
            Rule::Empty => {}
            Rule::Byte(byte) => self.literal(&[*byte])?,
            Rule::Literal(bytes) => self.literal(bytes)?,
            Rule::Uleb128 => {
                self.captures[event].number = Some(self.number()?);
            }
            Rule::Blob | Rule::Frame(_) | Rule::U24Frame(_) => {
                let len = if matches!(self.grammar.rules[id], Rule::U24Frame(_)) {
                    let (value, bytes) =
                        decode_u24(&self.input[self.pos..self.end], &mut self.work).map_err(
                            |mut e| {
                                e.offset += self.pos;
                                e
                            },
                        )?;
                    self.pos += bytes;
                    value
                } else {
                    self.number()?
                };
                let len =
                    usize::try_from(len).map_err(|_| Error::new(ErrorKind::Overflow, self.pos))?;
                bound(
                    len,
                    self.grammar.limits.max_frame_bytes,
                    ErrorKind::PayloadLimit,
                )?;
                let start = self.pos;
                let end = add(start, len)?;
                if end > self.end {
                    return Err(Error::new(ErrorKind::UnexpectedEof, self.end));
                }
                self.captures[event].payload = Some(start..end);
                if let Rule::Frame(child) | Rule::U24Frame(child) = &self.grammar.rules[id] {
                    let previous_end = self.end;
                    self.end = end;
                    self.rule(*child, depth + 1)?;
                    if self.pos != end {
                        return Err(Error::new(ErrorKind::TrailingBytes, self.pos));
                    }
                    self.end = previous_end;
                } else {
                    charge(&mut self.work, len)?;
                    self.pos = end;
                }
            }
            Rule::Sequence(children) => {
                for &child in children {
                    self.rule(child, depth + 1)?;
                }
            }
            Rule::Repeat { rule, count } => self.repeat(*rule, *count, depth)?,
            Rule::Counted(child) => {
                let count = self.number()?;
                self.captures[event].number = Some(count);
                let count = usize::try_from(count)
                    .map_err(|_| Error::new(ErrorKind::Overflow, self.pos))?;
                self.repeat(*child, count, depth)?;
            }
            Rule::Choice(branches) => {
                let byte = *self
                    .input
                    .get(self.pos)
                    .filter(|_| self.pos < self.end)
                    .ok_or(Error::new(ErrorKind::UnexpectedEof, self.pos))?;
                let mut selected = None;
                for branch in branches {
                    charge(&mut self.work, 1)?;
                    if self.grammar.meta[branch.rule].contains(byte) {
                        selected = Some((branch.id, branch.rule));
                        break;
                    }
                }
                let (branch, child) = selected.ok_or(Error::new(ErrorKind::Mismatch, self.pos))?;
                self.captures[event].branch = Some(branch);
                self.rule(child, depth + 1)?;
            }
        }
        self.captures[event].range.end = self.pos;
        Ok(())
    }
    fn repeat(&mut self, child: usize, count: usize, depth: usize) -> Result<()> {
        if count > self.grammar.limits.max_repetitions {
            return Err(Error::new(ErrorKind::CountLimit, self.pos));
        }
        // Every child, even a nullable one, costs at least one visit/event/work
        // unit. Refuse impossible counts before iteration. Descendant visits
        // retain their own cumulative guards; this never refunds a budget.
        let visits = add(self.captures.len(), count)?;
        bound(
            visits,
            self.grammar.limits.max_events,
            ErrorKind::CountLimit,
        )?;
        if visits > self.capacity {
            return Err(Error::new(ErrorKind::AllocationLimit, self.pos));
        }
        bound(count, self.work, ErrorKind::WorkLimit)?;
        for _ in 0..count {
            self.rule(child, depth + 1)?;
        }
        Ok(())
    }
    fn literal(&mut self, bytes: &[u8]) -> Result<()> {
        charge(&mut self.work, bytes.len())?;
        let end = add(self.pos, bytes.len())?;
        if end > self.end {
            return Err(Error::new(ErrorKind::UnexpectedEof, self.end));
        }
        if self.input[self.pos..end] != *bytes {
            return Err(Error::new(ErrorKind::Mismatch, self.pos));
        }
        self.pos = end;
        Ok(())
    }
    fn number(&mut self) -> Result<u64> {
        let (value, len) = framing::decode(&self.input[self.pos..self.end], &mut self.work)
            .map_err(|mut e| {
                e.offset += self.pos;
                e
            })?;
        self.pos += len;
        Ok(value)
    }
}
