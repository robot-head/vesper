use vesper::{
    ast::*,
    check::{self, World},
    parser,
    runtime::{Runtime, Value},
};

fn checked(source: &str) -> World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    assert_eq!(p.syntax().text().to_string(), source);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    c.world
}
fn run(source: &str) -> String {
    let world = checked(source);
    Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string()
}
fn rejects(source: &str, code: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:?}",
        c.diagnostics
    );
}

#[test]
fn precedence_and_functions() {
    assert_eq!(
        run(
            "fn add(a: i64, b: i64) -> i64 { wrapping_add(a, b) } fn main() -> i64 { add(2, 3 * 4) }"
        ),
        "14"
    );
}
#[test]
fn recursive_stack_machine() {
    assert_eq!(
        run(
            "fn fib(n: i64) -> i64 { if n < 2 { n } else { wrapping_add(fib(wrapping_sub(n, 1)), fib(wrapping_sub(n, 2))) } } fn main() -> i64 { fib(10) }"
        ),
        "55"
    );
}
#[test]
fn lossless_comments() {
    checked("/* nested /* yes */ ok */\n fn main() { // hi\n () }\n");
}
#[test]
fn error_recovery() {
    let p = parser::parse("fn main( { ? ; linear struct");
    assert!(!p.diagnostics.is_empty());
    assert_eq!(
        p.syntax().text().to_string(),
        "fn main( { ? ; linear struct"
    );
}
#[test]
fn linear_must_consume() {
    rejects(
        "linear struct Tx { id: u64 } fn main() { let tx = Tx { id: 1 }; }",
        "E0302",
    );
}
#[test]
fn linear_destructure() {
    assert_eq!(
        run(
            "linear struct Tx { id: u64 } fn main() -> u64 { let tx = Tx { id: 7 }; let Tx { id } = tx; id }"
        ),
        "7"
    );
}
#[test]
fn sink_method() {
    assert_eq!(
        run(
            "linear struct Tx { id: u64 } impl Tx { fn commit(sink self) -> u64 { let Tx { id } = self; id } } fn main() -> u64 { let tx = Tx { id: 9 }; tx.commit() }"
        ),
        "9"
    );
}
#[test]
fn moved_twice() {
    rejects(
        "linear struct Tx {} fn consume(sink tx: Tx) { let Tx {} = tx; } fn main() { let tx = Tx {}; consume(sink tx); consume(sink tx); }",
        "E0303",
    );
}
#[test]
fn branch_exhaustion() {
    rejects(
        "linear struct Tx {} fn consume(sink tx: Tx) { let Tx {} = tx; } fn bad(b: bool) { let tx = Tx {}; if b { consume(sink tx); } else { () } }",
        "E0309",
    );
}
#[test]
fn mode_crossing() {
    let w = checked("linear struct Tx {} affine struct Buffer {} fn main() {} ");
    assert_eq!(
        w.mode(
            &Type::Named("Option".into(), vec![Type::named("u64")]),
            &Default::default()
        ),
        Mode::Copy
    );
    assert_eq!(
        w.mode(
            &Type::Named("Option".into(), vec![Type::named("Buffer")]),
            &Default::default()
        ),
        Mode::Affine
    );
    assert_eq!(
        w.mode(
            &Type::Named("Option".into(), vec![Type::named("Tx")]),
            &Default::default()
        ),
        Mode::Linear
    );
}
#[test]
fn generic_identity() {
    assert_eq!(
        run("fn identity[T](sink x: T) -> T { x } fn main() -> u64 { identity[u64](42) }"),
        "42"
    );
}
#[test]
fn modal_bound() {
    rejects(
        "linear struct Tx {} fn copy_only[T: mod copy](x: T) {} fn bad(sink tx: Tx) { copy_only[Tx](in tx); let Tx {} = tx; }",
        "E0205",
    );
}
#[test]
fn projection_disjoint() {
    assert_eq!(
        run(
            "copy struct Pair { left: i64, right: i64 } fn update(inout p.{left}: Pair, in q: i64) { p.left = wrapping_add(p.left, q); } fn main() -> i64 { let p = Pair { left: 1, right: 2 }; update(inout p, in p.right); p.left }"
        ),
        "3"
    );
}
#[test]
fn projection_overlap() {
    rejects(
        "copy struct Pair { left: i64, right: i64 } fn update(inout p: Pair, in q: i64) {} fn bad(inout p: Pair) { update(inout p, in p.left); }",
        "E0301",
    );
}
#[test]
fn shared_mutation() {
    rejects("fn bad(in x: i64) { x = 2; }", "E0307");
}
#[test]
fn abortable_live_linear() {
    rejects(
        "linear struct Tx {} fn bad(sink tx: Tx) eff [Fail[i64]] { Fail::raise(1); let Tx {} = tx; }",
        "E0402",
    );
}
#[test]
fn unhandled_effect() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() -> i64 { Ask::ask() }",
        "E0401",
    );
}
#[test]
fn linear_handler() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), Ask::ask()) } with { return x => x, ask(k) => k(21), } }"
        ),
        "42"
    );
}
#[test]
fn stateful_handler() {
    assert_eq!(
        run(
            "linear effect Counter { fn next() -> i64; } fn main() -> i64 { handle Counter state s = 0 in { wrapping_add(Counter::next(), Counter::next()) } with { return (x, s) => wrapping_add(x, s), next(s, k) => k(s, wrapping_add(s, 1)), } }"
        ),
        "3"
    );
}
#[test]
fn affine_handler_abort() {
    assert_eq!(
        run(
            "effect Stop { fn stop() -> Never; } fn main() -> i64 { handle Stop in { Stop::stop(); 99 } with { return x => x, stop(k) => 7, } }"
        ),
        "7"
    );
}
#[test]
fn linear_continuation_drop() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() -> i64 { handle Ask in { Ask::ask() } with { return x => x, ask(k) => 0, } }",
        "E0302",
    );
}
#[test]
fn continuation_twice() {
    rejects(
        "effect Ask { fn ask() -> i64; } fn bad() -> i64 { handle Ask in { Ask::ask() } with { return x => x, ask(k) => { k(1); k(2) }, } }",
        "E0303",
    );
}
#[test]
fn lexical_labels() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { wrapping_add(outer.ask(), inner.ask()) } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(40), } }"
        ),
        "42"
    );
}
#[test]
fn closure_capture() {
    assert_eq!(
        run("fn main() -> i64 { let x = 40; let f = |y: i64| wrapping_add(x, y); f(2) }"),
        "42"
    );
}
#[test]
fn borrowed_capture() {
    rejects("fn bad(in x: i64) { let f = || x; }", "E0310");
}
#[test]
fn enum_match() {
    assert_eq!(
        run("fn main() -> i64 { let x = Some(42); match x { Some(v) => v, None => 0, } }"),
        "42"
    );
}
#[test]
fn enum_coverage() {
    rejects(
        "fn bad(x: Option[i64]) -> i64 { match x { Some(v) => v, } }",
        "E0212",
    );
}
#[test]
fn try_handler() {
    assert_eq!(
        run(
            "fn divide(a: i64, b: i64) -> Result[i64, i64] eff [Fail[ArithOverflow]] { if b == 0 { Err(7) } else { Ok(a / b) } } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Fail[i64] in { divide(10, 0)? } with { return x => x, raise(err, k) => err, } } with { return x => x, raise(overflow, k) => 0, } }"
        ),
        "7"
    );
}
#[test]
fn inout_call() {
    let w = checked(
        "fn increment(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let x = 41; increment(inout x); x }",
    );
    assert!(matches!(
        Runtime::new(&w).run("main", vec![]).unwrap(),
        Value::Int(42)
    ));
}

