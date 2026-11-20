//! Or-patterns `p | q` in `match` arms (self-hosting SH1 lane 5, #1148).
//! An arm with alternatives means one arm per alternative sharing the body;
//! every alternative binds the same names at the same types (`E0214`).
mod ergonomics_support;
use ergonomics_support::{accepted, agree, checked, rejects};

/// The fixture's `main`, computed in Rust.
fn model() -> u64 {
    let class = |k: u8| match k {
        0 | 9 | 10 | 13 | 32 => 1u64,
        40 | 41 | 91 | 93 | 123 | 125 => 2,
        _ => 3,
    };
    let pair = |a: u8, b: u8| match (a, b) {
        (0, _) | (_, 0) => 0u64,
        (1, x) | (x, 1) => u64::from(x),
        _ => 99,
    };
    let mut h: u64 = 0;
    for k in 0..130u8 {
        h = h.wrapping_mul(3).wrapping_add(class(k));
    }
    h = h.wrapping_add(105);
    h = h.wrapping_mul(7).wrapping_add(106);
    h = h.wrapping_mul(7).wrapping_add(7);
    h = h.wrapping_mul(7).wrapping_add(7);
    for v in [pair(0, 5), pair(1, 5), pair(5, 1), pair(5, 5)] {
        h = h.wrapping_mul(11).wrapping_add(v);
    }
    h
}

#[test]
fn literal_constructor_and_tuple_alternatives_agree_on_every_backend() {
    let source = include_str!("fixtures/ergonomics/or_patterns.ves");
    assert_eq!(agree("or_patterns", source), model());
}

#[test]
fn alternatives_cover_variants_for_exhaustiveness() {
    accepted(
        "enum E { A, B(u8), C } fn f(sink e: E) -> u64 { match e { E::A | E::C => 1, E::B(_) => 2 } } fn main() -> u64 { f(E::C) }",
    );
    accepted("fn f(b: bool) -> u64 { match b { true | false => 1 } } fn main() -> u64 { f(true) }");
    rejects(
        "enum E { A, B, C } fn f(sink e: E) -> u64 { match e { E::A | E::C => 1 } } fn main() -> u64 { 0 }",
        "E0212",
    );
}

#[test]
fn alternatives_bind_the_same_names_at_the_same_types() {
    // Different names.
    rejects(
        "enum T { A(u64), B(u64) } fn f(sink t: T) -> u64 { match t { T::A(x) | T::B(y) => 1 } } fn main() -> u64 { 0 }",
        "E0214",
    );
    // A name bound by one alternative only.
    rejects(
        "enum T { A(u64), B } fn f(sink t: T) -> u64 { match t { T::A(x) | T::B => 1 } } fn main() -> u64 { 0 }",
        "E0214",
    );
    // The same name at different types.
    rejects(
        "enum T { A(u64), B(u8) } fn f(sink t: T) -> u64 { match t { T::A(x) | T::B(x) => 1 } } fn main() -> u64 { 0 }",
        "E0214",
    );
}

#[test]
fn a_shared_body_reports_each_diagnostic_once() {
    let d = rejects(
        "fn f(k: u8) -> u64 { match k { 1 | 2 | 3 => nope, _ => 0 } } fn main() -> u64 { 0 }",
        "E0201",
    );
    assert_eq!(d.iter().filter(|d| d.code == "E0201").count(), 1, "{d:#?}");
}

#[test]
fn bodies_with_moves_and_effects_run_once_per_match() {
    // The body moves an owned value and performs an effect; each
    // alternative's copy is a separate branch, so neither is doubled.
    let source = r#"
enum K { A, B, C }
fn pick(sink k: K, sink s: ChunkSeq[u64]) -> u64 {
    match k {
        K::A | K::B => { let t = s; wrapping_add(t.len(), 10) },
        K::C => s.len(),
    }
}
fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    s.push(1);
    s.push(2);
    let mut u: ChunkSeq[u64] = ChunkSeq::new();
    u.push(3);
    wrapping_add(wrapping_mul(pick(K::B, s), 100), pick(K::C, u))
}
"#;
    assert_eq!(agree("or_pattern_moves", source), 1201);
}

#[test]
fn malformed_alternatives_are_syntax_errors() {
    for source in [
        "fn f(k: u8) -> u8 { match k { 1 | => 0, _ => 1 } }",
        "fn f(k: u8) -> u8 { match k { | 1 => 0, _ => 1 } }",
        "fn f(k: u8) -> u8 { match k { 1 || 2 => 0, _ => 1 } }",
    ] {
        let p = vesper::parser::parse(source);
        assert!(!p.diagnostics.is_empty(), "{source}");
    }
    // Without `match`, `|` stays an operator and a closure delimiter.
    let c = checked("fn main() -> u64 { let a: u64 = 6; match a | 1 { 7 | 8 => a | 2, _ => 0 } }");
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
}

#[test]
fn nested_alternatives_cannot_expand_exponentially() {
    // Each level doubles the copies of everything below it; the parser's
    // expansion budget refuses the source instead of building 2^40 bodies.
    let depth = 40;
    let source = format!(
        "fn main() -> u64 {{ {}0{} }}",
        "match true { true | false => ".repeat(depth),
        " }".repeat(depth)
    );
    let started = std::time::Instant::now();
    let p = vesper::parser::parse(&source);
    assert!(
        p.diagnostics.iter().any(|d| d
            .message
            .contains("or-pattern alternatives copy too much code")),
        "{:?}",
        p.diagnostics
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    // A few levels stay well inside the budget.
    let small = format!(
        "fn main() -> u64 {{ {}7{} }}",
        "match true { true | false => ".repeat(6),
        " }".repeat(6)
    );
    assert_eq!(agree("nested_or_patterns", &small), 7);
}

#[test]
fn checked_reference_covers_the_whole_domain() {
    use vesper::integer_widths::{CheckedOp, checked_builtin};
    let (op, kind) = checked_builtin("checked_mul_u64").unwrap();
    assert_eq!(op, CheckedOp::Mul);
    let max = u64::MAX as i128;
    assert_eq!(op.eval(kind, max, max), None);
    assert_eq!(op.eval(kind, max, 1), Some(max));
}
