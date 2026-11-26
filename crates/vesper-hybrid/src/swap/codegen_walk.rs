// The native walk's `register_locals` plan, entry plans (entry blocks and
// internal conventions) and block plans (gap lane 11, #1509), from
// `modules/codegen_walk`, through the `register_locals`, `entry_plans`,
// `export_plans` and `block_plan` hooks.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_walk.rs"]
mod codegen_walk_binding;

// The native walk swap's encoder and decoder (#1509), shared with
// tests/self_host_native_walk.rs.
#[path = "../../../../tests/common/native_walk_words.rs"]
mod native_walk_words;

// `@vesper/codegen-calls`' input encoder, for the `entry_plans` swap (#1509
// parts 3b and 4a), shared with tests/self_host_native_walk_frame.rs.
#[path = "../../../../tests/common/codegen_calls_words.rs"]
mod codegen_calls_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, env_off, ir_words, on, open, pass};

/// `register_locals` plans answered by Vesper (`@vesper/native-walk`), and declined.
static WALK_CALLS: AtomicU64 = AtomicU64::new(0);
static WALK_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Entry compiles whose entry-block plans came from Vesper, and declined.
static FRAME_CALLS: AtomicU64 = AtomicU64::new(0);
static FRAME_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Graphs whose block plan came from Vesper, and declined.
static BLOCK_CALLS: AtomicU64 = AtomicU64::new(0);
static BLOCK_DECLINED: AtomicU64 = AtomicU64::new(0);

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

/// The `entry_plans` hook of `vesper::codegen_swap` from `modules/codegen_walk`
/// (#1509 parts 3b and 4a): `@vesper/native-walk` plans each instance's
/// entry block and internal convention for one entry compile. Worlds the calls input cannot encode, and tasks
/// the package refuses, decline to Rust.
fn vesper_entry_plans(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    optimize: bool,
    trace: bool,
) -> Option<vesper::native::EntryPlans> {
    let flags = optimize as u64 | (trace as u64) << 1;
    let plans =
        codegen_calls_words::calls_input_flags(world, program, &[(entry.to_string(), flags)])
            .and_then(|bytes| {
                native_walk_words::decode_entry_plans(&codegen_walk_binding::frame_plans_words(
                    &bytes,
                ))
            });
    match plans {
        Some(_) => FRAME_CALLS.fetch_add(1, Relaxed),
        None => FRAME_DECLINED.fetch_add(1, Relaxed),
    };
    plans
}

/// The `export_plans` hook (#1509 part 5): one library task whose roots are
/// the exports (flags 4 on the first, 8 on the rest).
fn vesper_export_plans(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    roots: &[&str],
    optimize: bool,
) -> Option<vesper::native::EntryPlans> {
    let tasks = library_tasks(roots, optimize);
    let plans = codegen_calls_words::calls_input_flags(world, program, &tasks).and_then(|bytes| {
        native_walk_words::decode_entry_plans(&codegen_walk_binding::frame_plans_words(&bytes))
    });
    match plans {
        Some(_) => FRAME_CALLS.fetch_add(1, Relaxed),
        None => FRAME_DECLINED.fetch_add(1, Relaxed),
    };
    plans
}

/// The `block_plan` hook (#2094, part 6a): one graph's block order, cyclic
/// blocks and early seals from its successor lists. A graph the package
/// declines (a successor out of range) goes to Rust.
fn vesper_block_plan(targets: &[Vec<usize>]) -> Option<vesper::native::BlockPlan> {
    let words =
        codegen_walk_binding::block_plan_words(&native_walk_words::block_targets_input(targets));
    let plan = native_walk_words::decode_block_plan(&words, targets.len());
    match plan {
        Some(_) => BLOCK_CALLS.fetch_add(1, Relaxed),
        None => BLOCK_DECLINED.fetch_add(1, Relaxed),
    };
    plan
}

/// The calls tasks of a library build over `roots`.
fn library_tasks(roots: &[&str], optimize: bool) -> Vec<(String, u64)> {
    roots
        .iter()
        .enumerate()
        .map(|(i, r)| (r.to_string(), optimize as u64 | if i == 0 { 4 } else { 8 }))
        .collect()
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("register_locals", "0 diffs; no flip-gate configuration"),
    pass("entry_plans", "native-walk", "#2056"),
    pass("export_plans", "native-walk", "#2056"),
    open("block_plan", "0 diffs; no flip-gate configuration"),
];

pub(super) fn register(r: &mut Registry) {
    if on("register_locals") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.register_locals = Some(vesper_register_locals);
    }
    if on("entry_plans") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.entry_plans = Some(vesper_entry_plans);
    }
    if on("export_plans") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.export_plans = Some(vesper_export_plans);
    }
    if on("block_plan") && !env_off("VESPER_HYBRID_WALK") {
        r.codegen.block_plan = Some(vesper_block_plan);
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
    eprintln!(
        "vesper-hybrid: native walk planned blocks for {} graphs from Vesper ({} declined)",
        BLOCK_CALLS.load(Relaxed),
        BLOCK_DECLINED.load(Relaxed),
    );
}