#[test]
fn nested_loan_conflict() {
    rejects(
        "fn mutate(inout x: i64) -> i64 { x += 1; x } fn use_both(inout x: i64, y: i64) {} fn bad() { let x = 0; use_both(inout x, mutate(inout x)); }",
        "E0301",
    );
}
#[test]
fn shadow_scope() {
    assert_eq!(
        run("fn main() -> i64 { let x = 42; { let x = 7; x }; x }"),
        "42"
    );
}
#[test]
fn linear_shadow_inner() {
    rejects(
        "linear struct Tx {} fn main() { let x = 1; { let x = Tx {}; }; }",
        "E0302",
    );
}
#[test]
fn match_shadow_scope() {
    assert_eq!(
        run("fn main() -> i64 { let x = 42; match 7 { x => x, }; x }"),
        "42"
    );
}
#[test]
fn nested_coverage_reject() {
    rejects(
        "fn bad(x: Option[i64]) -> i64 { match x { Some(0) => 0, None => 1, } }",
        "E0212",
    );
}
#[test]
fn nested_coverage_accept() {
    assert_eq!(
        run(
            "fn main() -> i64 { let x = Some(true); match x { Some(true) => 42, Some(false) => 1, None => 0, } }"
        ),
        "42"
    );
}
#[test]
fn refutable_nested_let() {
    rejects(
        "copy struct Box { value: bool } fn bad(x: Box) { let Box { value: true } = x; }",
        "E0210",
    );
}
#[test]
fn unknown_closure_sink_mode() {
    rejects(
        "linear struct Tx {} fn main() { let f = |sink x| (); f(Tx {}); }",
        "E0102",
    );
}
#[test]
fn inferred_payload_mode_required() {
    rejects("fn main() { let x = None; }", "E0208");
}
#[test]
fn capability_escape() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() -> fn() -> i64 eff [Ask] { handle outer: Ask in { || eff [Ask] Ask::ask() } with { return f => f, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn effect_row_polymorphism() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply(|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}
#[test]
fn effect_row_cannot_hide_effects() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad[r: eff]() eff [r] { Ask::ask(); }",
        "E0401",
    );
}
#[test]
fn row_instantiation_checks_caller() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn bad() -> i64 { apply(|| eff [Ask] Ask::ask()) }",
        "E0401",
    );
}

