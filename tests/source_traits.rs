//! Source traits, associated types and canonical impls: static elaboration
//! to ordinary checked calls, executed on the evaluator and the JIT, and
//! emitted as AArch64 objects and Lean.
use vesper::{
    ast::{ExprKind, Item, Module},
    check, interface, ir, lean, native, pact_frontend, parser,
    runtime::Runtime,
};

fn module(source: &str) -> Module {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    assert_eq!(p.syntax().text().to_string(), source, "lossless CST");
    p.module
}

fn world(source: &str) -> check::World {
    let c = check::check(&module(source));
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    c.world
}

/// Evaluator and JIT agree; an AArch64 object and Lean are emitted.
fn execute(source: &str) -> String {
    let w = world(source);
    let interpreted = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    let jit = native::jit(&w, "main").unwrap().to_string();
    assert_eq!(jit, interpreted, "JIT and evaluator disagree");
    let (arm, _) =
        native::object_for_target(&w, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(&arm[..4], b"\x7fELF");
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183, "EM_AARCH64");
    let lean = lean::extract(&w).expect("Lean accepts the elaborated core");
    assert!(!lean.contains("$projection"));
    interpreted
}

fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:?}",
        c.diagnostics
    );
    c.diagnostics
}

const SHAPES: &str = include_str!("../examples/traits/shapes.ves");

#[test]
fn trait_impl_models_and_shadowing_execute() {
    // canonical 7, ByX 3 shadowed by ByY 4, explicit ByY 4, Counter 4 then 3.
    assert_eq!(execute(SHAPES), "4304070304");
}

#[test]
fn elaborated_world_uses_only_core_calls() {
    let w = world(SHAPES);
    // Every trait call became an ordinary call of a generated function.
    let program = ir::lower_world(&w);
    let text = serde_json::to_string(&program).unwrap();
    for generated in w.elaboration.generated.keys() {
        assert!(text.contains(generated.as_str()), "{generated} is called");
    }
    assert!(!text.contains("$projection"));
    assert!(!text.contains("\"Indirect\""), "no dynamic dispatch");
    // Surface trait calls (`p.score()`) are gone from the checked bodies.
    for f in w.functions.values() {
        let body = serde_json::to_string(&f.body).unwrap();
        assert!(
            !body.contains("\"score\""),
            "{} still calls by name",
            f.name
        );
    }
    let main = &w.functions["total"];
    assert_eq!(
        main.result,
        vesper::ast::Type::named("i64"),
        "projection normalized"
    );
    let names: Vec<&str> = w.elaboration.generated.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "impl$Score.Point$score",
            "impl$Source.Counter$next",
            "model$ByX$score",
            "model$ByY$score"
        ]
    );
    assert!(w.elaboration.generated.values().all(|g| !g.public));
    assert_eq!(w.elaboration.package.as_deref(), Some("@local/main@0.0.0"));
    assert!(w.elaboration.revision.is_some());
}

#[test]
fn generated_bodies_are_private_and_contracts_unsupported() {
    let w = world(SHAPES);
    let i = interface::export(&w, "@demo/shapes").unwrap();
    assert!(
        i.exports.keys().all(|k| !k.contains('$')),
        "{:?}",
        i.exports.keys()
    );
    assert!(i.exports.contains_key("@demo/shapes::fn::total"));
    for k in [
        "@demo/shapes::trait::Score",
        "@demo/shapes::trait::Source",
        "@demo/shapes::impl::Score.Point",
        "@demo/shapes::model::ByX",
    ] {
        assert!(
            matches!(
                i.exports.get(k),
                Some(vesper_semver::Declaration::Unsupported { .. })
            ),
            "{k}: {:?}",
            i.exports.get(k)
        );
    }
    // Unsupported contracts can never be certified compatible.
    let report = vesper_semver::compare(&i, &i);
    assert!(!report.is_compatible(), "{report:?}");
}

