//! `refute(predicate)` (§29.1.3): search for a counterexample to a predicate
//! over generated inputs and return a minimal deterministic reproduction.
//! The `vesper verify --refute` CLI over extracted Lean predicates is
//! integration work; this is the library entry point it calls.

use crate::fuzz::{Counterexample, FuzzConfig, NONDETERMINISTIC_KIND, fuzz};
use crate::source::{Invalid, Probe, PropError, Source};
use crate::tape::ChoiceTape;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};

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
    /// The generator or predicate failed some other way (for example it
    /// panicked: kind [`crate::PANIC_KIND`]). Not a refutation.
    Crashed { counterexample: Counterexample },
    /// A refutation was found but does not reproduce: replaying the shrunk
    /// tape does not regenerate an input (`error` is why), or the predicate
    /// is true on the regenerated input. The generator or predicate is
    /// nondeterministic. Not a refutation.
    Unreproducible {
        counterexample: Counterexample,
        error: Option<Invalid>,
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
    let Some(counterexample) = report.counterexample else {
        return Refutation::Survived {
            execs: report.execs,
            witnesses: report.corpus,
        };
    };
    if counterexample.failure.kind == NONDETERMINISTIC_KIND {
        return Refutation::Unreproducible {
            counterexample,
            error: None,
        };
    }
    if counterexample.failure.kind != REFUTED_KIND {
        return Refutation::Crashed { counterexample };
    }
    // Re-establish the refutation from the saved tape alone: regenerate the
    // input and check that the predicate is false on it.
    let mut src = Source::replay(&counterexample.tape);
    match generate(&mut src) {
        Err(error) => Refutation::Unreproducible {
            counterexample,
            error: Some(error),
        },
        Ok(input) => {
            let mut probe = Probe::default();
            let holds = catch_unwind(AssertUnwindSafe(|| predicate(&input, &mut probe)));
            match holds {
                Ok(false) => Refutation::Refuted {
                    input,
                    counterexample,
                },
                Ok(true) => Refutation::Unreproducible {
                    counterexample,
                    error: None,
                },
                Err(_) => Refutation::Crashed { counterexample },
            }
        }
    }
}