#[test]
fn wrong_handler_payload_type() {
    rejects(
        "fn main() -> String { handle Fail[String] in { Fail::raise(42) } with { return x => x, raise(e, k) => e, } }",
        "E0200",
    );
}
#[test]
fn named_capability_parameter() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn get() -> i64 eff [local: Ask] { local.ask() } fn main() -> i64 { handle local: Ask in { get() } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}

#[test]
fn contextual_closure() {
    assert_eq!(
        run("fn main() -> i64 { let f: fn(i64) -> i64 = |x| wrapping_add(x, 1); f(41) }"),
        "42"
    );
}
#[test]
fn contextual_generic_constructor() {
    assert_eq!(
        run(
            "fn main() -> u8 { let x: Option[u8] = Some(255); match x { Some(n) => n, None => 0, } }"
        ),
        "255"
    );
}
#[test]
fn inferred_closure_effect() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let get = || Ask::ask(); handle Ask in { get() } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}
#[test]
fn cannot_erase_closure_effect() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() { let get: fn() -> i64 = || eff [Ask] Ask::ask(); }",
        "E0200",
    );
}
#[test]
fn never_is_directional() {
    rejects("fn impossible() -> Never { 42 }", "E0200");
}
#[test]
fn open_row_frame() {
    rejects(
        "linear struct Tx {} fn bad[r: eff](sink tx: Tx, sink callback: fn() eff [r]) eff [r] { callback(); let Tx {} = tx; }",
        "E0402",
    );
}
#[test]
fn annotated_capability_escape() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() -> fn() -> i64 eff [Ask] { handle Ask in { let f: fn() -> i64 eff [Ask] = || eff [Ask] Ask::ask(); f } with { return f => f, ask(k) => k(42), } }",
        "E0403",
    );
}

#[test]
fn handler_pattern_shadows_outer_linear() {
    assert_eq!(
        run(
            "linear struct Tx {} linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = Tx {}; let value = handle Ask in { 42 } with { return x => x, ask(k) => k(1), }; let Tx {} = x; value }"
        ),
        "42"
    );
}
#[test]
fn reject_relaxing_field_mode() {
    rejects(
        include_str!("../examples/rejected/specification_counterexample.ves"),
        "E0105",
    );
}

