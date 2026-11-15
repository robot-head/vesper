use vesper::{check, parser};

#[test]
fn tilde_is_rejected_until_its_semantics_are_implemented() {
    let parsed = parser::parse("fn main()->u64 { ~1 }");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.message.contains("prefix '~' is not supported yet")),
        "{:?}",
        checked.diagnostics
    );
}
