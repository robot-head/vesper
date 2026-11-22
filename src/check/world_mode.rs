//! `World::mode` (SH5 lane 8, #1331): the mode of a type in a world, with a
//! per-world memo of ADT expansions.
//!
//! [`reference`] is the original algorithm, kept for the differential test
//! (tests/world_mode_speed.rs). It expands an ADT instance `n[args]` (keyed
//! by `n` and the modes of its arguments) into its fields, and cuts a key
//! that is already on the current expansion path at the ADT's declared mode.
//! Because the cut set is the path, the same instance is expanded again on
//! every path that reaches it, and every caller (`Checker::mode`,
//! `ir_check::Flow::mode`) starts from scratch.
//!
//! [`mode`] gives the same answer for every query. An expansion of key `K`
//! depends only on `K`, the world, and which of the keys it reaches are on
//! the path above it (those are cut). So when an expansion cuts nothing above
//! itself, its result is exactly what a query starting at `K` gives, and the
//! memo records it with the set of keys the expansion reached. A later
//! expansion of `K` reuses the entry only when none of those keys is on its
//! path: then every cut decision, and so the result, is the same as a fresh
//! expansion's. Expansions that cut a key above themselves (members of a
//! recursive cycle below its entry point) are not recorded, and run as
//! before.
use super::World;
use crate::ast::{Mode, Type};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

/// An ADT instance: its name and the modes of its type arguments.
type Key = (String, Vec<Mode>);

/// The recorded expansions of one ADT, by argument modes.
type Row = Vec<(Vec<Mode>, Entry)>;

struct Entry {
    mode: Mode,
    /// Every key the expansion reached, itself included.
    reached: Arc<BTreeSet<Key>>,
}

/// The memo. It belongs to one [`World`]: a clone starts empty, and the memo
/// never makes two worlds unequal. A world whose `adts` change after its
/// first `mode` query must call [`World::clear_mode_cache`].
#[derive(Default)]
pub struct ModeCache {
    entries: RwLock<HashMap<String, Row>>,
}

impl ModeCache {
    fn get(&self, name: &str, args: &[Mode], path: &[Key]) -> Option<(Mode, Arc<BTreeSet<Key>>)> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let (_, entry) = entries.get(name)?.iter().find(|(a, _)| a == args)?;
        if path.iter().any(|k| entry.reached.contains(k)) {
            return None;
        }
        Some((entry.mode, entry.reached.clone()))
    }
    fn put(&self, key: Key, mode: Mode, reached: Arc<BTreeSet<Key>>) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let row = entries.entry(key.0).or_default();
        if !row.iter().any(|(a, _)| *a == key.1) {
            row.push((key.1, Entry { mode, reached }));
        }
    }
    pub(super) fn clear(&self) {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    pub(super) fn len(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(Vec::len)
            .sum()
    }
}

impl Clone for ModeCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for ModeCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for ModeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModeCache")
    }
}

/// What a type's mode is when it does not depend on ADT expansion, or the
/// ADT and the type arguments to expand.
enum Shape<'a> {
    Leaf(Mode),
    Join(&'a [Type], Mode),
    Adt(&'a crate::ast::Adt, &'a str, &'a [Type]),
}

fn shape<'a>(w: &'a World, ty: &'a Type, bounds: &BTreeMap<String, Mode>) -> Shape<'a> {
    match ty {
        Type::Infer | Type::IntLiteral(_) => Shape::Leaf(Mode::Copy),
        Type::Row(_) => Shape::Leaf(Mode::Copy),
        Type::Tuple(ts) => Shape::Join(ts, Mode::Copy),
        Type::Function { mode, .. } => Shape::Leaf(*mode),
        Type::Named(n, args) => {
            if let Some(m) = bounds.get(n) {
                return Shape::Leaf(*m);
            }
            if crate::vector::shape_of(ty).is_some() {
                return Shape::Leaf(Mode::Copy);
            }
            if crate::numeric::is_scalar_name(n) {
                return Shape::Leaf(Mode::Copy);
            }
            if n == crate::strings::STRING_TYPE || n == crate::strings::STR_TYPE {
                return Shape::Leaf(Mode::Affine);
            }
            // A trait object owns an affine value (§7.3).
            if n == crate::ast::DYN_TYPE {
                return Shape::Leaf(Mode::Affine);
            }
            // RegionBox[r, T] has the mode of T (§15.2).
            if n == crate::regions::BOX_TYPE {
                return match args.get(1) {
                    Some(t) => Shape::Join(std::slice::from_ref(t), Mode::Copy),
                    None => Shape::Leaf(Mode::Linear),
                };
            }
            // ChunkSeq[T] owns its elements; Slice[T] is a borrow (§13).
            // TiledSeq[T] is an affine AoSoA sequence container (§12.5).
            if n == crate::seq::CHUNK_SEQ_TYPE
                || n == crate::seq::SLICE_TYPE
                || n == crate::tiled::TILED_SEQ_TYPE
            {
                return match args.first() {
                    Some(t) => Shape::Join(std::slice::from_ref(t), Mode::Affine),
                    None => Shape::Leaf(Mode::Affine),
                };
            }
            match w.adts.get(n) {
                Some(a) => Shape::Adt(a, n, args),
                None => Shape::Leaf(Mode::Linear),
            }
        }
    }
}

/// The original algorithm (see the module comment), unchanged.
pub fn reference(w: &World, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
    fn visit(
        w: &World,
        ty: &Type,
        bounds: &BTreeMap<String, Mode>,
        seen: &mut BTreeSet<String>,
    ) -> Mode {
        match ty {
            Type::Infer | Type::IntLiteral(_) => Mode::Copy,
            Type::Row(_) => Mode::Copy,
            Type::Tuple(ts) => ts
                .iter()
                .fold(Mode::Copy, |m, t| m.join(visit(w, t, bounds, seen))),
            Type::Function { mode, .. } => *mode,
            Type::Named(n, args) => {
                if let Some(m) = bounds.get(n) {
                    return *m;
                }
                if crate::vector::shape_of(ty).is_some() {
                    return Mode::Copy;
                }
                if crate::numeric::is_scalar_name(n) {
                    return Mode::Copy;
                }
                if n == crate::strings::STRING_TYPE || n == crate::strings::STR_TYPE {
                    return Mode::Affine;
                }
                // A trait object owns an affine value (§7.3).
                if n == crate::ast::DYN_TYPE {
                    return Mode::Affine;
                }
                // RegionBox[r, T] has the mode of T (§15.2).
                if n == crate::regions::BOX_TYPE {
                    return args
                        .get(1)
                        .map_or(Mode::Linear, |t| visit(w, t, bounds, seen));
                }
                // ChunkSeq[T] owns its elements; Slice[T] is a borrow (§13).
                if n == crate::seq::CHUNK_SEQ_TYPE || n == crate::seq::SLICE_TYPE {
                    return args
                        .first()
                        .map_or(Mode::Affine, |t| visit(w, t, bounds, seen))
                        .join(Mode::Affine);
                }
                // TiledSeq[T] is an affine AoSoA sequence container (§12.5).
                if n == crate::tiled::TILED_SEQ_TYPE {
                    return args
                        .first()
                        .map_or(Mode::Affine, |t| visit(w, t, bounds, seen))
                        .join(Mode::Affine);
                }
                let Some(a) = w.adts.get(n) else {
                    return Mode::Linear;
                };
                let argument_modes = args
                    .iter()
                    .map(|t| visit(w, t, bounds, seen))
                    .collect::<Vec<_>>();
                let key = format!("{n}:{argument_modes:?}");
                if !seen.insert(key.clone()) {
                    return a.mode;
                }
                let component_bounds = a
                    .generics
                    .iter()
                    .zip(argument_modes)
                    .map(|(g, m)| (g.name.clone(), m))
                    .collect();
                let mut m = a.mode;
                for f in a.variants.iter().flat_map(|v| &v.fields) {
                    let intrinsic = visit(w, &f.ty, &component_bounds, seen);
                    m = m.join(f.modality.unwrap_or(intrinsic));
                }
                seen.remove(&key);
                m
            }
        }
    }
    visit(w, ty, bounds, &mut BTreeSet::new())
}

/// One query's expansion path and what the innermost open expansions
/// reached.
struct Walk<'w> {
    w: &'w World,
    path: Vec<Key>,
    /// One set per open expansion: the keys it reached so far.
    reached: Vec<BTreeSet<Key>>,
    /// One entry per open expansion: the shallowest path depth it cut at.
    cut: Vec<usize>,
}

