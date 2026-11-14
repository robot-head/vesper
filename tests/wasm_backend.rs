//! Differential tests for the `wasm32-wasip2` backend: every supported
//! program gives the same result on the evaluator and as a component under
//! Wasmtime 50; unsupported constructs give typed errors.
use std::collections::BTreeMap;
use vesper::{
    ast::Type,
    check::{self, World},
    parser,
    runtime::Runtime,
    wasm_backend::{
        self, ENTRY_INTERFACE, EffectLowering, HandlerMode, Unsupported, WasmArtifact, WasmError,
        WasmOptions, WasmWorld,
    },
};
use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker, Val},
};

fn world(source: &str) -> World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn evaluate(world: &World) -> Result<String, String> {
    let mut r = Runtime::new(world);
    r.fuel = 10_000_000;
    r.run("main", vec![]).map(|v| v.to_string())
}

/// Whether this host's Wasmtime executes WasmFX stack switching.
fn stack_switching_host() -> bool {
    matches!(
        vesper_nexus::wasmfx::probe(),
        vesper_nexus::wasmfx::WasmFxSupport::Exercised { .. }
    )
}

fn engine(stack_switching: bool) -> Engine {
    let mut c = Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    if stack_switching {
        c.wasm_function_references(true);
        c.wasm_exceptions(true);
        c.wasm_stack_switching(true);
    }
    Engine::new(&c).expect("engine")
}

