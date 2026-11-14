//! Secondary indexes: one B+Tree per indexed column, keyed by
//! `(order-preserving 64-bit value prefix, row id)`.
//!
//! An index holds the key `(prefix(v), r)` exactly when some stored version
//! of row `r` has a non-null value `v` with that prefix in the column; the
//! commit group (and vacuum) that adds or removes such a version changes
//! the index in the same group, so table and index are atomic under crash
//! recovery. Entries are not versioned: a lookup reads candidate row ids
//! from the index and rechecks each against the row version its snapshot
//! sees. The prefix is monotone (`a <= b` implies `prefix(a) <=
//! prefix(b)`), so a value range maps to a key range; text and bytes
//! share a prefix when their first eight bytes agree, and the recheck
//! separates them.
use super::btree::{self, Pager, TreeCfg};
use super::error::{corrupt, StoreResult};
use super::pages::{Entry, Key};
use crate::{ScalarType, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Order-preserving 64-bit prefix of a non-null value.
pub(crate) fn prefix(v: &Value) -> Option<u64> {
    Some(match v {
        Value::Null => return None,
        Value::U64(x) => *x,
        Value::I64(x) => (*x as u64) ^ (1 << 63),
        Value::F64(x) => {
            let bits = if *x == 0.0 { 0 } else { x.to_bits() };
            if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ (1 << 63)
            }
        }
        Value::Bool(b) => u64::from(*b),
        Value::Text(s) => bytes_prefix(s.as_bytes()),
        Value::Bytes(b) => bytes_prefix(b),
    })
}
fn bytes_prefix(b: &[u8]) -> u64 {
    let mut out = [0u8; 8];
    let n = b.len().min(8);
    out[..n].copy_from_slice(&b[..n]);
    u64::from_be_bytes(out)
}

/// SQL order of two non-null values of one type (`None` otherwise).
pub(crate) fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::U64(x), Value::U64(y)) => Some(x.cmp(y)),
        (Value::I64(x), Value::I64(y)) => Some(x.cmp(y)),
        (Value::F64(x), Value::F64(y)) => x.partial_cmp(y),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Text(x), Value::Text(y)) => Some(x.cmp(y)),
        (Value::Bytes(x), Value::Bytes(y)) => Some(x.cmp(y)),
        _ => None,
    }
}
/// `lo <= v <= hi`.
pub(crate) fn within(v: &Value, lo: &Value, hi: &Value) -> bool {
    compare(lo, v).is_some_and(|o| o != Ordering::Greater)
        && compare(v, hi).is_some_and(|o| o != Ordering::Greater)
}

pub(crate) fn key(prefix: u64, rowid: u64) -> Key {
    Key {
        rowid: prefix,
        inv_ts: rowid,
    }
}

/// Distinct value prefixes (with one value each) over every stored version
/// of `rowid` in the table tree at `root`.
pub(crate) fn row_prefixes(
    p: &mut dyn Pager,
    root: u64,
    rowid: u64,
    column: usize,
) -> StoreResult<BTreeMap<u64, Value>> {
    let mut out = BTreeMap::new();
    btree::visit(
        p,
        root,
        Key { rowid, inv_ts: 0 },
        Key {
            rowid,
            inv_ts: u64::MAX,
        },
        &mut |e| {
            if !e.tomb {
                let v = e
                    .values
                    .get(column)
                    .ok_or_else(|| corrupt("index column beyond the row"))?;
                if let Some(px) = prefix(v) {
                    out.entry(px).or_insert_with(|| v.clone());
                }
            }
            Ok(true)
        },
    )?;
    Ok(out)
}

/// Brings the index at `root` from the row's prefixes `before` to `after`;
/// returns the index root.
pub(crate) fn apply(
    p: &mut dyn Pager,
    cfg: TreeCfg,
    ty: ScalarType,
    mut root: u64,
    rowid: u64,
    before: &BTreeMap<u64, Value>,
    after: &BTreeMap<u64, Value>,
) -> StoreResult<u64> {
    let types = [ty];
    for px in before.keys().filter(|k| !after.contains_key(k)) {
        let (r, found) = btree::remove(p, cfg, &types, root, key(*px, rowid))?;
        if !found {
            return Err(corrupt("index entry vanished"));
        }
        root = r;
    }
    for (px, v) in after.iter().filter(|(k, _)| !before.contains_key(k)) {
        root = btree::insert(
            p,
            cfg,
            &types,
            root,
            Entry {
                key: key(*px, rowid),
                tomb: false,
                values: vec![v.clone()],
            },
        )?;
    }
    Ok(root)
}