impl Walk<'_> {
    fn visit(&mut self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
        match shape(self.w, ty, bounds) {
            Shape::Leaf(m) => m,
            Shape::Join(ts, m) => ts.iter().fold(m, |m, t| m.join(self.visit(t, bounds))),
            Shape::Adt(a, n, args) => {
                let argument_modes = args
                    .iter()
                    .map(|t| self.visit(t, bounds))
                    .collect::<Vec<_>>();
                if let Some(depth) = self
                    .path
                    .iter()
                    .position(|(k, m)| k == n && *m == argument_modes)
                {
                    // A cut: the key is on the path.
                    if let Some(top) = self.reached.last_mut() {
                        top.insert((n.to_string(), argument_modes));
                    }
                    if let Some(c) = self.cut.last_mut() {
                        *c = (*c).min(depth);
                    }
                    return a.mode;
                }
                if let Some((m, reached)) = self.w.mode_cache.get(n, &argument_modes, &self.path) {
                    if let Some(top) = self.reached.last_mut() {
                        top.extend(reached.iter().cloned());
                    }
                    return m;
                }
                let depth = self.path.len();
                let component_bounds = a
                    .generics
                    .iter()
                    .zip(argument_modes.iter().copied())
                    .map(|(g, m)| (g.name.clone(), m))
                    .collect();
                self.path.push((n.to_string(), argument_modes));
                self.reached.push(BTreeSet::new());
                self.cut.push(usize::MAX);
                let mut m = a.mode;
                for f in a.variants.iter().flat_map(|v| &v.fields) {
                    let intrinsic = self.visit(&f.ty, &component_bounds);
                    m = m.join(f.modality.unwrap_or(intrinsic));
                }
                let key = self.path.pop().expect("pushed above");
                let mut reached = self.reached.pop().expect("pushed above");
                let cut = self.cut.pop().expect("pushed above");
                reached.insert(key.clone());
                if cut < depth {
                    // It cut a key above itself: its result depends on the
                    // path, so it is not recorded, and the cut propagates.
                    if let Some(c) = self.cut.last_mut() {
                        *c = (*c).min(cut);
                    }
                    if let Some(top) = self.reached.last_mut() {
                        top.extend(reached);
                    }
                } else {
                    let reached = Arc::new(reached);
                    if let Some(top) = self.reached.last_mut() {
                        top.extend(reached.iter().cloned());
                    }
                    self.w.mode_cache.put(key, m, reached);
                }
                m
            }
        }
    }
}

pub(super) fn mode(w: &World, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
    // Types that need no ADT expansion skip the walk's allocations.
    match shape(w, ty, bounds) {
        Shape::Leaf(m) => m,
        _ => Walk {
            w,
            path: vec![],
            reached: vec![],
            cut: vec![],
        }
        .visit(ty, bounds),
    }
}

/// The number of recorded expansions (for the bench and tests).
pub(super) fn cached(w: &World) -> usize {
    w.mode_cache.len()
}
