//! CBOR (RFC 8949) form of structured attribute values, as upstream
//! otel-arrow stores slices and maps in the attribute and body `ser`
//! columns (`common.Serialize`/`Deserialize`, fxamacker/cbor): strings as
//! text, integers as CBOR integers, doubles as floats, booleans, byte
//! strings, arrays, maps with text keys, and null for an empty value.
//!
//! Encoding and decoding go through `ciborium`. Decoding is bounded: a
//! structural pre-scan of the cell walks every item header, without
//! allocating, and charges each value ([`NODE_WEIGHT`], at least the size of
//! a `ciborium` value) and every string byte against the caller's
//! decoded-bytes budget before `ciborium` builds anything, so a large,
//! shallow cell cannot allocate past the budget. Nesting deeper than
//! [`MAX_DECODE_DEPTH`], integers outside `i64`, non-text map keys, tags,
//! undefined and simple values are typed errors.

use super::attributes::AttributeValue;
use super::decode::{DecodeError, DecodedValue};
use ciborium::value::Value;
use std::cell::Cell;

/// Deepest nesting accepted on decode (upstream does not bound it; this is
/// Beacon's own limit, well above [`super::MAX_VALUE_DEPTH`]).
pub const MAX_DECODE_DEPTH: usize = 32;
/// Budget charge per decoded value besides its string bytes: at least the
/// memory one `ciborium` value takes.
const NODE_WEIGHT: usize = if core::mem::size_of::<Value>() > 16 {
    core::mem::size_of::<Value>()
} else {
    16
};

fn to_cbor(value: &AttributeValue) -> Value {
    match value {
        AttributeValue::Bool(b) => Value::Bool(*b),
        AttributeValue::Int(i) => Value::Integer((*i).into()),
        AttributeValue::Double(d) => Value::Float(*d),
        AttributeValue::Bytes(b) => Value::Bytes(b.to_vec()),
        AttributeValue::Slice(items) => Value::Array(items.iter().map(to_cbor).collect()),
        AttributeValue::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(k, v)| (Value::Text((*k).to_owned()), to_cbor(v)))
                .collect(),
        ),
        other => Value::Text(other.export_str().expect("string forms")),
    }
}

/// The CBOR bytes of a stored value.
pub(crate) fn encode(value: &AttributeValue) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(&to_cbor(value), &mut out).expect("writing to a Vec cannot fail");
    out
}

fn charge(budget: &Cell<usize>, bytes: usize) -> Result<(), DecodeError> {
    let left = budget
        .get()
        .checked_sub(bytes.saturating_add(NODE_WEIGHT))
        .ok_or(DecodeError::DecodedTooLarge)?;
    budget.set(left);
    Ok(())
}

fn from_cbor(value: Value, budget: &Cell<usize>) -> Result<DecodedValue, DecodeError> {
    Ok(match value {
        Value::Null => {
            charge(budget, 0)?;
            DecodedValue::Empty
        }
        Value::Bool(b) => {
            charge(budget, 0)?;
            DecodedValue::Bool(b)
        }
        Value::Integer(i) => {
            charge(budget, 0)?;
            DecodedValue::Int(i64::try_from(i128::from(i)).map_err(|_| bad())?)
        }
        Value::Float(f) => {
            charge(budget, 0)?;
            DecodedValue::Double(f)
        }
        Value::Text(s) => {
            charge(budget, s.len())?;
            DecodedValue::Str(s)
        }
        Value::Bytes(b) => {
            charge(budget, b.len())?;
            DecodedValue::Bytes(b)
        }
        Value::Array(items) => {
            charge(budget, 0)?;
            DecodedValue::Slice(
                items
                    .into_iter()
                    .map(|v| from_cbor(v, budget))
                    .collect::<Result<_, _>>()?,
            )
        }
        Value::Map(entries) => {
            charge(budget, 0)?;
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let Value::Text(k) = k else {
                    return Err(bad());
                };
                charge(budget, k.len())?;
                out.push((k, from_cbor(v, budget)?));
            }
            DecodedValue::Map(out)
        }
        _ => return Err(bad()),
    })
}

