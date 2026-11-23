//! Rust exit gap wave 1 lane 9 (#1506): slices, `Str`/`String` bytes and
//! `ChunkSeq` in `@vesper/wasm-backend`, run under Wasmtime against the
//! evaluator. The Rust backend (src/wasm_backend) refuses these programs.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/corpus_shard.rs"]
mod corpus_shard;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;

fn backend() -> &'static Entry {
    static BACKEND: OnceLock<Entry> = OnceLock::new();
    BACKEND.get_or_init(|| {
        Package::load("packages/vesper-wasm-backend").entry("wasm_backend_compile", true)
    })
}

fn configs() -> [WasmOptions; 2] {
    let mk = |optimize| WasmOptions {
        optimize,
        effects: EffectLowering::Portable,
        ..WasmOptions::default()
    };
    [mk(true), mk(false)]
}

/// `main` on the evaluator: its value's display, or "trap".
fn evaluate(world: &World) -> String {
    let mut r = vesper::runtime::Runtime::new(world);
    r.fuel = 10_000_000;
    match r.run("main", vec![]) {
        Ok(v) => v.to_string(),
        Err(_) => "trap".into(),
    }
}

/// Run a nullary command `main` under Wasmtime: its scalar result's text
/// (`3`, `true`, `()`), or "trap".
fn run_main(component: &[u8]) -> String {
    use wasmtime::component::{Component, Linker, Val};
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    let engine = wasmtime::Engine::new(&c).expect("engine");
    let component = match Component::new(&engine, component) {
        Ok(c) => c,
        Err(e) => return format!("load: {e}"),
    };
    let linker = Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    store.set_fuel(100_000_000).unwrap();
    let instance = match linker.instantiate(&mut store, &component) {
        Ok(i) => i,
        Err(e) => return format!("instantiate: {e}"),
    };
    let iface = instance
        .get_export_index(&mut store, None, vesper::wasm_backend::ENTRY_INTERFACE)
        .expect("entry interface");
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .expect("main export");
    let func = instance.get_func(&mut store, main).expect("main func");
    let mut results = vec![Val::Bool(false); func.ty(&store).results().len()];
    match func.call(&mut store, &[], &mut results) {
        Ok(()) => match results.first() {
            None => "()".into(),
            Some(Val::S64(x)) => x.to_string(),
            Some(Val::U64(x)) => x.to_string(),
            Some(Val::U8(x)) => x.to_string(),
            Some(Val::U32(x)) => x.to_string(),
            Some(Val::Bool(x)) => x.to_string(),
            Some(v) => format!("{v:?}"),
        },
        Err(_) => "trap".into(),
    }
}

/// The port's module for `src` under `o`, or its refusal.
fn port(src: &str, o: &WasmOptions) -> Result<(Vec<u8>, World), String> {
    let p = vesper::parser::parse(src);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = vesper::check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let program = backend_program(&c.world, o).ok_or("optimize failed")?;
    let input = wasm_input(&c.world, &program, "main", o).ok_or("not encodable")?;
    match outcome(&backend().words(&input)) {
        Outcome::Module { core, wit, .. } => Ok((wrap(core, &wit, o.world), c.world)),
        o => Err(format!("{o:?}")),
    }
}

