//! GPU-shaped sort-and-sweep broadphase (§36.1 item 4, §36.3), run on the
//! CPU as the reference implementation.
//!
//! The algorithm is written the way a compute shader would run it: a fixed
//! sequence of data-parallel kernels over flat arrays, each dispatched over
//! workgroups of [`WORKGROUP`] items, with no atomics, no pointer chasing
//! and no dynamic allocation after [`SweepAndPrune::with_capacity`]. Every
//! kernel's output depends only on its inputs, never on the order in which
//! workgroups run, so a GPU dispatch of the same kernels would produce the
//! same bits. Here [`dispatch`] runs the workgroups one after another.
//!
//! 1. **validate** (reduction): the first box with a non-finite or inverted
//!    bound is a typed error.
//! 2. **axis** (reduction): the sweep axis is the one with the largest
//!    centroid extent (ties: lowest axis), computed with `min`/`max`, which
//!    are exact and order-independent.
//! 3. **keys** (map): each box's lower bound on the sweep axis as an
//!    order-preserving `u32` ([`sortable`]).
//! 4. **radix sort** (four 8-bit LSD passes; each pass is a per-workgroup
//!    histogram kernel, an exclusive scan, and a stable per-workgroup
//!    scatter): boxes sorted by `(key, index)`, a strict total order.
//! 5. **gather** (map): bounds in sorted order.
//! 6. **count** (map): for each sorted box, the later boxes whose lower
//!    bound is at most its upper bound on the sweep axis and that overlap it
//!    on all axes.
//! 7. **scan**: exclusive prefix sum of the counts gives every box its
//!    output offset (and the total, checked against the output capacity).
//! 8. **write** (map): the same sweep writes each pair at its offset.
//! 9. **radix sort** of the pair keys (`a << 32 | b`, eight passes): pairs in
//!    ascending order, the same order [`crate::lbvh::Lbvh`] reports.
//!
//! Executing these kernels on a GPU (Ignis) is deferred: there is no GPU on
//! CI or on the development host, so nothing here claims GPU execution.

use crate::lbvh::Aabb;

/// Items per workgroup.
pub const WORKGROUP: usize = 256;

/// Radix digit width in bits.
const RADIX_BITS: u32 = 8;
const RADIX: usize = 1 << RADIX_BITS;

/// Typed sort-and-sweep failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SapError {
    /// Box `index` has a non-finite or inverted bound.
    InvalidBox { index: usize },
    /// More boxes than the capacity reserved at construction.
    CapacityExceeded { boxes: usize, capacity: usize },
    /// More overlapping pairs than the output capacity.
    PairOverflow { capacity: usize },
}

impl std::fmt::Display for SapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBox { index } => write!(f, "box {index} is non-finite or inverted"),
            Self::CapacityExceeded { boxes, capacity } => {
                write!(f, "{boxes} boxes exceed sort-and-sweep capacity {capacity}")
            }
            Self::PairOverflow { capacity } => write!(f, "more than {capacity} overlapping pairs"),
        }
    }
}

impl std::error::Error for SapError {}

/// Run `kernel(group, items)` for every workgroup of `n` items, where
/// `items` is the group's index range. On the CPU the groups run in order;
/// kernels written for this helper must not depend on that order.
pub fn dispatch(n: usize, mut kernel: impl FnMut(usize, std::ops::Range<usize>)) {
    for group in 0..n.div_ceil(WORKGROUP) {
        let lo = group * WORKGROUP;
        kernel(group, lo..(lo + WORKGROUP).min(n));
    }
}

/// Order-preserving map from `f32` to `u32`: `a < b` implies
/// `sortable(a) < sortable(b)` for non-NaN inputs (`-0.0` sorts just
/// below `+0.0`).
#[inline]
pub fn sortable(x: f32) -> u32 {
    let b = x.to_bits();
    if b >> 31 == 1 { !b } else { b | 0x8000_0000 }
}

/// Exclusive prefix sum in place as a blocked scan (block sums, a scan of
/// the block sums, then a per-block pass), the shape of a GPU scan. Returns
/// the total, or `None` on `u32` overflow.
fn exclusive_scan(values: &mut [u32], block_sums: &mut Vec<u32>) -> Option<u64> {
    block_sums.clear();
    let mut overflow = false;
    // Kernel 1: per-block sums.
    dispatch(values.len(), |_, r| {
        let mut s = 0u32;
        for v in &values[r] {
            match s.checked_add(*v) {
                Some(x) => s = x,
                None => overflow = true,
            }
        }
        block_sums.push(s);
    });
    // Kernel 2: scan of the block sums (one workgroup on a GPU; recursive
    // for more than WORKGROUP blocks, sequential here).
    let mut running = 0u64;
    for s in block_sums.iter_mut() {
        let here = running;
        running += u64::from(*s);
        *s = u32::try_from(here).unwrap_or(u32::MAX);
        if here > u64::from(u32::MAX) {
            overflow = true;
        }
    }
    // Kernel 3: per-block exclusive scan from the block offset.
    dispatch(values.len(), |g, r| {
        let mut s = block_sums[g];
        for v in &mut values[r] {
            let here = s;
            s = s.wrapping_add(*v);
            *v = here;
        }
    });
    (!overflow && running <= u64::from(u32::MAX)).then_some(running)
}

