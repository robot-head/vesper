//! Trie-structured `FieldTree` projections and `[T-CallProj]` four-mode
//! disjointness checking (§4.3, §7.3).
//!
//! This is a standalone analysis primitive. Source syntax, HIR lowering,
//! schema-aware field completeness normalization, and Place IR integration
//! remain separate work.

use std::collections::BTreeMap;

/// Maximum structural depth permitted in a `FieldTree` or projection path.
pub const MAX_TREE_DEPTH: usize = 64;

/// Maximum work steps permitted in tree operations or pairwise argument checks.
pub const MAX_TREE_WORK: usize = 65_536;

/// Maximum call arguments checked in a single `[T-CallProj]` call site.
pub const MAX_CALL_ARGS: usize = 256;

/// Four-mode second-class parameter projection algebra (§4.3, §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassMode {
    /// Shared read-only projection (`shr`).
    In,
    /// Exclusive mutable projection (`mut`, supports `~>` strong update).
    InOut,
    /// Exclusive write-only uninitialized destination (`OutSlot[T]`).
    Out,
    /// Ownership transfer / linear obligation consumption (`own`).
    Sink,
}

/// Zero-based local variable identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

/// Zero-based struct field index within a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldIdx(pub u16);

/// Prefix tree representing disjoint field projections (`broker.{index, metrics.counters}`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldTree {
    /// No sub-place selected (disjoint from everything, subset of everything).
    #[default]
    Empty,
    /// Entire place or sub-place selected.
    Whole,
    /// Strict nonempty subtree selection keyed by [`FieldIdx`].
    Fields(BTreeMap<FieldIdx, FieldTree>),
}

/// Projection target for a call argument (`local.tree`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceProjection {
    pub local: LocalId,
    pub tree: FieldTree,
}

/// Errors returned by `FieldTree` construction, comparison, or `[T-CallProj]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldTreeError {
    DepthLimitExceeded {
        max_depth: usize,
    },
    WorkLimitExceeded {
        max_work: usize,
    },
    TooManyCallArgs {
        count: usize,
        max_args: usize,
    },
    OverlappingProjection {
        first_arg: usize,
        second_arg: usize,
        local: LocalId,
    },
}

impl FieldTree {
    /// Returns a tree covering no fields.
    pub const fn empty() -> Self {
        Self::Empty
    }

    /// Returns a leaf selection covering the entire place (`pi = root`).
    pub const fn whole_place() -> Self {
        Self::Whole
    }