/// Programs whose `main` runs the same under Wasmtime as on the evaluator.
/// Each uses an operation Rust refuses (`Op::Seq` or an integer
/// conversion). Indexing that can fail needs a `Fail` handler in `main`,
/// which part 2 of #1289 ports, so these use the total forms.
const SEED_PROGRAMS: &[&str] = &[
    // ChunkSeq push in a loop, a slice parameter, try_get.
    r#"fn sum(in s: Slice[u64]) -> u64 { let mut t: u64 = 0; for i in 0..s.len() { match s.try_get(i) { Option::Some(x) => { t = wrapping_add(t, x); }, Option::None => {} } } t }
fn main() -> u64 { let mut s = ChunkSeq::new[u64](); for i in 0..100 { s.push(wrapping_mul(i, i)); } wrapping_add(sum(s.as_slice()), s.len()) }"#,
    // String push_str growth, String passed as Str, try_byte_at.
    r#"fn count(in s: Str, b: u8) -> u64 { let mut n: u64 = 0; for i in 0..s.len() { match s.try_byte_at(i) { Option::Some(x) => { if x == b { n = wrapping_add(n, 1); } }, Option::None => {} } } n }
fn main() -> u64 { let mut s = String::new(); for i in 0..20 { s.push_str("ab"); s.push_str("a"); } let t = "banana"; wrapping_add(wrapping_mul(count(s, 97), 1000), count(t, 97)) }"#,
    // with_capacity growth, pop, last, truncate, clear.
    r#"fn main() -> u64 { let mut s = ChunkSeq::with_capacity[u64](2); for i in 0..10 { s.push(wrapping_add(i, 1)); }
  let mut t: u64 = 0;
  match s.pop() { Option::Some(x) => { t = wrapping_add(t, x); }, Option::None => {} }
  match s.last() { Option::Some(x) => { t = wrapping_add(t, wrapping_mul(x, 100)); }, Option::None => {} }
  s.truncate(3); t = wrapping_add(t, wrapping_mul(s.len(), 10000));
  s.clear(); match s.pop() { Option::Some(x) => { t = wrapping_add(t, 7); }, Option::None => { t = wrapping_add(t, 1000000); } }
  t }"#,
    // ChunkSeq[u8] (one-byte elements), try_set in and out of bounds, widen_u64.
    r#"fn main() -> u64 { let mut s = ChunkSeq::new[u8](); for i in 0..10 { s.push(3); }
  let ok = s.try_set(4, 9); let bad = s.try_set(40, 9);
  let mut t: u64 = 0; for i in 0..s.len() { match s.try_get(i) { Option::Some(x) => { t = wrapping_add(wrapping_mul(t, 10), widen_u64(x)); }, Option::None => {} } }
  if ok && !bad { t } else { 0 } }"#,
    // Sub-strings of a literal, Str parameters, as_bytes and byte-slice eq.
    r#"fn same(in a: Str, in b: Str) -> bool { a.as_bytes().eq(b.as_bytes()) }
fn kind(in s: Str) -> u64 { if same(s, "let") { 1 } else if same(s, "fn") { 2 } else { 3 } }
fn main() -> u64 { let src = "fn let x"; wrapping_add(wrapping_add(kind(src[0..2]), wrapping_mul(kind(src[3..6]), 10)), wrapping_mul(kind(src[7..8]), 100)) }"#,
    // push_char of 1- to 4-byte scalars and a surrogate, String ==.
    r#"fn main() -> u64 { let mut s = String::new(); let a = s.push_char(104); let b = s.push_char(233); let c = s.push_char(8364); let d = s.push_char(128512); let e = s.push_char(55296);
  let mut t = String::new(); t.push_str("h"); t.push_str("é€😀");
  let n = s.len(); if a && b && c && d && !e && s == t { n } else { 0 } }"#,
    // try_read_u64_le and try_read_u32_be on a ChunkSeq and a slice.
    r#"fn main() -> u64 { let mut s = ChunkSeq::new[u8](); for i in 0..12 { s.push(wrapping_u8(wrapping_add(i, 1))); }
  let a = match s.try_read_u64_le(1) { Option::Some(x) => x, Option::None => 0 };
  let b = match s.try_read_u64_le(5) { Option::Some(x) => x, Option::None => 77 };
  let c = match s.as_slice().try_read_u32_be(0) { Option::Some(x) => widen_u64(x), Option::None => 0 };
  wrapping_add(wrapping_add(a, b), c) }"#,
    // An inout ChunkSeq parameter that grows in the callee, wrapping_u8.
    r#"fn fill(inout s: ChunkSeq[u8], n: u64) { for i in 0..n { s.push(wrapping_u8(i)); } }
fn total(in s: Slice[u8]) -> u64 { let mut t: u64 = 0; for i in 0..s.len() { match s.try_get(i) { Option::Some(x) => { t = wrapping_add(t, widen_u64(x)); }, Option::None => {} } } t }
fn main() -> u64 { let mut s = ChunkSeq::new[u8](); fill(inout s, 30); fill(inout s, 40); wrapping_add(total(s.as_slice()), wrapping_mul(s.len(), 100000)) }"#,
    // A ChunkSeq field of a struct pushed through its place.
    r#"struct B { xs: ChunkSeq[u64], n: u64 }
