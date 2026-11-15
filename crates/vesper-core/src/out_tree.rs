//! Field-tree `OutInitTree` initialization coverage for write-only `out`
//! destinations (manuscript §4.4).
//!
//! Tracks partial initialization across nested struct field paths inside an
//! `OutSlot[T]`, enforces 100% completion prior to normal return, permits
//! unwinds when no linear sub-fields have been written yet, and computes
//! conservative branch-join intersection. This module is standalone and does
//! not allocate destination storage or integrate with the source CFG checker.

use crate::ast::Mode;
use std::collections::BTreeMap;

/// Zero-based struct field identifier in declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId(pub u16);

/// Bounds on field-tree nesting and node count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_depth: usize,
    pub max_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_nodes: 4096,
        }
    }
}

/// Static field schema of an `out` destination aggregate (`Leaf` or nested `Struct`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldSchema {
    Leaf { mode: Mode },
    Struct(BTreeMap<FieldId, FieldSchema>),
}

impl FieldSchema {
    /// Checks structural bounds (`max_depth`, `max_nodes`) and non-empty structs.
    pub fn validate(&self, limits: Limits) -> Result<(), Error> {
        let mut nodes = 0usize;
        self.validate_inner(0, limits, &mut nodes)
    }

    fn validate_inner(&self, depth: usize, limits: Limits, nodes: &mut usize) -> Result<(), Error> {
        if depth > limits.max_depth {
            return Err(Error::DepthLimit {
                max: limits.max_depth,
            });
        }
        *nodes = nodes
            .checked_add(1)
            .filter(|&n| n <= limits.max_nodes)
            .ok_or(Error::NodeLimit {
                max: limits.max_nodes,
            })?;
        if let Self::Struct(fields) = self {
            if fields.is_empty() {
                return Err(Error::EmptyStructSchema);
            }
            for child in fields.values() {
                child.validate_inner(depth + 1, limits, nodes)?;
            }
        }
        Ok(())
    }
}

/// Errors produced by `OutInitTree` operations. Mutating operations leave state
/// unchanged on failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownField { field: FieldId },
    NotAStruct { field: FieldId },
    AlreadyInitialized,
    InconsistentLinearInitAtJoin { field: FieldId },
    UnwindWithLiveLinearField { field: FieldId },
    EmptyStructSchema,
    DepthLimit { max: usize },
    NodeLimit { max: usize },
}

/// Path-local initialization state of a nested `out` destination (`§4.4`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutInitTree {
    /// Nothing in this subtree has been initialized yet.
    #[default]
    Uninit,
    /// This subtree may have been initialized on at least one branch, so
    /// re-writing any overlapping path is rejected, but it is not guaranteed
    /// initialized on all branches.
    MaybeInit,
    /// Some subset of direct child fields has been initialized or maybe-initialized.
    Fields(BTreeMap<FieldId, OutInitTree>),
    /// 100% of this subtree has been initialized on all branches.
    Whole,
}

impl OutInitTree {
    pub fn new() -> Self {
        Self::Uninit
    }

    /// Counts tree nodes cleanly under `limits`.
    pub fn node_count(&self) -> usize {
        match self {
            Self::Uninit | Self::MaybeInit | Self::Whole => 1,
            Self::Fields(map) => 1 + map.values().map(Self::node_count).sum::<usize>(),
        }
    }

    /// Returns `true` when 100% of the destination described by `schema` has
    /// been initialized (§4.4 `OutInitTree` return check).
    pub fn is_complete(&self, schema: &FieldSchema) -> bool {
        match (self, schema) {
            (Self::Whole, _) => true,
            (Self::Uninit | Self::MaybeInit, _) => false,
            (Self::Fields(_), FieldSchema::Leaf { .. }) => false,
            (Self::Fields(inits), FieldSchema::Struct(fields)) => {
                !fields.is_empty()
                    && fields.iter().all(|(id, sub_schema)| {
                        inits.get(id).is_some_and(|t| t.is_complete(sub_schema))
                    })
            }
        }
    }

