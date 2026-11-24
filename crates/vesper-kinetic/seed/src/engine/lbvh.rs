//! The frozen Rust Karras LBVH replaced in `src/lbvh.rs` (SH6 lane 9,
//! #1404): 63-bit Morton codes, the build and the pair query.

use crate::lbvh::{Aabb, Child, Lbvh, LbvhError, MORTON64_AXIS_MAX, Node};
use crate::world::det;

// The 21-bit magic-bits interleave (see `morton`): the pattern each
// spreading step leaves, named by its shift.
const SPREAD21_MASK_32: u64 = 0x001f_0000_0000_ffff;
const SPREAD21_MASK_16: u64 = 0x001f_0000_ff00_00ff;
const SPREAD21_MASK_8: u64 = 0x100f_00f0_0f00_f00f;
const SPREAD21_MASK_4: u64 = 0x10c3_0c30_c30c_30c3;
const SPREAD21_MASK_2: u64 = 0x1249_2492_4924_9249;

#[inline]
pub(crate) fn spread21(v: u32) -> u64 {
    let mut x = u64::from(v & MORTON64_AXIS_MAX);
    x = (x | (x << 32)) & SPREAD21_MASK_32;
    x = (x | (x << 16)) & SPREAD21_MASK_16;
    x = (x | (x << 8)) & SPREAD21_MASK_8;
    x = (x | (x << 4)) & SPREAD21_MASK_4;
    x = (x | (x << 2)) & SPREAD21_MASK_2;
    x
}

#[inline]
pub(crate) fn compact21(code: u64) -> u32 {
    let mut x = code & SPREAD21_MASK_2;
    x = (x | (x >> 2)) & SPREAD21_MASK_4;
    x = (x | (x >> 4)) & SPREAD21_MASK_8;
    x = (x | (x >> 8)) & SPREAD21_MASK_16;
    x = (x | (x >> 16)) & SPREAD21_MASK_32;
    x = (x | (x >> 32)) & u64::from(MORTON64_AXIS_MAX);
    x as u32
}

#[inline]
pub(crate) fn morton64(x: u32, y: u32, z: u32) -> u64 {
    spread21(x) | (spread21(y) << 1) | (spread21(z) << 2)
}

fn union(this: &Aabb, other: &Aabb) -> Aabb {
    let mut out = *this;
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

fn is_valid(b: &Aabb) -> bool {
    (0..3).all(|a| b.min[a].is_finite() && b.max[a].is_finite() && b.min[a] <= b.max[a])
}

const EMPTY_BOX: Aabb = Aabb {
    min: [0.0; 3],
    max: [0.0; 3],
};

pub(crate) fn build(t: &mut Lbvh, boxes: &[Aabb]) -> Result<(), LbvhError> {
    if let Some(index) = boxes.iter().position(|b| !is_valid(b)) {
        return Err(LbvhError::InvalidBox { index });
    }
    if boxes.is_empty() {
        return Ok(());
    }
    build_tree(t, boxes);
    Ok(())
}

fn build_tree(t: &mut Lbvh, boxes: &[Aabb]) {
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
        t.keys.push((morton64(q[0], q[1], q[2]), i as u32));
    }
    // Keys are distinct (the primitive index is unique), so an unstable
    // sort is still deterministic.
    t.keys.sort_unstable();
    for &(_, p) in &t.keys {
        t.leaf_bounds.push(boxes[p as usize]);
    }
    let n = t.keys.len();
    for i in 0..n.saturating_sub(1) {
        let node = internal(t, i);
        t.nodes.push(node);
        t.node_bounds.push(EMPTY_BOX);
    }
    if n >= 2 {
        // Pre-order from the root, then fill bounds children-first.
        t.stack.clear();
        t.stack.push(Child::Internal(0));
        while let Some(c) = t.stack.pop() {
            if let Child::Internal(k) = c {
                t.order.push(k);
                let node = t.nodes[k as usize];
                t.stack.push(node.left);
                t.stack.push(node.right);
            }
        }
        for idx in (0..t.order.len()).rev() {
            let k = t.order[idx] as usize;
            let node = t.nodes[k];
            let l = bounds_of(t, node.left);
            let r = bounds_of(t, node.right);
            t.node_bounds[k] = union(&l, &r);
        }
    }
}

fn bounds_of(t: &Lbvh, c: Child) -> Aabb {
    match c {
        Child::Leaf(i) => t.leaf_bounds[i as usize],
        Child::Internal(k) => t.node_bounds[k as usize],
    }
}

/// Common prefix length of sorted keys `i` and `j`; `-1` outside.
fn delta(t: &Lbvh, i: usize, j: i64) -> i64 {
    if j < 0 || j as usize >= t.keys.len() {
        return -1;
    }
    let j = j as usize;
    let (a, b) = (t.keys[i].0, t.keys[j].0);
    if a == b {
        64 + i64::from(((i as u32) ^ (j as u32)).leading_zeros())
    } else {
        i64::from((a ^ b).leading_zeros())
    }
}

/// Karras 2012, algorithm 3, for internal node `i`.
fn internal(tree: &Lbvh, i: usize) -> Node {
    let ii = i as i64;
    let d: i64 = if delta(tree, i, ii + 1) > delta(tree, i, ii - 1) {
        1
    } else {
        -1
    };
    let delta_min = delta(tree, i, ii - d);
    let mut l_max: i64 = 2;
    while delta(tree, i, ii + l_max * d) > delta_min {
        l_max *= 2;
    }
    let mut l: i64 = 0;
    let mut t = l_max / 2;
    while t >= 1 {
        if delta(tree, i, ii + (l + t) * d) > delta_min {
            l += t;
        }
        t /= 2;
    }
    let j = ii + l * d;
    let delta_node = delta(tree, i, j);
    let mut s: i64 = 0;
    let mut div: i64 = 2;
    loop {
        let t = (l + div - 1) / div;
        if delta(tree, i, ii + (s + t) * d) > delta_node {
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

pub(crate) fn overlapping_pairs(
    t: &mut Lbvh,
    out: &mut Vec<(u32, u32)>,
    max_pairs: usize,
) -> Result<(), LbvhError> {
    let n = t.keys.len();
    for p in 0..n {
        let mine = t.leaf_bounds[p];
        t.stack.clear();
        t.stack.push(Child::Internal(0));
        while let Some(c) = t.stack.pop() {
            match c {
                Child::Leaf(q) => {
                    let q = q as usize;
                    // Report each pair once, from its lower sorted leaf.
                    if q > p && mine.overlaps(&t.leaf_bounds[q]) {
                        if out.len() == max_pairs {
                            out.clear();
                            return Err(LbvhError::PairOverflow {
                                capacity: max_pairs,
                            });
                        }
                        let (a, b) = (t.keys[p].1, t.keys[q].1);
                        out.push((a.min(b), a.max(b)));
                    }
                }
                Child::Internal(k) => {
                    if mine.overlaps(&t.node_bounds[k as usize]) {
                        let node = t.nodes[k as usize];
                        t.stack.push(node.right);
                        t.stack.push(node.left);
                    }
                }
            }
        }
    }
    out.sort_unstable();
    Ok(())
}