/// Render a component value the way the evaluator displays the Vesper value.
fn render(world: &World, ty: &Type, v: &Val) -> String {
    let ty = vesper::numeric::default_type(ty);
    match (v, &ty) {
        (Val::S64(x), _) => x.to_string(),
        (Val::U64(x), _) => x.to_string(),
        (Val::U32(x), _) => x.to_string(),
        (Val::U8(x), _) => x.to_string(),
        (Val::Bool(x), _) => x.to_string(),
        (Val::Float64(x), _) => x.to_string(),
        (Val::String(x), _) => format!("{x:?}"),
        (Val::Tuple(vs), Type::Tuple(ts)) => format!(
            "({})",
            vs.iter()
                .zip(ts)
                .map(|(v, t)| render(world, t, v))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (_, Type::Named(n, args)) => {
            let adt = &world.adts[n];
            let vars: BTreeMap<String, Type> = adt
                .generics
                .iter()
                .zip(args)
                .map(|(g, t)| (g.name.clone(), t.clone()))
                .collect();
            let (variant, payload) = match v {
                Val::Record(_) => (&adt.variants[0], Some(v.clone())),
                Val::Variant(case, payload) => (
                    adt.variants
                        .iter()
                        .find(|var| wasm_backend_ident(&var.name) == *case)
                        .expect("case"),
                    payload.as_deref().cloned(),
                ),
                other => panic!("unexpected {other:?} for {ty}"),
            };
            let fields: Vec<(String, Type)> = variant
                .fields
                .iter()
                .map(|f| (f.name.clone(), vesper::ast::substitute(&f.ty, &vars)))
                .collect();
            let values: Vec<Val> = match (payload, fields.len()) {
                (None, _) => vec![],
                (Some(Val::Record(fs)), _) => fs.into_iter().map(|(_, v)| v).collect(),
                (Some(Val::Tuple(vs)), n) if n > 1 => vs,
                (Some(v), _) => vec![v],
            };
            let constructor = &variant.name;
            if fields.is_empty() {
                return constructor.clone();
            }
            let rendered: BTreeMap<String, String> = fields
                .iter()
                .zip(&values)
                .map(|((n, t), v)| (n.clone(), render(world, t, v)))
                .collect();
            if fields.iter().all(|(n, _)| n.parse::<usize>().is_ok()) {
                let mut pairs: Vec<_> = rendered.into_iter().collect();
                pairs.sort_by_key(|(k, _)| k.parse::<usize>().unwrap());
                format!(
                    "{constructor}({})",
                    pairs
                        .into_iter()
                        .map(|(_, v)| v)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!(
                    "{constructor} {{ {} }}",
                    rendered
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        (other, _) => panic!("unexpected {other:?} for {ty}"),
    }
}

/// The WIT spelling of a variant name (mirrors the backend's mangling for
/// the identifiers used in these tests).
fn wasm_backend_ident(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn run(world: &World, art: &WasmArtifact) -> Result<String, String> {
    run_with_fuel(world, art, 100_000_000)
}

fn run_with_fuel(world: &World, art: &WasmArtifact, fuel: u64) -> Result<String, String> {
    let engine = engine(art.stack_switching);
    let component = Component::new(&engine, &art.component).map_err(|e| format!("{e:?}"))?;
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    store.set_fuel(fuel).unwrap();
    let instance = linker
        .instantiate(&mut store, &component)
        .map_err(|e| format!("{e:?}"))?;
    let iface = instance
        .get_export_index(&mut store, None, ENTRY_INTERFACE)
        .expect("entry interface");
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .expect("main export");
    let func = instance.get_func(&mut store, main).expect("main func");
    let unit = art.result == Type::unit() || art.result.name() == "Unit";
    let mut results = if unit { vec![] } else { vec![Val::Bool(false)] };
    func.call(&mut store, &[], &mut results)
        .map_err(|e| format!("trap: {e:?}"))?;
    Ok(match results.first() {
        None => "()".into(),
        Some(v) => render(world, &art.result, v),
    })
}

fn compile(world: &World, options: &WasmOptions) -> Result<WasmArtifact, WasmError> {
    wasm_backend::compile(world, "main", options)
}

/// Evaluator and Wasmtime agree, with and without the IR optimizer and in
/// both effect-lowering modes; returns the default artifact.
fn differential(source: &str) -> WasmArtifact {
    let w = world(source);
    let expected = evaluate(&w).expect("evaluator");
    for effects in [EffectLowering::StackSwitching, EffectLowering::Portable] {
        for optimize in [false, true] {
            let art = compile(
                &w,
                &WasmOptions {
                    optimize,
                    effects,
                    ..WasmOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("compile ({effects:?}, {source}): {e}"));
            assert!(art.component.starts_with(b"\0asm"));
            if effects == EffectLowering::Portable {
                assert!(!art.stack_switching, "portable output uses WasmFX: {source}");
            }
            if art.stack_switching && !stack_switching_host() {
                eprintln!("stack switching unavailable on this host; compiled only: {source}");
                continue;
            }
            let got = run(&w, &art)
                .unwrap_or_else(|e| panic!("run ({effects:?}, {source}): {e}\n{}", art.wit));
            assert_eq!(got, expected, "{effects:?}: {source}\n{}", art.wit);
        }
    }
    compile(&w, &WasmOptions::default()).unwrap()
}

fn unsupported(source: &str, construct: Unsupported) {
    let w = world(source);
    match compile(&w, &WasmOptions::default()) {
        Err(WasmError::Unsupported { construct: c, .. }) => assert_eq!(c, construct, "{source}"),
        other => panic!("expected unsupported {construct:?} for {source}, got {other:?}"),
    }
}

// ---- scalars and checked arithmetic ---------------------------------------

#[test]
fn scalars() {
    differential("fn main() -> i64 { 2 + 3 * 4 }");
    differential("fn main() -> u64 { 18446744073709551615 }");
    differential("fn main() -> u8 { 255 }");
    differential("fn main() -> u32 { let x: u32 = 7; x * 3 }");
    differential("fn main() -> bool { 3 < 4 }");
    differential("fn main() -> f64 { 1e3 + 2.5e-1 }");
    differential("fn main() -> f64 { 0.0 / 0.0 }");
    differential("fn main() -> f64 { -(1.5 * 2.0) }");
    differential("fn main() -> Unit { }");
    differential("fn main() -> bool { false && (1 / 0 == 0) }");
    differential("fn main() -> i64 { let x = 1; if true { x += 41; } else { x -= 1; }; x }");
    differential("fn main() -> i64 { let x = 42; { let x = 2; x }; x }");
}

const FAIL_PRELUDE: &str = "fn main() -> i64 { handle Fail[ArithOverflow] in { handle Fail[RuntimeError] in { BODY } with { return v => v, raise(e, k) => match e { RuntimeError::DivisionByZero => -2, RuntimeError::IndexOutOfBounds => -3, }, } } with { return v => v, raise(e, k) => -1, } }";

fn arith(body: &str) {
    differential(&FAIL_PRELUDE.replace("BODY", body));
}

#[test]
fn checked_arithmetic_matches_the_evaluator() {
    let cases = [
        "let a: i64 = 9223372036854775807; a + 1",
        "let a: i64 = -9223372036854775808; a - 1",
        "let a: i64 = -9223372036854775808; a / -1",
        "let a: i64 = -9223372036854775808; a % -1",
        "let a: i64 = -9223372036854775808; -a",
        "let a: i64 = -1; let b: i64 = -9223372036854775808; a * b",
        "let a: i64 = 3037000500; a * a",
        "let a: i64 = 3037000499; a * a",
        "let a: i64 = -7; a / 2",
        "let a: i64 = -7; a % 2",
        "let a: i64 = 5; a / 0",
        "let a: i64 = 5; a % 0",
        "let a: i64 = 9223372036854775807; wrapping_add(a, 1)",
        "let a: i64 = -9223372036854775808; wrapping_sub(a, 1)",
        "let a: i64 = 4611686018427387904; wrapping_mul(a, 4)",
        "let x: u8 = 200; let y: u8 = 100; let z = x + y; 0",
        "let x: u8 = 200; let y: u8 = 100; if wrapping_add(x, y) == 44 { 1 } else { 0 }",
        "let x: u8 = 3; let y: u8 = 4; let z = x - y; 0",
        "let x: u8 = 16; let y: u8 = 16; let z = x * y; 0",
        "let x: u32 = 4294967295; let z = x + 1; 0",
        "let x: u32 = 65536; if wrapping_mul(x, x) == 0 { 1 } else { 0 }",
        "let x: u64 = 18446744073709551615; let z = x + 1; 0",
        "let x: u64 = 4294967296; let z = x * x; 0",
        "let x: u64 = 4294967295; let z = x * x; 1",
        "let x: u64 = 0; let z = x - 1; 0",
        "let x: u64 = 0; let z = -x; 1",
        "let x: u64 = 1; let z = -x; 0",
        "let x: u64 = 7; let y: u64 = 0; let z = x / y; 0",
        "let x: u64 = 18446744073709551615; if x > 1 { 1 } else { 0 }",
        "let a: i64 = 10; a -= 20; a *= 3; a",
        "let a: i64 = 9223372036854775807; a += 1; a",
    ];
    for c in cases {
        arith(c);
    }
}

#[test]
fn failure_selects_its_own_instance() {
    differential(include_str!("../examples/checked_math.ves"));
    differential(include_str!("../examples/fail.ves"));
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
        "fn main() -> u8 { handle Fail[i64] in { let x: Result[u8, i64] = Ok(42); x? } with { return x => x, raise(e, k) => 0, } }",
    );
    differential(
        "fn main() -> i64 { handle Fail[i64] in { let x: Result[u8, i64] = Err(7); let y = x?; 1 } with { return x => x, raise(e, k) => e, } }",
    );
    differential("fn main() -> i64 { let xs = (20, 42, 3); let index = 1; xs[index] }");
}

// ---- aggregates ----------------------------------------------------------

#[test]
fn aggregates() {
    differential("copy struct Pair { x: i64, y: i64 } fn main() -> Pair { Pair { y: 2, x: 1 } }");
    differential(
        "copy struct Pair { x: i64, y: i64 } fn main() -> i64 { let a = Pair { x: 1, y: 2 }; let b = a; b.x = 42; a.x }",
    );
    differential(
        "copy struct Pair { x: i64, y: i64 } fn sum(in p: Pair) -> i64 { wrapping_add(p.x, p.y) } fn main() -> i64 { let p = Pair { x: 20, y: 22 }; sum(in p) }",
    );
    differential(
        "fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } } fn main() -> i64 { match make(42) { Some(v) => v, None => 0, } }",
    );
    differential("fn main() -> Option[u8] { Some(255) }");
    differential("fn main() -> Option[u8] { None }");
    differential(
        "fn main() -> i64 { let x = Some(Some(true)); match x { Some(Some(true)) => 42, Some(Some(false)) => 0, Some(None) => 1, None => 2, } }",
    );
    differential(
        "copy struct Pair { x: i64, y: i64 } fn id[T](sink value: T) -> T { value } fn main() -> Pair { id[Pair](Pair { x: 1, y: 2 }) }",
    );
    differential("fn id[T](sink value: T) -> T { value } fn main() -> f64 { id[f64](42.0) }");
    differential(
        "copy struct Box[T] { value: T } fn wrap[T](sink x: T) -> Box[T] { Box { value: x } } fn main() -> Box[u64] { wrap[u64](42) }",
    );
    differential("fn main() -> (i64, bool) { (42, true) }");
    differential("fn main() -> i64 { let (x, y) = (20, 22); wrapping_add(x, y) }");
    differential("fn main() -> i64 { match -1 { -1 => 42, _ => 0, } }");
    differential("fn main() -> i64 { match 2 { 0 => 1, 1 => 2, _ => 42, } }");
    differential(include_str!("../examples/transaction.ves"));
    differential(include_str!("../examples/projections.ves"));
    differential(
        "copy struct Inner { a: i64, b: i64 } copy struct Outer { inner: Inner, c: u8 } fn main() -> Outer { let o = Outer { inner: Inner { a: 1, b: 2 }, c: 3 }; let p = o; p.inner.b = 40; o.inner.a = wrapping_add(p.inner.b, 2); o }",
    );
    differential(
        "copy enum Shape { Dot, Circle(i64), Rect { w: i64, h: i64 } } fn area(s: Shape) -> i64 { match s { Shape::Dot => 0, Shape::Circle(r) => wrapping_mul(3, wrapping_mul(r, r)), Shape::Rect { w, h } => wrapping_mul(w, h), } } fn main() -> (i64, i64, i64) { (area(Shape::Dot), area(Shape::Circle(2)), area(Shape::Rect { w: 6, h: 7 })) }",
    );
    differential(
        "copy enum Shape { Dot, Circle(i64), Rect { w: i64, h: i64 } } fn main() -> Shape { Shape::Rect { w: 6, h: 7 } }",
    );
    differential("fn main() -> Result[u32, bool] { Err(true) }");
    differential("fn main() -> (u8, (u32, f64), bool) { (1, (2, 3.5), false) }");
}

#[test]
fn structural_equality() {
    differential("fn main() -> bool { () == {} }");
    differential(
        "copy struct Pair { x: f64, y: i64 } fn main() -> (bool, bool) { let a = Pair { x: 1.0, y: 2 }; let b = Pair { x: 1.0, y: 3 }; (a == b, a != b) }",
    );
    differential(
        "fn main() -> (bool, bool, bool) { let a: Option[i64] = Some(1); let b: Option[i64] = None; (a == a, a == b, b == None) }",
    );
    differential("fn main() -> bool { (1, true) == (1, true) }");
}

// ---- calls, mutation, loops ----------------------------------------------

#[test]
fn calls_and_mutation() {
    // Every predecessor of the join is a tail call.
    differential(
        "fn a() -> i64 { 1 } fn b() -> i64 { 2 } fn main() -> i64 { if true { a() } else { b() } }",
    );
    differential(
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let x = 41; inc(inout x); x }",
    );
    differential(
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn twice(inout x: i64) { inc(inout x); inc(inout x); } fn main() -> i64 { let x = 40; twice(inout x); x }",
    );
    differential(
        "copy struct P { x: i64, y: i64 } fn bump(inout p: P) { p.x = wrapping_add(p.x, 1); } fn main() -> P { let p = P { x: 1, y: 2 }; bump(inout p); bump(inout p); p }",
    );
    differential(
        "copy struct P { x: i64, y: i64 } fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> P { let p = P { x: 1, y: 2 }; inc(inout p.y); p }",
    );
    differential("fn double(x: f64) -> f64 { x * 2.0 } fn main() -> f64 { double(21.0) }");
    differential("fn id[T](sink x: T) -> T { x } fn main() -> i64 { id[i64](42) }");
}

#[test]
fn recursion_and_loops() {
    differential(
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { wrapping_add(fib(wrapping_sub(n, 1)), fib(wrapping_sub(n, 2))) } } fn main() -> i64 { fib(20) }",
    );
    // A tail-recursive loop deeper than any native stack: `return_call`.
    differential(
        "fn count(n: u64, acc: u64) -> u64 { if n == 0 { acc } else { count(wrapping_sub(n, 1), wrapping_add(acc, n)) } } fn main() -> u64 { count(10000, 0) }",
    );
    differential(
        "fn even(n: u64) -> bool { if n == 0 { true } else { odd(wrapping_sub(n, 1)) } } fn odd(n: u64) -> bool { if n == 0 { false } else { even(wrapping_sub(n, 1)) } } fn main() -> bool { even(100001) }",
    );
    // A loop whose checked accumulator overflows part-way.
    differential(
        "fn go(n: i64, acc: i64) -> i64 eff [Fail[ArithOverflow]] { if n == 0 { acc } else { go(n - 1, acc * 3) } } fn main() -> i64 { handle Fail[ArithOverflow] in { go(100, 1) } with { return v => v, raise(e, k) => -1, } }",
    );
}

#[test]
fn tail_calls_do_not_grow_the_stack() {
    let w = world(
        "fn count(n: u64, acc: u64) -> u64 { if n == 0 { acc } else { count(wrapping_sub(n, 1), wrapping_add(acc, n)) } } fn main() -> u64 { count(10000000, 0) }",
    );
    let art = compile(&w, &WasmOptions::default()).unwrap();
    assert_eq!(
        run_with_fuel(&w, &art, 2_000_000_000).unwrap(),
        (10_000_000u64 * 10_000_001 / 2).to_string()
    );
}

#[test]
fn deep_non_tail_recursion_traps_instead_of_corrupting() {
    let w = world(
        "fn depth(n: u64) -> u64 { if n == 0 { 0 } else { wrapping_add(depth(wrapping_sub(n, 1)), 1) } } fn main() -> u64 { depth(100000000) }",
    );
    let art = compile(&w, &WasmOptions::default()).unwrap();
    let e = run(&w, &art).unwrap_err();
    assert!(e.contains("trap"), "{e}");
}

// ---- effects ---------------------------------------------------------------

#[test]
fn tail_resumptive_and_aborting_handlers_are_portable() {
    let sources = [
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), Ask::ask()) } with { return x => x, ask(k) => k(21), } }",
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { wrapping_add(Ask::ask(), Ask::ask()) } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
        "linear effect Counter { fn next() -> i64; } fn main() -> i64 { handle Counter state s = 0 in { wrapping_add(Counter::next(), Counter::next()) } with { return (x, s) => wrapping_add(x, s), next(s, k) => k(s, wrapping_add(s, 1)), } }",
        include_str!("../examples/counter.ves"),
        "effect Stop { fn stop() -> Never; } fn main() -> i64 { handle Stop in { Stop::stop(); 99 } with { return x => x, stop(k) => 7, } }",
        "effect Stop { fn stop() -> Never; } fn main() -> f64 { handle Stop in { Stop::stop(); 99.0 } with { return x => x, stop(k) => 7.0, } }",
        "copy struct Receipt { id: u64 } effect Stop { fn stop() -> Never; } fn main() -> Receipt { handle Stop in { Stop::stop(); Receipt { id: 99 } } with { return x => x, stop(k) => Receipt { id: 42 }, } }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let result = handle Ask in { x = 40; Ask::ask() } with { return value => wrapping_add(value, x), ask(k) => { x += 1; k(x) }, }; wrapping_add(x, result) }",
        "copy struct Config { x: i64 } linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let config = Config { x: 1 }; let result = handle Ask in { config.x = 40; wrapping_add(Ask::ask(), Ask::ask()) } with { return value => wrapping_add(value, config.x), ask(k) => { config.x = wrapping_add(config.x, 1); k(config.x) }, }; wrapping_add(config.x, result) }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { wrapping_add(outer.ask(), inner.ask()) } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(40), } }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { handle inner: Ask in { inner.ask() } with { return x => x, ask(k) => k(wrapping_add(outer.ask(), 2)), } } with { return x => x, ask(k) => k(40), } }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { handle Ask in { Ask::ask() } with { return x => x, ask(k) => k(wrapping_add(Ask::ask(), 1)), } } with { return x => x, ask(k) => k(10), } }",
        "effect Choose { fn choose(sink n: i64) -> bool; } fn pick(n: i64) -> i64 eff [Choose] { if Choose::choose(n) { n } else { wrapping_mul(n, 10) } } fn main() -> i64 { handle Choose in { wrapping_add(pick(1), pick(2)) } with { return x => x, choose(n, k) => if n == 1 { k(true) } else { k(false) }, } }",
        // A clause raising through an outer Fail handler while the body has
        // its own inner Fail handler: the clause sees the outer one.
        "effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Ask in { handle Fail[ArithOverflow] in { Ask::ask() } with { return x => x, raise(e, k) => 1, } } with { return x => x, ask(k) => { let m: i64 = 9223372036854775807; k(m + 1) }, } } with { return x => x, raise(e, k) => 2, } }",
        // The return clause overflows into the outer handler.
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Ask in { Ask::ask() } with { return x => x + 9223372036854775807, ask(k) => k(1), } } with { return x => x, raise(e, k) => 5, } }",
    ];
    for s in sources {
        let w = world(s);
        let art = compile(
            &w,
            &WasmOptions {
                effects: EffectLowering::Portable,
                ..WasmOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{s}: {e}"));
        assert!(!art.stack_switching, "{s}");
        assert_eq!(run(&w, &art).unwrap(), evaluate(&w).unwrap(), "{s}");
        differential(s);
    }
}

#[test]
fn general_one_shot_handlers_use_stack_switching() {
    let sources = [
        include_str!("../examples/resumptions.ves"),
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { wrapping_add(Ask::ask(), Ask::ask()) } with { return x => wrapping_mul(x, 10), ask(k) => { let v = k(1); wrapping_add(v, 100) }, } }",
        "linear effect Log { fn log(sink v: i64); } fn main() -> i64 { handle Log state total = 0 in { Log::log(1); Log::log(2); Log::log(3); 7 } with { return (x, total) => wrapping_add(wrapping_mul(total, 100), x), log(v, total, k) => { let r = k((), wrapping_add(total, v)); wrapping_add(r, 1000) }, } }",
        // Nested stack-switching handlers of different effects: the inner
        // runner forwards the outer effect.
        "linear effect A { fn a() -> i64; } linear effect B { fn b() -> i64; } fn main() -> i64 { handle A in { handle B in { wrapping_add(A::a(), B::b()) } with { return x => x, b(k) => { let r = k(2); wrapping_mul(r, 3) }, } } with { return x => x, a(k) => { let r = k(5); wrapping_add(r, 1) }, } }",
        // A Fail raised inside a suspended-and-resumed body.
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Ask in { let x: i64 = Ask::ask(); x + 9223372036854775807 } with { return x => x, ask(k) => { let r = k(1); wrapping_add(r, 1) }, } } with { return x => x, raise(e, k) => -9, } }",
    ];
    for s in sources {
        let art = differential(s);
        assert!(art.stack_switching, "{s}");
        assert!(
            art.handlers.iter().any(|h| h
                .clauses
                .iter()
                .any(|(_, m)| *m == HandlerMode::StackSwitching)),
            "{s}"
        );
        let w = world(s);
        match compile(
            &w,
            &WasmOptions {
                effects: EffectLowering::Portable,
                ..WasmOptions::default()
            },
        ) {
            Err(WasmError::Unsupported {
                construct: Unsupported::NonTailResumptiveHandler,
                ..
            }) => {}
            other => panic!("{s}: {other:?}"),
        }
    }
}

