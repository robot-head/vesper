//! Self-hosting SH3 lane 13 (#1252): `@vesper/place-ir-lower` (the Vesper
//! port of `ir::lower_world` and `thread_tokens`) against the Rust
//! lowering, over every corpus world.
//!
//! The port lowers each world function it covers; it leaves out a
//! function that reaches a construct it does not lower yet (closures,
//! handlers, sequence, tiled, vector and `Box` operations). Every function
//! it lowers must be `Debug`-equal to Rust's `FunctionIr`, and must pass
//! the IR check as Rust's does.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_lower_words.rs"]
mod ir_lower_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use vesper::ast::{Expr, ExprKind, Statement};
use vesper::check::World;
use vesper::ir::Program;

/// What a world function's body holds: 0 neither of the below, 1 a closure
/// or handler (lowered, with nested functions), 2 a routed sequence, tiled,
/// vector or `Box` operation anywhere (part 3; the function, or the nested
/// function holding it, is skipped).
fn skip_reason(e: &Expr, routed: &BTreeSet<(usize, usize)>) -> u8 {
    let mut reason = 0;
    let mut visit = |x: &Expr| {
        if matches!(x.kind, ExprKind::Call { .. } | ExprKind::Index(..))
            && routed.contains(&(x.span.start, x.span.end))
        {
            reason = 2;
        } else if matches!(x.kind, ExprKind::Closure { .. } | ExprKind::Handle { .. })
            && reason == 0
        {
            reason = 1;
        }
    };
    walk(e, &mut visit);
    reason
}

fn walk(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Tuple(es) => es.iter().for_each(|x| walk(x, f)),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, x)| walk(x, f)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => walk(value, f),
                    Statement::Expr(x) => walk(x, f),
                }
            }
            if let Some(t) = tail {
                walk(t, f);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            walk(callee, f);
            args.iter().for_each(|a| walk(&a.expr, f));
        }
        ExprKind::Field(x, _) | ExprKind::Try(x) | ExprKind::Unary(_, x) | ExprKind::Loop(x) => {
            walk(x, f)
        }
        ExprKind::Index(a, b)
        | ExprKind::Binary(_, a, b)
        | ExprKind::Assign(_, a, b)
        | ExprKind::Range {
            start: a, end: b, ..
        } => {
            walk(a, f);
            walk(b, f);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            walk(condition, f);
            walk(then, f);
            if let Some(o) = otherwise {
                walk(o, f);
            }
        }
        ExprKind::Match { value, arms } => {
            walk(value, f);
            arms.iter().for_each(|(_, x)| walk(x, f));
        }
        ExprKind::For { iter, body, .. } => {
            walk(iter, f);
            walk(body, f);
        }
        ExprKind::Break(Some(x)) | ExprKind::Return(Some(x)) => walk(x, f),
        _ => {}
    }
}

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-place-ir-lower"))
}

