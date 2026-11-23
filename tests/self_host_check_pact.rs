//! Self-hosting SH2 lane 13 (#1185), part 1: the declaration pass of the
//! trait frontend in Vesper (`@vesper/check-pact`) against
//! `pact_frontend::declaration_pass` (collecting and validating trait,
//! `impl`, `model`, `use model` and bound declarations, and the failures of
//! lowering them to Pact bindings). Each diagnostic is one line of text
//! (code, span, message; help and labels would be appended) and the texts
//! must be equal on every corpus world, on the checker's trait tests and on
//! mutants that make each diagnostic of the pass fire.
#[path = "common/check_harness.rs"]
mod check_harness;
// The encoder of the hybrid build's swap (`modules/pact`), shared with
// crates/vesper-hybrid/src/hybrid.rs.
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/pact_words.rs"]
mod pact_words;

use check_harness::*;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use vesper::ast::{
    AssocTypeBinding, ConformanceKind, Diagnostic, Effect, Generic, Item, Mode, ModelUse, Module,
    Pass, TraitBoundSyntax, TraitRef, Type, projection_type,
};
use vesper::check::World;
use vesper::pact_frontend;

/// The package identity `declaration_pass` lowers under, as `DefPath`
/// prints it.
const IDENTITY: &str = "@local/main@0.0.0";

/// `pact_frontend::base_module`: the module without trait declarations.
fn base_world(m: &Module) -> World {
    let base = Module {
        items: m
            .items
            .iter()
            .filter(|i| !matches!(i, Item::Trait(_) | Item::Conformance(_)))
            .cloned()
            .collect(),
        ..Module::default()
    };
    World::collect(&base).0
}

/// One line per diagnostic: code, span, message (then help and labels,
/// which this pass never attaches).
fn diagnostic_text(ds: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in ds {
        out += &format!("{} {} {} {}", d.code, d.span.start, d.span.end, d.message);
        if let Some(h) = &d.help {
            out += &format!(" help={h}");
        }
        for l in &d.related {
            out += &format!(" label={}..{} {}", l.span.start, l.span.end, l.message);
        }
        out.push('\n');
    }
    out
}

/// The declaration pass's input and the Rust answer, or `None` when the
/// pass does not run on `m`.
fn task(m: &Module) -> Option<(Vec<u8>, String)> {
    let (prepared, ds) = pact_frontend::declaration_pass(m)?;
    let base = base_world(&prepared);
    let input = pact_words::declaration_input(&prepared, &base, IDENTITY)?;
    Some((input, diagnostic_text(&ds)))
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-pact"))
}

fn parse(src: &str) -> Module {
    let p = vesper::parser::parse(src);
    assert!(p.diagnostics.is_empty(), "{src}: {:?}", p.diagnostics);
    p.module
}

// ---------------------------------------------------------------- inputs

const BASE: &str = "struct Point { x: i64, y: i64 } trait Show { type Out: mod copy; fn show(in self) -> Self::Out; } ";

/// The trait checker tests' sources (tests/source_traits.rs,
/// tests/source_models.rs, tests/bounded_generics.rs) and the rejected
/// fixtures, accepted or not.
fn checker_tests() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    let mut add = |name: &str, src: String| out.push((name.to_owned(), src));
    let main = "fn main() -> i64 { 0 }";
    add(
        "missing assoc and method",
        format!("{BASE} impl Show for Point {{ type Out = i64; }} {main}"),
    );
    add(
        "extra method",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} fn extra(in self) -> i64 {{ 2 }} }} {main}"
        ),
    );
    add(
        "missing assoc",
        format!("{BASE} impl Show for Point {{ fn show(in self) -> i64 {{ 1 }} }} {main}"),
    );
    add(
        "wrong result",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> bool {{ true }} }} {main}"
        ),
    );
    add(
        "extra param",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self, x: i64) -> i64 {{ x }} }} {main}"
        ),
    );
    add(
        "static trait fn",
        format!("trait Make {{ fn make() -> i64; }} {main}"),
    );
    add(
        "unknown trait",
        format!("{BASE} impl Missing for Point {{ }} {main}"),
    );
    add(
        "trait named like a type",
        format!("struct Show {{ x: i64 }} trait Show {{ }} {main}"),
    );
    add(
        "pass mismatch",
        format!(
            "struct C {{ n: i64 }} trait Next {{ fn next(inout self) -> i64; }} impl Next for C {{ fn next(in self) -> i64 {{ self.n }} }} {main}"
        ),
    );
    add(
        "effect mismatch",
        format!(
            "struct C {{ n: i64 }} effect Log {{ fn log(x: i64) -> Unit; }} trait Next {{ fn next(inout self) -> i64; }} impl Next for C {{ fn next(inout self) -> i64 eff [Log] {{ Log::log(1); self.n }} }} {main}"
        ),
    );
    add(
        "assoc mode bound",
        format!(
            "struct C {{ n: i64 }} trait Src {{ type Item: mod copy; fn get(in self) -> Self::Item; }} impl Src for C {{ type Item = String; fn get(in self) -> String {{ \"x\" }} }} {main}"
        ),
    );
    add(
        "arg mode bound",
        format!(
            "struct C {{ n: i64 }} trait Keep[T: mod copy] {{ fn keep(in self) -> i64; }} impl Keep[String] for C {{ fn keep(in self) -> i64 {{ 0 }} }} {main}"
        ),
    );
    add(
        "duplicate impl",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 2 }} }} {main}"
        ),
    );
    add("blanket overlap", "copy struct Box[T: mod copy] { v: T } trait Show { fn show(in self) -> i64; }\nimpl[T: mod copy] Show for Box[T] { fn show(in self) -> i64 { 1 } }\nimpl Show for Box[i64] { fn show(in self) -> i64 { 2 } }\nfn main() -> i64 { 0 }".into());
    add("two traits", "struct P { x: i64 } trait A { fn get(in self) -> i64; } trait B { fn get(in self) -> i64; }\nimpl A for P { fn get(in self) -> i64 { 1 } } impl B for P { fn get(in self) -> i64 { 2 } }\nfn main() -> i64 { let p = P { x: 0 }; p.get() }".into());
    add("deep where", "copy struct Box[T: mod copy] { v: T } trait Deep { fn deep(in self) -> i64; }\nimpl[T: mod copy] Deep for Box[T] where Box[Box[T]]: Deep { fn deep(in self) -> i64 { 0 } }\nfn main() -> i64 { let b = Box { v: 1 }; b.deep() }".into());
    add(
        "bounded generic",
        format!("{BASE} fn f[T: Show](in x: T) -> i64 {{ 0 }} {main}"),
    );
    add(
        "where generic",
        format!("{BASE} fn f[T](in x: T) -> i64 where T: Show {{ 0 }} {main}"),
    );
    add(
        "unbounded call",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} fn f[T](in x: T) -> i64 {{ x.show() }} {main}"
        ),
    );
    add(
        "projection escapes",
        format!("{BASE} fn f[T](in x: T) -> T::Out {{ x }} {main}"),
    );
    add(
        "projection field",
        format!("{BASE} struct Bad {{ v: Point::Out }} {main}"),
    );
    add(
        "projection unknown assoc",
        format!(
            "{BASE} impl Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} fn f(in x: Point) -> <Point as Show>::Nope {{ 0 }} {main}"
        ),
    );
    for (k, assoc) in [
        "type A = Self::A; type B = i64;",
        "type A = Self::B; type B = <Self as T>::A;",
        "type A = Box[Self::A]; type B = i64;",
    ]
    .iter()
    .enumerate()
    {
        add(
            &format!("assoc cycle {k}"),
            format!(
                "struct Box[X] {{ v: X }} trait T {{ type A; type B; fn get(in self) -> i64; }}\nimpl T for i64 {{ {assoc} fn get(in self) -> i64 {{ self }} }}\n{main}"
            ),
        );
    }
    add("concrete cycle", "trait T { type A; fn get(in self) -> i64; }\nimpl T for i64 { type A = <i64 as T>::A; fn get(in self) -> i64 { self } }\nfn f(in x: <i64 as T>::A) -> i64 { 0 } fn main() -> i64 { 0 }".into());
    add("qualified projections", "struct X { v: i64 }\ntrait Convert[U] { type Output; fn convert(in self) -> i64; }\nimpl Convert[i64] for X { type Output = i64; fn convert(in self) -> i64 { self.v } }\nimpl Convert[bool] for X { type Output = bool; fn convert(in self) -> i64 { 0 } }\nfn f(in b: <X as Convert[bool]>::Output, in n: <X as Convert[i64]>::Output) -> i64 { if b { n } else { 0 } }\nfn main() -> i64 { f(true, 42) }".into());
    add(
        "unknown model use",
        format!("{BASE} fn main() -> i64 {{ use model Nope; 0 }}"),
    );
    add(
        "model like a type",
        format!(
            "{BASE} model Point: Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ 1 }} }} {main}"
        ),
    );
    add(
        "model and use",
        format!(
            "{BASE} model ByX: Show for Point {{ type Out = i64; fn show(in self) -> i64 {{ self.x }} }} fn main() -> i64 {{ use model ByX; let p = Point {{ x: 1, y: 2 }}; p.show() }}"
        ),
    );
    add("row parameter", "struct C { n: i64 } effect Log { fn log(x: i64) -> Unit; }\ntrait Run[e: eff] { fn run(in self) -> i64 eff [e]; }\nimpl Run[Log] for C { fn run(in self) -> i64 eff [Log] { Log::log(1); self.n } }\nfn main() -> i64 { 0 }".into());
    add("open row", "struct C { n: i64 }\ntrait Run[e: eff] { fn run(in self) -> i64 eff [e]; }\nimpl[r: eff] Run[r] for C { fn run(in self) -> i64 eff [r] { self.n } }\nfn main() -> i64 { 0 }".into());
    let fixtures = [
        "examples/traits/shapes.ves",
        "examples/traits/overlap_rejected.ves",
        "tests/fixtures/bounded_generics/reject/ambiguous_method.ves",
        "tests/fixtures/bounded_generics/reject/derive_field.ves",
        "tests/fixtures/bounded_generics/reject/derive_nested.ves",
        "tests/fixtures/bounded_generics/reject/derive_shape.ves",
        "tests/fixtures/bounded_generics/reject/trait_method_bound.ves",
        "tests/fixtures/trace_effects/reject/telemetry_secret_impl.ves",
        "tests/fixtures/trace_effects/reject/telemetry_reads_secret.ves",
        "tests/fixtures/trace_effects/reject/telemetry_duplicate.ves",
    ];
    for f in fixtures {
        if let Ok(src) = std::fs::read_to_string(repo().join(f)) {
            out.push((f.to_owned(), src));
        }
    }
    out
}

