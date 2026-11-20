//! `else if` chains of any length the parser accepts format (self-hosting
//! SH1 lane 5, #1148): the preflight counts the arms of one chain as one
//! component, so a 21-arm table no longer fails with `DepthLimit`.
use vesper_fmt::{Error, Limits, format_source};

fn chain(arms: usize) -> String {
    let mut s = String::from("fn f(k: u64) -> u64 {\n    if k == 0 { 0 }");
    for i in 1..arms {
        s.push_str(&format!(" else if k == {i} {{ {i} }}"));
    }
    s.push_str(" else { 9 }\n}\n");
    s
}

#[test]
fn long_else_if_chains_format_and_are_idempotent() {
    for arms in [20, 21, 40, 100, 120] {
        let source = chain(arms);
        let out = format_source(&source, Limits::default())
            .unwrap_or_else(|e| panic!("{arms} arms: {e}"))
            .output;
        assert_eq!(out.matches("} else if k == ").count(), arms - 1, "{arms} arms");
        let again = format_source(&out, Limits::default()).unwrap();
        assert!(!again.changed, "{arms} arms: not idempotent");
    }
}

#[test]
fn chains_inside_expressions_still_count() {
    // An `if` used as an operand keeps counting toward the chain limit,
    // as before: only the arms of one `else if` chain collapse.
    let sum = (0..200)
        .map(|_| "if c { 0 } else { 1 }")
        .collect::<Vec<_>>()
        .join(" + ");
    assert_eq!(
        format_source(&format!("fn main() {{{sum}}}"), Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
    // A chain nested in an argument list and a block formats too.
    let arms = (1..60)
        .map(|i| format!(" else if k == {i} {{ {i} }}"))
        .collect::<String>();
    let nested = format!("fn g(k: u64) -> u64 {{ h({{ if k == 0 {{ 0 }}{arms} else {{ 9 }} }}) }}\n");
    format_source(&nested, Limits::default()).unwrap();
}
