//! The result manifest: the single object a derivation record points to.

use serde::{Deserialize, Serialize};
use vesper_loom_sandbox::{OutputFile, transport::Outcome};

/// Schema tag of [`ResultManifest`].
pub const RESULT_MANIFEST_SCHEMA: &str = "vesper-loom-build/result/v1";

/// A complete, accepted build result, stored as an `Artifact` blob whose
/// bytes are this struct's canonical JSON. Object ids are in their strict
/// `b3v1:` text form; the derivation key in its `drv:b3v1:` form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultManifest {
    pub schema: String,
    pub key: String,
    /// `Artifact` tree of the output directory.
    pub output_tree: String,
    /// Regular files in the output tree, sorted, as scanned by the sandbox.
    pub outputs: Vec<OutputFile>,
    /// The guest's successful outcome (tokens and non-error diagnostics).
    pub outcome: Outcome,
    /// `Artifact` blobs of the captured stdout and stderr.
    pub stdout: String,
    pub stderr: String,
    pub fuel_consumed: u64,
    pub peak_memory_bytes: usize,
}

impl ResultManifest {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serialization is infallible")
    }

    /// Strict decode: the bytes must be exactly the canonical encoding of
    /// the decoded value, with the expected schema tag.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let m: ResultManifest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if m.schema != RESULT_MANIFEST_SCHEMA {
            return Err(format!("unknown manifest schema `{}`", m.schema));
        }
        if m.encode() != bytes {
            return Err("manifest is not canonically encoded".into());
        }
        Ok(m)
    }
}
