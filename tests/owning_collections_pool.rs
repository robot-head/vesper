//! The census state pool (#1130 gap C5, self-hosting SH2 lane 1, #1173):
//! blocks of dataflow states, each owning sorted rows, updated in place
//! through element views (`pool_views`), against the census workaround
//! (`pool_flat`: one append-only pool of rows with a start and length per
//! block, loaded by copying into scratch) and against Rust's `Vec<State>`.
//!
//! The transfer of block `b` is the sorted union of the states of its two
//! predecessors (`b - 1` around the ring and `(7b + 3) mod blocks`), with
//! one row generated and one killed. Blocks are visited in order for a fixed
//! number of rounds, so later blocks see this round's states of earlier ones.
//!
//! `pool_agrees_with_vec_state` runs both shapes in the evaluator, the JIT
//! (optimizer on and off, with leak counters) against the Rust model. The
//! ignored tests time them (ABBA) and run each once for callgrind:
//!
//! ```text
//! cargo test --release --test owning_collections_pool -- --ignored --nocapture --test-threads=1
//! valgrind --tool=callgrind --callgrind-out-file=pool.out \
//!   target/release/deps/owning_collections_pool-* --ignored --exact once_for_callgrind
//! callgrind_annotate --inclusive=yes pool.out | grep run_
//! ```
use std::time::{Duration, Instant};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

const SOURCE: &str = r#"// Blocks of dataflow states that own sorted rows.
struct State { rows: ChunkSeq[u32] }

fn mask() -> u64 { 511 }

fn gen_of(b: u64) -> u32 { wrapping_u32(wrapping_mul(b, 2654435761) & mask()) }

fn kill_of(b: u64) -> u32 { wrapping_u32(wrapping_add(wrapping_mul(b, 40503), 17) & mask()) }

fn second_pred(b: u64, blocks: u64) -> u64 { wrapping_add(wrapping_mul(b, 7), 3) & wrapping_sub(blocks, 1) }

fn row(in s: ChunkSeq[u32], i: u64) -> u32 {
    match s.try_get(i) { Some(v) => v, None => 4294967295 }
}

// The sorted union of `a` and `b` with `g` added and `k` removed.
fn transfer(in a: ChunkSeq[u32], in b: ChunkSeq[u32], g: u32, k: u32) -> ChunkSeq[u32] {
    let mut out = ChunkSeq::with_capacity[u32](wrapping_add(wrapping_add(a.len(), b.len()), 1));
    let mut i: u64 = 0;
    let mut j: u64 = 0;
    let mut gen_left = true;
    loop {
        let x = row(in a, i);
        let y = row(in b, j);
        let z = if gen_left { g } else { 4294967295 };
        let m = if x < y { x } else { y };
        let m = if z < m { z } else { m };
        if m == 4294967295 { break; }
        if x == m { i = wrapping_add(i, 1); }
        if y == m { j = wrapping_add(j, 1); }
        if z == m { gen_left = false; }
        if m != k { out.push(m); }
    }
    out
}

fn checksum(acc: u64, in rows: ChunkSeq[u32]) -> u64 {
    let mut h = wrapping_add(wrapping_mul(acc, 31), rows.len());
    let mut i: u64 = 0;
    loop {
        if !(i < rows.len()) { break; }
        h = wrapping_add(wrapping_mul(h, 3), widen_u64(row(in rows, i)));
        i = wrapping_add(i, 1);
    }
    h
}

// The state pool on element views: `states[b]` owns its rows.
pub fn pool_views(blocks: u64, rounds: u64) -> u64 {
    let mut states = ChunkSeq::with_capacity[State](blocks);
    let mut b: u64 = 0;
    loop {
        if !(b < blocks) { break; }
        states.push(State { rows: ChunkSeq::new[u32]() });
        b = wrapping_add(b, 1);
    }
    let mut r: u64 = 0;
    loop {
        if !(r < rounds) { break; }
        b = 0;
        loop {
            if !(b < blocks) { break; }
            let p1 = wrapping_add(b, wrapping_sub(blocks, 1)) & wrapping_sub(blocks, 1);
            let p2 = second_pred(b, blocks);
            let next = match states.try_get(p1) {
                Some(s1) => match states.try_get(p2) {
                    Some(s2) => transfer(in s1.rows, in s2.rows, gen_of(b), kill_of(b)),
                    None => ChunkSeq::new[u32](),
                },
                None => ChunkSeq::new[u32](),
            };
            match states.try_get_mut(b) {
                Some(s) => { let _old = replace(inout s.rows, next); },
                None => {},
            }
            b = wrapping_add(b, 1);
        }
        r = wrapping_add(r, 1);
    }
    let mut h: u64 = 0;
    b = 0;
    loop {
        if !(b < blocks) { break; }
        h = match states.try_get(b) { Some(s) => checksum(h, in s.rows), None => h };
        b = wrapping_add(b, 1);
    }
    h
}

