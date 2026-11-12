//! Offline Loom build-step runner (design §8.2): connects the
//! `vesper-loom-cas` store to the `vesper-loom-sandbox` WASI component host.
//!
//! A [`BuildRequest`] names a `wasm32-wasip2` component, a build-script
//! invocation, read-only input directories with their guest mount points,
//! the target, options, determinism settings and resource limits. The
//! [`Runner`]:
//!
//! 1. validates the request (version, step kind, policy, mounts, limits)
//!    before touching any input;
//! 2. snapshots the component, the canonical invocation and every input
//!    tree into the store (two passes that must agree);
//! 3. computes a derivation key that binds all of those plus the
//!    builder/sandbox/CAS sources, locked engine dependencies, compiler and
//!    host target ([`ExecutionIdentity`]);
//! 4. on a hit, re-verifies every referenced object and materializes the
//!    outputs from the store; on a miss, runs the real sandbox against fresh
//!    materializations of the snapshots with a fresh, empty `/out`, accepts
//!    the result only if the guest succeeded and the output tree is valid,
//!    stores the output tree and a [`ResultManifest`], and only then
//!    publishes the derivation record.
//!
//! Only `StepKind::BuildScript` is accepted. A derivation record is a local
//! builder's claim, not provenance; nothing here certifies package
//! confinement or authority.
//!
//! ```no_run
//! use std::sync::Arc;
//! use vesper_loom_build::{BuildEngine, BuildRequest, RunOptions, Runner};
//! let engine = Arc::new(BuildEngine::new()?);
//! let runner = Runner::open(engine, "cache/store", "cache/scratch")?;
//! let request = BuildRequest::build_script(std::fs::read("step.component.wasm").unwrap(), "codegen")
//!     .with_input("/in/proto", "proto")
//!     .with_args(["/in/proto/schema.txt", "/out/generated.vsp"]);
//! let outcome = runner.build(&request, "generated", RunOptions::default())?;
//! println!("{:?} {}", outcome.cache, outcome.key);
//! # Ok::<(), vesper_loom_build::BuildError>(())
//! ```

#![warn(missing_debug_implementations)]

mod error;
mod fsx;
mod identity;
mod key;
mod manifest;
mod request;
mod runner;

pub use error::{BuildError, RequestError};
pub use identity::ExecutionIdentity;
pub use key::{BUILDER_NAME, KEY_SCHEMA_VERSION, canonical_invocation};
pub use manifest::{RESULT_MANIFEST_SCHEMA, ResultManifest};
pub use request::{
    BUILD_REQUEST_VERSION, BuildRequest, InputSpec, LimitsFile, NetworkPolicy, RequestFile,
    StepName,
};
pub use runner::{
    BUILD_OUTCOME_VERSION, BuildEngine, BuildOutcome, CachePolicy, CacheStatus, CorruptionPolicy,
    DestinationPolicy, PreparedBuild, RunOptions, Runner,
};
