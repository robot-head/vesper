//! `CacheBTree[K, V, M]`: a cache-line B+Tree with 16-key nodes (§13.5).
//!
//! Nodes live in two slabs (`Vec`s of leaves and of inner nodes) and refer to
//! each other through `u32` slab indices instead of pointers, so a whole tree
//! is relocatable by a plain move or `memcpy` of its slabs and nothing is
//! pinned. Every node stores up to [`CACHE_BTREE_B`] keys in a fixed array;
//! search computes the rank of the probe as a branchless count over all 16
//! lanes (`sum((i < len) & (key[i] < probe))`), which the compiler can
//! vectorize for integer keys. Leaves are chained in key order for iteration.
//!
//! The ordering is a zero-sized *model brand* `M: KeyOrder<K>` in the type
//! (defaulting to [`Canonical`], which uses `Ord`), so trees over the same key
//! type but different orders are distinct types even though their layouts
//! are identical:
//!
//! ```compile_fail
//! use vesper_alloc::{CacheBTree, Canonical, ReverseOrder};
//! let a: CacheBTree<u64, u8> = CacheBTree::new();
//! let b: CacheBTree<u64, u8, ReverseOrder<Canonical>> = a;
//! ```
//!
//! Keys are `Copy + Default` (unused lanes hold `K::default()` and are masked
//! out of every comparison); values are arbitrary. Freed nodes are recycled
//! through intrusive free lists, so once the slabs have grown to the working
//! set, insert/remove churn allocates nothing. The node count is bounded by
//! [`CacheBTreeConfig::max_len`]; exceeding it is a typed error.
#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Bound, RangeBounds};

/// Keys per node (the `B = 16` of the design).
pub const CACHE_BTREE_B: usize = 16;
const B: usize = CACHE_BTREE_B;
const MIN: usize = B / 2;
const NONE: u32 = u32::MAX;
/// Upper bound on the tree height (16^12 > u32::MAX entries per slab).
const MAX_HEIGHT: usize = 16;

/// A total order on `K`, used as a zero-sized type-level brand.
pub trait KeyOrder<K> {
    fn cmp(a: &K, b: &K) -> Ordering;
}

/// The canonical order of `K` (`Ord`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Canonical;

impl<K: Ord> KeyOrder<K> for Canonical {
    #[inline(always)]
    fn cmp(a: &K, b: &K) -> Ordering {
        a.cmp(b)
    }
}

/// The reverse of the order `O`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ReverseOrder<O>(PhantomData<O>);

impl<K, O: KeyOrder<K>> KeyOrder<K> for ReverseOrder<O> {
    #[inline(always)]
    fn cmp(a: &K, b: &K) -> Ordering {
        O::cmp(b, a)
    }
}

/// Limits of a [`CacheBTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheBTreeConfig {
    /// Largest number of entries (at most `u32::MAX / 2`).
    pub max_len: usize,
}

