//! The corpus programs at the bounds-hoisting boundary of the native
//! optimize pipeline, shared by the bounds-hoisting tests (#1251).
use super::check_harness::{Case, corpus};
use std::sync::OnceLock;
use vesper::ir::Program;

/// A corpus world's program at the bounds-hoisting boundary of the native
/// optimize pipeline (`native::compile_with` before `bounds_hoist::run`).
fn at_hoist(world: &vesper::check::World) -> Option<Program> {
    let mut program = vesper::ffi_lowering::drops::lower_world(world);
    vesper::ir_opt::optimize(world, &mut program).ok()?;
    vesper::ir_inline::inline(world, &mut program, &vesper::ir_inline::Budget::default());
    vesper::fip::elaborate_non_fip_drops(world, &mut program);
    Some(program)
}

thread_local! {
    /// Set while this thread lowers a world: its panics are expected.
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs, once, a panic hook that is silent on a thread lowering a world
/// and the previous hook everywhere else, so parallel tests never race on
/// swapping hooks.
fn quiet_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.get() {
                previous(info);
            }
        }));
    });
}

/// The corpus programs at the hoisting boundary, with their worlds, every
/// `stride`th world; each world is lowered once, on first use.
pub fn programs(stride: usize) -> Vec<(&'static Case, &'static Program)> {
    static P: OnceLock<Vec<OnceLock<Option<Program>>>> = OnceLock::new();
    let slots = P.get_or_init(|| corpus().iter().map(|_| OnceLock::new()).collect());
    let mut out = vec![];
    for (c, slot) in corpus().iter().zip(slots).step_by(stride) {
        let p = slot.get_or_init(|| {
            quiet_panics();
            QUIET.set(true);
            let p = std::panic::catch_unwind(|| at_hoist(&c.world))
                .ok()
                .flatten();
            QUIET.set(false);
            p
        });
        if let Some(p) = p {
            out.push((c, p));
        }
    }
    out
}
