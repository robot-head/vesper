//! `eff` is contextual (self-hosting SH2 lane 3, #1175; census gap C1). It
//! opens an effect row only where a signature allows one (after a `fn`
//! signature, a function type, a closure head, and as the `e: eff` row
//! bound); everywhere a name is expected it is a name: fields, methods,
//! bindings, parameters, patterns and paths.
use vesper::{check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    let c = check::check(&p.module);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
    c.world
}

/// The evaluator and the native JIT agree; the result as text.
fn both(source: &str) -> String {
    let world = accepted(source);
    let evaluated = Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&world, "main").unwrap().to_string();
    assert_eq!(evaluated, jit, "evaluator and native JIT agree");
    evaluated
}

/// The census repro (docs/parallel-results/self-host-census.md, C1).
#[test]
fn census_repro_field_named_eff() {
    assert_eq!(
        both("struct Row { eff: u32 }\nfn main() -> u32 { let r = Row { eff: 1 }; r.eff }"),
        "1"
    );
}

#[test]
fn eff_as_binding_parameter_and_method() {
    assert_eq!(
        both(
            "copy struct Row { eff: u64 }
impl Row { fn eff(in self) -> u64 { self.eff } }
fn add(eff: u64, n: u64) -> u64 { wrapping_add(eff, n) }
fn main() -> u64 {
    let eff = 40;
    let row = Row { eff };
    let Row { eff: inner } = row;
    let mut total = add(eff, 2);
    total = wrapping_add(total, wrapping_sub(row.eff(), inner));
    let shifted = match total { eff => wrapping_add(eff, 0) };
    shifted
}"
        ),
        "42"
    );
}

#[test]
fn eff_still_opens_effect_rows_in_signatures() {
    assert_eq!(
        both(
            "linear effect Ask { fn ask() -> u64; }
fn twice(eff: u64) -> u64 eff [Ask] { wrapping_add(eff, Ask::ask()) }
fn apply[e: eff](sink f: fn(u64) -> u64 eff [e], x: u64) -> u64 eff [e] { f(x) }
fn main() -> u64 {
    let eff: u64 = 1;
    handle Ask in {
        let g = |x: u64| eff [Ask] wrapping_add(x, Ask::ask());
        wrapping_add(twice(eff), apply(|x: u64| -> u64 eff [] { wrapping_add(x, eff) }, g(eff)))
    } with { return x => x, ask(k) => k(20), }
}"
        ),
        "43"
    );
}

#[test]
fn a_closure_body_may_start_with_a_name_eff() {
    assert_eq!(
        both(
            "fn main() -> u64 {
    let eff: u64 = 41;
    let f = |x: u64| wrapping_add(eff, x);
    let g = |x: u64| eff;
    wrapping_add(f(0), wrapping_sub(g(0), 40))
}"
        ),
        "42"
    );
}

#[test]
fn eff_without_a_row_after_a_signature_is_still_an_error() {
    let p = parser::parse("fn f() -> u64 eff { 1 }");
    assert!(!p.diagnostics.is_empty());
}

/// The fixture the front end v2 differential parses also checks and runs.
#[test]
fn the_differential_fixture_runs() {
    let source = include_str!("fixtures/contextual_eff.ves");
    let world = accepted(source);
    assert_eq!(
        Runtime::new(&world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        native::jit(&world, "main").unwrap().to_string()
    );
}

/// Review follow-ups: `for eff in ..` and calls through `Upper::eff`.
#[test]
fn eff_in_for_patterns_and_upper_paths() {
    assert_eq!(
        both(
            "copy struct Row { n: u64 }
impl Row { fn eff(n: u64) -> Row { Row { n } } }
fn main() -> u64 {
    let mut total: u64 = 0;
    for eff in 0..4 { total = wrapping_add(total, eff); }
    wrapping_add(total, Row::eff(36).n)
}"
        ),
        "42"
    );
}
