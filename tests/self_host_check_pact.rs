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

use check_harness::*;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use vesper::ast::{
    AssocTypeBinding, ConformanceKind, Diagnostic, Effect, Function, Generic, Item, Mode, ModelUse,
    Module, Pass, TraitBoundSyntax, TraitRef, Type, projection_type,
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

fn span(t: &mut Vec<u64>, s: &std::ops::Range<usize>) {
    t.extend([s.start as u64, s.end as u64]);
}

fn generics(e: &mut Enc, t: &mut Vec<u64>, gs: &[Generic]) {
    t.push(gs.len() as u64);
    for g in gs {
        t.push(e.name(&g.name));
        t.push(mode_word(g.bound));
        t.push(g.row as u64);
    }
}

fn trait_ref(e: &mut Enc, t: &mut Vec<u64>, r: &TraitRef) {
    t.push(e.opt_name(&r.package));
    t.push(e.name(&r.name));
    t.push(r.args.len() as u64);
    for a in &r.args {
        t.push(e.ty(a));
    }
    span(t, &r.span);
}

/// A signature; effect operations and trait methods have no body, and a
/// conformance method's body is not read by the pass.
fn signature(e: &mut Enc, t: &mut Vec<u64>, f: &Function) {
    t.push(e.name(&f.name));
    span(t, &f.span);
    generics(e, t, &f.generics);
    t.push(f.params.len() as u64);
    for p in &f.params {
        t.push(e.name(&p.name));
        t.push(pass_word(p.pass));
        t.push(p.projections.len() as u64);
        for list in &p.projections {
            t.push(list.len() as u64);
            for n in list {
                t.push(e.name(n));
            }
        }
        t.push(e.ty(&p.ty));
        span(t, &p.span);
    }
    t.push(e.ty(&f.result));
    t.push(e.ty(&Type::Row(f.effects.clone())));
}

/// The declaration section (`read_decls` in decl.ves).
fn encode_declarations(e: &mut Enc, t: &mut Vec<u64>, m: &Module, base: &World) {
    t.push(e.name(IDENTITY));
    t.push(base.functions.len() as u64);
    for (name, f) in &base.functions {
        t.push(e.name(name));
        generics(e, t, &f.generics);
    }
    let traits: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Trait(t) => Some(t),
            _ => None,
        })
        .collect();
    t.push(traits.len() as u64);
    for tr in traits {
        t.push(e.name(&tr.name));
        span(t, &tr.span);
        generics(e, t, &tr.generics);
        t.push(tr.assoc.len() as u64);
        for a in &tr.assoc {
            t.push(e.name(&a.name));
            t.push(mode_word(a.bound));
            span(t, &a.span);
        }
        t.push(tr.methods.len() as u64);
        for f in &tr.methods {
            signature(e, t, f);
        }
    }
    let confs: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Conformance(c) => Some(c),
            _ => None,
        })
        .collect();
    t.push(confs.len() as u64);
    for c in confs {
        match &c.kind {
            ConformanceKind::Canonical => t.extend([0, e.name("")]),
            ConformanceKind::Model { name, .. } => t.extend([1, e.name(name)]),
        }
        generics(e, t, &c.generics);
        trait_ref(e, t, &c.trait_ref);
        t.push(e.ty(&c.self_ty));
        t.push(c.where_clauses.len() as u64);
        for w in &c.where_clauses {
            t.push(e.ty(&w.ty));
            trait_ref(e, t, &w.trait_ref);
        }
        t.push(c.assoc.len() as u64);
        for a in &c.assoc {
            t.push(e.name(&a.name));
            t.push(e.ty(&a.ty));
            span(t, &a.span);
        }
        t.push(c.methods.len() as u64);
        for f in &c.methods {
            signature(e, t, f);
        }
        span(t, &c.span);
    }
    t.push(m.model_uses.len() as u64);
    for u in &m.model_uses {
        t.push(e.name(&u.model));
        span(t, &u.span);
    }
    t.push(m.trait_bounds.len() as u64);
    for b in &m.trait_bounds {
        t.push(e.name(&b.param));
        trait_ref(e, t, &b.trait_ref);
        span(t, &b.span);
        t.push(e.name(&b.owner));
    }
    let items: Vec<_> = m
        .items
        .iter()
        .filter(|i| matches!(i, Item::Adt(_) | Item::Effect(_)))
        .collect();
    t.push(items.len() as u64);
    for i in items {
        match i {
            Item::Adt(a) => {
                t.push(1);
                t.push(e.name(&a.name));
                span(t, &a.span);
                let fields: Vec<_> = a.variants.iter().flat_map(|v| &v.fields).collect();
                t.push(fields.len() as u64);
                for f in fields {
                    t.push(e.ty(&f.ty));
                }
            }
            Item::Effect(d) => {
                t.push(0);
                t.push(e.name(&d.name));
                span(t, &d.span);
                t.push(d.ops.len() as u64);
                for f in &d.ops {
                    assert!(f.body.is_none(), "effect operation with a body");
                    signature(e, t, f);
                }
            }
            _ => unreachable!(),
        }
    }
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
    let input = encode(&base, |e, t| encode_declarations(e, t, &prepared, &base));
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
    // The corpus worlds the pass runs on and their inputs.
    let worlds: Vec<(Module, Vec<u8>)> = corpus()
        .iter()
        .filter_map(|c| task(&c.module).map(|(input, _)| (c.module.clone(), input)))
        .collect();
    let mut mutated = 0;
    for (k, (m, _)) in worlds.iter().enumerate() {
        mutated += mutants(m, k).len();
    }
    eprintln!(
        "{} corpus worlds run the pass ({mutated} mutants)",
        worlds.len()
    );
    // Rust: `declaration_pass` (the lowerings before the pass, collecting,
    // validating and building the Pact binding DTOs). Vesper: decoding the
    // input and the pass (validation and the lowering checks, no DTOs).
    // Encoding is not timed.
    abba(
        "declarations",
        &mut || {
            time(|| {
                for (m, _) in &worlds {
                    std::hint::black_box(pact_frontend::declaration_pass(m));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, input) in &worlds {
                    std::hint::black_box(f.words(input));
                }
            })
        },
    );
    let t = time(|| {
        for (m, _) in &worlds {
            std::hint::black_box(task(m));
        }
    });
    eprintln!("encoding (with the Rust pass it runs): {t:?}");
}
