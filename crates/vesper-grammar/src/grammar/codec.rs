//! Bounded logical values for one validated acyclic grammar. Decoding borrows
//! blob payloads; encoding derives bytes from values and rules, not an input
//! transcript. Rust may still drop values: this is not source linearity or a
//! refinement proof. Logical metadata/work bounds exclude allocator bookkeeping
//! and caller-owned values/rules.
use super::{Grammar, Rule};
use crate::{Error, ErrorKind, Result, add, bound, charge, framing, mul};

#[derive(Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Unit,
    Number(u64),
    Bytes(&'a [u8]),
    Sequence(Vec<Value<'a>>),
    Choice { branch: u16, value: Box<Value<'a>> },
    Frame(Box<Value<'a>>),
    Repeat(Vec<Value<'a>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecLimits {
    pub max_values: usize,
    pub max_metadata_bytes: usize,
    /// Encoder-only wire-size bound. Decode uses the grammar's input bound.
    pub max_output_bytes: usize,
    pub max_depth: usize,
    pub max_work: usize,
}
impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_values: 4096,
            max_metadata_bytes: 512 * 1024,
            max_output_bytes: 1024 * 1024,
            max_depth: 32,
            max_work: 8 * 1024 * 1024,
        }
    }
}
impl CodecLimits {
    pub const HARD: Self = Self {
        max_values: 16384,
        max_metadata_bytes: 4 * 1024 * 1024,
        max_output_bytes: 4 * 1024 * 1024,
        max_depth: 64,
        max_work: 64 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<()> {
        let hard = Self::HARD;
        for (n, max) in [
            (self.max_values, hard.max_values),
            (self.max_metadata_bytes, hard.max_metadata_bytes),
            (self.max_output_bytes, hard.max_output_bytes),
            (self.max_depth, hard.max_depth),
            (self.max_work, hard.max_work),
        ] {
            if n == 0 || n > max {
                return Err(Error::new(ErrorKind::Rule, 0));
            }
        }
        Ok(())
    }
}

struct Budget {
    limits: CodecLimits,
    work: usize,
    metadata: usize,
    visits: usize,
    max_visits: usize,
    max_depth: usize,
}
impl Budget {
    fn new(grammar: &Grammar<'_>, limits: CodecLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            work: limits.max_work.min(grammar.limits.max_work),
            metadata: 0,
            visits: 0,
            max_visits: limits.max_values.min(grammar.limits.max_events),
            max_depth: limits.max_depth.min(grammar.limits.max_depth).min(64),
        })
    }
    fn work(&mut self, n: usize) -> Result<()> {
        charge(&mut self.work, n)
    }
    fn visit(&mut self, depth: usize) -> Result<()> {
        bound(depth, self.max_depth, ErrorKind::DepthLimit)?;
        self.visits = add(self.visits, 1)?;
        bound(self.visits, self.max_visits, ErrorKind::CountLimit)?;
        self.work(1)
    }
    fn children(&mut self, count: usize, depth: usize) -> Result<()> {
        if count != 0 {
            bound(add(depth, 1)?, self.max_depth, ErrorKind::DepthLimit)?;
        }
        bound(
            count,
            self.max_visits.saturating_sub(self.visits),
            ErrorKind::CountLimit,
        )?;
        self.work(count)
    }
    fn metadata(&mut self, bytes: usize) -> Result<()> {
        self.metadata = add(self.metadata, bytes)?;
        bound(
            self.metadata,
            self.limits.max_metadata_bytes,
            ErrorKind::AllocationLimit,
        )?;
        self.work(bytes)
    }
    fn values<'a>(&mut self, count: usize, depth: usize) -> Result<Vec<Value<'a>>> {
        self.children(count, depth)?;
        self.metadata(mul(count, size_of::<Value<'_>>())?)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        Ok(values)
    }
    fn boxed(&mut self, depth: usize) -> Result<()> {
        self.children(1, depth)?;
        self.metadata(size_of::<Value<'_>>())
    }
}

/// Decode real bytes into logical values. No Capture vector is allocated, so
/// max_capture_bytes is not consumed: max_events bounds visits and the codec's
/// metadata cap bounds the actual value vectors/boxes instead. max_output_bytes
/// is encoder-only. A successfully decoded value may need larger output/work/
/// storage budgets for serialization; there is no unconditional same-budget
/// roundtrip guarantee.
pub fn decode_exact<'a>(
    grammar: &Grammar<'_>,
    input: &'a [u8],
    limits: CodecLimits,
) -> Result<Value<'a>> {
    let budget = Budget::new(grammar, limits)?;
    bound(
        input.len(),
        grammar.limits.max_input_bytes,
        ErrorKind::InputLimit,
    )?;
    let mut reader = Reader {
        grammar,
        input,
        pos: 0,
        end: input.len(),
        budget,
    };
    let value = reader.node(grammar.root, 1)?;
    if reader.pos != input.len() {
        return Err(Error::new(ErrorKind::TrailingBytes, reader.pos));
    }
    Ok(value)
}
struct Reader<'g, 'r, 'a> {
    grammar: &'g Grammar<'r>,
    input: &'a [u8],
    pos: usize,
    end: usize,
    budget: Budget,
}
impl<'a> Reader<'_, '_, 'a> {
    fn number(&mut self) -> Result<u64> {
        let (value, bytes) =
            framing::decode(&self.input[self.pos..self.end], &mut self.budget.work).map_err(
                |mut e| {
                    e.offset += self.pos;
                    e
                },
            )?;
        self.pos += bytes;
        Ok(value)
    }
    fn literal(&mut self, bytes: &[u8]) -> Result<()> {
        self.budget.work(bytes.len())?;
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
    fn repeat(&mut self, child: usize, count: usize, depth: usize) -> Result<Value<'a>> {
        bound(
            count,
            self.grammar.limits.max_repetitions,
            ErrorKind::CountLimit,
        )?;
        let mut values = self.budget.values(count, depth)?;
        for _ in 0..count {
            values.push(self.node(child, depth + 1)?);
        }
        Ok(Value::Repeat(values))
    }
    fn node(&mut self, id: usize, depth: usize) -> Result<Value<'a>> {
        self.budget.visit(depth)?;
        match &self.grammar.rules[id] {
            Rule::Empty => Ok(Value::Unit),
            Rule::Byte(byte) => {
                self.literal(&[*byte])?;
                Ok(Value::Unit)
            }
            Rule::Literal(bytes) => {
                self.literal(bytes)?;
                Ok(Value::Unit)
            }
            Rule::Uleb128 => Ok(Value::Number(self.number()?)),
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
                if let Rule::Frame(child) = &self.grammar.rules[id] {
                    self.budget.boxed(depth)?;
                    let previous_end = self.end;
                    self.end = end;
                    let value = self.node(*child, depth + 1)?;
                    if self.pos != end {
                        return Err(Error::new(ErrorKind::TrailingBytes, self.pos));
                    }
                    self.end = previous_end;
                    Ok(Value::Frame(Box::new(value)))
                } else {
                    self.budget.work(len)?;
                    self.pos = end;
                    Ok(Value::Bytes(&self.input[start..end]))
                }
            }
            Rule::Sequence(children) => {
                let mut values = self.budget.values(children.len(), depth)?;
                for &child in children {
                    values.push(self.node(child, depth + 1)?);
                }
                Ok(Value::Sequence(values))
            }
            Rule::Repeat { rule, count } => self.repeat(*rule, *count, depth),
            Rule::Counted(child) => {
                let count = self.number()?;
                let count = usize::try_from(count)
                    .map_err(|_| Error::new(ErrorKind::Overflow, self.pos))?;
                self.repeat(*child, count, depth)
            }
            Rule::Choice(branches) => {
                let byte = *self
                    .input
                    .get(self.pos)
                    .filter(|_| self.pos < self.end)
                    .ok_or(Error::new(ErrorKind::UnexpectedEof, self.pos))?;
                let mut selected = None;
                for branch in branches {
                    self.budget.work(1)?;
                    if self.grammar.meta[branch.rule].contains(byte) {
                        selected = Some((branch.id, branch.rule));
                        break;
                    }
                }
                let (branch, child) = selected.ok_or(Error::new(ErrorKind::Mismatch, self.pos))?;
                self.budget.boxed(depth)?;
                let value = self.node(child, depth + 1)?;
                Ok(Value::Choice {
                    branch,
                    value: Box::new(value),
                })
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Write<'a> {
    Byte(u8),
    Number(u64, usize),
    Bytes(&'a [u8]),
}
struct Plan<'a> {
    writes: Vec<Write<'a>>,
    len: usize,
    budget: Budget,
}
struct Planner<'p, 'r> {
    grammar: &'p Grammar<'r>,
    plan: Plan<'p>,
}
impl<'p> Plan<'p> {
    fn push(&mut self, write: Write<'p>) -> Result<usize> {
        if self.writes.len() == self.writes.capacity() {
            let capacity = self.writes.capacity().max(2);
            let capacity = mul(capacity, 2)?;
            self.budget
                .metadata(mul(capacity, size_of::<Write<'_>>())?)?;
            self.budget
                .work(mul(self.writes.len(), size_of::<Write<'_>>())?)?;
            self.writes
                .try_reserve_exact(capacity - self.writes.len())
                .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
        }
        let index = self.writes.len();
        self.writes.push(write);
        Ok(index)
    }
}
impl<'p> Planner<'p, '_> {
    fn length(&mut self, bytes: usize) -> Result<()> {
        self.plan.len = add(self.plan.len, bytes)?;
        bound(
            self.plan.len,
            self.plan.budget.limits.max_output_bytes,
            ErrorKind::OutputLimit,
        )?;
        bound(
            self.plan.len,
            self.grammar.limits.max_input_bytes,
            ErrorKind::InputLimit,
        )
    }
    fn number(&mut self, number: u64) -> Result<()> {
        self.plan.budget.work(10)?;
        let len = framing::encoded_u64_len(number);
        self.length(len)?;
        self.plan.push(Write::Number(number, len))?;
        Ok(())
    }
    fn bytes(&mut self, bytes: &'p [u8]) -> Result<()> {
        self.length(bytes.len())?;
        if !bytes.is_empty() {
            self.plan.push(Write::Bytes(bytes))?;
        }
        Ok(())
    }
    fn repeat(&mut self, child: usize, values: &'p [Value<'_>], depth: usize) -> Result<()> {
        bound(
            values.len(),
            self.grammar.limits.max_repetitions,
            ErrorKind::CountLimit,
        )?;
        self.plan.budget.children(values.len(), depth)?;
        for value in values {
            self.node(child, value, depth + 1)?;
        }
        Ok(())
    }
    fn node(&mut self, id: usize, value: &'p Value<'_>, depth: usize) -> Result<()> {
        self.plan.budget.visit(depth)?;
        match (&self.grammar.rules[id], value) {
            (Rule::Empty, Value::Unit) => Ok(()),
            (Rule::Byte(byte), Value::Unit) => {
                self.length(1)?;
                self.plan.push(Write::Byte(*byte))?;
                Ok(())
            }
            (Rule::Literal(bytes), Value::Unit) => self.bytes(bytes),
            (Rule::Uleb128, Value::Number(value)) => self.number(*value),
            (Rule::Blob, Value::Bytes(bytes)) => {
                bound(
                    bytes.len(),
                    self.grammar.limits.max_frame_bytes,
                    ErrorKind::PayloadLimit,
                )?;
                self.number(
                    u64::try_from(bytes.len()).map_err(|_| Error::new(ErrorKind::Overflow, 0))?,
                )?;
                self.bytes(bytes)
            }
            (Rule::Frame(child), Value::Frame(value)) => {
                self.plan.budget.children(1, depth)?;
                let prefix = self.plan.push(Write::Number(0, 0))?;
                let start = self.plan.len;
                self.node(*child, value, depth + 1)?;
                let payload = self.plan.len - start;
                bound(
                    payload,
                    self.grammar.limits.max_frame_bytes,
                    ErrorKind::PayloadLimit,
                )?;
                let payload =
                    u64::try_from(payload).map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
                self.plan.budget.work(10)?;
                let len = framing::encoded_u64_len(payload);
                self.length(len)?;
                self.plan.writes[prefix] = Write::Number(payload, len);
                Ok(())
            }
            (Rule::Sequence(children), Value::Sequence(values)) => {
                if children.len() != values.len() {
                    return Err(Error::new(ErrorKind::Mismatch, 0));
                }
                self.plan.budget.children(values.len(), depth)?;
                for (&child, value) in children.iter().zip(values) {
                    self.node(child, value, depth + 1)?;
                }
                Ok(())
            }
            (Rule::Choice(branches), Value::Choice { branch, value }) => {
                let mut selected = None;
                for candidate in branches {
                    self.plan.budget.work(1)?;
                    if candidate.id == *branch {
                        selected = Some(candidate.rule);
                        break;
                    }
                }
                let child = selected.ok_or(Error::new(ErrorKind::Mismatch, 0))?;
                self.plan.budget.children(1, depth)?;
                self.node(child, value, depth + 1)
            }
            (Rule::Repeat { rule, count }, Value::Repeat(values)) => {
                if values.len() != *count {
                    return Err(Error::new(ErrorKind::Mismatch, 0));
                }
                self.repeat(*rule, values, depth)
            }
            (Rule::Counted(child), Value::Repeat(values)) => {
                bound(
                    values.len(),
                    self.grammar.limits.max_repetitions,
                    ErrorKind::CountLimit,
                )?;
                self.number(
                    u64::try_from(values.len()).map_err(|_| Error::new(ErrorKind::Overflow, 0))?,
                )?;
                self.repeat(*child, values, depth)
            }
            _ => Err(Error::new(ErrorKind::Mismatch, 0)),
        }
    }
}
fn plan<'p>(
    grammar: &'p Grammar<'_>,
    value: &'p Value<'_>,
    limits: CodecLimits,
) -> Result<Plan<'p>> {
    let mut planner = Planner {
        grammar,
        plan: Plan {
            writes: Vec::new(),
            len: 0,
            budget: Budget::new(grammar, limits)?,
        },
    };
    planner.node(grammar.root, value, 1)?;
    Ok(planner.plan)
}

/// Measure canonical bytes from semantic values. This validates the complete
/// shape and all grammar/output/plan bounds without creating an output buffer.
pub fn encoded_len(grammar: &Grammar<'_>, value: &Value<'_>, limits: CodecLimits) -> Result<usize> {
    Ok(plan(grammar, value, limits)?.len)
}

/// Every returned failure preserves the whole caller output. Successful writes
/// preserve its suffix. Plan/scratch storage and all copying work are prepaid;
/// physical allocator behavior/RSS are not certified by these logical caps.
pub fn encode_into(
    grammar: &Grammar<'_>,
    value: &Value<'_>,
    output: &mut [u8],
    limits: CodecLimits,
) -> Result<usize> {
    let mut plan = plan(grammar, value, limits)?;
    if output.len() < plan.len {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    plan.budget.metadata(plan.len)?;
    // Scratch zero initialization, writes, final copy, and write dispatch.
    plan.budget
        .work(add(mul(plan.len, 3)?, plan.writes.len())?)?;
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(plan.len)
        .map_err(|_| Error::new(ErrorKind::AllocationFailed, 0))?;
    scratch.resize(plan.len, 0);
    let mut offset = 0usize;
    for write in plan.writes {
        match write {
            Write::Byte(byte) => {
                let out = scratch
                    .get_mut(offset)
                    .ok_or(Error::new(ErrorKind::Rule, 0))?;
                *out = byte;
                offset += 1;
            }
            Write::Number(mut number, len) => {
                let end = add(offset, len)?;
                let bytes = scratch
                    .get_mut(offset..end)
                    .ok_or(Error::new(ErrorKind::Rule, 0))?;
                for byte in bytes {
                    *byte = (number as u8 & 127) | if number >= 128 { 128 } else { 0 };
                    number >>= 7;
                }
                offset = end;
            }
            Write::Bytes(bytes) => {
                let end = add(offset, bytes.len())?;
                let out = scratch
                    .get_mut(offset..end)
                    .ok_or(Error::new(ErrorKind::Rule, 0))?;
                out.copy_from_slice(bytes);
                offset = end;
            }
        }
    }
    if offset != plan.len {
        return Err(Error::new(ErrorKind::Rule, 0));
    }
    output[..plan.len].copy_from_slice(&scratch);
    Ok(plan.len)
}
