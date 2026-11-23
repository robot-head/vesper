//! The frozen GPU-shaped sort-and-sweep Rust implementation replaced in
//! `src/sap.rs` (SH6 lane 9, #1404).

use crate::lbvh::Aabb;
use crate::sap::{SapError, SweepAndPrune, WORKGROUP, dispatch};

/// Radix digit width in bits.
const RADIX_BITS: u32 = 8;
const RADIX: usize = 1 << RADIX_BITS;
/// IEEE 754 binary32's sign bit.
const F32_SIGN_BIT: u32 = 0x8000_0000;
/// A pair key takes eight radix passes, or four once indices fit in 16
/// bits (at most 65,536 of them).
const PAIR_PASSES: u32 = 8;
const SMALL_PAIR_PASSES: u32 = 4;
const SMALL_INDEX_COUNT: usize = 1 << 16;
const SMALL_INDEX_MASK: u64 = 0xffff;

#[inline]
pub(crate) fn sortable(x: f32) -> u32 {
    let b = x.to_bits();
    if b >> 31 == 1 { !b } else { b | F32_SIGN_BIT }
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
pub(crate) struct SortScratch {
    hist: Vec<u32>,
    block_sums: Vec<u32>,
    pub(crate) keys_tmp: Vec<u64>,
    vals_tmp: Vec<u32>,
}

impl SortScratch {
    pub(crate) fn with_capacity(n: usize) -> Self {
        let groups = n.div_ceil(WORKGROUP).max(1);
        Self {
            hist: Vec::with_capacity(RADIX * groups),
            block_sums: Vec::with_capacity((RADIX * groups).div_ceil(WORKGROUP) + 1),
            keys_tmp: Vec::with_capacity(n),
            vals_tmp: Vec::with_capacity(n),
        }
    }

    pub(crate) fn reserve(&mut self, n: usize) {
        let groups = n.div_ceil(WORKGROUP).max(1);
        let hist = RADIX * groups;
        self.hist.reserve(hist.saturating_sub(self.hist.len()));
        let blocks = hist.div_ceil(WORKGROUP) + 1;
        self.block_sums
            .reserve(blocks.saturating_sub(self.block_sums.len()));
        self.keys_tmp.reserve(n.saturating_sub(self.keys_tmp.len()));
    }
}

pub(crate) fn build(me: &mut SweepAndPrune, boxes: &[Aabb]) -> Result<(), SapError> {
    let n = boxes.len();
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
    me.axis = 0;
    if n > 0 {
        let extent = |a: usize| f64::from(hi[a]) - f64::from(lo[a]);
        for a in 1..3 {
            if extent(a) > extent(me.axis) {
                me.axis = a;
            }
        }
    }
    let axis = me.axis;
    // 3. Keys.
    me.keys.clear();
    me.order.clear();
    dispatch(n, |_, r| {
        for i in r {
            me.keys.push(u64::from(sortable(boxes[i].min[axis])));
            me.order.push(i as u32);
        }
    });
    // 4. Stable radix sort by key (initial order is by index).
    radix_sort(&mut me.keys, Some(&mut me.order), 4, &mut me.sort);
    // 5. Gather bounds in sweep order.
    me.min.clear();
    me.max.clear();
    dispatch(n, |_, r| {
        for s in r {
            let b = &boxes[me.order[s] as usize];
            me.min.push(b.min);
            me.max.push(b.max);
        }
    });
    Ok(())
}

pub(crate) fn overlapping_pairs(
    me: &mut SweepAndPrune,
    out: &mut Vec<(u32, u32)>,
    max_pairs: usize,
) -> Result<(), SapError> {
    let n = me.order.len();
    let axis = me.axis;
    let (min, max, order) = (&me.min, &me.max, &me.order);
    let overlaps =
        |s: usize, t: usize| (0..3).all(|a| min[s][a] <= max[t][a] && min[t][a] <= max[s][a]);
    // 6. Count.
    me.counts.clear();
    me.counts.resize(n, 0);
    {
        let counts = &mut me.counts;
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
    let total = match exclusive_scan(&mut me.counts, &mut me.sort.block_sums) {
        Some(t) if t <= max_pairs as u64 => t as usize,
        _ => {
            return Err(SapError::PairOverflow {
                capacity: max_pairs,
            });
        }
    };
    // 8. Write pair keys at their offsets.
    me.pair_keys.clear();
    me.pair_keys.resize(total, 0);
    let (counts, pair_keys) = (&me.counts, &mut me.pair_keys);
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
    let passes = if n <= SMALL_INDEX_COUNT {
        SMALL_PAIR_PASSES
    } else {
        PAIR_PASSES
    };
    if passes == SMALL_PAIR_PASSES {
        // Indices fit in 16 bits: compress each key to 32 bits so four
        // passes sort it.
        for k in &mut me.pair_keys {
            *k = ((*k >> 32) << 16) | (*k & SMALL_INDEX_MASK);
        }
    }
    radix_sort(&mut me.pair_keys, None, passes, &mut me.sort);
    for &k in &me.pair_keys {
        let (a, b) = if passes == SMALL_PAIR_PASSES {
            ((k >> 16) as u32, (k & SMALL_INDEX_MASK) as u32)
        } else {
            ((k >> 32) as u32, k as u32)
        };
        out.push((a, b));
    }
    Ok(())
}
