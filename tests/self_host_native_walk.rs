//! Rust-exit gap wave 1, lane 11 (#1509), parts 1-2: `@vesper/native-walk`
//! against the Rust walk of `src/native.rs`, both at the decision boundary
//! (`vesper_decisions_match_rust`) and end-to-end through the
//! `register_locals` codegen swap hook (`the_native_walk_swap_matches_rust`).
//!
//! - **Callee facts** (`@vesper/codegen-calls`): whether each callee's effect
//!   row is empty and whether it is specialized into its callers
//!   (`specialized_function`).
//! - **Register plans**: cyclic blocks (`cyclic_blocks`) and scalar/parameter
//!   locals kept in Cranelift SSA variables (`register_locals`, including
//!   non-empty `generic_vars`, `nested` inlined graphs and `Op::Handle`
//!   captures), proved to produce 0 CLIF and 0 object diffs on real compiles.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::ast::{Pass, Type};
use vesper::check::{self, World};
use vesper::codegen_swap::{self, CodegenHooks};
use vesper::ir::{FunctionIr, Op, Program};
use vesper::native::{cyclic_blocks, register_locals, specialized_function};
use vesper::parser;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-walk"))
}

/// Every `n`-th corpus world (`NATIVE_WALK_STEP`, default 4);
/// `NATIVE_WALK_STEP=1` runs every world (the report's numbers), and
/// `NATIVE_WALK_WORLD=a,b` only the worlds whose names contain one of them.
fn step() -> usize {
    std::env::var("NATIVE_WALK_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// One program the native path compiles.
pub struct Stage {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// Every corpus world's program as `native` lowers it (the stages of
/// tests/self_host_codegen_scalar.rs). A pass that refuses a program
/// drops it.
pub fn stages() -> &'static [Stage] {
    static S: OnceLock<Vec<Stage>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = vec![];
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let only: Option<Vec<String>> = std::env::var("NATIVE_WALK_WORLD")
            .ok()
            .map(|s| s.split(',').map(str::to_owned).collect());
        for c in corpus()
            .iter()
            .step_by(if only.is_some() { 1 } else { step() })
        {
            if only
                .as_ref()
                .is_some_and(|o| !o.iter().any(|n| c.name.contains(n.as_str())))
            {
                continue;
            }
            let w = &c.world;
            let r = std::panic::catch_unwind(|| {
                let mut v = vec![];
                let lowered = vesper::ffi_lowering::drops::lower_world(w);
                let mut p = lowered.clone();
                if vesper::ir_opt::optimize(w, &mut p).is_ok() {
                    vesper::ir_inline::inline(w, &mut p, &vesper::ir_inline::Budget::default());
                    vesper::fip::elaborate_non_fip_drops(w, &mut p);
                    vesper::bounds_hoist::run(&mut p);
                    v.push(("opt", p));
                }
                let mut q = lowered;
                if vesper::ir_opt::fip_reuse_pass(w, &mut q).is_ok() {
                    vesper::fip::elaborate_non_fip_drops(w, &mut q);
                    v.push(("unopt", q));
                }
                v
            });
            if let Ok(v) = r {
                for (stage, program) in v {
                    out.push(Stage {
                        name: format!("{}@{stage}", c.name),
                        world: w,
                        program,
                    });
                }
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

// ------------------------------------------------------------ the input

/// `ir_words::program_input` and this package's two sections: the
/// signatures of `world.functions` and each graph's handler free names.
fn input(w: &World, p: &Program) -> Option<Vec<u8>> {
    if !ir_words::encodable(w, p) {
        return None;
    }
    let sig_types = w.functions.values().flat_map(|f| {
        std::iter::once(f.result.clone())
            .chain(std::iter::once(Type::Row(f.effects.clone())))
            .chain(f.params.iter().map(|p| p.ty.clone()))
    });
    if !sig_types.into_iter().all(|t| ir_words::literal_fits(&t)) {
        return None;
    }
    let mut t = ir_words::Tables::of(p);
    t.label_names();
    let mut tail = t.section();
    tail.push(w.functions.len() as u64);
    for (name, f) in &w.functions {
        tail.push(t.enc.name(name));
        tail.push(f.generics.len() as u64);
        tail.push(t.enc.ty(&f.result));
        tail.push(t.enc.ty(&Type::Row(f.effects.clone())));
        tail.push(f.params.len() as u64);
        for q in &f.params {
            tail.push(t.enc.name(&q.name));
            tail.push(check_words::pass_word(q.pass));
            tail.push(t.enc.ty(&q.ty));
        }
    }
    for f in p.functions.values() {
        let names = handler_names(f);
        tail.push(names.len() as u64);
        for n in &names {
            tail.push(t.enc.name(n));
        }
    }
    let world = t.enc.world(w);
    Some(t.enc.finish(&world, &tail))
}

/// The free names of every `Op::Handle` body of `f`.
fn handler_names(f: &FunctionIr) -> BTreeSet<String> {
    f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| match &i.op {
            Op::Handle { expr, .. } => Some(vesper::check::free_names(expr)),
            _ => None,
        })
        .flatten()
        .collect()
}

// ------------------------------------------------------------ swap hook

fn register_locals_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    // Compiled with hooks disabled so initializing the entry never recurses.
    E.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            package().entry("native_walk_register_locals", true)
        })
    })
}

