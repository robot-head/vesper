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
    let sites = sites(world, program, &mut t);
    let mut tail = t.section();
    let world_words = t.enc.world(world);
    tail.push(fuel);
    tail.push(entry);
    tail.extend(sites);
    Some(t.enc.finish(&world_words, &tail))
}

// Site kinds of the effect table.
const SITE_HANDLE: u64 = 1;
const SITE_EFFECT_CALL: u64 = 2;
const SITE_TRY: u64 = 3;

/// The effect table after the entry: what `Runtime` reads from the AST and
/// the world at a handler, an effect operation call or a `?`, which the
/// program section does not carry.
///
/// ```text
/// overflow-key+1  runtime-error-key+1      the primitive failures' Fail keys
/// n, then per site: instruction (global index), kind, then
///   handle:  effect label+1 instance-key+1 coerce static-key
///            free-count free-name... body-fn+1
///            ret-fn+1 ret-pattern
///            op-count (op clause-fn+1 pattern-count pattern...)...
///   effect call: effect op instance-key+1
///   try:     instance-key+1
/// ```
///
/// A pattern word is 0 for `_`, name + 1 for a binding, or `u64::MAX` for
/// any other pattern (the machine declines it). A function word is 0 when
/// the body was not lowered (it would run as an AST expression).
fn sites(world: &World, program: &Program, t: &mut Tables) -> Vec<u64> {
    use vesper::ast::{ExprKind, PatternKind};
    use vesper::ir::Op;
    use vesper::numeric::instance_key;
    let named = |n: &str| vesper::ast::Type::Named(n.into(), vec![]);
    let mut out = vec![];
    for ty in ["ArithOverflow", "RuntimeError"] {
        let key = instance_key("Fail", &[named(ty)]).map(|k| t.enc.name(&k) + 1);
        out.push(key.unwrap_or(0));
    }
    let mut rows = vec![];
    let mut global = 0u64;
    for f in program.functions.values() {
        for b in &f.blocks {
            for i in &b.instructions {
                let at = global;
                global += 1;
                match &i.op {
                    Op::Handle { expr, .. } => {
                        let ExprKind::Handle {
                            effect,
                            state,
                            body,
                            ret,
                            ops,
                        } = &expr.kind
                        else {
                            continue;
                        };
                        let mut row = vec![at, SITE_HANDLE];
                        row.push(t.enc.name(&effect.name));
                        row.push(t.enc.opt_name(&effect.label));
                        row.push(
                            instance_key(&effect.name, &effect.args)
                                .map_or(0, |k| t.enc.name(&k) + 1),
                        );
                        // `Control::Eval` pushes a `Frame::Coerce` when the
                        // expression has a type (one more step); a state
                        // handler also evaluates its initializer.
                        let coerce = world.types.contains_key(&(expr.span.start, expr.span.end));
                        row.push(coerce as u64 | if state.is_some() { 2 } else { 0 });
                        row.push(t.enc.name(&format!("$static:$cap{}", expr.span.start)));
                        let mut free = vesper::check::clause_free_names(ret);
                        for op in ops {
                            free.extend(vesper::check::clause_free_names(op));
                        }
                        row.push(free.len() as u64);
                        for n in &free {
                            row.push(t.enc.name(n));
                        }
                        let mut lowered = |kind: &str, e: &vesper::ast::Expr| {
                            let name = vesper::ir::nested_name(kind, &e.span);
                            if program.functions.contains_key(&name) {
                                t.enc.name(&name) + 1
                            } else {
                                0
                            }
                        };
                        row.push(lowered("handler_body", body));
                        row.push(lowered("handler_clause", &ret.body));
                        let mut clause_fns = vec![];
                        for op in ops {
                            clause_fns.push(lowered("handler_clause", &op.body));
                        }
                        let pattern = |t: &mut Tables, p: &vesper::ast::Pattern| match &p.kind {
                            PatternKind::Wild => 0,
                            PatternKind::Bind(n) => t.enc.name(n) + 1,
                            _ => u64::MAX,
                        };
                        row.push(ret.pats.first().map_or(u64::MAX, |p| pattern(t, p)));
                        row.push(ops.len() as u64);
                        for (op, f) in ops.iter().zip(clause_fns) {
                            let name = t.enc.name(&op.name);
                            row.extend([name, f, op.pats.len() as u64]);
                            for p in &op.pats {
                                row.push(pattern(t, p));
                            }
                        }
                        rows.push(row);
                    }
                    Op::Call {
                        callee,
                        types,
                        capability,
                        ..
                    } if !world.functions.contains_key(callee) => {
                        let Some((effect, op)) = callee.split_once("::") else {
                            continue;
                        };
                        if capability.is_some() {
                            // A labelled operation: its `$label:` key.
                            continue;
                        }
                        let key = instance_key(effect, types).map_or(0, |k| t.enc.name(&k) + 1);
                        let effect = t.enc.name(effect);
                        let op = t.enc.name(op);
                        rows.push(vec![at, SITE_EFFECT_CALL, effect, op, key]);
                    }
                    Op::Try(v) => {
                        let error = f.values.get(v).and_then(|ty| match ty {
                            vesper::ast::Type::Named(n, args)
                                if n == "Result" && args.len() == 2 =>
                            {
                                Some(args[1].clone())
                            }
                            _ => None,
                        });
                        let key = instance_key("Fail", &error.into_iter().collect::<Vec<_>>())
                            .map_or(0, |k| t.enc.name(&k) + 1);
                        rows.push(vec![at, SITE_TRY, key]);
                    }
                    _ => {}
                }
            }
        }
    }
    out.push(rows.len() as u64);
    for r in rows {
        out.extend(r);
    }
    out
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
