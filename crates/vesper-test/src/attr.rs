//! Typed attribute DTOs for `@test(...)`, `@test_case(...)`, fault profiles,
//! budget gates, and test function signatures.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Expected diagnostic specification for `@test(expect_error = EXXXX, ...)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExpectErrorSpec {
    /// Expected diagnostic code (e.g. `"E0301"`, `"E0302"`).
    pub code: String,
    /// Optional expected substring in the primary diagnostic span text.
    pub primary: Option<String>,
    /// Optional expected substring in at least one related diagnostic label.
    pub related: Option<String>,
    /// Optional expected substring in the diagnostic's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ExpectErrorSpec {
    /// Create a new expectation matching only a diagnostic code.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            primary: None,
            related: None,
            message: None,
        }
    }

    /// Require `primary` to appear in the primary diagnostic span text.
    pub fn with_primary(mut self, primary: impl Into<String>) -> Self {
        self.primary = Some(primary.into());
        self
    }

    /// Require `related` to appear in a related diagnostic label.
    pub fn with_related(mut self, related: impl Into<String>) -> Self {
        self.related = Some(related.into());
        self
    }

    /// Require `message` to appear in the diagnostic's message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// Fault injection profile for `SimPulse` chaos and DPOR tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FaultProfile {
    /// Torn write-ahead-log sector / tail damage.
    TornWalSector,
    /// Simulated network partition / packet loss.
    NetPartition,
    /// Virtual clock skew across actors.
    ClockSkew,
    /// User-defined or suite-specific fault profile name.
    Custom(String),
}

impl FaultProfile {
    /// Parse a profile identifier from `@test(fault_profile = ...)`.
    pub fn from_name(name: &str) -> Self {
        match name {
            "TornWalSector" => Self::TornWalSector,
            "NetPartition" => Self::NetPartition,
            "ClockSkew" => Self::ClockSkew,
            other => Self::Custom(other.to_string()),
        }
    }
}

impl fmt::Display for FaultProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TornWalSector => f.write_str("TornWalSector"),
            Self::NetPartition => f.write_str("NetPartition"),
            Self::ClockSkew => f.write_str("ClockSkew"),
            Self::Custom(name) => f.write_str(name),
        }
    }
}

/// Hardware and resource budget constraints for a test.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BudgetGate {
    /// Maximum worst-case stack depth in bytes (Stage-5 WCSD).
    pub max_stack: Option<u64>,
    /// Maximum PMU cycle count (host hardware counters; non-cacheable).
    pub max_cycles: Option<u64>,
    /// Require zero dynamic heap allocations during execution.
    pub zero_alloc: bool,
    /// Require constant-time SSA taint verification (`eff [ConstTime]`).
    pub const_time: bool,
    /// Require real-time confinement verification (`eff [Realtime]`).
    pub realtime: bool,
}

impl BudgetGate {
    /// Returns `true` if any static or dynamic budget gate is enabled.
    pub fn is_active(&self) -> bool {
        self.max_stack.is_some()
            || self.max_cycles.is_some()
            || self.zero_alloc
            || self.const_time
            || self.realtime
    }

    /// Returns `true` if this gate relies on host PMU cycle counters.
    pub fn requires_pmu(&self) -> bool {
        self.max_cycles.is_some()
    }
}

/// Parsed configuration from `@test` or `@test(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestAttr {
    /// Number of generated property test cases (`@test(cases = N)`).
    pub cases: Option<u32>,
    /// Deterministic PRNG / simulation seed (`@test(seed = N)`).
    pub seed: Option<u64>,
    /// Number of `SimPulse` DPOR schedules (`@test(sim_schedules = N)`).
    pub sim_schedules: Option<u32>,
    /// Optional chaos fault profile (`@test(fault_profile = ...)`).
    pub fault_profile: Option<FaultProfile>,
    /// Optional negative checker expectation (`@test(expect_error = ...)`).
    pub expect_error: Option<ExpectErrorSpec>,
    /// Hardware and resource budget gates.
    pub budget: BudgetGate,
    /// Optional execution fuel cap for loops and bounded recursion.
    pub fuel_limit: Option<u64>,
    /// Optional wall-clock timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Whether this test is skipped by default (`@test(ignore = true)`).
    pub ignore: bool,
    /// Explicit marker for tests invoking nondeterministic FFI.
    pub nondeterministic_ffi: bool,
}