/// Encodes one `ir_graph_at` graph (`world_lean`, 1-function `Program`,
/// referenced callee signatures, `generics`, `nested`, handler free names)
/// for `@vesper/native-walk::native_walk_register_locals`.
fn function_input(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
    nested: bool,
) -> Option<Vec<u8>> {
    let adts_fit = world.adts.values().all(|a| {
        a.variants
            .iter()
            .all(|v| v.fields.iter().all(|fld| ir_words::literal_fits(&fld.ty)))
    });
    if !adts_fit || !ir_words::function_fits(f) || !generics.values().all(ir_words::literal_fits) {
        return None;
    }
    let mut callees: BTreeSet<&str> = BTreeSet::new();
    for b in &f.blocks {
        for i in &b.instructions {
            if let Op::Call { callee, .. } = &i.op
                && world.functions.contains_key(callee)
            {
                callees.insert(callee.as_str());
            }
        }
    }
    for callee in &callees {
        let g = &world.functions[*callee];
        let sig_fits = ir_words::literal_fits(&g.result)
            && ir_words::literal_fits(&Type::Row(g.effects.clone()))
            && g.params.iter().all(|p| ir_words::literal_fits(&p.ty));
        if !sig_fits {
            return None;
        }
    }
    let program = Program {
        functions: BTreeMap::from([(f.name.clone(), f.clone())]),
    };
    let mut t = ir_words::Tables::of(&program);
    t.label_names();
    let mut tail = t.section();
    tail.push(callees.len() as u64);
    for callee in callees {
        let g = &world.functions[callee];
        tail.push(t.enc.name(callee));
        tail.push(g.generics.len() as u64);
        tail.push(t.enc.ty(&g.result));
        tail.push(t.enc.ty(&Type::Row(g.effects.clone())));
        tail.push(g.params.len() as u64);
        for q in &g.params {
            tail.push(t.enc.name(&q.name));
            tail.push(check_words::pass_word(q.pass));
            tail.push(t.enc.ty(&q.ty));
        }
    }
    tail.push(generics.len() as u64);
    for (k, v) in generics {
        tail.push(t.enc.name(k));
        tail.push(t.enc.ty(v));
    }
    tail.push(u64::from(nested));
    let names = handler_names(f);
    tail.push(names.len() as u64);
    for n in &names {
        tail.push(t.enc.name(n));
    }
    let world_words = t.enc.world_lean(world);
    Some(t.enc.finish(&world_words, &tail))
}

