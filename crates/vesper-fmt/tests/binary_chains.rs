//! Long homogeneous binary chains format (self-hosting SH3 lane 5, #1244).
//! The preflight estimates the expression height at each delimiter level the
//! way the parser's own height limit counts it: one per binary operator of a
//! chain plus the postfix steps of the current operand, so `f(x) || f(x)`
//! counts one per `||`, not one per token. Chains past the parser's limit are
//! still refused.
use vesper_fmt::{Error, Limits, format_source};

fn formats(source: &str) -> String {
    let out = format_source(source, Limits::default())
        .unwrap_or_else(|e| panic!("{e}: {source}"))
        .output;
    let again = format_source(&out, Limits::default()).unwrap();
    assert!(!again.changed, "not idempotent: {source}");
    out
}

fn chain(term: &str, n: usize, op: &str) -> String {
    (0..n).map(|_| term).collect::<Vec<_>>().join(op)
}

/// The lane's repro: a `-> bool` function of 24 `f(x)` terms joined by `||`,
/// inside an `else if` arm of a nested function (the highlight classifier's
/// shape), used to stop at `DepthLimit`.
#[test]
fn lane_repro_24_terms_in_an_else_if_arm() {
    let terms = chain("f(x)", 24, " || ");
    formats(&format!("fn g(x: u64) -> bool {{ {terms} }}"));
    let nested = format!(
        "fn g(x: u64) -> bool {{ if x == 0 {{ false }} else if {terms} {{ match x {{ _ => {{ {terms} }} }} }} else {{ false }} }}"
    );
    formats(&nested);
    let methods = chain("s.is(x.k)", 24, " || ");
    formats(&format!(
        "fn g(s: S, x: X) -> bool {{ if s.a {{ true }} else if {methods} {{ true }} else {{ false }} }}"
    ));
}

#[test]
fn homogeneous_chains_format_up_to_the_parser_limit() {
    for n in [24, 44, 60, 100, 110] {
        for (term, op) in [
            ("f(x)", " || "),
            ("x", " + "),
            ("a.b(c)", " && "),
            ("v[i]", " | "),
        ] {
            formats(&format!("fn g() -> u64 {{ {} }}", chain(term, n, op)));
        }
    }
    // A comparison chain counts both operators of a term.
    formats(&format!(
        "fn g(x: u64) -> bool {{ {} }}",
        chain("x == 1", 60, " || ")
    ));
}

#[test]
fn chains_past_the_parser_limit_are_still_refused() {
    for (term, op) in [("f(x)", " || "), ("x", " + ")] {
        let source = format!("fn g() -> u64 {{ {} }}", chain(term, 200, op));
        assert_eq!(
            format_source(&source, Limits::default()).unwrap_err(),
            Error::DepthLimit
        );
    }
    // Between the parser's limit and the preflight's slack the parser
    // refuses the source.
    let source = format!("fn g() -> u64 {{ {} }}", chain("f(x)", 140, " || "));
    assert!(format_source(&source, Limits::default()).is_err());
}

/// `effect (`, `handle (`, `sink (` and `inout (` are calls of values named
/// so (#1244), glued like `eff (`.
#[test]
fn contextual_names_glue_their_call_parentheses() {
    let out = formats(
        "fn effect ( ) -> u64 { 1 } fn handle (x: u64) -> u64 { x } fn f(sink: u64) -> u64 { sink (1) + handle (sink) + effect () }",
    );
    assert!(out.contains("fn effect() -> u64"), "{out}");
    assert!(out.contains("fn handle(x: u64)"), "{out}");
    assert!(out.contains("sink(1) + handle(sink) + effect()"), "{out}");
    // A pass mode keeps its space before an operand.
    let out = formats("fn f(sink s: S, inout t: T) { g(sink s, inout t.a); }");
    assert!(out.contains("g(sink s, inout t.a)"), "{out}");
}

/// The four words used as names keep a name's spacing before `-`, `[` and
/// `?` (#1244).
#[test]
fn contextual_names_space_like_names() {
    let out = formats(
        "fn f(sink: u64, v: S) -> u64 { let a = sink - 1; let b = v.sink[0]; let c = v.handle - a; g(sink - 1, inout x) }",
    );
    assert!(out.contains("let a = sink - 1;"), "{out}");
    assert!(out.contains("let b = v.sink[0];"), "{out}");
    assert!(out.contains("let c = v.handle - a;"), "{out}");
    assert!(out.contains("g(sink - 1, inout x)"), "{out}");
}