impl Default for CacheBTreeConfig {
    fn default() -> Self {
        Self {
            max_len: (u32::MAX / 2) as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheBTreeError {
    InvalidConfig(&'static str),
    /// `max_len` entries are already stored.
    Full {
        max_len: usize,
    },
    /// The node slabs could not grow.
    OutOfMemory,
}

impl fmt::Display for CacheBTreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CacheBTreeError {}

/// Failed insert: the key and value are returned.
#[derive(Debug, PartialEq, Eq)]
pub struct CacheBTreeInsertError<K, V> {
    pub key: K,
    pub value: V,
    pub error: CacheBTreeError,
}

/// Slab and shape counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheBTreeStats {
    pub len: usize,
    pub height: usize,
    pub leaves: usize,
    pub inners: usize,
    pub free_leaves: usize,
    pub free_inners: usize,
}

#[derive(Clone)]
#[repr(align(64))]
struct Leaf<K, V> {
    keys: [K; B],
    vals: [Option<V>; B],
    len: u8,
    /// Next leaf in key order, or the next free leaf while on the free list.
    next: u32,
}

#[derive(Clone)]
#[repr(align(64))]
struct Inner<K> {
    /// `keys[i]` (for `i >= 1`) is a lower bound of every key under
    /// `children[i]` and an upper bound (exclusive) for `children[i - 1]`.
    /// `keys[0]` is not used for routing.
    keys: [K; B],
    /// Child slab indices; `children[0]` links the free list while freed.
    children: [u32; B],
    len: u8,
}

/// Rank of `probe`: the number of the first `len` keys that are `< probe`
/// (`strict`) or `<= probe`. Branchless over all 16 lanes.
#[inline(always)]
fn rank<K, O: KeyOrder<K>>(keys: &[K; B], len: usize, probe: &K, strict: bool) -> usize {
    let mut n = 0usize;
    for (i, k) in keys.iter().enumerate() {
        let c = O::cmp(k, probe);
        let below = (c == Ordering::Less) | (!strict & (c == Ordering::Equal));
        n += ((i < len) & below) as usize;
    }
    n
}

/// A B+Tree map with 16-key cache-aligned nodes and `u32` node indices.
pub struct CacheBTree<K: Copy + Default, V, O: KeyOrder<K> = Canonical> {
    leaves: Vec<Leaf<K, V>>,
    inners: Vec<Inner<K>>,
    free_leaf: u32,
    free_inner: u32,
    free_leaves: usize,
    free_inners: usize,
    /// Root slab index; a leaf when `height == 0`.
    root: u32,
    height: usize,
    len: usize,
    max_len: usize,
    _order: PhantomData<fn() -> O>,
}

impl<K: Copy + Default, V: Clone, O: KeyOrder<K>> Clone for CacheBTree<K, V, O> {
    fn clone(&self) -> Self {
        Self {
            leaves: self.leaves.clone(),
            inners: self.inners.clone(),
            free_leaf: self.free_leaf,
            free_inner: self.free_inner,
            free_leaves: self.free_leaves,
            free_inners: self.free_inners,
            root: self.root,
            height: self.height,
            len: self.len,
            max_len: self.max_len,
            _order: PhantomData,
        }
    }
}

impl<K: Copy + Default + fmt::Debug, V: fmt::Debug, O: KeyOrder<K>> fmt::Debug
    for CacheBTree<K, V, O>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Copy + Default, V, O: KeyOrder<K>> Default for CacheBTree<K, V, O> {
    fn default() -> Self {
        Self::new()
    }
}

fn empty_leaf<K: Copy + Default, V>() -> Leaf<K, V> {
    Leaf {
        keys: [K::default(); B],
        vals: std::array::from_fn(|_| None),
        len: 0,
        next: NONE,
    }
}

fn empty_inner<K: Copy + Default>() -> Inner<K> {
    Inner {
        keys: [K::default(); B],
        children: [NONE; B],
        len: 0,
    }
}

/// One step of a root-to-leaf path: inner node and the child slot taken.
#[derive(Clone, Copy, Default)]
struct Step {
    node: u32,
    slot: usize,
}

struct Path {
    steps: [Step; MAX_HEIGHT],
    depth: usize,
}

impl<K: Copy + Default, V, O: KeyOrder<K>> CacheBTree<K, V, O> {
    /// An empty tree. The first insert allocates the root leaf.
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            inners: Vec::new(),
            free_leaf: NONE,
            free_inner: NONE,
            free_leaves: 0,
            free_inners: 0,
            root: NONE,
            height: 0,
            len: 0,
            max_len: CacheBTreeConfig::default().max_len,
            _order: PhantomData,
        }
    }

    pub fn with_config(config: CacheBTreeConfig) -> Result<Self, CacheBTreeError> {
        if config.max_len == 0 || config.max_len > (u32::MAX / 2) as usize {
            return Err(CacheBTreeError::InvalidConfig(
                "max_len must be 1..=u32::MAX/2",
            ));
        }
        Ok(Self {
            max_len: config.max_len,
            ..Self::new()
        })
    }

    /// Reserves node slabs for `entries` entries at worst-case (half) fill.
    pub fn reserve(&mut self, entries: usize) -> Result<(), CacheBTreeError> {
        let leaves = entries.div_ceil(MIN).max(1);
        let inners = leaves.div_ceil(MIN - 1) + MAX_HEIGHT;
        self.leaves
            .try_reserve(leaves.saturating_sub(self.leaves.len()))
            .map_err(|_| CacheBTreeError::OutOfMemory)?;
        self.inners
            .try_reserve(inners.saturating_sub(self.inners.len()))
            .map_err(|_| CacheBTreeError::OutOfMemory)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn stats(&self) -> CacheBTreeStats {
        CacheBTreeStats {
            len: self.len,
            height: if self.root == NONE {
                0
            } else {
                self.height + 1
            },
            leaves: self.leaves.len() - self.free_leaves,
            inners: self.inners.len() - self.free_inners,
            free_leaves: self.free_leaves,
            free_inners: self.free_inners,
        }
    }

    /// Removes every entry, keeping the slabs for reuse.
    pub fn clear(&mut self) {
        self.leaves.clear();
        self.inners.clear();
        self.free_leaf = NONE;
        self.free_inner = NONE;
        self.free_leaves = 0;
        self.free_inners = 0;
        self.root = NONE;
        self.height = 0;
        self.len = 0;
    }

    fn alloc_leaf(&mut self) -> Result<u32, CacheBTreeError> {
        if self.free_leaf != NONE {
            let i = self.free_leaf;
            self.free_leaf = self.leaves[i as usize].next;
            self.free_leaves -= 1;
            self.leaves[i as usize].next = NONE;
            return Ok(i);
        }
        if self.leaves.len() >= NONE as usize {
            return Err(CacheBTreeError::OutOfMemory);
        }
        self.leaves
            .try_reserve(1)
            .map_err(|_| CacheBTreeError::OutOfMemory)?;
        self.leaves.push(empty_leaf());
        Ok((self.leaves.len() - 1) as u32)
    }

    fn free_leaf_node(&mut self, i: u32) {
        let leaf = &mut self.leaves[i as usize];
        debug_assert!(leaf.len == 0);
        leaf.next = self.free_leaf;
        self.free_leaf = i;
        self.free_leaves += 1;
    }

    fn alloc_inner(&mut self) -> Result<u32, CacheBTreeError> {
        if self.free_inner != NONE {
            let i = self.free_inner;
            self.free_inner = self.inners[i as usize].children[0];
            self.free_inners -= 1;
            self.inners[i as usize] = empty_inner();
            return Ok(i);
        }
        if self.inners.len() >= NONE as usize {
            return Err(CacheBTreeError::OutOfMemory);
        }
        self.inners
            .try_reserve(1)
            .map_err(|_| CacheBTreeError::OutOfMemory)?;
        self.inners.push(empty_inner());
        Ok((self.inners.len() - 1) as u32)
    }

    fn free_inner_node(&mut self, i: u32) {
        let node = &mut self.inners[i as usize];
        node.len = 0;
        node.children[0] = self.free_inner;
        self.free_inner = i;
        self.free_inners += 1;
    }

    /// Descends to the leaf that may hold `key`, recording the path.
    fn descend(&self, key: &K) -> (u32, Path) {
        let mut path = Path {
            steps: [Step::default(); MAX_HEIGHT],
            depth: 0,
        };
        let mut node = self.root;
        for _ in 0..self.height {
            let inner = &self.inners[node as usize];
            let slot = self.route(inner, key);
            path.steps[path.depth] = Step { node, slot };
            path.depth += 1;
            node = inner.children[slot];
        }
        (node, path)
    }

    /// Child slot for `key`: the number of separators `keys[1..len]` that are
    /// `<= key`.
    #[inline(always)]
    fn route(&self, inner: &Inner<K>, key: &K) -> usize {
        let n = inner.len as usize;
        let mut slot = 0usize;
        for (i, k) in inner.keys.iter().enumerate() {
            let le = O::cmp(k, key) != Ordering::Greater;
            slot += ((i >= 1) & (i < n) & le) as usize;
        }
        slot
    }

    fn find(&self, key: &K) -> Option<(u32, usize)> {
        if self.root == NONE {
            return None;
        }
        let (leaf, _) = self.descend(key);
        let l = &self.leaves[leaf as usize];
        let pos = rank::<K, O>(&l.keys, l.len as usize, key, true);
        (pos < l.len as usize && O::cmp(&l.keys[pos], key) == Ordering::Equal)
            .then_some((leaf, pos))
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.find(key).is_some()
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        let (leaf, pos) = self.find(key)?;
        self.leaves[leaf as usize].vals[pos].as_ref()
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let (leaf, pos) = self.find(key)?;
        self.leaves[leaf as usize].vals[pos].as_mut()
    }

    /// Inserts, returning the previous value of `key` if there was one.
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, CacheBTreeInsertError<K, V>> {
        if self.root == NONE {
            match self.alloc_leaf() {
                Ok(i) => {
                    self.root = i;
                    self.height = 0;
                }
                Err(error) => return Err(CacheBTreeInsertError { key, value, error }),
            }
        }
        let (leaf, path) = self.descend(&key);
        let l = &mut self.leaves[leaf as usize];
        let n = l.len as usize;
        let pos = rank::<K, O>(&l.keys, n, &key, true);
        if pos < n && O::cmp(&l.keys[pos], &key) == Ordering::Equal {
            return Ok(l.vals[pos].replace(value));
        }
        if self.len >= self.max_len {
            let error = CacheBTreeError::Full {
                max_len: self.max_len,
            };
            return Err(CacheBTreeInsertError { key, value, error });
        }
        if n < B {
            Self::leaf_insert_at(l, pos, key, value);
            self.len += 1;
            return Ok(None);
        }
        // Full leaf: provision every node a split cascade can need before
        // changing anything, so an allocation failure leaves the tree intact.
        if let Err(error) = self.provision_split(&path) {
            return Err(CacheBTreeInsertError { key, value, error });
        }
        let right = self.alloc_leaf().expect("provisioned");
        let (sep, right) = {
            let (left_part, right_part) = split_pair(&mut self.leaves, leaf, right);
            for i in 0..(B - MIN) {
                right_part.keys[i] = left_part.keys[MIN + i];
                right_part.vals[i] = left_part.vals[MIN + i].take();
            }
            right_part.len = (B - MIN) as u8;
            left_part.len = MIN as u8;
            right_part.next = left_part.next;
            left_part.next = right;
            if pos <= MIN && !(pos == MIN && O::cmp(&key, &right_part.keys[0]) == Ordering::Greater)
            {
                Self::leaf_insert_at(left_part, pos, key, value);
            } else {
                Self::leaf_insert_at(right_part, pos - MIN, key, value);
            }
            (right_part.keys[0], right)
        };
        self.len += 1;
        self.insert_separator(&path, sep, right);
        Ok(None)
    }

    fn leaf_insert_at(l: &mut Leaf<K, V>, pos: usize, key: K, value: V) {
        let n = l.len as usize;
        debug_assert!(n < B && pos <= n);
        l.keys.copy_within(pos..n, pos + 1);
        l.vals[pos..=n].rotate_right(1);
        l.keys[pos] = key;
        l.vals[pos] = Some(value);
        l.len += 1;
    }

    /// Ensures the free lists hold enough nodes for a full split cascade.
    fn provision_split(&mut self, path: &Path) -> Result<(), CacheBTreeError> {
        let full_inners = path.steps[..path.depth]
            .iter()
            .rev()
            .take_while(|s| self.inners[s.node as usize].len as usize == B)
            .count();
        let need_inners = full_inners + usize::from(full_inners == path.depth);
        // Grow the slabs directly: taking from a free list and pushing back
        // would not change its length.
        while self.free_leaves < 1 {
            if self.leaves.len() >= NONE as usize {
                return Err(CacheBTreeError::OutOfMemory);
            }
            self.leaves
                .try_reserve(1)
                .map_err(|_| CacheBTreeError::OutOfMemory)?;
            self.leaves.push(empty_leaf());
            self.free_leaf_node((self.leaves.len() - 1) as u32);
        }
        while self.free_inners < need_inners {
            if self.inners.len() >= NONE as usize {
                return Err(CacheBTreeError::OutOfMemory);
            }
            self.inners
                .try_reserve(1)
                .map_err(|_| CacheBTreeError::OutOfMemory)?;
            self.inners.push(empty_inner());
            self.free_inner_node((self.inners.len() - 1) as u32);
        }
        Ok(())
    }

    /// Inserts `(sep, child)` after the path's slot at each level, splitting
    /// full inner nodes upward. Nodes were provisioned by `provision_split`.
    fn insert_separator(&mut self, path: &Path, mut sep: K, mut child: u32) {
        for d in (0..path.depth).rev() {
            let Step { node, slot } = path.steps[d];
            let at = slot + 1;
            let n = self.inners[node as usize].len as usize;
            if n < B {
                Self::inner_insert_at(&mut self.inners[node as usize], at, sep, child);
                return;
            }
            let right = self.alloc_inner().expect("provisioned");
            let (l, r) = split_pair(&mut self.inners, node, right);
            for i in 0..(B - MIN) {
                r.keys[i] = l.keys[MIN + i];
                r.children[i] = l.children[MIN + i];
            }
            r.len = (B - MIN) as u8;
            l.len = MIN as u8;
            if at <= MIN {
                Self::inner_insert_at(l, at, sep, child);
            } else {
                Self::inner_insert_at(r, at - MIN, sep, child);
            }
            sep = r.keys[0];
            child = right;
        }
        // The root split: grow a new root.
        let old = self.root;
        let old_first = if self.height == 0 {
            self.leaves[old as usize].keys[0]
        } else {
            self.inners[old as usize].keys[0]
        };
        let root = self.alloc_inner().expect("provisioned");
        let r = &mut self.inners[root as usize];
        r.keys[0] = old_first;
        r.children[0] = old;
        r.keys[1] = sep;
        r.children[1] = child;
        r.len = 2;
        self.root = root;
        self.height += 1;
    }

    fn inner_insert_at(node: &mut Inner<K>, at: usize, sep: K, child: u32) {
        let n = node.len as usize;
        debug_assert!(n < B && at <= n);
        node.keys.copy_within(at..n, at + 1);
        node.children.copy_within(at..n, at + 1);
        node.keys[at] = sep;
        node.children[at] = child;
        node.len += 1;
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        if self.root == NONE {
            return None;
        }
        let (leaf, path) = self.descend(key);
        let l = &mut self.leaves[leaf as usize];
        let n = l.len as usize;
        let pos = rank::<K, O>(&l.keys, n, key, true);
        if pos >= n || O::cmp(&l.keys[pos], key) != Ordering::Equal {
            return None;
        }
        let value = l.vals[pos].take();
        l.keys.copy_within(pos + 1..n, pos);
        l.vals[pos..n].rotate_left(1);
        l.len -= 1;
        self.len -= 1;
        if path.depth == 0 {
            if self.leaves[leaf as usize].len == 0 {
                self.free_leaf_node(leaf);
                self.root = NONE;
            }
            return value;
        }
        if (self.leaves[leaf as usize].len as usize) < MIN {
            self.rebalance_leaf(&path, leaf);
        }
        value
    }

    fn rebalance_leaf(&mut self, path: &Path, leaf: u32) {
        let Step { node: parent, slot } = path.steps[path.depth - 1];
        let pn = self.inners[parent as usize].len as usize;
        if slot + 1 < pn {
            let right = self.inners[parent as usize].children[slot + 1];
            if self.leaves[right as usize].len as usize > MIN {
                let (l, r) = split_pair(&mut self.leaves, leaf, right);
                let ln = l.len as usize;
                let rn = r.len as usize;
                l.keys[ln] = r.keys[0];
                l.vals[ln] = r.vals[0].take();
                l.len += 1;
                r.keys.copy_within(1..rn, 0);
                r.vals[..rn].rotate_left(1);
                r.len -= 1;
                let first = r.keys[0];
                self.inners[parent as usize].keys[slot + 1] = first;
                return;
            }
            self.merge_leaves(path, leaf, right, slot + 1);
        } else {
            let left = self.inners[parent as usize].children[slot - 1];
            if self.leaves[left as usize].len as usize > MIN {
                let (l, r) = split_pair(&mut self.leaves, left, leaf);
                let ln = l.len as usize;
                let rn = r.len as usize;
                r.keys.copy_within(0..rn, 1);
                r.vals[..=rn].rotate_right(1);
                r.keys[0] = l.keys[ln - 1];
                r.vals[0] = l.vals[ln - 1].take();
                r.len += 1;
                l.len -= 1;
                let first = r.keys[0];
                self.inners[parent as usize].keys[slot] = first;
                return;
            }
            self.merge_leaves(path, left, leaf, slot);
        }
    }

    /// Moves every entry of `right` into `left`, frees `right` and removes
    /// child slot `right_slot` from the parent.
    fn merge_leaves(&mut self, path: &Path, left: u32, right: u32, right_slot: usize) {
        let (l, r) = split_pair(&mut self.leaves, left, right);
        let ln = l.len as usize;
        let rn = r.len as usize;
        for i in 0..rn {
            l.keys[ln + i] = r.keys[i];
            l.vals[ln + i] = r.vals[i].take();
        }
        l.len = (ln + rn) as u8;
        r.len = 0;
        l.next = r.next;
        self.free_leaf_node(right);
        self.remove_child(path, path.depth - 1, right_slot);
    }

    /// Removes child slot `slot` from the inner node at `path.steps[d]` and
    /// restores the fill invariant upward.
    fn remove_child(&mut self, path: &Path, d: usize, slot: usize) {
        let node = path.steps[d].node;
        {
            let x = &mut self.inners[node as usize];
            let n = x.len as usize;
            x.keys.copy_within(slot + 1..n, slot);
            x.children.copy_within(slot + 1..n, slot);
            x.len -= 1;
        }
        let n = self.inners[node as usize].len as usize;
        if d == 0 {
            if n == 1 {
                self.root = self.inners[node as usize].children[0];
                self.height -= 1;
                self.free_inner_node(node);
            }
            return;
        }
        if n >= MIN {
            return;
        }
        let Step {
            node: parent,
            slot: pslot,
        } = path.steps[d - 1];
        let pn = self.inners[parent as usize].len as usize;
        if pslot + 1 < pn {
            let right = self.inners[parent as usize].children[pslot + 1];
            let sep = self.inners[parent as usize].keys[pslot + 1];
            if self.inners[right as usize].len as usize > MIN {
                let (x, r) = split_pair(&mut self.inners, node, right);
                let xn = x.len as usize;
                let rn = r.len as usize;
                x.keys[xn] = sep;
                x.children[xn] = r.children[0];
                x.len += 1;
                let new_sep = r.keys[1];
                r.keys.copy_within(1..rn, 0);
                r.children.copy_within(1..rn, 0);
                r.len -= 1;
                self.inners[parent as usize].keys[pslot + 1] = new_sep;
                return;
            }
            self.merge_inners(path, d, node, right, sep, pslot + 1);
        } else {
            let left = self.inners[parent as usize].children[pslot - 1];
            let sep = self.inners[parent as usize].keys[pslot];
            if self.inners[left as usize].len as usize > MIN {
                let (l, x) = split_pair(&mut self.inners, left, node);
                let ln = l.len as usize;
                let xn = x.len as usize;
                x.keys.copy_within(0..xn, 1);
                x.children.copy_within(0..xn, 1);
                x.keys[1] = sep;
                x.keys[0] = l.keys[ln - 1];
                x.children[0] = l.children[ln - 1];
                x.len += 1;
                l.len -= 1;
                let moved = x.keys[0];
                self.inners[parent as usize].keys[pslot] = moved;
                return;
            }
            self.merge_inners(path, d, left, node, sep, pslot);
        }
    }

    fn merge_inners(
        &mut self,
        path: &Path,
        d: usize,
        left: u32,
        right: u32,
        sep: K,
        right_slot: usize,
    ) {
        let (l, r) = split_pair(&mut self.inners, left, right);
        let ln = l.len as usize;
        let rn = r.len as usize;
        l.keys[ln] = sep;
        l.children[ln] = r.children[0];
        for i in 1..rn {
            l.keys[ln + i] = r.keys[i];
            l.children[ln + i] = r.children[i];
        }
        l.len = (ln + rn) as u8;
        self.free_inner_node(right);
        self.remove_child(path, d - 1, right_slot);
    }

    fn leftmost_leaf(&self) -> u32 {
        let mut node = self.root;
        for _ in 0..self.height {
            node = self.inners[node as usize].children[0];
        }
        node
    }

    pub fn first(&self) -> Option<(&K, &V)> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<(&K, &V)> {
        if self.root == NONE {
            return None;
        }
        let mut node = self.root;
        for _ in 0..self.height {
            let x = &self.inners[node as usize];
            node = x.children[x.len as usize - 1];
        }
        let l = &self.leaves[node as usize];
        let i = (l.len as usize).checked_sub(1)?;
        Some((&l.keys[i], l.vals[i].as_ref()?))
    }

    /// Entries in key order.
    pub fn iter(&self) -> Iter<'_, K, V, O> {
        let leaf = if self.root == NONE {
            NONE
        } else {
            self.leftmost_leaf()
        };
        Iter {
            tree: self,
            leaf,
            pos: 0,
            end: Bound::Unbounded,
        }
    }

    /// Entries whose keys lie in `range`, in key order.
    pub fn range<R: RangeBounds<K>>(&self, range: R) -> Iter<'_, K, V, O> {
        let end = range.end_bound().cloned();
        if self.root == NONE {
            return Iter {
                tree: self,
                leaf: NONE,
                pos: 0,
                end,
            };
        }
        let (leaf, pos) = match range.start_bound() {
            Bound::Unbounded => (self.leftmost_leaf(), 0),
            Bound::Included(k) | Bound::Excluded(k) => {
                let strict = matches!(range.start_bound(), Bound::Included(_));
                let (leaf, _) = self.descend(k);
                let l = &self.leaves[leaf as usize];
                (leaf, rank::<K, O>(&l.keys, l.len as usize, k, strict))
            }
        };
        Iter {
            tree: self,
            leaf,
            pos,
            end,
        }
    }

