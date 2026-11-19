//! The Vesper build of the Z-set kernels and the frontier: thin adapters
//! from the crate's types to the Vesper library `flux` (`vesper/`, port wave
//! P3b lane 7, #1069) through the generated binding (`port/binding.rs`).
//! A set's cells are lent in place; each kernel returns the new set's cells.

use crate::zset::ZSet;
use crate::{Budget, Error, Limits};

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

use binding::{FrErr, ZErr};

fn zset_error(e: ZErr) -> Error {
    match e {
        ZErr::Work => Error::Limit("work"),
        ZErr::Rows => Error::Limit("rows"),
        ZErr::Overflow => Error::Overflow,
        ZErr::Product => Error::Limit("join product"),
        ZErr::Columns => Error::Limit("columns"),
        ZErr::InputEntries => Error::Limit("input entries"),
    }
}

pub(crate) fn frontier_error(e: FrErr) -> Error {
    match e {
        FrErr::Regression => Error::WatermarkRegression,
        FrErr::Holds => Error::Limit("frontier holds"),
        FrErr::Exhausted => Error::IdentityExhausted,
        FrErr::Stale => Error::StaleTicket,
    }
}

fn w(x: usize) -> u64 {
    x as u64
}

/// Runs a kernel on the budget's remaining work, keeping its charges.
fn charged<T>(
    budget: &mut Budget,
    f: impl FnOnce(&mut u64) -> Result<T, ZErr>,
) -> Result<T, Error> {
    let mut left = w(budget.remaining);
    let out = f(&mut left);
    budget.remaining = left as usize;
    out.map_err(zset_error)
}

/// `ZSet::consolidate` over `n` entries of the right width, as cells.
pub(crate) fn consolidate(
    width: usize,
    cells: &[i64],
    n: usize,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |b| {
        binding::zs_consolidate(
            w(width),
            cells,
            w(n),
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(width, out))
}

/// The sum of one to three sets of `width`.
pub(crate) fn union(
    width: usize,
    sets: &[&ZSet],
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let (cols, rows) = (w(limits.max_columns), w(limits.max_rows));
    let mut nonempty = sets.iter().filter(|s| !s.is_empty());
    let first = nonempty.next();
    if nonempty.next().is_none() {
        // At most one set has rows: the sum is a copy of it. The kernel
        // charges it, and the cells are copied here.
        let n = first.map_or(0, |s| s.len());
        charged(budget, |b| {
            binding::zs_copy_check(w(width), w(n), cols, rows, b)
        })?;
        return Ok(match first {
            Some(s) => (*s).clone(),
            None => ZSet::from_cells(width, Vec::new()),
        });
    }
    let empty: &[i64] = &[];
    let cells = |i: usize| sets.get(i).map_or(empty, |s| s.cells.as_slice());
    let len = |i: usize| sets.get(i).map_or(0, |s| w(s.len()));
    let out = charged(budget, |b| {
        binding::zs_union3(
            w(width),
            cells(0),
            len(0),
            cells(1),
            len(1),
            cells(2),
            len(2),
            cols,
            rows,
            b,
        )
    })?;
    Ok(ZSet::from_cells(width, out))
}

/// The `Filter` operator: `at_least` is `Predicate::AtLeast`.
pub(crate) fn filter(
    a: &ZSet,
    at_least: bool,
    column: usize,
    value: i64,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |b| {
        binding::zs_filter(
            w(a.width),
            &a.cells,
            w(a.len()),
            u64::from(at_least),
            w(column),
            value,
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(a.width, out))
}

/// The `Map` operator: `fields` holds (0, column) or (1, constant) pairs.
pub(crate) fn map(
    a: &ZSet,
    fields: &[i64],
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let nf = fields.len() / 2;
    let out = charged(budget, |b| {
        binding::zs_map(
            w(a.width),
            &a.cells,
            w(a.len()),
            fields,
            w(nf),
            w(limits.max_columns),
            w(limits.max_rows),
            b,
        )
    })?;
    Ok(ZSet::from_cells(nf, out))
}

/// The `Join` operator.
pub(crate) fn join(
    a: &ZSet,
    b: &ZSet,
    ka: usize,
    kb: usize,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let out = charged(budget, |left| {
        binding::zs_join(
            w(a.width),
            &a.cells,
            w(a.len()),
            w(b.width),
            &b.cells,
            w(b.len()),
            w(ka),
            w(kb),
            w(limits.max_columns),
            w(limits.max_product),
            w(limits.max_rows),
            left,
        )
    })?;
    Ok(ZSet::from_cells(a.width + b.width, out))
}
