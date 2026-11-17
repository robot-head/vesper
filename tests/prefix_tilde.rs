use vesper::{check, parser};

#[test]
fn tilde_is_the_bitwise_complement() {
    let parsed = parser::parse("fn main()->u64 { ~1 }");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let value = vesper::runtime::Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap();
    assert_eq!(value.to_string(), u64::MAX.wrapping_sub(1).to_string());
    let rejected = check::check(&parser::parse("fn main()->bool { ~true }").module);
    assert!(
        rejected
            .diagnostics
            .iter()
            .any(|d| d.message.contains("prefix '~' requires an integer operand")),
        "{:?}",
        rejected.diagnostics
    );
}
