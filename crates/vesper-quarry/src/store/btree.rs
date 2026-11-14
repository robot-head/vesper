//! B+Tree over store pages: internal nodes route by row-version key, PAX
//! leaves hold the versions. Splits propagate to the root; empty leaves are
//! unlinked and their pages freed; a root with one child collapses.
//!
//! The tree is not latch-coupled: the store is single-threaded per handle
//! and every mutation runs inside one commit group's private page staging.
use super::error::{corrupt, StoreError, StoreErrorKind, StoreResult};
use super::pages::{
    child, count, entry_bytes, internal_build, internal_insert, internal_parts, internal_remove,
    kind, layout_of, leaf_build, leaf_decode, leaf_insert, leaf_remove, leaf_search, level,
    new_page, route, Entry, Key, Page, KIND_INTERNAL, KIND_LEAF,
};
use crate::ScalarType;

/// Page access for tree code: staged (writable) or read-only.
pub(crate) trait Pager {
    fn page(&mut self, id: u64) -> StoreResult<&Page>;
    fn page_mut(&mut self, id: u64) -> StoreResult<&mut Page>;
    /// A fresh page (from the free-space map or by extending the file).
    fn alloc(&mut self) -> StoreResult<u64>;
    fn free(&mut self, id: u64) -> StoreResult<()>;
}

/// Node size limits. The hard capacities come from the page formats; lower
/// values make deep trees with few rows (used by tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeCfg {
    /// Separator keys per internal node.
    pub fanout: usize,
    /// Versions per leaf.
    pub leaf_max: usize,
}

const MAX_DEPTH: usize = 32;

fn tree_corrupt(what: &str) -> StoreError {
    corrupt(format!("B+Tree: {what}"))
}

/// Creates an empty leaf for a new table.
pub(crate) fn new_leaf(p: &mut dyn Pager, types: &[ScalarType], cfg: TreeCfg) -> StoreResult<u64> {
    let id = p.alloc()?;
    if !leaf_build(p.page_mut(id)?, id, types, &[], cfg.leaf_max) {
        return Err(tree_corrupt("empty leaf does not fit"));
    }
    Ok(id)
}

fn descend(p: &mut dyn Pager, root: u64, key: Key) -> StoreResult<(Vec<(u64, usize)>, u64)> {
    let mut path = Vec::new();
    let mut id = root;
    let mut expect: Option<u8> = None;
    loop {
        let b = p.page(id)?;
        if expect.is_some_and(|l| l != level(b)) {
            return Err(tree_corrupt("child level"));
        }
        match kind(b) {
            KIND_INTERNAL => {
                let i = route(b, key);
                expect = Some(level(b) - 1);
                path.push((id, i));
                id = child(b, i);
                if path.len() > MAX_DEPTH {
                    return Err(tree_corrupt("too deep"));
                }
            }
            KIND_LEAF => return Ok((path, id)),
            _ => return Err(tree_corrupt("unexpected page kind")),
        }
    }
}

fn check_types(b: &[u8], types: &[ScalarType]) -> StoreResult<super::pages::LeafLayout> {
    let lay = layout_of(b)?;
    if lay.types != types {
        return Err(tree_corrupt("leaf schema differs from its table"));
    }
    Ok(lay)
}

/// Inserts a new version; returns the (possibly new) root.
pub(crate) fn insert(
    p: &mut dyn Pager,
    cfg: TreeCfg,
    types: &[ScalarType],
    root: u64,
    e: Entry,
) -> StoreResult<u64> {
    let (path, leaf) = descend(p, root, e.key)?;
    let entries = {
        let b = p.page_mut(leaf)?;
        let lay = check_types(b, types)?;
        let pos = match leaf_search(b, &lay, e.key) {
            Ok(_) => return Err(tree_corrupt("duplicate row version")),
            Err(pos) => pos,
        };
        if count(b) < cfg.leaf_max && leaf_insert(b, &lay, pos, &e) {
            return Ok(root);
        }
        let (_, mut entries) = leaf_decode(b)?;
        entries.insert(pos, e);
        // A rebuild may fit (new capacity, compacted heap) without a split.
        if leaf_build(b, leaf, types, &entries, cfg.leaf_max) {
            return Ok(root);
        }
        entries
    };
    let (left, right) = split_leaf(types, &entries, cfg.leaf_max)?;
    let right_id = p.alloc()?;
    let sep = entries[left.len()].key;
    if !leaf_build(p.page_mut(right_id)?, right_id, types, right, cfg.leaf_max)
        || !leaf_build(p.page_mut(leaf)?, leaf, types, left, cfg.leaf_max)
    {
        return Err(tree_corrupt("split halves do not fit"));
    }
    propagate(p, cfg, path, sep, right_id, root)
}

