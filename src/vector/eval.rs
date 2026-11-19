//! Reference evaluator semantics of every vector operation. The native
//! backend is differential-tested against these definitions.
//!
//! A vector value is a `Value::Tuple` of its lanes (`UInt` for unsigned
//! lanes, `Int` for `i64`, `Float32` for `f32`, `Float` for `f64`); a mask
//! is a tuple of `Bool`s. Integer lanes wrap at their width. Float lanes use
//! IEEE 754 binary32 or binary64 at their own width (`mul_add` rounds once);
//! `min`/`max` return NaN when either lane is NaN and order -0.0 below +0.0
//! (Cranelift's `fmin`/`fmax`). Shift amounts are taken modulo the lane
//! width. Reductions fold lanes in index order.
use super::{
    BinOp, CmpOp, Conv, Elem, Intrinsic, MaskOp, ReduceOp, Shape, ShiftOp, SliceIo, UnaryOp,
    VectorOp,
};
use crate::runtime::Value;

fn raw(elem: Elem, v: &Value) -> Result<u64, String> {
    Ok(match (elem, v) {
        (Elem::F64, Value::Float(f)) => f.to_bits(),
        (Elem::F64, _) => return Err("f64 vector lane requires a float".into()),
        (Elem::F32, Value::Float32(f)) => u64::from(f.to_bits()),
        (Elem::F32, _) => return Err("f32 vector lane requires an f32".into()),
        (_, Value::Int(n)) => *n as u64 & elem.mask(),
        (_, Value::UInt(n, _)) => *n & elem.mask(),
        _ => return Err("integer vector lane requires an integer".into()),
    })
}
fn lane_value(elem: Elem, bits: u64) -> Value {
    match elem {
        Elem::F64 => Value::Float(f64::from_bits(bits)),
        Elem::F32 => Value::Float32(f32::from_bits(bits as u32)),
        Elem::I64 => Value::Int(bits as i64),
        e => Value::UInt(bits & e.mask(), e.bits()),
    }
}
/// A float lane's value, widened exactly to `f64` (`f32` lanes).
fn float_of(elem: Elem, bits: u64) -> f64 {
    match elem {
        Elem::F32 => f64::from(f32::from_bits(bits as u32)),
        _ => f64::from_bits(bits),
    }
}
fn vector(shape: Shape, v: &Value) -> Result<Vec<u64>, String> {
    match v {
        Value::Tuple(lanes) if lanes.len() == usize::from(shape.lanes) => {
            lanes.iter().map(|l| raw(shape.elem, l)).collect()
        }
        _ => Err(format!("expected a Vec{shape} value")),
    }
}
fn mask(shape: Shape, v: &Value) -> Result<Vec<bool>, String> {
    match v {
        Value::Tuple(lanes) if lanes.len() == usize::from(shape.lanes) => lanes
            .iter()
            .map(|l| match l {
                Value::Bool(b) => Ok(*b),
                _ => Err("mask lane requires a bool".to_string()),
            })
            .collect(),
        _ => Err(format!("expected a Mask{shape} value")),
    }
}
fn make_vector(shape: Shape, lanes: impl IntoIterator<Item = u64>) -> Value {
    Value::Tuple(
        lanes
            .into_iter()
            .map(|l| lane_value(shape.elem, l))
            .collect(),
    )
}
fn make_mask(lanes: impl IntoIterator<Item = bool>) -> Value {
    Value::Tuple(lanes.into_iter().map(Value::Bool).collect())
}
fn u64_arg(v: &Value) -> Result<u64, String> {
    match v {
        Value::Int(n) => Ok(*n as u64),
        Value::UInt(n, _) => Ok(*n),
        _ => Err("expected a u64 operand".into()),
    }
}
fn float_min(a: f64, b: f64, min: bool) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == 0.0 && b == 0.0 {
        // Distinguish the zeros: -0.0 < +0.0.
        let negative = if min {
            a.is_sign_negative() || b.is_sign_negative()
        } else {
            a.is_sign_negative() && b.is_sign_negative()
        };
        return if negative { -0.0 } else { 0.0 };
    }
    if (a < b) == min { a } else { b }
}
fn binary(op: BinOp, elem: Elem, a: u64, b: u64) -> u64 {
    if elem == Elem::F32 {
        let (x, y) = (f32::from_bits(a as u32), f32::from_bits(b as u32));
        let r = match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => x / y,
            // min and max pick an operand (or NaN): exact through f64.
            BinOp::Min => float_min(f64::from(x), f64::from(y), true) as f32,
            BinOp::Max => float_min(f64::from(x), f64::from(y), false) as f32,
            _ => f32::NAN,
        };
        return u64::from(r.to_bits());
    }
    if elem.is_float() {
        let (x, y) = (f64::from_bits(a), f64::from_bits(b));
        return match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => x / y,
            BinOp::Min => float_min(x, y, true),
            BinOp::Max => float_min(x, y, false),
            _ => f64::NAN,
        }
        .to_bits();
    }
    let m = elem.mask();
    let less = |a: u64, b: u64| {
        if elem.is_signed() {
            (a as i64) < (b as i64)
        } else {
            a < b
        }
    };
    match op {
        BinOp::Add => a.wrapping_add(b) & m,
        BinOp::Sub => a.wrapping_sub(b) & m,
        BinOp::Mul => a.wrapping_mul(b) & m,
        BinOp::Div => 0,
        BinOp::Min => {
            if less(b, a) {
                b
            } else {
                a
            }
        }
        BinOp::Max => {
            if less(a, b) {
                b
            } else {
                a
            }
        }
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::SaturatingAdd => (a + b).min(m),
        BinOp::SaturatingSub => a.saturating_sub(b),
    }
}
fn compare(op: CmpOp, elem: Elem, a: u64, b: u64) -> bool {
    use std::cmp::Ordering;
    let order = if elem.is_float() {
        float_of(elem, a).partial_cmp(&float_of(elem, b))
    } else if elem.is_signed() {
        Some((a as i64).cmp(&(b as i64)))
    } else {
        Some(a.cmp(&b))
    };
    match (op, order) {
        (CmpOp::Ne, None) => true,
        (_, None) => false,
        (CmpOp::Eq, Some(o)) => o == Ordering::Equal,
        (CmpOp::Ne, Some(o)) => o != Ordering::Equal,
        (CmpOp::Lt, Some(o)) => o == Ordering::Less,
        (CmpOp::Le, Some(o)) => o != Ordering::Greater,
        (CmpOp::Gt, Some(o)) => o == Ordering::Greater,
        (CmpOp::Ge, Some(o)) => o != Ordering::Less,
    }
}
fn unary(op: UnaryOp, elem: Elem, a: u64) -> u64 {
    if elem == Elem::F32 {
        let x = f32::from_bits(a as u32);
        let r = match op {
            UnaryOp::Sqrt => x.sqrt(),
            UnaryOp::Abs => x.abs(),
            UnaryOp::Neg => -x,
        };
        return u64::from(r.to_bits());
    }
    let x = f64::from_bits(a);
    match op {
        UnaryOp::Sqrt => x.sqrt(),
        UnaryOp::Abs => x.abs(),
        UnaryOp::Neg => -x,
    }
    .to_bits()
}
/// `a * b + c` rounded once (IEEE fusedMultiplyAdd).
fn mul_add(elem: Elem, a: u64, b: u64, c: u64) -> u64 {
    if elem == Elem::F32 {
        let [x, y, z] = [a, b, c].map(|v| f32::from_bits(v as u32));
        return u64::from(x.mul_add(y, z).to_bits());
    }
    let [x, y, z] = [a, b, c].map(f64::from_bits);
    x.mul_add(y, z).to_bits()
}
fn convert(conv: Conv, a: u64) -> u64 {
    match conv {
        Conv::ToBits | Conv::FromBits => a,
        Conv::Widen => f64::from(f32::from_bits(a as u32)).to_bits(),
        Conv::Narrow => u64::from((f64::from_bits(a) as f32).to_bits()),
    }
}
fn string_bytes(v: &Value) -> Result<&[u8], String> {
    match v {
        Value::String(s) => Ok(s.as_bytes()),
        _ => Err("expected a String operand".into()),
    }
}
/// Lane `i` is in range iff `offset + i < len`, without overflow.
fn in_range(offset: u64, len: u64, i: u64) -> bool {
    offset < len && i < len - offset
}