#[test]
fn handler_modes_are_reported() {
    let art = differential(include_str!("../examples/counter.ves"));
    let modes: Vec<_> = art
        .handlers
        .iter()
        .flat_map(|h| {
            h.clauses
                .iter()
                .map(|(op, m)| (h.effect.clone(), op.clone(), *m))
        })
        .collect();
    assert!(modes.contains(&("Counter".into(), "next".into(), HandlerMode::Tail)));
    assert!(modes.contains(&("Fail".into(), "raise".into(), HandlerMode::Abort)));
    assert!(!art.stack_switching);
}

// ---- closures and function values ---------------------------------------------

#[test]
fn closures_and_function_values() {
    for source in [
        "fn main() -> i64 { let x = 40; let f = |y: i64| wrapping_add(x, y); f(2) }",
        "fn make(sink x: i64) -> fn(i64) -> i64 { |y| wrapping_add(x, y) } fn main() -> i64 { let f = make(40); f(2) }",
        "fn apply(sink f: fn(i64) -> i64, x: i64) -> i64 { f(x) } fn main() -> i64 { apply(|x| wrapping_add(x, 2), 40) }",
        "fn main() -> i64 { let f = || 40; let g = || wrapping_add(f(), 2); g() }",
        "fn main() -> i64 { let x = 0; let f = || { x += 1; x }; wrapping_add(f(), f()) }",
        "fn main() -> i64 { let x = 40; let f = if true { |y: i64| wrapping_add(x, y) } else { |y: i64| wrapping_sub(x, y) }; f(2) }",
        "fn main() -> i64 { let inc = |inout x: i64| { x = wrapping_add(x, 1); x }; let x = 40; let y = inc(inout x); wrapping_sub(wrapping_add(x, y), 40) }",
        "fn answer() -> i64 { 42 } fn main() -> i64 { let f = answer; f() }",
        "copy struct Callback { f: fn@copy() -> i64 } fn invoke(in c: Callback) -> i64 { c.f() } fn main() -> i64 { let x = 42; let c = Callback { f: || x }; invoke(in c) }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { let f = || eff [Ask] Ask::ask(); f() } with { return x => x, ask(k) => k(42), } }",
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { handle Ask in { apply(|| eff [Ask] Ask::ask()) } with { return x => x, ask(k) => k(42), } }",
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() } fn apply(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle inner: Ask in { f() } with { return x => x, ask(k) => k(2), } } fn main() -> i64 { let f = make(); handle outer: Ask in { apply(sink f) } with { return x => x, ask(k) => k(1), } }",
        "fn make(sink x: i64) -> fn() -> i64 { || x } fn main() -> i64 { let f = make(20); let g = make(22); wrapping_add(f(), g()) }",
        "linear struct Ticket { value: i64 } fn main() -> i64 { let ticket = Ticket { value: 42 }; let use = || { let Ticket { value } = ticket; value }; use() }",
        "fn main() -> i64 { let f = || 40; let callbacks = (f, f); let g = || { let index = 1; wrapping_add(callbacks[index](), 2) }; g() }",
        "fn main() -> i64 { let factory = || { let x = 42; || x }; let f = factory(); f() }",
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle outer: Ask in { let f = || eff [Ask] Ask::ask(); handle inner: Ask in { wrapping_add(f(), inner.ask()) } with { return x => x, ask(k) => k(2), } } with { return x => x, ask(k) => k(40), } }",
        "linear effect Ask { fn ask() -> i64; } fn call(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { f() } fn answer() -> i64 eff [Ask] { Ask::ask() } fn main() -> i64 { handle Ask in { let f = answer; call(sink f) } with { return x => x, ask(k) => k(42), } }",
        "fn apply(sink f: fn(i64) -> i64 eff [Fail[ArithOverflow]], x: i64) -> i64 eff [Fail[ArithOverflow]] { wrapping_add(f(x), 1) } fn main() -> i64 { handle Fail[ArithOverflow] in { apply(|x| x * 4611686018427387904, 2) } with { return x => x, raise(e, k) => -1, } }",
    ] {
        differential(source);
    }
}

