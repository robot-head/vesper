//! `effect`, `handle`, `sink` and `inout` are contextual (self-hosting SH3
//! lane 5, #1244). `effect` starts only an item; `handle` starts a handle
//! expression only before its effect (`handle E`, `handle l: E`,
//! `handle self::m::E`); `sink` and `inout` are pass modes in a parameter
//! list only before the parameter's name, and in an argument list only before
//! a token that starts an operand and cannot continue a value named `sink`.
//! Everywhere else a name is expected they are names.
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

/// The lane's repros: each was E0002 ("found the keyword ..., which cannot
/// be a name").
#[test]
fn lane_repros() {
    assert_eq!(
        both("fn effect() {}\nfn main() -> u64 { effect(); 1 }"),
        "1"
    );
    assert_eq!(
        both("fn handle(x: u64) -> u64 { x }\nfn main() -> u64 { handle(2) }"),
        "2"
    );
    assert_eq!(both("fn main() -> u64 { let effect = 1; effect }"), "1");
    assert_eq!(
        both(
            "struct S { sink: u64, inout: u8 }
fn main() -> u64 { let s = S { sink: 3, inout: 4 }; wrapping_add(s.sink, widen_u64(s.inout)) }"
        ),
        "7"
    );
}

#[test]
fn pass_words_as_parameter_names_and_modes() {
    assert_eq!(
        both(
            "struct Box2 { v: u64 }
fn take(sink sink: Box2) -> u64 { sink.v }
fn bump(inout inout: u64) { inout = wrapping_add(inout, 1); }
fn pair(sink: u64, inout: u64) -> u64 { wrapping_add(sink, inout) }
fn main() -> u64 {
    let mut sink: u64 = 10;
    bump(inout sink);
    let inout = pair(sink, 20);
    let b = Box2 { v: 9 };
    wrapping_add(inout, take(sink b))
}"
        ),
        "40"
    );
}

/// In an argument list `sink` before `,`, `)`, `.`, `(`, `[` or an operator
/// is a value named `sink`, not a pass mode.
#[test]
fn pass_words_as_argument_values() {
    assert_eq!(
        both(
            "copy struct P { sink: u64 }
fn id(x: u64) -> u64 { x }
fn sink(x: u64) -> u64 { wrapping_add(x, 100) }
fn main() -> u64 {
    let inout: u64 = 1;
    let p = P { sink: 2 };
    let a = id(inout);
    let b = id(p.sink);
    let c = id(wrapping_add(inout, 3));
    let d = id(sink(4));
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))
}"
        ),
        "111"
    );
    // `sink` followed by an operator is the value.
    assert_eq!(
        both(
            "fn id(x: u64) -> u64 { x }
fn main() -> u64 { let sink: u64 = 5; id(sink - 1) }"
        ),
        "4"
    );
}

/// Before a closure's `|` or `||` the word is a mode: closures are passed by
/// `sink` far more often than a value named `sink` is or-ed.
#[test]
fn pass_words_before_closures_are_modes() {
    assert_eq!(
        both(
            "fn call(sink f: fn() -> u64) -> u64 { f() }
fn apply(sink f: fn(u64) -> u64) -> u64 { f(2) }
fn main() -> u64 { wrapping_add(call(sink || 40), apply(sink |x: u64| x)) }"
        ),
        "42"
    );
    // A value named `sink` is or-ed in parentheses.
    assert_eq!(
        both("fn id(x: u64) -> u64 { x }\nfn main() -> u64 { let sink: u64 = 40; id((sink) | 2) }"),
        "42"
    );
}

#[test]
fn handle_as_a_name_and_handle_expressions() {
    assert_eq!(
        both(
            "linear effect Ask { fn ask() -> u64; }
fn handle(x: u64) -> u64 { wrapping_add(x, 1) }
fn asks() -> u64 eff [Ask] { Ask::ask() }
fn main() -> u64 {
    let handle_n = handle(1);
    let handle = handle_n;
    let a = handle Ask in { asks() } with { return x => x, ask(k) => k(10), };
    let b = handle l: Ask in { asks() } with { return x => x, ask(k) => k(20), };
    wrapping_add(wrapping_add(a, b), handle)
}"
        ),
        "32"
    );
}

#[test]
fn effect_as_field_method_and_pattern() {
    assert_eq!(
        both(
            "copy struct Row { effect: u64, handle: u64 }
impl Row { fn effect(in self) -> u64 { self.effect } }
fn main() -> u64 {
    let row = Row { effect: 40, handle: 2 };
    let Row { effect, handle } = row;
    let m = match effect { effect => effect };
    wrapping_add(wrapping_sub(row.effect(), m), wrapping_add(effect, handle))
}"
        ),
        "42"
    );
}

#[test]
fn for_patterns_and_paths() {
    assert_eq!(
        both(
            "copy struct Row { n: u64 }
impl Row { fn handle(n: u64) -> Row { Row { n } } }
fn main() -> u64 {
    let mut total: u64 = 0;
    for sink in 0..4 { total = wrapping_add(total, sink); }
    for effect in 0..2 { total = wrapping_add(total, effect); }
    wrapping_add(total, Row::handle(35).n)
}"
        ),
        "42"
    );
}

/// `effect` still declares effects, and keywords that stay reserved keep
/// their message.
#[test]
fn keywords_still_work_and_reserved_words_stay_errors() {
    assert!(
        parser::parse("effect E { fn e() -> u64; }")
            .diagnostics
            .is_empty()
    );
    assert!(
        parser::parse("linear effect E { fn e() -> u64; }")
            .diagnostics
            .is_empty()
    );
    // `copy` and `affine` are contextual names since SH5 lane 4 (#1327,
    // tests/sh5_parser_gaps.rs).
    for word in ["in", "match", "struct"] {
        let p = parser::parse(&format!("fn {word}() -> u64 {{ 1 }}"));
        assert!(
            p.diagnostics
                .iter()
                .any(|d| d.message.contains("which cannot be a name")),
            "{word}: {:?}",
            p.diagnostics
        );
    }
}

/// The fixture the front end v2 and AST differentials parse also checks and
/// runs.
#[test]
fn the_differential_fixture_runs() {
    let source = include_str!("fixtures/contextual_names.ves");
    let world = accepted(source);
    let evaluated = Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(evaluated, native::jit(&world, "main").unwrap().to_string());
    assert_eq!(evaluated, "17");
}
