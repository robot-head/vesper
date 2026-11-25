// The wasm backend (SH4 lane 15, #1289) and the component wrapping
// (#1334), from `modules/wasm_backend`, through the `wasm` and
// `component` hooks.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/wasm_backend.rs"]
mod wasm_backend_binding;

// The wasm backend swap's input encoder (#1289): the Place IR words and the
// wasm section, shared with tests/self_host_wasm_backend.rs.
#[path = "../../../../tests/common/wasm_words.rs"]
mod wasm_words;

// The word encoders read the shared ones through `super::`.
use super::{Registry, Swap, check_words, ir_words, on, open, pass};

/// Programs the Vesper wasm backend compiled, and left to Rust.
static WASM_CALLS: AtomicU64 = AtomicU64::new(0);
static WASM_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Components `@vesper/wit-component` wrapped, and left to wit-component.
static WRAP_CALLS: AtomicU64 = AtomicU64::new(0);
static WRAP_DECLINED: AtomicU64 = AtomicU64::new(0);
/// `vesper build --target wasm32-wasip2` runs the Vesper driver encoded for
/// building in Vesper (#1895), and how many of those fell back to the Rust
/// pass.
pub(crate) static DRIVER_WASM_INPUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static DRIVER_WASM_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// The `wasm` hook of `vesper::codegen_swap` from `modules/wasm_backend`:
/// `@vesper/wasm-backend` writes the core module and the WIT, and
/// `@vesper/wit-component` wraps them into the component (#1334), with
/// Rust's wit-component where it declines (host imports). Errors (so the
/// messages stay Rust's) and constructs not ported yet (host capabilities,
/// CPS clauses, stack switching) decline to the Rust backend.
fn vesper_wasm(
    world: &vesper::check::World,
    entry: &str,
    options: &vesper::wasm_backend::WasmOptions,
) -> Option<Result<vesper::wasm_backend::WasmArtifact, vesper::wasm_backend::WasmError>> {
    match compile_wasm(world, entry, options) {
        Some(artifact) => {
            WASM_CALLS.fetch_add(1, Relaxed);
            Some(Ok(artifact))
        }
        None => {
            WASM_DECLINED.fetch_add(1, Relaxed);
            None
        }
    }
}

/// `wasm_words::compile_with` over the binding, with the component from
/// Vesper.
fn compile_wasm(
    w: &vesper::check::World,
    entry: &str,
    o: &vesper::wasm_backend::WasmOptions,
) -> Option<vesper::wasm_backend::WasmArtifact> {
    if !w.functions.contains_key(entry) {
        return None;
    }
    let program = wasm_words::backend_program(w, o)?;
    let input = wasm_words::wasm_input(w, &program, entry, o)?;
    let wasm_words::Outcome::Module {
        core,
        wit,
        functions,
        handlers,
    } = wasm_words::outcome(&wasm_backend_binding::compile_words(&input))
    else {
        return None;
    };
    match wrap_component(&core, &wit, o.world) {
        Some(component) => {
            WRAP_CALLS.fetch_add(1, Relaxed);
            Some(artifact(
                w, entry, o, core, component, wit, functions, &handlers,
            ))
        }
        None => {
            WRAP_DECLINED.fetch_add(1, Relaxed);
            Some(wasm_words::artifact(
                w, entry, o, core, wit, functions, &handlers,
            ))
        }
    }
}

/// `wasm_words::artifact` with the component given: the artifact of a
/// module without host capabilities.
#[allow(clippy::too_many_arguments)]
fn artifact(
    w: &vesper::check::World,
    entry: &str,
    o: &vesper::wasm_backend::WasmOptions,
    core: Vec<u8>,
    component: Vec<u8>,
    wit: String,
    functions: usize,
    handlers: &[wasm_words::PortHandler],
) -> vesper::wasm_backend::WasmArtifact {
    use vesper::wasm_backend::{ENTRY_INTERFACE, WasmWorld};
    let f = &w.functions[entry];
    vesper::wasm_backend::WasmArtifact {
        core,
        component,
        wit,
        world: o.world,
        export: match o.world {
            WasmWorld::Command => format!("{ENTRY_INTERFACE}#main"),
            WasmWorld::NexusPlugin => "run".to_owned(),
        },
        params: f.params.iter().map(|p| p.ty.clone()).collect(),
        result: vesper::numeric::default_type(&f.result),
        stack_switching: false,
        imports: vec![],
        handlers: wasm_words::handler_lowerings(w, handlers),
        functions,
    }
}