fn main() -> u64 { let mut b = B { xs: ChunkSeq::new[u64](), n: 5 }; for i in 0..50 { b.xs.push(i); b.n = wrapping_add(b.n, 1); }
  let mut t: u64 = 0; for i in 0..b.xs.len() { match b.xs.try_get(i) { Option::Some(x) => { t = wrapping_add(t, x); }, Option::None => {} } } wrapping_add(t, wrapping_mul(b.n, 1000)) }"#,
    // replace of a ChunkSeq, swap of two locals.
    r#"fn main() -> u64 { let mut s = ChunkSeq::new[u64](); s.push(1); s.push(2); let old = replace(inout s, ChunkSeq::new[u64]()); s.push(10);
  let mut a: u64 = 3; let mut b: u64 = 4; swap(inout a, inout b);
  wrapping_add(wrapping_add(wrapping_mul(old.len(), 100), s.len()), wrapping_add(wrapping_mul(a, 1000), wrapping_mul(b, 10000))) }"#,
    // A ChunkSeq of structs.
    r#"struct P { x: u64, y: u64 }
fn main() -> u64 { let mut s = ChunkSeq::new[P](); for i in 0..20 { s.push(P { x: i, y: wrapping_mul(i, 2) }); }
  let mut t: u64 = 0; for i in 0..s.len() { match s.try_get(i) { Option::Some(p) => { t = wrapping_add(t, wrapping_add(p.x, p.y)); }, Option::None => {} } } t }"#,
    // A literal pushed to in a loop (copied, not written), clear, push_str.
    r#"fn main() -> u64 { let mut total: u64 = 0; for i in 0..3 { let mut t = "ab"; t.push_str("c"); total = wrapping_add(total, t.len()); }
  let a = "xy"; let mut b = String::new(); b.push_str(a); b.push_str("cd"); b.clear(); b.push_str("q");
  wrapping_add(wrapping_add(total, wrapping_mul(a.len(), 10)), wrapping_mul(b.len(), 100)) }"#,
    // Atomic operations, which the port intercepts (one thread).
    r#"struct Job { chunk: u64, total: Atomic[u64] }
fn add(in job: Job, i: u64) { job.total.fetch_add(i, MemoryOrder::Relaxed); }
fn main() -> u64 { let job = Job { chunk: 1000, total: Atomic::new(0) }; for i in 0..4 { add(job, i); } job.total.load(MemoryOrder::SeqCst) }"#,
    // A `Str` view sliced off a char boundary faults, as the evaluator does.
    r#"fn n(in s: Str) -> u64 { s.len() }
fn cut(in s: Str) -> u64 { n(s[1..2]) }
fn main() -> u64 { cut("aéb") }"#,
    // A `Str` view sliced on char boundaries.
    r#"fn n(in s: Str) -> u64 { s.len() }
fn cut(in s: Str) -> u64 { n(s[1..3]) }
fn main() -> u64 { cut("aéb") }"#,
    // An atomic update in tail position.
    r#"fn main() -> u64 { let a: Atomic[u64] = Atomic::new(5); a.fetch_add(1, MemoryOrder::Relaxed) }"#,
    // A temporary mutable view passed `inout`.
    r#"fn bump(inout v: Slice[u64]) { for i in 0..v.len() { let ok = v.try_set(i, wrapping_add(i, 7)); } }