#[test]
fn source_programs_are_rewritten_not_dispatched_by_name() {
    // An inherent method with the trait method's name keeps priority, and a
    // receiver whose type only matches by printed name is never dispatched.
    let src = "struct A { v: i64 } struct B { v: i64 }
trait Get { fn get(in self) -> i64; }
impl A { fn get(in self) -> i64 { 1 } }
impl Get for A { fn get(in self) -> i64 { 2 } }
impl Get for B { fn get(in self) -> i64 { wrapping_add(self.v, 10) } }
fn main() -> i64 { let a = A { v: 0 }; let b = B { v: 5 }; wrapping_add(wrapping_mul(a.get(), 100), wrapping_add(Get::get(a), wrapping_mul(b.get(), 1000))) }";
    assert_eq!(execute(src), "15102");
}

#[test]
fn generic_impl_and_projection_normalize() {
    let src = "copy struct Box[T: mod copy] { v: T }
trait Unwrap { type Inner: mod copy; fn unwrap(in self) -> Self::Inner; }
impl[T: mod copy] Unwrap for Box[T] { type Inner = T; fn unwrap(in self) -> T { self.v } }
fn widen(in x: <Box[u8] as Unwrap>::Inner) -> u64 { if x == 255 { 1 } else { 0 } }
fn main() -> i64 {
    let a = Box { v: 40 };
    let b: Box[u8] = Box { v: 255 };
    let c = Box { v: true };
    let n: <Box[i64] as Unwrap>::Inner = a.unwrap();
    if c.unwrap() && widen(b.unwrap()) == 1 { wrapping_add(n, 2) } else { 0 }
}";
    assert_eq!(execute(src), "42");
}

#[test]
fn effect_row_parameters_and_handlers() {
    let src = "struct Counter { n: i64 }
linear effect Log { fn log(x: i64) -> Unit; }
trait Step[r: eff] { fn step(inout self) -> i64 eff [r]; }
impl Step[Log] for Counter {
    fn step(inout self) -> i64 eff [Log] { Log::log(self.n); self.n = wrapping_add(self.n, 1); self.n }
}
fn main() -> i64 {
    let c = Counter { n: 1 };
    handle Log in {
        let a = c.step();
        let b = c.step();
        wrapping_add(wrapping_mul(a, 10), b)
    } with {
        return x => x,
        log(v, k) => k(()),
    }
}";
    assert_eq!(execute(src), "23");
}

#[test]
fn linear_receivers_keep_exact_once_checks() {
    let ok = "linear struct Token { id: i64 }
trait Redeem { fn redeem(sink self) -> i64; fn peek(in self) -> i64; }
impl Redeem for Token {
    fn redeem(sink self) -> i64 { let Token { id } = self; id }
    fn peek(in self) -> i64 { self.id }
}
fn main() -> i64 { let t = Token { id: 7 }; let p = t.peek(); wrapping_add(p, t.redeem()) }";
    assert_eq!(execute(ok), "14");
    // Using the token after consuming it through the trait method.
    let reuse = ok.replace(
        "wrapping_add(p, t.redeem())",
        "let a = t.redeem(); wrapping_add(a, t.peek())",
    );
    let ds = check::check(&module(&reuse)).diagnostics;
    assert!(ds.iter().any(|d| d.code.starts_with("E03")), "{ds:?}");
    // Never consuming the linear token.
    let leak = ok.replace("wrapping_add(p, t.redeem())", "p");
    let ds = check::check(&module(&leak)).diagnostics;
    assert!(ds.iter().any(|d| d.code.starts_with("E03")), "{ds:?}");
}

#[test]
fn original_method_and_generic_adt_syntax_still_work() {
    let src = "copy struct Pair[T: mod copy] { a: T, b: T }
impl[T: mod copy] Pair[T] { fn first(in self) -> T { self.a } }
fn main() -> i64 { let p = Pair { a: 4, b: 5 }; p.first() }";
    assert_eq!(execute(src), "4");
    let m = module(src);
    assert!(!pact_frontend::has_frontend_syntax(&m));
    assert!(matches!(m.items[1], Item::Impl { .. }));
}

