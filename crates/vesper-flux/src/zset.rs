use crate::{Budget, Error, Limits};
use std::collections::BTreeMap;

pub type Row = Vec<i64>;

/// Canonical nonzero weights; private representation prevents malformed batches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZSet {
    pub(crate) width: usize,
    pub(crate) rows: BTreeMap<Row, i64>,
}
impl ZSet {
    pub fn empty(width: usize, limits: Limits) -> Result<Self, Error> {
        if width > limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        Ok(Self {
            width,
            rows: BTreeMap::new(),
        })
    }
    /// Borrows the raw owner, including on overflow/limit/schema errors.
    pub fn consolidate(
        width: usize,
        entries: &[(Row, i64)],
        limits: Limits,
    ) -> Result<Self, Error> {
        if entries.len() > limits.max_input_entries {
            return Err(Error::Limit("input entries"));
        }
        let mut acc = Accumulator::new(width, limits)?;
        let mut budget = Budget::new(limits);
        for (row, weight) in entries {
            acc.add(row, i128::from(*weight), &mut budget)?;
        }
        acc.finish()
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn entries(&self) -> impl Iterator<Item = (&Row, i64)> {
        self.rows.iter().map(|(r, w)| (r, *w))
    }
    pub fn plus(&self, other: &Self, limits: Limits) -> Result<Self, Error> {
        union(&[self, other], limits, &mut Budget::new(limits))
    }
}
pub(crate) struct Accumulator {
    width: usize,
    limits: Limits,
    rows: BTreeMap<Row, i128>,
}
impl Accumulator {
    pub(crate) fn new(width: usize, limits: Limits) -> Result<Self, Error> {
        if width > limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        Ok(Self {
            width,
            limits,
            rows: BTreeMap::new(),
        })
    }
    pub(crate) fn add(
        &mut self,
        row: &Row,
        weight: i128,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        budget.charge(1usize.checked_add(row.len()).ok_or(Error::Overflow)?)?;
        if row.len() != self.width {
            return Err(Error::Schema);
        }
        if weight == 0 {
            return Ok(());
        }
        if !self.rows.contains_key(row) && self.rows.len() >= self.limits.max_rows {
            return Err(Error::Limit("rows"));
        }
        let old = self.rows.get(row).copied().unwrap_or(0);
        self.rows
            .insert(row.clone(), old.checked_add(weight).ok_or(Error::Overflow)?);
        Ok(())
    }
    pub(crate) fn finish(self) -> Result<ZSet, Error> {
        let mut rows = BTreeMap::new();
        for (row, w) in self.rows {
            if w != 0 {
                rows.insert(row, i64::try_from(w).map_err(|_| Error::Overflow)?);
            }
        }
        Ok(ZSet {
            width: self.width,
            rows,
        })
    }
}
pub(crate) fn union(sets: &[&ZSet], limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    let width = sets.first().ok_or(Error::Schema)?.width;
    let mut acc = Accumulator::new(width, limits)?;
    for set in sets {
        if set.width != width {
            return Err(Error::Schema);
        }
        for (row, w) in set.entries() {
            acc.add(row, i128::from(w), budget)?;
        }
    }
    acc.finish()
}
