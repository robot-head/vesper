//! Rust-exit gap wave 1, lane 11 (#1509), part 1: `@vesper/native-walk`'s
//! function plans against the Rust walk of src/native.rs, over every corpus
//! world's program as the native path compiles it (optimize on: after
//! bounds hoisting; optimize off: after drop elaboration).
//!
//! Part 1 ports what the walk decides about a function before it emits
//! anything (see packages/vesper-codegen-walk/src/lib.ves):
//!
//! - **Callee facts**: for every function of `world.functions`, whether
//!   its row is empty and whether it is specialized into its callers
//!   (`specialized_function`), the two things the register plan asks of a
//!   callee. The predicates belong to SH4 lane 7 (#1281, PR #1306); the
//!   package's copies are private until that merges.
//! - **Register plans**: for every lowered graph, its cyclic blocks
//!   (`aggregates::cyclic_blocks`) and the locals `register_locals` keeps
//!   in registers, for a top-level graph and for one a specialized call
//!   inlines (`nested`), with the generic substitution empty.
//!
//! [`decisions`] is the Rust half. src/native.rs is read-only to this lane
//! and keeps these functions private, so the Rust half restates them
//! here, calling the compiler's own predicates where they are public
//! (`Layout::of`, `check::free_names`, `numeric::default_type`, the effect
//! name tests). Keep it in step with src/native.rs: a change there to a
//! function named above changes the matching function below and the
//! package. Part 2 replaces this restatement with the walk's own answers
//! through the swap point.
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
use vesper::ast::{Function, Pass, Pattern, PatternKind, Type};
use vesper::check::World;
use vesper::ir::{Argument, FunctionIr, Op, Program, Terminator};
use vesper::layout::Layout;
use vesper::seq::SeqOp;

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

// ------------------------------------------------------------ Rust half
//
// Restated from src/native.rs (`capability_only`, `fail_only`,
// `specialized_function`, `register_locals`, `keeps_in_home`,
// `in_param_registers`, `bound_names`), src/native_aggregates.rs
// (`cyclic_blocks`, `LOOP_WEIGHT`), src/source_analysis.rs
// (`contains_function`) and src/bounds_hoist.rs (`calls_out`,
// `keeps_length`), with the generic substitution empty.

const LOOP_WEIGHT: usize = 8;

fn contains_function(world: &World, t: &Type) -> bool {
    fn visit(world: &World, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Function { .. } => true,
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if !seen.insert(n.clone()) {
                    return args.iter().any(|t| matches!(t, Type::Function { .. }));
                }
                let result = world.adts.get(n).is_some_and(|a| {
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| visit(world, &vesper::ast::substitute(&f.ty, &vars), seen))
                });
                seen.remove(n);
                result
            }
            _ => false,
        }
    }
    visit(world, t, &mut BTreeSet::new())
}

fn capability_only(f: &Function) -> bool {
    f.effects.instances.iter().all(|e| {
        vesper::capability_effects::is_capability_effect(&e.name)
            || vesper::ffi_lowering::is_foreign_effect(&e.name)
    })
}

fn fail_only(world: &World, f: &Function) -> bool {
    f.effects.tail.is_none()
        && f.effects.instances.iter().any(|e| e.name == "Fail")
        && f.effects.instances.iter().all(|e| {
            (e.name == "Fail" && e.label.is_none() && e.args.len() == 1)
                || ((vesper::capability_effects::is_capability_effect(&e.name)
                    || vesper::ffi_lowering::is_foreign_effect(&e.name))
                    && e.label.is_none()
                    && e.args.is_empty())
        })
        && !contains_function(world, &f.result)
        && !f.params.iter().any(|p| contains_function(world, &p.ty))
}

fn specialized_function(world: &World, f: &Function) -> bool {
    (!capability_only(f)
        || f.effects.tail.is_some()
        || contains_function(world, &f.result)
        || f.params.iter().any(|p| contains_function(world, &p.ty)))
        && !fail_only(world, f)
}

fn cyclic_blocks(f: &FunctionIr) -> BTreeSet<usize> {
    let mut graph = petgraph::graphmap::DiGraphMap::<usize, ()>::new();
    for (b, block) in f.blocks.iter().enumerate() {
        graph.add_node(b);
        let targets: Vec<usize> = match &block.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
            _ => vec![],
        };
        for t in targets {
            graph.add_edge(b, t, ());
        }
    }
    petgraph::algo::tarjan_scc(&graph)
        .into_iter()
        .filter(|c| c.len() > 1 || graph.contains_edge(c[0], c[0]))
        .flatten()
        .collect()
}

fn keeps_length(op: SeqOp) -> bool {
    matches!(
        op,
        SeqOp::Get
            | SeqOp::SliceGet
            | SeqOp::Set
            | SeqOp::SliceSet
            | SeqOp::TryGet
            | SeqOp::TrySet
            | SeqOp::Len
            | SeqOp::SliceLen
            | SeqOp::AsSlice
            | SeqOp::SubSlice
    )
}

fn calls_out(op: &Op) -> bool {
    match op {
        Op::Call { .. }
        | Op::Tiled { .. }
        | Op::Fip { .. }
        | Op::Indirect { .. }
        | Op::Closure { .. }
        | Op::Handle { .. } => true,
        Op::Seq { op, .. } => {
            !(keeps_length(*op) || matches!(op, SeqOp::Hoisted(_) | SeqOp::BoundsGuard { .. }))
        }
        _ => false,
    }
}

