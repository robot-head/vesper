//! Rust exit gap wave 1 lane 9 (#1506): slices, `Str`/`String` bytes and
//! `ChunkSeq` in `@vesper/wasm-backend`, run under Wasmtime against the
//! evaluator. The Rust backend (src/wasm_backend) refuses these programs.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-wasm-backend"))
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
    let f = package().entry("wasm_backend_compile", true);
    match outcome(&f.words(&input)) {
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
];

#[test]
fn sequences_and_strings_run_like_the_evaluator() {
    let o = &configs()[0];
    for (i, src) in SEED_PROGRAMS.iter().enumerate() {
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
