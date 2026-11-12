//! Named models, lexical `use model` selection and shadowing, model brands,
//! cross-package orphan rules and revision-bound evidence.
use vesper::{
    ast::Module,
    check, lean, native,
    pact_frontend::{self, Dependency, FrontendOptions},
    parser,
    runtime::Runtime,
};
use vesper_pact::SolverConfig;
use vesper_pact_binding::PackageIdentity;

fn module(source: &str) -> Module {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    p.module
}

fn execute_world(w: &check::World) -> String {
    let interpreted = Runtime::new(w).run("main", vec![]).unwrap().to_string();
    let jit = native::jit(w, "main").unwrap().to_string();
    assert_eq!(jit, interpreted, "JIT and evaluator disagree");
    let (arm, _) = native::object_for_target(w, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183, "EM_AARCH64");
    lean::extract(w).expect("Lean accepts the elaborated core");
    interpreted
}

fn execute(source: &str) -> String {
    let c = check::check(&module(source));
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    execute_world(&c.world)
}

fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let c = check::check(&module(source));
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:?}",
        c.diagnostics
    );
    c.diagnostics
}

const ORD: &str = "copy struct Key { a: i64, b: i64 }
trait Rank { type Weight: mod copy; fn rank(in self) -> i64; fn weight(in self) -> Self::Weight; }
impl Rank for Key { type Weight = i64; fn rank(in self) -> i64 { 1 } fn weight(in self) -> i64 { self.a } }
model ByA: Rank for Key { type Weight = i64; fn rank(in self) -> i64 { 2 } fn weight(in self) -> i64 { self.a } }
model ByB: Rank for Key { type Weight = bool; fn rank(in self) -> i64 { 3 } fn weight(in self) -> bool { self.b > 0 } }
";

#[test]
fn lexical_selection_and_shadowing() {
    let src = format!(
        "{ORD} fn main() -> i64 {{
    let k = Key {{ a: 5, b: 1 }};
    let outer = k.rank();
    let nested = {{
        use model ByA;
        let x = k.rank();
        let y = {{ use model ByB; k.rank() }};
        let z = k.rank();
        wrapping_add(wrapping_mul(x, 100), wrapping_add(wrapping_mul(y, 10), z))
    }};
    let after = k.rank();
    wrapping_add(wrapping_mul(outer, 100000), wrapping_add(wrapping_mul(nested, 10), after))
}}"
    );
    // outer 1, nested (2,3,2) -> 232, after 1: canonical again.
    assert_eq!(execute(&src), "102321");
}

#[test]
fn later_use_in_the_same_block_shadows_earlier() {
    let src = format!(
        "{ORD} fn main() -> i64 {{ let k = Key {{ a: 0, b: 0 }}; use model ByA; let x = k.rank(); use model ByB; wrapping_add(wrapping_mul(x, 10), k.rank()) }}"
    );
    assert_eq!(execute(&src), "23");
}

#[test]
fn module_level_use_applies_everywhere() {
    let src = format!(
        "use model ByB; {ORD} fn f(in k: Key) -> i64 {{ k.rank() }} fn main() -> i64 {{ let k = Key {{ a: 0, b: 0 }}; wrapping_add(f(k), {{ use model ByA; k.rank() }}) }}"
    );
    assert_eq!(execute(&src), "5");
}

#[test]
fn associated_types_follow_the_selected_model() {
    let src = format!(
        "{ORD} fn main() -> i64 {{
    let k = Key {{ a: 7, b: 1 }};
    let w: <Key as Rank>::Weight = k.weight();
    use model ByB;
    let flag: <Key as Rank>::Weight = k.weight();
    if flag {{ w }} else {{ 0 }}
}}"
    );
    assert_eq!(execute(&src), "7");
    // Under ByB the projection is bool, so an i64 annotation is a type error.
    let bad = format!(
        "{ORD} fn main() -> i64 {{ let k = Key {{ a: 7, b: 1 }}; use model ByB; let w: i64 = k.weight(); w }}"
    );
    let ds = check::check(&module(&bad)).diagnostics;
    assert!(ds.iter().any(|d| d.code.starts_with("E02")), "{ds:?}");
}

