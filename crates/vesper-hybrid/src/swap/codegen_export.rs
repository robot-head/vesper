// The export ABI v2 glue, the export planner and the interface file
// (SH4 lane 12, #1286), from `modules/codegen_export`, through the
// `export`, `plan` and `interface_json` hooks.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_export.rs"]
mod codegen_export_binding;

// The export glue's interface encoder (#1286), shared with the root
// package's tests/self_host_codegen_export.rs.
#[allow(dead_code)]
#[path = "../../../../tests/self_host_codegen_export/words.rs"]
mod export_words;

// The planner's inputs and answer (#1286), shared with the root package's
// tests/self_host_codegen_export.rs.
#[allow(dead_code)]
#[path = "../../../../tests/self_host_codegen_export/plan_words.rs"]
mod plan_words;

use super::{Registry, Swap, on, open};

/// Libraries whose v2 glue the Vesper export module emitted, and declined.
static EXPORT_CALLS: AtomicU64 = AtomicU64::new(0);
static EXPORT_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Library interfaces the Vesper planner planned.
static PLAN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Interface files the Vesper writer wrote.
static JSON_CALLS: AtomicU64 = AtomicU64::new(0);

/// The `plan` hook of `vesper::codegen_swap` from `modules/codegen_export`
/// (#1286): `plan_with` in Vesper. The checker's answers about each type
/// travel with the request (plan_words.rs `plan_input`).
fn vesper_plan(
    world: &vesper::check::World,
    public: &std::collections::BTreeSet<String>,
    library: &str,
    directives: &vesper::native::export::Directives,
) -> Option<
    Result<vesper::native::export::LibraryInterface, Vec<vesper::native::export::ExportError>>,
> {
    let input = plan_words::plan_input(world, public, library, directives);
    PLAN_CALLS.fetch_add(1, Relaxed);
    Some(plan_words::decode_plan(&codegen_export_binding::plan(
        &input,
    )))
}

/// The `interface_json` hook of `vesper::codegen_swap` from
/// `modules/codegen_export` (#1286): the interface file in Vesper, from the
/// interface in the binding generator's words.
fn vesper_interface_json(interface: &vesper::native::export::LibraryInterface) -> Option<String> {
    let input = export_words::binding_input(interface, &Default::default());
    let text = String::from_utf8(codegen_export_binding::interface_file(&input)).ok()?;
    JSON_CALLS.fetch_add(1, Relaxed);
    Some(text)
}

/// The `export` hook of `vesper::codegen_swap` from `modules/codegen_export`
/// (#1286): the export ABI v2 glue after the runtime prelude, emitted in
/// Vesper over the lent module. The request's words are a flags word (what
/// the module already holds, `glue.ves`'s `F_*`) and the interface in the
/// words of tests/self_host_codegen_export/words.rs.
fn vesper_export(
    m: &mut dyn cranelift_module::Module,
    _world: &vesper::check::World,
    interface: &vesper::native::export::LibraryInterface,
) -> Option<Result<(), String>> {
    use cranelift_module::FuncOrDataId;
    // The Rust glue reports a library with no runtime symbols.
    interface.runtime.as_ref()?;
    let func = |name: &str| matches!(m.get_name(name), Some(FuncOrDataId::Func(_)));
    let data = |name: &str| matches!(m.get_name(name), Some(FuncOrDataId::Data(_)));
    let flags = u64::from(func("vesper_box_free"))
        | u64::from(data("vesper_seq_live")) << 1
        | u64::from(data("vesper_box_live")) << 2;
    let bytes = export_words::binding_input(interface, &Default::default());
    let mut words = vec![flags];
    words.extend(
        bytes
            .chunks(8)
            .map(|c| c.iter().rev().fold(0u64, |w, b| w << 8 | u64::from(*b))),
    );
    let (answer, crossing) =
        vesper_cranelift_sys::lend_module(m, 0, &words, codegen_export_binding::export);
    if let Some(e) = crossing.error {
        return Some(Err(format!("codegen (export glue): {e}")));
    }
    if crossing.unreturned {
        return Some(Err("codegen (export glue) kept a lent handle".into()));
    }
    if answer == 1 {
        EXPORT_CALLS.fetch_add(1, Relaxed);
        Some(Ok(()))
    } else {
        EXPORT_DECLINED.fetch_add(1, Relaxed);
        None
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("export", "no flip-gate configuration"),
    open("plan", "no flip-gate configuration"),
    open("interface_json", "no flip-gate configuration"),
];

pub(super) fn register(r: &mut Registry) {
    if on("export") {
        r.codegen.export = Some(vesper_export);
    }
    if on("plan") {
        r.codegen.plan = Some(vesper_plan);
    }
    if on("interface_json") {
        r.codegen.interface_json = Some(vesper_interface_json);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted the export glue of {} libraries from Vesper ({} declined)",
        EXPORT_CALLS.load(Relaxed),
        EXPORT_DECLINED.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: codegen planned {} library interfaces from Vesper",
        PLAN_CALLS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: codegen wrote {} interface files from Vesper",
        JSON_CALLS.load(Relaxed),
    );
}
