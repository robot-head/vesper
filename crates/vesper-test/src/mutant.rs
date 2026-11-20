//! Mutation testing plan, indirect-table dispatch slots, outcomes, and
//! `@test_case(...)` regression pin formatting (RFC Phase 3 & Phase 4).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Category of structured AST/HIR mutation applied during `--mutate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MutantKind {
    /// Flip comparison boundary (`<` <-> `<=`, `>` <-> `>=`, `==` <-> `!=`).
    BoundaryFlip,
    /// Swap arithmetic operator (`+` <-> `-`, `*` <-> `/`).
    ArithSwap,
    /// Invert boolean literal or logical operator (`&&` <-> `||`, `true` <-> `false`).
    BoolFlip,
    /// Drop a field or place store through an `inout` parameter.
    DropInoutStore,
    /// Swap two same-typed `in` arguments at a call site.
    SwapInArgs,
    /// Force an early `Fail::raise` along a fallible path.
    ForceFailRaise,
}

impl fmt::Display for MutantKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BoundaryFlip => f.write_str("boundary_flip"),
            Self::ArithSwap => f.write_str("arith_swap"),
            Self::BoolFlip => f.write_str("bool_flip"),
            Self::DropInoutStore => f.write_str("drop_inout_store"),
            Self::SwapInArgs => f.write_str("swap_in_args"),
            Self::ForceFailRaise => f.write_str("force_fail_raise"),
        }
    }
}

/// Planned mutation of a single target function and its reverse call-graph test slice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutantPlan {
    /// Monotonic mutant identifier within the mutation batch.
    pub id: u32,
    /// Target function being mutated.
    pub target_fn: String,
    /// Structured mutation operator applied.
    pub kind: MutantKind,
    /// Concise human description of the change (e.g. `"`<` -> `<=`"`).
    pub description: String,
    /// Inclusive byte offset where the mutated expression begins.
    pub span_start: usize,
    /// Exclusive byte offset where the mutated expression ends.
    pub span_end: usize,
    /// Tests that transitively call `target_fn` (reverse call-graph slice).
    pub impacted_tests: Vec<String>,
}

/// Indirect function-pointer dispatch table entry for hot-swapping mutants in
/// one shared `JITModule`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutantDispatchSlot {
    /// Index into the module-owned `AtomicPtr<u8>` dispatch table.
    pub slot_index: usize,
    /// Logical function name.
    pub fn_name: String,
    /// JIT symbol name of the unmutated baseline function.
    pub baseline_symbol: String,
    /// JIT symbol names of compiled valid mutants (`f$mut_0`, `f$mut_1`, ...).
    pub mutant_symbols: Vec<String>,
}

/// Result of executing impacted tests against a single mutant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MutantStatus {
    /// At least one impacted test failed, catching the mutation.
    Killed,
    /// All impacted tests passed despite the mutation.
    Survived,
    /// Mutation failed type/borrow checking and was filtered out before JIT.
    IllTyped,
    /// Mutant hit the fuel cap or execution timeout.
    Timeout,
}

impl fmt::Display for MutantStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Killed => f.write_str("KILLED"),
            Self::Survived => f.write_str("SURVIVED"),
            Self::IllTyped => f.write_str("ILL_TYPED"),
            Self::Timeout => f.write_str("TIMEOUT"),
        }
    }
}

/// Per-mutant execution outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutantOutcome {
    /// The mutant plan that was evaluated.
    pub plan: MutantPlan,
    /// Final classification of the mutant.
    pub status: MutantStatus,
    /// Name of the first test that killed the mutant, if any.
    pub killed_by: Option<String>,
    /// Execution time in microseconds.
    pub duration_us: u64,
}

/// Aggregate mutation testing report across all generated mutants.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutantReport {
    /// Individual outcomes for every generated mutant.
    pub outcomes: Vec<MutantOutcome>,
}

impl MutantReport {
    /// Total number of generated mutants (including `IllTyped`).
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    /// Number of mutants killed by tests.
    pub fn killed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status == MutantStatus::Killed)
            .count()
    }

    /// Number of mutants that survived all impacted tests.
    pub fn survived(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status == MutantStatus::Survived)
            .count()
    }

    /// Number of mutants filtered out as ill-typed.
    pub fn ill_typed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status == MutantStatus::IllTyped)
            .count()
    }

    /// Number of mutants that exhausted fuel or timed out.
    pub fn timed_out(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status == MutantStatus::Timeout)
            .count()
    }

    /// Mutation score in `[0.0, 1.0]`, defined as
    /// `(killed + timed_out) / (total - ill_typed)`. Returns `1.0` when there
    /// are no valid (well-typed) mutants.
    pub fn mutation_score(&self) -> f64 {
        let valid = self.total().saturating_sub(self.ill_typed());
        if valid == 0 {
            return 1.0;
        }
        let caught = self.killed() + self.timed_out();
        caught as f64 / valid as f64
    }
}

/// Format a `@test_case(...)` regression pin attribute for `fn_name` and its
/// shrunk counterexample `args`.
///
/// The returned snippet is valid Vesper source suitable for insertion directly
/// above `fn <fn_name>` (e.g. from CLI failure hints or *Vesper Lens* code actions).
pub fn format_test_case_pin(fn_name: &str, args: &[String]) -> String {
    let joined = args.join(", ");
    if fn_name.trim().is_empty() {
        format!("@test_case({joined})")
    } else {
        format!("// regression pin for {fn_name}\n@test_case({joined})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan(id: u32, kind: MutantKind) -> MutantPlan {
        MutantPlan {
            id,
            target_fn: "binary_search".into(),
            kind,
            description: "< -> <=".into(),
            span_start: 10,
            span_end: 11,
            impacted_tests: vec!["test_search_bounds".into()],
        }
    }

    #[test]
    fn mutant_report_counts_and_score() {
        let report = MutantReport {
            outcomes: vec![
                MutantOutcome {
                    plan: sample_plan(0, MutantKind::BoundaryFlip),
                    status: MutantStatus::Killed,
                    killed_by: Some("test_search_bounds".into()),
                    duration_us: 120,
                },
                MutantOutcome {
                    plan: sample_plan(1, MutantKind::ArithSwap),
                    status: MutantStatus::Survived,
                    killed_by: None,
                    duration_us: 140,
                },
                MutantOutcome {
                    plan: sample_plan(2, MutantKind::DropInoutStore),
                    status: MutantStatus::IllTyped,
                    killed_by: None,
                    duration_us: 30,
                },
                MutantOutcome {
                    plan: sample_plan(3, MutantKind::BoolFlip),
                    status: MutantStatus::Timeout,
                    killed_by: Some("test_search_bounds".into()),
                    duration_us: 500,
                },
            ],
        };

        assert_eq!(report.total(), 4);
        assert_eq!(report.killed(), 1);
        assert_eq!(report.survived(), 1);
        assert_eq!(report.ill_typed(), 1);
        assert_eq!(report.timed_out(), 1);
        // (1 killed + 1 timeout) / (4 - 1 ill_typed) = 2 / 3
        assert!((report.mutation_score() - (2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn empty_or_all_ill_typed_report_has_perfect_score() {
        let empty = MutantReport::default();
        assert_eq!(empty.mutation_score(), 1.0);
    }

    #[test]
    fn formats_test_case_regression_pin() {
        let pin = format_test_case_pin("test_decode", &["0".into(), "255".into()]);
        assert_eq!(pin, "// regression pin for test_decode\n@test_case(0, 255)");

        let anon = format_test_case_pin("", &["-1".into()]);
        assert_eq!(anon, "@test_case(-1)");
    }
}