fn main() -> u64 { let mut s = ChunkSeq::new[u64](); s.push(1); s.push(2); s.push(3); bump(inout s.as_slice_mut());
  let mut t: u64 = 0; for i in 0..s.len() { match s.try_get(i) { Option::Some(x) => { t = wrapping_add(wrapping_mul(t, 10), x); }, Option::None => {} } } t }"#,
    // Ranged fill on bytes and a subslice of words, including both invalid
    // interval shapes, an empty interval and the checked form's handler.
    r#"fn words(inout s: Slice[u64]) -> bool {
  let a = s.try_fill(1, 3, 42); let b = s.try_fill(2, 1, 99);
  let c = s.try_fill(1, 4, 99); let d = s.try_fill(3, 3, 99);
  a && !b && !c && d
}
fn main() -> u64 {
  let mut s = ChunkSeq::new[u64](); s.resize(5, 1);
  let mut b = ChunkSeq::new[u8](); b.resize(5, 2);
  let ok = words(inout s[1..4]);
  let fault = handle Fail[RuntimeError] in { b.fill(1, 4, 3); b.fill(4, 6, 9); 0 } with { return v => v, raise(e, k) => 1, };
  let mut d: u64 = fault;
  for i in 0..5 { match s.try_get(i) { Option::Some(x) => { d = wrapping_add(wrapping_mul(d, 31), x); }, Option::None => {} }
                  match b.try_get(i) { Option::Some(x) => { d = wrapping_add(wrapping_mul(d, 31), widen_u64(x)); }, Option::None => {} } }
  if ok { d } else { 0 }
}"#,
];

#[test]
fn sequences_and_strings_run_like_the_evaluator() {
    let o = &configs()[0];
    let shard = corpus_shard::Shard::from_environment();
    for (i, src) in shard.keep(SEED_PROGRAMS.iter().enumerate()) {
        let p = vesper::parser::parse(src);
        let c = vesper::check::check(&p.module);
        assert!(c.diagnostics.is_empty(), "#{i}: {:?}", c.diagnostics);
        assert!(
            matches!(
                vesper::wasm_backend::compile(&c.world, "main", o),
                Err(vesper::wasm_backend::WasmError::Unsupported {
                    construct: vesper::wasm_backend::Unsupported::Operation,
                    ..
                })
            ),
            "#{i}: Rust now lowers this; compare byte for byte instead"
        );
        let (component, world) = port(src, o).unwrap_or_else(|e| panic!("#{i}: {e}\n{src}"));
        let (got, expected) = (run_main(&component), evaluate(&world));
        assert_eq!(got, expected, "#{i}: wasm vs evaluator\n{src}");
    }
}

#[test]
fn str_literals_return_packed_views() {
    let source = r#"
fn word(flag: bool) -> Str {
    if flag { "open" } else { "é" }
}
fn main() -> u64 {
    let ascii = word(true);
    let unicode = word(false);
    let byte = match unicode.try_byte_at(1) {
        Option::Some(b) => widen_u64(b),
        Option::None => 0,
    };
    wrapping_add(wrapping_mul(ascii.as_bytes().len(), 1000),
                 wrapping_add(wrapping_mul(unicode.len(), 100), byte))
}
"#;
    for options in configs() {
        let (component, world) = port(source, &options).expect("compile Str literals");
        assert_eq!(evaluate(&world), "4369");
        assert_eq!(run_main(&component), "4369");
    }
}

#[test]
fn mutable_element_views_are_refused_only_when_reached() {
    if !corpus_shard::Shard::from_environment().contains(8) {
        return;
    }
    let prefix = r#"fn set(inout x: u64) { x = 3; }
fn by_index(inout s: ChunkSeq[u64]) eff [Fail[RuntimeError]] { set(inout s[0]); }
"#;
    for o in configs() {
        let unused = format!("{prefix}fn main() -> u64 {{ 7 }}");
        let (component, _) = port(&unused, &o).expect("unused mutable views are encodable");
        assert_eq!(run_main(&component), "7");

        let source = format!(
            "{prefix}fn main() -> u64 {{ let mut s = ChunkSeq::new[u64](); s.push(1); handle Fail[RuntimeError] in {{ by_index(inout s); }} with {{ return v => v, raise(e, k) => (), }}; s.len() }}"
        );
        let Err(error) = port(&source, &o) else {
            panic!("port accepted a reachable mutable index view: by_index");
        };
        assert!(error.contains("Invalid"), "{error}");
    }
}