#[test]
fn linear_temporary_across_aborting_argument() {
    rejects(
        "linear struct Tx {} fn consume(sink tx: Tx, number: i64) { let Tx {} = tx; } fn bad() eff [Fail[i64]] { consume(Tx {}, Fail::raise(1)); }",
        "E0410",
    );
}
#[test]
fn cannot_borrow_linear_temporary() {
    rejects(
        "linear struct Tx {} fn observe(in tx: Tx) {} fn main() { observe(Tx {}); }",
        "E0302",
    );
}
#[test]
fn cannot_project_from_linear_temporary() {
    rejects(
        "linear struct Tx { id: u64 } fn main() -> u64 { Tx { id: 42 }.id }",
        "E0306",
    );
}
#[test]
fn consume_linear_temporary() {
    assert_eq!(
        run(
            "linear struct Tx { id: u64 } fn consume(sink tx: Tx) -> u64 { let Tx { id } = tx; id } fn main() -> u64 { consume(Tx { id: 42 }) }"
        ),
        "42"
    );
}
#[test]
fn linear_nested_enum_cfg() {
    assert_eq!(
        run(
            "linear struct Tx { id: u64 } fn main() -> u64 { let tx = Some(Tx { id: 42 }); match tx { Some(Tx { id }) => id, None => 0, } }"
        ),
        "42"
    );
}

#[test]
fn cannot_shadow_builtin_type() {
    rejects(
        "linear struct Tx {} copy struct Unit { tx: Tx } fn main() { let x = Unit { tx: Tx {} }; }",
        "E0100",
    );
    rejects(
        "linear struct Tx {} affine struct String { tx: Tx } fn main() { let x = String { tx: Tx {} }; }",
        "E0100",
    );
}

#[test]
fn closure_keeps_outer_lexical_capability() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { let get = || eff [Ask] Ask::ask(); handle inner: Ask in { get() } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}
#[test]
fn factory_keeps_outer_lexical_capability() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] eff [Ask] { || eff [Ask] Ask::ask() } fn main() -> i64 { handle outer: Ask in { let get = make(); handle inner: Ask in { get() } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}

#[test]
fn factory_capability_cannot_escape() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] eff [Ask] { || eff [Ask] Ask::ask() } fn bad() -> fn() -> i64 eff [Ask] { handle Ask in { make() } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn indirect_factory_capability_cannot_escape() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] eff [Ask] { || eff [Ask] Ask::ask() } fn bad() -> fn() -> i64 eff [Ask] { let factory = make; handle Ask in { factory() } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn latent_factory_capability_cannot_escape() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> fn() -> i64 eff [Ask] eff [Ask] { || eff [Ask] || eff [Ask] Ask::ask() } fn bad() -> fn() -> i64 eff [Ask] { let factory = make(); handle Ask in { factory() } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn pure_factory_has_latent_capability() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn main() -> i64 { let get = make(); handle Ask in { get() } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}

#[test]
fn borrowed_effect_argument_cannot_become_owned() {
    rejects(
        "affine struct Token {} linear effect Inspect { fn inspect(in token: Token); } fn consume(sink token: Token) {} fn main() { let token = Token {}; handle Inspect in { Inspect::inspect(in token); consume(sink token); } with { return x => x, inspect(t, k) => { consume(sink t); k(()) }, } }",
        "E0305",
    );
}
#[test]
fn borrowed_effect_argument_is_not_a_linear_obligation() {
    checked(
        "linear struct Token {} linear effect Inspect { fn inspect(in token: Token); } fn main() { let token = Token {}; handle Inspect in { Inspect::inspect(in token); let Token {} = token; } with { return x => x, inspect(_, k) => k(()), } }",
    );
}
#[test]
fn mutable_effect_argument_updates_caller() {
    assert_eq!(
        run(
            "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 40; handle Inc in { Inc::inc(inout x); x } with { return n => n, inc(n, k) => { n = wrapping_add(n, 2); k(()) }, } }"
        ),
        "42"
    );
}
#[test]
fn mutable_effect_pattern_aliases_nested_field() {
    assert_eq!(
        run(
            "copy struct Pair { x: i64, y: i64 } linear effect Inc { fn inc(inout p: Pair); } fn main() -> i64 { let p = Pair { x: 40, y: 0 }; handle Inc in { Inc::inc(inout p); p.x } with { return n => n, inc(Pair { x, y }, k) => { x = wrapping_add(x, 2); k(()) }, } }"
        ),
        "42"
    );
}

#[test]
fn higher_order_callback_binds_incoming_capability() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn apply(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle inner: Ask in { f() } with { return x => x, ask(k) => k(2), } } fn main() -> i64 { let f = make(); handle outer: Ask in { apply(sink f) } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}
#[test]
fn higher_order_function_value_binds_incoming_capability() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn ask() -> i64 eff [Ask] { Ask::ask() } fn apply(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle inner: Ask in { f() } with { return x => x, ask(k) => k(2), } } fn main() -> i64 { let f = ask; handle outer: Ask in { apply(sink f) } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}

#[test]
fn explicit_pure_closure_cannot_hide_ambient_capability() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn bad() -> fn() -> i64 { handle Ask in { || eff [] Ask::ask() } with { return f => f, ask(k) => k(42), } }",
        "E0401",
    );
}
#[test]
fn callback_parameter_modes_are_contravariant() {
    rejects(
        "fn only_copy(sink f: fn@copy()) { f(); f(); } fn use(sink f: fn(sink fn@linear())) { f(sink ||@linear ()); } fn bad() { use(sink only_copy); }",
        "E0200",
    );
}

