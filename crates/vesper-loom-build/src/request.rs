//! Versioned build requests and their validation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vesper_loom_sandbox::transport::PROTOCOL_VERSION;
use vesper_loom_sandbox::{
    ArtifactKind, Determinism, Invocation, OUTPUT_GUEST_PATH, ResourceLimits, StepKind,
    TransportLimits, classify,
};

use crate::error::RequestError;

/// Version of [`BuildRequest`] (and of the JSON request file).
pub const BUILD_REQUEST_VERSION: u32 = 1;

/// Byte limit for labels bound into the key (matches `vesper-loom-cas`).
const MAX_LABEL_BYTES: usize = 4096;

/// Network authority requested for the step. Only `Denied` exists in the
/// sandbox; `Allowed` is representable so that a request asking for it is
/// rejected explicitly instead of being silently downgraded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPolicy {
    #[default]
    Denied,
    Allowed,
}

/// One declared, read-only input directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    /// Absolute, normalized guest path such as `/in/proto`.
    pub guest_path: String,
    /// Host directory to snapshot. It is never mounted itself.
    pub host_path: PathBuf,
}

/// A build-step request. Every field is part of the derivation key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildRequest {
    pub version: u32,
    /// `wasm32-wasip2` component exporting `vesper:sandbox/step@0.1.0`.
    pub component: Vec<u8>,
    /// Must be `StepKind::BuildScript` at the current protocol version.
    pub invocation: Invocation,
    pub inputs: Vec<InputSpec>,
    /// The build target the step is producing for, when it matters.
    pub target: Option<String>,
    /// Builder options passed through to the key (feature sets, profiles).
    pub options: BTreeMap<String, String>,
    pub determinism: Determinism,
    pub limits: ResourceLimits,
    pub network: NetworkPolicy,
}