// ---- f64 remainder, strings, monomorphization ------------------------------

#[test]
fn float_remainder_is_exact() {
    differential(include_str!("fixtures/wasm_backend/remainder.ves"));
    differential("fn main() -> f64 { let a: f64 = 10.5; a %= 4.0; a }");
    differential("fn main() -> f64 { let n: f64 = 0.0; let a: f64 = 1.0; a % (n / n) }");
    differential("fn main() -> f64 { let n: f64 = 0.0; let a: f64 = 1.0; (a / n) % 2.0 }");
    differential("fn main() -> f64 { let n: f64 = 0.0; let a: f64 = 2.0; a % (1.0 / n) }");
    differential("fn main() -> f64 { let a: f64 = 5.0; a % 0.0 }");
    // Every pair of a grid of awkward operands (zeros, subnormals, huge
    // ratios, exact multiples), compared through the evaluator's display,
    // which round-trips every finite f64 exactly.
    let values = [
        "0.0",
        "-0.0",
        "1.0",
        "-1.0",
        "0.5",
        "3.0",
        "1e-300",
        "2.5e-308",
        "5e-324",
        "3e-320",
        "1e300",
        "1.7976931348623157e308",
        "7.25",
        "-13.75",
        "4503599627370497.0",
        "0.1",
    ];
    for x in values {
        let row: Vec<String> = values
            .iter()
            .map(|y| format!("{{ let a: f64 = {x}; let b: f64 = {y}; a % b }}"))
            .collect();
        differential(&format!(
            "fn main() -> ({}) {{ ({}) }}",
            vec!["f64"; values.len()].join(", "),
            row.join(", ")
        ));
    }
}