fn bad() -> DecodeError {
    DecodeError::BadSerializedValue
}

/// Allocation-free structural walk over CBOR item headers, charging what
/// decoding will allocate.
struct Scan<'a> {
    bytes: &'a [u8],
    left: usize,
}
impl Scan<'_> {
    fn byte(&mut self) -> Result<u8, DecodeError> {
        let (&b, rest) = self.bytes.split_first().ok_or_else(bad)?;
        self.bytes = rest;
        Ok(b)
    }
    fn skip(&mut self, n: u64) -> Result<usize, DecodeError> {
        let n = usize::try_from(n).map_err(|_| bad())?;
        if n > self.bytes.len() {
            return Err(bad());
        }
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
    /// The header argument; `None` for indefinite length.
    fn arg(&mut self, info: u8) -> Result<Option<u64>, DecodeError> {
        let width = match info {
            0..=23 => return Ok(Some(u64::from(info))),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            31 => return Ok(None),
            _ => return Err(bad()),
        };
        let mut v = 0u64;
        for _ in 0..width {
            v = (v << 8) | u64::from(self.byte()?);
        }
        Ok(Some(v))
    }
    fn charge(&mut self, bytes: usize) -> Result<(), DecodeError> {
        self.left = self
            .left
            .checked_sub(bytes.saturating_add(NODE_WEIGHT))
            .ok_or(DecodeError::DecodedTooLarge)?;
        Ok(())
    }
    /// One data item; `false` when it was a break code instead.
    fn item(&mut self, depth: usize) -> Result<bool, DecodeError> {
        if depth > MAX_DECODE_DEPTH {
            return Err(bad());
        }
        let b = self.byte()?;
        let (major, info) = (b >> 5, b & 0x1f);
        match major {
            0 | 1 => {
                self.arg(info)?.ok_or_else(bad)?;
                self.charge(0)?;
            }
            2 | 3 => match self.arg(info)? {
                Some(n) => {
                    let n = self.skip(n)?;
                    self.charge(n)?;
                }
                None => {
                    let mut total = 0usize;
                    loop {
                        let c = self.byte()?;
                        if c == 0xff {
                            break;
                        }
                        if c >> 5 != major {
                            return Err(bad());
                        }
                        let n = self.arg(c & 0x1f)?.ok_or_else(bad)?;
                        total = total.saturating_add(self.skip(n)?);
                    }
                    self.charge(total)?;
                }
            },
            4 | 5 => {
                self.charge(0)?;
                let per = if major == 5 { 2 } else { 1 };
                match self.arg(info)? {
                    // Each item takes at least one byte, so this loop ends
                    // within the cell however large `n` claims to be.
                    Some(n) => {
                        let items = n.checked_mul(per).ok_or_else(bad)?;
                        for _ in 0..items {
                            if !self.item(depth + 1)? {
                                return Err(bad());
                            }
                        }
                    }
                    None => {
                        while self.item(depth + 1)? {
                            if per == 2 && !self.item(depth + 1)? {
                                return Err(bad());
                            }
                        }
                    }
                }
            }
            7 => match info {
                31 => return Ok(false),
                20..=22 => self.charge(0)?,
                25..=27 => {
                    self.skip(1 << (info - 24))?;
                    self.charge(0)?;
                }
                _ => return Err(bad()),
            },
            _ => return Err(bad()),
        }
        Ok(true)
    }
}

/// Decodes one `ser` cell. The whole cell must be one CBOR item.
pub(crate) fn decode(bytes: &[u8], budget: &Cell<usize>) -> Result<DecodedValue, DecodeError> {
    let mut scan = Scan {
        bytes,
        left: budget.get(),
    };
    if !scan.item(1)? || !scan.bytes.is_empty() {
        return Err(bad());
    }
    let mut reader = bytes;
    let value: Value =
        ciborium::de::from_reader_with_recursion_limit(&mut reader, MAX_DECODE_DEPTH)
            .map_err(|_| bad())?;
    if !reader.is_empty() {
        return Err(bad());
    }
    from_cbor(value, budget)
}