impl BuildRequest {
    /// A `BuildScript` request with default limits and determinism.
    pub fn build_script(component: Vec<u8>, entry: impl Into<String>) -> Self {
        BuildRequest {
            version: BUILD_REQUEST_VERSION,
            component,
            invocation: Invocation::new(StepKind::BuildScript, entry),
            inputs: Vec::new(),
            target: None,
            options: BTreeMap::new(),
            determinism: Determinism::default(),
            limits: ResourceLimits::default(),
            network: NetworkPolicy::Denied,
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.invocation = self.invocation.with_args(args);
        self
    }

    pub fn with_input(
        mut self,
        guest_path: impl Into<String>,
        host_path: impl Into<PathBuf>,
    ) -> Self {
        self.inputs.push(InputSpec {
            guest_path: guest_path.into(),
            host_path: host_path.into(),
        });
        self
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    /// Checks everything that can be checked without touching input
    /// contents. Called by the runner before any snapshot or execution.
    pub fn validate(&self) -> Result<(), RequestError> {
        if self.version != BUILD_REQUEST_VERSION {
            return Err(RequestError::UnsupportedVersion {
                found: self.version,
                supported: BUILD_REQUEST_VERSION,
            });
        }
        if self.invocation.protocol_version != PROTOCOL_VERSION {
            return Err(RequestError::UnsupportedProtocol {
                found: self.invocation.protocol_version,
                supported: PROTOCOL_VERSION,
            });
        }
        if self.invocation.step != StepKind::BuildScript {
            return Err(RequestError::UnsupportedStep(self.invocation.step));
        }
        if self.network != NetworkPolicy::Denied {
            return Err(RequestError::IncompatiblePolicy(
                "network access cannot be granted: the sandbox has no network mode".into(),
            ));
        }
        match classify(&self.component) {
            ArtifactKind::Component => {}
            detected => return Err(RequestError::NotAComponent(detected)),
        }
        label("entry", &self.invocation.entry, false)?;
        for arg in &self.invocation.args {
            label("argument", arg, true)?;
        }
        if let Some(t) = &self.target {
            label("target", t, false)?;
        }
        for (k, v) in &self.options {
            label("option key", k, false)?;
            label("option value", v, true)?;
        }
        check_limits(&self.limits)?;
        self.check_mounts()
    }

    fn check_mounts(&self) -> Result<(), RequestError> {
        let out = guest_components(OUTPUT_GUEST_PATH)?;
        let mut seen: Vec<(&str, Vec<&str>, PathBuf)> = Vec::new();
        for spec in &self.inputs {
            let parts = guest_components(&spec.guest_path)?;
            label("guest path", &spec.guest_path, false)?;
            if is_prefix(&parts, &out) || is_prefix(&out, &parts) {
                return Err(RequestError::MountOverlapsOutput(spec.guest_path.clone()));
            }
            let meta = std::fs::symlink_metadata(&spec.host_path).map_err(|e| {
                RequestError::InputUnavailable {
                    host_path: spec.host_path.clone(),
                    reason: e.to_string(),
                }
            })?;
            if !meta.is_dir() {
                return Err(RequestError::InputNotDirectory(spec.host_path.clone()));
            }
            let canon =
                spec.host_path
                    .canonicalize()
                    .map_err(|e| RequestError::InputUnavailable {
                        host_path: spec.host_path.clone(),
                        reason: e.to_string(),
                    })?;
            for (other, other_parts, other_canon) in &seen {
                if is_prefix(other_parts, &parts) || is_prefix(&parts, other_parts) {
                    return Err(RequestError::OverlappingMounts {
                        first: (*other).to_owned(),
                        second: spec.guest_path.clone(),
                    });
                }
                if canon.starts_with(other_canon) || other_canon.starts_with(&canon) {
                    return Err(RequestError::OverlappingHostInputs {
                        first: other_canon.clone(),
                        second: canon,
                    });
                }
            }
            seen.push((&spec.guest_path, parts, canon));
        }
        Ok(())
    }

    /// Canonical host paths of the declared inputs (after `validate`).
    pub(crate) fn canonical_inputs(&self) -> Result<Vec<PathBuf>, RequestError> {
        self.inputs
            .iter()
            .map(|s| {
                s.host_path
                    .canonicalize()
                    .map_err(|e| RequestError::InputUnavailable {
                        host_path: s.host_path.clone(),
                        reason: e.to_string(),
                    })
            })
            .collect()
    }
}

fn label(field: &'static str, value: &str, allow_empty: bool) -> Result<(), RequestError> {
    if (!allow_empty && value.is_empty())
        || value.len() > MAX_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(RequestError::Malformed {
            field,
            value: value.chars().take(64).collect(),
        });
    }
    Ok(())
}

fn check_limits(l: &ResourceLimits) -> Result<(), RequestError> {
    for (field, zero) in [
        ("fuel", l.fuel == 0),
        ("max_memory_bytes", l.max_memory_bytes == 0),
        ("max_instances", l.max_instances == 0),
        ("max_stdio_bytes", l.max_stdio_bytes == 0),
        ("max_output_entries", l.max_output_entries == 0),
        ("wall_clock_backstop", l.wall_clock_backstop.is_zero()),
    ] {
        if zero {
            return Err(RequestError::ZeroLimit(field));
        }
    }
    Ok(())
}

fn guest_components(path: &str) -> Result<Vec<&str>, RequestError> {
    let bad = || RequestError::InvalidGuestPath(path.to_owned());
    let rest = path.strip_prefix('/').ok_or_else(bad)?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts == [""]
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(['\\', '\0']))
    {
        return Err(bad());
    }
    Ok(parts)
}

fn is_prefix(a: &[&str], b: &[&str]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

// --------------------------------------------------------------------------
// JSON request file
// --------------------------------------------------------------------------

/// Optional overrides of [`ResourceLimits`]; omitted fields keep defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsFile {
    pub fuel: Option<u64>,
    pub max_memory_bytes: Option<usize>,
    pub max_table_elements: Option<usize>,
    pub max_instances: Option<usize>,
    pub max_memories: Option<usize>,
    pub max_tables: Option<usize>,
    pub max_stdio_bytes: Option<usize>,
    pub max_output_bytes: Option<u64>,
    pub max_output_entries: Option<usize>,
    pub wall_clock_backstop_ms: Option<u64>,
    pub transport: Option<TransportLimits>,
}

