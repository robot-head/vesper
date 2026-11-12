use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{check, lean, parser};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn validates(source: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let mut output = lean::extract(&c.world).unwrap();
    let expected = c
        .world
        .functions
        .get("main")
        .filter(|f| {
            f.params.is_empty()
                && f.effects.instances.is_empty()
                && matches!(f.result.name(), "u8" | "u32" | "u64" | "i64" | "bool")
        })
        .map(|_| {
            vesper::runtime::Runtime::new(&c.world)
                .run("main", vec![])
                .unwrap()
                .to_string()
        });
    if expected.is_some() {
        output.push_str("\n#eval Vesper.«main»\n");
    }
    assert!(
        !output.contains("sorry".to_string().as_str())
            || output.contains("No axioms or sorry placeholders")
    );
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &output).unwrap();
    let result = Command::new(compiler).arg(&path).output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{}\n{output}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    if let Some(expected) = expected {
        assert_eq!(
            String::from_utf8_lossy(&result.stdout)
                .trim()
                .lines()
                .last(),
            Some(expected.as_str()),
            "Lean result differs from the reference evaluator:\n{output}"
        );
    }
    fs::remove_file(path).unwrap();
}
#[test]
fn scalar() {
    validates("fn main() -> i64 { 42 }");
}
#[test]
fn recursive() {
    validates(include_str!("../examples/fibonacci.ves"));
}
#[test]
fn generic() {
    validates("fn id[T](sink x: T) -> T { x } fn main() -> i64 { id[i64](42) }");
}
#[test]
fn mutable() {
    validates("fn main() -> i64 { let x: i64 = 40; x += 2; x }");
}
#[test]
fn boolean() {
    validates("fn main() -> bool { 2 < 3 && 2 != 3 }");
}
#[test]
fn enum_match() {
    validates(
        "fn main() -> i64 { let x: Option[i64] = Some(42); match x { Some(v) => v, None => 0, } }",
    );
}
#[test]
fn struct_destructure() {
    validates(
        "linear struct Tx { id: u64 } fn main() -> u64 { let tx = Tx { id: 42 }; let Tx { id } = tx; id }",
    );
}
#[test]
fn effect_evidence() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { Ask::ask() + Ask::ask() }",
    );
}
#[test]
fn explicit_state() {
    validates("fn inc(inout x: i64) { x += 1; }");
}

#[test]
fn state_call() {
    validates("fn inc(inout x: i64) { x += 1; } fn main() -> i64 { let x = 41; inc(inout x); x }");
}
#[test]
fn method() {
    validates(include_str!("../examples/transaction.ves"));
}
#[test]
fn projections() {
    validates(include_str!("../examples/projections.ves"));
}

#[test]
fn nested_branch_mutation() {
    validates(
        "fn main() -> i64 { let x = 40; let y = if true { x += 1; x } else { x = 0; x }; x + y - 40 }",
    );
}
#[test]
fn state_call_used_as_value() {
    validates(
        "fn inc(inout x: i64) -> i64 { x += 1; x } fn main() -> i64 { let x = 40; let y = inc(inout x); x + y - 40 }",
    );
}
#[test]
fn nested_state_calls() {
    validates(
        "fn inc(inout x: i64) -> i64 { x += 1; x } fn main() -> i64 { let x = 40; inc(inout x) + inc(inout x) - 41 }",
    );
}
#[test]
fn disjoint_mutable_arguments_same_root() {
    validates(
        "copy struct Pair { x: i64, y: i64 } fn update(inout p.{x}: Pair, inout q.{y}: Pair) { p.x += 1; q.y += 2; } fn main() -> i64 { let p = Pair { x: 20, y: 19 }; update(inout p, inout p); p.x + p.y }",
    );
}
#[test]
fn nested_match_and_state() {
    validates(
        "fn main() -> i64 { let x = 40; let y = Some(Some(true)); match y { Some(Some(true)) => { x += 2; }, Some(Some(false)) => { x = 0; }, Some(None) => { x = 1; }, None => { x = 2; }, }; x }",
    );
}

