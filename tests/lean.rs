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