static SWAP_CALLS: AtomicU64 = AtomicU64::new(0);
static SWAP_DECLINED: AtomicU64 = AtomicU64::new(0);
static SWAP_GENERICS: AtomicU64 = AtomicU64::new(0);
static SWAP_NESTED: AtomicU64 = AtomicU64::new(0);
static SWAP_HANDLES: AtomicU64 = AtomicU64::new(0);
static SWAP_REGS: AtomicU64 = AtomicU64::new(0);

fn vesper_register_locals(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
    nested: bool,
) -> Option<BTreeSet<String>> {
    SWAP_CALLS.fetch_add(1, Ordering::Relaxed);
    let Some(bytes) = function_input(world, f, generics, nested) else {
        SWAP_DECLINED.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    if !generics.is_empty() {
        SWAP_GENERICS.fetch_add(1, Ordering::Relaxed);
    }
    if nested {
        SWAP_NESTED.fetch_add(1, Ordering::Relaxed);
    }
    if f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| matches!(i.op, Op::Handle { .. }))
    {
        SWAP_HANDLES.fetch_add(1, Ordering::Relaxed);
    }
    let out = register_locals_entry().text(&bytes);
    let regs: BTreeSet<String> = out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if !regs.is_empty() {
        SWAP_REGS.fetch_add(1, Ordering::Relaxed);
    }
    Some(regs)
}

/// The decisions text of one program from `src/native.rs` directly.
fn decisions(w: &World, p: &Program) -> String {
    let mut o = String::new();
    let bit = |b: bool| if b { '1' } else { '0' };
    let empty = BTreeMap::new();
    for (name, f) in &w.functions {
        let pure = f.effects.instances.is_empty() && f.effects.tail.is_none();
        let _ = writeln!(
            o,
            "sig {name} {}{}",
            bit(pure),
            bit(specialized_function(w, f))
        );
    }
    for (name, f) in &p.functions {
        let _ = writeln!(o, "fn {name}");
        o.push_str("cyclic");
        for b in cyclic_blocks(f) {
            let _ = write!(o, " {b}");
        }
        o.push('\n');
        for (label, nested) in [("registers", false), ("nested", true)] {
            let regs = register_locals(w, f, &empty, nested);
            o.push_str(label);
            for n in f.locals.keys().filter(|n| regs.contains(*n)) {
                let _ = write!(o, " {n}");
            }
            o.push('\n');
        }
    }
    o
}

// ------------------------------------------------------------ tests

#[test]
fn the_package_compiles() {
    let _ = package().entry("native_walk_decisions", true);
    let _ = register_locals_entry();
}

fn check_source(source: &str) -> World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

fn synthetic_swap_worlds() -> Vec<(&'static str, World)> {
    vec![
        (
            "generics_and_callbacks",
            check_source(
                r#"
copy struct Pair { a: i64, b: i64 }

fn bump(x: i64) -> i64 { wrapping_add(x, 1) }

fn swap_pair(in p: Pair) -> i64 { wrapping_add(p.a, p.b) }

fn apply_twice[T: mod copy](x: T, in cb: fn(T) -> T) -> T {
    let first = cb(x);
    cb(first)
}

fn keep_in[T](in a: T, in b: i64) -> i64 { b }

fn id_local[T: mod copy](x: T) -> T {
    let tmp = x;
    tmp
}

pub fn main() -> i64 {
    let s = apply_twice[i64](3, in bump);
    let p = id_local[Pair](Pair { a: 4, b: 5 });
    let k = id_local[i64](s);
    let r1 = keep_in[i64](in k, in 1);
    let r2 = keep_in[Pair](in p, in 2);
    wrapping_add(wrapping_add(r1, r2), swap_pair(in p))
}
"#,
            ),
        ),
        (
            "handles_and_nested_graphs",
            check_source(
                r#"
linear effect Ask {
    fn ask() -> i64;
}

fn helper(in seed: i64) -> i64 eff [Ask] {
    let mut acc = seed;
    let delta = Ask::ask();
    acc = wrapping_add(acc, delta);
    acc
}

pub fn main() -> i64 {
    let mut outer = 10;
    let inner = handle Ask in { helper(in outer) } with {
        return v => wrapping_add(v, outer),
        ask(k) => {
            outer = wrapping_add(outer, 2);
            k(outer)
        },
    };
    wrapping_add(outer, inner)
}
"#,
            ),
        ),
    ]
}