/// Every `n`-th corpus world (`IR_LOWER_STEP`, default 2): every world
/// takes about 11 minutes in a debug build, close to Bazel's `long`
/// timeout, so the default runs half; `IR_LOWER_STEP=1` runs every world
/// (the report's numbers).
fn step() -> usize {
    std::env::var("IR_LOWER_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
}

/// A corpus world with Rust's lowering.
struct Lowered {
    name: String,
    world: &'static World,
    program: Program,
}

fn lowered() -> &'static [Lowered] {
    static S: OnceLock<Vec<Lowered>> = OnceLock::new();
    S.get_or_init(|| {
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut out = vec![];
        for c in corpus().iter().step_by(step()) {
            let w = &c.world;
            if let Ok(program) = std::panic::catch_unwind(|| vesper::ir::lower_world(w)) {
                out.push(Lowered {
                    name: c.name.clone(),
                    world: w,
                    program,
                });
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

/// The Vesper lowering of `w`: the program and (functions lowered,
/// functions skipped).
fn vesper_lower(entry: &Entry, w: &World, reference: &Program) -> Option<(Program, u64, u64)> {
    let (input, pool) = ir_lower_words::lower_input(w, reference)?;
    let words = entry.words(&input);
    let n = words.len();
    let (lowered, skipped) = (words[n - 2], words[n - 1]);
    let program = ir_words::decode_program(&words, &pool);
    Some((program, lowered, skipped))
}

#[test]
fn vesper_lowering_matches_rust() {
    let entry = package().entry("ir_lower_words", true);
    let (mut bad, mut worlds, mut skipped_worlds) = (0, 0, 0);
    let (mut lowered_fns, mut skipped_fns, mut instructions) = (0u64, 0u64, 0usize);
    let mut ir_check_differs = 0;
    let mut reasons = [0u64; 3];
    let (mut rust_nested, mut vesper_nested) = (0u64, 0u64);
    for c in lowered() {
        let routed = ir_lower_words::foreign(&c.program);
        for f in c.world.functions.values() {
            if let Some(b) = &f.body {
                reasons[skip_reason(b, &routed) as usize] += 1;
            }
        }
        let Some((program, lowered_n, skipped_n)) = vesper_lower(&entry, c.world, &c.program)
        else {
            skipped_worlds += 1;
            continue;
        };
        worlds += 1;
        rust_nested += c
            .program
            .functions
            .keys()
            .filter(|n| n.starts_with('$'))
            .count() as u64;
        vesper_nested += program
            .functions
            .keys()
            .filter(|n| n.starts_with('$'))
            .count() as u64;
        lowered_fns += lowered_n;
        skipped_fns += skipped_n;
        assert_eq!(
            lowered_n as usize,
            program.functions.len(),
            "{}: lowered count",
            c.name
        );
        let mut compared = Program::default();
        for (name, f) in &program.functions {
            let Some(r) = c.program.functions.get(name) else {
                bad += 1;
                eprintln!("{}: Vesper lowered {name}, which Rust did not", c.name);
                continue;
            };
            instructions += r.blocks.iter().map(|b| b.instructions.len()).sum::<usize>();
            let (rs, vs) = (format!("{r:#?}"), format!("{f:#?}"));
            if let Some(d) = first_difference(&rs, &vs) {
                bad += 1;
                if bad <= 8 {
                    eprintln!("{}: function {name} differs at {d}", c.name);
                }
            }
            compared.functions.insert(name.clone(), r.clone());
        }
        // The IR check sees the same program both ways: Rust's functions
        // the port lowered, then the port's.
        let rust_check = vesper::ir_check::validate(c.world, &compared);
        let vesper_check = vesper::ir_check::validate(c.world, &program);
        if format!("{rust_check:?}") != format!("{vesper_check:?}") {
            ir_check_differs += 1;
        }
    }
    eprintln!(
        "ir lower: {bad} functions differ; {worlds} worlds ({skipped_worlds} not encodable), \
         {lowered_fns} functions lowered, {skipped_fns} skipped, {instructions} instructions; \
         IR check differs on {ir_check_differs} worlds; world functions: {} plain, {} with \
         closures or handlers, {} with payload operations; closure and handler functions: \
         {vesper_nested} of Rust's {rust_nested} lowered",
        reasons[0], reasons[1], reasons[2]
    );
    assert!(worlds > 50, "only {worlds} worlds");
    assert!(
        lowered_fns > 1000,
        "coverage floor: only {lowered_fns} functions lowered"
    );
    assert_eq!(bad, 0);
    assert_eq!(ir_check_differs, 0);
}

/// A corpus world cut down to the functions the port lowers (the others
/// keep their signatures and lose their bodies), with its input.
struct Covered {
    world: World,
    input: Vec<u8>,
    pool: ir_words::Pool,
}

fn covered() -> Vec<Covered> {
    let entry = package().entry("ir_lower_words", true);
    let mut out = vec![];
    for c in lowered() {
        let Some((program, _, _)) = vesper_lower(&entry, c.world, &c.program) else {
            continue;
        };
        let mut world = c.world.clone();
        for (name, f) in world.functions.iter_mut() {
            if !program.functions.contains_key(name) {
                f.body = None;
            }
        }
        let reference = vesper::ir::lower_world(&world);
        if let Some((input, pool)) = ir_lower_words::lower_input(&world, &reference) {
            out.push(Covered { world, input, pool });
        }
    }
    out
}

#[inline(never)]
fn run_rust(cs: &[Covered]) -> usize {
    cs.iter()
        .map(|c| vesper::ir::lower_world(&c.world).functions.len())
        .sum()
}

#[inline(never)]
fn run_vesper(entry: &Entry, cs: &[Covered]) -> usize {
    cs.iter().map(|c| entry.words(&c.input).len()).sum()
}

#[inline(never)]
fn run_vesper_read(entry: &Entry, cs: &[Covered]) -> usize {
    cs.iter().map(|c| entry.words(&c.input).len()).sum()
}

/// Lowering alone and with its encoding, over the covered functions of
/// every corpus world: Rust's `lower_world` against the Vesper entry
/// (decoding its input, lowering, writing words), the Vesper read alone,
/// and the Rust encode and decode around it. ABBA, best of six.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn ir_lower_speed() {
    if !speed() {
        return;
    }
    let cs = covered();
    let full = package().entry("ir_lower_words", true);
    let read = package().entry("ir_lower_read", true);
    let fns: usize = cs.iter().map(|c| c.world.functions.len()).sum();
    let (r, v) = abba(
        "lowering with Vesper decode and write",
        &mut || {
            time(|| {
                std::hint::black_box(run_rust(&cs));
            })
        },
        &mut || {
            time(|| {
                std::hint::black_box(run_vesper(&full, &cs));
            })
        },
    );
    let (_, vr) = abba(
        "Vesper read alone",
        &mut || {
            time(|| {
                std::hint::black_box(run_rust(&cs));
            })
        },
        &mut || {
            time(|| {
                std::hint::black_box(run_vesper_read(&read, &cs));
            })
        },
    );
    let encode = (0..6)
        .map(|_| {
            time(|| {
                for c in &cs {
                    let reference = vesper::ir::lower_world(&c.world);
                    std::hint::black_box(ir_lower_words::lower_input(&c.world, &reference));
                }
            })
        })
        .min()
        .unwrap()
        .saturating_sub(r);
    let outputs: Vec<Vec<u64>> = cs.iter().map(|c| full.words(&c.input)).collect();
    let decode = (0..6)
        .map(|_| {
            time(|| {
                for (o, c) in outputs.iter().zip(&cs) {
                    std::hint::black_box(ir_words::decode_program(o, &c.pool));
                }
            })
        })
        .min()
        .unwrap();
    let alone = v.saturating_sub(vr);
    eprintln!(
        "ir lower speed over {} worlds ({fns} functions): Rust lower_world {r:?}; Vesper entry \
         {v:?} ({:.2}x); Vesper read {vr:?}; Vesper lowering alone {alone:?} ({:.2}x); \
         encode {encode:?}; decode {decode:?}; swap total {:?} ({:.2}x)",
        cs.len(),
        v.as_secs_f64() / r.as_secs_f64(),
        alone.as_secs_f64() / r.as_secs_f64(),
        encode + v + decode,
        (encode + v + decode).as_secs_f64() / r.as_secs_f64(),
    );
}

/// One side over every covered world, for callgrind
/// (`--toggle-collect='*self_host_ir_lower::run_*'`): `IR_LOWER_SIDE` is
/// `rust`, `vesper` or `vesper-read`.
#[test]
#[ignore = "callgrind"]
fn callgrind_one_side() {
    let Ok(side) = std::env::var("IR_LOWER_SIDE") else {
        return;
    };
    let cs = covered();
    let full = package().entry("ir_lower_words", true);
    let read = package().entry("ir_lower_read", true);
    let n = match side.as_str() {
        "rust" => run_rust(&cs),
        "vesper" => run_vesper(&full, &cs),
        "vesper-read" => run_vesper_read(&read, &cs),
        other => panic!("unknown side {other}"),
    };
    eprintln!("{side}: {n}");
}
