//! Hermetic Wasmtime component host for Vesper Loom build steps and Forge
//! Tier 2 macros (design §§8.2, 16.4).
//!
//! A step is a `wasm32-wasip2` component exporting
//! `vesper:sandbox/step@0.1.0` (see `wit/sandbox.wit`). Each run gets:
//!
//! * a fuel budget and aggregate memory/table/instance limits,
//! * the declared input directories, read-only,
//! * one dedicated, initially empty, writable output directory at `/out`,
//! * no environment variables, arguments, stdin, or socket authority,
//! * a fixed wall clock, a non-advancing monotonic clock, and seeded
//!   reproducible randomness,
//! * bounded captured stdout/stderr.
//!
//! Isolation is Wasmtime's software sandbox plus `cap-std` directory
//! capabilities. This crate makes no hardware-isolation claim. Fuel makes
//! instruction counts reproducible for a fixed engine configuration; time
//! spent blocked in host calls is bounded only by a wall-clock backstop.
//!
//! Scope-set hygiene and macro expansion are not implemented here; this
//! crate only transports tokens with their scope marks.
//!
//! ```no_run
//! use vesper_loom_sandbox::{Invocation, SandboxHost, StepConfig, StepKind};
//! let host = SandboxHost::new()?;
//! let step = host.compile(&std::fs::read("step.wasm").unwrap())?;
//! let cfg = StepConfig::new("target/out").input("proto", "/in/proto");
//! let inv = Invocation::new(StepKind::BuildScript, "codegen")
//!     .with_args(["/in/proto/schema.txt", "/out/generated.vsp"]);
//! let report = host.run(&step, &cfg, &inv)?;
//! println!("{:?} fuel={}", report.result, report.fuel_consumed);
//! # Ok::<(), vesper_loom_sandbox::SandboxError>(())
//! ```

mod artifact;
mod fs_determinism;
mod host;
mod policy;
pub mod quote_transport;
pub mod transport;

pub use artifact::{ArtifactKind, classify};
pub use host::{OutputFile, OutputViolation, SandboxError, SandboxHost, StepComponent, StepReport};
pub use policy::{
    ConfigError, Determinism, InputMount, OUTPUT_GUEST_PATH, ResourceLimits, StepConfig,
};
pub use transport::{
    Diagnostic, Invocation, Outcome, PROTOCOL_VERSION, STEP_INTERFACE, Severity, StepKind, Token,
    TokenKind, TransportLimits, WIT_PACKAGE,
};