    /// Checks every structural invariant (sorted keys, fill, separators,
    /// leaf chain, counts). For tests; `O(n)`.
    #[doc(hidden)]
    pub fn check_invariants(&self) -> Result<(), String> {
        if self.root == NONE {
            return if self.len == 0 {
                Ok(())
            } else {
                Err("empty root with nonzero len".into())
            };
        }
        let mut leaves_in_order = Vec::new();
        let count = self.check_node(
            self.root,
            self.height,
            None,
            None,
            true,
            &mut leaves_in_order,
        )?;
        if count != self.len {
            return Err(format!("len {} but {} entries", self.len, count));
        }
        let mut chain = Vec::new();
        let mut l = self.leftmost_leaf();
        while l != NONE {
            chain.push(l);
            l = self.leaves[l as usize].next;
            if chain.len() > self.leaves.len() {
                return Err("leaf chain cycle".into());
            }
        }
        if chain != leaves_in_order {
            return Err("leaf chain differs from tree order".into());
        }
        Ok(())
    }

    fn check_node(
        &self,
        node: u32,
        height: usize,
        lo: Option<K>,
        hi: Option<K>,
        is_root: bool,
        leaves: &mut Vec<u32>,
    ) -> Result<usize, String> {
        let in_bounds = |k: &K| {
            lo.is_none_or(|lo| O::cmp(&lo, k) != Ordering::Greater)
                && hi.is_none_or(|hi| O::cmp(k, &hi) == Ordering::Less)
        };
        if height == 0 {
            let l = &self.leaves[node as usize];
            let n = l.len as usize;
            if n == 0 || (!is_root && n < MIN) {
                return Err(format!("leaf {node} underfull: {n}"));
            }
            for i in 0..n {
                if !in_bounds(&l.keys[i]) {
                    return Err(format!("leaf {node} key {i} out of bounds"));
                }
                if i > 0 && O::cmp(&l.keys[i - 1], &l.keys[i]) != Ordering::Less {
                    return Err(format!("leaf {node} unsorted at {i}"));
                }
                if l.vals[i].is_none() {
                    return Err(format!("leaf {node} missing value {i}"));
                }
            }
            if l.vals[n..].iter().any(Option::is_some) {
                return Err(format!("leaf {node} stale value past len"));
            }
            leaves.push(node);
            return Ok(n);
        }
        let x = &self.inners[node as usize];
        let n = x.len as usize;
        if n < 2 || (!is_root && n < MIN) {
            return Err(format!("inner {node} underfull: {n}"));
        }
        let mut total = 0;
        for i in 0..n {
            if i > 1 && O::cmp(&x.keys[i - 1], &x.keys[i]) != Ordering::Less {
                return Err(format!("inner {node} unsorted at {i}"));
            }
            if i >= 1 && !in_bounds(&x.keys[i]) {
                return Err(format!("inner {node} separator {i} out of bounds"));
            }
            let clo = if i == 0 { lo } else { Some(x.keys[i]) };
            let chi = if i + 1 < n { Some(x.keys[i + 1]) } else { hi };
            total += self.check_node(x.children[i], height - 1, clo, chi, false, leaves)?;
        }
        Ok(total)
    }
}

