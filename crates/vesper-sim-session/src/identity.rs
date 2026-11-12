//! Replay binding: which executable, program, scenario and semantics produced a run.
use crate::{
    model::{MODEL_SEMANTICS, WARP_MODEL_SEMANTICS},
    program::{CheckedProgram, sha256_hex},
    scenario::{Plan, canonical, param_name},
    value::type_name,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::OnceLock;

/// Bumped whenever the adapter changes how source operations map to SimPulse
/// transitions or source trace events.
pub const SEMANTICS: &str = "vesper-reference-cek+simpulse-journal-v1+sim-session-v2";

/// Every field must match for a replay to proceed. The executable digest is
/// the SHA-256 of the running binary, which statically links the compiler,
/// checker, reference evaluator and SimPulse kernel; a package version or a
/// caller-provided label is never accepted in its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub executable_sha256: String,
    pub semantics: String,
    /// The model semantics ([`MODEL_SEMANTICS`], or [`WARP_MODEL_SEMANTICS`]
    /// for scenario version 3).
    pub model_semantics: String,
    pub source_sha256: String,
    pub scenario_sha256: String,
    pub bindings_sha256: String,
    pub options_sha256: String,
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityMismatch {
    pub field: &'static str,
    pub recorded: String,
    pub current: String,
}

impl ExecutionIdentity {
    pub(crate) fn compute(program: &CheckedProgram, plan: &Plan) -> Result<Self, String> {
        let scenario = plan.scenario();
        let scenario_json =
            serde_json::to_value(scenario).map_err(|e| format!("scenario digest: {e}"))?;
        let bindings: Vec<_> = plan
            .bindings
            .iter()
            .map(|((effect, operation), bound)| {
                json!({
                    "effect": effect,
                    "operation": operation,
                    "params": bound.params.iter().map(|(p, t)| param_name(*p, t)).collect::<Vec<_>>(),
                    "result": type_name(&bound.result),
                    "action": serde_json::to_value(&bound.action).unwrap(),
                })
            })
            .collect();
        let options = json!({
            "limits": serde_json::to_value(&scenario.limits).unwrap(),
            "runtime": serde_json::to_value(&scenario.runtime).unwrap(),
        });
        Ok(Self {
            executable_sha256: executable_sha256()?,
            semantics: SEMANTICS.into(),
            model_semantics: if plan.model.as_ref().is_some_and(|m| m.spec.warp.is_some()) {
                WARP_MODEL_SEMANTICS
            } else {
                MODEL_SEMANTICS
            }
            .into(),
            source_sha256: program.source_sha256().into(),
            scenario_sha256: sha256_hex(canonical(&scenario_json).as_bytes()),
            bindings_sha256: sha256_hex(canonical(&json!(bindings)).as_bytes()),
            options_sha256: sha256_hex(canonical(&options).as_bytes()),
            seed: scenario.seed,
        })
    }

    /// The first differing field, checked in a fixed order.
    pub fn compare(&self, current: &Self) -> Result<(), IdentityMismatch> {
        let fields: [(&'static str, String, String); 8] = [
            (
                "executable_sha256",
                self.executable_sha256.clone(),
                current.executable_sha256.clone(),
            ),
            (
                "semantics",
                self.semantics.clone(),
                current.semantics.clone(),
            ),
            (
                "model_semantics",
                self.model_semantics.clone(),
                current.model_semantics.clone(),
            ),
            (
                "source_sha256",
                self.source_sha256.clone(),
                current.source_sha256.clone(),
            ),
            (
                "scenario_sha256",
                self.scenario_sha256.clone(),
                current.scenario_sha256.clone(),
            ),
            (
                "bindings_sha256",
                self.bindings_sha256.clone(),
                current.bindings_sha256.clone(),
            ),
            (
                "options_sha256",
                self.options_sha256.clone(),
                current.options_sha256.clone(),
            ),
            ("seed", self.seed.to_string(), current.seed.to_string()),
        ];
        for (field, recorded, current) in fields {
            if recorded != current {
                return Err(IdentityMismatch {
                    field,
                    recorded,
                    current,
                });
            }
        }
        Ok(())
    }
}

/// SHA-256 of the current executable's bytes, computed once per process.
pub fn executable_sha256() -> Result<String, String> {
    static DIGEST: OnceLock<Result<String, String>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let path = std::env::current_exe()
                .map_err(|e| format!("cannot locate the embedding executable: {e}"))?;
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("cannot read {} for its digest: {e}", path.display()))?;
            Ok(sha256_hex(&bytes))
        })
        .clone()
}
