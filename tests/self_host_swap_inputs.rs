//! Self-hosting SH4 lane 2 (#1276): swap inputs that cross once per
//! compilation.
//!
//! - `vesper::swap_input`: scopes, slots, nesting and re-entrancy.
//! - The encode-once inputs of the checker swaps (`WorldLog` in
//!   tests/common/check_words.rs): each corpus world goes to one Vesper
//!   context in parts, the way the hybrid build sends a compilation's
//!   world, and every answer must equal the Rust pass: the layout of every
//!   type the world mentions (each type's part carries only what the
//!   context lacks), and the IR check of the world's program and its
//!   mutants (one context, the program changing between calls).
//! - `swap_input_costs` (ignored, `CHECK_HARNESS_SPEED=1`): each swap's
//!   input cost before and after, beside the Rust pass it replaces.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/corpus_shard.rs"]
mod corpus_shard;

use check_harness::*;
use check_words::{Cursor, WorldLog};
use std::sync::OnceLock;
use std::time::Duration;
use vesper::ast::Type;
use vesper::check::World;
use vesper::swap_input;

fn layout_package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-check-layout"))
}

fn ir_package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ir-check"))
}

/// The `check_*_parts` input: a count, then each part's byte length and
/// bytes, padded to words.
fn pack(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (parts.len() as u64).to_le_bytes().to_vec();
    for p in parts {
        out.extend((p.len() as u64).to_le_bytes());
        out.extend(p);
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    out
}

/// The `check_*_parts` result: each part's answer.
fn unpack(words: &[u64]) -> Vec<&[u64]> {
    let mut out = vec![];
    let mut i = 0;
    while i < words.len() {
        let n = words[i] as usize;
        out.push(&words[i + 1..i + 1 + n]);
        i += 1 + n;
    }
    out
}

/// Every `n`-th corpus world (`SWAP_INPUTS_STEP`, default `default`).
fn step(default: usize) -> usize {
    std::env::var("SWAP_INPUTS_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

// ------------------------------------------------------------------ scopes

#[test]
fn scopes_hold_one_slot_per_world_and_type() {
    let a = World::default();
    let b = World::default();
    // Before `enable`, nothing is kept (the default compiler).
    {
        let _s = swap_input::open(&a);
        if !swap_input::enabled() {
            assert!(swap_input::with(&a, || 1u32, |x| *x).is_none());
        }
    }
    swap_input::enable();
    assert!(swap_input::with(&a, || 1u32, |x| *x).is_none(), "no scope");
    {
        let _s = swap_input::open(&a);
        assert!(swap_input::is_open(&a) && !swap_input::is_open(&b));
        let mut inits = 0;
        for _ in 0..3 {
            let got = swap_input::with(
                &a,
                || {
                    inits += 1;
                    0u32
                },
                |x| {
                    *x += 1;
                    *x
                },
            );
            assert!(got.is_some());
        }
        assert_eq!(inits, 1);
        assert_eq!(swap_input::with(&a, || 0u32, |x| *x), Some(3));
        // Another slot type is its own slot.
        assert_eq!(swap_input::with(&a, || 7u64, |x| *x), Some(7));
        // A nested scope of the same world shares the slots.
        {
            let _inner = swap_input::open(&a);
            assert_eq!(swap_input::with(&a, || 0u32, |x| *x), Some(3));
        }
        assert_eq!(swap_input::with(&a, || 0u32, |x| *x), Some(3));
        // A nested scope of another world has its own, and the outer
        // world's stay reachable.
        {
            let _other = swap_input::open(&b);
            assert_eq!(swap_input::with(&b, || 10u32, |x| *x), Some(10));
            assert_eq!(swap_input::with(&a, || 0u32, |x| *x), Some(3));
        }
        assert!(!swap_input::is_open(&b));
        // A re-entrant use of a slot falls back instead of aliasing it.
        let inner = swap_input::with(&a, || 0u32, |_| swap_input::with(&a, || 0u32, |x| *x));
        assert_eq!(inner, Some(None));
    }
    assert!(!swap_input::is_open(&a));
    // Closing the scope dropped the slots: a new scope starts afresh.
    let _s = swap_input::open(&a);
    assert_eq!(swap_input::with(&a, || 0u32, |x| *x), Some(0));
}

/// Slots are dropped when their scope closes (the hybrid's slots free
/// Vesper values there).
#[test]
fn closing_a_scope_drops_its_slots() {
    use std::rc::Rc;
    swap_input::enable();
    let w = World::default();
    let alive = Rc::new(());
    {
        let _s = swap_input::open(&w);
        let held = alive.clone();
        swap_input::with(&w, move || held, |_| ()).unwrap();
        assert_eq!(Rc::strong_count(&alive), 2);
    }
    assert_eq!(Rc::strong_count(&alive), 1);
}

/// Scopes of two worlds dropped out of order each close their own world:
/// the world whose scope closed first has no slots left, and the other
/// world keeps its slots until its own scope closes.
#[test]
fn scopes_dropped_out_of_order_close_their_own_world() {
    swap_input::enable();
    let a = World::default();
    let b = World::default();
    let sa = swap_input::open(&a);
    assert_eq!(swap_input::with(&a, || 1u32, |x| *x), Some(1));
    let sb = swap_input::open(&b);
    assert_eq!(swap_input::with(&b, || 2u32, |x| *x), Some(2));
    drop(sa);
    assert!(!swap_input::is_open(&a), "a's own frame closed");
    assert!(swap_input::with(&a, || 9u32, |x| *x).is_none());
    assert!(swap_input::is_open(&b));
    assert_eq!(swap_input::with(&b, || 0u32, |x| *x), Some(2));
    drop(sb);
    assert!(!swap_input::is_open(&b));
}

// ------------------------------------------------------------------ layout

/// One world's layout parts: every type of the world, then each again
/// (the repeats carry no world data).
fn layout_parts(w: &World, types: &[Type]) -> (Vec<Vec<u8>>, usize) {
    let mut log = WorldLog::new(w);
    let mut cur = Cursor::default();
    let mut parts = vec![];
    let mut repeat_bytes = 0;
    for (round, t) in types.iter().chain(types).enumerate() {
        let part = log
            .layout_part(w, &mut cur, t)
            .expect("encodable corpus type");
        if round >= types.len() {
            repeat_bytes += part.len();
        }
        parts.push(part);
    }
    (parts, repeat_bytes)
}

#[test]
fn layout_parts_match_rust() {
    let f = layout_package().entry("check_layout_parts", true);
    let (mut bad, mut checked, mut errors) = (0, 0, 0);
    let shard = corpus_shard::Shard::from_environment();
    for c in shard.keep(corpus().iter().step_by(step(1))) {
        let types = world_types(&c.world);
        let (parts, repeat_bytes) = layout_parts(&c.world, &types);
        // A repeated type sends its name-free part: 9 words of empty
        // sections and its index.
        assert_eq!(repeat_bytes, types.len() * 72, "{}", c.name);
        let words = f.words(&pack(&parts));
        let answers = unpack(&words);
        assert_eq!(answers.len(), parts.len(), "{}", c.name);
        for (t, words) in types.iter().chain(&types).zip(answers) {
            let vesper = check_words::decode_layout(words);
            let rust = vesper::layout::rust_layout_of(&c.world, t);
            checked += 1;
            errors += rust.is_err() as usize;
            if format!("{vesper:?}") != format!("{rust:?}") {
                bad += 1;
                if bad <= 3 {
                    eprintln!("{} {t}:\n  rust:   {rust:?}\n  vesper: {vesper:?}", c.name);
                }
            }
        }
    }
    eprintln!("layout parts: {bad} of {checked} layouts differ ({errors} errors)");
    assert!(checked > shard.share(1000), "only {checked} layouts");
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------------ IR check

/// Every `step`-th world's program and its mutants, as parts of one
/// context per world.
#[test]
fn ir_check_parts_match_rust() {
    let f = ir_package().entry("check_ir_parts", true);
    let cases = ir_cases(IR_MUTANTS);
    let shard = corpus_shard::Shard::from_environment();
    let mut stepped = 0;
    let (mut bad, mut checked, mut diags) = (0, 0, 0);
    let mut worlds = 0;
    let mut i = 0;
    while i < cases.len() {
        let w = cases[i].world;
        let mut j = i;
        while j < cases.len() && std::ptr::eq(cases[j].world, w) {
            j += 1;
        }
        let group = &cases[i..j];
        i = j;
        worlds += 1;
        if (worlds - 1) % step(5) != 0 {
            continue;
        }
        stepped += 1;
        if !shard.contains(stepped - 1) {
            continue;
        }
        let mut log = WorldLog::new(w);
        let mut cur = Cursor::default();
        // Programs the encoder refuses (element-view worlds, as on #1232's
        // head) fall back to Rust in the hybrid; skip them here.
        let (group, parts): (Vec<_>, Vec<Vec<u8>>) = group
            .iter()
            .filter_map(|c| log.ir_part(w, &mut cur, &c.program).map(|p| (c, p)))
            .unzip();
        if group.is_empty() {
            continue;
        }
        let words = f.words(&pack(&parts));
        let answers = unpack(&words);
        assert_eq!(answers.len(), group.len());
        for (c, words) in group.iter().zip(answers) {
            let vesper = check_words::decode_diagnostics(words);
            let rust = vesper::ir_check::rust_validate(w, &c.program);
            checked += 1;
            diags += rust.len();
            if vesper != rust {
                bad += 1;
                if bad <= 3 {
                    eprintln!("{}:\n  rust:   {rust:?}\n  vesper: {vesper:?}", c.name);
                }
            }
        }
    }
    eprintln!("ir check parts: {bad} of {checked} programs differ ({diags} Rust diagnostics)");
    assert!(
        diags > shard.share(100),
        "the mutants produced only {diags} diagnostics"
    );
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------------ costs

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The input cost of each checker swap before and after, beside the Rust
/// pass, over every corpus world: `Layout::of` of every type the world
/// mentions (the per-call input against one log per world), and the IR
/// check of each world's program (the whole world section against the lean
/// one, and against a log whose world was already sent).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn swap_input_costs() {
    if !speed() {
        return;
    }
    let cases: Vec<(&World, Vec<Type>)> = corpus()
        .iter()
        .map(|c| (&c.world, world_types(&c.world)))
        .collect();
    let calls: usize = cases.iter().map(|(_, t)| t.len()).sum();
    let best = |f: &mut dyn FnMut() -> Duration| (0..5).map(|_| f()).min().unwrap();

    let rust = best(&mut || {
        time(|| {
            for (w, ts) in &cases {
                for t in ts {
                    std::hint::black_box(vesper::layout::rust_layout_of(w, t).ok());
                }
            }
        })
    });
    let mut per_call_bytes = 0;
    let before = best(&mut || {
        per_call_bytes = 0;
        time(|| {
            for (w, ts) in &cases {
                for t in ts {
                    let input = check_words::layout_input(w, t).unwrap();
                    per_call_bytes += input.len();
                    std::hint::black_box(input);
                }
            }
        })
    });
    let mut part_bytes = 0;
    let after = best(&mut || {
        part_bytes = 0;
        time(|| {
            for (w, ts) in &cases {
                let mut log = WorldLog::new(w);
                let mut cur = Cursor::default();
                for t in ts {
                    let part = log.layout_part(w, &mut cur, t).unwrap();
                    part_bytes += part.len();
                    std::hint::black_box(part);
                }
            }
        })
    });
    // Vesper's side: one JIT call per type decoding its own input, against
    // one call per world reading the parts into one context.
    let per_call = layout_package().entry("check_layout_words", true);
    let parts_entry = layout_package().entry("check_layout_parts", true);
    let per_call_inputs: Vec<Vec<Vec<u8>>> = cases
        .iter()
        .map(|(w, ts)| {
            ts.iter()
                .map(|t| check_words::layout_input(w, t).unwrap())
                .collect()
        })
        .collect();
    let packed: Vec<Vec<u8>> = cases
        .iter()
        .map(|(w, ts)| {
            let mut log = WorldLog::new(w);
            let mut cur = Cursor::default();
            pack(
                &ts.iter()
                    .map(|t| log.layout_part(w, &mut cur, t).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let vesper_before = best(&mut || {
        time(|| {
            for inputs in &per_call_inputs {
                for i in inputs {
                    std::hint::black_box(per_call.words(i));
                }
            }
        })
    });
    let vesper_after = best(&mut || {
        time(|| {
            for p in &packed {
                std::hint::black_box(parts_entry.words(p));
            }
        })
    });
    eprintln!(
        "layout, {calls} calls: rust {:.2} ms; input before {:.2} ms ({:.2}x of rust, {} KiB), \
         after {:.2} ms ({:.2}x, {} KiB); vesper with decode before {:.2} ms, after {:.2} ms",
        ms(rust),
        ms(before),
        before.as_secs_f64() / rust.as_secs_f64(),
        per_call_bytes / 1024,
        ms(after),
        after.as_secs_f64() / rust.as_secs_f64(),
        part_bytes / 1024,
        ms(vesper_before),
        ms(vesper_after),
    );

    let programs: Vec<(&World, vesper::ir::Program)> = corpus()
        .iter()
        .map(|c| (&c.world, vesper::ir::lower_world(&c.world)))
        .filter(|(w, p)| check_words::ir_input(w, p).is_some())
        .collect();
    let rust = best(&mut || {
        time(|| {
            for (w, p) in &programs {
                std::hint::black_box(vesper::ir_check::rust_validate(w, p));
            }
        })
    });
    let full = best(&mut || {
        time(|| {
            for (w, p) in &programs {
                std::hint::black_box(check_words::ir_input_full(w, p).unwrap());
            }
        })
    });
    let lean = best(&mut || {
        time(|| {
            for (w, p) in &programs {
                std::hint::black_box(check_words::ir_input(w, p).unwrap());
            }
        })
    });
    // A second call in the same compilation: the world is already in the
    // context, so only the program (and new names and types) crosses.
    let mut logs: Vec<(WorldLog, Cursor)> = programs
        .iter()
        .map(|(w, p)| {
            let mut log = WorldLog::new(w);
            let mut cur = Cursor::default();
            log.ir_part(w, &mut cur, p).unwrap();
            (log, cur)
        })
        .collect();
    let again = best(&mut || {
        time(|| {
            for ((w, p), (log, cur)) in programs.iter().zip(&mut logs) {
                std::hint::black_box(log.ir_part(w, cur, p).unwrap());
            }
        })
    });
    let (fb, lb): (usize, usize) = programs
        .iter()
        .map(|(w, p)| {
            (
                check_words::ir_input_full(w, p).unwrap().len(),
                check_words::ir_input(w, p).unwrap().len(),
            )
        })
        .fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
    let ir_entry = ir_package().entry("check_ir_words", true);
    let full_inputs: Vec<Vec<u8>> = programs
        .iter()
        .map(|(w, p)| check_words::ir_input_full(w, p).unwrap())
        .collect();
    let lean_inputs: Vec<Vec<u8>> = programs
        .iter()
        .map(|(w, p)| check_words::ir_input(w, p).unwrap())
        .collect();
    let vesper_full = best(&mut || {
        time(|| {
            full_inputs
                .iter()
                .for_each(|i| drop(std::hint::black_box(ir_entry.words(i))))
        })
    });
    let vesper_lean = best(&mut || {
        time(|| {
            lean_inputs
                .iter()
                .for_each(|i| drop(std::hint::black_box(ir_entry.words(i))))
        })
    });
    eprintln!(
        "ir check, {} programs: rust {:.2} ms; input whole world {:.2} ms ({:.2}x, {} KiB), \
         lean {:.2} ms ({:.2}x, {} KiB), second call in a scope {:.2} ms ({:.2}x); \
         vesper with decode whole {:.2} ms, lean {:.2} ms",
        programs.len(),
        ms(rust),
        ms(full),
        full.as_secs_f64() / rust.as_secs_f64(),
        fb / 1024,
        ms(lean),
        lean.as_secs_f64() / rust.as_secs_f64(),
        lb / 1024,
        ms(again),
        again.as_secs_f64() / rust.as_secs_f64(),
        ms(vesper_full),
        ms(vesper_lean),
    );
}
