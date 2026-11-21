//! The native path's IR swap point (self-hosting SH3 lane 1, #1240).
//!
//! The native backend's IR pipeline is lowering (with the FFI handle
//! drops), `ir_opt::optimize`, the inliner, `fip` drop elaboration and
//! bounds hoisting. Each SH3 IR lane ports one of those passes to Vesper
//! and installs it here for the hybrid Stage 2 build: [`IrPasses`] holds
//! an optional hook per pass, and the native call sites (src/native.rs,
//! src/native_export.rs, src/native_session.rs) call [`lower`],
//! [`optimize`], [`inline`] and [`hoist`] instead of the Rust passes.
//!
//! Nothing in the default compiler installs a hook, so every function here
//! runs the Rust pass and the default compiler's objects are unchanged.
//! The verified backend (src/verified_codegen), wasm, eBPF, Lean and the
//! evaluator never call this module: they keep `ir::lower_world` and
//! `ir_opt::optimize` (the Stage 1 guard). An installed hook must return
//! what the Rust pass returns; the hybrid's tests and the pass harness of
//! tests/self_host_place_ir*.rs check it.
//!
//! A hook should not re-encode the world on every call: the native call
//! sites run inside a [`crate::swap_input`] scope (one per compilation),
//! which gives each hook a per-compilation slot for its encoder state and
//! the Vesper value it decoded once (SH4 lane 2, #1276).
use crate::check::World;
use crate::ir::Program;
use crate::ir_inline::{Budget, Report};
use std::sync::OnceLock;

/// The `opt` hook: `ir_opt::optimize`'s signature.
pub type OptHook = fn(&World, &mut Program) -> Result<usize, String>;

/// The hooks of the native IR pipeline; `None` runs the Rust pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct IrPasses {
    /// Replaces `ir::lower_world` (the FFI handle drops are still inserted
    /// after it by Rust).
    pub lower: Option<fn(&World) -> Program>,
    /// Replaces `ir_opt::optimize`.
    pub opt: Option<OptHook>,
    /// Replaces `ir_inline::inline`.
    pub inline: Option<fn(&World, &mut Program, &Budget) -> Report>,
    /// Replaces `bounds_hoist::run`.
    pub hoist: Option<fn(&mut Program) -> usize>,
}

static PASSES: OnceLock<IrPasses> = OnceLock::new();

/// Installs `passes` for the rest of the process. Only the first call takes
/// effect; a later one returns its table back.
pub fn install(passes: IrPasses) -> Result<(), IrPasses> {
    PASSES.set(passes)
}

/// The installed hooks (all `None` in the default compiler).
pub fn installed() -> IrPasses {
    PASSES.get().copied().unwrap_or_default()
}

/// The native path's lowering: `ffi_lowering::drops::lower_world`, or the
/// `lower` hook followed by the same drop insertion.
pub fn lower(world: &World) -> Program {
    match installed().lower {
        Some(f) => {
            let mut program = f(world);
            crate::ffi_lowering::drops::insert_drops(world, &mut program);
            program
        }
        None => crate::ffi_lowering::drops::lower_world(world),
    }
}

/// The native path's `ir_opt::optimize`.
pub fn optimize(world: &World, program: &mut Program) -> Result<usize, String> {
    match installed().opt {
        Some(f) => f(world, program),
        None => crate::ir_opt::optimize(world, program),
    }
}

/// The native path's `ir_inline::inline`.
pub fn inline(world: &World, program: &mut Program, budget: &Budget) -> Report {
    match installed().inline {
        Some(f) => f(world, program, budget),
        None => crate::ir_inline::inline(world, program, budget),
    }
}

/// The native path's `bounds_hoist::run`.
pub fn hoist(program: &mut Program) -> usize {
    match installed().hoist {
        Some(f) => f(program),
        None => crate::bounds_hoist::run(program),
    }
}