fn keeps_in_home(others: usize, loans: usize) -> bool {
    loans >= LOOP_WEIGHT && others > 0
}

fn bound_names(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| bound_names(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| bound_names(p, out))
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

fn register_locals(world: &World, f: &FunctionIr, nested: bool) -> BTreeSet<String> {
    let mut memory = BTreeSet::new();
    let place_roots = |args: &[Argument], memory: &mut BTreeSet<String>| {
        for a in args {
            if let Some(p) = &a.place {
                memory.insert(p.root.clone());
            }
        }
    };
    let cyclic = cyclic_blocks(f);
    let weight = |b: usize| {
        if cyclic.contains(&b) { LOOP_WEIGHT } else { 1 }
    };
    let mut lent_in: BTreeMap<String, usize> = BTreeMap::new();
    let mut touched: BTreeMap<String, usize> = BTreeMap::new();
    for (b, inst) in f
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| block.instructions.iter().map(move |i| (b, i)))
    {
        match &inst.op {
            Op::Read { root, .. } => *touched.entry(root.clone()).or_default() += weight(b),
            Op::Write { place, .. } => {
                *touched
                    .entry(place.split('.').next().unwrap_or(place).to_owned())
                    .or_default() += weight(b)
            }
            _ => {}
        }
        match &inst.op {
            Op::Call { callee, args, .. } => {
                let g = world.functions.get(callee);
                let pure =
                    g.is_some_and(|g| g.effects.instances.is_empty() && g.effects.tail.is_none());
                let inlined = g.is_some_and(|g| specialized_function(world, g));
                if !pure {
                    for a in args {
                        let Some(p) = &a.place else {
                            continue;
                        };
                        if a.pass != Pass::In {
                            memory.insert(p.root.clone());
                        } else if !inlined {
                            *lent_in.entry(p.root.clone()).or_default() += weight(b);
                        }
                    }
                }
            }
            Op::Indirect { args, .. } | Op::Tiled { args, .. } => place_roots(args, &mut memory),
            Op::Closure { moves, .. } => {
                memory.extend(moves.iter().map(|p| p.root.clone()));
            }
            Op::Handle {
                expr,
                aliases,
                moves,
                ..
            } => {
                memory.extend(vesper::check::free_names(expr));
                memory.extend(aliases.keys().cloned());
                memory.extend(aliases.values().cloned());
                memory.extend(moves.iter().map(|p| p.root.clone()));
            }
            _ => {}
        }
    }
    for (n, loans) in lent_in {
        let others = touched.get(&n).copied().unwrap_or(0).saturating_sub(loans);
        if keeps_in_home(others, loans) {
            memory.insert(n);
        }
    }
    f.locals
        .iter()
        .filter(|(n, t)| {
            let t = vesper::numeric::default_type(t);
            !memory.contains(*n)
                && matches!(&t, Type::Named(name, args) if args.is_empty()
                    && (vesper::numeric::is_integer_name(name)
                        || matches!(name.as_str(), "f32" | "f64" | "bool")))
        })
        .map(|(n, _)| n.clone())
        .chain(in_param_registers(world, f, nested))
        .collect()
}

fn in_param_registers(world: &World, f: &FunctionIr, nested: bool) -> Vec<String> {
    let mut kept = BTreeSet::new();
    let lent = |args: &[Argument], kept: &mut BTreeSet<String>| {
        for a in args {
            if a.pass != Pass::In
                && let Some(p) = &a.place
            {
                kept.insert(p.root.clone());
            }
        }
    };
    let callback = |name: &str| {
        f.params.iter().any(|p| {
            p.name == name && matches!(vesper::numeric::default_type(&p.ty), Type::Function { .. })
        })
    };
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Write { place, .. } => {
                kept.insert(place.split('.').next().unwrap_or(place).to_owned());
            }
            Op::Bind { pattern, .. } => bound_names(pattern, &mut kept),
            Op::Read {
                root, pass, path, ..
            } if *pass != Pass::In && !(path.is_empty() && callback(root)) => {
                kept.insert(root.clone());
            }
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Tiled { args, .. }
            | Op::Fip { args, .. }
            | Op::Seq { args, .. } => lent(args, &mut kept),
            Op::Closure { moves, .. } => kept.extend(moves.iter().map(|p| p.root.clone())),
            Op::Handle {
                expr,
                aliases,
                moves,
                ..
            } => {
                kept.extend(vesper::check::free_names(expr));
                kept.extend(aliases.keys().cloned());
                kept.extend(aliases.values().cloned());
                kept.extend(moves.iter().map(|p| p.root.clone()));
            }
            _ => {}
        }
    }
    if !nested
        && f.blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .any(|i| calls_out(&i.op))
    {
        kept.extend(f.params.iter().map(|p| p.name.clone()));
    }
    f.params
        .iter()
        .filter(|p| {
            let ty = vesper::numeric::default_type(&p.ty);
            p.pass == Pass::In
                && !kept.contains(&p.name)
                && f.locals.contains_key(&p.name)
                && Layout::of(world, &ty).is_ok_and(|l| !l.aggregate())
        })
        .map(|p| p.name.clone())
        .collect()
}

/// The decisions text of one program (the package's `native_walk_decisions`).
fn decisions(w: &World, p: &Program) -> String {
    let mut o = String::new();
    let bit = |b: bool| if b { '1' } else { '0' };
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
            let regs = register_locals(w, f, nested);
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