fn named(n: &str) -> Type {
    Type::named(n)
}

/// Mutants of `m` that make the pass's diagnostics fire: each changes one
/// declaration (the `k`-th of its kind, so every corpus world contributes
/// different ones).
fn mutants(m: &Module, k: usize) -> Vec<(&'static str, Module)> {
    let mut out: Vec<(&'static str, Module)> = vec![];
    let confs: Vec<usize> = (0..m.items.len())
        .filter(|i| matches!(m.items[*i], Item::Conformance(_)))
        .collect();
    let traits: Vec<usize> = (0..m.items.len())
        .filter(|i| matches!(m.items[*i], Item::Trait(_)))
        .collect();
    let adts: Vec<usize> = (0..m.items.len())
        .filter(|i| matches!(m.items[*i], Item::Adt(_)))
        .collect();
    let effects: Vec<usize> = (0..m.items.len())
        .filter(|i| matches!(m.items[*i], Item::Effect(_)))
        .collect();
    let pick = |v: &[usize]| (!v.is_empty()).then(|| v[k % v.len()]);
    let mut conf = |label: &'static str, f: &dyn Fn(&mut vesper::ast::Conformance) -> bool| {
        if let Some(i) = pick(&confs) {
            let mut x = m.clone();
            if let Item::Conformance(c) = &mut x.items[i]
                && f(c)
            {
                out.push((label, x));
            }
        }
    };
    conf("drop method", &|c| c.methods.pop().is_some());
    conf("duplicate method", &|c| {
        let Some(f) = c.methods.first().cloned() else {
            return false;
        };
        c.methods.push(f);
        true
    });
    conf("rename method", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.name.push_str("_x");
        true
    });
    conf("pass", &|c| {
        let Some(p) = c.methods.first_mut().and_then(|f| f.params.first_mut()) else {
            return false;
        };
        p.pass = match p.pass {
            Pass::In => Pass::Inout,
            Pass::Inout => Pass::Sink,
            Pass::Sink => Pass::In,
        };
        true
    });
    conf("not self first", &|c| {
        let Some(p) = c.methods.first_mut().and_then(|f| f.params.first_mut()) else {
            return false;
        };
        p.name = "me".into();
        true
    });
    conf("projections", &|c| {
        let Some(p) = c.methods.first_mut().and_then(|f| f.params.first_mut()) else {
            return false;
        };
        p.projections.push(vec!["x".into()]);
        true
    });
    conf("param type", &|c| {
        let Some(p) = c.methods.first_mut().and_then(|f| f.params.get_mut(1)) else {
            return false;
        };
        p.ty = Type::Tuple(vec![named("bool"), p.ty.clone()]);
        true
    });
    conf("result", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.result = Type::Named("Option".into(), vec![f.result.clone()]);
        true
    });
    conf("extra param", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        let mut p = f
            .params
            .first()
            .cloned()
            .unwrap_or_else(|| vesper::ast::Param {
                name: "y".into(),
                pass: Pass::In,
                projections: vec![],
                ty: named("i64"),
                span: f.span.clone(),
            });
        p.name = "y".into();
        f.params.push(p);
        true
    });
    conf("generic method", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.generics.push(Generic {
            name: "Q".into(),
            bound: Mode::Affine,
            row: false,
        });
        true
    });
    conf("extra effect", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.effects.instances.push(Effect {
            label: None,
            name: "Alloc".into(),
            args: vec![named("i64")],
        });
        true
    });
    conf("labelled effect", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.effects.instances.push(Effect {
            label: Some("cap".into()),
            name: "Fail".into(),
            args: vec![],
        });
        true
    });
    conf("open row", &|c| {
        let Some(f) = c.methods.first_mut() else {
            return false;
        };
        f.effects.tail = Some("zz".into());
        true
    });
    conf("drop assoc", &|c| c.assoc.pop().is_some());
    conf("duplicate assoc", &|c| {
        let Some(a) = c.assoc.first().cloned() else {
            return false;
        };
        c.assoc.push(a);
        true
    });
    conf("unknown assoc", &|c| {
        c.assoc.push(AssocTypeBinding {
            name: "Nope".into(),
            ty: named("i64"),
            span: c.span.clone(),
        });
        true
    });
    conf("assoc mode", &|c| {
        let Some(a) = c.assoc.first_mut() else {
            return false;
        };
        a.ty = named("String");
        true
    });
    conf("assoc cycle", &|c| {
        let Some(a) = c.assoc.first_mut() else {
            return false;
        };
        a.ty = projection_type(named("Self"), None, a.name.clone());
        true
    });
    conf("assoc unknown projection", &|c| {
        let Some(a) = c.assoc.first_mut() else {
            return false;
        };
        a.ty = projection_type(named("i64"), Some(("Nope".into(), vec![])), "A");
        true
    });
    conf("assoc shorthand", &|c| {
        let Some(a) = c.assoc.first_mut() else {
            return false;
        };
        a.ty = projection_type(named("i64"), None, "Nope");
        true
    });
    conf("unknown trait", &|c| {
        c.trait_ref.name.push_str("_x");
        true
    });
    conf("dependency trait", &|c| {
        c.trait_ref.package = Some("@dep/x".into());
        true
    });
    conf("trait arity", &|c| {
        c.trait_ref.args.push(named("i64"));
        true
    });
    conf("trait arg mode", &|c| {
        if c.trait_ref.args.is_empty() {
            return false;
        }
        c.trait_ref.args[0] = named("String");
        true
    });
    conf("function self type", &|c| {
        c.self_ty = Type::Function {
            params: vec![(Pass::In, named("i64"))],
            result: Box::new(named("i64")),
            effects: Default::default(),
            mode: Mode::Copy,
        };
        true
    });
    conf("infer self type", &|c| {
        c.self_ty = Type::Named("Option".into(), vec![Type::Infer]);
        true
    });
    conf("unknown where", &|c| {
        c.where_clauses.push(vesper::ast::WhereClause {
            ty: named("i64"),
            trait_ref: TraitRef {
                package: None,
                name: "Nope".into(),
                args: vec![],
                span: c.span.clone(),
            },
            span: c.span.clone(),
        });
        true
    });
    conf("where arity", &|c| {
        let r = c.trait_ref.clone();
        let mut r2 = r.clone();
        r2.args.push(named("bool"));
        c.where_clauses.push(vesper::ast::WhereClause {
            ty: c.self_ty.clone(),
            trait_ref: r2,
            span: c.span.clone(),
        });
        true
    });
    conf("model rename", &|c| {
        c.kind = ConformanceKind::Model {
            name: "Point".into(),
            public: false,
        };
        true
    });
    // A second copy of the conformance: overlapping canonical impls.
    if let Some(i) = pick(&confs) {
        let mut x = m.clone();
        let item = x.items[i].clone();
        x.items.push(item);
        out.push(("duplicate conformance", x));
    }
    let mut tr = |label: &'static str, f: &dyn Fn(&mut vesper::ast::TraitDecl) -> bool| {
        if let Some(i) = pick(&traits) {
            let mut x = m.clone();
            if let Item::Trait(t) = &mut x.items[i]
                && f(t)
            {
                out.push((label, x));
            }
        }
    };
    tr("trait method generics", &|t| {
        let Some(f) = t.methods.first_mut() else {
            return false;
        };
        f.generics.push(Generic {
            name: "Q".into(),
            bound: Mode::Affine,
            row: false,
        });
        true
    });
    tr("trait static method", &|t| {
        let Some(f) = t.methods.first_mut() else {
            return false;
        };
        f.params.clear();
        true
    });
    tr("trait self type", &|t| {
        let Some(p) = t.methods.first_mut().and_then(|f| f.params.first_mut()) else {
            return false;
        };
        p.ty = named("i64");
        true
    });
    tr("trait second self", &|t| {
        let Some(f) = t.methods.first_mut() else {
            return false;
        };
        let Some(mut p) = f.params.first().cloned() else {
            return false;
        };
        p.ty = Type::Infer;
        f.params.push(p);
        true
    });
    tr("trait duplicate method", &|t| {
        let Some(f) = t.methods.first().cloned() else {
            return false;
        };
        t.methods.push(f);
        true
    });
    tr("trait duplicate assoc", &|t| {
        let Some(a) = t.assoc.first().cloned() else {
            return false;
        };
        t.assoc.push(a);
        true
    });
    tr("trait Self parameter", &|t| {
        t.generics.push(Generic {
            name: "Self".into(),
            bound: Mode::Affine,
            row: false,
        });
        true
    });
    tr("trait duplicate parameter", &|t| {
        let Some(g) = t.generics.first().cloned() else {
            return false;
        };
        t.generics.push(g);
        true
    });
    tr("trait new assoc", &|t| {
        t.assoc.push(vesper::ast::AssocTypeDecl {
            name: "Fresh".into(),
            bound: Mode::Copy,
            span: t.span.clone(),
        });
        true
    });
    tr("trait new method", &|t| {
        let Some(mut f) = t.methods.first().cloned() else {
            return false;
        };
        f.name = "fresh".into();
        t.methods.push(f);
        true
    });
    // A trait named like a type, and a second trait of the same name.
    if let (Some(i), Some(a)) = (pick(&traits), pick(&adts)) {
        let mut x = m.clone();
        let Item::Adt(adt) = &m.items[a] else {
            unreachable!()
        };
        if let Item::Trait(t) = &mut x.items[i] {
            t.name = adt.name.clone();
        }
        out.push(("trait like a type", x));
    }
    if let Some(i) = pick(&traits) {
        let mut x = m.clone();
        let item = x.items[i].clone();
        x.items.insert(i, item);
        out.push(("duplicate trait", x));
    }
    // `use model` and bounds.
    {
        let mut x = m.clone();
        x.model_uses.push(ModelUse {
            model: "Nope".into(),
            span: 0..0,
            scope: 0..0,
        });
        out.push(("unknown model use", x));
    }
    let first_trait = traits.first().map(|i| match &m.items[*i] {
        Item::Trait(t) => t.clone(),
        _ => unreachable!(),
    });
    if let Some(t) = first_trait {
        let fns: Vec<(String, Vec<Generic>)> = m
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Function(f) => Some((f.name.clone(), f.generics.clone())),
                _ => None,
            })
            .collect();
        let r = TraitRef {
            package: None,
            name: t.name.clone(),
            args: t.generics.iter().map(|_| named("i64")).collect(),
            span: t.span.clone(),
        };
        let bound = |param: &str, owner: &str, r: &TraitRef| TraitBoundSyntax {
            param: param.into(),
            trait_ref: r.clone(),
            span: t.span.clone(),
            owner: owner.into(),
        };
        let mut push = |label: &'static str, b: TraitBoundSyntax| {
            let mut x = m.clone();
            x.trait_bounds.push(b);
            out.push((label, x));
        };
        push("bound nowhere", bound("T", "", &r));
        push(
            "bound unsupported",
            bound("T", "$unsupported::Show::show", &r),
        );
        push("bound unknown owner", bound("T", "nope_fn", &r));
        if let Some((f, gs)) = fns.get(k % fns.len().max(1)) {
            push("bound not a parameter", bound("NotAParam", f, &r));
            if let Some(g) = gs.iter().find(|g| !g.row) {
                let mut bad = r.clone();
                bad.name = "Nope".into();
                push("bound unknown trait", bound(&g.name, f, &bad));
                let mut arity = r.clone();
                arity.args.push(named("bool"));
                push("bound arity", bound(&g.name, f, &arity));
                let mut dep = r.clone();
                dep.package = Some("@dep/x".into());
                push("bound dependency", bound(&g.name, f, &dep));
                push("bound ok", bound(&g.name, f, &r));
            }
        }
        if let Some(a) = pick(&adts) {
            let Item::Adt(adt) = &m.items[a] else {
                unreachable!()
            };
            let name = adt.name.clone();
            match adt.generics.iter().find(|g| !g.row) {
                Some(g) => push("type bound", bound(&g.name, &name, &r)),
                None => push("type bound not a parameter", bound("T", &name, &r)),
            }
        }
    }
    // Projections in an ADT field and an effect operation.
    if let Some(a) = pick(&adts) {
        let mut x = m.clone();
        if let Item::Adt(adt) = &mut x.items[a]
            && let Some(f) = adt.variants.iter_mut().flat_map(|v| &mut v.fields).next()
        {
            f.ty = projection_type(named("i64"), None, "Out");
            out.push(("projection field", x));
        }
    }
    if let Some(e) = pick(&effects) {
        let mut x = m.clone();
        if let Item::Effect(d) = &mut x.items[e]
            && let Some(op) = d.ops.first_mut()
        {
            op.result = projection_type(named("i64"), None, "Out");
            out.push(("projection effect op", x));
        }
    }
    out
}