/// The `component` hook of `vesper::codegen_swap` (#1334): every component
/// the Rust wasm backend wraps (programs the Vesper backend leaves to it,
/// host imports among them) is written by `@vesper/wit-component`, with
/// wit-component where the port declines.
fn vesper_component(
    core: &[u8],
    wit: &str,
    world: vesper::wasm_backend::WasmWorld,
) -> Option<Vec<u8>> {
    let component = wrap_component(core, wit, world);
    if component.is_some() {
        WRAP_CALLS.fetch_add(1, Relaxed);
    } else {
        WRAP_DECLINED.fetch_add(1, Relaxed);
    }
    component
}

/// `@vesper/wit-component`'s component for a core module and its WIT, or
/// `None` where it declines (modules/wasm_backend `wrap_words`).
fn wrap_component(
    core: &[u8],
    wit: &str,
    world: vesper::wasm_backend::WasmWorld,
) -> Option<Vec<u8>> {
    let mut input = Vec::with_capacity(5 + wit.len() + core.len());
    input.push(match world {
        vesper::wasm_backend::WasmWorld::Command => 0,
        vesper::wasm_backend::WasmWorld::NexusPlugin => 1,
    });
    input.extend_from_slice(&u32::try_from(wit.len()).ok()?.to_le_bytes());
    input.extend_from_slice(wit.as_bytes());
    input.extend_from_slice(core);
    let words = wasm_backend_binding::wrap_words(&input);
    if words.first() != Some(&0) {
        return None;
    }
    let len = usize::try_from(*words.get(1)?).ok()?;
    let mut bytes = Vec::with_capacity(len + 8);
    for x in &words[2..] {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    if bytes.len() < len {
        return None;
    }
    bytes.truncate(len);
    Some(bytes)
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
/// The `wasm` swap's input for `vesper build --target wasm32-wasip2`, which
/// the Vesper driver compiles and wraps itself (#1895): `None` when the
/// swap is off or the world does not encode (the Rust step builds it).
/// `VESPER_HYBRID_DRIVER_WASM=0` keeps the build on the Rust step (the
/// differential's baseline).
pub(crate) fn wasm_build_input(
    w: &vesper::check::World,
    entry: &str,
    o: &vesper::wasm_backend::WasmOptions,
) -> Option<Vec<u8>> {
    vesper::codegen_swap::installed().wasm?;
    if super::env_off("VESPER_HYBRID_DRIVER_WASM") {
        return None;
    }
    if !w.functions.contains_key(entry) {
        return None;
    }
    let program = wasm_words::backend_program(w, o)?;
    wasm_words::wasm_input(w, &program, entry, o)
}

pub(super) const SWAPS: &[Swap] = &[
    open("wasm", "0 diffs; no flip-gate configuration"),
    pass("component", "wit-component", "#1334"),
];

pub(super) fn register(r: &mut Registry) {
    if on("wasm") {
        r.codegen.wasm = Some(vesper_wasm);
    }
    if on("component") {
        r.codegen.component = Some(vesper_component);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: wasm backend compiled {} programs from Vesper ({} declined)",
        WASM_CALLS.load(Relaxed),
        WASM_DECLINED.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: driver built {} wasm components in Vesper ({} fell back to Rust)",
        DRIVER_WASM_INPUTS.load(Relaxed) - DRIVER_WASM_FALLBACKS.load(Relaxed),
        DRIVER_WASM_FALLBACKS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: wit-component wrapped {} components from Vesper ({} declined)",
        WRAP_CALLS.load(Relaxed),
        WRAP_DECLINED.load(Relaxed),
    );
}
