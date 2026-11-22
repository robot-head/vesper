//! SH5 lane 8 (#1331): `World::mode` memoizes ADT expansions per world and
//! gives exactly the original algorithm's answers
//! (`World::mode_reference`).
//!
//! - `corpus_modes_match_the_reference`: every type the checker recorded
//!   for a corpus world, every signature and field type (under its
//!   generics' declared bounds and under each uniform bound), and every ADT
//!   instantiated with copy, affine and linear arguments. One world answers
//!   all of them in turn, so later queries are served from the memo.
//! - `random_worlds_match_the_reference`: generated worlds of mutually
//!   recursive generic ADTs with declared modes and field modalities, the
//!   shapes where a path-dependent cut could leak into the memo. Each world
//!   is queried in two orders.
//! - `bench_world_mode` (ignored): reference against memoized time over the
//!   corpus queries, used by docs/parallel-results/self-host-world-mode-speed.md.
#[path = "common/check_harness.rs"]
mod check_harness;

use std::collections::BTreeMap;
use vesper::ast::{Mode, Type};
use vesper::check::World;

const MODES: [Mode; 3] = [Mode::Copy, Mode::Affine, Mode::Linear];

/// A copy, an affine and a linear stand-in type for arguments. Unknown names
/// are linear, so `__linear_arg` is.
fn argument_types() -> [Type; 3] {
    [
        Type::named("i64"),
        Type::named("String"),
        Type::named("__linear_arg"),
    ]
}

/// A type and the bounds of its type parameters.
type Query = (Type, BTreeMap<String, Mode>);

/// The queries for one world.
fn queries(world: &World) -> Vec<Query> {
    let mut out = vec![];
    for t in world.types.values() {
        out.push((t.clone(), BTreeMap::new()));
    }
    let uniform = |names: &[String], m: Mode| -> BTreeMap<String, Mode> {
        names.iter().map(|n| (n.clone(), m)).collect()
    };
    for f in world.functions.values() {
        let names: Vec<String> = f.generics.iter().map(|g| g.name.clone()).collect();
        let declared: BTreeMap<String, Mode> = f
            .generics
            .iter()
            .map(|g| (g.name.clone(), g.bound))
            .collect();
        let tys = f.params.iter().map(|p| &p.ty).chain([&f.result]);
        for t in tys {
            out.push((t.clone(), declared.clone()));
            for m in MODES {
                out.push((t.clone(), uniform(&names, m)));
            }
        }
    }
    for a in world.adts.values() {
        let names: Vec<String> = a.generics.iter().map(|g| g.name.clone()).collect();
        for m in MODES {
            for field in a.variants.iter().flat_map(|v| &v.fields) {
                out.push((field.ty.clone(), uniform(&names, m)));
            }
        }
        for arg in argument_types() {
            let args = vec![arg; a.generics.len()];
            out.push((Type::Named(a.name.clone(), args), BTreeMap::new()));
        }
        // Mixed arguments: the i-th parameter gets the i-th stand-in.
        let args = (0..a.generics.len())
            .map(|i| argument_types()[i % 3].clone())
            .collect();
        out.push((Type::Named(a.name.clone(), args), BTreeMap::new()));
    }
    out
}

fn assert_same(world: &World, qs: &[Query], what: &str) -> usize {
    let mut n = 0;
    for (t, bounds) in qs {
        let expected = world.mode_reference(t, bounds);
        assert_eq!(
            world.mode(t, bounds),
            expected,
            "{what}: mode of {t} under {bounds:?}"
        );
        n += 1;
    }
    n
}