/// The cases: corpus worlds the pass runs on, the checker tests, and the
/// mutants of both.
fn cases() -> &'static [(String, Vec<u8>, String)] {
    static CASES: OnceLock<Vec<(String, Vec<u8>, String)>> = OnceLock::new();
    CASES.get_or_init(|| {
        let mut modules: Vec<(String, Module)> = corpus()
            .iter()
            .map(|c| (c.name.clone(), c.module.clone()))
            .collect();
        for (name, src) in checker_tests() {
            modules.push((format!("test: {name}"), parse(&src)));
        }
        let mut out = vec![];
        let mut seen = BTreeSet::new();
        for (k, (name, m)) in modules.iter().enumerate() {
            let Some((input, text)) = task(m) else {
                continue;
            };
            out.push((name.clone(), input, text));
            for (label, x) in mutants(m, k) {
                if let Some((input, text)) = task(&x)
                    && seen.insert(input.clone())
                {
                    out.push((format!("{name}#{label}"), input, text));
                }
            }
        }
        out
    })
}

fn run(optimize: bool) -> (usize, usize, BTreeSet<String>) {
    let f = package().entry("check_pact_declarations", optimize);
    let mut bad = 0;
    let mut lines = 0;
    let mut codes = BTreeSet::new();
    for (name, input, rust) in cases() {
        let vesper = f.text(input);
        lines += rust.lines().count();
        for l in rust.lines() {
            codes.insert(l.split(' ').next().unwrap_or("").to_owned());
        }
        if let Some(d) = first_difference(rust, &vesper) {
            bad += 1;
            if bad <= 8 {
                eprintln!("{name} differs at {d}");
            }
        }
    }
    let mutants = cases().iter().filter(|c| c.0.contains('#')).count();
    let tests = cases()
        .iter()
        .filter(|c| c.0.starts_with("test: ") && !c.0.contains('#'))
        .count();
    eprintln!(
        "declarations: {bad} of {} cases differ ({} corpus worlds, {tests} checker tests, {mutants} mutants; {lines} diagnostics, codes {codes:?})",
        cases().len(),
        cases().len() - mutants - tests
    );
    (bad, lines, codes)
}