#[test]
fn annotated_container_cannot_hide_capability() {
    rejects(
        "copy struct Box[T] { value: T } linear effect Ask { fn ask() -> i64; } fn bad() -> Box[fn() -> i64 eff [Ask]] { handle Ask in { let boxed: Box[fn() -> i64 eff [Ask]] = Box { value: || eff [Ask] Ask::ask() }; boxed } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn explicit_generic_argument_cannot_hide_capability() {
    rejects(
        "fn id[T](sink x: T) -> T { x } linear effect Ask { fn ask() -> i64; } fn bad() -> fn() -> i64 eff [Ask] { handle Ask in { id[fn() -> i64 eff [Ask]](|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}

#[test]
fn copy_closure_invocation_snapshots_captures() {
    assert_eq!(
        run("fn main() -> i64 { let x = 0; let f = || { x += 1; x }; wrapping_add(f(), f()) }"),
        "2"
    );
}
#[test]
fn copy_closure_copies_do_not_share_mutable_captures() {
    assert_eq!(
        run(
            "fn main() -> i64 { let x = 0; let f = || { x += 1; x }; let g = f; wrapping_add(f(), g()) }"
        ),
        "2"
    );
}

#[test]
fn recursive_type_argument_changes_preserve_linear_payload() {
    rejects(
        "linear struct Ticket {} copy enum Weird[T] { Done(T), Next(Weird[Ticket]) } fn bad() { let hidden: Weird[u64] = Next(Done(Ticket {})); }",
        "E0302",
    );
    let world = checked(
        "linear struct Ticket {} copy enum Weird[T] { Done(T), Next(Weird[Ticket]) } fn main() {}",
    );
    assert_eq!(
        world.mode(
            &Type::Named("Weird".into(), vec![Type::named("u64")]),
            &Default::default()
        ),
        Mode::Linear
    );
}
#[test]
fn nested_type_arguments_obey_declaration_bounds() {
    rejects(
        "linear struct Ticket {} copy struct CopyOnly[T: mod copy] { value: T @@ copy } copy struct Bad { hidden: CopyOnly[Ticket] } fn main() {}",
        "E0205",
    );
}

#[test]
fn assignment_cannot_export_handler_capability() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn bad() { let f = make(); handle Ask in { f = || eff [Ask] Ask::ask(); } with { return x => x, ask(k) => k(42), }; }",
        "E0403",
    );
}
#[test]
fn mutable_argument_cannot_export_handler_capability() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn install(inout f: fn() -> i64 eff [Ask]) eff [Ask] { f = || eff [Ask] Ask::ask(); } fn bad() { let f = make(); handle Ask in { install(inout f); } with { return x => x, ask(k) => k(42), }; }",
        "E0403",
    );
}
#[test]
fn assignment_specializes_function_value_evidence() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn ask() -> i64 eff [Ask] { Ask::ask() } fn main() -> i64 { handle outer: Ask in { let f = || eff [Ask] Ask::ask(); f = ask; handle inner: Ask in { f() } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}

#[test]
fn function_types_validate_effect_rows() {
    rejects("fn bad(f: fn() eff [Missing]) {}", "E0400");
    rejects("fn bad(f: fn() eff [r]) {}", "E0406");
    rejects("fn bad() eff [r] {}", "E0406");
}
#[test]
fn generic_argument_kinds_and_names_are_checked() {
    rejects(
        "fn ignore[T]() {} fn main() { ignore[Missing](); }",
        "E0101",
    );
    rejects("fn bad[T,T]() {}", "E0100");
    rejects("fn bad(x: u64[i64]) {}", "E0101");
}

#[test]
fn equality_cannot_discard_linear_instances() {
    rejects(
        "linear struct Ticket {} fn main() -> bool { Ticket {} == Ticket {} }",
        "E0302",
    );
    rejects(
        "fn equal[T](sink a: T, sink b: T) -> bool { a == b }",
        "E0302",
    );
}

#[test]
fn explicit_effect_row_argument() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply[Ask](|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}
#[test]
fn explicit_named_effect_row_argument() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { apply[outer: Ask](|| eff [outer: Ask] outer.ask()) } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(42), } }"
        ),
        "42"
    );
}
#[test]
fn explicit_open_row_cannot_widen() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn bad[r: eff]() -> i64 eff [r] { apply[r](|| eff [Ask] Ask::ask()) }",
        "E0406",
    );
}