/// Splits sorted entries into two buildable halves, as even in bytes as
/// possible.
fn split_leaf<'e>(
    types: &[ScalarType],
    entries: &'e [Entry],
    leaf_max: usize,
) -> StoreResult<(&'e [Entry], &'e [Entry])> {
    let sizes: Vec<usize> = entries.iter().map(|e| entry_bytes(types, e)).collect();
    let total: usize = sizes.iter().sum();
    let mut prefix = Vec::with_capacity(entries.len() + 1);
    prefix.push(0usize);
    for s in &sizes {
        prefix.push(prefix.last().expect("seeded") + s);
    }
    let mut ks: Vec<usize> = (1..entries.len()).collect();
    ks.sort_by_key(|&k| (2 * prefix[k]).abs_diff(total));
    let mut scratch = new_page();
    for k in ks {
        let (l, r) = entries.split_at(k);
        if l.len() <= leaf_max
            && r.len() <= leaf_max
            && leaf_build(&mut scratch[..], 0, types, l, leaf_max)
            && leaf_build(&mut scratch[..], 0, types, r, leaf_max)
        {
            return Ok((l, r));
        }
    }
    Err(StoreError::new(
        StoreErrorKind::Limit,
        "row versions too large to split a leaf",
    ))
}

fn propagate(
    p: &mut dyn Pager,
    cfg: TreeCfg,
    mut path: Vec<(u64, usize)>,
    mut sep: Key,
    mut right: u64,
    root: u64,
) -> StoreResult<u64> {
    while let Some((pid, i)) = path.pop() {
        let (lvl, children, keys) = {
            let b = p.page_mut(pid)?;
            if count(b) < cfg.fanout {
                internal_insert(b, i, sep, right);
                return Ok(root);
            }
            let (mut children, mut keys) = internal_parts(b);
            keys.insert(i, sep);
            children.insert(i + 1, right);
            (level(b), children, keys)
        };
        let mid = keys.len() / 2;
        let rid = p.alloc()?;
        internal_build(p.page_mut(pid)?, pid, lvl, &children[..=mid], &keys[..mid]);
        internal_build(
            p.page_mut(rid)?,
            rid,
            lvl,
            &children[mid + 1..],
            &keys[mid + 1..],
        );
        sep = keys[mid];
        right = rid;
    }
    let lvl = level(p.page(root)?);
    if usize::from(lvl) + 1 > MAX_DEPTH {
        return Err(tree_corrupt("too deep"));
    }
    let new_root = p.alloc()?;
    internal_build(
        p.page_mut(new_root)?,
        new_root,
        lvl + 1,
        &[root, right],
        &[sep],
    );
    Ok(new_root)
}

/// Removes one version. Returns the (possibly new) root and whether the key
/// existed. Empty non-root leaves are unlinked and freed.
pub(crate) fn remove(
    p: &mut dyn Pager,
    cfg: TreeCfg,
    types: &[ScalarType],
    root: u64,
    key: Key,
) -> StoreResult<(u64, bool)> {
    let (mut path, leaf) = descend(p, root, key)?;
    let empty = {
        let b = p.page_mut(leaf)?;
        let lay = check_types(b, types)?;
        let Ok(pos) = leaf_search(b, &lay, key) else {
            return Ok((root, false));
        };
        leaf_remove(b, &lay, pos);
        count(b) == 0
    };
    if !empty || leaf == root {
        return Ok((root, true));
    }
    p.free(leaf)?;
    while let Some((pid, i)) = path.pop() {
        let b = p.page_mut(pid)?;
        if count(b) > 0 {
            internal_remove(b, i);
            break;
        }
        // `pid` had only this child.
        if pid == root {
            if !leaf_build(b, pid, types, &[], cfg.leaf_max) {
                return Err(tree_corrupt("empty root leaf does not fit"));
            }
            return Ok((root, true));
        }
        p.free(pid)?;
    }
    let mut root = root;
    loop {
        let b = p.page(root)?;
        if kind(b) == KIND_INTERNAL && count(b) == 0 {
            let only = child(b, 0);
            p.free(root)?;
            root = only;
        } else {
            return Ok((root, true));
        }
    }
}

