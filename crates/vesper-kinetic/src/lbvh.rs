//! 64-bit Morton linear BVH broadphase (§36.3, Karras 2012 construction).
//!
//! Primitive centroids are quantised to 21 bits per axis over the centroid
//! bounds and interleaved into 63-bit Morton codes. Leaves are sorted by
//! `(code, primitive index)`, which is a strict total order, so the sort and
//! therefore the whole tree is deterministic even with duplicate codes. The
//! Karras construction then builds the `n − 1` internal nodes, each from the
//! common-prefix lengths `δ(i, j)` of the sorted keys, where ties between
//! equal codes are broken by the sorted positions (`64 + clz(i ⊕ j)`).
//! `lean/KineticColoring.lean` proves, about its model of sorted keys, the
//! prefix property the construction relies on: keys between two keys that
//! share a prefix share it too (`sorted_prefix_between`).
//!
//! Every buffer is reserved by [`Lbvh::with_capacity`]; rebuilding and
//! querying with at most that many primitives does not allocate.

use crate::world::det;

/// Bits per axis of a 64-bit Morton code.
pub const MORTON64_AXIS_BITS: u32 = 21;
/// Largest quantised coordinate.
pub const MORTON64_AXIS_MAX: u32 = (1 << MORTON64_AXIS_BITS) - 1;

/// Spread the low 21 bits of `v` so bit `i` lands on bit `3i`.
#[inline]
pub fn spread21(v: u32) -> u64 {
    let mut x = u64::from(v & MORTON64_AXIS_MAX);
    x = (x | (x << 32)) & 0x001f_0000_0000_ffff;
    x = (x | (x << 16)) & 0x001f_0000_ff00_00ff;
    x = (x | (x << 8)) & 0x100f_00f0_0f00_f00f;
    x = (x | (x << 4)) & 0x10c3_0c30_c30c_30c3;
    x = (x | (x << 2)) & 0x1249_2492_4924_9249;
    x
}

/// Inverse of [`spread21`].
#[inline]
pub fn compact21(code: u64) -> u32 {
    let mut x = code & 0x1249_2492_4924_9249;
    x = (x | (x >> 2)) & 0x10c3_0c30_c30c_30c3;
    x = (x | (x >> 4)) & 0x100f_00f0_0f00_f00f;
    x = (x | (x >> 8)) & 0x001f_0000_ff00_00ff;
    x = (x | (x >> 16)) & 0x001f_0000_0000_ffff;
    x = (x | (x >> 32)) & 0x0000_0000_001f_ffff;
    x as u32
}

/// 63-bit Morton code of three 21-bit coordinates (`x` on bit 0).
#[inline]
pub fn morton64(x: u32, y: u32, z: u32) -> u64 {
    spread21(x) | (spread21(y) << 1) | (spread21(z) << 2)
}

/// Decode a [`morton64`] code.
#[inline]
pub fn demorton64(code: u64) -> [u32; 3] {
    [compact21(code), compact21(code >> 1), compact21(code >> 2)]
}

/// Axis-aligned bounding box (inclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    /// The box `center ± half`.
    pub fn around(center: [f32; 3], half: f32) -> Self {
        Self {
            min: [
                det::sub(center[0], half),
                det::sub(center[1], half),
                det::sub(center[2], half),
            ],
            max: [
                det::add(center[0], half),
                det::add(center[1], half),
                det::add(center[2], half),
            ],
        }
    }

    /// Whether the closed boxes intersect.
    #[inline]
    pub fn overlaps(&self, other: &Self) -> bool {
        (0..3).all(|a| self.min[a] <= other.max[a] && other.min[a] <= self.max[a])
    }

    fn union(&self, other: &Self) -> Self {
        let mut out = *self;
        for a in 0..3 {
            if other.min[a] < out.min[a] {
                out.min[a] = other.min[a];
            }
            if other.max[a] > out.max[a] {
                out.max[a] = other.max[a];
            }
        }
        out
    }

    fn is_valid(&self) -> bool {
        (0..3).all(|a| {
            self.min[a].is_finite() && self.max[a].is_finite() && self.min[a] <= self.max[a]
        })
    }
}

/// Typed LBVH failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbvhError {
    /// Box `index` has a non-finite or inverted bound.
    InvalidBox { index: usize },
    /// More primitives than the capacity reserved at construction.
    CapacityExceeded { primitives: usize, capacity: usize },
    /// More overlapping pairs than the output capacity.
    PairOverflow { capacity: usize },
}