    /// Records initialization of `path` (empty slice initializes the entire
    /// destination root) validated against `schema`. Leaves `self` unchanged on error.
    pub fn initialize_path(
        &mut self,
        path: &[FieldId],
        schema: &FieldSchema,
        limits: Limits,
    ) -> Result<(), Error> {
        schema.validate(limits)?;
        let mut next = self.clone();
        next.insert_path(path, schema, 0, limits)?;
        next.normalize(schema);
        if next.node_count() > limits.max_nodes {
            return Err(Error::NodeLimit {
                max: limits.max_nodes,
            });
        }
        *self = next;
        Ok(())
    }

    fn insert_path(
        &mut self,
        path: &[FieldId],
        schema: &FieldSchema,
        depth: usize,
        limits: Limits,
    ) -> Result<(), Error> {
        if depth > limits.max_depth {
            return Err(Error::DepthLimit {
                max: limits.max_depth,
            });
        }
        if matches!(self, Self::Whole | Self::MaybeInit) {
            return Err(Error::AlreadyInitialized);
        }
        let Some((&first, rest)) = path.split_first() else {
            return match self {
                Self::Uninit => {
                    *self = Self::Whole;
                    Ok(())
                }
                Self::Fields(_) | Self::Whole | Self::MaybeInit => Err(Error::AlreadyInitialized),
            };
        };

        let FieldSchema::Struct(fields) = schema else {
            return Err(Error::NotAStruct { field: first });
        };
        let Some(sub_schema) = fields.get(&first) else {
            return Err(Error::UnknownField { field: first });
        };

        if matches!(self, Self::Uninit) {
            *self = Self::Fields(BTreeMap::new());
        }
        let Self::Fields(map) = self else {
            unreachable!("converted above");
        };
        let entry = map.entry(first).or_default();
        entry.insert_path(rest, sub_schema, depth + 1, limits)
    }

    fn normalize(&mut self, schema: &FieldSchema) {
        if let (Self::Fields(map), FieldSchema::Struct(fields)) = (self.clone(), schema) {
            let mut normalized = BTreeMap::new();
            for (id, mut sub) in map {
                if let Some(sub_schema) = fields.get(&id) {
                    sub.normalize(sub_schema);
                }
                if !matches!(sub, Self::Uninit) {
                    normalized.insert(id, sub);
                }
            }
            if normalized.is_empty() {
                *self = Self::Uninit;
            } else if fields.len() == normalized.len()
                && fields
                    .keys()
                    .all(|id| matches!(normalized.get(id), Some(Self::Whole)))
            {
                *self = Self::Whole;
            } else if fields.len() == normalized.len()
                && fields
                    .keys()
                    .all(|id| matches!(normalized.get(id), Some(Self::MaybeInit)))
            {
                *self = Self::MaybeInit;
            } else {
                *self = Self::Fields(normalized);
            }
        }
    }

    /// Checks whether an early `Fail` unwind is safe prior to full destination
    /// return (§4.4). Uninitialized destinations always unwind safely; partial
    /// initialization succeeds iff no initialized sub-field holds `Mode::Linear`.
    pub fn check_unwind_safety(&self, schema: &FieldSchema) -> Result<(), Error> {
        match (self, schema) {
            (Self::Uninit, _) => Ok(()),
            (Self::Whole | Self::MaybeInit, FieldSchema::Leaf { mode }) => {
                if *mode == Mode::Linear {
                    Err(Error::UnwindWithLiveLinearField { field: FieldId(0) })
                } else {
                    Ok(())
                }
            }
            (Self::Whole | Self::MaybeInit, FieldSchema::Struct(fields)) => {
                for (&id, sub_schema) in fields {
                    self.check_leaf_linear(id, sub_schema)?;
                }
                Ok(())
            }
            (Self::Fields(map), FieldSchema::Struct(fields)) => {
                for (&id, sub_tree) in map {
                    if let Some(sub_schema) = fields.get(&id) {
                        if matches!(sub_schema, FieldSchema::Leaf { mode } if *mode == Mode::Linear)
                            && !matches!(sub_tree, Self::Uninit)
                        {
                            return Err(Error::UnwindWithLiveLinearField { field: id });
                        }
                        sub_tree.check_unwind_safety(sub_schema)?;
                    }
                }
                Ok(())
            }
            (Self::Fields(_), FieldSchema::Leaf { mode }) => {
                if *mode == Mode::Linear {
                    Err(Error::UnwindWithLiveLinearField { field: FieldId(0) })
                } else {
                    Ok(())
                }
            }
        }
    }