fn big_stack(f: impl FnOnce() + Send) {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// Installs `@vesper/native-walk::native_walk_register_locals` at the
/// `register_locals` swap hook and proves 0 CLIF and 0 object diffs on
/// synthetic edge-case programs and corpus worlds.
#[test]
fn the_native_walk_swap_matches_rust() {
    big_stack(|| {
        let _ = register_locals_entry();
        let hooks = CodegenHooks {
            register_locals: Some(vesper_register_locals),
            ..CodegenHooks::default()
        };
        let before = [
            &SWAP_CALLS,
            &SWAP_DECLINED,
            &SWAP_GENERICS,
            &SWAP_NESTED,
            &SWAP_HANDLES,
            &SWAP_REGS,
        ]
        .map(|c| c.load(Ordering::Relaxed));

        let mut bad = 0usize;
        let mut compiled = 0usize;

        let synthetic = synthetic_swap_worlds();
        let mut cases: Vec<(String, &World)> = synthetic
            .iter()
            .map(|(n, w)| ((*n).to_owned(), w))
            .collect();
        for c in corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key("main"))
            .step_by(step())
        {
            cases.push((c.name.clone(), &c.world));
        }

        for (name, world) in &cases {
            let rust = vesper::native::object_with_options(world, "main", None, true);
            let hooked = codegen_swap::with_hooks(hooks, || {
                vesper::native::object_with_options(world, "main", None, true)
            });
            match (rust, hooked) {
                (Ok((robj, rclif)), Ok((hobj, hclif))) => {
                    compiled += 1;
                    if rclif != hclif || robj != hobj {
                        bad += 1;
                        if bad <= 5 {
                            let diff = first_difference(&rclif, &hclif)
                                .unwrap_or_else(|| "object bytes differ".into());
                            eprintln!("native walk swap: {name} differs: {diff}");
                        }
                    }
                }
                (Err(re), Err(he)) => {
                    assert_eq!(re, he, "native walk swap error mismatch on {name}");
                }
                (r, h) => {
                    bad += 1;
                    eprintln!(
                        "native walk swap outcome mismatch on {name}: rust={:?}, hooked={:?}",
                        r.err(),
                        h.err()
                    );
                }
            }
        }

        // Unoptimized compilation does not call `register_locals` and stays clean.
        let unopt_rust = vesper::native::object_with_options(&synthetic[0].1, "main", None, false)
            .expect("unoptimized compile");
        let unopt_hooked = codegen_swap::with_hooks(hooks, || {
            vesper::native::object_with_options(&synthetic[0].1, "main", None, false)
        })
        .expect("unoptimized hooked compile");
        assert_eq!(unopt_rust, unopt_hooked, "unoptimized build differed");

        let after = [
            &SWAP_CALLS,
            &SWAP_DECLINED,
            &SWAP_GENERICS,
            &SWAP_NESTED,
            &SWAP_HANDLES,
            &SWAP_REGS,
        ]
        .map(|c| c.load(Ordering::Relaxed));
        let [calls, declined, generics, nested, handles, regs] =
            [0, 1, 2, 3, 4, 5].map(|i| after[i] - before[i]);

        eprintln!(
            "native walk swap: {bad} of {compiled} compiled worlds differ \
             ({calls} calls, {declined} declined, {generics} with generics, \
             {nested} nested, {handles} with handles, {regs} non-empty)"
        );
        assert!(
            compiled >= synthetic.len(),
            "only {compiled} worlds compiled"
        );
        assert!(calls > 0, "hook never called");
        assert_eq!(declined, 0, "{declined} graphs were declined");
        assert!(generics > 0, "no non-empty generic_vars exercised");
        assert!(nested > 0, "no nested graph exercised");
        assert!(handles > 0, "no Op::Handle graph exercised");
        assert!(regs > 0, "no non-empty register set exercised");
        assert_eq!(bad, 0);
    });
}

