//! Compiler/evaluator facade retaining the original public module paths.
//!
//! Dependency direction: `vesper -> vesper-syntax -> vesper-core`, with
//! `vesper -> vesper-core` and `vesper -> vesper-pulse`. Pulse has no compiler
//! dependencies. Checking, Place IR, optimization and both backends remain here:
//! checking currently invokes IR validation while lowering needs checked World
//! metadata, so extracting them independently would create a crate cycle.
pub use vesper_core::{ast, numeric};
pub use vesper_pulse::frames;
#[cfg(target_os = "linux")]
pub use vesper_pulse::{io_driver, scheduler};
pub use vesper_syntax::{lexer, parser};
pub mod capability_effects;
pub mod check;
pub mod database;
pub mod doctest;
pub mod forge_frontend;
pub mod interface;
#[cfg(target_os = "linux")]
pub mod io_session;
pub mod ir;
pub mod ir_check;
pub mod ir_opt;
pub mod layout;
pub mod lean;
mod lean_ir;
pub(crate) mod lean_nodes;
pub mod lsp;
pub mod native;
pub mod native_coverage;
pub mod native_verifiers;
pub mod optimize;
pub mod pact_frontend;
pub mod runtime;
pub mod stack_bound;
pub mod tooling;