/// Borrows two distinct slab entries mutably.
fn split_pair<T>(v: &mut [T], a: u32, b: u32) -> (&mut T, &mut T) {
    let (a, b) = (a as usize, b as usize);
    assert_ne!(a, b);
    if a < b {
        let (lo, hi) = v.split_at_mut(b);
        (&mut lo[a], &mut hi[0])
    } else {
        let (lo, hi) = v.split_at_mut(a);
        (&mut hi[0], &mut lo[b])
    }
}

/// In-order iterator over a [`CacheBTree`] (or a range of it).
pub struct Iter<'a, K: Copy + Default, V, O: KeyOrder<K>> {
    tree: &'a CacheBTree<K, V, O>,
    leaf: u32,
    pos: usize,
    end: Bound<K>,
}

impl<'a, K: Copy + Default, V, O: KeyOrder<K>> Iterator for Iter<'a, K, V, O> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.leaf == NONE {
                return None;
            }
            let l = &self.tree.leaves[self.leaf as usize];
            if self.pos >= l.len as usize {
                self.leaf = l.next;
                self.pos = 0;
                continue;
            }
            let k = &l.keys[self.pos];
            let past = match &self.end {
                Bound::Unbounded => false,
                Bound::Included(e) => O::cmp(k, e) == Ordering::Greater,
                Bound::Excluded(e) => O::cmp(k, e) != Ordering::Less,
            };
            if past {
                self.leaf = NONE;
                return None;
            }
            let v = l.vals[self.pos].as_ref()?;
            self.pos += 1;
            return Some((k, v));
        }
    }
}
