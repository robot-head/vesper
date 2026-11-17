//! `vesper build --lib` (export ABI `vesper.native.export-abi/1`): the
//! pilot's committed binding is current, refusals are typed, and objects
//! build for every hosted target with their C symbols.
use std::path::PathBuf;
use vesper::native::export::{
    self, AbiType, BindingOptions, ExportError, LibraryOptions, compile_source, rust_binding,
};

fn pilot() -> PathBuf {
    let here = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("crate directory");
    here.parent().expect("crates/").join("vesper-port-pilot")
}

fn options(target: Option<&str>) -> LibraryOptions<'_> {
    LibraryOptions {
        library: "pilot",
        target,
        optimize: true,
    }
}

fn pilot_source() -> String {
    std::fs::read_to_string(pilot().join("vesper/pilot.ves")).expect("pilot source")
}

#[test]
fn pilot_binding_is_current() {
    let library = compile_source("pilot.ves", &pilot_source(), &options(None)).expect("pilot");
    let expected = rust_binding(
        &library.interface,
        &BindingOptions {
            types_path: Some("crate".into()),
        },
    );
    vesper_port_harness::check_binding(&pilot().join("port/binding.rs"), &expected)
        .expect("regenerate with VESPER_PORT_BLESS=1 cargo test -p vesper-port-pilot --features vesper-port");
}

#[test]
fn pilot_interface_lists_every_pub_fn() {
    let library = compile_source("pilot.ves", &pilot_source(), &options(None)).expect("pilot");
    let i = &library.interface;
    assert_eq!(i.abi, export::EXPORT_ABI);
    let names = i
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>();
    // `collatz_from` is private: it is compiled (collatz_steps calls it)
    // but not exported.
    assert_eq!(
        names,
        [
            "add_i8",
            "area",
            "bounds",
            "clamp",
            "collatz_steps",
            "first_some",
            "gcd",
            "is_digit",
            "midpoint",
            "ratio",
            "sum_u16"
        ]
    );
    let ratio = i.functions.iter().find(|f| f.name == "ratio").unwrap();
    assert_eq!(ratio.symbol, "vesper_pilot_ratio");
    assert_eq!(ratio.fail_record_words, Some(2));
    assert_eq!(
        ratio.errors,
        [
            AbiType::Named {
                name: "ArithOverflow".into(),
                args: vec![]
            },
            AbiType::Named {
                name: "RuntimeError".into(),
                args: vec![]
            }
        ]
    );
    let midpoint = i.functions.iter().find(|f| f.name == "midpoint").unwrap();
    assert!(midpoint.result_pointer && midpoint.params.iter().all(|p| p.pointer));
    assert_eq!(i.types["Shape"].words, 3);
    assert_eq!(i.types["Point"].words, 2);
    // The interface file round-trips.
    let json = export::interface_json(i).unwrap();
    let back: export::LibraryInterface = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, i);
}

#[test]
fn objects_build_for_every_hosted_target() {
    let source = pilot_source();
    for (target, symbol) in [
        ("x86_64-unknown-linux-gnu", "vesper_pilot_gcd"),
        ("aarch64-unknown-linux-gnu", "vesper_pilot_gcd"),
        ("aarch64-apple-darwin", "_vesper_pilot_gcd"),
        ("x86_64-apple-darwin", "_vesper_pilot_gcd"),
        ("x86_64-pc-windows-msvc", "vesper_pilot_gcd"),
    ] {
        let a = compile_source("pilot.ves", &source, &options(Some(target)))
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        let b = compile_source("pilot.ves", &source, &options(Some(target))).unwrap();
        assert_eq!(a.object, b.object, "{target}: objects are deterministic");
        let found = a
            .object
            .windows(symbol.len())
            .any(|w| w == symbol.as_bytes());
        assert!(found, "{target}: {symbol} missing from the object");
    }
}

fn refusal(source: &str) -> String {
    match compile_source("t.ves", source, &options(None)) {
        Ok(_) => panic!("exported:\n{source}"),
        Err(ExportError::Compile(message)) => message,
        Err(other) => other.to_string(),
    }
}

#[test]
fn unexportable_functions_are_refused_with_the_reason() {
    let cases = [
        (
            "pub fn id[T](sink x: T) -> T { x }",
            "cannot export generic function 'id'",
        ),
        (
            "effect Tick { fn tick() -> i64; }\npub fn f() -> i64 eff [Tick] { Tick::tick() }",
            "effect Tick is not exportable",
        ),
        // Export ABI v2 (#898) exports `inout` scalars and handle types;
        // what it still refuses is listed in tests/native_export_v2.rs.
        (
            "pub fn f(sink xs: ChunkSeq[u64]) -> u64 { 0 }",
            "is a sequence taken by value",
        ),
        (
            "pub fn s(sink s: String) -> u64 { 0 }",
            "type String is a string taken by value",
        ),
        (
            "struct S { xs: ChunkSeq[u64] }\npub fn f(sink s: S) -> u64 { 0 }",
            "a handle that owns a sequence, taken by value",
        ),
        (
            "copy struct Pair[T] { a: T, b: T }\npub fn p(x: Pair[i64]) -> i64 { x.a }",
            "is a generic user type",
        ),
        (
            "fn private() -> i64 { 1 }",
            "library has no `pub fn` to export",
        ),
    ];
    for (source, expected) in cases {
        let message = refusal(source);
        assert!(
            message.contains(expected),
            "{source}\nexpected '{expected}', got: {message}"
        );
    }
}

#[test]
fn every_refusal_is_reported_together() {
    let message = refusal("pub fn a[T](sink x: T) -> T { x }\npub fn b[U](sink x: U) -> U { x }");
    assert!(
        message.contains("'a'") && message.contains("'b'"),
        "{message}"
    );
}

#[test]
fn library_names_prefix_symbols() {
    let source = "pub fn one() -> i64 { 1 }";
    let bad = compile_source(
        "t.ves",
        source,
        &LibraryOptions {
            library: "Bad-Name",
            target: None,
            optimize: true,
        },
    )
    .unwrap_err();
    assert!(
        bad.to_string().contains("must match [a-z][a-z0-9_]*"),
        "{bad}"
    );
}

#[test]
fn binding_declares_types_without_a_types_path() {
    let library = compile_source("pilot.ves", &pilot_source(), &options(None)).unwrap();
    let text = rust_binding(&library.interface, &BindingOptions::default());
    assert!(
        text.contains("pub struct Point { pub x: i64, pub y: i64 }"),
        "{text}"
    );
    assert!(text.contains("pub enum Shape {"));
    assert!(text.contains("pub enum RatioError {"));
    assert!(text.contains("fn vesper_pilot_ratio(a: i64, b: i64, fail: *mut u64) -> i64;"));
}

#[test]
fn visibility_is_read_from_tokens() {
    // Comments between `pub` and `fn`, and `@fip pub fn`, still export.
    let source = "pub/* API */fn one() -> i64 {\n    1\n}\n\n@fip\npub fn two() -> i64 {\n    2\n}\n\nfn hidden() -> i64 {\n    3\n}\n";
    let library = compile_source("t.ves", source, &options(None)).expect("exports");
    let names = library
        .interface
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["one", "two"]);
}