#[test]
fn declaration_pass_matches_rust() {
    let (bad, lines, codes) = run(true);
    assert_eq!(bad, 0);
    assert!(lines > 0);
    // Every code the pass reports fires at least once.
    for c in [
        "E0102", "E0600", "E0601", "E0602", "E0603", "E0604", "E0605", "E0606", "E0607", "E0613",
    ] {
        assert!(codes.contains(c), "{c} never fires: {codes:?}");
    }
}

#[test]
fn unoptimized_jit_agrees() {
    let (bad, _, _) = run(false);
    assert_eq!(bad, 0);
}

#[test]
fn every_message_of_the_pass_fires() {
    // One fixed message stem per diagnostic the pass can report (the
    // `Frontend::new` part of src/pact_frontend.rs without dependencies).
    let stems = [
        "conflicts with a type of the same name",
        "conflicts with a trait of the same name",
        "duplicate or reserved trait parameter",
        "duplicate associated type",
        "duplicate trait method",
        "generic trait methods are not supported",
        "a trait method's 'self' has type Self",
        "needs a 'self' receiver",
        "'self' must be the first parameter",
        "trait method parameters require a type",
        "unknown trait",
        "unknown dependency package",
        "overlaps an earlier canonical impl",
        "is not a model declared in this package",
        "takes 0 arguments, found 1",
        "exceeds its modal bound",
        "is not an associated type of",
        "is given twice",
        "missing associated type",
        "associated type binding cycle",
        "is defined twice",
        "is not a method of trait",
        "missing method",
        "generic methods in impls and models are not supported",
        "parameters in the trait, found",
        "must take 'self' first",
        "is passed",
        "changes the trait's place projections",
        "has type",
        "returns",
        "but the trait signature does not permit it",
        "labelled capabilities in trait method signatures",
        "has an open effect row",
        "is not supported here; bounds go on",
        "constrains an unknown item",
        "is not a type parameter of",
        "projections in the fields of",
        "projections in effect operation signatures",
        "cannot lower",
        "has no Pact type constructor",
        "unresolved source type",
        "in a projection",
        "no trait declares an associated type",
    ];
    let all: String = cases().iter().map(|c| c.2.as_str()).collect();
    for s in stems {
        assert!(all.contains(s), "no case reports '{s}'");
    }
}

/// The declaration pass of `@vesper/check-pact` as the hybrid build
/// installs it: the shared encoder, the JIT entry and the text decoder.
fn vesper_declarations(
    m: &Module,
    base: &World,
    identity: &vesper_pact_binding::PackageIdentity,
) -> Option<Vec<Diagnostic>> {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    let f = ENTRY.get_or_init(|| package().entry("check_pact_declarations", true));
    let input = pact_words::declaration_input(m, base, &identity.to_string())?;
    Some(pact_words::decode_declaration_text(&f.text(&input)))
}

#[test]
fn check_package_agrees_through_the_swap_point() {
    // `check_package` with the Vesper pass installed answers every checker
    // test and its mutants as the Rust pass does (diagnostics compared as
    // values, so the decoder is checked too). The corpus goes through the
    // hybrid build's suite.
    assert_eq!(pact_frontend::default_identity().to_string(), IDENTITY);
    let mut modules = vec![];
    for (k, (name, src)) in checker_tests().into_iter().enumerate() {
        let m = parse(&src);
        for (label, x) in mutants(&m, k) {
            modules.push((format!("{name}#{label}"), x));
        }
        modules.push((name, m));
    }
    let check = |m: &Module| {
        pact_frontend::check_package(m, &pact_frontend::default_identity())
            .checked
            .diagnostics
    };
    let rust: Vec<_> = modules.iter().map(|(_, m)| check(m)).collect();
    assert!(
        pact_frontend::install_declaration_pass(pact_frontend::DeclarationPassTable {
            diagnostics: vesper_declarations,
        })
        .is_ok()
    );
    let mut bad = 0;
    let mut rejected = 0;
    for ((name, m), rust) in modules.iter().zip(&rust) {
        let (prepared, _) = match pact_frontend::declaration_pass(m) {
            Some(p) => p,
            None => (m.clone(), vec![]),
        };
        let base = base_world(&prepared);
        let identity = pact_frontend::default_identity();
        if vesper_declarations(&prepared, &base, &identity).is_some_and(|d| !d.is_empty()) {
            rejected += 1;
        }
        let swapped = check(m);
        if swapped != *rust {
            bad += 1;
            if bad <= 8 {
                eprintln!("{name}: rust {rust:?}\nswapped {swapped:?}");
            }
        }
    }
    eprintln!(
        "check_package: {bad} of {} modules differ through the swap point ({rejected} rejected by the Vesper pass)",
        modules.len()
    );
    assert_eq!(bad, 0);
    assert!(rejected > 0);
}

#[test]
fn answers_depend_on_the_input() {
    // A module without declarations does not run the pass; one with a bad
    // impl reports it, and the Vesper side reads the same input.
    assert!(pact_frontend::declaration_pass(&parse("fn main() -> i64 { 0 }")).is_none());
    let m = parse(&format!(
        "{BASE} impl Show for Point {{ fn show(in self) -> i64 {{ 1 }} }} fn main() -> i64 {{ 0 }}"
    ));
    let (input, rust) = task(&m).unwrap();
    assert!(rust.contains("E0601"), "{rust}");
    let f = package().entry("check_pact_declarations", true);
    assert_eq!(f.text(&input), rust);
}

#[test]
#[ignore = "timing: CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_check_pact -- --ignored"]
fn declaration_pass_speed() {
    if !speed() {
        return;
    }
    let f = package().entry("check_pact_declarations", true);
    let identity = pact_frontend::default_identity();
    // The swap point's inputs on the corpus worlds the pass runs on: the
    // prepared module and its base world, as `check_package` holds them.
    let worlds: Vec<(Module, World, Vec<u8>)> = corpus()
        .iter()
        .filter_map(|c| {
            let (prepared, _) = pact_frontend::declaration_pass(&c.module)?;
            let base = base_world(&prepared);
            let input = pact_words::declaration_input(&prepared, &base, IDENTITY)?;
            Some((prepared, base, input))
        })
        .collect();
    eprintln!("{} corpus worlds run the pass", worlds.len());
    // Like for like, behind the swap point: Rust's
    // `rust_declaration_diagnostics` (`Frontend::new` given the base world:
    // collecting, validating and building the binding DTOs) against the
    // hybrid's answer (encoding, the Vesper pass, decoding).
    abba(
        "declarations (swap point, with encode and decode)",
        &mut || {
            time(|| {
                for (m, base, _) in &worlds {
                    std::hint::black_box(pact_frontend::rust_declaration_diagnostics(
                        m, base, &identity,
                    ));
                }
            })
        },
        &mut || {
            time(|| {
                for (m, base, _) in &worlds {
                    let input = pact_words::declaration_input(m, base, IDENTITY).unwrap();
                    std::hint::black_box(pact_words::decode_declarations(&f.words(&input)));
                }
            })
        },
    );
    // The Vesper pass alone on prebuilt inputs.
    abba(
        "declarations (pass alone)",
        &mut || {
            time(|| {
                for (m, base, _) in &worlds {
                    std::hint::black_box(pact_frontend::rust_declaration_diagnostics(
                        m, base, &identity,
                    ));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, _, input) in &worlds {
                    std::hint::black_box(f.words(input));
                }
            })
        },
    );
}

