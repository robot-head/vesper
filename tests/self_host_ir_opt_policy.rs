//! Rust-exit gap wave 1 lane 12 (#1510): `@vesper/ir-opt-policy`
//! (packages/vesper-ir-opt-policy) against `ir_opt_policy` (src/ir_opt_policy.rs).
//!
//! The inputs are tests/self_host_ir_opt.rs's: every `IR_OPT_STEP`-th corpus
//! world's native-path IR, the census IR mutants and the `@fip` cases.
//!
//! - `screen_matches_rust`: per block, the package's screen keeps exactly
//!   the blocks `ir_opt_policy::block_can_improve` keeps.
//! - `policy_matches_rust`: the package's `policy_words` (the screen, then
//!   @vesper/ir-opt's forwarding on the kept blocks and `@fip` reuse) with
//!   the Rust tail of `optimize` (`ir::thread_tokens`, `ir_check::validate`)
//!   gives `ir_opt_policy::optimize`'s count or error and program.
//! - `policy_cost` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA of the package
//!   against the Rust policy, and against @vesper/ir-opt without the screen.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/corpus_shard.rs"]
mod corpus_shard;
#[path = "common/ir_opt_words.rs"]
mod ir_opt_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use ir_opt_words::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ir::Program;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ir-opt-policy"))
}

fn policy() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("ir_opt_policy_entry", true))
}

fn screen() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("ir_opt_policy_screen_entry", true))
}

fn unscreened() -> &'static Entry {
    static P: OnceLock<Package> = OnceLock::new();
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| {
        P.get_or_init(|| Package::load("packages/vesper-ir-opt"))
            .entry("ir_opt_entry", true)
    })
}

