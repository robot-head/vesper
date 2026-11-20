//! Generated Rust bindings keep a call's ABI words on the stack (#1147,
//! self-host hybrid build gap 1): the result, pointer arguments and the
//! failure record are arrays of a constant size, never a `Vec` per call.
use vesper::native::export::{BindingOptions, LibraryOptions, compile_source, rust_binding};

fn binding(source: &str) -> String {
    let options = LibraryOptions {
        library: "t",
        target: None,
        optimize: true,
    };
    let lib = compile_source("t.ves", source, &options).unwrap_or_else(|e| panic!("{e}"));
    rust_binding(&lib.interface, &BindingOptions::default())
}

/// Export ABI v1 (copy values only): `Option` results, struct arguments and
/// `Fail` records.
#[test]
fn v1_aggregate_words_are_stack_arrays() {
    let text = binding(
        "copy struct P { x: i64, y: i64 }
pub fn bounds(x: i32) -> Option[i32] {
    if x > 0 { Option::Some(x) } else { Option::None }
}
pub fn norm(p: P) -> i64 { p.x ^ p.y }
pub fn half(x: i64) -> i64 eff [Fail[u64]] {
    if x > 0 { x } else { Fail::raise(1) }
}
",
    );
    assert!(text.contains("export-abi/1"), "{text}");
    assert!(
        !text.contains("vec!"),
        "a binding allocates per call:\n{text}"
    );
    assert!(text.contains("let mut result = [0u64; 2];"), "{text}");
    assert!(
        text.contains("let mut arg_p = [0u64; <P as Abi>::WORDS];"),
        "{text}"
    );
    assert!(text.contains("let mut fail = [0u64; 2];"), "{text}");
}

/// Export ABI v2/v3 bindings already used stack arrays; a `String` result
/// is taken through `string_take`, which trusts Vesper's UTF-8.
#[test]
fn v2_string_results_use_stack_words() {
    let text = binding(
        "pub fn greet(in name: Str) -> String {
    let mut s = String::with_capacity(16);
    s.push_str(in \"hello, \");
    s.push_str(in name);
    s
}
",
    );
    assert!(
        !text.contains("vec!"),
        "a binding allocates per call:\n{text}"
    );
    assert!(
        text.contains("string_take(RUNTIME, &result[0..])"),
        "{text}"
    );
}