impl std::fmt::Display for LbvhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBox { index } => write!(f, "box {index} is non-finite or inverted"),
            Self::CapacityExceeded {
                primitives,
                capacity,
            } => write!(f, "{primitives} primitives exceed LBVH capacity {capacity}"),
            Self::PairOverflow { capacity } => {
                write!(f, "more than {capacity} overlapping pairs")
            }
        }
    }
}

impl std::error::Error for LbvhError {}

/// A child reference: a leaf (sorted position) or an internal node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    Leaf(u32),
    Internal(u32),
}

/// One internal node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub left: Child,
    pub right: Child,
}

/// A linear BVH over axis-aligned boxes.
#[derive(Debug, Clone)]
pub struct Lbvh {
    capacity: usize,
    /// Sorted `(morton code, primitive index)`.
    keys: Vec<(u64, u32)>,
    leaf_bounds: Vec<Aabb>,
    nodes: Vec<Node>,
    node_bounds: Vec<Aabb>,
    order: Vec<u32>,
    stack: Vec<Child>,
}

const EMPTY_BOX: Aabb = Aabb {
    min: [0.0; 3],
    max: [0.0; 3],
};

impl Lbvh {
    /// Reserve storage for up to `capacity` primitives.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            keys: Vec::with_capacity(capacity),
            leaf_bounds: Vec::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity.saturating_sub(1)),
            node_bounds: Vec::with_capacity(capacity.saturating_sub(1)),
            order: Vec::with_capacity(capacity.saturating_sub(1)),
            // A root-to-leaf path is at most n − 1 internal nodes deep, and
            // the traversal keeps at most one sibling per level.
            stack: Vec::with_capacity(capacity + 1),
        }
    }

    /// Primitives in the last build.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the last build was empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Sorted `(code, primitive)` keys of the last build.
    pub fn keys(&self) -> &[(u64, u32)] {
        &self.keys
    }

    /// Internal nodes of the last build; node 0 is the root when there are
    /// at least two primitives.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Rebuild over `boxes`. On error the previous tree is cleared.
    pub fn build(&mut self, boxes: &[Aabb]) -> Result<(), LbvhError> {
        self.keys.clear();
        self.leaf_bounds.clear();
        self.nodes.clear();
        self.node_bounds.clear();
        self.order.clear();
        if boxes.len() > self.capacity || boxes.len() > u32::MAX as usize {
            return Err(LbvhError::CapacityExceeded {
                primitives: boxes.len(),
                capacity: self.capacity,
            });
        }
        if let Some(index) = boxes.iter().position(|b| !b.is_valid()) {
            return Err(LbvhError::InvalidBox { index });
        }
        if boxes.is_empty() {
            return Ok(());
        }
        // Centroid bounds.
        let centroid = |b: &Aabb, a: usize| det::mul(det::add(b.min[a], b.max[a]), 0.5);
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for b in boxes {
            for a in 0..3 {
                let c = centroid(b, a);
                if c < lo[a] {
                    lo[a] = c;
                }
                if c > hi[a] {
                    hi[a] = c;
                }
            }
        }
        let scale = MORTON64_AXIS_MAX as f32;
        for (i, b) in boxes.iter().enumerate() {
            let mut q = [0u32; 3];
            for a in 0..3 {
                let extent = det::sub(hi[a], lo[a]);
                if extent > 0.0 {
                    let t = det::div(det::sub(centroid(b, a), lo[a]), extent);
                    // Saturating float-to-int conversion is defined everywhere.
                    q[a] = (det::mul(t, scale) as u32).min(MORTON64_AXIS_MAX);
                }
            }
            self.keys.push((morton64(q[0], q[1], q[2]), i as u32));
        }
        // Keys are distinct (the primitive index is unique), so an unstable
        // sort is still deterministic.
        self.keys.sort_unstable();
        for &(_, p) in &self.keys {
            self.leaf_bounds.push(boxes[p as usize]);
        }
        let n = self.keys.len();
        for i in 0..n.saturating_sub(1) {
            let node = self.internal(i);
            self.nodes.push(node);
            self.node_bounds.push(EMPTY_BOX);
        }
        if n >= 2 {
            // Pre-order from the root, then fill bounds children-first.
            self.stack.clear();
            self.stack.push(Child::Internal(0));
            while let Some(c) = self.stack.pop() {
                if let Child::Internal(k) = c {
                    self.order.push(k);
                    let node = self.nodes[k as usize];
                    self.stack.push(node.left);
                    self.stack.push(node.right);
                }
            }
            for idx in (0..self.order.len()).rev() {
                let k = self.order[idx] as usize;
                let node = self.nodes[k];
                let l = self.bounds_of(node.left);
                let r = self.bounds_of(node.right);
                self.node_bounds[k] = l.union(&r);
            }
        }
        Ok(())
    }

    fn bounds_of(&self, c: Child) -> Aabb {
        match c {
            Child::Leaf(i) => self.leaf_bounds[i as usize],
            Child::Internal(k) => self.node_bounds[k as usize],
        }
    }

    /// Common prefix length of sorted keys `i` and `j`; `-1` outside.
    fn delta(&self, i: usize, j: i64) -> i64 {
        if j < 0 || j as usize >= self.keys.len() {
            return -1;
        }
        let j = j as usize;
        let (a, b) = (self.keys[i].0, self.keys[j].0);
        if a == b {
            64 + i64::from(((i as u32) ^ (j as u32)).leading_zeros())
        } else {
            i64::from((a ^ b).leading_zeros())
        }
    }

    /// Karras 2012, algorithm 3, for internal node `i`.
    fn internal(&self, i: usize) -> Node {
        let ii = i as i64;
        let d: i64 = if self.delta(i, ii + 1) > self.delta(i, ii - 1) {
            1
        } else {
            -1
        };
        let delta_min = self.delta(i, ii - d);
        let mut l_max: i64 = 2;
        while self.delta(i, ii + l_max * d) > delta_min {
            l_max *= 2;
        }
        let mut l: i64 = 0;
        let mut t = l_max / 2;
        while t >= 1 {
            if self.delta(i, ii + (l + t) * d) > delta_min {
                l += t;
            }
            t /= 2;
        }
        let j = ii + l * d;
        let delta_node = self.delta(i, j);
        let mut s: i64 = 0;
        let mut div: i64 = 2;
        loop {
            let t = (l + div - 1) / div;
            if self.delta(i, ii + (s + t) * d) > delta_node {
                s += t;
            }
            if t <= 1 {
                break;
            }
            div *= 2;
        }
        let gamma = ii + s * d + d.min(0);
        let left = if ii.min(j) == gamma {
            Child::Leaf(gamma as u32)
        } else {
            Child::Internal(gamma as u32)
        };
        let right = if ii.max(j) == gamma + 1 {
            Child::Leaf(gamma as u32 + 1)
        } else {
            Child::Internal(gamma as u32 + 1)
        };
        Node { left, right }
    }

    /// Every pair of primitives `(a, b)`, `a < b`, whose boxes overlap,
    /// appended to `out` in ascending order. `out` keeps its capacity;
    /// exceeding `max_pairs` is an error (and `out` is cleared).
    pub fn overlapping_pairs(
        &mut self,
        out: &mut Vec<(u32, u32)>,
        max_pairs: usize,
    ) -> Result<(), LbvhError> {
        out.clear();
        let n = self.keys.len();
        if n < 2 {
            return Ok(());
        }
        for p in 0..n {
            let mine = self.leaf_bounds[p];
            self.stack.clear();
            self.stack.push(Child::Internal(0));
            while let Some(c) = self.stack.pop() {
                match c {
                    Child::Leaf(q) => {
                        let q = q as usize;
                        // Report each pair once, from its lower sorted leaf.
                        if q > p && mine.overlaps(&self.leaf_bounds[q]) {
                            if out.len() == max_pairs {
                                out.clear();
                                return Err(LbvhError::PairOverflow {
                                    capacity: max_pairs,
                                });
                            }
                            let (a, b) = (self.keys[p].1, self.keys[q].1);
                            out.push((a.min(b), a.max(b)));
                        }
                    }
                    Child::Internal(k) => {
                        if mine.overlaps(&self.node_bounds[k as usize]) {
                            let node = self.nodes[k as usize];
                            self.stack.push(node.right);
                            self.stack.push(node.left);
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        Ok(())
    }
}

/// Reference broadphase: every overlapping pair by exhaustive comparison.
pub fn brute_force_pairs(boxes: &[Aabb]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for i in 0..boxes.len() {
        for j in i + 1..boxes.len() {
            if boxes[i].overlaps(&boxes[j]) {
                out.push((i as u32, j as u32));
            }
        }
    }
    out
}