#[test]
fn parse_trait_declarations_with_spans() {
    let m = module(SHAPES);
    let Some(Item::Trait(t)) = m.items.iter().find(|i| matches!(i, Item::Trait(_))) else {
        panic!("no trait");
    };
    assert_eq!(t.name, "Score");
    assert_eq!(t.assoc[0].name, "Out");
    assert_eq!(t.assoc[0].bound, vesper::ast::Mode::Copy);
    assert_eq!(&SHAPES[t.span.clone()][..11], "trait Score");
    assert_eq!(m.model_uses.len(), 2);
    assert_eq!(&SHAPES[m.model_uses[0].span.clone()], "use model ByX;");
    // Inner use's scope is nested in the outer use's scope.
    assert!(m.model_uses[0].scope.start < m.model_uses[1].scope.start);
    assert!(m.model_uses[1].scope.end < m.model_uses[0].scope.end);
    let f = m
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if f.name == "total" => Some(f),
            _ => None,
        })
        .unwrap();
    assert!(vesper::ast::as_projection(&f.result).is_some());
    let body = f.body.as_ref().unwrap();
    assert!(matches!(body.kind, ExprKind::Block { .. }));
}

#[test]
fn malformed_declarations_are_diagnosed() {
    for (src, msg) in [
        ("trait { }", "upper-case"),
        ("trait T { fn f(in self) -> i64 }", "Semi"),
        ("trait T { let x = 1; }", "expected 'type' or 'fn'"),
        ("impl T for { }", "identifier"),
        ("model M T for i64 { }", "Colon"),
    ] {
        let p = parser::parse(src);
        assert!(
            p.diagnostics.iter().any(|d| d.message.contains(msg)),
            "{src}: {:?}",
            p.diagnostics
        );
        assert_eq!(p.syntax().text().to_string(), src);
    }
}

const BASE: &str = "struct Point { x: i64, y: i64 } trait Show { type Out: mod copy; fn show(in self) -> Self::Out; } ";

#[test]
fn bad_method_signatures_are_rejected() {
    rejects(
        &format!("{BASE} impl Show for Point {{ type Out = i64; }} fn main() -> i64 {{ 0 }}"),
        "E0601",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} fn extra(in self) -> i64 {{ 2 }} }} fn main() -> i64 {{ 0 }}"
        ),
        "E0601",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ fn show(in self) -> i64 {{ 1 }} }} fn main() -> i64 {{ 0 }}"
        ),
        "E0601",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> bool {{ true }} }} fn main() -> i64 {{ 0 }}"
        ),
        "E0602",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self, x: i64) -> i64 {{ x }} }} fn main() -> i64 {{ 0 }}"
        ),
        "E0602",
    );
    rejects(
        "trait Make { fn make() -> i64; } fn main() -> i64 { 0 }",
        "E0607",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ true }} }} fn main() -> i64 {{ 0 }}"
        ),
        "E0200",
    );
    rejects(
        &format!("{BASE} impl Missing for Point {{ }} fn main() -> i64 {{ 0 }}"),
        "E0601",
    );
    rejects(
        "struct Show { x: i64 } trait Show { } fn main() -> i64 { 0 }",
        "E0600",
    );
}

#[test]
fn mode_and_effect_mismatches_are_rejected() {
    let src = "struct C { n: i64 } trait Next { fn next(inout self) -> i64; } impl Next for C { fn next(in self) -> i64 { self.n } } fn main() -> i64 { 0 }";
    rejects(src, "E0603");
    let src = "struct C { n: i64 } effect Log { fn log(x: i64) -> Unit; } trait Next { fn next(inout self) -> i64; } impl Next for C { fn next(inout self) -> i64 eff [Log] { Log::log(1); self.n } } fn main() -> i64 { 0 }";
    rejects(src, "E0604");
    // Modal bounds are rechecked after substitution.
    let src = "struct C { n: i64 } trait Src { type Item: mod copy; fn get(in self) -> Self::Item; } impl Src for C { type Item = String; fn get(in self) -> String { \"x\" } } fn main() -> i64 { 0 }";
    rejects(src, "E0606");
    let src = "struct C { n: i64 } trait Keep[T: mod copy] { fn keep(in self) -> i64; } impl Keep[String] for C { fn keep(in self) -> i64 { 0 } } fn main() -> i64 { 0 }";
    rejects(src, "E0606");
    // The method body itself is checked against its declared effects.
    let src = "struct C { n: i64 } effect Log { fn log(x: i64) -> Unit; } trait Next { fn next(inout self) -> i64; } impl Next for C { fn next(inout self) -> i64 { Log::log(1); self.n } } fn main() -> i64 { 0 }";
    let ds = check::check(&module(src)).diagnostics;
    assert!(!ds.is_empty());
}

