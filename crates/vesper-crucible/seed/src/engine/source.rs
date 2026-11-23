//! Frozen Rust choice-source fallback for `vesper-crucible-seed`.

use crate::source::{Extend, Gen, Invalid, Source};

pub(crate) fn next_value(
    prefix: &[u64],
    extend: &mut Extend,
    pos: usize,
    max_draws: usize,
    max: u64,
    generation: Gen,
) -> Result<u64, Invalid> {
    if pos >= max_draws {
        return Err(Invalid::Overrun);
    }
    let value = match prefix.get(pos) {
        Some(&v) => v.min(max),
        None => match extend {
            Extend::Strict => return Err(Invalid::Overrun),
            Extend::Random(rng) => match generation {
                Gen::Uniform => rng.below_inclusive(max),
                Gen::Bernoulli(n) => u64::from(rng.chance(n)).min(max),
            },
        },
    };
    Ok(value)
}

pub(crate) fn draw_bytes(
    src: &mut Source,
    max_len: usize,
    average: usize,
) -> Result<Vec<u8>, Invalid> {
    let mut out = Vec::new();
    while src.more(out.len(), max_len, average)? {
        out.push(src.draw_u8()?);
    }
    Ok(out)
}
