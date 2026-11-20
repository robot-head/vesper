//! Unified JSON and concise human-readable reporting for Vesper Anvil suites.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::classify::TestMode;
use crate::ledger::LedgerReport;
use crate::mutant::MutantReport;
use crate::verdict::VerdictStatus;

/// Execution report for a single `@test` item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnvilTestReport {
    /// Name of the test function.
    pub name: String,
    /// Synthesized execution tier.
    pub mode: TestMode,
    /// Outcome (`Pass`, `Fail`, or `Unsupported`).
    pub status: VerdictStatus,
    /// Whether this verdict was served directly from the `fn_blake3` CAS store.
    pub cached: bool,
    /// Execution time in microseconds (0 for cache hits).
    pub duration_us: u64,
    /// Optional failure or unsupported diagnostic detail.
    pub message: Option<String>,
    /// Optional obligation leak report when auditing ran.
    pub ledger: Option<LedgerReport>,
    /// Optional path to a sealed `.vchrono` failure capsule.
    pub capsule_path: Option<String>,
    /// Optional `@test_case(...)` regression pin snippet for counterexamples.
    pub suggested_pin: Option<String>,
}

/// Aggregate report for a full `vesper test` invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnvilSuiteReport {
    /// Per-test execution results in deterministic order.
    pub tests: Vec<AnvilTestReport>,
    /// Optional mutation testing summary when `--mutate` was enabled.
    pub mutation: Option<MutantReport>,
    /// Total wall-clock suite duration in microseconds.
    pub total_duration_us: u64,
}

impl AnvilSuiteReport {
    /// Count of passing tests.
    pub fn passed(&self) -> usize {
        self.tests
            .iter()
            .filter(|t| t.status == VerdictStatus::Pass)
            .count()
    }

    /// Count of failing tests.
    pub fn failed(&self) -> usize {
        self.tests
            .iter()
            .filter(|t| t.status == VerdictStatus::Fail)
            .count()
    }

    /// Count of tests that reported `Unsupported`.
    pub fn unsupported(&self) -> usize {
        self.tests
            .iter()
            .filter(|t| t.status == VerdictStatus::Unsupported)
            .count()
    }

    /// Count of tests satisfied from the verdict cache.
    pub fn cached(&self) -> usize {
        self.tests.iter().filter(|t| t.cached).count()
    }

    /// Returns `true` only if zero tests failed and zero tests were unsupported
    /// (RFC §5.2: never silently pass unsupported subsets).
    pub fn all_passed(&self) -> bool {
        self.failed() == 0 && self.unsupported() == 0
    }

    /// Serialize this suite report as compact JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize this suite report as pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render a concise, deterministic human-readable report.
    pub fn format_human(&self) -> String {
        let mut out = String::new();
        for test in &self.tests {
            let cache_tag = if test.cached { " [cached]" } else { "" };
            let _ = writeln!(
                out,
                "[{status}] {name} ({mode}, {us}us){cache_tag}",
                status = test.status,
                name = test.name,
                mode = test.mode,
                us = test.duration_us,
            );
            if let Some(msg) = &test.message {
                let _ = writeln!(out, "  detail: {msg}");
            }
            if let Some(ledger) = &test.ledger
                && ledger.has_leaks()
            {
                let _ = writeln!(out, "  ledger: {ledger}");
            }
            if let Some(capsule) = &test.capsule_path {
                let _ = writeln!(out, "  replay: vesper replay {capsule}");
            }
            if let Some(pin) = &test.suggested_pin {
                for line in pin.lines() {
                    let _ = writeln!(out, "  pin: {line}");
                }
            }
        }

        let _ = writeln!(
            out,
            "summary: {} passed, {} failed, {} unsupported ({} cached) in {}us",
            self.passed(),
            self.failed(),
            self.unsupported(),
            self.cached(),
            self.total_duration_us,
        );

        if let Some(mutation) = &self.mutation {
            let _ = writeln!(
                out,
                "mutation: {} killed, {} survived, {} timeout, {} ill-typed (score {:.1}%)",
                mutation.killed(),
                mutation.survived(),
                mutation.timed_out(),
                mutation.ill_typed(),
                mutation.mutation_score() * 100.0,
            );
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerObligationKind, ObligationDelta};
    use crate::mutant::{MutantKind, MutantOutcome, MutantPlan, MutantStatus};

    #[test]
    fn suite_report_json_and_human_formatting() {
        let suite = AnvilSuiteReport {
            tests: vec![
                AnvilTestReport {
                    name: "test_add".into(),
                    mode: TestMode::Unit,
                    status: VerdictStatus::Pass,
                    cached: true,
                    duration_us: 0,
                    message: None,
                    ledger: None,
                    capsule_path: None,
                    suggested_pin: None,
                },
                AnvilTestReport {
                    name: "test_vault_leak".into(),
                    mode: TestMode::Unit,
                    status: VerdictStatus::Fail,
                    cached: false,
                    duration_us: 450,
                    message: Some("secret slot leaked".into()),
                    ledger: Some(LedgerReport {
                        deltas: vec![ObligationDelta {
                            kind: LedgerObligationKind::VaultSecretSlot,
                            leaked: 1,
                        }],
                    }),
                    capsule_path: Some("target/anvil/test_vault_leak.vchrono".into()),
                    suggested_pin: Some("@test_case(0)".into()),
                },
            ],
            mutation: Some(MutantReport {
                outcomes: vec![MutantOutcome {
                    plan: MutantPlan {
                        id: 0,
                        target_fn: "add".into(),
                        kind: MutantKind::ArithSwap,
                        description: "+ -> -".into(),
                        span_start: 0,
                        span_end: 1,
                        impacted_tests: vec!["test_add".into()],
                    },
                    status: MutantStatus::Killed,
                    killed_by: Some("test_add".into()),
                    duration_us: 100,
                }],
            }),
            total_duration_us: 550,
        };

        assert_eq!(suite.passed(), 1);
        assert_eq!(suite.failed(), 1);
        assert_eq!(suite.unsupported(), 0);
        assert_eq!(suite.cached(), 1);
        assert!(!suite.all_passed());

        let json = suite.to_json().expect("serialize suite");
        let parsed: AnvilSuiteReport = serde_json::from_str(&json).expect("deserialize suite");
        assert_eq!(suite, parsed);

        let human = suite.format_human();
        assert!(human.contains("[PASS] test_add (unit, 0us) [cached]"));
        assert!(human.contains("[FAIL] test_vault_leak (unit, 450us)"));
        assert!(human.contains("vault::SecretSlot=1"));
        assert!(human.contains("vesper replay target/anvil/test_vault_leak.vchrono"));
        assert!(human.contains("summary: 1 passed, 1 failed, 0 unsupported (1 cached)"));
        assert!(
            human.contains("mutation: 1 killed, 0 survived, 0 timeout, 0 ill-typed (score 100.0%)")
        );
    }

    #[test]
    fn unsupported_test_prevents_all_passed() {
        let suite = AnvilSuiteReport {
            tests: vec![AnvilTestReport {
                name: "test_pmu_cycles".into(),
                mode: TestMode::BudgetGate,
                status: VerdictStatus::Unsupported,
                cached: false,
                duration_us: 10,
                message: Some("PMU counters unavailable".into()),
                ledger: None,
                capsule_path: None,
                suggested_pin: None,
            }],
            mutation: None,
            total_duration_us: 10,
        };
        assert_eq!(suite.unsupported(), 1);
        assert!(!suite.all_passed());
    }
}