#[test]
fn overlapping_canonical_impls_are_rejected() {
    let dup = format!(
        "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 2 }} }} fn main() -> i64 {{ 0 }}"
    );
    rejects(&dup, "E0605");
    let blanket = "copy struct Box[T: mod copy] { v: T } trait Show { fn show(in self) -> i64; }
impl[T: mod copy] Show for Box[T] { fn show(in self) -> i64 { 1 } }
impl Show for Box[i64] { fn show(in self) -> i64 { 2 } }
fn main() -> i64 { 0 }";
    let ds = rejects(blanket, "E0605");
    rejects(
        include_str!("../examples/traits/overlap_rejected.ves"),
        "E0605",
    );
    assert!(ds.iter().any(|d| d.message.contains("PACT0102")), "{ds:?}");
    // The same alternative is accepted as a named model.
    let ok = blanket.replace("impl Show for Box[i64]", "model Exact: Show for Box[i64]");
    let ok = ok.replace(
        "fn main() -> i64 { 0 }",
        "fn main() -> i64 { let b = Box { v: 3 }; let x = b.show(); use model Exact; wrapping_add(wrapping_mul(x, 10), b.show()) }",
    );
    assert_eq!(execute(&ok), "12");
}

#[test]
fn obligations_report_no_solution_ambiguity_and_overflow() {
    // No implementation.
    rejects(
        &format!("{BASE} fn main() -> i64 {{ let p = Point {{ x: 1, y: 2 }}; p.show() }}"),
        "E0608",
    );
    // Two traits provide the method.
    let two = "struct P { x: i64 } trait A { fn get(in self) -> i64; } trait B { fn get(in self) -> i64; }
impl A for P { fn get(in self) -> i64 { 1 } } impl B for P { fn get(in self) -> i64 { 2 } }
fn main() -> i64 { let p = P { x: 0 }; p.get() }";
    rejects(two, "E0609");
    // ...unless the trait is named.
    assert_eq!(
        execute(&two.replace(
            "p.get() }",
            "wrapping_add(A::get(p), wrapping_mul(B::get(p), 10)) }"
        )),
        "21"
    );
    // An underdetermined receiver is ambiguous between two impls.
    let under = "trait Z { fn z(in self) -> i64; }
impl Z for Option[i64] { fn z(in self) -> i64 { 1 } }
impl Z for Option[bool] { fn z(in self) -> i64 { 2 } }
fn main() -> i64 { let o = None; o.z() }";
    let ds = check::check(&module(under)).diagnostics;
    assert!(
        ds.iter()
            .any(|d| d.code == "E0609" || d.code.starts_with("E02")),
        "{ds:?}"
    );
    // A where-clause chain that never bottoms out.
    let deep = "copy struct Box[T: mod copy] { v: T } trait Deep { fn deep(in self) -> i64; }
impl[T: mod copy] Deep for Box[T] where Box[Box[T]]: Deep { fn deep(in self) -> i64 { 0 } }
fn main() -> i64 { let b = Box { v: 1 }; b.deep() }";
    rejects(deep, "E0611");
}

#[test]
fn generic_dictionary_dispatch_is_rejected() {
    rejects(
        &format!("{BASE} fn f[T: Show](in x: T) -> i64 {{ 0 }} fn main() -> i64 {{ 0 }}"),
        "E0607",
    );
    rejects(
        &format!("{BASE} fn f[T](in x: T) -> i64 where T: Show {{ 0 }} fn main() -> i64 {{ 0 }}"),
        "E0607",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} fn f[T](in x: T) -> i64 {{ x.show() }} fn main() -> i64 {{ 0 }}"
        ),
        "E0607",
    );
    // A where-clause assumption cannot be discharged statically.
    let src = "copy struct Box[T: mod copy] { v: T } trait Show { fn show(in self) -> i64; }
impl Show for i64 { fn show(in self) -> i64 { 5 } }
impl[T: mod copy] Show for Box[T] where T: Show { fn show(in self) -> i64 { self.v.show() } }
fn main() -> i64 { let b = Box { v: 1 }; b.show() }";
    rejects(src, "E0607");
    // Without needing the assumption the generic impl executes.
    let ok = src.replace("self.v.show()", "7");
    assert_eq!(execute(&ok), "7");
}