#[test]
fn closure_captures_and_nullary_call() {
    validates("fn main() -> i64 { let x = 40; let f = || x + 2; f() }");
}
#[test]
fn closure_mutable_captures_are_values() {
    validates("fn main() -> i64 { let x = 0; let f = || { x += 1; x }; f() + f() }");
}
#[test]
fn returned_closure() {
    validates(
        "fn make(sink x: i64) -> fn(i64) -> i64 { |y| x + y } fn main() -> i64 { let f = make(40); f(2) }",
    );
}
#[test]
fn higher_order_pure_function() {
    validates(
        "fn apply(sink f: fn(i64) -> i64, x: i64) -> i64 { f(x) } fn main() -> i64 { apply(|x| x + 2, 40) }",
    );
}
#[test]
fn mutable_closure_parameter() {
    validates(
        "fn main() -> i64 { let inc = |inout x: i64| { x += 1; x }; let x = 40; let y = inc(inout x); x + y - 40 }",
    );
}
#[test]
fn function_value_nullary() {
    validates("fn answer() -> i64 { 42 } fn main() -> i64 { let f = answer; f() }");
}

#[test]
fn declaration_dependency_order() {
    validates(
        "copy struct A { b: Z } copy struct Z { value: i64 } fn main() -> i64 { let a = A { b: Z { value: 42 } }; a.b.value }",
    );
}
#[test]
fn generic_declaration_dependencies() {
    validates(
        "copy struct A[T] { b: Z[T] } copy struct Z[T] { value: T } fn main() -> i64 { let a: A[i64] = A { b: Z { value: 42 } }; a.b.value }",
    );
}
#[test]
fn mutually_recursive_struct_and_enum() {
    validates(
        "copy struct Node { next: Chain } copy enum Chain { Nil, Cons(Node) } fn main() -> i64 { let n = Node { next: Nil }; match n { Node { next: Nil } => 42, Node { next: Cons(x) } => 0, } }",
    );
}

#[test]
fn stateful_lexical_handler() {
    validates(include_str!("../examples/counter.ves"));
}
#[test]
fn fail_handler_and_try() {
    validates(include_str!("../examples/fail.ves"));
}
#[test]
fn non_tail_one_shot_handler() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { Ask::ask() + 1 } with { return x => x * 2, ask(k) => k(20) + 0, } }",
    );
}
#[test]
fn handler_forwards_named_outer_capability() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { inner.ask() } with { return x => x, ask(k) => k(outer.ask() + 2), } } with { return x => x, ask(k) => k(40), } }",
    );
}
#[test]
fn handler_calls_effectful_helper() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { Ask::ask() + Ask::ask() } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
    );
}

#[test]
fn checked_primitives() {
    validates(
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(0) } with { return x => x, raise(e, k) => match e { RuntimeError::DivisionByZero => 42, RuntimeError::IndexOutOfBounds => 0, }, } }",
    );
    validates(
        "fn index(i: i64) -> i64 eff [Fail[RuntimeError]] { let xs = (10, 20); xs[i] } fn main() -> i64 { handle Fail[RuntimeError] in { index(-1) } with { return x => x, raise(e, k) => match e { RuntimeError::IndexOutOfBounds => 42, RuntimeError::DivisionByZero => 0, }, } }",
    );
    validates(
        "fn rem(b: u64) -> u64 eff [Fail[RuntimeError]] { let a: u64 = 84; a %= b; a } fn main() -> u64 { handle Fail[RuntimeError] in { rem(0) } with { return x => x, raise(e, k) => 42, } }",
    );
    validates("fn main() -> i64 { let xs = (1, 42, 3); let i = 1; xs[i] }");
}

#[test]
fn effectful_callbacks_and_open_rows() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply(|| Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
    );
    validates(
        "linear effect Ask { fn ask() -> i64; } fn ask() -> i64 eff [Ask] { Ask::ask() } fn main() -> i64 { handle Ask in { let f = ask; f() } with { return x => x, ask(k) => k(42), } }",
    );
    validates(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 0; handle Ask in { let f = || { x += 1; x + Ask::ask() }; f() + f() } with { return x => x, ask(k) => k(20), } }",
    );
}

