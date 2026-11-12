use vesper::{check, native, parser, runtime::Runtime};
fn differential(source: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let interpreted = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted);
    let (obj, clif) = native::object(&c.world, "main").unwrap();
    assert!(!obj.is_empty());
    assert!(clif.contains("function"));
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(&arm[..4], b"\x7fELF");
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183);
}
#[test]
fn scalar() {
    differential("fn main() -> i64 { 2 + 3 * 4 }");
}

#[test]
fn native_inout() {
    differential(
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let x = 41; inc(inout x); x }",
    );
}

#[test]
fn native_evidence_specialization() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { wrapping_add(Ask::ask(), Ask::ask()) } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
    );
}

#[test]
fn native_linear_transaction() {
    differential(include_str!("../examples/transaction.ves"));
}
#[test]
fn native_projected_broker() {
    differential(include_str!("../examples/projections.ves"));
}
#[test]
fn native_record_return() {
    differential("copy struct Pair { x: i64, y: i64 } fn main() -> Pair { Pair { y: 2, x: 1 } }");
}
#[test]
fn native_record_copy() {
    differential(
        "copy struct Pair { x: i64, y: i64 } fn main() -> i64 { let a = Pair { x: 1, y: 2 }; let b = a; b.x = 42; a.x }",
    );
}
#[test]
fn native_record_parameter() {
    differential(
        "copy struct Pair { x: i64, y: i64 } fn sum(in p: Pair) -> i64 { wrapping_add(p.x, p.y) } fn main() -> i64 { let p = Pair { x: 20, y: 22 }; sum(in p) }",
    );
}
#[test]
fn native_enum() {
    differential(
        "fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } } fn main() -> i64 { match make(42) { Some(v) => v, None => 0, } }",
    );
}
#[test]
fn native_enum_return() {
    differential("fn main() -> Option[u8] { Some(255) }");
}
#[test]
fn native_nested_pattern() {
    differential(
        "fn main() -> i64 { let x = Some(Some(true)); match x { Some(Some(true)) => 42, Some(Some(false)) => 0, Some(None) => 1, None => 2, } }",
    );
}
#[test]
fn native_generic_record() {
    differential(
        "copy struct Pair { x: i64, y: i64 } fn id[T](sink value: T) -> T { value } fn main() -> Pair { id[Pair](Pair { x: 1, y: 2 }) }",
    );
}
#[test]
fn native_generic_float() {
    differential("fn id[T](sink value: T) -> T { value } fn main() -> f64 { id[f64](42.0) }");
}
#[test]
fn native_generic_wrapper() {
    differential(
        "copy struct Box[T] { value: T } fn wrap[T](sink x: T) -> Box[T] { Box { value: x } } fn main() -> Box[u64] { wrap[u64](42) }",
    );
}
#[test]
fn native_structural_equality() {
    differential(
        "copy struct Pair { x: f64, y: i64 } fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> bool { let a = Pair { x: 1.0, y: 2 }; same[Pair](a, a) }",
    );
}
#[test]
fn native_tuple_return() {
    differential("fn main() -> (i64, bool) { (42, true) }");
}
#[test]
fn native_tuple_pattern() {
    differential("fn main() -> i64 { let (x, y) = (20, 22); wrapping_add(x, y) }");
}
#[test]
fn native_tuple_index() {
    differential("fn main() -> i64 { let xs = (20, 42, 3); let index = 1; xs[index] }");
}

#[test]
fn native_fail() {
    differential(include_str!("../examples/fail.ves"));
}
#[test]
fn native_abort() {
    differential(
        "effect Stop { fn stop() -> Never; } fn main() -> i64 { handle Stop in { Stop::stop(); 99 } with { return x => x, stop(k) => 7, } }",
    );
}
#[test]
fn native_abort_float() {
    differential(
        "effect Stop { fn stop() -> Never; } fn main() -> f64 { handle Stop in { Stop::stop(); 99.0 } with { return x => x, stop(k) => 7.0, } }",
    );
}
#[test]
fn native_try_success() {
    differential(
        "fn main() -> u8 { handle Fail[i64] in { let x: Result[u8, i64] = Ok(42); x? } with { return x => x, raise(e, k) => 0, } }",
    );
}
#[test]
fn native_abort_record() {
    differential(
        "copy struct Receipt { id: u64 } effect Stop { fn stop() -> Never; } fn main() -> Receipt { handle Stop in { Stop::stop(); Receipt { id: 99 } } with { return x => x, stop(k) => Receipt { id: 42 }, } }",
    );
}