impl LimitsFile {
    fn apply(&self) -> ResourceLimits {
        let d = ResourceLimits::default();
        ResourceLimits {
            fuel: self.fuel.unwrap_or(d.fuel),
            max_memory_bytes: self.max_memory_bytes.unwrap_or(d.max_memory_bytes),
            max_table_elements: self.max_table_elements.unwrap_or(d.max_table_elements),
            max_instances: self.max_instances.unwrap_or(d.max_instances),
            max_memories: self.max_memories.unwrap_or(d.max_memories),
            max_tables: self.max_tables.unwrap_or(d.max_tables),
            max_stdio_bytes: self.max_stdio_bytes.unwrap_or(d.max_stdio_bytes),
            max_output_bytes: self.max_output_bytes.unwrap_or(d.max_output_bytes),
            max_output_entries: self.max_output_entries.unwrap_or(d.max_output_entries),
            wall_clock_backstop: self
                .wall_clock_backstop_ms
                .map(Duration::from_millis)
                .unwrap_or(d.wall_clock_backstop),
            transport: self.transport.unwrap_or(d.transport),
        }
    }
}

/// On-disk form of a request. Relative paths are resolved against the
/// directory containing the request file.
///
/// ```json
/// { "version": 1, "component": "step.component.wasm", "entry": "codegen",
///   "args": ["/in/proto/schema.txt", "/out/generated.vsp"],
///   "inputs": [{ "guest_path": "/in/proto", "host_path": "proto" }] }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFile {
    pub version: u32,
    pub component: PathBuf,
    /// `build-script` (default); macro kinds are rejected by validation.
    #[serde(default = "default_step")]
    pub step: StepName,
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<InputSpec>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub determinism: Option<Determinism>,
    #[serde(default)]
    pub limits: LimitsFile,
    #[serde(default)]
    pub network: NetworkPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepName {
    BuildScript,
    ProcMacro,
    DeriveMacro,
}

fn default_step() -> StepName {
    StepName::BuildScript
}

impl RequestFile {
    /// Parses a request file. The version is checked before the rest of the
    /// document, so a future format reports `UnsupportedVersion` rather than
    /// an unknown-field error.
    pub fn parse(text: &str) -> Result<Self, RequestError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| RequestError::Json(e.to_string()))?;
        let version = value
            .get("version")
            .ok_or_else(|| RequestError::Json("missing `version`".into()))?
            .as_u64()
            .ok_or_else(|| RequestError::Json("`version` is not an unsigned integer".into()))?;
        if version != u64::from(BUILD_REQUEST_VERSION) {
            return Err(RequestError::UnsupportedVersion {
                found: u32::try_from(version).unwrap_or(u32::MAX),
                supported: BUILD_REQUEST_VERSION,
            });
        }
        serde_json::from_value(value).map_err(|e| RequestError::Json(e.to_string()))
    }

    /// Loads a request file and the component it names.
    pub fn load(path: &Path) -> Result<BuildRequest, RequestError> {
        let text = std::fs::read_to_string(path).map_err(|e| RequestError::InputUnavailable {
            host_path: path.to_owned(),
            reason: e.to_string(),
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text)?.into_request(base)
    }

    pub fn into_request(self, base: &Path) -> Result<BuildRequest, RequestError> {
        let resolve = |p: &Path| {
            if p.is_absolute() {
                p.to_owned()
            } else {
                base.join(p)
            }
        };
        let component_path = resolve(&self.component);
        let component =
            std::fs::read(&component_path).map_err(|e| RequestError::InputUnavailable {
                host_path: component_path.clone(),
                reason: e.to_string(),
            })?;
        let step = match self.step {
            StepName::BuildScript => StepKind::BuildScript,
            StepName::ProcMacro => StepKind::ProcMacro,
            StepName::DeriveMacro => StepKind::DeriveMacro,
        };
        Ok(BuildRequest {
            version: self.version,
            component,
            invocation: Invocation::new(step, self.entry).with_args(self.args),
            inputs: self
                .inputs
                .into_iter()
                .map(|i| InputSpec {
                    guest_path: i.guest_path,
                    host_path: resolve(&i.host_path),
                })
                .collect(),
            target: self.target,
            options: self.options,
            determinism: self.determinism.unwrap_or_default(),
            limits: self.limits.apply(),
            network: self.network,
        })
    }
}