#[test]
fn closure_explicit_row_selects_lexical_evidence() {
    assert_eq!(
        run(
            "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { let get = || eff [outer: Ask] Ask::ask(); get() } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(1), } }"
        ),
        "1"
    );
}

#[test]
fn continuation_effects_protect_clause_linear_resources() {
    rejects(
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Stop in { handle Ask in { Ask::ask(); if true { Stop::stop() } else { 0 } } with { return x => x, ask(k) => { let ticket = Ticket {}; let value = k(1); let Ticket {} = ticket; value }, } } with { return x => x, stop(k) => 42, } }",
        "E0402",
    );
}
#[test]
fn monomorphic_callback_parameter_cannot_hide_capture() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn keep(sink f: fn() -> i64 eff [Ask]) -> fn() -> i64 eff [Ask] { f } fn bad() -> fn() -> i64 eff [Ask] { handle Ask in { keep(|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
        "E0403",
    );
}
#[test]
fn outer_operation_cannot_export_inner_capability() {
    rejects(
        "linear effect Ask { fn ask() -> i64; } linear effect Send { fn send(sink f: fn() -> i64 eff [Ask]); } fn bad() { handle Send in { handle Ask in { Send::send(|| eff [Ask] Ask::ask()); } with { return x => x, ask(k) => k(42), } } with { return x => x, send(f,k) => k(()), } }",
        "E0403",
    );
}

#[test]
fn closure_cfg_checks_linear_temporaries() {
    rejects(
        "linear struct Ticket {} fn consume(sink t: Ticket, value: i64) { let Ticket {} = t; } fn bad() { let f = || eff [Fail[i64]] consume(Ticket {}, Fail::raise(0)); }",
        "E0410",
    );
}
#[test]
fn handler_clause_cfg_checks_linear_temporaries() {
    rejects(
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn consume(sink t: Ticket, value: i64) -> i64 { let Ticket {} = t; value } fn main() -> i64 { handle Stop in { handle Ask in { Ask::ask() } with { return x => x, ask(k) => k(consume(Ticket {}, Stop::stop())), } } with { return x => x, stop(k) => 42, } }",
        "E0410",
    );
}
#[test]
fn nested_handler_cfg_checks_outer_argument_temporaries() {
    rejects(
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn consume(sink t: Ticket, value: i64) { let Ticket {} = t; } fn main() { handle Stop in { consume(Ticket {}, handle Ask in { Stop::stop() } with { return x => x, ask(k) => k(0), }); } with { return x => x, stop(k) => (), } }",
        "E0410",
    );
}

#[test]
fn outer_abort_cannot_discard_linear_handler_state() {
    rejects(
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Stop in { handle Ask state ticket = Ticket {} in { if true { Stop::stop() } else { 0 } } with { return (value,ticket) => { let Ticket {} = ticket; value }, ask(ticket,k) => k(0,ticket), } } with { return x => x, stop(k) => 42, } }",
        "E0402",
    );
}
#[test]
fn own_aborting_operation_may_consume_linear_state() {
    assert_eq!(
        run(
            "linear struct Ticket {} effect Stop { fn stop() -> Never; } fn main() -> i64 { handle Stop state ticket = Ticket {} in { Stop::stop() } with { return (value,ticket) => { let Ticket {} = ticket; value }, stop(ticket,k) => { let Ticket {} = ticket; 42 }, } }"
        ),
        "42"
    );
}
#[test]
fn handler_initializer_cfg_checks_linear_temporaries() {
    rejects(
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn consume(sink t: Ticket, value: i64) -> i64 { let Ticket {} = t; value } fn main() -> i64 { handle Stop in { handle Ask state counter = consume(Ticket {}, Stop::stop()) in { 0 } with { return (value,counter) => value, ask(counter,k) => k(0,counter), } } with { return x => x, stop(k) => 42, } }",
        "E0410",
    );
}