/// A single pinned regression input from `@test_case(...)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestCaseAttr {
    /// Source-level argument expressions in parameter order.
    pub args: Vec<String>,
    /// Optional human-readable label for reporting.
    pub label: Option<String>,
}

impl TestCaseAttr {
    /// Create a new `@test_case(...)` entry with argument expressions.
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            label: None,
        }
    }

    /// Attach a human-readable label to this pinned case.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// A parameter in a `@test` function signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestParam {
    /// Parameter identifier.
    pub name: String,
    /// Rendered type expression (e.g. `"u64"`, `"SimStorageCtl"`).
    pub ty: String,
    /// Whether this parameter is an injected capability handle rather than
    /// a property-tested value input.
    pub is_capability: bool,
}

impl TestParam {
    /// Construct a non-capability value parameter.
    pub fn value(name: impl Into<String>, ty: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            is_capability: false,
        }
    }

    /// Construct a capability handle parameter.
    pub fn capability(name: impl Into<String>, ty: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            is_capability: true,
        }
    }
}

/// Checked function signature metadata fed into [`crate::classify::classify`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestSignature {
    /// Fully qualified or local function name.
    pub name: String,
    /// Formal parameters of the test function.
    pub params: Vec<TestParam>,
    /// Declared effect row entries from `eff [...]` (e.g. `"Fail[E]"`, `"Net"`, `"ConstTime"`).
    pub effects: Vec<String>,
    /// Parsed `@test(...)` attribute configuration.
    pub attr: TestAttr,
    /// Repeatable `@test_case(...)` regression pins attached to the function.
    pub cases: Vec<TestCaseAttr>,
}

impl TestSignature {
    /// Create a minimal signature for a 0-argument pure unit test.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Returns `true` if `effects` contains `effect_name` (matching either
    /// exact names like `"Net"` or parameterized names like `"Fail[E]"`).
    pub fn has_effect(&self, effect_name: &str) -> bool {
        self.effects
            .iter()
            .any(|eff| effect_head(eff) == effect_name)
    }

    /// Iterate over non-capability value parameters.
    pub fn value_params(&self) -> impl Iterator<Item = &TestParam> {
        self.params.iter().filter(|p| !p.is_capability)
    }
}

/// Extract the leading effect identifier before `[` or whitespace (e.g. `"Fail[E]"` -> `"Fail"`).
pub(crate) fn effect_head(eff: &str) -> &str {
    let trimmed = eff.trim();
    trimmed
        .split(['[', ' ', '\t', '@'])
        .next()
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_head_parses_parameterized_and_plain_effects() {
        let sig = TestSignature {
            name: "test_net_fail".into(),
            params: vec![
                TestParam::capability("ctl", "SimStorageCtl"),
                TestParam::value("x", "u64"),
            ],
            effects: vec![
                "Fail[IoError]".into(),
                "Net".into(),
                "Stack @ <= 4096".into(),
            ],
            attr: TestAttr::default(),
            cases: vec![],
        };

        assert!(sig.has_effect("Fail"));
        assert!(sig.has_effect("Net"));
        assert!(sig.has_effect("Stack"));
        assert!(!sig.has_effect("Clock"));
        assert_eq!(sig.value_params().count(), 1);
    }

    #[test]
    fn fault_profile_roundtrip_names() {
        assert_eq!(
            FaultProfile::from_name("TornWalSector"),
            FaultProfile::TornWalSector
        );
        assert_eq!(
            FaultProfile::from_name("NetPartition"),
            FaultProfile::NetPartition
        );
        assert_eq!(
            FaultProfile::from_name("ClockSkew"),
            FaultProfile::ClockSkew
        );
        assert_eq!(
            FaultProfile::from_name("DropEveryThird"),
            FaultProfile::Custom("DropEveryThird".into())
        );
        assert_eq!(FaultProfile::ClockSkew.to_string(), "ClockSkew");
    }

    #[test]
    fn budget_gate_activation_and_pmu() {
        let mut gate = BudgetGate::default();
        assert!(!gate.is_active());
        assert!(!gate.requires_pmu());

        gate.max_cycles = Some(10_000);
        assert!(gate.is_active());
        assert!(gate.requires_pmu());
    }
}
