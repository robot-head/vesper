//! Self-hosting SH3 lane 9 (#1248): `@vesper/ir-opt` (packages/vesper-ir-opt)
//! against `ir_opt::optimize` and `ir_opt::fip_reuse_pass`.
//!
//! Every corpus world's native-path IR (`ffi_lowering::drops::lower_world`,
//! the input of `ir_opt::optimize` in src/native.rs), plus the census IR
//! mutants (`CHECK_HARNESS_MUTANTS`, default 1, for the error paths):
//!
//! - `optimize_matches_rust`: the whole pass. The Vesper tables go through
//!   the package's `ir_opt_entry`, come back as words, and are decoded;
//!   the Rust tail of `optimize` (`ir::thread_tokens`, then
//!   `ir_check::validate`) runs on the decoded program. The result (the
//!   count of replaced reads plus reuse matches, or the error) and the
//!   program's `Debug` text must equal Rust's.
//! - `fip_reuse_matches_rust`: `fip_reuse_pass` alone on the same inputs.
//! - `optimize_cost` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA of the pass
//!   alone against Rust's, with encoding and decoding timed separately.
//!
//! The world's answers the `@fip` reuse needs (`fip::active`,
//! `caller_slot_functions`, `box_inner`, `pattern_box_slots`, `slot_key`)
//! are written as facts after the program (`facts`): the package does not
//! port the layout and `Box` questions, the way the inliner lane's facts do.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use std::time::Duration;
use vesper::ast::{Generic, Type};
use vesper::check::World;
use vesper::fip::FipOp;
use vesper::ir::{Op, Program};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ir-opt"))
}

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("ir_opt_entry", true))
}

/// Every `n`-th corpus input (`IR_OPT_STEP`, default 24): Rust's egglog
/// takes most of the time (about 9 minutes for the whole corpus in the
/// port-compiler profile, much longer in a debug build), so the default
/// runs a slice; `IR_OPT_STEP=1` runs every input (the report's numbers).
/// The `@fip` cases always run.
fn step() -> usize {
    std::env::var("IR_OPT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(24)
}

/// One input of the pass.
struct Input {
    name: String,
    world: &'static World,
    program: Program,
}

fn inputs() -> &'static [Input] {
    static I: OnceLock<Vec<Input>> = OnceLock::new();
    I.get_or_init(|| {
        let mut out = vec![];
        for c in corpus() {
            out.push(Input {
                name: c.name.clone(),
                world: &c.world,
                program: vesper::ffi_lowering::drops::lower_world(&c.world),
            });
        }
        for c in ir_cases(1) {
            if c.name.contains("#m") {
                out.push(Input {
                    name: c.name,
                    world: c.world,
                    program: c.program,
                });
            }
        }
        let mut out: Vec<Input> = out.into_iter().step_by(step()).collect();
        // The corpus has no `@fip` code: the fip checker's test sources
        // (tests/fip.rs and the others, gathered by SH2 lane 12) and their
        // five mutants exercise the reuse pass.
        for (name, world) in fip_cases() {
            let program = vesper::ffi_lowering::drops::lower_world(world);
            for m in 1..=IR_MUTANTS {
                out.push(Input {
                    name: format!("{name}#m{m}"),
                    world,
                    program: ir_mutant(&program, m),
                });
            }
            out.push(Input {
                name: name.clone(),
                world,
                program,
            });
        }
        out
    })
}