fn session_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("check_pact_session", true))
}

fn queries_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("check_pact_queries", true))
}

fn build_session_input_for(
    m: &Module,
    pkg_name: &str,
    pkg_ver: &str,
    max_depth: u64,
    max_steps: u64,
    max_fix: u64,
) -> Option<(Module, Vec<u8>)> {
    let (prepared, _) = pact_frontend::declaration_pass(m)?;
    let base_mod = Module {
        sugar_origins: prepared.sugar_origins.clone(),
        items: prepared
            .items
            .iter()
            .filter(|i| !matches!(i, Item::Trait(_) | Item::Conformance(_)))
            .cloned()
            .collect(),
        ..Module::default()
    };
    let base = World::collect(&base_mod).0;
    let prelude = pact_frontend::prelude_world();
    let fp = pact_frontend::declaration_fingerprint(&prepared);
    let input = pact_words::session_input(
        &prepared, &base, &prelude, pkg_name, pkg_ver, max_depth, max_steps, max_fix, &fp,
    )?;
    Some((prepared, input))
}

fn rust_query_solve(
    sess: &vesper_pact_binding::CoherentSession,
    tables_confs: &[String],
    q: &vesper_pact_binding::QueryBindingDto,
) -> Result<(usize, String), (u8, String)> {
    let outcome = sess.solve(q).map_err(|e| (0u8, e.to_string()))?;
    match outcome {
        vesper_pact_binding::BridgeOutcome::Proven(e) => {
            let consumed = sess.consume(&e).map_err(|e| (0u8, e.to_string()))?;
            let path = match &consumed.selection {
                vesper_pact_binding::Selection::Impl { path, .. }
                | vesper_pact_binding::Selection::Model { path, .. } => path.to_string(),
                vesper_pact_binding::Selection::Assumption(_) => {
                    return Err((
                        5,
                        "the obligation is discharged by a trait bound of the enclosing generic"
                            .into(),
                    ));
                }
                other => {
                    return Err((
                        4,
                        format!(
                            "the obligation is discharged by {other}, which needs generic dictionary dispatch"
                        ),
                    ));
                }
            };
            let idx = tables_confs
                .iter()
                .position(|p| *p == path)
                .ok_or_else(|| (0u8, format!("no declaration for {path}")))?;
            Ok((idx, consumed.selection.to_string()))
        }
        vesper_pact_binding::BridgeOutcome::NoSolution { explanation, .. } => Err((1, explanation)),
        vesper_pact_binding::BridgeOutcome::Ambiguous { explanation, .. } => Err((2, explanation)),
        vesper_pact_binding::BridgeOutcome::Overflow { kind } => Err((3, format!("{kind:?}"))),
        vesper_pact_binding::BridgeOutcome::Unsupported { explanation, .. } => {
            Err((0, explanation))
        }
    }
}