/// Every corpus program's decisions, Rust and Vesper, line for line.
#[test]
fn vesper_decisions_match_rust() {
    let entry = package().entry("native_walk_decisions", true);
    let (mut bad, mut programs, mut lines, mut skipped) = (0, 0, 0, 0);
    let (mut registers, mut cyclic) = (0, 0);
    for s in stages() {
        let Some(i) = input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        programs += 1;
        let r = decisions(s.world, &s.program);
        lines += r.lines().count();
        registers += r
            .lines()
            .filter(|l| l.starts_with("registers "))
            .map(|l| l.split(' ').count() - 1)
            .sum::<usize>();
        cyclic += r.lines().filter(|l| l.starts_with("cyclic ")).count();
        if let Some(d) = first_difference(&r, &entry.text(&i)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("native walk: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "native walk: {bad} of {programs} programs differ ({lines} lines, {registers} register \
         locals, {cyclic} graphs with loops, {skipped} not encodable)"
    );
    assert!(programs > 3, "only {programs} programs");
    assert!(registers > 0 && cyclic > 0, "the corpus exercises nothing");
    assert_eq!(bad, 0);
}

/// Mutant `m` of a program: every `Read`'s pass and every argument's pass
/// cycled, so the plans meet `inout` and `sink` loans, written parameters
/// and callbacks the corpus rarely has. The mutants need not type check:
/// the plans read only names, passes and types.
fn pass_mutant(p: &Program, m: usize) -> Program {
    let passes = [Pass::In, Pass::Inout, Pass::Sink];
    let mut p = p.clone();
    let mut j = m;
    let mut next = || {
        j += 1;
        passes[j % 3]
    };
    for f in p.functions.values_mut() {
        for q in &mut f.params {
            q.pass = next();
        }
        for b in &mut f.blocks {
            for i in &mut b.instructions {
                match &mut i.op {
                    Op::Read { pass, .. } => *pass = next(),
                    Op::Call { args, .. }
                    | Op::Indirect { args, .. }
                    | Op::Tiled { args, .. }
                    | Op::Fip { args, .. }
                    | Op::Seq { args, .. } => {
                        for a in args {
                            a.pass = next();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    p
}

#[test]
fn vesper_decisions_match_rust_on_pass_mutants() {
    let entry = package().entry("native_walk_decisions", true);
    let (mut bad, mut programs) = (0, 0);
    for s in stages().iter().step_by(4) {
        for m in 1..=2 {
            let p = pass_mutant(&s.program, m);
            let Some(i) = input(s.world, &p) else {
                continue;
            };
            programs += 1;
            if let Some(d) = first_difference(&decisions(s.world, &p), &entry.text(&i)) {
                bad += 1;
                if bad <= 5 {
                    eprintln!("pass mutant: {}#m{m} differs at {d}", s.name);
                }
            }
        }
    }
    eprintln!("pass mutants: {bad} of {programs} programs differ");
    assert!(programs > 3, "only {programs} programs");
    assert_eq!(bad, 0);
}

/// The decisions of every corpus program, Rust and Vesper, best of five
/// (the encoding is timed separately and excluded, as SH3 measures passes).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn native_walk_decision_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("native_walk_decisions", true);
    let inputs: Vec<Vec<u8>> = stages()
        .iter()
        .filter_map(|s| input(s.world, &s.program))
        .collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let rust = best(&mut || {
        for s in stages() {
            std::hint::black_box(decisions(s.world, &s.program));
        }
    });
    let vesper = best(&mut || {
        for i in &inputs {
            std::hint::black_box(entry.words(i));
        }
    });
    eprintln!(
        "native-walk decisions over {} programs: Rust {rust:?}, Vesper {vesper:?} ({:.2}x)",
        inputs.len(),
        vesper.as_secs_f64() / rust.as_secs_f64()
    );
}
