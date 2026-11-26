//! The input of `@vesper/run` (packages/vesper-run, the `vesper run`
//! evaluator in Vesper; claim #2053) and its result, shared by
//! tests/self_host_run.rs and the hybrid build's `run` swap
//! (crates/vesper-hybrid/src/swap/run.rs).
//!
//! [`program`] builds the program `Runtime::run` evaluates (the drop
//! lowering, then the IR optimizer when `optimize`); [`run_input`] writes
//! check-data's names, types and world, the program section of
//! `ir_words::program_input`, then the fuel and the entry's name index;
//! [`decode`] reads the status and text back.
#![allow(dead_code)]

use super::ir_words::{Tables, encodable};
use vesper::check::World;
use vesper::ir::Program;

/// The package's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The value's `Display` text: `Runtime::run`'s `Ok`.
    Value(String),
    /// `Runtime::run`'s `Err`.
    Error(String),
    /// The machine does not run this program yet; the Rust evaluator
    /// must. The text says what it lacks.
    Declined(String),
}

/// The world-level features the Vesper machine does not model yet, which
/// the Rust evaluator reads from the world rather than the program: the
/// atomic and scoped-worker preludes (`atomics::intercepted`), `dyn`
/// objects (vtable stubs and hidden parameters), element views, source
/// regions and the generated `Pinned[T]`. `None` when the program may run.
pub fn world_declines(world: &World) -> Option<&'static str> {
    if world.atomic_prelude || world.workers_prelude {
        Some("the atomic prelude")
    } else if !world.dyn_objects.stubs.is_empty() || !world.dyn_objects.params.is_empty() {
        Some("dyn objects")
    } else if !world.element_views.is_empty() {
        Some("element views")
    } else if vesper::regions::active(world) {
        Some("source regions")
    } else if world.adts.contains_key("Pinned") {
        Some("pinned buffers")
    } else {
        None
    }
}

/// The program `Runtime::run` evaluates (`Runtime::new` and
/// `run_execution`), or the optimizer's error, which `run` returns.
pub fn program(world: &World, optimize: bool) -> Result<Program, String> {
    let mut program = vesper::ffi_lowering::drops::lower_world(world);
    if optimize {
        vesper::ir_opt::optimize(world, &mut program)?;
    }
    Ok(program)
}

/// The package's input, or `None` when the world or the program has a
/// type the check-data format cannot encode.
pub fn run_input(world: &World, program: &Program, entry: &str, fuel: u64) -> Option<Vec<u8>> {
    if !encodable(world, program) {
        return None;
    }
    let mut t = Tables::of(program);
    t.label_names();
    let entry = t.enc.name(entry);
    let mut tail = t.section();
    let world_words = t.enc.world(world);
    tail.push(fuel);
    tail.push(entry);
    Some(t.enc.finish(&world_words, &tail))
}

/// The words `run_words` returns: a status, then the text.
pub fn decode(words: &[u64]) -> Answer {
    let status = words.first().copied().unwrap_or(2);
    let text = words.get(1..).map(text).unwrap_or_default();
    match status {
        0 => Answer::Value(text),
        1 => Answer::Error(text),
        _ => Answer::Declined(text),
    }
}

/// `put_text_words`'s text: a byte length, then the bytes eight per word.
fn text(words: &[u64]) -> String {
    let len = words.first().copied().unwrap_or(0) as usize;
    let mut bytes = Vec::with_capacity(len + 8);
    for w in &words[1..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    String::from_utf8_lossy(&bytes).into_owned()
}