#[test]
fn try_get_mut_runs_like_the_evaluator() {
    if !corpus_shard::Shard::from_environment().contains(9) {
        return;
    }
    let source = r#"
struct Pair { a: u64, b: u64 }
fn set(inout x: u64) { x = 3; }
fn main() -> u64 {
    let mut s = ChunkSeq::new[u64]();
    s.push(1);
    s.push(10);
    match s.try_get_mut(0) {
        Option::Some(x) => set(inout x),
        Option::None => {},
    }
    let mut ps = ChunkSeq::new[Pair]();
    ps.push(Pair { a: 4, b: 5 });
    match ps.try_get_mut(0) {
        Option::Some(p) => { p.b = 9; },
        Option::None => {},
    }
    let old = match ps.try_get_mut(0) {
        Option::Some(p) => replace(inout p, Pair { a: 20, b: 30 }),
        Option::None => Pair { a: 0, b: 0 },
    };
    let oob = match ps.try_get_mut(5) {
        Option::Some(p) => p.a,
        Option::None => 7,
    };
    let s0 = match s.try_get(0) { Option::Some(v) => v, Option::None => 0 };
    let p0_sum = match ps.try_get(0) {
        Option::Some(v) => wrapping_add(wrapping_mul(v.a, 10), v.b),
        Option::None => 0,
    };
    wrapping_add(
        wrapping_add(wrapping_mul(s0, 10000), wrapping_mul(old.b, 1000)),
        wrapping_add(p0_sum, oob)
    )
}
"#;
    for o in configs() {
        let (component, world) = port(source, &o).expect("compile try_get_mut");
        let expected = evaluate(&world);
        assert_ne!(expected, "trap");
        assert_eq!(run_main(&component), expected);
    }
}

#[test]
fn narrow_integers_run_like_the_evaluator() {
    if !corpus_shard::Shard::from_environment().contains(10) {
        return;
    }
    let programs: &[&str] = &[
        r#"fn main() -> u64 {
    let a: i8 = -100;
    let b: i8 = 30;
    let c: i8 = wrapping_add(a, b);
    let d: i8 = wrapping_mul(a, 2);
    let e: i8 = ~a;
    let f: i8 = -(-12);
    let g: i16 = -20000;
    let h: i16 = wrapping_sub(g, 15000);
    let i: u16 = 50000;
    let j: u16 = wrapping_add(i, 20000);
    let k: i32 = -1000000000;
    let l: i32 = wrapping_mul(k, 3);
    let m: i32 = rotate_left(k, 5);
    let n: i32 = rotate_right(k, 5);
    let mut acc: u64 = 0;
    acc = wrapping_add(acc, wrapping_u64(widen_i64(c)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(d)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(e)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(f)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(h)));
    acc = wrapping_add(wrapping_mul(acc, 31), widen_u64(j));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(l)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(m)));
    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(widen_i64(n)));
    acc
}"#,
        r#"fn div_i32(x: i32, y: i32) -> i32 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x / y }
fn rem_i32(x: i32, y: i32) -> i32 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x % y }
fn neg_i8(x: i8) -> i8 eff [Fail[ArithOverflow]] { -x }
fn main() -> u64 {
    let q = handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in { div_i32(-7, 2) } with { return v => v, raise(e, k) => 99, }
    } with { return v => v, raise(e, k) => 88, };
    let r = handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in { rem_i32(-7, 2) } with { return v => v, raise(e, k) => 99, }
    } with { return v => v, raise(e, k) => 88, };
    let ov = handle Fail[ArithOverflow] in { neg_i8(-128) } with { return v => widen_i64(v), raise(e, k) => 777, };
    let x32: i32 = -5;
    let y32: i32 = 2;
    let u16a: u16 = 50000;
    let u16b: u16 = 100;
    let cmp: u64 = if x32 < y32 && u16a > u16b { 1 } else { 0 };
    wrapping_add(wrapping_mul(wrapping_u64(widen_i64(q)), 10000),
                 wrapping_add(wrapping_mul(wrapping_u64(widen_i64(r)), 100),
                              wrapping_add(wrapping_u64(ov), cmp)))
}"#,
    ];
    for (idx, source) in programs.iter().enumerate() {
        for (k, o) in configs().iter().enumerate() {
            let (component, world) = port(source, o).unwrap_or_else(|e| panic!("[{idx}/{k}] {e}"));
            let expected = evaluate(&world);
            assert_ne!(expected, "trap", "[{idx}/{k}]");
            assert_eq!(run_main(&component), expected, "[{idx}/{k}]");
        }
    }
}