#[test]
fn deferred_effectful_factory() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn main() -> i64 { handle Ask in { let f = make(); f() } with { return x => x, ask(k) => k(42), } }",
    );
}

#[test]
fn callback_keeps_outer_evidence_under_nested_handlers() {
    validates(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { let f = || outer.ask(); handle Ask in { f() } with { return x => x, ask(k) => k(1), } } with { return x => x, ask(k) => k(42), } }",
    );
    validates(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle outer: Ask in { let f = || outer.ask(); handle Ask in { apply(f) } with { return x => x, ask(k) => k(1), } } with { return x => x, ask(k) => k(42), } }",
    );
    validates(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply[Ask](|| Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
    );
}

#[test]
fn literal_types_join_across_control_flow() {
    validates(
        "fn main() -> u64 { let value: Result[u64, u32] = Ok(42); let x = match value { Ok(n) => n, Err(_) => 0, }; x }",
    );
    validates("fn main() -> u64 { let n: u64 = 42; let x = if false { 1 } else { n }; x }");
}

/// Pin the reference evaluator's value, then require Lean to compile the
/// extraction and print the same value.
fn agrees(source: &str, expected: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert_eq!(
        vesper::runtime::Runtime::new(&c.world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        expected,
        "reference evaluator changed"
    );
    validates(source);
}
/// Accepted programs whose observable aliasing the extractor cannot yet
/// express must be rejected explicitly instead of extracted with snapshots.
fn rejects(source: &str, reason: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    vesper::runtime::Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap();
    let error = lean::extract(&c.world).unwrap_err();
    assert!(error.contains(reason), "{error}");
}

#[test]
fn mutable_operation_arguments() {
    agrees(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 40; handle Inc in { Inc::inc(inout x); x } with { return n => n, inc(n, k) => { n += 2; k(()) }, } }",
        "42",
    );
    agrees(
        "copy struct Pair { x: i64, y: i64 } linear effect Inc { fn inc(inout p: Pair); } fn main() -> i64 { let p = Pair { x: 40, y: 0 }; handle Inc in { Inc::inc(inout p); p.x } with { return n => n, inc(Pair { x, y }, k) => { x += 2; k(()) }, } }",
        "42",
    );
    agrees(
        "linear effect St[T] { fn modify(inout x: T) -> T; } fn main() -> i64 { let x: i64 = 5; let r = handle St[i64] in { let old = St::modify(inout x); old + x * 10 } with { return v => v, modify(x, k) => { let old = x; x += 1; let r = k(old); x *= 2; r }, }; r * 100 + x }",
        "6512",
    );
}

#[test]
fn mutations_before_and_after_non_tail_resumption() {
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; let result = handle Mut in { let y = Mut::change(inout x); x + y } with { return v => v, change(x, k) => { x += 1; let result = k(20); x += 1; result }, }; result + x }",
        "25",
    );
    // After resuming, the clause observes the body's later write to the
    // same place, and its own later write survives the handler.
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; let result = handle Mut in { let y = Mut::change(inout x); x += 100; y } with { return v => v, change(x, k) => { x += 1; let result = k(20); x * 1000 + result }, }; result * 10 + x }",
        "1020302",
    );
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; let r = handle m: Mut in { let y = m.change(inout x); x += 100; y } with { return v => v, change(x, k) => { x += 1; let result = k(20); x * 1000 + result }, }; r * 10 + x }",
        "1020302",
    );
    // Default and `in` parameters alias the performer's place as well.
    for pass in ["", "in "] {
        agrees(
            &format!(
                "linear effect Tell {{ fn tell({pass}x: i64) -> i64; }} fn main() -> i64 {{ let y = 1; let r = handle Tell in {{ let a = Tell::tell({pass}y); y += 10; a }} with {{ return v => v, tell(x, k) => {{ let r = k(5); x * 100 + r }}, }}; r }}"
            ),
            "1105",
        );
    }
}