#[test]
fn unresolved_projections_cannot_escape() {
    rejects(
        &format!("{BASE} fn f[T](in x: T) -> T::Out {{ x }} fn main() -> i64 {{ 0 }}"),
        "E0612",
    );
    rejects(
        &format!("{BASE} fn f(in x: Point) -> Point::Out {{ 0 }} fn main() -> i64 {{ 0 }}"),
        "E0612",
    );
    rejects(
        &format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} fn f(in x: Point) -> <Point as Show>::Nope {{ 0 }} fn main() -> i64 {{ 0 }}"
        ),
        "E0612",
    );
    rejects("struct P { x: i64 } fn main() -> P::Item { 0 }", "E0612");
    rejects(
        &format!("{BASE} struct Bad {{ v: Point::Out }} fn main() -> i64 {{ 0 }}"),
        "E0607",
    );
}

#[test]
fn ordinary_modules_are_unchanged() {
    let src = "fn main() -> i64 { 1 }";
    let m = module(src);
    assert!(!pact_frontend::has_frontend_syntax(&m));
    let w = world(src);
    assert_eq!(w.elaboration, Default::default());
}

#[test]
fn call_and_struct_receivers_have_declared_types() {
    let src = "struct Point { x: i64 } trait Show { fn show(in self) -> i64; }
impl Show for Point { fn show(in self) -> i64 { wrapping_add(self.x, 1) } }
fn make() -> Point { Point { x: 1 } }
fn main() -> i64 { wrapping_add(make().show(), wrapping_mul(Point { x: 4 }.show(), 10)) }";
    assert_eq!(execute(src), "52");
    // A literal receiver defaults to i64.
    let lit = "trait Twice { fn twice(in self) -> i64; } impl Twice for i64 { fn twice(in self) -> i64 { wrapping_mul(self, 2) } } fn main() -> i64 { 21.twice() }";
    assert_eq!(execute(lit), "42");
}

/// Real Lean 4 compilation and `#eval` of the elaborated output, compared
/// with the evaluator. Skipped (reported on stderr) without a Lean binary;
/// set `VESPER_LEAN` to the pinned toolchain's `lean`.
#[test]
fn lean_accepts_and_evaluates_elaborated_programs() {
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if std::process::Command::new(&compiler)
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let generic = "copy struct Box[T: mod copy] { v: T }
trait Unwrap { type Inner: mod copy; fn unwrap(in self) -> Self::Inner; }
impl[T: mod copy] Unwrap for Box[T] { type Inner = T; fn unwrap(in self) -> T { self.v } }
model Plus: Unwrap for Box[i64] { type Inner = i64; fn unwrap(in self) -> i64 { wrapping_add(self.v, 1) } }
fn main() -> i64 { let b = Box { v: 40 }; let x = b.unwrap(); use model Plus; wrapping_add(x, b.unwrap()) }";
    for (i, src) in [SHAPES, generic].into_iter().enumerate() {
        let w = world(src);
        let expected = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
        let mut output = lean::extract(&w).unwrap();
        output.push_str("\n#eval Vesper.«main»\n");
        let path = std::env::temp_dir().join(format!(
            "vesper-source-traits-{}-{i}.lean",
            std::process::id()
        ));
        std::fs::write(&path, &output).unwrap();
        let result = std::process::Command::new(&compiler)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}\n{output}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout)
                .trim()
                .lines()
                .last(),
            Some(expected.as_str())
        );
    }
}

#[test]
fn cyclic_associated_type_bindings_are_rejected() {
    // Direct, mutual and through-a-constructor cycles are diagnosed instead
    // of overflowing the compiler's stack.
    for assoc in [
        "type A = Self::A; type B = i64;",
        "type A = Self::B; type B = <Self as T>::A;",
        "type A = Box[Self::A]; type B = i64;",
    ] {
        let src = format!(
            "struct Box[X] {{ v: X }} trait T {{ type A; type B; fn get(in self) -> i64; }}
impl T for i64 {{ {assoc} fn get(in self) -> i64 {{ self }} }}
fn main() -> i64 {{ 0 }}"
        );
        let ds = rejects(&src, "E0601");
        assert!(
            ds.iter().any(|d| d.message.contains("cycle")),
            "{assoc}: {ds:?}"
        );
    }
    // A binding through a projection of the concrete self type is a cycle
    // the coherence gate reports.
    rejects(
        "trait T { type A; fn get(in self) -> i64; }
impl T for i64 { type A = <i64 as T>::A; fn get(in self) -> i64 { self } }
fn f(in x: <i64 as T>::A) -> i64 { 0 } fn main() -> i64 { 0 }",
        "E0605",
    );
}