#[test]
fn i128_runs_like_the_evaluator() {
    if !corpus_shard::Shard::from_environment().contains(11) {
        return;
    }
    let programs: &[&str] = &[
        r#"fn main() -> u64 {
    let a: i128 = -170141183460469231731687303715884105728;
    let b: i128 = 123456789012345678901234567890;
    let c: i128 = -98765432109876543210;
    let mut h: u64 = 0;
    let s1 = wrapping_add(b, c);
    let s2 = wrapping_sub(c, b);
    let p = wrapping_mul(b, c);
    let neg_a = wrapping_neg_i128(a);
    let neg_c = wrapping_neg_i128(c);
    let shl1 = wrapping_shl_i128(c, 65);
    let shr1 = wrapping_shr_i128(c, 5);
    let rotl = rotate_left(b, 64);
    let rotr = rotate_right(b, 19);
    let bit = (b & c) | (b ^ ~c);
    let mut cmp: u64 = 0;
    if a < c { cmp = wrapping_add(cmp, 1); }
    if c > a { cmp = wrapping_add(cmp, 2); }
    if c <= c { cmp = wrapping_add(cmp, 4); }
    if b >= c { cmp = wrapping_add(cmp, 8); }
    if !(c < a) { cmp = wrapping_add(cmp, 16); }
    h = wrapping_add(h, wrapping_u64(s1));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(s2));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(p));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(neg_a));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(neg_c));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(shl1));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(shr1));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(rotl));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(rotr));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(bit));
    wrapping_add(wrapping_mul(h, 31), cmp)
}"#,
        r#"fn div_i(x: i128, y: i128) -> i128 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x / y }
fn rem_i(x: i128, y: i128) -> i128 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x % y }
fn go() -> u64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let x: i128 = -100000000000000000000000007;
    let y: i128 = 1000000000000000003;
    let q1 = div_i(x, y);
    let r1 = rem_i(x, y);
    let q2 = div_i(x, -y);
    let r2 = rem_i(x, -y);
    let s = match checked_add_i128(q1, q2) { Option::Some(v) => v, Option::None => 0 };
    let d = match checked_sub_i128(r1, r2) { Option::Some(v) => v, Option::None => 0 };
    let m1: i64 = 1000000000000;
    let m2: i64 = -700000000000;
    let m = match checked_mul_i128(widen_i128(m1), widen_i128(m2)) { Option::Some(v) => v, Option::None => 0 };
    let c1 = (q1 + q2) - (r1 * r2);
    let mut h: u64 = wrapping_u64(s);
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(d));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(m));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(c1));
    h = wrapping_add(wrapping_mul(h, 31), wrapping_u64(x >> 67));
    h
}
fn main() -> u64 {
    let ok = handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in { go() } with { return v => v, raise(e, k) => 555, }
    } with { return v => v, raise(e, k) => 444, };
    let ov_min = handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            let m: i128 = -170141183460469231731687303715884105728;
            let _q = div_i(m, -1);
            0
        } with { return v => v, raise(e, k) => 222, }
    } with { return v => v, raise(e, k) => 333, };
    wrapping_add(ok, ov_min)
}"#,
    ];
    for (idx, source) in programs.iter().enumerate() {
        for (k, o) in configs().iter().enumerate() {
            let (component, world) = port(source, o).unwrap_or_else(|e| panic!("[{idx}/{k}] {e}"));
            let expected = evaluate(&world);
            assert_ne!(expected, "trap", "[{idx}/{k}]");
            assert_eq!(run_main(&component), expected, "[{idx}/{k}]");
        }
    }
}