// The census workaround: every stored state's rows go into one append-only
// pool, with a start and length per block, and a state is loaded by copying
// its rows out into a scratch sequence.
struct Pool { rows: ChunkSeq[u32], start: ChunkSeq[u64], len: ChunkSeq[u64] }

fn word(in s: ChunkSeq[u64], i: u64) -> u64 {
    match s.try_get(i) { Some(v) => v, None => 0 }
}

fn pool_load(in p: Pool, b: u64, inout out: ChunkSeq[u32]) {
    out.clear();
    let a = word(in p.start, b);
    let n = word(in p.len, b);
    out.extend(in p.rows[a..wrapping_add(a, n)]);
}

fn pool_store(inout p: Pool, b: u64, in s: ChunkSeq[u32]) {
    let ok = p.start.try_set(b, p.rows.len());
    let ok2 = p.len.try_set(b, s.len());
    p.rows.extend(in s.as_slice());
}

pub fn pool_flat(blocks: u64, rounds: u64) -> u64 {
    let mut p = Pool { rows: ChunkSeq::new[u32](), start: ChunkSeq::new[u64](), len: ChunkSeq::new[u64]() };
    p.start.resize(blocks, 0);
    p.len.resize(blocks, 0);
    let mut s1 = ChunkSeq::new[u32]();
    let mut s2 = ChunkSeq::new[u32]();
    let mut r: u64 = 0;
    loop {
        if !(r < rounds) { break; }
        let mut b: u64 = 0;
        loop {
            if !(b < blocks) { break; }
            let p1 = wrapping_add(b, wrapping_sub(blocks, 1)) & wrapping_sub(blocks, 1);
            let p2 = second_pred(b, blocks);
            pool_load(in p, p1, inout s1);
            pool_load(in p, p2, inout s2);
            let next = transfer(in s1, in s2, gen_of(b), kill_of(b));
            pool_store(inout p, b, in next);
            b = wrapping_add(b, 1);
        }
        r = wrapping_add(r, 1);
    }
    let mut h: u64 = 0;
    let mut b: u64 = 0;
    loop {
        if !(b < blocks) { break; }
        pool_load(in p, b, inout s1);
        h = checksum(h, in s1);
        b = wrapping_add(b, 1);
    }
    h
}

fn main() -> u64 { pool_views(32, 3) }
"#;

/// `Vec<State>` in Rust, the same algorithm.
struct State {
    rows: Vec<u32>,
}

fn transfer(a: &[u32], b: &[u32], g: u32, k: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len() + 1);
    let (mut i, mut j, mut gen_left) = (0, 0, true);
    loop {
        let x = a.get(i).copied().unwrap_or(u32::MAX);
        let y = b.get(j).copied().unwrap_or(u32::MAX);
        let z = if gen_left { g } else { u32::MAX };
        let m = x.min(y).min(z);
        if m == u32::MAX {
            break;
        }
        if x == m {
            i += 1;
        }
        if y == m {
            j += 1;
        }
        if z == m {
            gen_left = false;
        }
        if m != k {
            out.push(m);
        }
    }
    out
}

#[inline(never)]
fn run_rust(blocks: u64, rounds: u64) -> u64 {
    let n = blocks as usize;
    let mut states: Vec<State> = (0..n).map(|_| State { rows: vec![] }).collect();
    for _ in 0..rounds {
        for b in 0..n {
            let p1 = (b + n - 1) & (n - 1);
            let p2 = (b.wrapping_mul(7) + 3) & (n - 1);
            let g = ((b as u64).wrapping_mul(2654435761) & 511) as u32;
            let k = ((b as u64).wrapping_mul(40503).wrapping_add(17) & 511) as u32;
            let next = transfer(&states[p1].rows, &states[p2].rows, g, k);
            let _old = std::mem::replace(&mut states[b].rows, next);
        }
    }
    states.iter().fold(0u64, |acc, s| {
        let h = acc.wrapping_mul(31).wrapping_add(s.rows.len() as u64);
        s.rows
            .iter()
            .fold(h, |h, &v| h.wrapping_mul(3).wrapping_add(u64::from(v)))
    })
}