#[test]
fn explicit_model_paths_check_their_brand() {
    let src = format!(
        "{ORD} fn main() -> i64 {{ let k = Key {{ a: 0, b: 0 }}; wrapping_add(wrapping_mul(ByA::rank(k), 10), ByB::rank(k)) }}"
    );
    assert_eq!(execute(&src), "23");
    // A model of the right trait but another type: wrong brand.
    let wrong = format!(
        "{ORD} copy struct Other {{ v: i64 }} fn main() -> i64 {{ let o = Other {{ v: 0 }}; ByA::rank(o) }}"
    );
    rejects(&wrong, "E0613");
    // A model name that does not exist.
    rejects(
        &format!("{ORD} fn main() -> i64 {{ use model Nope; 0 }}"),
        "E0613",
    );
    // A model whose trait lacks the method.
    rejects(
        &format!("{ORD} fn main() -> i64 {{ let k = Key {{ a: 0, b: 0 }}; ByA::missing(k) }}"),
        "E0608",
    );
}

#[test]
fn named_models_are_never_canonical() {
    // Two models alone give no canonical implementation.
    let src = "copy struct K { v: i64 } trait R { fn r(in self) -> i64; }
model M1: R for K { fn r(in self) -> i64 { 1 } }
model M2: R for K { fn r(in self) -> i64 { 2 } }
fn main() -> i64 { let k = K { v: 0 }; k.r() }";
    rejects(src, "E0608");
    assert_eq!(
        execute(&src.replace("k.r() }", "use model M2; k.r() }")),
        "2"
    );
}

fn dep() -> Dependency {
    Dependency {
        identity: PackageIdentity::new("@demo/base", "1.0.0"),
        module: module("trait Describe { fn describe(in self) -> i64; }"),
    }
}

#[test]
fn orphan_canonical_impls_are_rejected_but_models_allowed() {
    let options = FrontendOptions {
        dependencies: vec![dep()],
        ..FrontendOptions::default()
    };
    let me = PackageIdentity::new("@demo/app", "1.0.0");
    // Neither the trait nor i64 belongs to @demo/app.
    let orphan = module(
        "impl @demo/base::Describe for i64 { fn describe(in self) -> i64 { 1 } } fn main() -> i64 { 0 }",
    );
    let r = pact_frontend::check_package_with(&orphan, &me, &options);
    assert!(
        r.checked
            .diagnostics
            .iter()
            .any(|d| d.code == "E0605" && d.message.contains("orphan")),
        "{:?}",
        r.checked.diagnostics
    );
    // A local self type makes the canonical impl legal.
    let local = module(
        "copy struct Mine { v: i64 } impl @demo/base::Describe for Mine { fn describe(in self) -> i64 { wrapping_add(self.v, 1) } } fn main() -> i64 { let m = Mine { v: 41 }; m.describe() }",
    );
    let r = pact_frontend::check_package_with(&local, &me, &options);
    assert!(
        r.checked.diagnostics.is_empty(),
        "{:?}",
        r.checked.diagnostics
    );
    assert_eq!(execute_world(&r.checked.world), "42");
    // The orphan alternative is accepted as a named model and executes.
    let model = module(
        "model Plain: @demo/base::Describe for i64 { fn describe(in self) -> i64 { wrapping_mul(self, 2) } } fn main() -> i64 { use model Plain; let x = 21; x.describe() }",
    );
    let r = pact_frontend::check_package_with(&model, &me, &options);
    assert!(
        r.checked.diagnostics.is_empty(),
        "{:?}",
        r.checked.diagnostics
    );
    assert_eq!(execute_world(&r.checked.world), "42");
    // An unknown dependency package is diagnosed.
    let unknown = module("impl @demo/nope::Describe for i64 { fn describe(in self) -> i64 { 1 } }");
    let r = pact_frontend::check_package_with(&unknown, &me, &options);
    assert!(r.checked.diagnostics.iter().any(|d| d.code == "E0601"));
}

#[test]
fn cross_package_overlap_is_rejected() {
    let base = Dependency {
        identity: PackageIdentity::new("@demo/base", "1.0.0"),
        module: module(
            "copy struct Shared { v: i64 } trait Describe { fn describe(in self) -> i64; } impl[T: mod copy] Describe for Option[T] { fn describe(in self) -> i64 { 0 } }",
        ),
    };
    let options = FrontendOptions {
        dependencies: vec![base],
        ..FrontendOptions::default()
    };
    let me = PackageIdentity::new("@demo/app", "1.0.0");
    let overlap = module(
        "copy struct Mine { v: i64 } impl @demo/base::Describe for Option[Mine] { fn describe(in self) -> i64 { 1 } }",
    );
    let r = pact_frontend::check_package_with(&overlap, &me, &options);
    assert!(
        r.checked.diagnostics.iter().any(|d| d.code == "E0605"),
        "{:?}",
        r.checked.diagnostics
    );
    // Selecting the dependency's own implementation would need linking.
    let call = module("fn main() -> i64 { let o = Some(1); o.describe() }");
    let r = pact_frontend::check_package_with(&call, &me, &options);
    assert!(
        r.checked.diagnostics.iter().any(|d| d.code == "E0607"),
        "{:?}",
        r.checked.diagnostics
    );
}