/// `u128` as the port lowers it (a 16-byte block, #1508), against the
/// evaluator. Rust refuses every one (`IntegerWidth`).
const WIDE_PROGRAMS: &[&str] = &[
    // `<<` below, at and past 64 bits, including 0.
    r#"fn hi(x: u128) -> u64 eff [Fail[ArithOverflow]] { wrapping_u64(x / 18446744073709551616) }
fn mix(a: u64, x: u128) -> u64 eff [Fail[ArithOverflow]] { wrapping_add(wrapping_mul(a, 1000003), wrapping_add(wrapping_u64(x), wrapping_mul(hi(x), 7919))) }
fn sh(x: u128, k: u128) -> u128 eff [Fail[ArithOverflow]] { x << k }
fn go() -> u64 eff [Fail[ArithOverflow]] { let one = widen_u128(1); let x = widen_u128(12345678901234567); let mut a: u64 = 0;
  a = mix(a, sh(x, widen_u128(0))); a = mix(a, sh(x, widen_u128(1))); a = mix(a, sh(sh(one, widen_u128(63)), widen_u128(1)));
  a = mix(a, sh(x, widen_u128(63))); a = mix(a, sh(x, widen_u128(64))); a = mix(a, sh(x, widen_u128(100))); a }
fn main() -> u64 { handle Fail[ArithOverflow] in { go() } with { return v => v, raise(e, k) => 999, } }"#,
    // OR in both words, logical right shifts across their boundary, and
    // checked shift counts at and above the width.
    r#"fn mix(a: u64, v: u128) -> u64 eff [Fail[ArithOverflow]] {
  let hi = wrapping_u64(v / 18446744073709551616);
  wrapping_add(wrapping_mul(a, 31), wrapping_add(wrapping_mul(hi, 17), wrapping_u64(v)))
}
fn shr(v: u128, k: u128) -> u128 eff [Fail[ArithOverflow]] { v >> k }
fn handled(v: u128, k: u128) -> u128 {
  handle Fail[ArithOverflow] in { shr(v, k) } with { return v => v, raise(e, k) => 7, }
}
fn go() -> u64 eff [Fail[ArithOverflow]] {
  let a = widen_u128(9223372036854775809);
  let b = widen_u128(4611686018427388418);
  let u = ((a << 64) | b) | ((b << 64) | a);
  let mut d = mix(1, shr(u, 0)); d = mix(d, shr(u, 1)); d = mix(d, shr(u, 63));
  d = mix(d, shr(u, 64)); d = mix(d, shr(u, 65)); d = mix(d, shr(u, 127));
  d = mix(d, handled(u, 128)); d = mix(d, handled(u, 18446744073709551616));
  wrapping_add(d, widen_u64(wrapping_u8(shr(u, 8))))
}
fn main() -> u64 { handle Fail[ArithOverflow] in { go() } with { return v => v, raise(e, k) => 999, } }"#,
    // A literal pattern on a runtime `u128`, add with carry, mul, div, rem, compare.
    r#"fn class(x: u128) -> u64 { match x { 0 => 1, 1 => 7, 18446744073709551616 => 11, _ => 9 } }
fn f(a: u128, b: u128) -> u64 eff [Fail[ArithOverflow]] { let s = wrapping_add(a, b); let m = wrapping_mul(a, b);
  let q = m / 1000000007; let r = m % 1000000007;
  let c: u64 = if a < b { 1 } else if a > b { 2 } else { 3 };
  wrapping_add(wrapping_add(wrapping_u64(s), wrapping_u64(q)), wrapping_add(wrapping_mul(wrapping_u64(r), 3), c)) }
fn go() -> u64 eff [Fail[ArithOverflow]] { let big = widen_u128(18446744073709551615);
  wrapping_add(wrapping_add(class(widen_u128(1)), class(widen_u128(0))), wrapping_add(class(wrapping_add(big, widen_u128(1))),
  wrapping_add(class(widen_u128(5)), wrapping_mul(f(big, big), 13)))) }
fn main() -> u64 { handle Fail[ArithOverflow] in { go() } with { return v => v, raise(e, k) => 999, } }"#,
];

#[test]
fn u128_runs_like_the_evaluator() {
    let o = &configs()[0];
    for (i, src) in WIDE_PROGRAMS.iter().enumerate() {
        let p = vesper::parser::parse(src);
        let c = vesper::check::check(&p.module);
        assert!(c.diagnostics.is_empty(), "#{i}: {:?}", c.diagnostics);
        assert!(
            vesper::wasm_backend::compile(&c.world, "main", o).is_err(),
            "#{i}: Rust now lowers this; compare byte for byte instead"
        );
        let (component, world) = port(src, o).unwrap_or_else(|e| panic!("#{i}: {e}\n{src}"));
        let (got, expected) = (run_main(&component), evaluate(&world));
        assert_eq!(got, expected, "#{i}: wasm vs evaluator\n{src}");
    }
}

