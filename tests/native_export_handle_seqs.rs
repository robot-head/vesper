//! Handle types that own sequences of handles and boxed trait objects
//! (#1087): `ChunkSeq[H]` of another handle type, `ChunkSeq[String]`,
//! `Box[dyn P]` and `Option`/`ChunkSeq` of those are accepted as fields of a
//! handle type, which stays opaque to Rust; dropping it from Rust runs the
//! full drop glue (every element, each boxed value through its vtable).
//! Such a sequence never crosses on its own.
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
fn handle_fields_hold_sequences_of_handles_and_boxed_objects() {
    let i = widen();
    let fields = |ty: &str| i.types[ty].variants[0].fields.clone();
    let files = &fields("Fabric")[0];
    assert_eq!(files.name, "files");
    assert!(
        matches!(&files.ty, AbiType::Seq { elem } if matches!(&**elem, AbiType::Named { name, .. } if name == "Bank"))
    );
    let procs = &fields("Graph")[0];
    assert!(
        matches!(&procs.ty, AbiType::Seq { elem } if matches!(&**elem, AbiType::Box { inner } if matches!(&**inner, AbiType::Object { name } if name == "dyn Proc"))),
        "{procs:?}"
    );
    for ty in ["Bank", "Fabric", "Graph"] {
        assert!(
            i.types[ty].handle && i.types[ty].drop_symbol.is_some(),
            "{ty}"
        );
    }
    let json = export::interface_json(&i).unwrap();
    assert!(json.contains("\"kind\": \"object\""), "{json}");
    let text = binding(&i);
    for line in [
        "pub fn files(&self) -> vesper_port_abi::OpaqueSeq<'a> {",
        "pub fn procs(&self) -> vesper_port_abi::OpaqueSeq<'a> {",
        "pub fn tail(&self) -> Option<vesper_port_abi::OpaqueObject<'a>> {",
        "pub fn fabric_pop(f: &mut Fabric) -> Option<Bank> {",
    ] {
        assert!(text.contains(line), "missing {line}\n{text}");
    }
}

#[test]
fn the_reports_repros_are_accepted() {
    // silicon gap 3 and Resonance gap 4.
    for source in [
        "// @export abi 3\npub struct Imsic {\n    eip: ChunkSeq[u64]\n}\npub struct F {\n    files: ChunkSeq[Imsic]\n}\npub fn f_len(inout f: F) -> u64 {\n    f.files.len()\n}\n",
        "// @export abi 3\npub trait P {\n    fn run(inout self, x: u64) -> u64;\n}\npub struct G {\n    procs: ChunkSeq[Box[dyn P]]\n}\npub fn g_len(in g: G) -> u64 {\n    g.procs.len()\n}\n",
    ] {
        compile_source("t.ves", source, &options("t", None))
            .unwrap_or_else(|e| panic!("{e}\n{source}"));
    }
}

#[test]
fn sequences_of_handles_cross_only_inside_handles() {
    let base =
        "pub struct B {\n    s: ChunkSeq[u64]\n}\ntrait P {\n    fn run(inout self) -> u64;\n}\n";
    for (f, needle) in [
        (
            "pub fn f(in s: ChunkSeq[B]) -> u64 {\n    s.len()\n}\n",
            "crosses only inside a handle type",
        ),
        (
            "pub fn f(inout s: ChunkSeq[B]) -> u64 {\n    s.len()\n}\n",
            "crosses only inside a handle type",
        ),
        (
            "pub fn f() -> ChunkSeq[B] {\n    ChunkSeq::new[B]()\n}\n",
            "crosses only inside a handle type",
        ),
        (
            "pub fn f() -> Option[ChunkSeq[B]] {\n    Option::None\n}\n",
            "crosses only inside a handle type",
        ),
        (
            "pub fn f(in s: Slice[B]) -> u64 {\n    s.len()\n}\n",
            "non-copy elements",
        ),
        (
            "pub fn f(in s: ChunkSeq[Box[dyn P]]) -> u64 {\n    s.len()\n}\n",
            "crosses only inside a handle type",
        ),
        (
            "pub struct C {\n    v: Slice[B]\n}\npub fn f(inout c: C) -> u64 {\n    0\n}\n",
            "",
        ),
    ] {
        let e = refused(&format!("{base}{f}"));
        assert!(e.contains(needle), "{f}: {e}");
    }
}

#[test]
fn dropped_fabrics_and_graphs_free_everything() {
    let fabric = agree(
        "fn main() -> u64 {
    let mut total: u64 = 0;
    for round in 0..20 {
        let mut f = fabric_new(3, 4);
        let last = fabric_pop(inout f);
        total = wrapping_add(total, match last { Option::Some(b) => bank_read(in b, 1), Option::None => 0 });
        total = wrapping_add(total, fabric_len(in f));
    }
    total
}",
    );
    assert_eq!(fabric, (20 * (201 + 2)).to_string());
    let graph = agree(
        "fn main() -> i64 {
    let mut g = graph_new(2, 2, true);
    handle Fail[RuntimeError] in {
        let a = graph_run(inout g, 1);
        let b = graph_run(inout g, 2);
        let c = graph_run(inout g, 5);
        let d = graph_run(inout g, 7);
        wrapping_add(wrapping_add(a, b), wrapping_add(wrapping_mul(c, 100), wrapping_mul(d, 10000)))
    } with { return x => x, raise(e, k) => -1, }
}",
    );
    assert_eq!(graph, (6 * 100 + 12 * 10000).to_string());
}
