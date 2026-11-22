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

// The port-backed build forwards `sortable`, `SweepAndPrune::build` and
// `SweepAndPrune::overlapping_pairs` to the Vesper port (`vesper/src/sap.ves`);
// the frozen Rust engine lives in `seed/src/engine/sap.rs` for the
// compiler's closure (SH6 lane 9, #1404).

use crate::lbvh::Aabb;

/// Items per workgroup.
pub const WORKGROUP: usize = 256;

/// Radix digit width in bits (used by `SapPort` histogram allocation).
#[cfg(vesper_port)]
const RADIX: usize = 256;

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
    #[cfg(vesper_port)]
    {
        crate::port::sap_sortable(x.to_bits())
    }
    #[cfg(not(vesper_port))]
    {
        crate::engine::sap::sortable(x)
    }
}

/// A sort-and-sweep broadphase over flat arrays.
#[derive(Debug)]
pub struct SweepAndPrune {
    pub(crate) capacity: usize,
    pub(crate) axis: usize,
    pub(crate) keys: Vec<u64>,
    pub(crate) order: Vec<u32>,
    #[cfg(not(vesper_port))]
    pub(crate) min: Vec<[f32; 3]>,
    #[cfg(not(vesper_port))]
    pub(crate) max: Vec<[f32; 3]>,
    pub(crate) counts: Vec<u32>,
    #[cfg(not(vesper_port))]
    pub(crate) pair_keys: Vec<u64>,
    #[cfg(not(vesper_port))]
    pub(crate) sort: crate::engine::sap::SortScratch,
    #[cfg(vesper_port)]
    port: SapPort,
}

/// The port-backed build's buffers, lent to the Vesper port: boxes as six
/// f32 bit patterns, the sorted bounds as three lanes each, radix-sort
/// scratch and the packed pairs `(a << 32) | b`.
#[cfg(vesper_port)]
#[derive(Debug, Default)]
struct SapPort {
    boxes: Vec<u32>,
    smin: Vec<u32>,
    smax: Vec<u32>,
    tk: Vec<u64>,
    tv: Vec<u32>,
    hist: Vec<u32>,
    out: Vec<u64>,
}

#[cfg(vesper_port)]
impl SapPort {
    fn with_capacity(n: usize) -> Self {
        Self {
            boxes: Vec::with_capacity(6 * n),
            smin: Vec::with_capacity(3 * n),
            smax: Vec::with_capacity(3 * n),
            tk: Vec::with_capacity(n),
            tv: Vec::with_capacity(n),
            hist: vec![0; RADIX],
            out: Vec::new(),
        }
    }
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
            #[cfg(not(vesper_port))]
            min: Vec::with_capacity(capacity),
            #[cfg(not(vesper_port))]
            max: Vec::with_capacity(capacity),
            counts: Vec::with_capacity(capacity),
            #[cfg(not(vesper_port))]
            pair_keys: Vec::new(),
            #[cfg(not(vesper_port))]
            sort: crate::engine::sap::SortScratch::with_capacity(capacity),
            #[cfg(vesper_port)]
            port: SapPort::with_capacity(capacity),
        }
    }

    /// Reserve pair storage for `max_pairs` pairs, so later queries with
    /// that bound do not allocate.
    pub fn reserve_pairs(&mut self, max_pairs: usize) {
        #[cfg(not(vesper_port))]
        {
            self.pair_keys
                .reserve(max_pairs.saturating_sub(self.pair_keys.len()));
            self.sort
                .keys_tmp
                .reserve(max_pairs.saturating_sub(self.sort.keys_tmp.len()));
            self.sort.reserve(max_pairs);
        }
        #[cfg(vesper_port)]
        {
            let p = &mut self.port;
            p.out.reserve(max_pairs.saturating_sub(p.out.len()));
            p.tk.reserve(max_pairs.saturating_sub(p.tk.len()));
        }
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
        #[cfg(vesper_port)]
        {
            let p = &mut self.port;
            crate::lbvh::box_bits(boxes, &mut p.boxes);
            self.keys.resize(n, 0);
            self.order.resize(n, 0);
            p.smin.resize(3 * n, 0);
            p.smax.resize(3 * n, 0);
            p.tk.resize(n, 0);
            p.tv.resize(n, 0);
            self.axis = 0;
            self.order.clear();
            self.order.resize(n, 0);
            self.axis = crate::port::sap_build(
                &p.boxes,
                &mut self.keys,
                &mut self.order,
                &mut p.smin,
                &mut p.smax,
                &mut p.tk,
                &mut p.tv,
                &mut p.hist,
            )? as usize;
            Ok(())
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::sap::build(self, boxes)
        }
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
        #[cfg(vesper_port)]
        {
            let p = &mut self.port;
            self.counts.clear();
            self.counts.resize(self.order.len(), 0);
            let total =
                crate::port::sap_pair_total(&p.smin, &p.smax, self.axis as u64, &mut self.counts);
            if total > max_pairs as u64 || total > u64::from(u32::MAX) {
                return Err(SapError::PairOverflow {
                    capacity: max_pairs,
                });
            }
            let total = total as usize;
            p.out.clear();
            p.out.resize(total, 0);
            p.tk.clear();
            p.tk.resize(total, 0);
            crate::port::sap_write_pairs(
                &self.order,
                &p.smin,
                &p.smax,
                self.axis as u64,
                &self.counts,
                &mut p.out,
                &mut p.tk,
                &mut [],
                &mut [],
                &mut p.hist,
            );
            out.extend(p.out.iter().map(|&k| ((k >> 32) as u32, k as u32)));
            Ok(())
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::sap::overlapping_pairs(self, out, max_pairs)
        }
    }
}