/// Stable LSD radix sort of `keys` (with optional `vals`) on the low
/// `passes · 8` bits, using the per-workgroup histogram / scan / scatter
/// kernels. Ties keep their input order.
fn radix_sort(
    keys: &mut Vec<u64>,
    mut vals: Option<&mut Vec<u32>>,
    passes: u32,
    s: &mut SortScratch,
) {
    let n = keys.len();
    let groups = n.div_ceil(WORKGROUP);
    for pass in 0..passes {
        let shift = pass * RADIX_BITS;
        let digit = |k: u64| ((k >> shift) as usize) & (RADIX - 1);
        // Histogram kernel: hist[d * groups + g] counts digit d in group g.
        s.hist.clear();
        s.hist.resize(RADIX * groups, 0);
        dispatch(n, |g, r| {
            for &k in &keys[r] {
                s.hist[digit(k) * groups + g] += 1;
            }
        });
        // Scan kernel: digit-major order makes the sort stable across groups.
        exclusive_scan(&mut s.hist, &mut s.block_sums).expect("at most u32::MAX keys");
        // Scatter kernel: each group writes its items, in order, from its
        // offset for each digit.
        s.keys_tmp.clear();
        s.keys_tmp.resize(n, 0);
        if vals.is_some() {
            s.vals_tmp.clear();
            s.vals_tmp.resize(n, 0);
        }
        dispatch(n, |g, r| {
            let mut next = [0u32; RADIX];
            for (d, slot) in next.iter_mut().enumerate() {
                *slot = s.hist[d * groups + g];
            }
            for i in r {
                let d = digit(keys[i]);
                let at = next[d] as usize;
                next[d] += 1;
                s.keys_tmp[at] = keys[i];
                if let Some(v) = vals.as_deref() {
                    s.vals_tmp[at] = v[i];
                }
            }
        });
        std::mem::swap(keys, &mut s.keys_tmp);
        if let Some(v) = vals.as_deref_mut() {
            std::mem::swap(v, &mut s.vals_tmp);
        }
    }
}

#[derive(Debug, Default)]
struct SortScratch {
    hist: Vec<u32>,
    block_sums: Vec<u32>,
    keys_tmp: Vec<u64>,
    vals_tmp: Vec<u32>,
}

impl SortScratch {
    fn with_capacity(n: usize) -> Self {
        let groups = n.div_ceil(WORKGROUP).max(1);
        Self {
            hist: Vec::with_capacity(RADIX * groups),
            block_sums: Vec::with_capacity((RADIX * groups).div_ceil(WORKGROUP) + 1),
            keys_tmp: Vec::with_capacity(n),
            vals_tmp: Vec::with_capacity(n),
        }
    }

    fn reserve(&mut self, n: usize) {
        let groups = n.div_ceil(WORKGROUP).max(1);
        let hist = RADIX * groups;
        self.hist.reserve(hist.saturating_sub(self.hist.len()));
        let blocks = hist.div_ceil(WORKGROUP) + 1;
        self.block_sums
            .reserve(blocks.saturating_sub(self.block_sums.len()));
        self.keys_tmp.reserve(n.saturating_sub(self.keys_tmp.len()));
    }
}

/// A sort-and-sweep broadphase over flat arrays.
#[derive(Debug)]
pub struct SweepAndPrune {
    capacity: usize,
    axis: usize,
    keys: Vec<u64>,
    order: Vec<u32>,
    min: Vec<[f32; 3]>,
    max: Vec<[f32; 3]>,
    counts: Vec<u32>,
    pair_keys: Vec<u64>,
    sort: SortScratch,
}

