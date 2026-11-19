//! Deterministic verdict cache keys, serializable verdict records, and
//! cacheability rules (RFC §5.1).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::classify::{HarnessPlan, TestMode};

/// Current Anvil harness ABI / verdict schema version mixed into every cache key.
pub const HARNESS_VERSION: u32 = 1;

/// Domain separation prefix for Anvil verdict cache keys.
const VERDICT_KEY_DOMAIN: &[u8] = b"vesper.anvil.verdict.v1\0";

/// Deterministic lookup key for a cached test verdict.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerdictKey {
    /// Hex-encoded transitive BLAKE3 fingerprint of the test function and its
    /// call-graph closure (`TransitiveFnBlake3`).
    pub fn_blake3: String,
    /// Deterministic PRNG / `SimPulse` schedule seed.
    pub sim_seed: u64,
    /// Target instruction set architecture identifier (e.g. `"x86_64"`, `"aarch64"`).
    pub target_isa: String,
    /// Harness / compiler ABI version.
    pub harness_version: u32,
}

impl VerdictKey {
    /// Construct a new [`VerdictKey`] using [`HARNESS_VERSION`].
    pub fn new(fn_blake3: impl Into<String>, sim_seed: u64, target_isa: impl Into<String>) -> Self {
        Self {
            fn_blake3: fn_blake3.into(),
            sim_seed,
            target_isa: target_isa.into(),
            harness_version: HARNESS_VERSION,
        }
    }

    /// Compute the canonical 32-byte BLAKE3 digest for this verdict key.
    ///
    /// All variable-length fields are length-prefixed to prevent framing ambiguity.
    pub fn cache_key_bytes(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(VERDICT_KEY_DOMAIN);
        hash_len_prefixed(&mut hasher, self.fn_blake3.as_bytes());
        hasher.update(&self.sim_seed.to_le_bytes());
        hash_len_prefixed(&mut hasher, self.target_isa.as_bytes());
        hasher.update(&self.harness_version.to_le_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Compute the deterministic 64-character lowercase hex BLAKE3 cache key.
    pub fn cache_key_hex(&self) -> String {
        let bytes = self.cache_key_bytes();
        let mut hex = String::with_capacity(64);
        for b in bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }
}

fn hash_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    let len = bytes.len() as u64;
    hasher.update(&len.to_le_bytes());
    hasher.update(bytes);
}

/// Outcome status of a single test execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerdictStatus {
    /// Test satisfied all assertions, obligations, and budget gates.
    Pass,
    /// Test failed an assertion, obligation check, diagnostic match, or budget gate.
    Fail,
    /// Test requested a capability unavailable on this host/backend (e.g. PMU counters);
    /// never treated as a silent pass (RFC §5.2).
    Unsupported,
}

impl fmt::Display for VerdictStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("PASS"),
            Self::Fail => f.write_str("FAIL"),
            Self::Unsupported => f.write_str("UNSUPPORTED"),
        }
    }
}

/// Serializable test verdict persisted in the content-addressed store (`~/.vesper/store`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    /// Deterministic key under which this verdict is indexed.
    pub key: VerdictKey,
    /// Name of the test function.
    pub test_name: String,
    /// Execution mode under which this verdict was produced.
    pub mode: TestMode,
    /// Outcome status (`Pass`, `Fail`, or `Unsupported`).
    pub status: VerdictStatus,
    /// Execution duration in microseconds.
    pub duration_us: u64,
    /// Optional failure or unsupported diagnostic detail.
    pub message: Option<String>,
    /// Optional path to a sealed `.vchrono` failure capsule.
    pub capsule_path: Option<String>,
}

impl VerdictRecord {
    /// Compute the deterministic lowercase hex BLAKE3 cache key for this record.
    pub fn cache_key_hex(&self) -> String {
        self.key.cache_key_hex()
    }

    /// Serialize this verdict record as compact JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize a verdict record from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Determine whether a [`HarnessPlan`]'s verdict may be soundly cached in CAS.
///
/// Per RFC §5.1, verdicts are non-cacheable if the test:
/// - exercises hardware PMU cycle counters (`max_cycles`),
/// - invokes nondeterministic FFI,
/// - declares ambient host I/O effects, or
/// - is marked `ignore`.
pub fn is_cacheable(plan: &HarnessPlan) -> bool {
    if plan.ignore {
        return false;
    }
    if plan.budget.requires_pmu() {
        return false;
    }
    if plan.uses_nondeterministic_ffi || plan.uses_ambient_host_io {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::{BudgetGate, TestSignature};
    use crate::classify::classify;

    #[test]
    fn verdict_key_is_deterministic_and_sensitive_to_all_fields() {
        let base = VerdictKey::new("abc123", 42, "x86_64");
        let same = VerdictKey::new("abc123", 42, "x86_64");
        assert_eq!(base.cache_key_hex(), same.cache_key_hex());
        assert_eq!(base.cache_key_hex().len(), 64);

        let diff_hash = VerdictKey::new("abc124", 42, "x86_64");
        let diff_seed = VerdictKey::new("abc123", 43, "x86_64");
        let diff_isa = VerdictKey::new("abc123", 42, "aarch64");
        let mut diff_ver = base.clone();
        diff_ver.harness_version += 1;

        assert_ne!(base.cache_key_hex(), diff_hash.cache_key_hex());
        assert_ne!(base.cache_key_hex(), diff_seed.cache_key_hex());
        assert_ne!(base.cache_key_hex(), diff_isa.cache_key_hex());
        assert_ne!(base.cache_key_hex(), diff_ver.cache_key_hex());
    }

    #[test]
    fn verdict_record_json_roundtrip() {
        let record = VerdictRecord {
            key: VerdictKey::new("deadbeef", 7, "x86_64"),
            test_name: "test_sort_stable".into(),
            mode: TestMode::Property,
            status: VerdictStatus::Pass,
            duration_us: 1250,
            message: None,
            capsule_path: None,
        };

        let json = record.to_json().expect("serialize");
        let decoded = VerdictRecord::from_json(&json).expect("deserialize");
        assert_eq!(record, decoded);
        assert_eq!(record.cache_key_hex(), decoded.cache_key_hex());
    }

    #[test]
    fn cacheability_rejects_pmu_ffi_ambient_io_and_ignored() {
        let unit = classify(&TestSignature::new("test_unit"));
        assert!(is_cacheable(&unit));

        let mut pmu_sig = TestSignature::new("test_pmu");
        pmu_sig.attr.budget = BudgetGate {
            max_cycles: Some(5_000),
            ..BudgetGate::default()
        };
        assert!(!is_cacheable(&classify(&pmu_sig)));

        let mut static_budget = TestSignature::new("test_stack");
        static_budget.attr.budget = BudgetGate {
            max_stack: Some(4096),
            const_time: true,
            ..BudgetGate::default()
        };
        assert!(is_cacheable(&classify(&static_budget)));

        let mut ffi_sig = TestSignature::new("test_ffi");
        ffi_sig.attr.nondeterministic_ffi = true;
        assert!(!is_cacheable(&classify(&ffi_sig)));

        let mut io_sig = TestSignature::new("test_fs");
        io_sig.effects.push("Fs".into());
        assert!(!is_cacheable(&classify(&io_sig)));

        let mut sim_sig = TestSignature::new("test_sim");
        sim_sig.effects.extend(["Net".into(), "Storage".into()]);
        assert!(is_cacheable(&classify(&sim_sig)));
    }
}
