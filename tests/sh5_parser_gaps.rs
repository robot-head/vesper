//! Parser gaps the SH4 backend ports hit (self-hosting SH5 lane 4, #1327).
//!
//! - `if` and `match` are operands, so `a && if b { .. } else { .. }` and
//!   `a || match v { .. }` parse (manuscript `PrimaryExpr`).
//! - A statement-position `handle` (or `region`) ending in `}` needs no `;`.
//! - `copy` and `affine` are contextual names, like `linear`: they are modes
//!   only after `mod`, `@`, `@@` and at an item start.
//! - A keyword that cannot be a name gives one diagnostic naming it.
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

/// SH4 lane 16's repro (E0002 "expected expression") and lane 15's `match`.
#[test]
fn if_and_match_are_operands() {
    let source = "fn f(a: bool, b: bool) -> bool {
    a && if b { true } else { false }
}
fn g(a: bool, x: u64) -> bool {
    a || match x { 0 => true, _ => false }
}
fn h(x: u64) -> bool {
    10 == if x == 0 { 10 } else { 20 }
}
fn main() -> u64 {
    let mut n: u64 = 0;
    if f(true, true) { n = wrapping_add(n, 1); }
    if !f(true, false) { n = wrapping_add(n, 2); }
    if !f(false, true) { n = wrapping_add(n, 4); }
    if g(false, 0) { n = wrapping_add(n, 8); }
    if !g(false, 3) { n = wrapping_add(n, 16); }
    if g(true, 3) { n = wrapping_add(n, 32); }
    if h(0) { n = wrapping_add(n, 64); }
    if !h(1) { n = wrapping_add(n, 128); }
    n
}";
    assert_eq!(both(source), "255");
}

/// An `if` operand in an `if` condition ends at its own blocks.
#[test]
fn operand_if_in_a_condition() {
    assert_eq!(
        both(
            "fn main() -> u64 {
    let a = true;
    let b = false;
    if a && if b { false } else { true } { 1 } else { 2 }
}"
        ),
        "1"
    );
}

/// At a statement start `if` and `match` are still statements: a following
/// `-1` is a new expression, not a subtraction.
#[test]
fn statement_if_is_unchanged() {
    let p = parser::parse("fn main() -> i64 { let x: i64 = 3; if x == 3 { } -1 }");
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

/// SH4 lane 15: a `handle` statement needed a `;` before the next statement.
#[test]
fn handle_statement_needs_no_semicolon() {
    assert_eq!(
        both(
            "linear effect Ask { fn ask() -> u64; }
fn asks() -> u64 eff [Ask] { Ask::ask() }
fn main() -> u64 {
    let mut n: u64 = 1;
    handle Ask in { asks() } with { return x => x, ask(k) => k(10), }
    n = wrapping_mul(n, 2);
    n
}"
        ),
        "2"
    );
}

/// SH4 lanes 9 and 13: `copy` and `affine` as names.
#[test]
fn copy_and_affine_are_names() {
    assert_eq!(
        both(
            "struct Row { copy: u64, affine: u8 }
impl Row { fn copy(in self) -> u64 { self.copy } }
fn affine(affine: bool) -> u64 { if affine { 1 } else { 0 } }
fn copy(x: u64) -> u64 { wrapping_add(x, 1) }
fn main() -> u64 {
    let copy = copy(4);
    let affine = affine(true);
    let r = Row { copy: copy, affine: 2 };
    let m = match copy { copy => copy };
    wrapping_add(wrapping_add(r.copy(), widen_u64(r.affine)), wrapping_add(m, affine))
}"
        ),
        "13"
    );
}

/// The modes keep working where their forms start.
#[test]
fn copy_and_affine_modes_are_unchanged() {
    assert_eq!(
        both(
            "copy struct P { x: u64 }
affine struct B { v: u64 }
pub copy struct Q { y: u64 }
struct H { p: P @@ copy }
fn id[T: mod copy](x: T) -> T { x }
fn twice(f: fn@copy(u64) -> u64, x: u64) -> u64 { f(f(x)) }
fn inc(x: u64) -> u64 { wrapping_add(x, 1) }
fn main() -> u64 {
    let p = P { x: 1 };
    let q = id(p);
    let b = B { v: 2 };
    let h = H { p: q };
    let r = Q { y: 3 };
    wrapping_add(wrapping_add(h.p.x, b.v), twice(inc, r.y))
}"
        ),
        "8"
    );
}

/// `sink copy` passes a value named `copy` by mode.
#[test]
fn copy_after_a_pass_mode() {
    assert_eq!(
        both(
            "struct B { v: u64 }
fn take(sink b: B) -> u64 { b.v }
fn main() -> u64 { let copy = B { v: 5 }; take(sink copy) }"
        ),
        "5"
    );
}

/// A keyword that cannot be a name gives one diagnostic that names it, and
/// the parse goes on (lanes 9 and 11 hit the 128-diagnostic limit first).
#[test]
fn reserved_keyword_gives_one_diagnostic() {
    for (source, word) in [
        ("fn main() -> u64 { let in = 1; 2 }", "in"),
        ("fn main() -> u64 { let fn = 1; 2 }", "fn"),
        ("fn main() -> u64 { let (a, mod) = (1, 2); a }", "mod"),
        ("fn f(match: u64) -> u64 { 1 }", "match"),
    ] {
        let p = parser::parse(source);
        assert_eq!(p.diagnostics.len(), 1, "{source}\n{:#?}", p.diagnostics);
        let d = &p.diagnostics[0];
        assert!(
            d.message
                .contains(&format!("the keyword `{word}`, which cannot be a name")),
            "{source}\n{d:?}"
        );
        assert_eq!(&source[d.span.clone()], word, "{source}");
    }
}
