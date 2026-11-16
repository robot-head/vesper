//! CBOR (RFC 8949) form of structured attribute values, as upstream
//! otel-arrow stores slices and maps in the attribute and body `ser`
//! columns (`common.Serialize`/`Deserialize`, fxamacker/cbor): strings as
//! text, integers as CBOR integers, doubles as floats, booleans, byte
//! strings, arrays, maps with text keys, and null for an empty value.
//!
//! Encoding and decoding go through `ciborium`. Decoding is bounded: the
//! recursion limit is [`MAX_DECODE_DEPTH`], every decoded string and byte
//! string is charged against the caller's decoded-bytes budget (plus a
//! fixed charge per value), and integers outside `i64`, non-text map keys,
//! tags, undefined and simple values are typed errors.

use super::attributes::AttributeValue;
use super::decode::{DecodeError, DecodedValue};
use ciborium::value::Value;
use std::cell::Cell;

/// Deepest nesting accepted on decode (upstream does not bound it; this is
/// Beacon's own limit, well above [`super::MAX_VALUE_DEPTH`]).
pub const MAX_DECODE_DEPTH: usize = 32;
/// Budget charge per decoded value besides its string bytes.
const NODE_WEIGHT: usize = 16;

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

/// Decodes one `ser` cell. The whole cell must be one CBOR item.
pub(crate) fn decode(bytes: &[u8], budget: &Cell<usize>) -> Result<DecodedValue, DecodeError> {
    let mut reader = bytes;
    let value: Value =
        ciborium::de::from_reader_with_recursion_limit(&mut reader, MAX_DECODE_DEPTH)
            .map_err(|_| bad())?;
    if !reader.is_empty() {
        return Err(bad());
    }
    from_cbor(value, budget)
}
