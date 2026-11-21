//! Local native functions are named after their Vesper instances
//! (`vesper_function_{i}_{key}`), so profilers and disassemblers show
//! which function a sample or an instruction belongs to.
use vesper::{check, native, parser};

#[test]
fn local_functions_carry_their_vesper_names() {
    let source = "fn count(n: u64) -> u64 { if n == 0 { 0 } else { count(wrapping_sub(n, 1)) } }
fn main() -> u64 { count(10) }";
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let (bytes, _) = native::object(&c.world, "main").unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let symbols: Vec<&str> = text
        .split('\0')
        .filter(|s| s.starts_with("vesper_function_"))
        .collect();
    assert!(symbols.iter().any(|s| s.ends_with("_count")), "{symbols:?}");
    // Every character outside [A-Za-z0-9_] in an instance key becomes `_`.
    assert!(
        symbols
            .iter()
            .all(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
        "{symbols:?}"
    );
}
