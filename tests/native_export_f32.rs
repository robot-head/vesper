//! `f32` across the export boundary (#1087): scalars as parameters and
//! results in float registers (the platform C ABI), `Option[f32]`,
//! `Slice[f32]`/`ChunkSeq[f32]` (copied, as an `f32` takes a whole word in a
//! sequence buffer) and `f32` mirror fields, in v1, v2 and v3 libraries.
#![allow(dead_code, unused_imports)]
use vesper::native::export::{
    self, AbiType, BindingOptions, LibraryOptions, compile_source, rust_binding,
};
use vesper::{check, native, parser, runtime::Runtime};

const WIDEN: &str = include_str!("../crates/vesper-port-pilot/vesper/widen.ves");

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

/// Runs `main` (appended to the pilot's widening library) in the evaluator
/// and in the JIT with the optimizer on and off, which must agree and free
/// every box they make; `crates/vesper-port-pilot/tests/widen.rs` checks the
/// same values through the linked library object.
fn agree(main: &str) -> String {
    let w = world(&format!("{WIDEN}\n{main}"));
    let mut results = vec![];
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        let value = native::jit_with_options(&w, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            vesper::fip::native_runtime::alloc_count(),
            vesper::fip::native_runtime::free_count(),
            "optimize {optimize}: boxes leaked or freed twice"
        );
        results.push(value);
    }
    assert_eq!(
        results[0], results[1],
        "optimized and unoptimized JIT disagree"
    );
    let evaluated = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    assert_eq!(evaluated, results[0], "the evaluator and the JIT disagree");
    results.remove(0)
}

fn options(library: &'static str, target: Option<&'static str>) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target,
        optimize: true,
    }
}

/// The library object for every export target, and its interface.
fn widen() -> export::LibraryInterface {
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        let lib = compile_source("widen.ves", WIDEN, &options("widen", Some(target)))
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        assert!(!lib.object.is_empty());
    }
    compile_source("widen.ves", WIDEN, &options("widen", None))
        .unwrap()
        .interface
}

fn binding(interface: &export::LibraryInterface) -> String {
    rust_binding(interface, &BindingOptions::default())
}

fn refused(source: &str) -> String {
    match compile_source("t.ves", source, &options("t", None)) {
        Ok(_) => panic!("accepted:\n{source}"),
        Err(e) => e.to_string(),
    }
}

fn bits(x: f32) -> String {
    u64::from(x.to_bits()).to_string()
}

#[test]
fn f32_values_agree_with_ieee_single() {
    // The pilot's tests/widen.rs expects the same values from the object.
    let scale = agree("fn main() -> u64 { widen_u64(to_bits_f32(scale(0.1, 3.0))) }");
    assert_eq!(scale, bits(0.1f32 * 3.0 + 0.5));
    let halve = agree(
        "fn main() -> u64 { match halve(3.0) { Option::Some(x) => widen_u64(to_bits_f32(x)), Option::None => 0 } }",
    );
    assert_eq!(halve, bits(1.5));
    let total = agree(
        "fn main() -> u64 {
    let mut xs = ChunkSeq::new[f32]();
    xs.push(1.0);
    xs.push(2.5);
    xs.push(-0.5);
    widen_u64(to_bits_f32(total(in xs.as_slice())))
}",
    );
    assert_eq!(total, bits(3.0));
    let ramp = agree(
        "fn main() -> u64 {
    let r = ramp(4, 0.25);
    let none: f32 = 0.0;
    let last = match r.try_get(3) { Option::Some(x) => x, Option::None => none };
    wrapping_add(wrapping_mul(r.len(), 1000000000000), widen_u64(to_bits_f32(last)))
}",
    );
    assert_eq!(
        ramp,
        (4_000_000_000_000 + u64::from(0.75f32.to_bits())).to_string()
    );
    let pt = agree(
        "fn main() -> u64 {
    let mut p = Pt { x: -1.5, y: 2.0, tag: 7 };
    let n = pt_norm1(in p);
    pt_nudge(inout p, 0.5);
    wrapping_add(widen_u64(to_bits_f32(n)), wrapping_add(widen_u64(to_bits_f32(p.x)), p.tag))
}",
    );
    assert_eq!(
        pt,
        (u64::from(3.5f32.to_bits()) + u64::from((-1.0f32).to_bits()) + 8).to_string()
    );
}

#[test]
fn f32_crosses_as_a_float_in_every_binding() {
    let i = widen();
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    let scale = f("scale");
    assert!(
        scale
            .params
            .iter()
            .all(|p| !p.pointer && p.ty == AbiType::Scalar { name: "f32".into() })
    );
    assert!(!scale.result_pointer);
    let text = binding(&i);
    for line in [
        "fn vesper_widen_scale(x: f32, k: f32) -> f32;",
        "fn vesper_widen_total(xs: *mut u64) -> f32;",
        "fn vesper_widen_pt_norm1(p: *mut u64) -> f32;",
        "pub fn scale(x: f32, k: f32) -> f32 {",
        "pub fn halve(x: f32) -> Option<f32> {",
        "pub fn total(xs: &[f32]) -> f32 {",
        "pub fn ramp(n: u64, step: f32) -> Vec<f32> {",
        "pub x: vesper_port_abi::Word<f32>,",
    ] {
        assert!(text.contains(line), "missing {line}\n{text}");
    }
    // The committed binding is what the compiler writes.
    let committed = include_str!("../crates/vesper-port-pilot/port/widen.rs");
    assert_eq!(text, committed.replace("\r\n", "\n"));
    // v1 (no v2 form) and v2 bindings pass f32 the same way.
    for (abi, source) in [
        (
            export::EXPORT_ABI,
            "pub fn f(x: f32, y: u64) -> f32 {\n    x\n}\n",
        ),
        (
            export::EXPORT_ABI_V2,
            "pub fn f(x: f32, y: u64) -> f32 {\n    x\n}\npub fn g(inout x: f32) {\n    x = x + 1.0;\n}\n",
        ),
    ] {
        let lib = compile_source("t.ves", source, &options("t", None)).unwrap();
        assert_eq!(lib.interface.abi, abi);
        let text = binding(&lib.interface);
        assert!(
            text.contains("fn vesper_t_f(x: f32, y: i64) -> f32;"),
            "{text}"
        );
        assert!(text.contains("pub fn f(x: f32, y: u64) -> f32 {"), "{text}");
    }
}