#[test]
fn stateful_handlers_thread_mutable_arguments() {
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; handle Mut state n = 10 in { let y = Mut::change(inout x); x + y } with { return (v, n) => v + n, change(x, n, k) => { x += n; k(20, n + 1) }, } }",
        "42",
    );
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let x = 1; let r = handle Mut state n = 10 in { let a = Mut::change(inout x); x += 1; let b = Mut::change(inout x); a + b } with { return (v, n) => v * 100 + n, change(y, n, k) => { y += n; let r = k(y, n + 1); y += 1000; r }, }; r * 100000 + x }",
        "341202023",
    );
}

#[test]
fn handler_bodies_mutate_outer_bindings() {
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { x += Ask::ask(); x * 10 } with { return v => v, ask(k) => k(4), }; r + x }",
        "55",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { if Ask::ask() > 2 { x += Ask::ask(); } else { x = 0; }; match x { 1 => { x += 1; }, _ => { x *= 2; }, }; x } with { return v => v + 1, ask(k) => k(4), }; r * 100 + x }",
        "1110",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { let b = false && { x += 100; Ask::ask() > 0 }; let c = true && { x += 1; Ask::ask() > 0 }; if b || c { x } else { 0 } } with { return v => v, ask(k) => k(3), }; r * 100 + x }",
        "202",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn bump(inout x: i64) -> i64 { handle Ask in { x += Ask::ask(); x } with { return v => v, ask(k) => k(7), } } fn main() -> i64 { let x = 1; let r = bump(inout x); r * 100 + x }",
        "808",
    );
    // Clauses capture install-time snapshots; their writes stay local.
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { x = 50; 2 } with { return v => { x += 1000; v + x }, ask(k) => k(x), }; r * 10000 + x }",
        "10030050",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; handle Ask in { x = 50; Ask::ask() } with { return v => v, ask(k) => k(x), } }",
        "1",
    );
}

#[test]
fn aborts_observe_writes_before_the_operation() {
    agrees(
        "effect Stop { fn stop() -> i64; } fn main() -> i64 { let x = 0; let r = handle Stop in { x += 5; Stop::stop(); x += 100; 1 } with { return v => v, stop(k) => 7, }; r * 1000 + x }",
        "7005",
    );
    agrees(
        "effect Stop { fn stop(inout x: i64) -> i64; } fn main() -> i64 { let x = 0; let r = handle Stop in { x += 5; Stop::stop(inout x); x += 100; 1 } with { return v => v, stop(x, k) => { x += 30; 7 }, }; r * 1000 + x }",
        "7035",
    );
    for (start, expected) in [("1", "114"), ("-1", "99")] {
        agrees(
            &format!(
                "effect Choose {{ fn pick(inout x: i64) -> bool; }} fn main() -> i64 {{ let x = {start}; let r = handle Choose in {{ if Choose::pick(inout x) {{ x += 10; 1 }} else {{ 2 }} }} with {{ return v => v, pick(x, k) => if x > 0 {{ x += 3; k(true) }} else {{ x = 99; 0 }}, }}; r * 100 + x }}"
            ),
            expected,
        );
    }
    agrees(
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { let x = 0; let r = handle Fail[RuntimeError] in { x += 5; let q = divide(0); x += 100; q } with { return v => v, raise(e, k) => 42, }; r * 1000 + x }",
        "42005",
    );
    agrees(
        "effect Stop { fn stop() -> Never; } fn main() -> i64 { handle Stop in { Stop::stop(); 99 } with { return x => x, stop(k) => 7, } }",
        "7",
    );
}