#[test]
fn local_annotations_and_closure_types_are_validated() {
    rejects("fn main() { let f = |x: Missing| (); }", "E0101");
    rejects(
        "copy struct Phantom[T] {} fn main() { let x: Phantom[Missing] = Phantom {}; }",
        "E0101",
    );
    rejects(
        "copy struct Phantom[T] {} fn main() { Phantom[Missing](); }",
        "E0101",
    );
    rejects(
        "linear effect Ask[T] { fn ask(); } fn main() { handle Ask[Missing] in { () } with { return x => x, ask(k) => k(()), } }",
        "E0101",
    );
}

#[test]
fn linear_handler_state_moves_from_owned_binding() {
    assert_eq!(
        run(
            "linear struct Ticket {} effect Stop { fn stop() -> Never; } fn main() -> i64 { let ticket = Ticket {}; handle Stop state resource = ticket in { Stop::stop() } with { return (value,resource) => { let Ticket {} = resource; value }, stop(resource,k) => { let Ticket {} = resource; 42 }, } }"
        ),
        "42"
    );
}

#[test]
fn primitive_failures_cannot_abandon_linear_resources() {
    rejects(
        "linear struct Ticket {} fn bad(b: i64) eff [Fail[RuntimeError]] { let ticket = Ticket {}; 1 / b; let Ticket {} = ticket; }",
        "E0402",
    );
    rejects(
        "linear struct Ticket {} fn use(sink t: Ticket, x: i64) { let Ticket {} = t; } fn bad(b: i64) eff [Fail[RuntimeError]] { use(Ticket {}, 1 / b); }",
        "E0410",
    );
    rejects(
        "linear struct Ticket {} linear effect E { fn op() -> i64; } fn bad(b: i64) -> i64 eff [Fail[RuntimeError]] { handle E state ticket = Ticket {} in { 1 / b } with { return (x, ticket) => { let Ticket {} = ticket; x }, op(ticket, k) => k(1, ticket), } }",
        "E0402",
    );
    rejects("fn bad(b: i64) -> i64 { 1 / b }", "E0401");
    rejects(
        "fn zero(inout b: i64) { b = 0; } fn bad() -> i64 { let b = 1; zero(inout b); 1 / b }",
        "E0401",
    );
    rejects("fn bad(i: i64) -> i64 { let xs = (1, 2); xs[i] }", "E0401");
}

