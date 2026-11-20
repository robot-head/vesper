//! Deterministic signature-to-tier classification (RFC §3.2).
//!
//! Given a checked [`TestSignature`], [`classify`] synthesizes a [`HarnessPlan`]
//! whose primary [`TestMode`] is determined strictly by the `@test` attribute,
//! parameter list, and `eff [...]` row.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::attr::{BudgetGate, ExpectErrorSpec, FaultProfile, TestCaseAttr, TestSignature};

/// Default number of generated cases for parameterized property tests.
pub const DEFAULT_PROPERTY_CASES: u32 = 64;

/// Default number of DPOR schedules for `SimPulse` tests.
pub const DEFAULT_SIM_SCHEDULES: u32 = 16;

/// Default deterministic PRNG / simulation seed when `@test(seed = ...)` is omitted.
pub const DEFAULT_SEED: u64 = 0;

/// Effects that trigger `SimPulse` deterministic simulation & DPOR execution.
const SIMPULSE_EFFECTS: [&str; 4] = ["Scope", "Net", "Storage", "Clock"];

/// Effects that indicate ambient host I/O outside `SimPulse` virtualization.
const AMBIENT_IO_EFFECTS: [&str; 4] = ["Io", "Fs", "Env", "Process"];

/// Effects that indicate nondeterministic FFI boundaries.
const NONDETERMINISTIC_FFI_EFFECTS: [&str; 2] = ["Ffi", "Unsafe"];

/// The five unified assurance tiers of Vesper Anvil.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TestMode {
    /// Deterministic 0-argument unit test (`fn t() eff [Fail[E]]`).
    Unit,
    /// Parameterized property / concolic test driven by `ChoiceTape`.
    Property,
    /// Deterministic `SimPulse` chaos and DPOR schedule exploration.
    SimPulse,
    /// Static/dynamic hardware & resource budget gate (`ConstTime`, `Realtime`, stack, PMU).
    BudgetGate,
    /// In-memory negative substructural checker test (`@test(expect_error = ...)`).
    ExpectError,
}

impl fmt::Display for TestMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unit => f.write_str("unit"),
            Self::Property => f.write_str("property"),
            Self::SimPulse => f.write_str("simpulse"),
            Self::BudgetGate => f.write_str("budget"),
            Self::ExpectError => f.write_str("expect_error"),
        }
    }
}

/// Complete synthesized execution plan for a single `@test` function.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessPlan {
    /// Test function name.
    pub name: String,
    /// Primary execution tier selected by RFC §3.2 rules.
    pub primary_mode: TestMode,
    /// Whether property-based value generation or `@test_case` replay is active
    /// (may compose with `SimPulse` or `BudgetGate`).
    pub has_property_params: bool,
    /// Whether `SimPulse` virtual time / DPOR / fault injection is active.
    pub uses_simpulse: bool,
    /// Effective hardware and resource budget gates (merged from `@test(...)` and `eff [...]`).
    pub budget: BudgetGate,
    /// Negative diagnostic expectation when `primary_mode == TestMode::ExpectError`.
    pub expect_error: Option<ExpectErrorSpec>,
    /// Number of generated property cases (0 when property generation is inactive).
    pub property_cases: u32,
    /// Number of `SimPulse` schedules to explore (0 when `SimPulse` is inactive).
    pub sim_schedules: u32,
    /// Optional chaos fault profile for `SimPulse`.
    pub fault_profile: Option<FaultProfile>,
    /// Deterministic seed used for property generation and `SimPulse` scheduling.
    pub seed: u64,
    /// Explicit `@test_case(...)` regression pins replayed before generated cases.
    pub pinned_cases: Vec<TestCaseAttr>,
    /// Optional execution fuel cap.
    pub fuel_limit: Option<u64>,
    /// Optional wall-clock timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Whether this test is marked `@test(ignore = true)`.
    pub ignore: bool,
    /// Whether the test invokes nondeterministic FFI (disables verdict caching).
    pub uses_nondeterministic_ffi: bool,
    /// Whether the test declares ambient host I/O effects (disables verdict caching).
    pub uses_ambient_host_io: bool,
}