impl SweepAndPrune {
    /// Reserve every buffer for up to `capacity` boxes (pair buffers grow
    /// to the largest output seen, up to the caller's `max_pairs`).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            axis: 0,
            keys: Vec::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
            min: Vec::with_capacity(capacity),
            max: Vec::with_capacity(capacity),
            counts: Vec::with_capacity(capacity),
            pair_keys: Vec::new(),
            sort: SortScratch::with_capacity(capacity),
        }
    }

    /// Reserve pair storage for `max_pairs` pairs, so later queries with
    /// that bound do not allocate.
    pub fn reserve_pairs(&mut self, max_pairs: usize) {
        self.pair_keys
            .reserve(max_pairs.saturating_sub(self.pair_keys.len()));
        self.sort
            .keys_tmp
            .reserve(max_pairs.saturating_sub(self.sort.keys_tmp.len()));
        self.sort.reserve(max_pairs);
    }

    /// Boxes reserved for.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The sweep axis chosen by the last [`SweepAndPrune::build`].
    pub fn axis(&self) -> usize {
        self.axis
    }

    /// Box indices in sweep order after the last build.
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// Kernels 1–5: validate, choose the axis, compute keys, sort, gather.
    pub fn build(&mut self, boxes: &[Aabb]) -> Result<(), SapError> {
        let n = boxes.len();
        if n > self.capacity || n > u32::MAX as usize {
            return Err(SapError::CapacityExceeded {
                boxes: n,
                capacity: self.capacity,
            });
        }
        // 1. Validate (reduction to the smallest bad index).
        let mut bad = usize::MAX;
        dispatch(n, |_, r| {
            for i in r {
                let b = &boxes[i];
                let ok = (0..3)
                    .all(|a| b.min[a].is_finite() && b.max[a].is_finite() && b.min[a] <= b.max[a]);
                if !ok && i < bad {
                    bad = i;
                }
            }
        });
        if bad != usize::MAX {
            return Err(SapError::InvalidBox { index: bad });
        }
        // 2. Axis of largest centroid extent (min/max reduction).
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        dispatch(n, |_, r| {
            for i in r {
                for a in 0..3 {
                    let c = (boxes[i].min[a] + boxes[i].max[a]) * 0.5;
                    if c < lo[a] {
                        lo[a] = c;
                    }
                    if c > hi[a] {
                        hi[a] = c;
                    }
                }
            }
        });
        self.axis = 0;
        if n > 0 {
            let extent = |a: usize| f64::from(hi[a]) - f64::from(lo[a]);
            for a in 1..3 {
                if extent(a) > extent(self.axis) {
                    self.axis = a;
                }
            }
        }
        let axis = self.axis;
        // 3. Keys.
        self.keys.clear();
        self.order.clear();
        dispatch(n, |_, r| {
            for i in r {
                self.keys.push(u64::from(sortable(boxes[i].min[axis])));
                self.order.push(i as u32);
            }
        });
        // 4. Stable radix sort by key (initial order is by index).
        radix_sort(&mut self.keys, Some(&mut self.order), 4, &mut self.sort);
        // 5. Gather bounds in sweep order.
        self.min.clear();
        self.max.clear();
        dispatch(n, |_, r| {
            for s in r {
                let b = &boxes[self.order[s] as usize];
                self.min.push(b.min);
                self.max.push(b.max);
            }
        });
        Ok(())
    }

    /// Kernels 6–9: every pair `(a, b)`, `a < b`, of boxes from the last
    /// build that overlap (closed boxes, like [`Aabb::overlaps`]), in
    /// ascending order. More than `max_pairs` is an error (and `out` is
    /// cleared). `out` keeps its capacity.
    pub fn overlapping_pairs(
        &mut self,
        out: &mut Vec<(u32, u32)>,
        max_pairs: usize,
    ) -> Result<(), SapError> {
        out.clear();
        let n = self.order.len();
        let axis = self.axis;
        let (min, max, order) = (&self.min, &self.max, &self.order);
        let overlaps =
            |s: usize, t: usize| (0..3).all(|a| min[s][a] <= max[t][a] && min[t][a] <= max[s][a]);
        // 6. Count.
        self.counts.clear();
        self.counts.resize(n, 0);
        {
            let counts = &mut self.counts;
            dispatch(n, |_, r| {
                for s in r {
                    let mut c = 0u32;
                    let mut t = s + 1;
                    while t < n && min[t][axis] <= max[s][axis] {
                        if overlaps(s, t) {
                            c = c.saturating_add(1);
                        }
                        t += 1;
                    }
                    counts[s] = c;
                }
            });
        }
        // 7. Scan to output offsets.
        let total = match exclusive_scan(&mut self.counts, &mut self.sort.block_sums) {
            Some(t) if t <= max_pairs as u64 => t as usize,
            _ => {
                return Err(SapError::PairOverflow {
                    capacity: max_pairs,
                });
            }
        };
        // 8. Write pair keys at their offsets.
        self.pair_keys.clear();
        self.pair_keys.resize(total, 0);
        let (counts, pair_keys) = (&self.counts, &mut self.pair_keys);
        dispatch(n, |_, r| {
            for s in r {
                let mut at = counts[s] as usize;
                let mut t = s + 1;
                while t < n && min[t][axis] <= max[s][axis] {
                    if overlaps(s, t) {
                        let (a, b) = (order[s], order[t]);
                        pair_keys[at] = (u64::from(a.min(b)) << 32) | u64::from(a.max(b));
                        at += 1;
                    }
                    t += 1;
                }
            }
        });
        // 9. Sort the pair keys.
        let passes = if n <= 1 << 16 { 4 } else { 8 };
        if passes == 4 {
            // Indices fit in 16 bits: compress each key to 32 bits so four
            // passes sort it.
            for k in &mut self.pair_keys {
                *k = ((*k >> 32) << 16) | (*k & 0xffff);
            }
        }
        radix_sort(&mut self.pair_keys, None, passes, &mut self.sort);
        for &k in &self.pair_keys {
            let (a, b) = if passes == 4 {
                ((k >> 16) as u32, (k & 0xffff) as u32)
            } else {
                ((k >> 32) as u32, k as u32)
            };
            out.push((a, b));
        }
        Ok(())
    }
}