fn checked() -> check::Checked {
    let p = parser::parse(SOURCE);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn with_main(body: &str) -> check::Checked {
    let source = SOURCE.replace("fn main() -> u64 { pool_views(32, 3) }", body);
    let p = parser::parse(&source);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

#[test]
fn pool_agrees_with_vec_state() {
    for (blocks, rounds) in [(8, 2), (32, 3)] {
        let want = run_rust(blocks, rounds).to_string();
        for shape in ["pool_views", "pool_flat"] {
            let c = with_main(&format!(
                "fn main() -> u64 {{ {shape}({blocks}, {rounds}) }}"
            ));
            let evaluated = Runtime::new(&c.world)
                .run("main", vec![])
                .unwrap()
                .to_string();
            assert_eq!(evaluated, want, "{shape}({blocks}, {rounds}): evaluator");
            for optimize in [true, false] {
                let stray = native_runtime::stray_frees();
                let jit = native::jit_with_options(&c.world, "main", optimize)
                    .unwrap()
                    .to_string();
                assert_eq!(jit, want, "{shape}({blocks}, {rounds}): JIT {optimize}");
                assert_eq!(
                    native_runtime::teardown_frees(),
                    0,
                    "{shape}: JIT (optimize {optimize}) left sequences for teardown"
                );
                assert_eq!(native_runtime::stray_frees(), stray, "{shape}: stray frees");
            }
        }
    }
}

struct Bench {
    views: native::WordFunction,
    flat: native::WordFunction,
}

fn bench() -> Bench {
    let c = checked();
    Bench {
        views: native::WordFunction::compile(&c.world, "pool_views", true).unwrap(),
        flat: native::WordFunction::compile(&c.world, "pool_flat", true).unwrap(),
    }
}

const BLOCKS: u64 = 256;
const ROUNDS: u64 = 8;

#[inline(never)]
fn run_views(b: &Bench) -> u64 {
    // SAFETY: two u64 arguments.
    unsafe { b.views.call(&[BLOCKS, ROUNDS]) }.unwrap()
}

#[inline(never)]
fn run_flat(b: &Bench) -> u64 {
    // SAFETY: two u64 arguments.
    unsafe { b.flat.call(&[BLOCKS, ROUNDS]) }.unwrap()
}

fn time(mut f: impl FnMut()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore = "benchmark"]
fn abba_against_vec_state() {
    let b = bench();
    let want = run_rust(BLOCKS, ROUNDS);
    assert_eq!(run_views(&b), want);
    assert_eq!(run_flat(&b), want);
    let (mut r, mut v, mut f) = (vec![], vec![], vec![]);
    for _ in 0..9 {
        r.push(time(|| {
            std::hint::black_box(run_rust(BLOCKS, ROUNDS));
        }));
        v.push(time(|| {
            std::hint::black_box(run_views(&b));
        }));
        f.push(time(|| {
            std::hint::black_box(run_flat(&b));
        }));
        f.push(time(|| {
            std::hint::black_box(run_flat(&b));
        }));
        v.push(time(|| {
            std::hint::black_box(run_views(&b));
        }));
        r.push(time(|| {
            std::hint::black_box(run_rust(BLOCKS, ROUNDS));
        }));
    }
    let (r, v, f) = (median(r), median(v), median(f));
    let ratio = |d: Duration| d.as_secs_f64() / r.as_secs_f64();
    println!("state pool, {BLOCKS} blocks x {ROUNDS} rounds:");
    println!("  rust Vec<State> {r:?}");
    println!("  vesper views    {v:?} ({:.2}x)", ratio(v));
    println!("  vesper flat     {f:?} ({:.2}x)", ratio(f));
}

#[test]
#[ignore = "benchmark"]
fn once_for_callgrind() {
    let b = bench();
    let want = run_rust(BLOCKS, ROUNDS);
    assert_eq!(run_views(&b), want);
    assert_eq!(run_flat(&b), want);
}
