// The native walk's `register_locals` plan and entry-block plans (gap lane
// 11, #1509), from `modules/codegen_walk`, through the `register_locals`
// and `frame_plans` hooks.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_walk.rs"]
mod codegen_walk_binding;

// The native walk swap's encoder and decoder (#1509), shared with
// tests/self_host_native_walk.rs.
#[path = "../../../../tests/common/native_walk_words.rs"]
mod native_walk_words;

// `@vesper/codegen-calls`' input encoder, for the `frame_plans` swap (#1509
// part 3b), shared with tests/self_host_native_walk_frame.rs.
#[path = "../../../../tests/common/codegen_calls_words.rs"]
mod codegen_calls_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, env_off, ir_words, on, open};

/// `register_locals` plans answered by Vesper (`@vesper/native-walk`), and declined.
static WALK_CALLS: AtomicU64 = AtomicU64::new(0);
static WALK_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Entry compiles whose entry-block plans came from Vesper, and declined.
static FRAME_CALLS: AtomicU64 = AtomicU64::new(0);
static FRAME_DECLINED: AtomicU64 = AtomicU64::new(0);

/// The `register_locals` hook of `vesper::codegen_swap` from `modules/codegen_walk`
/// (#1509): `@vesper/native-walk` decides which scalar/parameter locals stay in
/// Cranelift SSA variables. Non-encodable graphs decline to Rust.
fn vesper_register_locals(
    world: &vesper::check::World,
    f: &vesper::ir::FunctionIr,
    generics: &std::collections::BTreeMap<String, vesper::ast::Type>,
    nested: bool,
) -> Option<std::collections::BTreeSet<String>> {
    let Some(bytes) = native_walk_words::function_input(world, f, generics, nested) else {
        WALK_DECLINED.fetch_add(1, Relaxed);
        return None;
    };
    let words = codegen_walk_binding::register_locals_words(&bytes);
    WALK_CALLS.fetch_add(1, Relaxed);
    Some(native_walk_words::decode_register_locals(&words))
}

/// The `frame_plans` hook of `vesper::codegen_swap` from `modules/codegen_walk`
/// (#1509 part 3b): `@vesper/native-walk` plans each instance's entry block
/// for one entry compile. Worlds the calls input cannot encode, and tasks
/// the package refuses, decline to Rust.
fn vesper_frame_plans(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    optimize: bool,
) -> Option<std::collections::BTreeMap<String, vesper::native::FramePlan>> {
    let plans = codegen_calls_words::calls_input(world, program, &[(entry.to_string(), optimize)])
        .and_then(|bytes| {
            native_walk_words::decode_frames(&codegen_walk_binding::frame_plans_words(&bytes))
        });
    match plans {
        Some(_) => FRAME_CALLS.fetch_add(1, Relaxed),
        None => FRAME_DECLINED.fetch_add(1, Relaxed),
    };
    plans
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("register_locals", "0 diffs; no flip-gate configuration"),
    open("frame_plans", "0 diffs; no flip-gate configuration"),
];

pub(super) fn register(r: &mut Registry) {
    if on("register_locals") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.register_locals = Some(vesper_register_locals);
    }
    if on("frame_plans") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.frame_plans = Some(vesper_frame_plans);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: native walk planned register locals {} times from Vesper ({} declined)",
        WALK_CALLS.load(Relaxed),
        WALK_DECLINED.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: native walk planned entry blocks for {} compiles from Vesper ({} declined)",
        FRAME_CALLS.load(Relaxed),
        FRAME_DECLINED.load(Relaxed),
    );
}