#[test]
fn nonzero_facts_follow_control_and_mutation() {
    assert_eq!(
        run(
            "linear struct Ticket {} fn divide(b: i64) -> i64 { if b == 0 { 42 } else { let t = Ticket {}; let x = 84 / b; let Ticket {} = t; x } } fn main() -> i64 { divide(2) }"
        ),
        "42"
    );
    rejects(
        "fn bad(b: i64) -> i64 { if b != 0 { let divisor = b; divisor = 0; 1 / divisor } else { 0 } }",
        "E0401",
    );
    rejects(
        "fn bad() -> i64 { let divisor: u8 = 255; divisor += 1; 1 / divisor }",
        "E0401",
    );
    assert_eq!(
        run(
            "fn main() -> i64 { let divisor = 2; { let divisor = 1; divisor = 0; }; 84 / divisor }"
        ),
        "42"
    );
}

#[test]
fn local_values_disambiguate_indexing_from_rows() {
    checked(
        "fn test[r: eff](r: i64) -> i64 eff [Fail[RuntimeError], r] { let r = r; let f = |xs: (i64, i64)| xs[r]; f((20, 42)) }",
    );
    checked(
        "fn test[r: eff](r: i64, xs: (i64, i64)) -> i64 eff [Fail[RuntimeError], r] { match Some(xs) { Some(values) => values[r], None => 0, } }",
    );
    checked(
        "fn test[r: eff]() -> i64 eff [r] { let r = 1; let f = |values: (i64, i64)| values[r]; f((20, 42)) }",
    );
}

#[test]
fn resumption_invalidates_borrowed_nonzero_facts() {
    rejects(
        "linear effect E { fn op(x: i64); } fn main() -> i64 { let x = 1; handle E in { E::op(x); x = 0; 42 } with { return result => result, op(x, k) => if x != 0 { k(()); 1 / x } else { k(()); 0 }, } }",
        "E0401",
    );
}

#[test]
fn external_linear_effects_own_and_resume_the_entire_machine() {
    use vesper::runtime::Execution;
    let world = checked(
        "linear struct Ticket { id: i64 } linear effect IO { fn read() -> i64; } fn main() -> i64 eff [IO] { let ticket = Ticket { id: 1 }; let first = IO::read(); let second = IO::read(); let Ticket { id } = ticket; wrapping_add(wrapping_add(first, second), id) }",
    );
    let execution = Runtime::new(&world).begin_external("main", vec![]).unwrap();
    let Execution::Suspended(first) = execution else {
        panic!("expected a parked continuation")
    };
    assert_eq!(first.request().effect.name, "IO");
    assert_eq!(first.request().operation, "read");
    let Execution::Suspended(second) = first.resume(Value::Int(20)).unwrap() else {
        panic!("expected the second parked continuation")
    };
    let Execution::Complete(value) = second.resume(Value::Int(21)).unwrap() else {
        panic!("unexpected further operation")
    };
    assert_eq!(value.to_string(), "42");
}

#[test]
fn external_mutable_places_and_lexical_capabilities_survive_parking() {
    use vesper::runtime::Execution;
    let world = checked(
        "linear effect IO { fn read(inout count: i64) -> i64; } fn helper(inout x: i64) -> i64 eff [IO] { IO::read(inout x) } fn main() -> i64 eff [IO] { let x = 0; let value = helper(inout x); wrapping_add(x, value) }",
    );
    let Execution::Suspended(mut pending) =
        Runtime::new(&world).begin_external("main", vec![]).unwrap()
    else {
        panic!("expected suspension")
    };
    pending.write_argument(0, Value::Int(22)).unwrap();
    let Execution::Complete(value) = pending.resume(Value::Int(20)).unwrap() else {
        panic!("unexpected further operation")
    };
    assert_eq!(value.to_string(), "42");
    let world = checked(
        "effect Abort { fn abort() -> Never; } fn main() -> i64 eff [Abort] { Abort::abort() }",
    );
    assert!(Runtime::new(&world).begin_external("main", vec![]).is_err());
}

#[test]
fn branch_joins_resolve_empty_constructors_and_callback_rows() {
    assert_eq!(
        run(
            "fn main() -> i64 { let result = if false { None } else { Some(42) }; match result { Some(n) => n, None => 0, } }"
        ),
        "42"
    );
    rejects(
        "linear effect E { fn op() -> i64; } fn bad(i: i64) -> fn() -> i64 { handle E in { let f = || 0; let g = || E::op(); (f, g)[i] } with { return f => f, op(k) => k(42), } }",
        "E0403",
    );
}
