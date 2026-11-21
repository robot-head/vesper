//! Self-hosting SH3 lane 1 (#1240): `@vesper/place-ir` against the Rust
//! Place IR, over every corpus world at every native-path pass boundary.
//!
//! - Rust round trip: `Program` -> words -> `Program` is `Debug`-equal
//!   (tests/common/ir_words.rs alone).
//! - Reader: the Vesper tables' canonical dump equals the Rust dump.
//! - Writer: the Vesper tables written back as words decode to a
//!   `Program` `Debug`-equal to the input.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ir::Program;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-place-ir"))
}

/// Every `n`-th corpus world (`PLACE_IR_STEP`, default 4): the passes of
/// every world take about half an hour in a debug build, so the default
/// runs a slice; `PLACE_IR_STEP=1` runs every world (the report's numbers).
fn step() -> usize {
    std::env::var("PLACE_IR_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// One program at one pass boundary.
pub struct Stage {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// Every corpus world's program after each native-path pass: lowering
/// (with the FFI drops), then with optimize on `ir_opt`, the inliner, drop
/// elaboration and bounds hoisting; with optimize off `fip_reuse_pass` and
/// drop elaboration. A pass that refuses a program ends its stages.
pub fn stages() -> &'static [Stage] {
    static S: OnceLock<Vec<Stage>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = vec![];
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for c in corpus().iter().step_by(step()) {
            let w = &c.world;
            let r = std::panic::catch_unwind(|| {
                let mut v = vec![];
                let lowered = vesper::ffi_lowering::drops::lower_world(w);
                v.push(("lower", lowered.clone()));
                let mut p = lowered.clone();
                if vesper::ir_opt::optimize(w, &mut p).is_ok() {
                    v.push(("opt", p.clone()));
                    vesper::ir_inline::inline(w, &mut p, &vesper::ir_inline::Budget::default());
                    v.push(("inline", p.clone()));
                    vesper::fip::elaborate_non_fip_drops(w, &mut p);
                    v.push(("drops", p.clone()));
                    vesper::bounds_hoist::run(&mut p);
                    v.push(("hoist", p));
                }
                let mut q = lowered;
                if vesper::ir_opt::fip_reuse_pass(w, &mut q).is_ok() {
                    vesper::fip::elaborate_non_fip_drops(w, &mut q);
                    v.push(("drops-unopt", q));
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

fn debug(p: &Program) -> String {
    format!("{p:#?}")
}

#[test]
fn rust_round_trip_is_debug_equal() {
    let (mut bad, mut skipped, mut words) = (0, 0, 0usize);
    for s in stages() {
        let Some((w, pool)) = ir_words::program_words(&s.program) else {
            skipped += 1;
            continue;
        };
        words += w.len();
        let back = ir_words::decode_program(&w, &pool);
        if let Some(d) = first_difference(&debug(&s.program), &debug(&back)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("rust round trip: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "rust round trip: {bad} of {} stages differ, {skipped} not encodable, {words} words",
        stages().len()
    );
    assert!(stages().len() > 50, "only {} stages", stages().len());
    assert_eq!(bad, 0);
}

#[test]
fn vesper_dump_matches_rust() {
    let dump = package().entry("place_ir_dump", true);
    let (mut bad, mut skipped, mut lines) = (0, 0, 0);
    for s in stages() {
        let Some((input, _)) = ir_words::program_input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let r = ir_words::dump(&s.program);
        lines += r.lines().count();
        if let Some(d) = first_difference(&r, &dump.text(&input)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("dump: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "dump: {bad} of {} stages differ, {skipped} not encodable, {lines} lines",
        stages().len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn vesper_writer_round_trip_is_debug_equal() {
    let write = package().entry("place_ir_words", true);
    let (mut bad, mut skipped) = (0, 0);
    for s in stages() {
        let Some((input, pool)) = ir_words::program_input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let back = ir_words::decode_program(&write.words(&input), &pool);
        if let Some(d) = first_difference(&debug(&s.program), &debug(&back)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("writer: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "writer: {bad} of {} stages differ, {skipped} not encodable",
        stages().len()
    );
    assert_eq!(bad, 0);
}

/// The cost of a swap, per corpus world at the `lower` boundary: encoding
/// (Rust), the Vesper read and write, and decoding (Rust), next to the
/// Rust lowering it would replace. IR lanes subtract it from their pass.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn place_ir_cost() {
    if !speed() {
        return;
    }
    let write = package().entry("place_ir_words", true);
    let lowered: Vec<&Stage> = stages()
        .iter()
        .filter(|s| s.name.ends_with("@lower"))
        .collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = lowered
        .iter()
        .filter_map(|s| ir_words::program_input(s.world, &s.program))
        .collect();
    let outputs: Vec<Vec<u64>> = inputs.iter().map(|(i, _)| write.words(i)).collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let lower = best(&mut || {
        for s in &lowered {
            std::hint::black_box(vesper::ir::lower_world(s.world));
        }
    });
    let encode = best(&mut || {
        for s in &lowered {
            std::hint::black_box(ir_words::program_input(s.world, &s.program));
        }
    });
    let vesper = best(&mut || {
        for (i, _) in &inputs {
            std::hint::black_box(write.words(i));
        }
    });
    let decode = best(&mut || {
        for (o, (_, pool)) in outputs.iter().zip(&inputs) {
            std::hint::black_box(ir_words::decode_program(o, pool));
        }
    });
    let bytes: usize = inputs.iter().map(|(i, _)| i.len()).sum();
    eprintln!(
        "place-ir cost over {} worlds ({} input MB): lower_world {lower:?}, encode {encode:?}, \
         Vesper read+write {vesper:?}, decode {decode:?}",
        inputs.len(),
        bytes / 1_000_000
    );
}

/// One phase of a swap over the lowered stages, for callgrind:
/// `PLACE_IR_PHASE` is `lower` (Rust `ir::lower_world`), `encode`,
/// `vesper` (read + write), `decode` or `none` (setup only). Collect inside
/// the phase only:
///
/// ```text
/// PLACE_IR_PHASE=vesper PLACE_IR_STEP=10 valgrind --tool=callgrind \
///   --toggle-collect='*place_ir_measured*' <test binary> --ignored \
///   --exact place_ir_callgrind --nocapture
/// ```
///
/// Add `--instr-atstart=no` and `PLACE_IR_GO=<file>` to skip simulating
/// the setup (see below); docs/parallel-results/self-host-place-ir-data.md
/// has the driver loop.
#[test]
#[ignore = "callgrind; PLACE_IR_PHASE"]
fn place_ir_callgrind() {
    let Ok(phase) = std::env::var("PLACE_IR_PHASE") else {
        return;
    };
    let write = package().entry("place_ir_words", true);
    // Lowering only (not `stages()`, whose later passes would cost hours
    // under callgrind).
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let owned: Vec<Stage> = corpus()
        .iter()
        .step_by(step())
        .filter_map(|c| {
            let w = &c.world;
            std::panic::catch_unwind(|| vesper::ffi_lowering::drops::lower_world(w))
                .ok()
                .map(|program| Stage {
                    name: c.name.clone(),
                    world: w,
                    program,
                })
        })
        .collect();
    std::panic::set_hook(quiet);
    let lowered: Vec<&Stage> = owned.iter().collect();
    let inputs: Vec<(Vec<u8>, ir_words::Pool)> = lowered
        .iter()
        .filter_map(|s| ir_words::program_input(s.world, &s.program))
        .collect();
    let outputs: Vec<Vec<u64>> = inputs.iter().map(|(i, _)| write.words(i)).collect();
    let words: usize = outputs.iter().map(Vec::len).sum();
    // With `PLACE_IR_GO=<file>`, wait for the file: a driver run with
    // `--instr-atstart=no` turns instrumentation on (`callgrind_control -i
    // on`) after the setup, which takes minutes natively, then creates it.
    if let Ok(go) = std::env::var("PLACE_IR_GO") {
        eprintln!("place-ir callgrind: ready");
        while !std::path::Path::new(&go).exists() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    match phase.as_str() {
        "lower" => place_ir_measured_lower(&lowered),
        "encode" => place_ir_measured_encode(&lowered),
        "vesper" => place_ir_measured_vesper(&write, &inputs),
        "decode" => place_ir_measured_decode(&outputs, &inputs),
        "none" => {}
        other => panic!("unknown PLACE_IR_PHASE {other}"),
    }
    eprintln!(
        "place-ir callgrind: {phase} over {} worlds ({words} output words)",
        inputs.len()
    );
}

#[inline(never)]
fn place_ir_measured_lower(lowered: &[&Stage]) {
    for s in lowered {
        std::hint::black_box(vesper::ir::lower_world(s.world));
    }
}

#[inline(never)]
fn place_ir_measured_encode(lowered: &[&Stage]) {
    for s in lowered {
        std::hint::black_box(ir_words::program_input(s.world, &s.program));
    }
}

#[inline(never)]
fn place_ir_measured_vesper(write: &Entry, inputs: &[(Vec<u8>, ir_words::Pool)]) {
    for (i, _) in inputs {
        std::hint::black_box(write.words(i));
    }
}

#[inline(never)]
fn place_ir_measured_decode(outputs: &[Vec<u64>], inputs: &[(Vec<u8>, ir_words::Pool)]) {
    for (o, (_, pool)) in outputs.iter().zip(inputs) {
        std::hint::black_box(ir_words::decode_program(o, pool));
    }
}

/// The pass harness with the identity pass on both sides agrees at every
/// boundary, and catches a real difference: `ir_opt` on the Rust side
/// against the Vesper identity must differ on some world.
/// Every `HARNESS_STEP`-th stage goes through the pass harness.
const HARNESS_STEP: usize = 16;

#[test]
fn pass_harness_agrees_on_identity_and_catches_a_pass() {
    let write = package().entry("place_ir_words", true);
    let (mut bad, mut caught, mut skipped) = (0, 0, 0);
    // The IR check runs twice per case and takes seconds on the larger
    // programs in a debug build, so every sixteenth stage (the other
    // tests already cover every stage; this one checks the harness).
    for s in stages().iter().step_by(HARNESS_STEP) {
        let same = ir_words::compare_pass(s.world, &s.program, |_| {}, |i| write.words(i));
        skipped += same.skipped as usize;
        if !same.agrees() {
            bad += 1;
            if bad <= 5 {
                eprintln!("pass harness: {} identity differs: {same:?}", s.name);
            }
        }
    }
    // `ir_opt` against the identity, on lowered programs until it has
    // changed three of them.
    for s in stages().iter().filter(|s| s.name.ends_with("@lower")) {
        if caught == 3 {
            break;
        }
        let opt = ir_words::compare_pass(
            s.world,
            &s.program,
            |p| {
                let _ = vesper::ir_opt::optimize(s.world, p);
            },
            |i| write.words(i),
        );
        caught += (!opt.agrees()) as usize;
    }
    eprintln!(
        "pass harness: {bad} of {} identity cases differ, {skipped} skipped; ir_opt vs identity differs on {caught} worlds",
        stages().len().div_ceil(HARNESS_STEP)
    );
    assert_eq!(bad, 0);
    assert!(caught > 0, "the harness never saw ir_opt change a program");
}