fn with_large_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn session_diagnostics_agree_across_corpus_and_mutants() {
    with_large_stack(|| {
        let f = session_entry();
        let mut cases: Vec<(String, Module, String, String)> = vec![];
        for c in corpus() {
            cases.push((
                c.name.clone(),
                c.module.clone(),
                "@local/main".into(),
                "0.0.0".into(),
            ));
        }
        for (k, (name, src)) in checker_tests().into_iter().enumerate() {
            let m = parse(&src);
            for (label, x) in mutants(&m, k) {
                cases.push((
                    format!("{name}#{label}"),
                    x,
                    "@local/main".into(),
                    "0.0.0".into(),
                ));
            }
            cases.push((name, m, "@local/main".into(), "0.0.0".into()));
        }

        // Session-phase specific mutants (Part 2b): identity, BRIDGE0001, BRIDGE0002, PACT0102, PACT0103.
        let id_ok = parse(
            "trait Tag { fn tag(in self) -> i64; } impl Tag for i64 { fn tag(in self) -> i64 { 1 } } fn main() -> i64 { 0 }",
        );
        cases.push((
            "bad_pkg_name".into(),
            id_ok.clone(),
            "Bad Name".into(),
            "0.0.0".into(),
        ));
        cases.push((
            "bad_pkg_ver".into(),
            id_ok.clone(),
            "@a/b".into(),
            "1 0".into(),
        ));
        cases.push((
            "reserved_pkg".into(),
            id_ok.clone(),
            "@vesper/builtin".into(),
            "0.0.0".into(),
        ));
        cases.push((
            "pkg_order_gt".into(),
            id_ok,
            "@z/pkg".into(),
            "1.0.0".into(),
        ));

        // BRIDGE0001: unknown primitive (`f32`), wrong arg mode on Copy ADT.
        cases.push((
            "bridge_unknown_f32".into(),
            parse("trait T { fn f(in self) -> i64; } impl T for f32 { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }"),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        cases.push((
            "bridge_copy_mode_violation".into(),
            parse(
                "struct CopyBox[T: mod copy] { v: T } trait T { fn f(in self) -> i64; } impl T for CopyBox[String] { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        // BRIDGE0002: Wide9 (>6561 combinations), Tuple13 (>12 elements), projection to Copy bound.
        cases.push((
            "bridge_wide9".into(),
            parse(
                "struct Wide[A, B, C, D, E, F, G, H, I] { a: A, b: B, c: C, d: D, e: E, f: F, g: G, h: H, i: I } trait T { fn f(in self) -> i64; } impl T for i64 { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        cases.push((
            "bridge_tuple13".into(),
            parse(
                "trait T { fn f(in self) -> i64; } impl T for (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        cases.push((
            "bridge_proj_in_copy_box".into(),
            parse(
                "struct CopyBox[T: mod copy] { v: T } trait K { type A; } trait T { fn f(in self) -> i64; } impl[X] T for CopyBox[<X as K>::A] where X: K { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        // PACT0102: overlapping parameterized canonical impls; PACT0103: impl not well-formed.
        cases.push((
            "pact_overlap".into(),
            parse(
                "struct Pair[A, B] { a: A, b: B } trait T { fn f(in self) -> i64; } impl[X] T for Pair[X, i64] { fn f(in self) -> i64 { 0 } } impl[Y] T for Pair[i64, Y] { fn f(in self) -> i64 { 1 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        cases.push((
            "pact_not_wf".into(),
            parse(
                "struct CopyBox[T: mod copy] { v: T } trait T { fn f(in self) -> i64; } impl[X] T for CopyBox[X] { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));
        // Duplicate effect labels in row argument.
        cases.push((
            "effect_row_arg".into(),
            parse(
                "effect E { fn e() -> Unit; } trait T[r: eff] { fn f(in self) -> i64; } impl T[E] for i64 { fn f(in self) -> i64 { 0 } } fn main() -> i64 { 0 }",
            ),
            "@test/s".into(),
            "0.1.0".into(),
        ));

        let mut diffs = 0usize;
        let mut rejected = 0usize;
        for (name, m, pkg, ver) in &cases {
            let Some((_prepared, input)) = build_session_input_for(m, pkg, ver, 48, 100_000, 16)
            else {
                continue;
            };
            let vesper_diags = pact_words::decode_session_words(&f.words(&input));
            let ident = vesper_pact_binding::PackageIdentity::new(pkg, ver);
            let rust = pact_frontend::check_package(m, &ident).checked.diagnostics;

            // Only diagnostics produced before pass1/pass2 (E0600, E0601, E0605, E0606, E0607, E0613)
            // fire inside `session_open`; clean `session_open` means Rust either succeeds cleanly
            // or later fails in pass1/mono/check_core (`E0610`..`E0612`, `E02xx`, etc.).
            if !vesper_diags.is_empty() {
                rejected += 1;
                if vesper_diags != rust {
                    diffs += 1;
                    if diffs <= 8 {
                        eprintln!("DIFF {name}:\n  rust:   {rust:?}\n  vesper: {vesper_diags:?}");
                    }
                }
            } else {
                // If Vesper says session is open, check Rust did not reject it with E0600..E0607/E0613
                // during collection/session build!
                let rust_session_rejected = rust.iter().any(|d| {
                    matches!(
                        d.code.as_str(),
                        "E0600" | "E0601" | "E0605" | "E0606" | "E0613"
                    ) || (d.code == "E0607"
                        && (d.message.starts_with("BRIDGE0002:")
                            || d.message.starts_with("cannot lower ")
                            || d.message.contains("are not supported")))
                });
                if rust_session_rejected {
                    diffs += 1;
                    if diffs <= 8 {
                        eprintln!("MISSED SESSION ERROR {name}:\n  rust: {rust:?}\n  vesper: []");
                    }
                }
            }
        }
        eprintln!(
            "session_diagnostics: {diffs} of {} cases differ ({rejected} session rejections matched)",
            cases.len()
        );
        assert_eq!(diffs, 0);
        assert!(
            rejected >= 10,
            "expected session rejections, got {rejected}"
        );
    });
}

#[test]
fn session_queries_and_revision_agree_with_rust() {
    with_large_stack(|| {
        let fq = queries_entry();
        let src = r#"
struct CopyBox[T: mod copy] { x: T }
struct Boxed[T] { x: T }
trait Show { fn show(in self) -> i64; }
trait Rank { fn rank(in self) -> i64; }
impl Show for i64 { fn show(in self) -> i64 { 1 } }
impl[T] Show for Boxed[T] where T: Show { fn show(in self) -> i64 { 2 } }
model Fast: Rank for i64 { fn rank(in self) -> i64 { 10 } }
model Slow: Rank for i64 { fn rank(in self) -> i64 { 20 } }
fn helper() -> i64 { 0 }
fn main() -> i64 { 0 }
"#;
        let m = parse(src);
        let ident = vesper_pact_binding::PackageIdentity::new("@test/q", "0.1.0");
        let rust_res = pact_frontend::check_package(&m, &ident);
        assert!(
            rust_res.checked.diagnostics.is_empty(),
            "{:?}",
            rust_res.checked.diagnostics
        );
        let rust_sess = rust_res.session.as_ref().unwrap();
        let conf_paths: Vec<String> = rust_res
            .bindings
            .impls
            .iter()
            .map(|i| format!("impl {ident}::{}", i.key))
            .chain(
                rust_res
                    .bindings
                    .models
                    .iter()
                    .map(|m| format!("model {ident}::{}", m.name)),
            )
            .collect();
        // Match `Frontend::confs` order as Rust iterates `impl` + `model` items in module order:
        let conf_paths_decl_order: Vec<String> = m
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Conformance(c) => match &c.kind {
                    ConformanceKind::Canonical => {
                        let key = match &c.self_ty {
                            Type::Named(n, args) if args.is_empty() => {
                                format!("{}.{n}", c.trait_ref.name)
                            }
                            Type::Named(n, args) => {
                                let segs: Vec<String> =
                                    args.iter().map(|a| format!("{a}")).collect();
                                format!("{}.{n}-0-{}-2", c.trait_ref.name, segs.join("-1-"))
                            }
                            _ => unreachable!(),
                        };
                        Some(format!("impl {ident}::{key}"))
                    }
                    ConformanceKind::Model { name, .. } => Some(format!("model {ident}::{name}")),
                },
                _ => None,
            })
            .collect();
        let _ = conf_paths;

        let g_param = |n: &str| Generic {
            name: n.into(),
            bound: Mode::Linear,
            row: false,
        };
        let t_named = |n: &str| Type::Named(n.into(), vec![]);
        let t_app = |n: &str, a: Type| Type::Named(n.into(), vec![a]);

        let queries = vec![
            // 1. Direct canonical impl: `i64: Show`
            (
                vec![],
                vec![],
                vec![],
                vec![],
                (t_named("i64"), "Show".into(), vec![], None),
            ),
            // 2. Recursive canonical impl with infer var: `Boxed[?X]: Show` -> solves `?X` or picks Boxed? (Wait: Boxed[?X] needs Show on ?X -> Self unknown / NoSolution vs concrete Boxed[i64])
            (
                vec![],
                vec![],
                vec![],
                vec![],
                (t_app("Boxed", t_named("i64")), "Show".into(), vec![], None),
            ),
            // 3. Explicit named model: `i64: Rank` via `Fast`
            (
                vec![],
                vec![],
                vec![],
                vec![],
                (t_named("i64"), "Rank".into(), vec![], Some("Fast".into())),
            ),
            // 4. Scoped model via `use model`: `[Slow]`
            (
                vec![],
                vec![],
                vec![vec!["Slow".into()]],
                vec![],
                (t_named("i64"), "Rank".into(), vec![], None),
            ),
            // 5. Conflicting scoped models: `[Fast, Slow]` -> Ambiguous
            (
                vec![],
                vec![],
                vec![vec!["Fast".into(), "Slow".into()]],
                vec![],
                (t_named("i64"), "Rank".into(), vec![], None),
            ),
            // 6. Generic assumption -> tag 5
            (
                vec![g_param("U")],
                vec![(t_named("U"), "Show".into(), vec![], None)],
                vec![],
                vec![],
                (t_named("U"), "Show".into(), vec![], None),
            ),
            // 7. NoSolution multiline failure: `bool: Show`
            (
                vec![],
                vec![],
                vec![],
                vec![],
                (t_named("bool"), "Show".into(), vec![], None),
            ),
            // 8. Inference variable self-type ambiguity: `?Z: Show`
            (
                vec![],
                vec![],
                vec![],
                vec![("_z".into(), 0u8)],
                (t_named("_z"), "Show".into(), vec![], None),
            ),
        ];

        let (_, sess_bytes) =
            build_session_input_for(&m, "@test/q", "0.1.0", 48, 100_000, 16).unwrap();
        let mut full_input = sess_bytes;
        full_input.extend((queries.len() as u64).to_le_bytes());

        let mut rust_answers = vec![];
        for (env_params, assumptions, scopes, infer, goal) in &queries {
            let sq = pact_words::SessionQuery {
                env_params,
                assumptions,
                scopes,
                infer,
                goal: goal.clone(),
            };
            let qbytes = pact_words::encode_session_query(&sq);
            full_input.extend((qbytes.len() as u64).to_le_bytes());
            full_input.extend(&qbytes);
            while !full_input.len().is_multiple_of(8) {
                full_input.push(0);
            }

            let dto = vesper_pact_binding::QueryBindingDto::new(
                ident.clone(),
                vesper_pact_binding::EnvDto {
                    params: env_params
                        .iter()
                        .map(|g| vesper_pact_binding::ParamDto::ty(g.name.clone(), Some(g.bound)))
                        .collect(),
                    assumptions: assumptions
                        .iter()
                        .map(
                            |(st, tr, args, br)| vesper_pact_binding::GoalDto::Implements {
                                self_ty: vesper_pact_binding::TypeDto::from_source(st).unwrap(),
                                trait_ref: vesper_pact_binding::DefRefDto::local(tr.clone()),
                                args: args
                                    .iter()
                                    .map(|(_, t)| {
                                        vesper_pact_binding::ArgDto::from_source(t).unwrap()
                                    })
                                    .collect(),
                                brand: br.as_ref().map(|m| {
                                    vesper_pact_binding::BrandDto::Model(
                                        vesper_pact_binding::DefRefDto::local(m.clone()),
                                    )
                                }),
                            },
                        )
                        .collect(),
                    use_models: scopes
                        .iter()
                        .map(|sc| {
                            sc.iter()
                                .map(|m| vesper_pact_binding::DefRefDto::local(m.clone()))
                                .collect()
                        })
                        .collect(),
                },
                infer
                    .iter()
                    .map(|(n, _)| vesper_pact_binding::ParamDto::ty(n.clone(), None))
                    .collect(),
                vesper_pact_binding::GoalDto::Implements {
                    self_ty: vesper_pact_binding::TypeDto::from_source(&goal.0).unwrap(),
                    trait_ref: vesper_pact_binding::DefRefDto::local(goal.1.clone()),
                    args: vec![],
                    brand: goal.3.as_ref().map(|m| {
                        vesper_pact_binding::BrandDto::Model(vesper_pact_binding::DefRefDto::local(
                            m.clone(),
                        ))
                    }),
                },
            );
            rust_answers.push(rust_query_solve(rust_sess, &conf_paths_decl_order, &dto));
        }

        let words = fq.words(&full_input);
        let decoded = pact_words::decode_session_queries(&words);
        assert!(decoded.diagnostics.is_empty(), "{:?}", decoded.diagnostics);
        assert!(decoded.ok);
        assert_eq!(decoded.revision.len(), 64);
        assert_eq!(decoded.answers, rust_answers);

        // Revision stability across whitespace/reformat vs sensitivity to signature edit:
        let reformatted = parse(&format!("\n  /* comment */ \n{src}\n"));
        let (_, input_refmt) =
            build_session_input_for(&reformatted, "@test/q", "0.1.0", 48, 100_000, 16).unwrap();
        let mut q_refmt = input_refmt;
        q_refmt.extend(0u64.to_le_bytes());
        let rev_refmt = pact_words::decode_session_queries(&fq.words(&q_refmt)).revision;
        assert_eq!(decoded.revision, rev_refmt);

        let edited = parse(&src.replace("fn helper() -> i64", "fn helper(x: i64) -> i64"));
        let (_, input_edited) =
            build_session_input_for(&edited, "@test/q", "0.1.0", 48, 100_000, 16).unwrap();
        let mut q_edit = input_edited;
        q_edit.extend(0u64.to_le_bytes());
        let rev_edit = pact_words::decode_session_queries(&fq.words(&q_edit)).revision;
        assert_ne!(decoded.revision, rev_edit);
    });
}

#[test]
fn resolve_call_and_normalize_agree_with_rust() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let resolve_fn = package().entry("check_pact_resolve", true);
            let normalize_fn = package().entry("check_pact_normalize", true);

            let src = r#"
                trait Area {
                    fn area(in self) -> i64;
                }
                trait Perim {
                    fn area(in self) -> i64;
                    fn perim(in self) -> i64;
                }
                trait Container {
                    type Item;
                    fn get(in self) -> Self::Item;
                }
                trait OtherContainer {
                    type Item;
                    fn other(in self) -> Self::Item;
                }
                trait Tagged[U] {
                    type Tag;
                    fn tag(in self) -> Self::Tag;
                }
                copy struct Circle { r: i64 }
                copy struct Rect { w: i64, h: i64 }
                copy struct Wrap[T] { val: T }
                impl Area for Circle {
                    fn area(in self) -> i64 { self.r }
                }
                impl Area for Rect {
                    fn area(in self) -> i64 { self.w }
                }
                impl Perim for Rect {
                    fn area(in self) -> i64 { self.w }
                    fn perim(in self) -> i64 { self.h }
                }
                model FastArea: Area for Circle {
                    fn area(in self) -> i64 { self.r }
                }
                impl[T] Container for Wrap[T] {
                    type Item = T;
                    fn get(in self) -> Self::Item { self.val }
                }
                fn helper[T: Area](in x: T) -> i64 { x.area() }
                fn helper_c[T: Container](in x: T) -> i64 { 0 }
                fn helper_dyn(in x: dyn Area) -> i64 { x.area() }
            "#;

            let m = parse(src);
            let identity =
                vesper_pact_binding::PackageIdentity::new("@local/resolve_test", "0.0.0");
            let (prepared, sess_bytes) =
                build_session_input_for(&m, "@local/resolve_test", "0.0.0", 48, 100_000, 16)
                    .unwrap();
            let base = base_world(&prepared);
            let sp = vesper::ast::Span { start: 10, end: 20 };

            let t_circle = Type::Named("Circle".into(), vec![]);
            let t_rect = Type::Named("Rect".into(), vec![]);
            let t_i64 = Type::Named("i64".into(), vec![]);
            let t_bool = Type::Named("bool".into(), vec![]);
            let t_box_area = Type::Named(
                "Box".into(),
                vec![Type::Named(
                    "dyn".into(),
                    vec![Type::Named("Area".into(), vec![])],
                )],
            );
            let t_dyn_gen = Type::Named("__dyn__x".into(), vec![]);
            let t_param = Type::Named("T".into(), vec![]);

            let bounded_gen = vec![Generic {
                name: "T".into(),
                bound: Mode::Linear,
                row: false,
            }];
            let area_bounds = vec![vec![("Area".to_string(), vec![])]];
            let container_bounds = vec![vec![("Container".to_string(), vec![])]];
            let no_bounds: Vec<Vec<(String, Vec<Type>)>> = vec![];
            let dyn_gen = vec![Generic {
                name: "__dyn__x".into(),
                bound: Mode::Linear,
                row: false,
            }];

            type GenBounds = Vec<Vec<(String, Vec<Type>)>>;
            type CallCase<'a> = (&'a str, bool, Type, Vec<Generic>, GenBounds, &'a str);
            let call_cases: Vec<CallCase<'_>> = vec![
                (
                    "area",
                    false,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "Area::area",
                    true,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "FastArea::area",
                    true,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "FastArea::area",
                    true,
                    t_rect.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "Area::missing",
                    true,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "unknown_fn",
                    false,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "area",
                    false,
                    t_rect.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "perim",
                    false,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "eq",
                    false,
                    t_circle.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "area",
                    false,
                    t_box_area.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "perim",
                    false,
                    t_box_area.clone(),
                    vec![],
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "area",
                    false,
                    t_param.clone(),
                    bounded_gen.clone(),
                    area_bounds.clone(),
                    "helper",
                ),
                (
                    "area",
                    false,
                    t_param.clone(),
                    bounded_gen.clone(),
                    no_bounds.clone(),
                    "main",
                ),
                (
                    "area",
                    false,
                    t_dyn_gen.clone(),
                    dyn_gen.clone(),
                    area_bounds.clone(),
                    "helper_dyn",
                ),
            ];

            let call_probes: Vec<pact_frontend::ResolveCallProbe<'_>> = call_cases
                .iter()
                .map(
                    |(callee, path, recv, gens, _, func)| pact_frontend::ResolveCallProbe {
                        callee,
                        path: *path,
                        receiver: recv.clone(),
                        generics: gens.as_slice(),
                        function: func,
                        span: sp.clone(),
                    },
                )
                .collect();

            let proj_wrap_i64 = Type::Named(
                "$projection".into(),
                vec![
                    Type::Named("Wrap".into(), vec![t_i64.clone()]),
                    Type::Named("Container".into(), vec![]),
                    Type::Named("Item".into(), vec![]),
                ],
            );
            let proj_nested = Type::Named(
                "$projection".into(),
                vec![
                    Type::Named("Wrap".into(), vec![proj_wrap_i64.clone()]),
                    Type::Named("Container".into(), vec![]),
                    Type::Named("Item".into(), vec![]),
                ],
            );
            let proj_tuple = Type::Tuple(vec![
                proj_wrap_i64.clone(),
                Type::Named("Wrap".into(), vec![proj_nested.clone()]),
            ]);
            let proj_unknown_trait = Type::Named(
                "$projection".into(),
                vec![
                    t_i64.clone(),
                    Type::Named("NoSuchTrait".into(), vec![]),
                    Type::Named("Item".into(), vec![]),
                ],
            );
            let proj_missing_assoc = Type::Named(
                "$projection".into(),
                vec![
                    t_circle.clone(),
                    Type::Named("Area".into(), vec![]),
                    Type::Named("Nope".into(), vec![]),
                ],
            );
            let proj_ambig_assoc = Type::Named(
                "$projection".into(),
                vec![
                    t_i64.clone(),
                    Type::Named("".into(), vec![]),
                    Type::Named("Item".into(), vec![]),
                ],
            );
            let proj_wrong_args = Type::Named(
                "$projection".into(),
                vec![
                    t_i64.clone(),
                    Type::Named("Tagged".into(), vec![t_i64.clone(), t_bool.clone()]),
                    Type::Named("Tag".into(), vec![]),
                ],
            );
            let proj_bounded = Type::Named(
                "$projection".into(),
                vec![
                    t_param.clone(),
                    Type::Named("Container".into(), vec![]),
                    Type::Named("Item".into(), vec![]),
                ],
            );

            type NormCase<'a> = (Type, Vec<Generic>, GenBounds, &'a str);
            let norm_cases: Vec<NormCase<'_>> = vec![
                (proj_wrap_i64, vec![], no_bounds.clone(), "main"),
                (proj_nested, vec![], no_bounds.clone(), "main"),
                (proj_tuple, vec![], no_bounds.clone(), "main"),
                (proj_unknown_trait, vec![], no_bounds.clone(), "main"),
                (proj_missing_assoc, vec![], no_bounds.clone(), "main"),
                (proj_ambig_assoc, vec![], no_bounds.clone(), "main"),
                (proj_wrong_args, vec![], no_bounds.clone(), "main"),
                (proj_bounded, bounded_gen, container_bounds, "helper_c"),
            ];

            let norm_probes: Vec<pact_frontend::NormalizeProbe<'_>> = norm_cases
                .iter()
                .map(|(ty, gens, _, func)| pact_frontend::NormalizeProbe {
                    ty: ty.clone(),
                    generics: gens.as_slice(),
                    function: func,
                    at: sp.start,
                    span: sp.clone(),
                })
                .collect();

            let probe = pact_frontend::probe_resolve_and_normalize(
                &prepared,
                &base,
                &identity,
                &call_probes,
                &norm_probes,
            )
            .unwrap();
            let rust_calls = probe.calls;
            let rust_norms = probe.norms;

            let mut full_resolve = sess_bytes.clone();
            full_resolve.extend((call_cases.len() as u64).to_le_bytes());
            for (callee, path, recv, gens, gbounds, func) in &call_cases {
                let qbytes = pact_words::encode_resolve_call(&pact_words::ResolveCallPayload {
                    span: &sp,
                    function: func,
                    generics: gens.as_slice(),
                    gen_bounds: gbounds.as_slice(),
                    conf_bounds: &[],
                    scopes: &[],
                    callee,
                    path: *path,
                    receiver: recv,
                })
                .unwrap();
                full_resolve.extend((qbytes.len() as u64).to_le_bytes());
                full_resolve.extend(&qbytes);
                let pad = (8 - (qbytes.len() % 8)) % 8;
                full_resolve.resize(full_resolve.len() + pad, 0);
            }
            let (r_diags, r_ok, v_calls) =
                pact_words::decode_resolve_outcomes(&resolve_fn.words(&full_resolve));
            assert!(r_diags.is_empty(), "{r_diags:?}");
            assert!(r_ok);
            assert_eq!(v_calls.len(), rust_calls.len());
            for (i, (v, r)) in v_calls.iter().zip(&rust_calls).enumerate() {
                let expected = match r {
                    None => pact_words::DecodedResolveOutcome::None,
                    Some(Ok(s)) => pact_words::DecodedResolveOutcome::Ok(s.clone()),
                    Some(Err(d)) => pact_words::DecodedResolveOutcome::Err(vec![d.clone()]),
                };
                assert_eq!(
                    v, &expected,
                    "resolve mismatch on case {i}: {:?}",
                    call_cases[i].0
                );
            }

            let mut full_norm = sess_bytes;
            full_norm.extend((norm_cases.len() as u64).to_le_bytes());
            for (ty, gens, gbounds, func) in &norm_cases {
                let qbytes = pact_words::encode_normalize(&pact_words::NormalizePayload {
                    span: &sp,
                    function: func,
                    generics: gens.as_slice(),
                    gen_bounds: gbounds.as_slice(),
                    conf_bounds: &[],
                    scopes: &[],
                    ty,
                })
                .unwrap();
                full_norm.extend((qbytes.len() as u64).to_le_bytes());
                full_norm.extend(&qbytes);
                let pad = (8 - (qbytes.len() % 8)) % 8;
                full_norm.resize(full_norm.len() + pad, 0);
            }
            let (n_diags, n_ok, v_norms) =
                pact_words::decode_resolve_outcomes(&normalize_fn.words(&full_norm));
            assert!(n_diags.is_empty(), "{n_diags:?}");
            assert!(n_ok);
            assert_eq!(v_norms.len(), rust_norms.len());
            for (i, (v, r)) in v_norms.iter().zip(&rust_norms).enumerate() {
                let expected = match r {
                    Ok(t) => pact_words::DecodedResolveOutcome::Ok(format!("{t}")),
                    Err(d) => pact_words::DecodedResolveOutcome::Err(vec![d.clone()]),
                };
                assert_eq!(v, &expected, "normalize mismatch on case {i}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn builtin_derive_agrees_with_rust() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let entry = package().entry("check_pact_builtin_derive", true);

            let ok_src = r#"
                copy struct Point { x: u64, y: u64 }
                struct Pair[T] { a: T, b: T }
                struct Tagged[T, U] { val: T }
                struct Custom { raw: f64 }
                struct Nested { p: Point, tup: (u8, u32), pair: Pair[bool] }
                impl Eq for Custom { fn eq(in self, in other: Custom) -> bool { true } }
                struct WithCustom { c: Custom }
                impl Eq for Point {}
                impl Ord for Point {}
                impl Hash for Point {}
                impl[T] Eq for Pair[T] {}
                impl[T] Ord for Pair[T] {}
                impl[T] Hash for Pair[T] {}
                impl[T, U] Eq for Tagged[T, U] {}
                impl Eq for Nested {}
                impl Eq for WithCustom {}
            "#;

            let m_ok = parse(ok_src);
            let base_ok = base_world(&m_ok);
            let input_ok = pact_words::encode_builtin_derive(&m_ok, &base_ok).unwrap();
            let (diags_ok, ok_flag, impls_src) =
                pact_words::decode_builtin_derive(&entry.words(&input_ok));
            assert!(diags_ok.is_empty(), "{diags_ok:?}");
            assert!(ok_flag);

            // Parse the Vesper-generated derived `impl`s alongside the stripped
            // user module + builtin prelude items and verify `declaration_pass`
            // accepts the whole derived module cleanly (0 errors).
            let mut combined = String::from(
                "copy enum Ordering { Less, Equal, Greater }\n\
                 trait Eq { fn eq(in self, in other: Self) -> bool; }\n\
                 trait Ord { fn cmp(in self, in other: Self) -> Ordering; }\n\
                 trait Hash { fn hash(in self) -> u64; }\n\
                 fn vesper_builtin_hash_mix(h: u64, v: u64) -> u64 { wrapping_mul(wrapping_add(h, v), 1099511628211) }\n",
            );
            for line in ok_src.lines() {
                if !line.trim_start().starts_with("impl ") && !line.trim_start().starts_with("impl[") {
                    combined.push_str(line);
                    combined.push('\n');
                }
            }
            combined.push_str(
                "impl Eq for Custom { fn eq(in self, in other: Custom) -> bool { true } }\n",
            );
            combined.push_str(&impls_src);
            let parsed_combined = parse(&combined);
            let (_, rust_diags) = pact_frontend::declaration_pass(&parsed_combined).unwrap();
            assert!(
                rust_diags.is_empty(),
                "Vesper-derived impls failed declaration_pass: {rust_diags:?}\n{impls_src}"
            );

            // Rejected derive cases: compare `E0616` span & message 1-for-1 with Rust.
            let err_cases = [
                "impl Eq for i64 {}",
                "struct Pair[A, B] { a: A, b: B }\nimpl[B, A] Eq for Pair[A, B] {}",
                "struct BadFloat { x: f64 }\nimpl Eq for BadFloat {}",
                "struct BadTup[T] { t: (T, u8) }\nimpl[T] Eq for BadTup[T] {}",
                "struct MissingInner { p: Inner }\nstruct Inner { x: u64 }\nimpl Eq for MissingInner {}",
            ];
            for (ci, src) in err_cases.iter().enumerate() {
                let m = parse(src);
                let base = base_world(&m);
                let input = pact_words::encode_builtin_derive(&m, &base).unwrap();
                let (v_diags, v_ok, _) = pact_words::decode_builtin_derive(&entry.words(&input));
                assert!(!v_ok, "expected derive failure on case {ci}");
                let rust_res = pact_frontend::check_package(
                    &m,
                    &vesper_pact_binding::PackageIdentity::new("@local/derive_err", "0.0.0"),
                );
                let rust_e0616: Vec<Diagnostic> = rust_res
                    .checked
                    .diagnostics
                    .into_iter()
                    .filter(|d| d.code == "E0616")
                    .collect();
                assert_eq!(
                    diagnostic_text(&v_diags),
                    diagnostic_text(&rust_e0616),
                    "E0616 mismatch on case {ci}"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
