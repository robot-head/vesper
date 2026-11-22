//! Vesper Anvil (`crates/vesper-test`): root-free leaf crate providing the
//! core data models, signature-driven mode classifier, deterministic verdict
//! cache schema, negative-test diagnostic matcher, runtime obligation ledger,
//! mutation plan types, and unified test reporters.
//!
//! See `docs/rfcs/vesper-anvil.md` for the full architectural specification.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod attr;
pub mod classify;
pub mod expect_error;
#[cfg(not(target_arch = "wasm32"))]
pub mod ledger;
pub mod mutant;
#[cfg(not(target_arch = "wasm32"))]
pub mod report;
pub mod verdict;

pub use attr::{
    BudgetGate, ExpectErrorSpec, FaultProfile, TestAttr, TestCaseAttr, TestParam, TestSignature,
};
pub use classify::{
    DEFAULT_PROPERTY_CASES, DEFAULT_SEED, DEFAULT_SIM_SCHEDULES, HarnessPlan, TestMode, classify,
};
pub use expect_error::{ExpectErrorOutcome, ObservedDiagnostic, verify_expect_error};
#[cfg(not(target_arch = "wasm32"))]
pub use ledger::{
    ClosureCounter, LeakCounter, LedgerObligationKind, LedgerReport, LedgerSnapshot,
    ObligationDelta, ObligationLedger,
};
pub use mutant::{
    MutantDispatchSlot, MutantKind, MutantOutcome, MutantPlan, MutantReport, MutantStatus,
    format_test_case_pin,
};
#[cfg(not(target_arch = "wasm32"))]
pub use report::{AnvilSuiteReport, AnvilTestReport};
pub use verdict::{HARNESS_VERSION, VerdictKey, VerdictRecord, VerdictStatus, is_cacheable};