/// Evaluate a vector operation on its IR operands.
pub fn evaluate(op: &VectorOp, args: &[Value]) -> Result<Value, String> {
    let (intrinsic, shape, constants) = match op {
        VectorOp::IsaSupported(p) => return Ok(Value::Bool(super::host_enabled(*p))),
        VectorOp::Lanes {
            intrinsic,
            shape,
            constants,
        } => (*intrinsic, *shape, constants.as_slice()),
    };
    let elem = shape.elem;
    let n = usize::from(shape.lanes);
    let arg = |i: usize| {
        args.get(i)
            .ok_or_else(|| format!("vector operation lacks operand {i}"))
    };
    let constant = |i: usize| {
        constants
            .get(i)
            .map(|c| usize::from(*c))
            .filter(|c| *c < n)
            .ok_or_else(|| "vector lane index out of range".to_string())
    };
    use Intrinsic as I;
    Ok(match intrinsic {
        I::Splat => make_vector(shape, vec![raw(elem, arg(0)?)?; n]),
        I::From => {
            if args.len() != n {
                return Err("Vec::from lane count mismatch".into());
            }
            make_vector(
                shape,
                args.iter()
                    .map(|v| raw(elem, v))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        I::Binary(op) => {
            let (a, b) = (vector(shape, arg(0)?)?, vector(shape, arg(1)?)?);
            make_vector(
                shape,
                a.into_iter().zip(b).map(|(x, y)| binary(op, elem, x, y)),
            )
        }
        I::Unary(op) => make_vector(
            shape,
            vector(shape, arg(0)?)?
                .into_iter()
                .map(|x| unary(op, elem, x)),
        ),
        I::MulAdd => {
            let (a, b, c) = (
                vector(shape, arg(0)?)?,
                vector(shape, arg(1)?)?,
                vector(shape, arg(2)?)?,
            );
            make_vector(
                shape,
                a.into_iter()
                    .zip(b)
                    .zip(c)
                    .map(|((x, y), z)| mul_add(elem, x, y, z)),
            )
        }
        I::Convert(conv) => {
            let target = conv
                .result(shape)
                .ok_or("vector conversion has no valid result shape")?;
            make_vector(
                target,
                vector(shape, arg(0)?)?
                    .into_iter()
                    .map(|x| convert(conv, x)),
            )
        }
        I::Not => make_vector(
            shape,
            vector(shape, arg(0)?)?
                .into_iter()
                .map(|x| !x & elem.mask()),
        ),
        I::Shift(op) => {
            let k = (u64_arg(arg(1)?)? % u64::from(elem.bits())) as u32;
            make_vector(
                shape,
                vector(shape, arg(0)?)?.into_iter().map(|x| match op {
                    ShiftOp::Shl => (x << k) & elem.mask(),
                    ShiftOp::Shr if elem.is_signed() => ((x as i64) >> k) as u64,
                    ShiftOp::Shr => x >> k,
                }),
            )
        }
        I::Compare(op) => {
            let (a, b) = (vector(shape, arg(0)?)?, vector(shape, arg(1)?)?);
            make_mask(a.into_iter().zip(b).map(|(x, y)| compare(op, elem, x, y)))
        }
        I::Select => {
            let m = mask(shape, arg(0)?)?;
            let (a, b) = (vector(shape, arg(1)?)?, vector(shape, arg(2)?)?);
            make_vector(
                shape,
                m.into_iter()
                    .zip(a.into_iter().zip(b))
                    .map(|(m, (x, y))| if m { x } else { y }),
            )
        }
        I::MaskBinary(op) => {
            let (a, b) = (mask(shape, arg(0)?)?, mask(shape, arg(1)?)?);
            make_mask(a.into_iter().zip(b).map(|(x, y)| match op {
                MaskOp::And => x & y,
                MaskOp::Or => x | y,
                MaskOp::Xor => x ^ y,
            }))
        }
        I::MaskNot => make_mask(mask(shape, arg(0)?)?.into_iter().map(|x| !x)),
        I::MaskCount => Value::UInt(
            mask(shape, arg(0)?)?.into_iter().filter(|b| *b).count() as u64,
            64,
        ),
        I::MaskBitmask => Value::UInt(
            mask(shape, arg(0)?)?
                .into_iter()
                .enumerate()
                .fold(0u64, |acc, (i, b)| acc | (u64::from(b) << i)),
            64,
        ),
        I::MaskAny => Value::Bool(mask(shape, arg(0)?)?.into_iter().any(|b| b)),
        I::MaskAll => Value::Bool(mask(shape, arg(0)?)?.into_iter().all(|b| b)),
        I::Reduce(op) => {
            let v = vector(shape, arg(0)?)?;
            let op = match op {
                ReduceOp::Add => BinOp::Add,
                ReduceOp::Xor => BinOp::Xor,
                ReduceOp::Or => BinOp::Or,
                ReduceOp::And => BinOp::And,
                ReduceOp::Min => BinOp::Min,
                ReduceOp::Max => BinOp::Max,
            };
            let first = v[0];
            lane_value(
                elem,
                v.into_iter()
                    .skip(1)
                    .fold(first, |acc, x| binary(op, elem, acc, x)),
            )
        }
        I::Extract => lane_value(elem, vector(shape, arg(0)?)?[constant(0)?]),
        I::Replace => {
            let mut v = vector(shape, arg(0)?)?;
            v[constant(0)?] = raw(elem, arg(1)?)?;
            make_vector(shape, v)
        }
        I::Shuffle => {
            let v = vector(shape, arg(0)?)?;
            if constants.len() != n {
                return Err("Vec::shuffle lane count mismatch".into());
            }
            make_vector(
                shape,
                (0..n)
                    .map(|i| constant(i).map(|c| v[c]))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        I::LoadMasked => {
            let s = string_bytes(arg(0)?)?;
            let offset = u64_arg(arg(1)?)?;
            let len = s.len() as u64;
            make_vector(
                shape,
                (0..n as u64).map(|i| {
                    if in_range(offset, len, i) {
                        u64::from(s[(offset + i) as usize])
                    } else {
                        0
                    }
                }),
            )
        }
        I::InBounds => {
            let len = string_bytes(arg(0)?)?.len() as u64;
            let offset = u64_arg(arg(1)?)?;
            make_mask((0..n as u64).map(|i| in_range(offset, len, i)))
        }
        I::Active => {
            let (offset, len) = (u64_arg(arg(0)?)?, u64_arg(arg(1)?)?);
            make_mask((0..n as u64).map(|i| in_range(offset, len, i)))
        }
        I::IsaSupported => return Err("Isa::supported has no shape".into()),
        I::Load | I::Store | I::StoreMasked => {
            return Err(format!(
                "{intrinsic:?} is a sequence operation (SeqOp::Vector)"
            ));
        }
    })
}

/// A slice load or store on the window `items` of its sequence: the loaded
/// vector, or `Unit` after a store; `None` when an unmasked access does not
/// fit (`IndexOutOfBounds`).
pub fn slice_io(
    io: SliceIo,
    items: &mut [Value],
    at: u64,
    value: Option<&Value>,
) -> Result<Option<Value>, String> {
    let shape = io.shape;
    let n = u64::from(shape.lanes);
    let len = items.len() as u64;
    if !io.masked && !(at <= len && n <= len - at) {
        return Ok(None);
    }
    if io.store {
        let lanes = vector(shape, value.ok_or("Vec::store lacks its vector")?)?;
        for (i, lane) in lanes.into_iter().enumerate() {
            if in_range(at, len, i as u64) {
                items[(at + i as u64) as usize] = lane_value(shape.elem, lane);
            }
        }
        return Ok(Some(Value::Tuple(vec![])));
    }
    let lanes = (0..n)
        .map(|i| {
            if in_range(at, len, i) {
                raw(shape.elem, &items[(at + i) as usize])
            } else {
                Ok(0)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(make_vector(shape, lanes)))
}