/// Deterministically classify a checked [`TestSignature`] into a [`HarnessPlan`]
/// following RFC §3.2:
///
/// 1. `@test(expect_error = EXXXX, ...)` -> [`TestMode::ExpectError`]
/// 2. `eff [...]` contains `Scope`, `Net`, `Storage`, or `Clock`, or
///    `@test(sim_schedules = ..., fault_profile = ...)` -> [`TestMode::SimPulse`]
/// 3. 1+ non-capability value parameters, or `@test(cases = ...)` / `@test_case(...)`
///    -> [`TestMode::Property`]
/// 4. `eff [...]` contains `ConstTime` or `Realtime`, or `@test(max_stack = ...,
///    max_cycles = ..., zero_alloc = true)` -> [`TestMode::BudgetGate`]
/// 5. Otherwise -> [`TestMode::Unit`]
pub fn classify(sig: &TestSignature) -> HarnessPlan {
    let mut budget = sig.attr.budget.clone();
    if sig.has_effect("ConstTime") {
        budget.const_time = true;
    }
    if sig.has_effect("Realtime") {
        budget.realtime = true;
    }

    let has_simpulse_effect = SIMPULSE_EFFECTS.iter().any(|e| sig.has_effect(e));
    let wants_simpulse =
        has_simpulse_effect || sig.attr.sim_schedules.is_some() || sig.attr.fault_profile.is_some();

    let has_value_params = sig.value_params().next().is_some();
    let has_property_params = has_value_params || sig.attr.cases.is_some() || !sig.cases.is_empty();

    let uses_nondeterministic_ffi = sig.attr.nondeterministic_ffi
        || NONDETERMINISTIC_FFI_EFFECTS
            .iter()
            .any(|e| sig.has_effect(e));
    let uses_ambient_host_io = AMBIENT_IO_EFFECTS.iter().any(|e| sig.has_effect(e));

    let primary_mode = if sig.attr.expect_error.is_some() {
        TestMode::ExpectError
    } else if wants_simpulse {
        TestMode::SimPulse
    } else if has_property_params {
        TestMode::Property
    } else if budget.is_active() {
        TestMode::BudgetGate
    } else {
        TestMode::Unit
    };

    let is_expect_error = primary_mode == TestMode::ExpectError;
    let uses_simpulse = !is_expect_error && wants_simpulse;
    let effective_property = !is_expect_error && has_property_params;

    let property_cases = if effective_property {
        sig.attr.cases.unwrap_or(DEFAULT_PROPERTY_CASES)
    } else {
        0
    };

    let sim_schedules = if uses_simpulse {
        sig.attr.sim_schedules.unwrap_or(DEFAULT_SIM_SCHEDULES)
    } else {
        0
    };

    HarnessPlan {
        name: sig.name.clone(),
        primary_mode,
        has_property_params: effective_property,
        uses_simpulse,
        budget,
        expect_error: sig.attr.expect_error.clone(),
        property_cases,
        sim_schedules,
        fault_profile: if uses_simpulse {
            sig.attr.fault_profile.clone()
        } else {
            None
        },
        seed: sig.attr.seed.unwrap_or(DEFAULT_SEED),
        pinned_cases: sig.cases.clone(),
        fuel_limit: sig.attr.fuel_limit,
        timeout_ms: sig.attr.timeout_ms,
        ignore: sig.attr.ignore,
        uses_nondeterministic_ffi,
        uses_ambient_host_io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::{TestAttr, TestParam};

    #[test]
    fn classifies_pure_zero_arg_as_unit() {
        let mut sig = TestSignature::new("test_add");
        sig.effects.push("Fail[AssertError]".into());

        let plan = classify(&sig);
        assert_eq!(plan.primary_mode, TestMode::Unit);
        assert!(!plan.has_property_params);
        assert!(!plan.uses_simpulse);
        assert!(!plan.budget.is_active());
        assert_eq!(plan.property_cases, 0);
        assert_eq!(plan.sim_schedules, 0);
    }

    #[test]
    fn classifies_expect_error_highest_priority() {
        let mut sig = TestSignature::new("test_linear_leak");
        sig.effects.extend(["Net".into(), "ConstTime".into()]);
        sig.params.push(TestParam::value("x", "u64"));
        sig.attr.expect_error = Some(ExpectErrorSpec::new("E0302"));

        let plan = classify(&sig);
        assert_eq!(plan.primary_mode, TestMode::ExpectError);
        assert!(!plan.uses_simpulse);
        assert!(!plan.has_property_params);
        assert_eq!(plan.expect_error.as_ref().unwrap().code, "E0302");
    }

    #[test]
    fn classifies_simpulse_from_effects_or_attrs() {
        let mut sig = TestSignature::new("test_wal_recovery");
        sig.effects.extend(["Scope".into(), "Storage".into()]);
        sig.params.push(TestParam::value("seed_val", "u64"));
        sig.attr.fault_profile = Some(FaultProfile::TornWalSector);
        sig.attr.sim_schedules = Some(32);

        let plan = classify(&sig);
        assert_eq!(plan.primary_mode, TestMode::SimPulse);
        assert!(plan.uses_simpulse);
        assert!(plan.has_property_params);
        assert_eq!(plan.sim_schedules, 32);
        assert_eq!(plan.property_cases, DEFAULT_PROPERTY_CASES);
        assert_eq!(plan.fault_profile, Some(FaultProfile::TornWalSector));
    }

    #[test]
    fn classifies_property_ignoring_capability_only_params() {
        let mut cap_only = TestSignature::new("test_cap_only");
        cap_only
            .params
            .push(TestParam::capability("arena", "Arena"));
        assert_eq!(classify(&cap_only).primary_mode, TestMode::Unit);

        let mut prop = TestSignature::new("test_commutative");
        prop.params.push(TestParam::value("a", "i64"));
        prop.params.push(TestParam::value("b", "i64"));
        prop.cases.push(TestCaseAttr::new(["0", "1"]));
        let plan = classify(&prop);
        assert_eq!(plan.primary_mode, TestMode::Property);
        assert!(plan.has_property_params);
        assert_eq!(plan.property_cases, DEFAULT_PROPERTY_CASES);
        assert_eq!(plan.pinned_cases.len(), 1);
    }

    #[test]
    fn classifies_budget_gate_from_effects_and_attrs() {
        let mut sig = TestSignature::new("test_ct_encrypt");
        sig.effects.push("ConstTime".into());
        sig.attr = TestAttr {
            budget: BudgetGate {
                max_stack: Some(2048),
                zero_alloc: true,
                ..BudgetGate::default()
            },
            ..TestAttr::default()
        };

        let plan = classify(&sig);
        assert_eq!(plan.primary_mode, TestMode::BudgetGate);
        assert!(plan.budget.const_time);
        assert!(plan.budget.zero_alloc);
        assert_eq!(plan.budget.max_stack, Some(2048));
    }
}
