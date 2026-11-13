//! Executable acyclic byte-FIRST grammar subset, not a source or general PEG
//! compiler. FIRST/nullable information is derived from actual rules. Choices
//! require nonnullable, disjoint alternatives; FOLLOW-dependent epsilon choices
//! are explicitly unsupported. All rule references, including unreachable ones,
//! are validated. Shared DAG rules are legal; recursion is not.
pub mod codec;

use crate::framing::{self, Remainder};
use crate::{Error, ErrorKind, Result, add, bound, charge, mul};
use core::ops::Range;

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
    Sequence(Vec<usize>),
    Choice(Vec<Branch>),
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
impl Meta {
    fn contains(self, byte: u8) -> bool {
        self.first[usize::from(byte) / 64] & (1 << (byte % 64)) != 0
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
    meta: Vec<Meta>,
    limits: Limits,
}
impl<'g> Grammar<'g> {
    /// Borrows rule owners, and allocates one bounded metadata vector. Invalid
    /// rules never consume or modify the caller's rule definitions.
    pub fn validate(rules: &'g [Rule], root: usize, limits: Limits) -> Result<Self> {
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
                Rule::Frame(child) => (Some(*child), &[], &[]),
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
            build(id, rules, &mut meta, &mut work, limits.max_depth.min(64), 1)?;
        }
        Ok(Self {
            rules,
            root,
            meta,
            limits,
        })
    }
    pub fn nullable(&self, rule: usize) -> Option<bool> {
        self.meta.get(rule).map(|m| m.nullable)
    }
    pub fn first_contains(&self, rule: usize, byte: u8) -> Option<bool> {
        self.meta.get(rule).map(|m| m.contains(byte))
    }
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
    pub fn parse_exact(&self, input: &[u8]) -> Result<Vec<Capture>> {
        let parsed = self.parse_prefix(input)?;
        parsed.tail.assert_exact_eof().map_err(|e| e.error)?;
        Ok(parsed.captures)
    }
}
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
        Rule::Byte(byte) => result.first[usize::from(*byte) / 64] |= 1 << (byte % 64),
        Rule::Literal(bytes) => {
            if let Some(byte) = bytes.first() {
                result.first[usize::from(*byte) / 64] |= 1 << (byte % 64);
            } else {
                result.nullable = true;
            }
        }
        Rule::Uleb128 | Rule::Blob => result.first = [u64::MAX; 4],
        Rule::Frame(child) => {
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
struct Run<'g, 'r, 'a> {
    grammar: &'g Grammar<'r>,
    input: &'a [u8],
    pos: usize,
    end: usize,
    work: usize,
    captures: Vec<Capture>,
    capacity: usize,
}
impl Run<'_, '_, '_> {
    fn rule(&mut self, id: usize, depth: usize) -> Result<()> {
        charge(&mut self.work, 1)?;
        bound(
            depth,
            self.grammar.limits.max_depth.min(64),
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
            Rule::Blob | Rule::Frame(_) => {
                let len = self.number()?;
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
                if let Rule::Frame(child) = &self.grammar.rules[id] {
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
