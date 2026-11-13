//! Running a property once against a source.

use crate::source::{DrawKind, Failure, Invalid, Probe, PropError, Source};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Outcome of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail(Failure),
    Invalid(Invalid),
}

/// Failure kind reported for a property that panicked.
pub const PANIC_KIND: &str = "panic";

pub(crate) struct Run {
    pub verdict: Verdict,
    pub values: Vec<u64>,
    pub kinds: Vec<DrawKind>,
    pub probe: Probe,
}

pub(crate) fn execute<P>(prop: &mut P, mut src: Source, catch_panics: bool) -> Run
where
    P: FnMut(&mut Source) -> Result<(), PropError>,
{
    let result = if catch_panics {
        match catch_unwind(AssertUnwindSafe(|| prop(&mut src))) {
            Ok(result) => result,
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_owned());
                Err(PropError::fail(PANIC_KIND, message))
            }
        }
    } else {
        prop(&mut src)
    };
    let verdict = match result {
        Ok(()) => Verdict::Pass,
        Err(PropError::Fail(f)) => Verdict::Fail(f),
        Err(PropError::Invalid(i)) => Verdict::Invalid(i),
    };
    let (values, kinds, probe) = src.into_parts();
    Run {
        verdict,
        values,
        kinds,
        probe,
    }
}
