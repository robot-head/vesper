//! `refute(predicate)` (§29.1.3): search for a counterexample to a predicate
//! over generated inputs and return a minimal deterministic reproduction.
//! The `vesper verify --refute` CLI over extracted Lean predicates is
//! integration work; this is the library entry point it calls.

use crate::fuzz::{Counterexample, FuzzConfig, fuzz};
use crate::source::{Invalid, Probe, PropError, Source};
use crate::tape::ChoiceTape;
use std::fmt::Debug;

/// Failure kind used for refutations.
pub const REFUTED_KIND: &str = "refuted";

/// Outcome of [`refute`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refutation<T> {
    /// The predicate is false on `input`, regenerated from the shrunk tape.
    Refuted {
        input: T,
        counterexample: Counterexample,
    },
    /// No counterexample within the budget. `witnesses` are the corpus tapes
    /// (one per coverage-distinct behaviour), usable as boundary seeds.
    /// Surviving is evidence, not proof.
    Survived {
        execs: u64,
        witnesses: Vec<ChoiceTape>,
    },
    /// A failure was found but the generator does not replay its own shrunk
    /// tape: the generator or predicate is nondeterministic.
    Unreproducible {
        counterexample: Counterexample,
        error: Invalid,
    },
}

/// Search for an input on which `predicate` is false. The predicate may
/// report comparisons through the [`Probe`] to guide the search.
pub fn refute<T, G, P>(config: &FuzzConfig, mut generate: G, mut predicate: P) -> Refutation<T>
where
    T: Debug,
    G: FnMut(&mut Source) -> Result<T, Invalid>,
    P: FnMut(&T, &mut Probe) -> bool,
{
    let report = fuzz(config, |src: &mut Source| {
        let value = generate(src)?;
        if predicate(&value, src.probe()) {
            Ok(())
        } else {
            let mut shown = format!("{value:?}");
            if shown.len() > 256 {
                let cut = (0..=256)
                    .rev()
                    .find(|&i| shown.is_char_boundary(i))
                    .unwrap_or(0);
                shown.truncate(cut);
                shown.push('…');
            }
            Err(PropError::fail(REFUTED_KIND, shown))
        }
    });
    match report.counterexample {
        Some(counterexample) => match generate(&mut Source::replay(&counterexample.tape)) {
            Ok(input) => Refutation::Refuted {
                input,
                counterexample,
            },
            Err(error) => Refutation::Unreproducible {
                counterexample,
                error,
            },
        },
        None => Refutation::Survived {
            execs: report.execs,
            witnesses: report.corpus,
        },
    }
}