#[test]
fn handler_captures_are_snapshots() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let result = handle Ask in { x = 40; Ask::ask() } with { return value => wrapping_add(value, x), ask(k) => { x += 1; k(x) }, }; wrapping_add(x, result) }",
    );
}
#[test]
fn handler_record_captures_are_snapshots() {
    differential(
        "copy struct Config { x: i64 } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let config = Config { x: 1 }; let result = handle Ask in { config.x = 40; wrapping_add(Ask::ask(), Ask::ask()) } with { return value => wrapping_add(value, config.x), ask(k) => { config.x = wrapping_add(config.x, 1); k(config.x) }, }; wrapping_add(config.x, result) }",
    );
}

#[test]
fn native_float_remainder() {
    differential("fn main() -> f64 { 7.5 % 2.0 }");
}
#[test]
fn native_scientific_literal() {
    differential("fn main() -> f64 { 1e3 + 2.5e-1 }");
}
#[test]
fn native_negative_pattern() {
    differential("fn main() -> i64 { match -1 { -1 => 42, _ => 0, } }");
}
#[test]
fn native_nested_inout() {
    differential(
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn twice(inout x: i64) { inc(inout x); inc(inout x); } fn main() -> i64 { let x = 40; twice(inout x); x }",
    );
}
#[test]
fn recursion() {
    differential(
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { wrapping_add(fib(wrapping_sub(n, 1)), fib(wrapping_sub(n, 2))) } } fn main() -> i64 { fib(10) }",
    );
}
#[test]
fn native_mutation() {
    differential("fn main() -> i64 { let x = 1; if true { x += 41; } else { x -= 1; }; x }");
}
#[test]
fn native_shadow() {
    differential("fn main() -> i64 { let x = 42; { let x = 2; x }; x }");
}
#[test]
fn native_generic() {
    differential("fn id[T](sink x: T) -> T { x } fn main() -> i64 { id[i64](42) }");
}
#[test]
fn native_float() {
    differential("fn double(x: f64) -> f64 { x * 2.0 } fn main() -> f64 { double(21.0) }");
}
#[test]
fn native_match() {
    differential("fn main() -> i64 { match 2 { 0 => 1, 1 => 2, _ => 42, } }");
}
#[test]
fn native_handler() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), Ask::ask()) } with { return x => x, ask(k) => k(21), } }",
    );
}
#[test]
fn native_state() {
    differential(
        "linear effect Counter { fn next() -> i64; } fn main() -> i64 { handle Counter state s = 0 in { wrapping_add(Counter::next(), Counter::next()) } with { return (x, s) => wrapping_add(x, s), next(s, k) => k(s, wrapping_add(s, 1)), } }",
    );
}
#[test]
fn native_shortcircuit() {
    differential("fn main() -> bool { false && (1 / 0 == 0) }");
}
#[test]
fn optimization_preserves_calls() {
    let source = "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(wrapping_mul(Ask::ask(), 0), 42) } with { return x => x, ask(k) => k(7), } }";
    let mut p = parser::parse(source);
    let c = check::check(&p.module);
    vesper::optimize::optimize(&mut p.module, &c.expression_types);
    let optimized = check::check(&p.module);
    assert!(
        optimized.diagnostics.is_empty(),
        "{:?}",
        optimized.diagnostics
    );
    assert_eq!(
        Runtime::new(&optimized.world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        "42"
    );
    let ir = vesper::ir::lower(&optimized);
    assert!(!ir.functions["main"].blocks.is_empty());
}

#[test]
fn native_mutable_effect_argument() {
    differential(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 40; handle Inc in { Inc::inc(inout x); x } with { return n => n, inc(n, k) => { n = wrapping_add(n, 2); k(()) }, } }",
    );
}
#[test]
fn native_mutable_effect_pattern() {
    differential(
        "copy struct Pair { x: i64, y: i64 } linear effect Inc { fn inc(inout p: Pair); } fn main() -> i64 { let p = Pair { x: 40, y: 0 }; handle Inc in { Inc::inc(inout p); p.x } with { return n => n, inc(Pair { x, y }, k) => { x = wrapping_add(x, 2); k(()) }, } }",
    );
}

#[test]
fn native_named_evidence_and_helper_cfg() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn get(b: bool) -> i64 eff [outer: Ask] { if b { outer.ask() } else { wrapping_add(outer.ask(), 1) } } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { wrapping_add(get(true), inner.ask()) } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(40), } }",
    );
}
#[test]
fn native_handler_clause_forwards_outer_evidence() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { inner.ask() } with { return x => x, ask(k) => k(wrapping_add(outer.ask(), 2)), } } with { return x => x, ask(k) => k(40), } }",
    );
}