#[test]
fn disjoint_projected_operation_arguments() {
    agrees(
        "copy struct Pair { x: i64, y: i64 } linear effect Inc { fn inc(inout p.{x}: Pair); } fn main() -> i64 { let p = Pair { x: 1, y: 2 }; handle Inc in { Inc::inc(inout p); p.x * 10 + p.y } with { return n => n, inc(q, k) => { q.x += 2; k(()) }, } }",
        "32",
    );
    agrees(
        "copy struct Pair { x: i64, y: i64 } linear effect Two { fn two(inout a.{x}: Pair, inout b.{y}: Pair) -> i64; } fn main() -> i64 { let p = Pair { x: 1, y: 2 }; let r = handle Two in { let v = Two::two(inout p, inout p); p.x * 100 + p.y * 10 + v } with { return v => v, two(a, b, k) => { a.x += 4; b.y += 5; let r = k(3); a.x += 1; r }, }; r * 1000 + p.x }",
        "573006",
    );
    agrees(
        "copy struct Pair { x: i64, y: i64 } linear effect Inc { fn inc(inout p: i64) -> i64; } fn main() -> i64 { let p = Pair { x: 1, y: 2 }; let r = handle Inc in { let a = Inc::inc(inout p.x); p.y += 10; let b = Inc::inc(inout p.y); a + b } with { return v => v, inc(n, k) => { n += 1; let r = k(n); n += 100; r }, }; r * 10000 + p.x * 100 + p.y }",
        "160313",
    );
}

#[test]
fn nested_mutable_handlers() {
    agrees(
        "effect Stop { fn stop() -> i64; } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Stop in { handle Ask in { x += Ask::ask(); Stop::stop(); x += 1000; 0 } with { return v => v, ask(k) => k(10), } } with { return v => v, stop(k) => 7, }; r * 100 + x }",
        "711",
    );
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle outer: Mut in { handle Ask in { let a = Ask::ask(); let b = outer.change(inout x); x += a; b } with { return v => v, ask(k) => k(3), } } with { return v => v, change(y, k) => { y *= 10; let r = k(5); y += 1; r }, }; r * 1000 + x }",
        "5014",
    );
    agrees(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 1; let r = handle Inc in { handle Inc in { Inc::inc(inout x); x } with { return n => n * 10, inc(n, k) => { n += 2; let r = k(()); n += 1; r }, } } with { return n => n, inc(n, k) => { n += 100; k(()) }, }; r * 100 + x }",
        "3004",
    );
}

#[test]
fn continuation_effects_with_mutable_arguments() {
    // A clause performs an outer effect before and after writing the
    // borrowed argument, then resumes.
    agrees(
        "linear effect Outer { fn ask() -> i64; } linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 1; let r = handle Outer in { handle Inc in { Inc::inc(inout x); x * 2 } with { return v => v, inc(y, k) => { let v = Outer::ask(); y += v; k(()) }, } } with { return v => v, ask(k) => k(20) + 1, }; r * 1000 + x }",
        "43021",
    );
    agrees(
        "linear effect Outer { fn ask() -> i64; } linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 1; let r = handle Outer in { handle Inc in { Inc::inc(inout x); x * 2 } with { return v => v, inc(y, k) => { y += 5; let v = Outer::ask(); y += v; k(()) }, } } with { return v => v, ask(k) => k(20) + 1, }; r * 1000 + x }",
        "53026",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn resume[T](sink k: fn@linear(sink i64) -> T) -> T { k(20) } fn main() -> i64 { handle Ask in { Ask::ask() + 1 } with { return x => x, ask(k) => resume(k) * 2, } }",
        "42",
    );
}

#[test]
fn helpers_and_closures_with_mutable_operations() {
    // Helpers forward the updated argument through their own state return.
    agrees(
        "linear effect Inc { fn inc(inout x: i64); } fn helper(inout x: i64) eff [Inc] { Inc::inc(inout x); x += 1; } fn main() -> i64 { let x = 1; handle Inc in { helper(inout x); x } with { return n => n, inc(n, k) => { n += 2; k(()) }, } }",
        "4",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn helper(inout x: i64) -> i64 eff [Ask] { x += Ask::ask(); x } fn main() -> i64 { let x = 1; let r = handle Ask in { helper(inout x) + helper(inout x) * 100 } with { return v => v, ask(k) => k(3), }; r * 1000 + x }",
        "704007",
    );
    agrees(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 1; handle Inc in { let f = || { let z = 10; Inc::inc(inout z); z }; f() + x } with { return n => n, inc(n, k) => { n += 2; k(()) }, } }",
        "13",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let f = || { let x = 1; handle Ask in { x += Ask::ask(); x } with { return v => v, ask(k) => k(2), } }; f() }",
        "3",
    );
}

