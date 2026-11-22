//! Compiler/evaluator facade retaining the original public module paths.
//!
//! Dependency direction: `vesper -> vesper-syntax -> vesper-core`, with
//! `vesper -> vesper-core` and `vesper -> vesper-pulse`. Pulse has no compiler
//! dependencies. Checking, Place IR, optimization and both backends remain here:
//! checking currently invokes IR validation while lowering needs checked World
//! metadata, so extracting them independently would create a crate cycle.
include!("compiler_modules.rs");