/// Visits versions with `lo <= key <= hi` in key order until `f` returns
/// `false`.
pub(crate) fn visit(
    p: &mut dyn Pager,
    root: u64,
    lo: Key,
    hi: Key,
    f: &mut dyn FnMut(Entry) -> StoreResult<bool>,
) -> StoreResult<()> {
    visit_at(p, root, lo, hi, None, 0, f).map(drop)
}
fn visit_at(
    p: &mut dyn Pager,
    id: u64,
    lo: Key,
    hi: Key,
    expect: Option<u8>,
    depth: usize,
    f: &mut dyn FnMut(Entry) -> StoreResult<bool>,
) -> StoreResult<bool> {
    if depth > MAX_DEPTH {
        return Err(tree_corrupt("too deep"));
    }
    enum Node {
        Inner(Vec<u64>, u8),
        Leaf(Vec<Entry>),
    }
    let node = {
        let b = p.page(id)?;
        if expect.is_some_and(|l| l != level(b)) {
            return Err(tree_corrupt("child level"));
        }
        match kind(b) {
            KIND_INTERNAL => {
                let (from, to) = (route(b, lo), route(b, hi));
                Node::Inner((from..=to).map(|i| child(b, i)).collect(), level(b) - 1)
            }
            KIND_LEAF => {
                let (_, entries) = leaf_decode(b)?;
                Node::Leaf(
                    entries
                        .into_iter()
                        .filter(|e| lo <= e.key && e.key <= hi)
                        .collect(),
                )
            }
            _ => return Err(tree_corrupt("unexpected page kind")),
        }
    };
    match node {
        Node::Inner(children, lvl) => {
            for c in children {
                if !visit_at(p, c, lo, hi, Some(lvl), depth + 1, f)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Node::Leaf(entries) => {
            for e in entries {
                if !f(e)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// Every page of the tree (for dropping a table).
pub(crate) fn pages(p: &mut dyn Pager, root: u64, out: &mut Vec<u64>) -> StoreResult<()> {
    let mut stack = vec![(root, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        if depth > MAX_DEPTH || out.len() > 1 << 20 {
            return Err(tree_corrupt("too deep"));
        }
        out.push(id);
        let b = p.page(id)?;
        if kind(b) == KIND_INTERNAL {
            for i in 0..=count(b) {
                stack.push((child(b, i), depth + 1));
            }
        }
    }
    Ok(())
}

/// Shape of a validated tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeShape {
    pub height: usize,
    pub leaves: usize,
    pub internal: usize,
    pub versions: usize,
}
/// Validates the whole tree: uniform leaf depth, child levels, every key
/// inside its separators, leaf schemas, and no page reached twice.
pub(crate) fn check(p: &mut dyn Pager, root: u64, types: &[ScalarType]) -> StoreResult<TreeShape> {
    let mut shape = TreeShape::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut leaf_depth = None;
    check_at(
        p,
        root,
        Key::MIN,
        None,
        0,
        types,
        &mut shape,
        &mut seen,
        &mut leaf_depth,
    )?;
    shape.height = leaf_depth.unwrap_or(0) + 1;
    Ok(shape)
}
#[allow(clippy::too_many_arguments)]
fn check_at(
    p: &mut dyn Pager,
    id: u64,
    lo: Key,
    hi: Option<Key>,
    depth: usize,
    types: &[ScalarType],
    shape: &mut TreeShape,
    seen: &mut std::collections::BTreeSet<u64>,
    leaf_depth: &mut Option<usize>,
) -> StoreResult<()> {
    if depth > MAX_DEPTH || !seen.insert(id) {
        return Err(tree_corrupt("cycle or too deep"));
    }
    let b = p.page(id)?;
    match kind(b) {
        KIND_INTERNAL => {
            shape.internal += 1;
            let (children, keys) = internal_parts(b);
            for (i, &c) in children.iter().enumerate() {
                let clo = if i == 0 { lo } else { keys[i - 1] };
                let chi = keys.get(i).copied().or(hi);
                if i > 0 && (keys[i - 1] < lo || hi.is_some_and(|h| keys[i - 1] >= h)) {
                    return Err(tree_corrupt("separator outside its range"));
                }
                check_at(p, c, clo, chi, depth + 1, types, shape, seen, leaf_depth)?;
            }
            Ok(())
        }
        KIND_LEAF => {
            let (lay, entries) = leaf_decode(b)?;
            if lay.types != types {
                return Err(tree_corrupt("leaf schema"));
            }
            if entries
                .iter()
                .any(|e| e.key < lo || hi.is_some_and(|h| e.key >= h))
            {
                return Err(tree_corrupt("key outside its separators"));
            }
            if *leaf_depth.get_or_insert(depth) != depth {
                return Err(tree_corrupt("leaves at different depths"));
            }
            shape.leaves += 1;
            shape.versions += entries.len();
            Ok(())
        }
        _ => Err(tree_corrupt("unexpected page kind")),
    }
}