#[test]
fn qualified_projection_arguments_constrain_selection() {
    let src = "struct X { v: i64 }
trait Convert[U] { type Output; fn convert(in self) -> i64; }
impl Convert[i64] for X { type Output = i64; fn convert(in self) -> i64 { self.v } }
impl Convert[bool] for X { type Output = bool; fn convert(in self) -> i64 { 0 } }
fn f(in b: <X as Convert[bool]>::Output, in n: <X as Convert[i64]>::Output) -> i64 { if b { n } else { 0 } }
fn main() -> i64 { f(true, 42) }";
    assert_eq!(execute(src), "42");
    // A conformance with other arguments does not satisfy the projection.
    rejects(
        "struct X { v: i64 }
trait Convert[U] { type Output; fn convert(in self) -> i64; }
impl Convert[i64] for X { type Output = i64; fn convert(in self) -> i64 { self.v } }
fn f(in b: <X as Convert[bool]>::Output) -> i64 { 0 } fn main() -> i64 { 0 }",
        "E0612",
    );
}

#[test]
fn effect_row_arguments_in_where_clauses() {
    let src = "struct Counter { n: i64 } struct Wrap[C] { c: C }
linear effect Log { fn log(x: i64) -> Unit; }
trait Step[r: eff] { fn step(inout self) -> i64 eff [r]; }
trait Twice { fn twice(in self) -> i64; }
impl Step[Log] for Counter {
    fn step(inout self) -> i64 eff [Log] { Log::log(self.n); self.n = wrapping_add(self.n, 1); self.n }
}
impl[C] Twice for Wrap[C] where C: Step[Log] { fn twice(in self) -> i64 { 2 } }
fn main() -> i64 { let w = Wrap { c: Counter { n: 1 } }; w.twice() }";
    assert_eq!(execute(src), "2");
}

#[test]
fn chained_trait_calls_type_their_receivers() {
    let src = "struct Cell { v: i64 } struct Wrapper { c: Cell }
trait Next { fn next(in self) -> Cell; }
trait Grow { fn grow(in self) -> Cell; }
trait Value { fn value(in self) -> i64; }
impl Next for Wrapper { fn next(in self) -> Cell { Cell { v: wrapping_add(self.c.v, 1) } } }
impl Grow for Cell { fn grow(in self) -> Cell { Cell { v: wrapping_add(self.v, 10) } } }
impl Value for Cell { fn value(in self) -> i64 { wrapping_mul(self.v, 2) } }
fn main() -> i64 {
    let x = Wrapper { c: Cell { v: 20 } };
    let y = Wrapper { c: Cell { v: 0 } };
    wrapping_add(x.next().value(), wrapping_mul(y.next().grow().value(), 100))
}";
    // 21 * 2 = 42; (0 + 1 + 10) * 2 * 100 = 2200.
    assert_eq!(execute(src), "2242");
    // The chain behaves like its let-bound form.
    let bound = src.replace(
        "wrapping_add(x.next().value()",
        "let n = x.next(); wrapping_add(n.value()",
    );
    assert_eq!(execute(&bound), "2242");
}

#[test]
fn inherent_impl_generics_scope_over_method_projections() {
    let src = "copy struct Box[T: mod copy] { v: T }
trait Unwrap { type Inner: mod copy; fn unwrap(in self) -> Self::Inner; }
impl[T: mod copy] Unwrap for Box[T] { type Inner = T; fn unwrap(in self) -> T { self.v } }
impl[T: mod copy] Box[T] { fn get(in self) -> <Box[T] as Unwrap>::Inner { self.v } }
fn main() -> i64 { let b = Box { v: 42 }; b.get() }";
    assert_eq!(execute(src), "42");
}