#[test]
fn strings() {
    differential(r#"fn main() -> String { "hello, wasm" }"#);
    differential(r#"fn main() -> String { "" }"#);
    differential(r#"fn main() -> bool { let a = "abc"; let b = "abd"; a == b }"#);
    differential(r#"fn main() -> (bool, bool) { ("abc" == "abc", "abc" != "ab") }"#);
    differential(
        r#"fn name(n: u8) -> String { match n { 0 => "zero", 1 => "one", _ => "many", } } fn main() -> (String, String, String) { (name(0), name(1), name(7)) }"#,
    );
    differential(
        r#"fn classify(sink s: String) -> i64 { match s { "red" => 1, "green" => 2, _ => 0, } } fn main() -> (i64, i64, i64) { (classify("red"), classify("green"), classify("blue")) }"#,
    );
    differential(
        r#"struct Named { name: String, value: i64 } fn main() -> Named { Named { name: "answer", value: 42 } }"#,
    );
    differential(r#"fn main() -> Option[String] { Some("unicode: é😀") }"#);
    differential(
        r#"fn main() -> bool { let a: Option[String] = Some("x"); let b: Option[String] = Some("x"); a == b }"#,
    );
}

#[test]
fn generic_functions_are_monomorphized() {
    differential(
        "copy struct Pair { x: f64, y: i64 } fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> bool { let a = Pair { x: 1.0, y: 2 }; same[Pair](a, a) }",
    );
    // Inferred type arguments, several instances of one function.
    differential(
        "fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn main() -> (bool, bool, bool) { (same(1, 2), same(true, true), same((1, 2.5), (1, 2.5))) }",
    );
    differential(
        r#"fn same[T: mod copy](a: T, b: T) -> bool { a == b } fn twice[T: mod copy](a: T, b: T) -> bool { same(a, b) } fn main() -> (bool, bool) { (twice(7, 7), twice(Some(1), None)) }"#,
    );
    // A generic Fail instance resolved per instantiation.
    differential(
        "fn fail_with[E](sink e: E) -> i64 eff [Fail[E]] { Fail::raise(e) } fn main() -> i64 { handle Fail[u8] in { handle Fail[bool] in { fail_with(true) } with { return x => x, raise(e, k) => 1, } } with { return x => x, raise(e, k) => 2, } }",
    );
    differential(
        "linear effect Get[T] { fn get() -> T; } fn ask[T]() -> T eff [Get[T]] { Get::get() } fn main() -> (u8, bool) { let a = handle Get[u8] in { ask[u8]() } with { return x => x, get(k) => k(9), }; let b = handle Get[bool] in { ask[bool]() } with { return x => x, get(k) => k(true), }; (a, b) }",
    );
    // Closures inside a generic instance see its types.
    differential(
        "fn eq_later[T: mod copy](sink a: T, sink b: T) -> bool { let f = || a == b; f() } fn main() -> (bool, bool) { (eq_later(1.5, 1.5), eq_later((1, 2), (2, 1))) }",
    );
}

// ---- rejections --------------------------------------------------------------

#[test]
fn unsupported_constructs_are_typed_errors() {
    unsupported(
        "linear effect Inc { fn inc(inout x: i64); } fn main() -> i64 { let x = 40; handle Inc in { Inc::inc(inout x); x } with { return n => n, inc(n, k) => { n = wrapping_add(n, 2); k(()) }, } }",
        Unsupported::InoutEffectArgument,
    );
    unsupported(
        "linear effect Ask { fn ask() -> i64; } fn get(b: bool) -> i64 eff [outer: Ask] { if b { outer.ask() } else { wrapping_add(outer.ask(), 1) } } fn main() -> i64 { handle outer: Ask in { get(true) } with { return x => x, ask(k) => k(40), } }",
        Unsupported::LabelledEffectRow,
    );
}

#[test]
fn entry_and_limit_errors() {
    let params: Vec<String> = (0..17).map(|i| format!("p{i}: u8")).collect();
    let many = world(&format!("fn many({}) -> i64 {{ 1 }}", params.join(", ")));
    assert!(matches!(
        wasm_backend::compile(&many, "many", &WasmOptions::default()),
        Err(WasmError::Entry(_))
    ));
    let w = world("fn main() -> i64 { 1 }");
    assert!(matches!(
        wasm_backend::compile(&w, "missing", &WasmOptions::default()),
        Err(WasmError::Entry(_))
    ));
    assert!(matches!(
        compile(
            &w,
            &WasmOptions {
                world: WasmWorld::NexusPlugin,
                ..WasmOptions::default()
            }
        ),
        Err(WasmError::Entry(_))
    ));
    assert!(matches!(
        compile(
            &w,
            &WasmOptions {
                max_memory_pages: 0,
                ..WasmOptions::default()
            }
        ),
        Err(WasmError::Limit(_))
    ));
    let w = world(
        "fn a(n: i64) -> i64 { wrapping_add(n, 1) } fn b(n: i64) -> i64 { a(a(n)) } fn main() -> i64 { b(b(1)) }",
    );
    assert!(matches!(
        compile(
            &w,
            &WasmOptions {
                max_functions: 2,
                ..WasmOptions::default()
            }
        ),
        Err(WasmError::Limit(_))
    ));
}

#[test]
fn memory_exhaustion_traps() {
    let w = world(
        "fn build(n: u64, acc: (u64, u64)) -> u64 { let (a, b) = acc; if n == 0 { a } else { build(wrapping_sub(n, 1), (wrapping_add(a, 1), n)) } } fn main() -> u64 { build(10000000, (0, 0)) }",
    );
    let art = compile(
        &w,
        &WasmOptions {
            max_memory_pages: 4,
            ..WasmOptions::default()
        },
    )
    .unwrap();
    let e = run(&w, &art).unwrap_err();
    assert!(e.contains("trap"), "{e}");
}

// ---- component worlds ----------------------------------------------------------

#[test]
fn command_component_exports_wasi_cli_run() {
    let w = world("fn main() -> i64 { 42 }");
    let art = compile(&w, &WasmOptions::default()).unwrap();
    assert!(
        art.wit.contains("export wasi:cli/run@0.2.0;"),
        "{}",
        art.wit
    );
    let engine = engine(false);
    let component = Component::new(&engine, &art.component).unwrap();
    let mut store = Store::new(&engine, ());
    store.set_fuel(1_000_000).unwrap();
    let instance = Linker::<()>::new(&engine)
        .instantiate(&mut store, &component)
        .unwrap();
    let iface = instance
        .get_export_index(&mut store, None, wasm_backend::WASI_RUN)
        .expect("wasi:cli/run export");
    let run = instance
        .get_export_index(&mut store, Some(&iface), "run")
        .unwrap();
    let run = instance
        .get_typed_func::<(), (Result<(), ()>,)>(&mut store, run)
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), (Ok(()),));
    // The component imports nothing.
    assert_eq!(component.component_type().imports(&engine).count(), 0);
}

#[test]
fn runs_as_a_nexus_plugin() {
    let source = "fn step(x: u64) -> u64 { wrapping_add(wrapping_mul(x, 3), 1) } fn run(scenario: u32, arg: u64) -> i64 { handle Fail[ArithOverflow] in { let s: u64 = 0; if scenario == 0 { let y = step(arg); 7 } else { let z: i64 = 9223372036854775807; z + 1 } } with { return v => v, raise(e, k) => -1, } }";
    let w = world(source);
    let art = wasm_backend::compile(
        &w,
        "run",
        &WasmOptions {
            world: WasmWorld::NexusPlugin,
            ..WasmOptions::default()
        },
    )
    .unwrap();
    assert!(art.wit.contains("export run: func("), "{}", art.wit);
    let mut host = vesper_nexus::NexusEngine::new(vesper_nexus::NexusConfig::default()).unwrap();
    let compiled = host.compile(&art.component).unwrap();
    let mut inst = host.instantiate_with_fuel(&compiled, 1, 1_000_000).unwrap();
    assert_eq!(host.call_run(&mut inst, 0, 5).unwrap(), 7);
    assert_eq!(host.call_run(&mut inst, 1, 0).unwrap(), -1);
    assert!(inst.fuel_consumed() > 0);
    let report = host.destroy_instance(inst);
    let _ = report;
    assert_eq!(host.leaked_instances(), 0);
}

#[test]
fn nexus_plugin_runs_out_of_fuel() {
    let source = "fn spin(n: u64) -> u64 { if n == 0 { 0 } else { spin(wrapping_sub(n, 1)) } } fn run(scenario: u32, arg: u64) -> i64 { let x = spin(arg); 1 }";
    let w = world(source);
    let art = wasm_backend::compile(
        &w,
        "run",
        &WasmOptions {
            world: WasmWorld::NexusPlugin,
            ..WasmOptions::default()
        },
    )
    .unwrap();
    let mut host = vesper_nexus::NexusEngine::new(vesper_nexus::NexusConfig::default()).unwrap();
    let compiled = host.compile(&art.component).unwrap();
    let mut inst = host.instantiate_with_fuel(&compiled, 2, 10_000).unwrap();
    assert_eq!(host.call_run(&mut inst, 0, 10).unwrap(), 1);
    assert!(host.call_run(&mut inst, 0, 1_000_000_000).is_err());
    host.destroy_instance(inst);
}
