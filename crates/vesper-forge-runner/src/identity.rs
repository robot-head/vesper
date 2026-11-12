//! Identity of the host implementation that executes macro components.
//!
//! Everything about the executing implementation that could change what a
//! component observably produces is bound into the execution key. Instead of
//! trusting version labels or source fingerprints, the runner hashes the
//! bytes of the running executable and, on Linux, of every file-backed
//! executable mapping of the process (the dynamic loader, libc and any other
//! shared object). Wasmtime, Cranelift, WASI, the sandbox policy, the Forge
//! importer and this crate are linked statically and so are covered by the
//! executable digest. Any rebuild of the host therefore invalidates every
//! cached execution, including rebuilds that change nothing observable.
//!
//! An identity with any field that could not be determined is incomplete;
//! the runner then refuses to read or publish cached executions.

use std::fs::File;
use std::io::Read;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use vesper_loom_sandbox::transport::{PROTOCOL_VERSION, WIT_PACKAGE, WIT_SOURCE};

use crate::record::RECORD_SCHEMA_VERSION;

const UNKNOWN: &str = "unknown";

/// Complete identity of the executing host implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    /// `vesper-forge-runner` package version (informational; the executable
    /// digest is what binds the implementation).
    pub runner_version: String,
    /// BLAKE3 of the running executable's bytes.
    pub executable: Option<String>,
    /// How the executable was located (`/proc/self/exe` opens the mapped
    /// inode even if the path was replaced after start).
    pub executable_source: String,
    /// BLAKE3 over `(path, bytes)` of every file-backed executable mapping
    /// in `/proc/self/maps`, sorted by path. `None` where unavailable.
    pub mapped_objects: Option<String>,
    /// Whether `mapped_objects` is required for completeness on this OS.
    pub mapped_objects_required: bool,
    /// `rustc -vV` of the compiler that built the runner.
    pub rustc: Option<String>,
    /// Target triple the runner (and so the Wasmtime JIT) runs on.
    pub host_target: Option<String>,
    /// Runtime-detected CPU features that Cranelift may select code by.
    pub cpu_features: String,
    pub wit_package: String,
    /// BLAKE3 of the sandbox protocol WIT source.
    pub wit_digest: String,
    pub protocol_version: u32,
    /// Raw execution record encoding version.
    pub record_schema: u32,
}

fn known(value: &'static str) -> Option<String> {
    (value != UNKNOWN && !value.is_empty()).then(|| value.to_owned())
}

fn digest_reader(context: &str, mut r: impl Read) -> std::io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

fn executable_digest() -> (Option<String>, String) {
    #[cfg(target_os = "linux")]
    let (file, source) = (File::open("/proc/self/exe"), "/proc/self/exe".to_owned());
    #[cfg(not(target_os = "linux"))]
    let (file, source) = match std::env::current_exe() {
        Ok(p) => (File::open(&p), format!("current_exe {}", p.display())),
        Err(e) => (Err(e), "current_exe".to_owned()),
    };
    let digest = file
        .and_then(|f| digest_reader("vesper-forge-runner 2026-11 executable", f))
        .ok()
        .map(|h| h.to_hex().to_string());
    (digest, source)
}

#[cfg(target_os = "linux")]
fn mapped_objects_digest() -> Option<String> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let mut paths = std::collections::BTreeSet::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let perms = fields.nth(1)?;
        // offset, dev, inode
        let path: Vec<&str> = fields.skip(3).collect();
        if !perms.contains('x') || path.is_empty() {
            continue;
        }
        let path = path.join(" ");
        if !path.starts_with('/') {
            // `[vdso]`, `[vsyscall]`: kernel-provided, not a file.
            continue;
        }
        paths.insert(path);
    }
    if paths.is_empty() {
        return None;
    }
    let mut hasher = blake3::Hasher::new_derive_key("vesper-forge-runner 2026-11 mapped-objects");
    for path in &paths {
        // A replaced or deleted mapping cannot be re-read faithfully.
        if path.ends_with(" (deleted)") {
            return None;
        }
        let file = File::open(path).ok()?;
        let digest = digest_reader("vesper-forge-runner 2026-11 mapped-object", file).ok()?;
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(digest.as_bytes());
    }
    Some(hasher.finalize().to_hex().to_string())
}

#[cfg(not(target_os = "linux"))]
fn mapped_objects_digest() -> Option<String> {
    None
}

fn cpu_features() -> String {
    #[allow(unused_mut)]
    let mut found: Vec<&str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => {$(
                if std::arch::is_x86_feature_detected!($f) { found.push($f); }
            )*};
        }
        probe!(
            "sse3",
            "ssse3",
            "sse4.1",
            "sse4.2",
            "popcnt",
            "avx",
            "avx2",
            "bmi1",
            "bmi2",
            "lzcnt",
            "fma",
            "avx512f",
            "avx512vl",
            "avx512dq",
            "avx512bitalg",
            "avx512vbmi"
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => {$(
                if std::arch::is_aarch64_feature_detected!($f) { found.push($f); }
            )*};
        }
        probe!("neon", "lse", "fp16", "paca", "bti");
    }
    format!("{}:{}", std::env::consts::ARCH, found.join(","))
}

impl HostIdentity {
    /// The identity of this running process, computed once.
    pub fn current() -> Self {
        static CURRENT: OnceLock<HostIdentity> = OnceLock::new();
        CURRENT
            .get_or_init(|| {
                let (executable, executable_source) = executable_digest();
                HostIdentity {
                    runner_version: env!("CARGO_PKG_VERSION").to_owned(),
                    executable,
                    executable_source,
                    mapped_objects: mapped_objects_digest(),
                    mapped_objects_required: cfg!(target_os = "linux"),
                    rustc: known(env!("VFR_RUSTC")),
                    host_target: known(env!("VFR_HOST_TARGET")),
                    cpu_features: cpu_features(),
                    wit_package: WIT_PACKAGE.to_owned(),
                    wit_digest: blake3::hash(WIT_SOURCE.as_bytes()).to_hex().to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    record_schema: RECORD_SCHEMA_VERSION,
                }
            })
            .clone()
    }

    /// Names of fields that could not be determined.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.executable.as_deref().is_none_or(str::is_empty) {
            out.push("executable");
        }
        if self.mapped_objects_required && self.mapped_objects.as_deref().is_none_or(str::is_empty)
        {
            out.push("mapped_objects");
        }
        if self.rustc.as_deref().is_none_or(str::is_empty) {
            out.push("rustc");
        }
        if self.host_target.as_deref().is_none_or(str::is_empty) {
            out.push("host_target");
        }
        out
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }

    /// `(option key, value)` pairs bound into the execution key. Unknown
    /// values encode as `unknown`; such keys are never looked up or
    /// published.
    pub(crate) fn key_options(&self) -> Vec<(&'static str, String)> {
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| UNKNOWN.to_owned());
        vec![
            ("host.cpu-features", self.cpu_features.clone()),
            ("host.executable", opt(&self.executable)),
            ("host.mapped-objects", opt(&self.mapped_objects)),
            ("host.rustc", opt(&self.rustc)),
            ("host.target", opt(&self.host_target)),
            ("host.runner-version", self.runner_version.clone()),
            ("sandbox.protocol", self.protocol_version.to_string()),
            ("sandbox.wit-blake3", self.wit_digest.clone()),
            ("sandbox.wit-package", self.wit_package.clone()),
            ("vfr.record-schema", self.record_schema.to_string()),
        ]
    }
}