#[test]
fn corpus_modes_match_the_reference() {
    // Every n-th corpus world (`WORLD_MODE_STEP`, default 1 in release
    // builds and 3 in debug ones).
    let step = std::env::var("WORLD_MODE_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 3 } else { 1 })
        .max(1);
    let corpus: Vec<_> = check_harness::corpus().iter().step_by(step).collect();
    assert!(!corpus.is_empty());
    let (mut total, mut memo) = (0, 0);
    for case in &corpus {
        let world = case.world.clone();
        let qs = queries(&world);
        total += assert_same(&world, &qs, &case.name);
        // The same queries again, now from the memo, and backwards on a
        // fresh clone so that different expansions are recorded first.
        assert_same(&world, &qs, &case.name);
        let fresh = case.world.clone();
        assert_eq!(fresh.mode_cache_len(), 0, "a clone starts empty");
        let backwards: Vec<_> = qs.iter().rev().cloned().collect();
        assert_same(&fresh, &backwards, &case.name);
        memo += world.mode_cache_len();
    }
    println!(
        "{} worlds, {total} queries, {memo} memoized expansions",
        corpus.len()
    );
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const PARAMS: [&str; 2] = ["T", "U"];

fn random_type(rng: &mut Rng, arity: &[usize], generics: usize, depth: u32) -> String {
    let leaf = |rng: &mut Rng| -> String {
        match rng.below(4) {
            0 => "i64".into(),
            1 => "String".into(),
            2 if generics > 0 => PARAMS[rng.below(generics as u64) as usize].into(),
            _ => "u8".into(),
        }
    };
    if depth == 0 {
        return leaf(rng);
    }
    match rng.below(8) {
        0 | 1 => leaf(rng),
        2 => format!("ChunkSeq[{}]", random_type(rng, arity, generics, depth - 1)),
        3 => format!(
            "({}, {})",
            random_type(rng, arity, generics, depth - 1),
            random_type(rng, arity, generics, depth - 1)
        ),
        _ => {
            let j = rng.below(arity.len() as u64) as usize;
            if arity[j] == 0 {
                format!("A{j}")
            } else {
                let args: Vec<String> = (0..arity[j])
                    .map(|_| random_type(rng, arity, generics, depth - 1))
                    .collect();
                format!("A{j}[{}]", args.join(", "))
            }
        }
    }
}

fn random_world(seed: u64) -> (World, Vec<Query>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let n = 2 + rng.below(4) as usize;
    let arity: Vec<usize> = (0..n).map(|_| rng.below(3) as usize).collect();
    let mut source = String::new();
    for (i, &k) in arity.iter().enumerate() {
        let mode = ["copy ", "affine ", "linear ", ""][rng.below(4) as usize];
        let generics = if k == 0 {
            String::new()
        } else {
            format!("[{}]", PARAMS[..k].join(", "))
        };
        let fields: Vec<String> = (0..1 + rng.below(3))
            .map(|f| {
                let ty = random_type(&mut rng, &arity, k, 2);
                let modality = match rng.below(6) {
                    0 => " @@ copy",
                    1 => " @@ linear",
                    2 => " @@ affine",
                    _ => "",
                };
                format!("f{f}: {ty}{modality}")
            })
            .collect();
        source.push_str(&format!(
            "{mode}struct A{i}{generics} {{ {} }}\n",
            fields.join(", ")
        ));
    }
    let module = vesper::parser::parse(&source).module;
    let (world, _) = World::collect(&module);
    assert!(world.adts.len() >= n, "{source}");
    let mut qs = queries(&world);
    for _ in 0..20 {
        let ty = vesper::parser::parse(&format!(
            "fn q(x: {}) {{}}",
            random_type(&mut rng, &arity, 2, 3)
        ))
        .module;
        let (w, _) = World::collect(&ty);
        if let Some(f) = w.functions.get("q") {
            let bounds = PARAMS
                .iter()
                .map(|p| (p.to_string(), MODES[rng.below(3) as usize]))
                .collect();
            qs.push((f.params[0].ty.clone(), bounds));
        }
    }
    (world, qs)
}

#[test]
fn random_worlds_match_the_reference() {
    let mut total = 0;
    for seed in 0..400 {
        let (world, qs) = random_world(seed);
        total += assert_same(&world, &qs, &format!("seed {seed}"));
        let fresh = world.clone();
        let backwards: Vec<_> = qs.iter().rev().cloned().collect();
        assert_same(&fresh, &backwards, &format!("seed {seed}, backwards"));
    }
    println!("400 random worlds, {total} queries");
}

#[test]
#[ignore = "timing for the SH5 lane 8 report"]
fn bench_world_mode() {
    let corpus = check_harness::corpus();
    let all: Vec<_> = corpus
        .iter()
        .map(|case| (case.world.clone(), queries(&case.world)))
        .collect();
    let count: usize = all.iter().map(|(_, q)| q.len()).sum();
    for round in 0..3 {
        let start = std::time::Instant::now();
        let mut sum = 0u64;
        for (w, qs) in &all {
            for (t, b) in qs {
                sum += w.mode_reference(t, b) as u64;
            }
        }
        let reference = start.elapsed();
        let start = std::time::Instant::now();
        let mut memo_sum = 0u64;
        for (w, qs) in &all {
            let w = w.clone();
            for (t, b) in qs {
                memo_sum += w.mode(t, b) as u64;
            }
        }
        let memo = start.elapsed();
        assert_eq!(sum, memo_sum);
        println!(
            "round {round}: {count} queries, reference {reference:?}, memoized {memo:?} (fresh memo per world)"
        );
    }
}