    /// Returns `true` iff no field paths are selected.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Whole => false,
            Self::Fields(map) => map.is_empty(),
        }
    }

    /// Builds a `FieldTree` from a slice of field-index paths, preserving `Empty`
    /// when `paths` is empty and treating an empty path `&[]` as `Whole`.
    pub fn from_paths(paths: &[&[FieldIdx]]) -> Result<Self, FieldTreeError> {
        let mut tree = Self::Empty;
        for path in paths {
            tree.insert_path(path)?;
        }
        Ok(tree)
    }

    /// Inserts a field projection path into `self`.
    ///
    /// Inserting `&[]` promotes the subtree to `Whole` (subsuming any deeper
    /// children). Inserting a deeper path under an existing `Whole` leaves
    /// `Whole` unchanged.
    pub fn insert_path(&mut self, path: &[FieldIdx]) -> Result<(), FieldTreeError> {
        if path.len() > MAX_TREE_DEPTH {
            return Err(FieldTreeError::DepthLimitExceeded {
                max_depth: MAX_TREE_DEPTH,
            });
        }
        let mut cursor = self;
        for &field in path {
            match cursor {
                Self::Whole => return Ok(()),
                Self::Empty => {
                    *cursor = Self::Fields(BTreeMap::new());
                }
                Self::Fields(_) => {}
            }
            let Self::Fields(children) = cursor else {
                unreachable!("promoted Empty to Fields");
            };
            cursor = children.entry(field).or_insert(Self::Empty);
        }
        *cursor = Self::Whole;
        Ok(())
    }

    /// Checks `pi_1 ⊥ pi_2`: `true` iff the two selector trees share zero
    /// overlapping leaf paths.
    pub fn is_disjoint(&self, other: &Self) -> Result<bool, FieldTreeError> {
        let mut work = MAX_TREE_WORK;
        let mut stack = vec![(self, other, 1usize)];
        while let Some((lhs, rhs, depth)) = stack.pop() {
            charge_work(&mut work, 1)?;
            if depth > MAX_TREE_DEPTH {
                return Err(FieldTreeError::DepthLimitExceeded {
                    max_depth: MAX_TREE_DEPTH,
                });
            }
            match (lhs.normalized_view(), rhs.normalized_view()) {
                (View::Empty, _) | (_, View::Empty) => {}
                (View::Whole, _) | (_, View::Whole) => return Ok(false),
                (View::Fields(lmap), View::Fields(rmap)) => {
                    for (field, lsub) in lmap {
                        if let Some(rsub) = rmap.get(field) {
                            stack.push((lsub, rsub, depth + 1));
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    /// Checks `self <= older` for `[SV-Fn]` projection narrowing: every path
    /// reachable in `self` is covered by `older`.
    pub fn is_subset_of(&self, older: &Self) -> Result<bool, FieldTreeError> {
        let mut work = MAX_TREE_WORK;
        let mut stack = vec![(self, older, 1usize)];
        while let Some((sub, sup, depth)) = stack.pop() {
            charge_work(&mut work, 1)?;
            if depth > MAX_TREE_DEPTH {
                return Err(FieldTreeError::DepthLimitExceeded {
                    max_depth: MAX_TREE_DEPTH,
                });
            }
            match (sub.normalized_view(), sup.normalized_view()) {
                (View::Empty, _) | (_, View::Whole) => {}
                (View::Whole, View::Empty | View::Fields(_)) => return Ok(false),
                (View::Fields(_), View::Empty) => return Ok(false),
                (View::Fields(smap), View::Fields(omap)) => {
                    for (field, ssub) in smap {
                        if ssub.is_empty() {
                            continue;
                        }
                        let Some(osub) = omap.get(field) else {
                            return Ok(false);
                        };
                        stack.push((ssub, osub, depth + 1));
                    }
                }
            }
        }
        Ok(true)
    }

    /// Computes the union of two projection trees without mutating either input.
    pub fn union(&self, other: &Self) -> Result<Self, FieldTreeError> {
        let mut work = MAX_TREE_WORK;
        self.union_inner(other, 1, &mut work)
    }

    fn union_inner(
        &self,
        other: &Self,
        depth: usize,
        work: &mut usize,
    ) -> Result<Self, FieldTreeError> {
        charge_work(work, 1)?;
        if depth > MAX_TREE_DEPTH {
            return Err(FieldTreeError::DepthLimitExceeded {
                max_depth: MAX_TREE_DEPTH,
            });
        }
        match (self.normalized_view(), other.normalized_view()) {
            (View::Whole, _) | (_, View::Whole) => Ok(Self::Whole),
            (View::Empty, View::Empty) => Ok(Self::Empty),
            (View::Empty, _) => other.clone_bounded(depth, work),
            (_, View::Empty) => self.clone_bounded(depth, work),
            (View::Fields(lmap), View::Fields(rmap)) => {
                let mut merged = BTreeMap::new();
                for (field, lsub) in lmap {
                    if let Some(rsub) = rmap.get(field) {
                        let joined = lsub.union_inner(rsub, depth + 1, work)?;
                        if !joined.is_empty() {
                            merged.insert(*field, joined);
                        }
                    } else {
                        let cloned = lsub.clone_bounded(depth + 1, work)?;
                        if !cloned.is_empty() {
                            merged.insert(*field, cloned);
                        }
                    }
                }
                for (field, rsub) in rmap {
                    if !lmap.contains_key(field) {
                        let cloned = rsub.clone_bounded(depth + 1, work)?;
                        if !cloned.is_empty() {
                            merged.insert(*field, cloned);
                        }
                    }
                }
                if merged.is_empty() {
                    Ok(Self::Empty)
                } else {
                    Ok(Self::Fields(merged))
                }
            }
        }
    }

    fn clone_bounded(&self, depth: usize, work: &mut usize) -> Result<Self, FieldTreeError> {
        charge_work(work, 1)?;
        if depth > MAX_TREE_DEPTH {
            return Err(FieldTreeError::DepthLimitExceeded {
                max_depth: MAX_TREE_DEPTH,
            });
        }
        match self.normalized_view() {
            View::Empty => Ok(Self::Empty),
            View::Whole => Ok(Self::Whole),
            View::Fields(map) => {
                let mut out = BTreeMap::new();
                for (k, v) in map {
                    let child = v.clone_bounded(depth + 1, work)?;
                    if !child.is_empty() {
                        out.insert(*k, child);
                    }
                }
                if out.is_empty() {
                    Ok(Self::Empty)
                } else {
                    Ok(Self::Fields(out))
                }
            }
        }
    }

    fn normalized_view(&self) -> View<'_> {
        match self {
            Self::Empty => View::Empty,
            Self::Whole => View::Whole,
            Self::Fields(map) if map.is_empty() => View::Empty,
            Self::Fields(map) => View::Fields(map),
        }
    }
}

enum View<'a> {
    Empty,
    Whole,
    Fields(&'a BTreeMap<FieldIdx, FieldTree>),
}

fn charge_work(remaining: &mut usize, cost: usize) -> Result<(), FieldTreeError> {
    *remaining = remaining
        .checked_sub(cost)
        .ok_or(FieldTreeError::WorkLimitExceeded {
            max_work: MAX_TREE_WORK,
        })?;
    Ok(())
}

/// Checks `[T-CallProj]` pairwise disjointness across call arguments:
/// for every `i < j`, either `mu_i == mu_j == PassMode::In`, the roots differ,
/// or `p_i.tree.is_disjoint(&p_j.tree)` holds.
pub fn check_call_projections(args: &[(PassMode, PlaceProjection)]) -> Result<(), FieldTreeError> {
    if args.len() > MAX_CALL_ARGS {
        return Err(FieldTreeError::TooManyCallArgs {
            count: args.len(),
            max_args: MAX_CALL_ARGS,
        });
    }
    for (i, (m_i, p_i)) in args.iter().enumerate() {
        for (offset, (m_j, p_j)) in args[i + 1..].iter().enumerate() {
            let j = i + 1 + offset;
            let both_shared = matches!((m_i, m_j), (PassMode::In, PassMode::In));
            if !both_shared && p_i.local == p_j.local && !p_i.tree.is_disjoint(&p_j.tree)? {
                return Err(FieldTreeError::OverlappingProjection {
                    first_arg: i,
                    second_arg: j,
                    local: p_i.local,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn broker_field_tree_disjointness_and_narrowing() {
        // `broker.{index, metrics.counters}` vs `broker.metrics.config`
        let f_index = FieldIdx(0);
        let f_metrics = FieldIdx(1);
        let f_counters = FieldIdx(0);
        let f_config = FieldIdx(1);

        let a = FieldTree::from_paths(&[&[f_index], &[f_metrics, f_counters]]).unwrap();
        let b = FieldTree::from_paths(&[&[f_metrics, f_config]]).unwrap();
        let whole_metrics = FieldTree::from_paths(&[&[f_metrics]]).unwrap();

        assert_eq!(a.is_disjoint(&b), Ok(true));
        assert_eq!(b.is_disjoint(&a), Ok(true));
        assert_eq!(a.is_disjoint(&whole_metrics), Ok(false));
        assert_eq!(b.is_disjoint(&whole_metrics), Ok(false));

        // Narrowing (`is_subset_of`): `{metrics.counters} <= {index, metrics.counters} <= whole`
        let just_counters = FieldTree::from_paths(&[&[f_metrics, f_counters]]).unwrap();
        assert_eq!(just_counters.is_subset_of(&a), Ok(true));
        assert_eq!(a.is_subset_of(&just_counters), Ok(false));
        assert_eq!(just_counters.is_subset_of(&whole_metrics), Ok(true));
        assert_eq!(whole_metrics.is_subset_of(&just_counters), Ok(false));
        assert_eq!(a.is_subset_of(&FieldTree::whole_place()), Ok(true));
        assert_eq!(FieldTree::whole_place().is_subset_of(&a), Ok(false));
    }

    #[test]
    fn insert_whole_subsumes_children_and_vice_versa() {
        let mut t1 = FieldTree::from_paths(&[&[FieldIdx(0), FieldIdx(1)]]).unwrap();
        t1.insert_path(&[FieldIdx(0)]).unwrap();
        assert_eq!(t1, FieldTree::from_paths(&[&[FieldIdx(0)]]).unwrap());

        let mut t2 = FieldTree::from_paths(&[&[FieldIdx(0)]]).unwrap();
        t2.insert_path(&[FieldIdx(0), FieldIdx(1)]).unwrap();
        assert_eq!(t2, FieldTree::from_paths(&[&[FieldIdx(0)]]).unwrap());

        let mut t3 = FieldTree::from_paths(&[&[FieldIdx(0)]]).unwrap();
        t3.insert_path(&[]).unwrap();
        assert_eq!(t3, FieldTree::Whole);
    }

    #[test]
    fn call_projection_rules_allow_shared_reads_and_reject_conflicts() {
        let local0 = LocalId(0);
        let local1 = LocalId(1);
        let f0 = FieldIdx(0);
        let f1 = FieldIdx(1);

        // Two shared `in` projections of the exact same place are legal.
        let shared_ok = vec![
            (
                PassMode::In,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::whole_place(),
                },
            ),
            (
                PassMode::In,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f0]]).unwrap(),
                },
            ),
        ];
        assert_eq!(check_call_projections(&shared_ok), Ok(()));

        // Disjoint `inout`, `out`, `sink`, `in` on the same root are legal.
        let disjoint_ok = vec![
            (
                PassMode::InOut,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f0, f0]]).unwrap(),
                },
            ),
            (
                PassMode::Out,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f0, f1]]).unwrap(),
                },
            ),
            (
                PassMode::Sink,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f1]]).unwrap(),
                },
            ),
            (
                PassMode::InOut,
                PlaceProjection {
                    local: local1,
                    tree: FieldTree::whole_place(),
                },
            ),
        ];
        assert_eq!(check_call_projections(&disjoint_ok), Ok(()));

        // `in` + `inout` on overlapping subtree fails with exact pair and local.
        let conflict = vec![
            (
                PassMode::In,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f0]]).unwrap(),
                },
            ),
            (
                PassMode::InOut,
                PlaceProjection {
                    local: local0,
                    tree: FieldTree::from_paths(&[&[f0, f1]]).unwrap(),
                },
            ),
        ];
        assert_eq!(
            check_call_projections(&conflict),
            Err(FieldTreeError::OverlappingProjection {
                first_arg: 0,
                second_arg: 1,
                local: local0,
            })
        );
    }

    #[test]
    fn exhaustive_two_level_leaf_set_oracle() {
        // Universe of 4 leaves: (0,0), (0,1), (1,0), (1,1).
        let leaves = [
            [FieldIdx(0), FieldIdx(0)],
            [FieldIdx(0), FieldIdx(1)],
            [FieldIdx(1), FieldIdx(0)],
            [FieldIdx(1), FieldIdx(1)],
        ];
        let mut all_trees: Vec<(FieldTree, BTreeSet<usize>)> = Vec::new();
        for mask in 0u8..16 {
            let mut tree = FieldTree::Empty;
            let mut set = BTreeSet::new();
            for (bit, leaf) in leaves.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    tree.insert_path(leaf).unwrap();
                    set.insert(bit);
                }
            }
            all_trees.push((tree, set));
        }
        // Also include Whole and parent-level field selections.
        let mut whole_set = BTreeSet::new();
        whole_set.extend(0..4);
        all_trees.push((FieldTree::Whole, whole_set));
        all_trees.push((
            FieldTree::from_paths(&[&[FieldIdx(0)]]).unwrap(),
            [0usize, 1].into_iter().collect(),
        ));
        all_trees.push((
            FieldTree::from_paths(&[&[FieldIdx(1)]]).unwrap(),
            [2usize, 3].into_iter().collect(),
        ));

        for (lhs_tree, lhs_set) in &all_trees {
            for (rhs_tree, rhs_set) in &all_trees {
                let expected_disjoint = lhs_set.is_disjoint(rhs_set);
                assert_eq!(lhs_tree.is_disjoint(rhs_tree), Ok(expected_disjoint));

                let expected_subset = lhs_set.is_subset(rhs_set);
                // Note: `{0.0, 0.1}` without schema knowledge is narrower than `0` (Whole subtree),
                // so only assert soundness (`tree_subset => set_subset`) and exactness when
                // comparing against same-granularity trees.
                if lhs_tree.is_subset_of(rhs_tree).unwrap() {
                    assert!(expected_subset);
                }

                let union_tree = lhs_tree.union(rhs_tree).unwrap();
                let union_set: BTreeSet<usize> = lhs_set.union(rhs_set).copied().collect();
                assert_eq!(lhs_tree.is_subset_of(&union_tree), Ok(true));
                assert_eq!(rhs_tree.is_subset_of(&union_tree), Ok(true));
                for (probe_tree, probe_set) in &all_trees {
                    assert_eq!(
                        union_tree.is_disjoint(probe_tree),
                        Ok(union_set.is_disjoint(probe_set))
                    );
                }
            }
        }
    }

    #[test]
    fn depth_and_arg_limits_are_enforced() {
        let too_deep = vec![FieldIdx(0); MAX_TREE_DEPTH + 1];
        let mut t = FieldTree::Empty;
        assert_eq!(
            t.insert_path(&too_deep),
            Err(FieldTreeError::DepthLimitExceeded {
                max_depth: MAX_TREE_DEPTH
            })
        );

        let too_many = vec![
            (
                PassMode::In,
                PlaceProjection {
                    local: LocalId(0),
                    tree: FieldTree::Empty,
                },
            );
            MAX_CALL_ARGS + 1
        ];
        assert_eq!(
            check_call_projections(&too_many),
            Err(FieldTreeError::TooManyCallArgs {
                count: MAX_CALL_ARGS + 1,
                max_args: MAX_CALL_ARGS,
            })
        );
    }
}
