//! The frozen Rust constraint colouring (`Colorer::color`,
//! `MultiColorer::color`), which the Vesper port (vesper/src/coloring.ves)
//! replaced in the package vesper-kinetic (SH6 lane 9, #1404).

use crate::coloring::{
    BodyId, Colorer, ColoringError, ConstraintBodies, MAX_CONSTRAINT_BODIES, MultiColorer,
};

pub(crate) fn color(
    me: &mut Colorer,
    constraints: &[ConstraintBodies],
) -> Result<(), ColoringError> {
    let capacity = me.colors.len();
    if constraints.len() > capacity || constraints.len() > u32::MAX as usize {
        return Err(ColoringError::CapacityExceeded {
            constraints: constraints.len(),
            capacity,
        });
    }
    let bodies = me.body_masks.len();
    for (constraint, c) in constraints.iter().enumerate() {
        for body in [Some(c.a), c.b].into_iter().flatten() {
            if body as usize >= bodies {
                return Err(ColoringError::BodyOutOfRange { constraint, body });
            }
        }
        if c.b == Some(c.a) {
            return Err(ColoringError::SelfConstraint {
                constraint,
                body: c.a,
            });
        }
    }
    me.body_masks.fill(0);
    me.order.clear();
    me.order.extend(0..constraints.len() as u32);
    // Keys are unique (index breaks ties), so an unstable sort is
    // deterministic and allocation-free.
    me.order
        .sort_unstable_by_key(|&i| (constraints[i as usize].key(), i));
    let mut used_colors = 0usize;
    for &i in &me.order {
        let c = constraints[i as usize];
        let mut busy = me.body_masks[c.a as usize];
        if let Some(b) = c.b {
            busy |= me.body_masks[b as usize];
        }
        let free = !busy;
        if free == 0 {
            return Err(ColoringError::TooManyColors {
                constraint: i as usize,
            });
        }
        let color = free.trailing_zeros();
        let bit = 1u64 << color;
        me.body_masks[c.a as usize] |= bit;
        if let Some(b) = c.b {
            me.body_masks[b as usize] |= bit;
        }
        me.colors[i as usize] = color as u8;
        used_colors = used_colors.max(color as usize + 1);
    }
    // Bucket by colour, keeping visiting order inside each colour.
    me.layer_offsets.clear();
    me.sorted.clear();
    for color in 0..used_colors {
        me.layer_offsets.push(me.sorted.len() as u32);
        for &i in &me.order {
            if me.colors[i as usize] as usize == color {
                me.sorted.push(i);
            }
        }
    }
    me.layer_offsets.push(me.sorted.len() as u32);
    me.layer_count = used_colors;
    Ok(())
}

pub(crate) fn color_sets(
    me: &mut MultiColorer,
    bodies: &[BodyId],
    offsets: &[u32],
) -> Result<(), ColoringError> {
    let count = offsets.len().saturating_sub(1);
    let capacity = me.colors.len();
    if count > capacity || count > u32::MAX as usize {
        return Err(ColoringError::CapacityExceeded {
            constraints: count,
            capacity,
        });
    }
    if offsets.first().is_some_and(|o| *o != 0) {
        return Err(ColoringError::BadArity { constraint: 0 });
    }
    let n = me.body_masks.len();
    for i in 0..count {
        let (lo, hi) = (offsets[i] as usize, offsets[i + 1] as usize);
        if hi <= lo || hi - lo > MAX_CONSTRAINT_BODIES || hi > bodies.len() {
            return Err(ColoringError::BadArity { constraint: i });
        }
        let set = &bodies[lo..hi];
        for (k, &b) in set.iter().enumerate() {
            if b as usize >= n {
                return Err(ColoringError::BodyOutOfRange {
                    constraint: i,
                    body: b,
                });
            }
            if set[..k].contains(&b) {
                return Err(ColoringError::SelfConstraint {
                    constraint: i,
                    body: b,
                });
            }
        }
        let min = set.iter().copied().min().unwrap_or(0);
        let max = set.iter().copied().max().unwrap_or(0);
        me.keys[i] = (min, max);
    }
    me.body_masks.fill(0);
    me.order.clear();
    me.order.extend(0..count as u32);
    let keys = &me.keys;
    me.order.sort_unstable_by_key(|&i| (keys[i as usize], i));
    let mut used = 0usize;
    for &i in &me.order {
        let set = &bodies[offsets[i as usize] as usize..offsets[i as usize + 1] as usize];
        let busy = set.iter().fold(0u64, |m, &b| m | me.body_masks[b as usize]);
        let free = !busy;
        if free == 0 {
            return Err(ColoringError::TooManyColors {
                constraint: i as usize,
            });
        }
        let color = free.trailing_zeros();
        for &b in set {
            me.body_masks[b as usize] |= 1u64 << color;
        }
        me.colors[i as usize] = color as u8;
        used = used.max(color as usize + 1);
    }
    me.layer_offsets.clear();
    me.sorted.clear();
    for color in 0..used {
        me.layer_offsets.push(me.sorted.len() as u32);
        for &i in &me.order {
            if me.colors[i as usize] as usize == color {
                me.sorted.push(i);
            }
        }
    }
    me.layer_offsets.push(me.sorted.len() as u32);
    me.layer_count = used;
    Ok(())
}
