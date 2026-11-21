//! Self-hosting SH4 lane 9 (#1283), part 1: `@vesper/codegen-seq`'s
//! lowering decisions against the Rust compiler's, per function, over every
//! corpus program native code is emitted from.
//!
//! The decisions are what src/seq/native.rs and the `Op::Seq` dispatch of
//! src/native.rs read before they emit anything:
//!
//! - `drop <local> <kind>`: [`vesper::fip::seq_drop_plan`], the slots native
//!   code frees itself.
//! - `borrowed %v`: [`vesper::fip::borrowed_scrutinees`].
//! - `seq %v <name> recv= pass= mut= fail= prim= scan= elem= stride=`: per
//!   `Op::Seq`, [`SeqOp::name`], `has_receiver`, `receiver_pass`,
//!   `mutates`, `fallible`, `place_primitive`, `byte_scan`, and the
//!   defaulted element type (of the receiver, or of the result when there
//!   is none) with its packed stride ([`vesper::seq::elem_bytes`]).
//!
//! Programs: each corpus world lowered as the native backend does, with
//! optimize on (lowering, `ir_opt`, inliner, drop elaboration, bounds
//! hoisting) and off (`fip_reuse_pass`, drop elaboration).
//! `CODEGEN_SEQ_STEP=n` runs every n-th world (default 1 under Cargo, 4 under Bazel).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::fmt::Write as _;
use std::sync::OnceLock;
use vesper::ast::{Pass, Type};
use vesper::check::World;
use vesper::ir::{Op, Program};
use vesper::seq::SeqOp;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-seq"))
}

/// Every `step()`-th world: `CODEGEN_SEQ_STEP`, else 1 under Cargo and 4
/// under Bazel, whose CI run shares one time limit with the whole suite.
fn step() -> usize {
    std::env::var("CODEGEN_SEQ_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(if std::env::var_os("TEST_SRCDIR").is_some() {
            4
        } else {
            1
        })
}

/// One program native code is emitted from.
pub struct Stage {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// `w` lowered as the native backend lowers it: optimize on (lowering,
/// `ir_opt`, inliner, drop elaboration, bounds hoisting) and off
/// (`fip_reuse_pass`, drop elaboration).
fn native_programs(w: &World) -> Vec<(&'static str, Program)> {
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
}

/// Every corpus world's native-path program, optimized and not. A pass
/// that refuses a program drops that stage.
pub fn stages() -> &'static [Stage] {
    static S: OnceLock<Vec<Stage>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = vec![];
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for c in corpus().iter().step_by(step()) {
            let w = &c.world;
            let r = std::panic::catch_unwind(|| native_programs(w));
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

/// The package's input: @vesper/place-ir's, then one `fip` flag per
/// function in program order (`world.functions[f].fip`).
fn input(w: &World, p: &Program) -> Option<Vec<u8>> {
    let (mut bytes, _) = ir_words::program_input(w, p)?;
    bytes.extend((p.functions.len() as u64).to_le_bytes());
    for f in p.functions.values() {
        let fip = w.functions.get(&f.name).is_some_and(|s| s.fip);
        bytes.extend(u64::from(fip).to_le_bytes());
    }
    Some(bytes)
}

fn bit(b: bool) -> char {
    if b { '1' } else { '0' }
}

/// The Rust compiler's decisions, in the package's text format.
pub fn rust_decisions(w: &World, p: &Program) -> String {
    let mut o = String::new();
    for f in p.functions.values() {
        writeln!(o, "fn {}", f.name).unwrap();
        for (name, kind) in vesper::fip::seq_drop_plan(w, f) {
            writeln!(o, "drop {name} {kind:?}").unwrap();
        }
        for v in vesper::fip::borrowed_scrutinees(f) {
            writeln!(o, "borrowed %{v}").unwrap();
        }
        for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
            let Op::Seq { op, args } = &inst.op else {
                continue;
            };
            let op: SeqOp = *op;
            let pass = match op.receiver_pass() {
                Pass::Inout => "inout",
                _ => "in",
            };
            write!(
                o,
                "seq %{} {} recv={} pass={pass} mut={} fail={} prim={} scan={} elem=",
                inst.dest,
                op.name(),
                bit(op.has_receiver()),
                bit(op.mutates()),
                bit(op.fallible()),
                bit(op.place_primitive()),
                bit(op.byte_scan()),
            )
            .unwrap();
            let source: Option<&Type> = if op.has_receiver() {
                args.first().and_then(|a| f.values.get(&a.value))
            } else {
                Some(&inst.ty)
            };
            match source.and_then(vesper::seq::elem) {
                Some(e) => {
                    let e = vesper::numeric::default_type(e);
                    let stride = vesper::seq::elem_bytes(w, &e)
                        .map_or_else(|_| "-".to_string(), |s| s.to_string());
                    writeln!(o, "{e:?} stride={stride}").unwrap();
                }
                None => writeln!(o, "- stride=-").unwrap(),
            }
        }
    }
    o
}

#[test]
fn decisions_match_rust() {
    let entry = package().entry("codegen_seq_decisions", true);
    let (mut bad, mut skipped, mut lines, mut drops, mut seqs) = (0, 0, 0, 0, 0);
    let (mut rust_time, mut vesper_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for s in stages() {
        let Some(bytes) = input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let mut r = String::new();
        rust_time += time(|| r = rust_decisions(s.world, &s.program));
        let mut v = String::new();
        vesper_time += time(|| v = entry.text(&bytes));
        lines += r.lines().count();
        drops += r.lines().filter(|l| l.starts_with("drop ")).count();
        seqs += r.lines().filter(|l| l.starts_with("seq ")).count();
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("decisions: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "decisions: {bad} of {} programs differ, {skipped} not encodable; {lines} lines \
         ({drops} planned drops, {seqs} sequence ops); Rust {rust_time:?}, Vesper {vesper_time:?} \
         (decode included)",
        stages().len()
    );
    assert!(stages().len() > 50, "only {} programs", stages().len());
    assert!(drops > 0 && seqs > 0);
    assert_eq!(bad, 0);
}

/// Sequences of `i128`/`u128` (16-byte elements), which no corpus file
/// holds.
#[test]
fn wide_element_strides_match_rust() {
    let src = "fn f() -> u64 {
    let mut s: ChunkSeq[u128] = ChunkSeq::new[u128]();
    s.push(7);
    let mut t: ChunkSeq[i128] = ChunkSeq::with_capacity[i128](2);
    t.push(-1);
    let a = match s.try_get(0) { Option::Some(x) => wrapping_u64(x), Option::None => 0 };
    let b = match t.try_get(0) { Option::Some(x) => wrapping_u64(x), Option::None => 0 };
    wrapping_add(wrapping_add(a, b), wrapping_add(s.len(), t.len()))
}
fn main() -> u64 { f() }
";
    let parsed = vesper::parser::parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let w = checked.world;
    let entry = package().entry("codegen_seq_decisions", true);
    let programs = native_programs(&w);
    assert_eq!(programs.len(), 2);
    for (stage, p) in &programs {
        let r = rust_decisions(&w, p);
        assert!(r.contains("stride=16"), "{stage}: {r}");
        let v = entry.text(&input(&w, p).expect("encodable"));
        assert_eq!(first_difference(&r, &v), None, "{stage}");
    }
}