#[test]
fn evidence_is_bound_to_the_current_revision() {
    let me = PackageIdentity::new("@demo/app", "1.0.0");
    let v1 = format!("{ORD} fn main() -> i64 {{ let k = Key {{ a: 0, b: 0 }}; k.rank() }}");
    let a = pact_frontend::check_package(&module(&v1), &me);
    assert!(
        a.checked.diagnostics.is_empty(),
        "{:?}",
        a.checked.diagnostics
    );
    assert_eq!(a.evidence.len(), 1);
    let cert = &a.evidence[0].certificate;
    assert!(
        a.evidence[0].selection.contains("Rank.Key"),
        "{}",
        a.evidence[0].selection
    );
    assert_eq!(
        a.checked.world.elaboration.revision.as_deref(),
        Some(a.revision.as_ref().unwrap().0.as_str())
    );
    // An identical rebuild accepts the certificate.
    let b = pact_frontend::check_package(&module(&v1), &me);
    assert_eq!(a.revision, b.revision);
    pact_frontend::revalidate(b.session.as_ref().unwrap(), cert).unwrap();
    // Reformatting does not change declaration identities.
    let reformatted = v1.replace("; ", ";\n    ");
    let c = pact_frontend::check_package(&module(&reformatted), &me);
    assert_eq!(a.revision, c.revision);
    // Any declaration change invalidates the evidence.
    let v2 = v1.replace("model ByB", "model ByC");
    let d = pact_frontend::check_package(&module(&v2), &me);
    assert!(
        d.checked.diagnostics.is_empty(),
        "{:?}",
        d.checked.diagnostics
    );
    assert_ne!(a.revision, d.revision);
    let e = pact_frontend::revalidate(d.session.as_ref().unwrap(), cert).unwrap_err();
    assert_eq!(e.code, "E0614", "{e:?}");
    // So does a source type change and a different package identity.
    let v3 = v1
        .replace(
            "copy struct Key { a: i64, b: i64 }",
            "copy struct Key { a: i64, b: i64, c: i64 }",
        )
        .replace("Key { a: 0, b: 0 }", "Key { a: 0, b: 0, c: 0 }");
    let f = pact_frontend::check_package(&module(&v3), &me);
    assert!(
        f.checked.diagnostics.is_empty(),
        "{:?}",
        f.checked.diagnostics
    );
    assert_eq!(
        pact_frontend::revalidate(f.session.as_ref().unwrap(), cert)
            .unwrap_err()
            .code,
        "E0614"
    );
    let g =
        pact_frontend::check_package(&module(&v1), &PackageIdentity::new("@demo/other", "1.0.0"));
    assert!(pact_frontend::revalidate(g.session.as_ref().unwrap(), cert).is_err());
}

#[test]
fn solver_budgets_report_overflow() {
    let src = "copy struct Box[T: mod copy] { v: T } trait Show { fn show(in self) -> i64; }
impl Show for i64 { fn show(in self) -> i64 { 1 } }
impl[T: mod copy] Show for Box[T] where T: Show { fn show(in self) -> i64 { 2 } }
fn main() -> i64 { let b = Box { v: Box { v: Box { v: 1 } } }; b.show() }";
    let me = PackageIdentity::new("@demo/app", "1.0.0");
    let ok = pact_frontend::check_package(&module(src), &me);
    assert!(
        ok.checked.diagnostics.is_empty(),
        "{:?}",
        ok.checked.diagnostics
    );
    assert_eq!(execute_world(&ok.checked.world), "2");
    let tight = FrontendOptions {
        solver: Some(SolverConfig {
            max_depth: 2,
            ..SolverConfig::default()
        }),
        ..FrontendOptions::default()
    };
    let r = pact_frontend::check_package_with(&module(src), &me, &tight);
    assert!(
        r.checked.diagnostics.iter().any(|d| d.code == "E0611"),
        "{:?}",
        r.checked.diagnostics
    );
}
