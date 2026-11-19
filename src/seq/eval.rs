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
    /// A string operation that replaces its receiver (argument 0): the
    /// caller writes the new string (first) back to the receiver's place;
    /// the second value is the operation's result.
    Update(Value, Value),
}

pub(crate) fn index(v: &Value) -> Result<u64, String> {
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
        if let SeqOp::Str(op) = op {
            return self.str_op(op, &args, site);
        }
        if op == SeqOp::SubSlice && matches!(args.first(), Some(Value::String(_))) {
            return self.str_op(crate::strings::StrOp::SubStr, &args, site);
        }
        // A hoisted access is its checked access under a guard that held:
        // the reference semantics of `crate::bounds_hoist`.
        if let SeqOp::Hoisted(access) = op {
            let slice = matches!(args.first(), Some(Value::Slice { .. }));
            return match self.seq_op(access.checked(slice), args, site)? {
                OutOfBounds => Err(format!(
                    "hoisted ChunkSeq::{} is out of bounds: its guard was wrong",
                    op.name()
                )),
                outcome => Ok(outcome),
            };
        }
        if let SeqOp::BoundsGuard { inclusive } = op {
            let lo = index(arg(0)?)?;
            let hi = index(arg(1)?)?;
            let mut inside = true;
            for pair in args.get(2..).unwrap_or_default().chunks(2) {
                let [s, offset] = pair else {
                    return Err("bounds guard needs a sequence and an offset".into());
                };
                let len = u128::from(view(s)?.2 as u64);
                let end = u128::from(index(offset)?) + u128::from(hi);
                inside &= if inclusive { end < len } else { end <= len };
            }
            let empty = if inclusive { lo > hi } else { lo >= hi };
            return Ok(V(Value::Bool(empty || inside)));
        }
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
            SeqOp::Reserve => {
                // A capacity hint: the evaluator's vector grows on demand.
                index(arg(1)?)?;
                seq(arg(0)?)?;
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
            SeqOp::Vector(io) => {
                let (cell, start, len) = view(arg(0)?)?;
                let at = index(arg(1)?)?;
                let mut items = cell.borrow_mut();
                match crate::vector::slice_io(io, &mut items[start..start + len], at, args.get(2))?
                {
                    Some(v) => V(v),
                    None => OutOfBounds,
                }
            }
            SeqOp::CopyFrom { total } => {
                let (cell, start, len) = view(arg(0)?)?;
                let at = index(arg(1)?)?;
                let (src, from, n) = view(arg(2)?)?;
                if !fits(len, at, n) {
                    return Ok(if total {
                        V(Value::Bool(false))
                    } else {
                        OutOfBounds
                    });
                }
                // Copy out first: the source may be a view of the same cell.
                let items = src.borrow()[from..from + n].to_vec();
                let at = start + at as usize;
                cell.borrow_mut()[at..at + n].clone_from_slice(&items);
                V(if total {
                    Value::Bool(true)
                } else {
                    Value::Tuple(vec![])
                })
            }
            SeqOp::CopyWithin { total } => {
                let (cell, start, len) = view(arg(0)?)?;
                let lo = index(arg(1)?)?;
                let hi = index(arg(2)?)?;
                let at = index(arg(3)?)?;
                if lo > hi || hi > len as u64 || !fits(len, at, (hi - lo) as usize) {
                    return Ok(if total {
                        V(Value::Bool(false))
                    } else {
                        OutOfBounds
                    });
                }
                let (lo, hi, at) = (
                    start + lo as usize,
                    start + hi as usize,
                    start + at as usize,
                );
                // `memmove`: the source and destination may overlap.
                let items = cell.borrow()[lo..hi].to_vec();
                cell.borrow_mut()[at..at + items.len()].clone_from_slice(&items);
                V(if total {
                    Value::Bool(true)
                } else {
                    Value::Tuple(vec![])
                })
            }
            SeqOp::Fill => {
                let (cell, start, len) = view(arg(0)?)?;
                let value = arg(1)?.clone();
                cell.borrow_mut()[start..start + len].fill(value);
                V(Value::Tuple(vec![]))
            }
            SeqOp::Wide(wide) => return self.wide_op(wide, &args, site),
            SeqOp::SumBe16 => {
                let bytes = byte_view(arg(0)?)?;
                let mut sum: u64 = 0;
                for pair in bytes.chunks(2) {
                    let word = u64::from(pair[0]) << 8 | u64::from(*pair.get(1).unwrap_or(&0));
                    sum = sum.wrapping_add(word);
                }
                V(Value::UInt(sum, 64))
            }
            SeqOp::BytesEq => V(Value::Bool(byte_view(arg(0)?)? == byte_view(arg(1)?)?)),
            SeqOp::EqIgnoreAsciiCase => V(Value::Bool(
                byte_view(arg(0)?)?.eq_ignore_ascii_case(&byte_view(arg(1)?)?),
            )),
            SeqOp::ExtendAsciiLowercase => {
                let bytes = byte_view(arg(1)?)?;
                seq(arg(0)?)?.borrow_mut().extend(
                    bytes
                        .iter()
                        .map(|b| Value::UInt(u64::from(b.to_ascii_lowercase()), 8)),
                );
                V(Value::Tuple(vec![]))
            }
            SeqOp::MakeAsciiLowercase { total } => {
                let (cell, start, len) = view(arg(0)?)?;
                let lo = index(arg(1)?)?;
                let hi = index(arg(2)?)?;
                if lo > hi || hi > len as u64 {
                    return Ok(if total {
                        V(Value::Bool(false))
                    } else {
                        OutOfBounds
                    });
                }
                for v in &mut cell.borrow_mut()[start + lo as usize..start + hi as usize] {
                    match v {
                        Value::UInt(b, _) if (u64::from(b'A')..=u64::from(b'Z')).contains(b) => {
                            *b |= 0x20
                        }
                        Value::Int(b) if (i64::from(b'A')..=i64::from(b'Z')).contains(b) => {
                            *b |= 0x20
                        }
                        _ => {}
                    }
                }
                V(if total {
                    Value::Bool(true)
                } else {
                    Value::Tuple(vec![])
                })
            }
            SeqOp::FindByte | SeqOp::FindEither | SeqOp::FindNotIn => {
                let bytes = byte_view(arg(0)?)?;
                let found = match op {
                    SeqOp::FindByte => {
                        let b = index(arg(1)?)?;
                        bytes.iter().position(|&x| u64::from(x) == b)
                    }
                    SeqOp::FindEither => {
                        let a = index(arg(1)?)?;
                        let b = index(arg(2)?)?;
                        bytes
                            .iter()
                            .position(|&x| u64::from(x) == a || u64::from(x) == b)
                    }
                    _ => {
                        let lo = index(arg(1)?)?;
                        let hi = index(arg(2)?)?;
                        let high = matches!(arg(3)?, Value::Bool(true));
                        bytes
                            .iter()
                            .position(|&x| !byte_class_holds(lo, hi, high, x))
                    }
                };
                let (constructor, fields) = match found {
                    Some(i) => ("Some", BTreeMap::from([("0".to_string(), u64_value(i))])),
                    None => ("None", BTreeMap::new()),
                };
                V(self.construct_data(
                    "Option".into(),
                    constructor.into(),
                    fields,
                    Some(site.clone()),
                )?)
            }
            SeqOp::Str(_) | SeqOp::Hoisted(_) | SeqOp::BoundsGuard { .. } => {
                unreachable!("string and hoisted operations return above")
            }
            SeqOp::Swap | SeqOp::Replace => {
                return Err(format!(
                    "{} writes places; the evaluator runs it at its instruction",
                    op.name()
                ));
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

/// The bytes of a `ChunkSeq[u8]` or `Slice[u8]` value.
fn byte_view(v: &Value) -> Result<Vec<u8>, String> {
    let (cell, start, len) = view(v)?;
    let items = cell.borrow();
    items[start..start + len]
        .iter()
        .map(|b| index(b).map(|b| b as u8))
        .collect()
}

/// Whether byte `x` is in the class of `find_not_in(lo, hi, high)`: bit `x`
/// of `lo` for `x < 64`, bit `x - 64` of `hi` for `x < 128`, else `high`.
pub(crate) fn byte_class_holds(lo: u64, hi: u64, high: bool, x: u8) -> bool {
    match x {
        0..=63 => lo >> x & 1 == 1,
        64..=127 => hi >> (x - 64) & 1 == 1,
        _ => high,
    }
}

/// Whether `n` elements starting at `at` fit in `len`.
fn fits(len: usize, at: u64, n: usize) -> bool {
    at <= len as u64 && n as u64 <= len as u64 - at
}

impl Runtime<'_> {
    /// `read_*`, `write_*` and their total `try_` forms on a byte sequence:
    /// the reference semantics of one bounds check for the whole width.
    fn wide_op(
        &mut self,
        wide: crate::seq::Wide,
        args: &[Value],
        site: &crate::ast::Span,
    ) -> Result<SeqOutcome, String> {
        use SeqOutcome::{OutOfBounds, Value as V};
        let arg = |i: usize| {
            args.get(i)
                .ok_or_else(|| format!("ChunkSeq::{} is missing an operand", wide.name()))
        };
        let (cell, start, len) = view(arg(0)?)?;
        let at = index(arg(1)?)?;
        let n = usize::from(wide.bytes);
        let bits = u32::from(wide.bytes) * 8;
        let inside = fits(len, at, n);
        // Only an in-bounds offset is made absolute: `at` may be near
        // `u64::MAX`.
        let at = if inside { start + at as usize } else { 0 };
        if wide.write {
            if inside {
                let v = index(arg(2)?)?;
                let bytes = v.to_le_bytes();
                let mut items = cell.borrow_mut();
                for k in 0..n {
                    let b = if wide.big { bytes[n - 1 - k] } else { bytes[k] };
                    items[at + k] = Value::UInt(u64::from(b), 8);
                }
            }
            return Ok(match (wide.total, inside) {
                (true, _) => V(Value::Bool(inside)),
                (false, true) => V(Value::Tuple(vec![])),
                (false, false) => OutOfBounds,
            });
        }
        let read = if inside {
            let items = cell.borrow();
            let mut v: u64 = 0;
            for k in 0..n {
                let b = index(&items[if wide.big { at + k } else { at + n - 1 - k }])?;
                v = (v << 8) | (b & 0xff);
            }
            Some(Value::UInt(v, bits))
        } else {
            None
        };
        Ok(match (wide.total, read) {
            (false, Some(v)) => V(v),
            (false, None) => OutOfBounds,
            (true, read) => {
                let (constructor, fields) = match read {
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
        })
    }

    /// A string primitive (§13 strings). A string and a view are both a
    /// Rust `String` here: a view is a copy of its bytes, which no
    /// operation can tell apart from a borrow because views are read-only.
    fn str_op(
        &mut self,
        op: crate::strings::StrOp,
        args: &[Value],
        site: &crate::ast::Span,
    ) -> Result<SeqOutcome, String> {
        use crate::strings::StrOp;
        use SeqOutcome::{OutOfBounds, Update, Value as V};
        let arg = |i: usize| {
            args.get(i)
                .ok_or_else(|| format!("String::{} is missing an operand", op.name()))
        };
        let text = |v: &Value| match v {
            Value::String(s) => Ok(s.clone()),
            _ => Err(format!("String::{} expects a string operand", op.name())),
        };
        let s = text(arg(0)?)?;
        Ok(match op {
            StrOp::Len => V(u64_value(s.len())),
            StrOp::ByteAt | StrOp::TryByteAt => {
                let i = index(arg(1)?)?;
                let byte = usize::try_from(i)
                    .ok()
                    .and_then(|i| s.as_bytes().get(i).copied());
                match (op, byte) {
                    (StrOp::ByteAt, Some(b)) => V(Value::UInt(u64::from(b), 8)),
                    (StrOp::ByteAt, None) => OutOfBounds,
                    (_, b) => {
                        let (constructor, fields) = match b {
                            Some(b) => (
                                "Some",
                                BTreeMap::from([("0".to_string(), Value::UInt(u64::from(b), 8))]),
                            ),
                            None => ("None", BTreeMap::new()),
                        };
                        V(self.construct_data(
                            "Option".into(),
                            constructor.into(),
                            fields,
                            Some(site.clone()),
                        )?)
                    }
                }
            }
            StrOp::AsStr => V(Value::String(s)),
            StrOp::AsBytes => {
                let bytes: Vec<Value> = s.bytes().map(|b| Value::UInt(u64::from(b), 8)).collect();
                let len = bytes.len();
                V(Value::Slice {
                    seq: Rc::new(RefCell::new(bytes)),
                    start: 0,
                    len,
                })
            }
            StrOp::SubStr => {
                let lo = index(arg(1)?)?;
                let hi = index(arg(2)?)?;
                let len = s.len() as u64;
                if lo > hi || hi > len {
                    return Err(format!(
                        "String fault: interval {lo}..{hi} is out of bounds of length {len}"
                    ));
                }
                match s.get(lo as usize..hi as usize) {
                    Some(view) => V(Value::String(view.to_string())),
                    None => {
                        return Err(format!(
                            "String fault: interval {lo}..{hi} is not on char boundaries"
                        ));
                    }
                }
            }
            StrOp::PushStr => Update(Value::String(s + &text(arg(1)?)?), Value::Tuple(vec![])),
            StrOp::PushChar => {
                let c = index(arg(1)?)?;
                match u32::try_from(c).ok().and_then(char::from_u32) {
                    Some(c) => {
                        let mut s = s;
                        s.push(c);
                        Update(Value::String(s), Value::Bool(true))
                    }
                    None => V(Value::Bool(false)),
                }
            }
            StrOp::Clear => Update(Value::String(String::new()), Value::Tuple(vec![])),
            StrOp::Reserve => {
                let n = usize::try_from(index(arg(1)?)?)
                    .map_err(|_| "String capacity overflows the address space")?;
                let mut s = s;
                s.try_reserve(n)
                    .map_err(|_| "String out of memory: capacity overflows the address space")?;
                Update(Value::String(s), Value::Tuple(vec![]))
            }
        })
    }
}
