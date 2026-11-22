//! Forwards [`crate::target::Target::parse`] to the `vesper/target` package, then
//! builds the public decoded-target types from the byte ranges it returns.

use crate::target::{OriginTarget, Query, QueryEntry, Segment, Target, TargetError, TargetLimits};

/// The types at the library boundary (`--rust-types`).
pub(crate) mod abi {
    pub use crate::target::{TargetError, TargetLimits, Utf8Policy};
}

#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../../port/target.rs"]
mod binding;

/// The value end of a query entry without `=` (`ABSENT` in the package).
const ABSENT: u64 = u64::MAX;

pub(crate) fn parse(raw: &str, limits: TargetLimits) -> Result<Target, TargetError> {
    let parsed = binding::target_parse(raw.as_bytes(), limits)?;
    let view = parsed.as_ref();
    if view.asterisk() {
        return Ok(Target::Asterisk);
    }
    let (bytes, ends) = (view.bytes().as_slice(), view.ends().as_slice());
    let (segment_ends, entry_ends) = ends.split_at(view.segments() as usize);
    let mut segments = Vec::with_capacity(segment_ends.len());
    let mut at = 0;
    for &end in segment_ends {
        let end = end as usize;
        segments.push(Segment(bytes[at..end].to_vec()));
        at = end;
    }
    let query = view.has_query().then(|| {
        let mut entries = Vec::with_capacity(entry_ends.len() / 2);
        for &[key_end, value_end] in entry_ends.as_chunks::<2>().0 {
            let key_end = key_end as usize;
            let key = bytes[at..key_end].to_vec();
            at = key_end;
            let value = (value_end != ABSENT).then(|| {
                let value_end = value_end as usize;
                let value = bytes[at..value_end].to_vec();
                at = value_end;
                value
            });
            entries.push(QueryEntry { key, value });
        }
        Query { entries }
    });
    Ok(Target::Origin(OriginTarget {
        segments,
        trailing_slash: view.trailing_slash(),
        query,
    }))
}