/// Every `n`-th corpus input (`IR_OPT_STEP`, default 24), as in
/// tests/self_host_ir_opt.rs: Rust's egglog takes most of the time.
fn step() -> usize {
    std::env::var("IR_OPT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(24)
}

struct Input {
    name: String,
    world: &'static World,
    program: Program,
}

/// The native input of every corpus world `keep` accepts, then their
/// mutants. Lowering the whole corpus and its mutants took over 13 GiB, and
/// the kernel killed a Bazel shard that held it all, so each test lowers only
/// the worlds it checks (as `ir_cases_where`, #1913).
fn inputs_where(keep: &dyn Fn(usize) -> bool) -> Vec<Input> {
    let mut out = vec![];
    for (i, c) in corpus().iter().enumerate() {
        if keep(i) {
            out.push(Input {
                name: c.name.clone(),
                world: &c.world,
                program: vesper::ir_opt_policy::native_input(&c.world),
            });
        }
    }
    for c in ir_cases_where(1, keep) {
        if c.name.contains("#m") {
            out.push(Input {
                name: c.name,
                world: c.world,
                program: c.program,
            });
        }
    }
    out
}

/// The inputs of every `step()`-th world, for the passes that run egglog.
fn inputs() -> Vec<Input> {
    let step = step();
    inputs_where(&|i| i % step == 0)
}

#[test]
fn screen_matches_rust() {
    screen();
    let (mut done, mut bad, mut blocks, mut kept) = (0, 0, 0, 0);
    // Bazel shards the corpus worlds (BUILD.bazel `test_shards`).
    let shard = corpus_shard::Shard::from_environment();
    for c in &inputs_where(&|i| shard.contains(i)) {
        let Some(prep) = prepare(c.world, &c.program, MODE_OPTIMIZE) else {
            continue;
        };
        done += 1;
        let words = screen().words(&prep.input);
        let rust: Vec<u64> = c
            .program
            .functions
            .values()
            .flat_map(|f| {
                f.blocks
                    .iter()
                    .map(|b| u64::from(vesper::ir_opt_policy::block_can_improve(f, b)))
            })
            .collect();
        blocks += rust.len();
        kept += rust.iter().sum::<u64>();
        let ours = &words[1..];
        let ours: Vec<u64> = ours
            .iter()
            .copied()
            .chain(std::iter::repeat(0))
            .take(rust.len())
            .collect();
        if ours != rust || words[0] != rust.iter().sum::<u64>() {
            bad += 1;
            if bad <= 5 {
                let at = rust.iter().zip(&ours).position(|(a, b)| a != b);
                eprintln!("{} differs at block row {at:?}", c.name);
            }
        }
    }
    eprintln!("screen: {bad} of {done} inputs differ; {kept} of {blocks} blocks kept");
    let least = corpus_shard::Shard::from_environment().share(50);
    assert!(done > least, "only {done} inputs");
    assert_eq!(bad, 0);
}

fn same_result(rust: &Result<usize, String>, ours: &Result<usize, String>) -> bool {
    match (rust, ours) {
        (Err(a), Err(b)) if b == REFUSAL_PREFIX => a.starts_with(b.as_str()),
        // The policy validates per part: only the text of a validation
        // error can differ from the whole pass's.
        (Err(a), Err(b)) if a.starts_with(VALIDATION) => b.starts_with(VALIDATION),
        _ => rust == ours,
    }
}

const VALIDATION: &str = "optimized IR failed ownership validation: ";

#[test]
fn policy_matches_rust() {
    policy();
    let quiet = quiet_panics();
    let (mut done, mut bad, mut skipped) = (0, 0, 0);
    let all = inputs();
    for c in corpus_shard::Shard::from_environment().keep(&all) {
        let rust = std::panic::catch_unwind(|| {
            let mut p = c.program.clone();
            let r = vesper::ir_opt_policy::optimize(c.world, &mut p);
            (r, p)
        });
        let Ok((rr, rp)) = rust else {
            skipped += 1;
            continue;
        };
        let Some(prep) = prepare(c.world, &c.program, MODE_OPTIMIZE) else {
            skipped += 1;
            continue;
        };
        done += 1;
        let words = policy().words(&prep.input);
        let (vr, vp) = finish(c.world, &prep, &words, MODE_OPTIMIZE);
        let mut diff = None;
        if !same_result(&rr, &vr) {
            diff = Some(format!("result rust {rr:?} vesper {vr:?}"));
        } else if rr.is_ok()
            && let Some(vp) = &vp
            && let Some(d) = first_difference(&format!("{rp:#?}"), &format!("{vp:#?}"))
        {
            diff = Some(d);
        }
        if let Some(d) = diff {
            bad += 1;
            if bad <= 5 {
                eprintln!("{} differs: {d}", c.name);
            }
        }
    }
    drop(quiet);
    eprintln!("policy: {bad} of {done} inputs differ ({skipped} skipped)");
    // The slice is every `step()`-th world and its mutants; shards take
    // four-input blocks of it.
    let least = corpus_shard::Shard::from_environment().share(12);
    assert!(done > least, "only {done} inputs");
    assert_eq!(bad, 0);
}

/// Mutate the field written between two reads. Forwarding must account for
/// the exact place even when two fields have the same scalar type.
#[test]
fn projected_writes_match_the_rust_policy() {
    for field in ["left", "right"] {
        let source = format!(
            "copy struct Pair {{ left: u64, right: u64 }}
             fn update(inout p: Pair, n: u64) -> u64 {{
                 let first = p.left;
                 p.{field} = n;
                 wrapping_add(first, p.left)
             }}"
        );
        let parsed = vesper::parser::parse(&source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let world = &checked.world;
        let mut rust = vesper::ir_opt_policy::native_input(world);
        let prep = prepare(world, &rust, MODE_OPTIMIZE).unwrap();
        let count = vesper::ir_opt_policy::optimize(world, &mut rust).unwrap();
        let words = policy().words(&prep.input);
        let (result, program) = finish(world, &prep, &words, MODE_OPTIMIZE);
        assert_eq!(result, Ok(count), "write to {field}");
        assert_eq!(
            format!("{:#?}", program.unwrap()),
            format!("{rust:#?}"),
            "write to {field}"
        );
    }
}

/// The package against the Rust policy and against @vesper/ir-opt without
/// the screen, ABBA, encoding and the Rust tail included on both Vesper
/// sides.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn policy_cost() {
    if !speed() {
        return;
    }
    let all = inputs();
    let cases: Vec<(&Input, Prepared)> = all
        .iter()
        .filter(|c| !c.name.contains("#m"))
        .filter(|c| vesper::ir_opt_policy::optimize(c.world, &mut c.program.clone()).is_ok())
        .filter_map(|c| prepare(c.world, &c.program, MODE_OPTIMIZE).map(|p| (c, p)))
        .collect();
    let mut rust = || {
        time(|| {
            for (c, _) in &cases {
                let mut p = c.program.clone();
                std::hint::black_box(vesper::ir_opt_policy::optimize(c.world, &mut p).ok());
            }
        })
    };
    let run = |e: &Entry| {
        time(|| {
            for (c, _) in &cases {
                let prep = prepare(c.world, &c.program, MODE_OPTIMIZE).expect("prepared");
                let words = e.words(&prep.input);
                let _ = std::hint::black_box(finish(c.world, &prep, &words, MODE_OPTIMIZE));
            }
        })
    };
    let mut ves = || run(policy());
    let (r, v) = abba("ir_opt_policy", &mut rust, &mut ves);
    let mut whole = || run(unscreened());
    let mut ves = || run(policy());
    let (u, s) = abba("screen", &mut whole, &mut ves);
    eprintln!(
        "ir_opt_policy over {} worlds: rust policy {r:?}, vesper policy {v:?}; \
         vesper without the screen {u:?}, with it {s:?}",
        cases.len()
    );
}