    fn check_leaf_linear(&self, id: FieldId, schema: &FieldSchema) -> Result<(), Error> {
        match (self, schema) {
            (Self::Uninit, _) => Ok(()),
            (_, FieldSchema::Leaf { mode }) if *mode == Mode::Linear => {
                Err(Error::UnwindWithLiveLinearField { field: id })
            }
            (_, FieldSchema::Leaf { .. }) => Ok(()),
            (Self::Whole | Self::MaybeInit, FieldSchema::Struct(fields)) => {
                for (&child_id, sub_schema) in fields {
                    self.check_leaf_linear(child_id, sub_schema)?;
                }
                Ok(())
            }
            (Self::Fields(map), FieldSchema::Struct(fields)) => {
                for (&child_id, sub) in map {
                    if let Some(sub_schema) = fields.get(&child_id) {
                        sub.check_leaf_linear(child_id, sub_schema)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Computes conservative branch-join initialization (`self ∩ other` for
    /// completion, `self ∪ other` for overlap tracking) while rejecting branches
    /// that initialize a `Mode::Linear` sub-field on only one side
    /// (`Theorem 2` / `[T-Match]` consistency).
    pub fn join(&self, other: &Self, schema: &FieldSchema) -> Result<Self, Error> {
        let mut out = self.join_inner(other, schema, FieldId(0))?;
        out.normalize(schema);
        Ok(out)
    }

    fn join_inner(
        &self,
        other: &Self,
        schema: &FieldSchema,
        field_hint: FieldId,
    ) -> Result<Self, Error> {
        match (self, other, schema) {
            (Self::Whole, Self::Whole, _) => Ok(Self::Whole),
            (Self::Uninit, Self::Uninit, _) => Ok(Self::Uninit),
            (Self::MaybeInit, Self::MaybeInit, _) => {
                self.check_no_linear_fields(schema, field_hint)?;
                Ok(Self::MaybeInit)
            }
            (Self::Uninit | Self::MaybeInit, Self::Whole, _)
            | (Self::Whole, Self::Uninit | Self::MaybeInit, _) => {
                Self::Whole.check_no_linear_fields(schema, field_hint)?;
                Ok(Self::MaybeInit)
            }
            (Self::Uninit, Self::MaybeInit, _) | (Self::MaybeInit, Self::Uninit, _) => {
                Self::MaybeInit.check_no_linear_fields(schema, field_hint)?;
                Ok(Self::MaybeInit)
            }
            (Self::MaybeInit, Self::Fields(_), FieldSchema::Struct(_))
            | (Self::Fields(_), Self::MaybeInit, FieldSchema::Struct(_)) => {
                Self::MaybeInit.check_no_linear_fields(schema, field_hint)?;
                Ok(Self::MaybeInit)
            }
            (Self::Uninit, Self::Fields(map), FieldSchema::Struct(fields))
            | (Self::Fields(map), Self::Uninit, FieldSchema::Struct(fields)) => {
                let mut joined = BTreeMap::new();
                for (&id, sub_schema) in fields {
                    let sub_rhs = map.get(&id).cloned().unwrap_or(Self::Uninit);
                    let res = Self::Uninit.join_inner(&sub_rhs, sub_schema, id)?;
                    if !matches!(res, Self::Uninit) {
                        joined.insert(id, res);
                    }
                }
                Ok(Self::Fields(joined))
            }
            (Self::Whole, Self::Fields(map), FieldSchema::Struct(fields))
            | (Self::Fields(map), Self::Whole, FieldSchema::Struct(fields)) => {
                let mut joined = BTreeMap::new();
                for (&id, sub_schema) in fields {
                    let sub_rhs = map.get(&id).cloned().unwrap_or(Self::Uninit);
                    let res = Self::Whole.join_inner(&sub_rhs, sub_schema, id)?;
                    if !matches!(res, Self::Uninit) {
                        joined.insert(id, res);
                    }
                }
                Ok(Self::Fields(joined))
            }
            (Self::Fields(lhs), Self::Fields(rhs), FieldSchema::Struct(fields)) => {
                let mut joined = BTreeMap::new();
                for (&id, sub_schema) in fields {
                    let l = lhs.get(&id).cloned().unwrap_or(Self::Uninit);
                    let r = rhs.get(&id).cloned().unwrap_or(Self::Uninit);
                    let res = l.join_inner(&r, sub_schema, id)?;
                    if !matches!(res, Self::Uninit) {
                        joined.insert(id, res);
                    }
                }
                Ok(Self::Fields(joined))
            }
            (Self::Fields(map), other_side, FieldSchema::Leaf { mode })
            | (other_side, Self::Fields(map), FieldSchema::Leaf { mode }) => {
                if *mode == Mode::Linear && (!map.is_empty() || !matches!(other_side, Self::Uninit))
                {
                    return Err(Error::InconsistentLinearInitAtJoin { field: field_hint });
                }
                if map.is_empty() && matches!(other_side, Self::Uninit) {
                    Ok(Self::Uninit)
                } else {
                    Ok(Self::MaybeInit)
                }
            }
        }
    }

    fn check_no_linear_fields(
        &self,
        schema: &FieldSchema,
        field_hint: FieldId,
    ) -> Result<(), Error> {
        match (self, schema) {
            (Self::Uninit, _) => Ok(()),
            (_, FieldSchema::Leaf { mode }) => {
                if *mode == Mode::Linear {
                    Err(Error::InconsistentLinearInitAtJoin { field: field_hint })
                } else {
                    Ok(())
                }
            }
            (Self::Whole | Self::MaybeInit, FieldSchema::Struct(fields)) => {
                for (&id, sub_schema) in fields {
                    if self.has_linear_leaf(sub_schema) {
                        return Err(Error::InconsistentLinearInitAtJoin { field: id });
                    }
                }
                Ok(())
            }
            (Self::Fields(map), FieldSchema::Struct(fields)) => {
                for (&id, sub) in map {
                    if let Some(sub_schema) = fields.get(&id)
                        && sub.has_linear_leaf(sub_schema)
                    {
                        return Err(Error::InconsistentLinearInitAtJoin { field: id });
                    }
                }
                Ok(())
            }
        }
    }

    fn has_linear_leaf(&self, schema: &FieldSchema) -> bool {
        match (self, schema) {
            (Self::Uninit, _) => false,
            (_, FieldSchema::Leaf { mode }) => *mode == Mode::Linear,
            (Self::Whole | Self::MaybeInit, FieldSchema::Struct(fields)) => {
                fields.values().any(|s| self.has_linear_leaf(s))
            }
            (Self::Fields(map), FieldSchema::Struct(fields)) => map
                .iter()
                .any(|(id, sub)| fields.get(id).is_some_and(|s| sub.has_linear_leaf(s))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker_schema() -> FieldSchema {
        // Broker { 0: config (Copy), 1: metrics: Metrics { 0: batches (Copy), 1: wal_lsn (Affine) } }
        let metrics = BTreeMap::from([
            (FieldId(0), FieldSchema::Leaf { mode: Mode::Copy }),
            (FieldId(1), FieldSchema::Leaf { mode: Mode::Affine }),
        ]);
        FieldSchema::Struct(BTreeMap::from([
            (FieldId(0), FieldSchema::Leaf { mode: Mode::Copy }),
            (FieldId(1), FieldSchema::Struct(metrics)),
        ]))
    }

    #[test]
    fn incremental_field_initialization_completes_nested_schema_and_rejects_overlap() {
        let schema = broker_schema();
        let limits = Limits::default();
        let mut tree = OutInitTree::new();

        assert!(!tree.is_complete(&schema));
        assert_eq!(tree.check_unwind_safety(&schema), Ok(()));

        tree.initialize_path(&[FieldId(0)], &schema, limits)
            .unwrap();
        assert!(!tree.is_complete(&schema));

        // Duplicate init leaves state unchanged.
        let snapshot = tree.clone();
        assert_eq!(
            tree.initialize_path(&[FieldId(0)], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(
            tree.initialize_path(&[], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(tree, snapshot);

        tree.initialize_path(&[FieldId(1), FieldId(0)], &schema, limits)
            .unwrap();
        assert!(!tree.is_complete(&schema));

        tree.initialize_path(&[FieldId(1), FieldId(1)], &schema, limits)
            .unwrap();
        assert!(tree.is_complete(&schema));
        assert_eq!(tree, OutInitTree::Whole);
    }

    #[test]
    fn branch_join_intersects_discardable_fields_and_rejects_one_sided_linear() {
        let schema = broker_schema();
        let limits = Limits::default();

        let mut lhs = OutInitTree::new();
        lhs.initialize_path(&[FieldId(0)], &schema, limits).unwrap();
        lhs.initialize_path(&[FieldId(1), FieldId(0)], &schema, limits)
            .unwrap();

        let mut rhs = OutInitTree::new();
        rhs.initialize_path(&[FieldId(0)], &schema, limits).unwrap();
        rhs.initialize_path(&[FieldId(1), FieldId(1)], &schema, limits)
            .unwrap();

        let mut joined = lhs.join(&rhs, &schema).unwrap();
        assert!(!joined.is_complete(&schema));
        assert_eq!(
            joined,
            OutInitTree::Fields(BTreeMap::from([
                (FieldId(0), OutInitTree::Whole),
                (FieldId(1), OutInitTree::MaybeInit),
            ]))
        );
        // Possible writes on either branch must prevent subsequent overlapping writes.
        assert_eq!(
            joined.initialize_path(&[FieldId(1), FieldId(0)], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(
            joined.initialize_path(&[FieldId(1), FieldId(1)], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(
            joined.initialize_path(&[], &schema, limits),
            Err(Error::AlreadyInitialized)
        );

        // One-sided initialization of a Linear leaf cannot silently drop on join or unwind.
        let linear_schema = FieldSchema::Struct(BTreeMap::from([
            (FieldId(0), FieldSchema::Leaf { mode: Mode::Linear }),
            (FieldId(1), FieldSchema::Leaf { mode: Mode::Copy }),
        ]));
        let mut arm_a = OutInitTree::new();
        arm_a
            .initialize_path(&[FieldId(0)], &linear_schema, limits)
            .unwrap();
        let arm_b = OutInitTree::new();
        assert_eq!(
            arm_a.join(&arm_b, &linear_schema),
            Err(Error::InconsistentLinearInitAtJoin { field: FieldId(0) })
        );
        assert_eq!(
            arm_a.check_unwind_safety(&linear_schema),
            Err(Error::UnwindWithLiveLinearField { field: FieldId(0) })
        );
        assert_eq!(arm_b.check_unwind_safety(&linear_schema), Ok(()));
    }

    #[test]
    fn validates_unknown_fields_non_structs_and_limits() {
        let schema = broker_schema();
        let limits = Limits::default();
        let mut tree = OutInitTree::new();

        assert_eq!(
            tree.initialize_path(&[FieldId(9)], &schema, limits),
            Err(Error::UnknownField { field: FieldId(9) })
        );
        assert_eq!(
            tree.initialize_path(&[FieldId(0), FieldId(0)], &schema, limits),
            Err(Error::NotAStruct { field: FieldId(0) })
        );
        assert_eq!(tree, OutInitTree::Uninit);

        let tight = Limits {
            max_depth: 1,
            max_nodes: 16,
        };
        assert_eq!(
            tree.initialize_path(&[FieldId(1), FieldId(0)], &schema, tight),
            Err(Error::DepthLimit { max: 1 })
        );
    }

    #[test]
    fn one_sided_branch_init_preserves_possible_init_for_overlap_checks() {
        let limits = Limits::default();

        // Leaf Affine/Copy: Whole + Uninit becomes MaybeInit, not complete, rejects re-init.
        for mode in [Mode::Copy, Mode::Affine] {
            let leaf = FieldSchema::Leaf { mode };
            for (a, b) in [
                (OutInitTree::Whole, OutInitTree::Uninit),
                (OutInitTree::Uninit, OutInitTree::Whole),
            ] {
                let mut joined = a.join(&b, &leaf).unwrap();
                assert_eq!(joined, OutInitTree::MaybeInit);
                assert!(!joined.is_complete(&leaf));
                assert_eq!(joined.check_unwind_safety(&leaf), Ok(()));
                assert_eq!(
                    joined.initialize_path(&[], &leaf, limits),
                    Err(Error::AlreadyInitialized)
                );
            }
        }

        // Partial struct: branch A initializes field 0, branch B initializes nothing.
        // After join, field 0 rejects re-init, root rejects re-init, untouched field 1 succeeds,
        // and completion remains false.
        let schema = broker_schema();
        let mut lhs = OutInitTree::new();
        lhs.initialize_path(&[FieldId(0)], &schema, limits).unwrap();
        let rhs = OutInitTree::new();
        let mut joined = lhs.join(&rhs, &schema).unwrap();
        assert!(!joined.is_complete(&schema));
        assert_eq!(
            joined.initialize_path(&[FieldId(0)], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(
            joined.initialize_path(&[], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        // Untouched sibling [1, 0] and [1, 1] can still be initialized.
        joined
            .initialize_path(&[FieldId(1), FieldId(0)], &schema, limits)
            .unwrap();
        joined
            .initialize_path(&[FieldId(1), FieldId(1)], &schema, limits)
            .unwrap();
        assert!(!joined.is_complete(&schema));

        // Joining partial post-join state with a fully initialized branch keeps field 1 Whole
        // while preserving MaybeInit on field 0.
        let joined_with_whole = joined.join(&OutInitTree::Whole, &schema).unwrap();
        assert_eq!(
            joined_with_whole,
            OutInitTree::Fields(BTreeMap::from([
                (FieldId(0), OutInitTree::MaybeInit),
                (FieldId(1), OutInitTree::Whole),
            ]))
        );
        assert!(!joined_with_whole.is_complete(&schema));

        // Whole + Uninit on a struct produces MaybeInit and rejects sub-field writes.
        let mut whole_join_uninit = OutInitTree::Whole
            .join(&OutInitTree::Uninit, &schema)
            .unwrap();
        assert_eq!(whole_join_uninit, OutInitTree::MaybeInit);
        assert!(!whole_join_uninit.is_complete(&schema));
        assert_eq!(
            whole_join_uninit.initialize_path(&[FieldId(0)], &schema, limits),
            Err(Error::AlreadyInitialized)
        );
        assert_eq!(
            whole_join_uninit.initialize_path(&[], &schema, limits),
            Err(Error::AlreadyInitialized)
        );

        // Linear fields cannot tolerate MaybeInit across join or unwind.
        let linear_schema = FieldSchema::Struct(BTreeMap::from([
            (FieldId(0), FieldSchema::Leaf { mode: Mode::Copy }),
            (FieldId(1), FieldSchema::Leaf { mode: Mode::Linear }),
        ]));
        assert_eq!(
            OutInitTree::MaybeInit.join(&OutInitTree::Uninit, &linear_schema),
            Err(Error::InconsistentLinearInitAtJoin { field: FieldId(1) })
        );
        assert_eq!(
            OutInitTree::MaybeInit.check_unwind_safety(&linear_schema),
            Err(Error::UnwindWithLiveLinearField { field: FieldId(1) })
        );
    }
}