#[test]
fn native_closure_capture() {
    differential("fn main() -> i64 { let x = 40; let f = |y: i64| wrapping_add(x, y); f(2) }");
}
#[test]
fn native_closure_factory() {
    differential(
        "fn make(sink x: i64) -> fn(i64) -> i64 { |y| wrapping_add(x, y) } fn main() -> i64 { let f = make(40); f(2) }",
    );
}
#[test]
fn native_higher_order_closure() {
    differential(
        "fn apply(sink f: fn(i64) -> i64, x: i64) -> i64 { f(x) } fn main() -> i64 { apply(|x| wrapping_add(x, 2), 40) }",
    );
}
#[test]
fn native_closure_calls_captured_closure() {
    differential("fn main() -> i64 { let f = || 40; let g = || wrapping_add(f(), 2); g() }");
}
#[test]
fn native_closure_capture_is_a_snapshot() {
    differential(
        "fn main() -> i64 { let x = 0; let f = || { x += 1; x }; wrapping_add(f(), f()) }",
    );
}
#[test]
fn native_closure_branch_join() {
    differential(
        "fn main() -> i64 { let x = 40; let f = if true { |y: i64| wrapping_add(x, y) } else { |y: i64| wrapping_sub(x, y) }; f(2) }",
    );
}
#[test]
fn native_closure_mutable_parameter() {
    differential(
        "fn main() -> i64 { let inc = |inout x: i64| { x = wrapping_add(x, 1); x }; let x = 40; let y = inc(inout x); wrapping_sub(wrapping_add(x, y), 40) }",
    );
}
#[test]
fn native_function_value() {
    differential("fn answer() -> i64 { 42 } fn main() -> i64 { let f = answer; f() }");
}
#[test]
fn native_closure_in_record() {
    differential(
        "copy struct Callback { f: fn@copy() -> i64 } fn invoke(in c: Callback) -> i64 { c.f() } fn main() -> i64 { let x = 42; let c = Callback { f: || x }; invoke(in c) }",
    );
}

#[test]
fn native_effectful_closure() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { let f = || eff [Ask] Ask::ask(); f() } with { return x => x, ask(k) => k(42), } }",
    );
}
#[test]
fn native_effect_polymorphic_callback() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply(|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
    );
}
#[test]
fn native_higher_order_callback_keeps_incoming_evidence() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn apply(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle inner: Ask in { f() } with { return x => x, ask(k) => k(2), } } fn main() -> i64 { let f = make(); handle outer: Ask in { apply(sink f) } with { return x => x, ask(k) => k(1), } }",
    );
}

#[test]
fn native_factory_captures_are_independent() {
    differential(
        "fn make(sink x: i64) -> fn() -> i64 { || x } fn main() -> i64 { let f = make(20); let g = make(22); wrapping_add(f(), g()) }",
    );
}
#[test]
fn native_linear_closure() {
    differential(
        "linear struct Ticket { value: i64 } fn main() -> i64 { let ticket = Ticket { value: 42 }; let use = || { let Ticket { value } = ticket; value }; use() }",
    );
}
#[test]
fn native_dynamic_callback_index() {
    differential(
        "fn main() -> i64 { let f = || 40; let callbacks = (f, f); let g = || { let index = 1; wrapping_add(callbacks[index](), 2) }; g() }",
    );
}
#[test]
fn native_closure_returns_closure() {
    differential(
        "fn main() -> i64 { let factory = || { let x = 42; || x }; let f = factory(); f() }",
    );
}

#[test]
fn checked_primitive_failures() {
    differential(
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(0) } with { return x => x, raise(e, k) => match e { RuntimeError::DivisionByZero => 42, RuntimeError::IndexOutOfBounds => 0, }, } }",
    );
    differential(
        "fn index(i: i64) -> i64 eff [Fail[RuntimeError]] { let xs = (10, 20); xs[i] } fn main() -> i64 { handle Fail[RuntimeError] in { index(-1) } with { return x => x, raise(e, k) => match e { RuntimeError::IndexOutOfBounds => 42, RuntimeError::DivisionByZero => 0, }, } }",
    );
    differential(
        "fn rem(b: u64) -> u64 eff [Fail[RuntimeError]] { let a: u64 = 84; a %= b; a } fn main() -> u64 { handle Fail[RuntimeError] in { rem(0) } with { return x => x, raise(e, k) => 42, } }",
    );
    differential(
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(2) } with { return x => x, raise(e, k) => 0, } }",
    );
}