/// packages/vesper-check-fip/tests/cases.txt, the cases that check clean.
fn fip_cases() -> Vec<(String, &'static World)> {
    let text =
        std::fs::read_to_string(repo().join("packages/vesper-check-fip/tests/cases.txt")).unwrap();
    let mut out = vec![];
    let mut sections: Vec<(String, String)> = vec![];
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("==== ") {
            sections.push((n.to_string(), String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    for (name, body) in sections {
        let parsed = vesper::parser::parse(&body);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let checked = vesper::check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            continue;
        }
        let world: &'static World = Box::leak(Box::new(checked.world));
        out.push((format!("fip:{name}"), world));
    }
    out
}

// ------------------------------------------------------------------ facts

/// `fip::slot_key` (pub(crate) in src/fip.rs), transcribed.
fn slot_key(world: &World, generics: &[Generic], t: &Type) -> Result<(String, u32), String> {
    fn has_generic(t: &Type, generics: &[Generic]) -> bool {
        match t {
            Type::Named(n, args) => {
                generics.iter().any(|g| !g.row && &g.name == n)
                    || args.iter().any(|x| has_generic(x, generics))
            }
            Type::Tuple(ts) => ts.iter().any(|x| has_generic(x, generics)),
            Type::Function { params, result, .. } => {
                params.iter().any(|(_, t)| has_generic(t, generics))
                    || has_generic(result, generics)
            }
            Type::Row(_) | Type::IntLiteral(_) | Type::Infer => false,
        }
    }
    let norm = vesper::numeric::default_type(t);
    if !has_generic(&norm, generics) {
        let layout = vesper::layout::Layout::of(world, &norm)?;
        Ok((format!("size:{}", layout.size), layout.size))
    } else {
        Ok((format!("type:{norm}"), 0))
    }
}

/// The world's answers for reuse.ves, and the `slot_key` error messages
/// its error indices name.
#[derive(Default)]
struct Facts {
    keys: BTreeMap<String, u64>,
    errors: Vec<String>,
}

impl Facts {
    fn key(&mut self, k: String) -> u64 {
        let n = self.keys.len() as u64;
        *self.keys.entry(k).or_insert(n)
    }

    fn error(&mut self, e: String) -> u64 {
        self.errors.push(e);
        self.errors.len() as u64 - 1
    }

    /// A `Box::new` or caller-slot call of type `ty`: nothing, a key or an
    /// error.
    fn need(&mut self, world: &World, generics: &[Generic], ty: &Type, out: &mut Vec<u64>) {
        match vesper::fip::box_inner(world, ty) {
            None => out.push(0),
            Some(inner) => match slot_key(world, generics, inner) {
                Ok((k, _)) => {
                    let k = self.key(k);
                    out.extend([3, k]);
                }
                Err(e) => {
                    let e = self.error(e);
                    out.extend([4, e]);
                }
            },
        }
    }

    fn words(&mut self, world: &World, program: &Program) -> Vec<u64> {
        if !vesper::fip::active(world) {
            return vec![0];
        }
        let caller = vesper::fip::caller_slot_functions(world);
        let mut body = vec![];
        for f in program.functions.values() {
            let Some(src) = world.functions.get(&f.name).filter(|s| s.fip) else {
                body.push(0);
                continue;
            };
            body.push(1);
            let ret = vesper::fip::box_inner(world, &src.result)
                .and_then(|inner| slot_key(world, &src.generics, inner).ok());
            match ret {
                Some((k, _)) => {
                    let k = self.key(k);
                    body.push(k + 1);
                }
                None => body.push(0),
            }
            let n: usize = f.blocks.iter().map(|b| b.instructions.len()).sum();
            body.push(n as u64);
            for b in &f.blocks {
                for i in &b.instructions {
                    match &i.op {
                        Op::Bind { pattern, value } => {
                            let Some(ty) = f.values.get(value) else {
                                body.push(0);
                                continue;
                            };
                            let slots = vesper::fip::pattern_box_slots(world, pattern, ty);
                            let mut rec = vec![];
                            let mut err = None;
                            for (_, inner) in &slots {
                                match slot_key(world, &src.generics, inner) {
                                    Ok((k, size)) => {
                                        let k = self.key(k);
                                        rec.extend([k, size as u64]);
                                    }
                                    Err(e) => {
                                        err = Some(e);
                                        break;
                                    }
                                }
                            }
                            if let Some(e) = err {
                                let e = self.error(e);
                                body.extend([2, e]);
                            } else {
                                body.extend([1, slots.len() as u64]);
                                body.extend(rec);
                            }
                        }
                        Op::Fip {
                            op: FipOp::BoxNew, ..
                        } => self.need(world, &src.generics, &i.ty, &mut body),
                        Op::Call { callee, .. } if caller.contains(callee) => {
                            self.need(world, &src.generics, &i.ty, &mut body)
                        }
                        _ => body.push(0),
                    }
                }
            }
        }
        let mut out = vec![1, self.keys.len() as u64];
        out.extend(body);
        out
    }
}

// ------------------------------------------------------------------ the pass

const MODE_OPTIMIZE: u64 = 0;
const MODE_REUSE: u64 = 1;

/// The entry's input and what decoding its output needs.
struct Prepared {
    input: Vec<u8>,
    pool: ir_words::Pool,
    errors: Vec<String>,
}

fn prepare(world: &World, program: &Program, mode: u64) -> Option<Prepared> {
    let (mut input, pool) = ir_words::program_input(world, program)?;
    let mut facts = Facts::default();
    let mut tail = vec![mode];
    tail.extend(facts.words(world, program));
    for w in tail {
        input.extend(w.to_le_bytes());
    }
    Some(Prepared {
        input,
        pool,
        errors: facts.errors,
    })
}

/// The entry's words as the pass's result: the count or the error, and the
/// rewritten program. `optimize` runs Rust's tail (token threading and the
/// IR check) on the decoded program.
fn finish(
    world: &World,
    prep: &Prepared,
    words: &[u64],
    mode: u64,
) -> (Result<usize, String>, Option<Program>) {
    let (status, error, count, np) = (words[0], words[1], words[2], words[3] as usize);
    match status {
        0 => {}
        1 => return (Err("read lacks state evidence".into()), None),
        // egglog's own message is not reproduced; the prefix is compared.
        2 => return (Err("effect equality saturation: ".into()), None),
        _ => return (Err(prep.errors[error as usize].clone()), None),
    }
    let mut pool = prep.pool.clone();
    for k in 0..np {
        let w = &words[4 + 3 * k..7 + 3 * k];
        pool.items.push(ir_words::Payload::Fip(FipOp::BoxReuse {
            bind_dest: w[0] as u32,
            slot_index: w[1] as usize,
            size: w[2] as u32,
        }));
    }
    let mut program = ir_words::decode_program(&words[4 + 3 * np..], &pool);
    if mode == MODE_OPTIMIZE {
        vesper::ir::thread_tokens(&mut program);
        let errors = vesper::ir_check::validate(world, &program);
        if !errors.is_empty() {
            return (
                Err(format!(
                    "optimized IR failed ownership validation: {errors:?}"
                )),
                None,
            );
        }
    }
    (Ok(count as usize), Some(program))
}

fn rust_pass(world: &World, program: &Program, mode: u64) -> (Result<usize, String>, Program) {
    let mut p = program.clone();
    let r = if mode == MODE_OPTIMIZE {
        vesper::ir_opt::optimize(world, &mut p)
    } else {
        vesper::ir_opt::fip_reuse_pass(world, &mut p)
    };
    (r, p)
}

fn same_result(rust: &Result<usize, String>, ours: &Result<usize, String>) -> bool {
    match (rust, ours) {
        (Err(a), Err(b)) if b == "effect equality saturation: " => a.starts_with(b.as_str()),
        _ => rust == ours,
    }
}

fn debug(p: &Program) -> String {
    format!("{p:#?}")
}

/// Runs both sides on every input; returns (inputs, skipped, differing).
fn compare(mode: u64) -> (usize, usize, usize) {
    // Load and compile the package with panics shown.
    entry();
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut bad, mut skipped, mut done) = (0, 0, 0);
    let (mut forwarded, mut errors, mut egglog_errors) = (0, 0, 0);
    for c in inputs() {
        let rust = std::panic::catch_unwind(|| rust_pass(c.world, &c.program, mode));
        let Ok((rr, rp)) = rust else {
            skipped += 1;
            continue;
        };
        let Some(prep) = prepare(c.world, &c.program, mode) else {
            skipped += 1;
            continue;
        };
        done += 1;
        let words = entry().words(&prep.input);
        let (vr, vp) = finish(c.world, &prep, &words, mode);
        match &rr {
            Ok(n) => forwarded += n,
            Err(e) => {
                errors += 1;
                if e.starts_with("effect equality saturation") {
                    egglog_errors += 1;
                }
            }
        }
        let mut diff = None;
        if !same_result(&rr, &vr) {
            diff = Some(format!("result rust {rr:?} vesper {vr:?}"));
        } else if rr.is_ok()
            && let Some(vp) = &vp
            && let Some(d) = first_difference(&debug(&rp), &debug(vp))
        {
            diff = Some(d);
        }
        if let Some(d) = diff {
            bad += 1;
            if bad <= 5 {
                eprintln!("mode {mode}: {} differs: {d}", c.name);
            }
        }
    }
    std::panic::set_hook(quiet);
    eprintln!(
        "mode {mode}: {bad} of {done} inputs differ ({skipped} skipped), {forwarded} replaced or reused, {errors} errors ({egglog_errors} egglog refusals, compared by prefix)"
    );
    (done, skipped, bad)
}

#[test]
fn optimize_matches_rust() {
    let (done, _, bad) = compare(MODE_OPTIMIZE);
    assert!(done > 50, "only {done} inputs");
    assert_eq!(bad, 0);
}

#[test]
fn fip_reuse_matches_rust() {
    let (done, _, bad) = compare(MODE_REUSE);
    assert!(done > 50, "only {done} inputs");
    assert_eq!(bad, 0);
}

/// The `@fip` inputs exist: the reuse pass has something to match.
#[test]
fn corpus_has_reuse() {
    let mut matched = 0;
    let mut worlds = BTreeSet::new();
    for c in inputs() {
        if !vesper::fip::active(c.world) || c.name.contains("#m") {
            continue;
        }
        let mut p = c.program.clone();
        if let Ok(n) = vesper::ir_opt::fip_reuse_pass(c.world, &mut p)
            && n > 0
        {
            matched += n;
            worlds.insert(c.name.clone());
        }
    }
    eprintln!("reuse: {matched} matches in {} worlds", worlds.len());
    assert!(matched > 0);
}

/// The pass alone (Vesper minus encode and decode) against Rust's, ABBA.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn optimize_cost() {
    if !speed() {
        return;
    }
    let cases: Vec<(&Input, Prepared)> = inputs()
        .iter()
        .filter(|c| !c.name.contains("#m"))
        .filter(|c| vesper::ir_opt::optimize(c.world, &mut c.program.clone()).is_ok())
        .filter_map(|c| prepare(c.world, &c.program, MODE_OPTIMIZE).map(|p| (c, p)))
        .collect();
    let outputs: Vec<Vec<u64>> = cases.iter().map(|(_, p)| entry().words(&p.input)).collect();
    let mut rust = || {
        time(|| {
            for (c, _) in &cases {
                let mut p = c.program.clone();
                std::hint::black_box(vesper::ir_opt::optimize(c.world, &mut p).ok());
            }
        })
    };
    let mut ves = || {
        time(|| {
            for ((c, prep), w) in cases.iter().zip(&outputs) {
                let _ = w;
                let words = entry().words(&prep.input);
                let _ = std::hint::black_box(finish(c.world, prep, &words, MODE_OPTIMIZE));
            }
        })
    };
    let (r, v) = abba("ir_opt whole", &mut rust, &mut ves);
    let best = |f: &mut dyn FnMut() -> Duration| (0..5).map(|_| f()).min().unwrap();
    let encode = best(&mut || {
        time(|| {
            for (c, _) in &cases {
                std::hint::black_box(prepare(c.world, &c.program, MODE_OPTIMIZE));
            }
        })
    });
    let pass = best(&mut || {
        time(|| {
            for (_, p) in &cases {
                std::hint::black_box(entry().words(&p.input));
            }
        })
    });
    let decode = best(&mut || {
        time(|| {
            for ((_, prep), w) in cases.iter().zip(&outputs) {
                let np = w[3] as usize;
                std::hint::black_box(ir_words::decode_program(&w[4 + 3 * np..], &prep.pool));
            }
        })
    });
    let tail = best(&mut || {
        time(|| {
            for (c, p) in &cases {
                let mut q = c.program.clone();
                vesper::ir::thread_tokens(&mut q);
                std::hint::black_box(vesper::ir_check::validate(c.world, &q));
                let _ = p;
            }
        })
    });
    eprintln!(
        "ir_opt over {} worlds: rust {r:?}, vesper whole {v:?}; encode {encode:?}, \
         Vesper read+pass+write {pass:?}, decode {decode:?}, thread+validate {tail:?}",
        cases.len()
    );
}
