//! CEK evaluator support for `ChunkSeq[T]` and `Slice[T]` (§13).
//!
//! A sequence is its element values behind a shared cell, so a moved or
//! borrowed handle observes every update. A slice is the same cell with an
//! interval `[start, start + len)`.
use crate::{
    runtime::{Runtime, Value},
    seq::SeqOp,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub type SeqCell = Rc<RefCell<Vec<Value>>>;

/// The outcome of a sequence operation: a value, or an
/// `IndexOutOfBounds` failure for the caller to raise.
pub(crate) enum SeqOutcome {
    Value(Value),
    OutOfBounds,
}

fn index(v: &Value) -> Result<u64, String> {
    match v {
        Value::UInt(n, _) => Ok(*n),
        Value::Int(n) => u64::try_from(*n).map_err(|_| "sequence index is negative".into()),
        _ => Err("sequence index must be an integer".into()),
    }
}

fn u64_value(n: usize) -> Value {
    Value::UInt(n as u64, 64)
}

/// The cell and interval of a sequence or slice value.
fn view(v: &Value) -> Result<(SeqCell, usize, usize), String> {
    match v {
        Value::Seq(cell) => {
            let len = cell.borrow().len();
            Ok((cell.clone(), 0, len))
        }
        Value::Slice { seq, start, len } => Ok((seq.clone(), *start, *len)),
        _ => Err("expected a ChunkSeq or Slice value".into()),
    }
}

fn seq(v: &Value) -> Result<&SeqCell, String> {
    match v {
        Value::Seq(cell) => Ok(cell),
        _ => Err("expected a ChunkSeq value".into()),
    }
}

impl Runtime<'_> {
    pub(crate) fn seq_op(
        &mut self,
        op: SeqOp,
        args: Vec<Value>,
        site: &crate::ast::Span,
    ) -> Result<SeqOutcome, String> {
        use SeqOutcome::{OutOfBounds, Value as V};
        let arg = |i: usize| {
            args.get(i)
                .ok_or_else(|| format!("ChunkSeq::{} is missing an operand", op.name()))
        };
        Ok(match op {
            SeqOp::New => V(Value::Seq(Rc::new(RefCell::new(Vec::new())))),
            SeqOp::WithCapacity => {
                let cap = usize::try_from(index(arg(0)?)?)
                    .map_err(|_| "ChunkSeq capacity overflows the address space")?;
                let mut items = Vec::new();
                items
                    .try_reserve(cap)
                    .map_err(|_| "ChunkSeq out of memory: capacity overflows the address space")?;
                V(Value::Seq(Rc::new(RefCell::new(items))))
            }
            SeqOp::Len => V(u64_value(seq(arg(0)?)?.borrow().len())),
            SeqOp::SliceLen => V(u64_value(view(arg(0)?)?.2)),
            SeqOp::Push => {
                let value = arg(1)?.clone();
                seq(arg(0)?)?.borrow_mut().push(value);
                V(Value::Tuple(vec![]))
            }
            SeqOp::Pop => {
                let popped = seq(arg(0)?)?.borrow_mut().pop();
                let (constructor, fields) = match popped {
                    Some(v) => ("Some", BTreeMap::from([("0".to_string(), v)])),
                    None => ("None", BTreeMap::new()),
                };
                V(self.construct_data(
                    "Option".into(),
                    constructor.into(),
                    fields,
                    Some(site.clone()),
                )?)
            }
            SeqOp::Get | SeqOp::SliceGet => {
                let (cell, start, len) = view(arg(0)?)?;
                let i = index(arg(1)?)?;
                match usize::try_from(i).ok().filter(|i| *i < len) {
                    Some(i) => V(cell.borrow()[start + i].clone()),
                    None => OutOfBounds,
                }
            }
            SeqOp::Set | SeqOp::SliceSet => {
                let (cell, start, len) = view(arg(0)?)?;
                let i = index(arg(1)?)?;
                match usize::try_from(i).ok().filter(|i| *i < len) {
                    Some(i) => {
                        cell.borrow_mut()[start + i] = arg(2)?.clone();
                        V(Value::Tuple(vec![]))
                    }
                    None => OutOfBounds,
                }
            }
            SeqOp::TryGet => {
                let (cell, start, len) = view(arg(0)?)?;
                let i = index(arg(1)?)?;
                let item = usize::try_from(i)
                    .ok()
                    .filter(|i| *i < len)
                    .map(|i| cell.borrow()[start + i].clone());
                let (constructor, fields) = match item {
                    Some(v) => ("Some", BTreeMap::from([("0".to_string(), v)])),
                    None => ("None", BTreeMap::new()),
                };
                V(self.construct_data(
                    "Option".into(),
                    constructor.into(),
                    fields,
                    Some(site.clone()),
                )?)
            }
            SeqOp::TrySet => {
                let (cell, start, len) = view(arg(0)?)?;
                let i = index(arg(1)?)?;
                match usize::try_from(i).ok().filter(|i| *i < len) {
                    Some(i) => {
                        cell.borrow_mut()[start + i] = arg(2)?.clone();
                        V(Value::Bool(true))
                    }
                    None => V(Value::Bool(false)),
                }
            }
            SeqOp::Clear => {
                seq(arg(0)?)?.borrow_mut().clear();
                V(Value::Tuple(vec![]))
            }
            SeqOp::Truncate => {
                let n = usize::try_from(index(arg(1)?)?).unwrap_or(usize::MAX);
                seq(arg(0)?)?.borrow_mut().truncate(n);
                V(Value::Tuple(vec![]))
            }
            SeqOp::Extend => {
                let (src, start, len) = view(arg(1)?)?;
                let items = src.borrow()[start..start + len].to_vec();
                seq(arg(0)?)?.borrow_mut().extend(items);
                V(Value::Tuple(vec![]))
            }
            SeqOp::Finish => {
                if seq(arg(0)?)?.borrow().is_empty() {
                    V(Value::Tuple(vec![]))
                } else {
                    OutOfBounds
                }
            }
            SeqOp::AsSlice => {
                let (cell, start, len) = view(arg(0)?)?;
                V(Value::Slice {
                    seq: cell,
                    start,
                    len,
                })
            }
            SeqOp::SubSlice => {
                let (cell, start, len) = view(arg(0)?)?;
                let lo = index(arg(1)?)?;
                let hi = index(arg(2)?)?;
                if lo <= hi && hi <= len as u64 {
                    V(Value::Slice {
                        seq: cell,
                        start: start + lo as usize,
                        len: (hi - lo) as usize,
                    })
                } else {
                    return Err(format!(
                        "ChunkSeq fault: slice interval {lo}..{hi} is out of bounds of length {len}"
                    ));
                }
            }
        })
    }
}
