//! `in` handle arguments lent in place in export ABI v3 (#1087): the
//! binding passes a pointer to the caller's own handle words, as it already
//! does for `in` mirrors, instead of a scratch copy (a 27-word `memcpy` per
//! call for silicon's `Aplic`). Vesper never writes an `in` value (E0307).
//! `inout` and by-value handles, and v2 libraries, are unchanged.
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

#[test]
fn in_handles_are_lent_in_place_in_v3_only() {
    let i = widen();
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    assert!(f("bank_read").params[0].in_place);
    assert_eq!(f("bank_read").params[0].pass(), "in");
    assert!(
        !f("bank_write").params[0].in_place,
        "inout handles keep their v2 form"
    );
    assert!(f("graph_runs").params[0].in_place);
    let text = binding(&i);
    assert!(
        text.contains("vesper_widen_bank_read(b.words.as_ptr().cast_mut(), i as i64)"),
        "{text}"
    );
    assert!(
        text.contains("vesper_widen_bank_write(b.words.as_mut_ptr(), i as i64, v as i64)"),
        "{text}"
    );
    assert!(!text.contains("arg_b"), "{text}");
    // The repro from the silicon report, without and with the directive.
    let repro = "pub struct H {\n    a: u64,\n    s: ChunkSeq[u64]\n}\npub fn h_new() -> H {\n    H { a: 7, s: ChunkSeq::with_capacity[u64](2) }\n}\npub fn h_a(in h: H) -> u64 {\n    h.a\n}\n";
    let v2 = compile_source("h.ves", repro, &options("h", None)).unwrap();
    assert_eq!(v2.interface.abi, export::EXPORT_ABI_V2);
    let text = binding(&v2.interface);
    assert!(
        text.contains("let mut arg_h = h.words;"),
        "v2 keeps its copy:\n{text}"
    );
    let v3 = compile_source(
        "h.ves",
        &format!("// @export abi 3\n{repro}"),
        &options("h", None),
    )
    .unwrap();
    let text = binding(&v3.interface);
    assert!(
        text.contains("vesper_h_h_a(h.words.as_ptr().cast_mut())"),
        "{text}"
    );
    assert!(!text.contains("arg_h"), "{text}");
    // Only the binding changes.
    assert_eq!(v2.object, v3.object);
}

#[test]
fn lent_handles_read_what_the_caller_holds() {
    let read = agree(
        "fn main() -> u64 {
    let mut b = bank_new(10, 4);
    bank_write(inout b, 1, 99);
    let mut t: u64 = 0;
    let n: u64 = 1000;
    for i in 0..n {
        t = wrapping_add(t, bank_read(in b, i % 5));
    }
    t
}",
    );
    // 200 rounds of 10 + 99 + 12 + 13 + the base for the miss.
    assert_eq!(read, (200 * (10 + 99 + 12 + 13 + 10)).to_string());
}
