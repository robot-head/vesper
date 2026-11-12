//! Builder/engine identity: the execution fingerprint bound into every key.

use serde::{Deserialize, Serialize};
use vesper_loom_sandbox::transport::{PROTOCOL_VERSION, WIT_PACKAGE, WIT_SOURCE};

const UNKNOWN: &str = "unknown";

/// Everything about the executing implementation that can change what a
/// build step produces, independent of the request.
///
/// [`ExecutionIdentity::current`] is computed at compile time by `build.rs`
/// from source digests, the locked engine dependency set and the compiler.
/// A field that could not be determined is `None`; such an identity is
/// incomplete and the runner refuses to read or publish reusable results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    /// `vesper-loom-build` package version.
    pub builder_version: String,
    /// BLAKE3 over this crate's sources and build script.
    pub builder_source: Option<String>,
    /// BLAKE3 over `vesper-loom-sandbox` sources and WIT (engine
    /// configuration, WASI policy and filesystem normalization live there).
    pub sandbox_source: Option<String>,
    /// BLAKE3 over `vesper-loom-cas` sources (tree/derivation encodings).
    pub cas_source: Option<String>,
    /// BLAKE3 over locked Wasmtime/Cranelift/WASI/cap-std package versions.
    pub engine_dependencies: Option<String>,
    /// `rustc -vV` of the compiler that built the runner.
    pub rustc: Option<String>,
    /// Target triple the runner (and so the Wasmtime JIT) runs on.
    pub host_target: Option<String>,
    /// WIT package name of the step protocol.
    pub wit_package: String,
    /// BLAKE3 of the WIT source text.
    pub wit_digest: String,
    /// Payload protocol version.
    pub protocol_version: u32,
}

fn known(value: &'static str) -> Option<String> {
    (value != UNKNOWN && !value.is_empty()).then(|| value.to_owned())
}

impl ExecutionIdentity {
    /// The identity of this compiled runner.
    pub fn current() -> Self {
        ExecutionIdentity {
            builder_version: env!("CARGO_PKG_VERSION").to_owned(),
            builder_source: known(env!("VLB_BUILDER_SRC")),
            sandbox_source: known(env!("VLB_SANDBOX_SRC")),
            cas_source: known(env!("VLB_CAS_SRC")),
            engine_dependencies: known(env!("VLB_ENGINE_DEPS")),
            rustc: known(env!("VLB_RUSTC")),
            host_target: known(env!("VLB_HOST_TARGET")),
            wit_package: WIT_PACKAGE.to_owned(),
            wit_digest: blake3::hash(WIT_SOURCE.as_bytes()).to_hex().to_string(),
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Names of the fields that could not be determined.
    pub fn missing(&self) -> Vec<&'static str> {
        [
            ("builder_source", &self.builder_source),
            ("sandbox_source", &self.sandbox_source),
            ("cas_source", &self.cas_source),
            ("engine_dependencies", &self.engine_dependencies),
            ("rustc", &self.rustc),
            ("host_target", &self.host_target),
        ]
        .into_iter()
        .filter(|(_, v)| v.as_deref().is_none_or(str::is_empty))
        .map(|(n, _)| n)
        .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }

    /// `(option key, value)` pairs bound into the derivation key.
    /// Unknown fields encode as `unknown`; keys built from an incomplete
    /// identity are never used for lookup or publication.
    pub(crate) fn key_options(&self) -> Vec<(&'static str, String)> {
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| UNKNOWN.to_owned());
        vec![
            ("builder.source", opt(&self.builder_source)),
            ("cas.source", opt(&self.cas_source)),
            ("engine.dependencies", opt(&self.engine_dependencies)),
            ("engine.host-target", opt(&self.host_target)),
            ("engine.rustc", opt(&self.rustc)),
            ("sandbox.protocol", self.protocol_version.to_string()),
            ("sandbox.source", opt(&self.sandbox_source)),
            ("sandbox.wit-blake3", self.wit_digest.clone()),
            ("sandbox.wit-package", self.wit_package.clone()),
        ]
    }
}