/// The programs of `WASM_SEED_PROBE` (a file, or a directory of `.ves`
/// files): the port's answer and the run against the evaluator.
#[test]
#[ignore]
fn probe() {
    let path = std::path::PathBuf::from(std::env::var("WASM_SEED_PROBE").expect("WASM_SEED_PROBE"));
    let mut files = vec![];
    if path.is_dir() {
        for e in std::fs::read_dir(&path).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "ves") {
                files.push(p);
            }
        }
        files.sort();
    } else {
        files.push(path);
    }
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap();
        for (k, o) in configs().iter().enumerate() {
            match port(&src, o) {
                Ok((component, world)) => eprintln!(
                    "{} [{k}]: wasm {} evaluator {}",
                    file.display(),
                    run_main(&component),
                    evaluate(&world)
                ),
                Err(e) => eprintln!("{} [{k}]: refused {e}", file.display()),
            }
        }
    }
}

/// A compiler package built to wasm (lane 9 acceptance): a program over
/// `@vesper/place-ir` (the place-IR reader and writer, and `@vesper/check-data`
/// under it) compiles through the port, handlers and bit operators included,
/// and runs like the evaluator. Rust refuses it (bit operators).
#[test]
fn place_ir_package_builds_and_runs() {
    // One package: shard 0 runs it.
    if !corpus_shard::Shard::from_environment().contains(0) {
        return;
    }
    compare_package(
        "place-ir",
        "use @vesper/place-ir::{ place_ir_round_trip };\n\
         fn main() -> u64 {\n\
         \x20   let s = \"\";\n\
         \x20   let words = place_ir_round_trip(in s.as_bytes());\n\
         \x20   words.len()\n\
         }\n",
    );
}

/// Compile the compiler's dynamic-dispatch checker and run its trait and
/// function-object lowering on real source. Hash every output word so the
/// comparison covers its diagnostics and the complete lowered AST.
#[test]
fn dyn_checker_package_builds_and_runs() {
    if !corpus_shard::Shard::from_environment().contains(4) {
        return;
    }
    compare_package(
        "check-dyn",
        r#"use @vesper/check-dyn::{ check_dyn };
fn main() -> u64 {
    let words = check_dyn(in "trait T { fn f(in self) -> i64; } fn g(in x: dyn T) -> i64 { x.f() }", 1000000, 1);
    let mut digest: u64 = 1;
    for i in 0..words.len() {
        match words.try_get(i) {
            Option::Some(v) => { digest = wrapping_add(wrapping_mul(digest, 31), v); },
            Option::None => {}
        }
    }
    digest
}"#,
    );
}

fn compare_package(name: &str, source: &str) {
    let dep = std::fs::canonicalize(format!("packages/vesper-{name}")).expect("package");
    let dir = std::env::temp_dir().join(format!("wasm-seed-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).expect("source dir");
    std::fs::write(
        dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@wasm-seed/{name}-main\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\"@vesper/{name}\" = {{ version = \"^0.1\", path = {:?} }}\n",
            dep.display().to_string()
        ),
    )
    .expect("Loom.toml");
    std::fs::write(dir.join("src/main.ves"), source).expect("main.ves");
    let assembled = vesper::packages::load(&dir).expect("load");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let c = vesper::check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let expected = evaluate(&c.world);
    assert_ne!(expected, "trap", "{name} evaluator");
    for (k, o) in configs().iter().enumerate() {
        assert!(
            vesper::wasm_backend::compile(&c.world, "main", o).is_err(),
            "[{k}] Rust now lowers {name}; compare byte for byte instead"
        );
        let program = backend_program(&c.world, o).expect("optimize");
        let input = wasm_input(&c.world, &program, "main", o).expect("encodable");
        match outcome(&backend().words(&input)) {
            Outcome::Module { core, wit, .. } => {
                let component = wrap(core, &wit, o.world);
                assert_eq!(run_main(&component), expected, "[{k}] wasm vs evaluator");
            }
            other => panic!("[{k}] port {other:?}"),
        }
    }
    std::fs::remove_dir_all(&dir).expect("remove package dir");
}