#[test]
fn native_non_tail_resumption() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), 1) } with { return x => x, ask(k) => { let x = k(20); wrapping_mul(x, 2) }, } }",
    );
}

#[test]
fn native_non_tail_continuation_frames() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let n = 2; handle Ask in { let x = 10; wrapping_add(wrapping_add(x, Ask::ask()), Ask::ask()) } with { return x => x, ask(k) => wrapping_add(n, k(10)), } }",
    );
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask state count = 0 in { wrapping_add(Ask::ask(), Ask::ask()) } with { return (x, count) => wrapping_add(x, count), ask(count, k) => { let x = k(10, wrapping_add(count, 1)); wrapping_add(x, 10) }, } }",
    );
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { if true { wrapping_add(Ask::ask(), 1) } else { 3 } } with { return x => wrapping_mul(x, 2), ask(k) => if false { wrapping_add(k(1), 1) } else { wrapping_add(k(20), 2) }, } }",
    );
    differential(
        "linear effect Ask { fn ask() -> i64; } fn resume[T](sink k: fn@linear(sink i64) -> T) -> T { k(20) } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), 1) } with { return x => x, ask(k) => wrapping_mul(resume(k), 2), } }",
    );
}

#[test]
fn native_non_tail_conditional_entry() {
    differential(
        "linear effect Ask { fn ask() -> i64; } fn choose(flag: bool, n: i64) -> i64 { let owned = n; handle Ask in { if flag { wrapping_add(Ask::ask(), 1) } else { 42 } } with { return x => x, ask(k) => wrapping_add(wrapping_mul(owned, 2), k(20)), } } fn main() -> i64 { choose(true, 21) }",
    );
}

#[test]
fn native_nested_non_tail_and_linear_state() {
    differential(
        "linear struct Ticket { id: i64 } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask state ticket = Ticket { id: 20 } in { wrapping_add(Ask::ask(), 1) } with { return (x, ticket) => { let Ticket { id } = ticket; wrapping_add(x, id) }, ask(ticket, k) => wrapping_add(k(1, ticket), 20), } }",
    );
    differential(
        "linear effect Outer { fn ask() -> i64; } linear effect Inner { fn get() -> i64; } fn main() -> i64 { handle Outer in { handle Inner in { wrapping_add(Inner::get(), 1) } with { return x => x, get(k) => { let value = Outer::ask(); wrapping_add(k(value), 1) }, } } with { return x => x, ask(k) => wrapping_mul(k(20), 2), } }",
    );
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle Ask in { wrapping_add(Ask::ask(), 1) } with { return x => x, ask(k) => { let value = outer.ask(); wrapping_add(k(value), 1) }, } } with { return x => x, ask(k) => wrapping_mul(k(20), 2), } }",
    );
}

#[test]
fn native_non_tail_with_place_arguments() {
    differential(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; let result = handle Mut in { let y = Mut::change(inout x); wrapping_add(x, y) } with { return v => v, change(x, k) => { x = wrapping_add(x, 1); let result = k(20); x = wrapping_add(x, 1); result }, }; wrapping_add(result, x) }",
    );
    differential(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), 1) } with { return x => x, ask(k) => { let resume = ||@linear k(20); let value = resume(); wrapping_mul(value, 2) }, } }",
    );
}

#[test]
fn native_inline_strings() {
    differential(r#"fn text() -> String { "hello, 世界" } fn main() -> String { text() }"#);
    differential(
        r#"fn text(sink s: String) -> String { s } fn main() -> bool { text("hello") == "hello" }"#,
    );
    differential(
        r#"fn main() -> i64 { let value = "hello"; match value { "world longer" => 0, "hello" => 42, _ => 1, } }"#,
    );
    differential(r#"fn main() -> String { let f = || "hello"; f() }"#);
    differential(r#"fn main() -> String { "" }"#);
}

#[test]
fn native_affine_continuation_wildcard() {
    differential(include_str!("../examples/checked_math.ves"));
}

#[test]
fn native_shared_effect_argument_after_resumption() {
    differential(
        "linear effect E { fn op(x: i64); } fn main() -> i64 { handle Fail[RuntimeError] in { let x = 1; handle E in { E::op(x); x = 0; 42 } with { return result => result, op(x, k) => if x != 0 { k(()); 1 / x } else { k(()); 0 }, } } with { return result => result, raise(error, _) => 42, } }",
    );
}
