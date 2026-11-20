//! The parse module (#1155) frees what it allocates. Its own test binary,
//! because the live allocation counter is global to the process and the
//! other parse tests run in parallel; for the same reason it is one test.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/parse.rs"]
mod binding;

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A parse frees every sequence the Vesper module allocates, and the result
/// stream is handed over whole.
#[test]
fn parse_frees_its_allocations() {
    let source =
        std::fs::read_to_string(root().join("crates/vesper-hybrid/modules/parse/src/lib.ves"))
            .unwrap();
    let _ = binding::parse_words(&source);
    let before = binding::live_allocations();
    for _ in 0..3 {
        let words = binding::parse_words(&source);
        assert!(!words.is_empty());
    }
    assert_eq!(binding::live_allocations(), before);
    // Sources that stop early (halted, limited, oversized) free everything
    // too. One test, because the counter is global to the process.
    let deep = format!("fn f() {{ {}1{} }}", "(".repeat(200), ")".repeat(200));
    let many = format!("fn f() {{ {} }}", "x;".repeat(40000));
    let huge = format!("fn f() {{}} {}", " ".repeat(1 << 20));
    for source in [deep.as_str(), many.as_str(), huge.as_str(), "", "fn"] {
        let _ = binding::parse_words(source);
        let before = binding::live_allocations();
        let _ = binding::parse_words(source);
        assert_eq!(
            binding::live_allocations(),
            before,
            "{:?}",
            &source[..source.len().min(40)]
        );
    }
}