#[test]
fn closures_keep_labeled_lexical_evidence() {
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { let get = || eff [outer: Ask] Ask::ask(); get() } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(1), } }",
        "1",
    );
}

#[test]
fn generated_names_are_hygienic() {
    agrees(
        "fn main() -> i64 { let xs = (20, 42, 3); let index = 1; xs[index] }",
        "42",
    );
    agrees(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let value = 1; let get = 2; let set = 3; let w = 4; handle Ask in { value += Ask::ask(); value + get + set + w } with { return v => v, ask(k) => k(10), } }",
        "20",
    );
}

#[test]
fn unobservable_aliases_are_rejected() {
    // The evaluator reads the final value of a body-local place after the
    // body returned; no frame holds it.
    rejects(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { handle Mut in { let z = 3; let y = Mut::change(inout z); z += 100; y } with { return v => v, change(x, k) => { x += 1; let result = k(20); x * 1000 + result }, } }",
        "local to the handled computation",
    );
    // A helper's parameter aliases the frame, but helpers cannot pass lenses.
    rejects(
        "linear effect Inc { fn inc(inout x: i64); } fn helper(inout x: i64) eff [Inc] { Inc::inc(inout x); x += 1; } fn main() -> i64 { let x = 1; let r = handle Inc in { helper(inout x); x } with { return n => n, inc(n, k) => { let r = k(()); n += 2; r }, }; r * 100 + x }",
        "through a helper, callback, or closure",
    );
    // A first-class continuation would drop the resumed frame.
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { x += 1; Ask::ask() } with { return v => v, ask(k) => { let resume = ||@linear k(20); resume() * 2 }, }; r * 100 + x }",
        "continuation",
    );
}

#[test]
fn effect_places_lemmas_are_axiom_free() {
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let source = include_str!("../lean/EffectPlaces.lean");
    let code = source.split_once("-/").unwrap().1;
    assert!(!code.contains("sorry") && !code.contains("axiom"));
    // The lemmas are about the same CPS definitions the extractor emits.
    let generated =
        lean::extract(&check::check(&parser::parse("fn main() -> i64 { 0 }").module).world)
            .unwrap();
    for line in [
        "abbrev HandlerCPS (S O : Type) (m : Type → Type) (A : Type) := (A → S → m O) → S → m O",
        "  bind action next := fun k s => action (fun a s' => next a k s') s",
    ] {
        assert!(source.contains(line) && generated.contains(line), "{line}");
    }
    let theorems = source
        .lines()
        .filter_map(|l| l.strip_prefix("theorem "))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(theorems.len() >= 15, "{theorems:?}");
    let mut checked = source.to_string();
    for t in &theorems {
        checked.push_str(&format!("\n#print axioms Vesper.EffectPlaces.{t}"));
    }
    let path =
        std::env::temp_dir().join(format!("vesper-effect-places-{}.lean", std::process::id()));
    fs::write(&path, checked).unwrap();
    let result = Command::new(compiler).arg(&path).output().unwrap();
    fs::remove_file(&path).unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        stdout.matches("does not depend on any axioms").count(),
        theorems.len(),
        "{stdout}"
    );
}

#[test]
fn implicit_mutable_modes_and_handlers_inside_closures() {
    // Omitted argument modes still resolve to the operation's `inout`.
    agrees(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 40; handle Inc in { Inc::inc(x); } with { return v => v, inc(n, k) => { n += 2; k(()) }, }; x }",
        "42",
    );
    agrees(
        "fn inc(inout x: i64) { x += 2; } fn main() -> i64 { let x = 40; let f = |inout y: i64| { y += 1; }; handle Fail[RuntimeError] in { inc(x); f(x); } with { return v => v, raise(e, k) => (), }; x }",
        "43",
    );
    // A handler installed inside a closure still passes frame lenses.
    agrees(
        "linear effect Mut { fn change(inout x: i64) -> i64; } fn main() -> i64 { let f = || { let x = 1; handle Mut in { let y = Mut::change(inout x); x += 100; y } with { return v => v, change(x, k) => { x += 1; let result = k(20); x * 1000 + result }, } }; f() }",
        "102020",
    );
}
