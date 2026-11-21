//! Self-hosting SH4 lane 10 (#1284), part 1: the control family's plans in
//! `@vesper/codegen-control` against the Rust compiler, over the corpus.
//!
//! - Session plans: `native::session::plan` on every corpus function that
//!   could be a session entry, with bindings synthesized from its row and
//!   the result enums of its operations, plus mutants of those bindings
//!   that make each refusal fire. The Rust side writes `{:?}` of the plan
//!   or the message; the Vesper side writes the same text.
//! - Handler keys: `native_handlers::effect_key` of every effect instance
//!   of every row a world mentions, and `Kind::of` of every type.
//!
//! What the Vesper side cannot compute yet comes in the input: each
//! function's direct callees, which Rust finds by walking checked bodies
//! and the lowered Place IR (`native_session::callees`), copied below.
//! Neither checked bodies nor the closure and handler expressions of Place
//! IR are encoded for Vesper yet (report, gap 1).
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::check::World;
use vesper::native::session::{
    Cancellation, ECANCELED, EffectBinding, Operation, OperationBinding, plan,
};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-control"))
}

// ------------------------------------------------------------------ callees

/// `native::visit`: every expression of `e`, pre-order.
fn visit(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Tuple(es) => es.iter().for_each(|e| visit(e, f)),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, e)| visit(e, f)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                visit(
                    match s {
                        Statement::Let { value, .. } => value,
                        Statement::Expr(e) => e,
                    },
                    f,
                )
            }
            if let Some(e) = tail {
                visit(e, f)
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit(callee, f);
            args.iter().for_each(|a| visit(&a.expr, f));
        }
        ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => visit(e, f),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            visit(a, f);
            visit(b, f)
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit(condition, f);
            visit(then, f);
            if let Some(e) = otherwise {
                visit(e, f)
            }
        }
        ExprKind::Match { value, arms } => {
            visit(value, f);
            arms.iter().for_each(|(_, e)| visit(e, f));
        }
        ExprKind::Closure { body, .. } => visit(body, f),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, e)) = state {
                visit(e, f)
            }
            visit(body, f);
            visit(&ret.body, f);
            ops.iter().for_each(|c| visit(&c.body, f));
        }
        k @ (ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Continue
        | ExprKind::Return(_)) => {
            for c in k.control_children().into_iter().flatten() {
                visit(c, f)
            }
        }
        _ => {}
    }
}

type References = BTreeSet<(usize, usize, String)>;

/// `native_session::function_references`.
fn function_references(world: &World) -> References {
    vesper::ir_swap::lower(world)
        .functions
        .values()
        .flat_map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .filter_map(|i| match &i.op {
                    vesper::ir::Op::Read { root, .. }
                        if world.functions.contains_key(root) && !f.locals.contains_key(root) =>
                    {
                        Some((i.span.start, i.span.end, root.clone()))
                    }
                    _ => None,
                })
        })
        .collect()
}

/// `native_session::callees`.
fn callees(world: &World, references: &References, f: &Function) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(body) = &f.body {
        visit(body, &mut |e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                match &callee.kind {
                    ExprKind::Name(n)
                        if world.functions.contains_key(n)
                            && !world
                                .types
                                .contains_key(&(callee.span.start, callee.span.end)) =>
                    {
                        out.insert(n.clone());
                    }
                    ExprKind::Field(base, method) => {
                        if let Some(t) = world.types.get(&(base.span.start, base.span.end)) {
                            let name = format!("{}::{method}", t.name());
                            if world.functions.contains_key(&name) {
                                out.insert(name);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let ExprKind::Name(n) = &e.kind
                && references.contains(&(e.span.start, e.span.end, n.clone()))
            {
                out.insert(n.clone());
            }
        });
    }
    out
}

// ------------------------------------------------------------------ encoding

fn params(e: &mut Enc, ps: &[Param], out: &mut Vec<u64>) {
    out.push(ps.len() as u64);
    for p in ps {
        out.push(e.name(&p.name));
        out.push(pass_word(p.pass));
        out.push(e.ty(&p.ty));
    }
}

/// The function and effect-declaration sections (tables.ves).
fn tables(e: &mut Enc, w: &World, out: &mut Vec<u64>) {
    let references = function_references(w);
    let index: BTreeMap<&str, u64> = w
        .functions
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i as u64))
        .collect();
    out.push(w.functions.len() as u64);
    for (name, f) in &w.functions {
        out.push(e.name(name));
        out.push(f.body.is_some() as u64);
        out.push(f.generics.len() as u64);
        params(e, &f.params, out);
        out.push(e.ty(&f.result));
        out.push(e.ty(&Type::Row(f.effects.clone())));
        let cs = callees(w, &references, f);
        out.push(cs.len() as u64);
        out.extend(cs.iter().map(|c| index[c.as_str()]));
    }
    out.push(w.effects.len() as u64);
    for (name, d) in &w.effects {
        out.push(e.name(name));
        out.push(d.linear as u64);
        out.push(d.generics.len() as u64);
        out.push(d.ops.len() as u64);
        for op in &d.ops {
            out.push(e.name(&op.name));
            out.push(op.generics.len() as u64);
            params(e, &op.params, out);
            out.push(e.ty(&op.result));
        }
    }
}

fn bindings_words(e: &mut Enc, bindings: &[EffectBinding], out: &mut Vec<u64>) {
    out.push(bindings.len() as u64);
    for b in bindings {
        out.push(e.name(&b.effect));
        out.push(b.ops.len() as u64);
        for o in &b.ops {
            out.push(e.name(&o.op));
            out.push((o.operation == Operation::Read) as u64);
            out.push(e.name(&o.value));
            out.push(e.name(&o.error));
            match &o.cancel {
                Cancellation::Constructor(c) => {
                    out.push(0);
                    out.push(e.name(c));
                }
                Cancellation::ErrorCode(code) => {
                    out.push(1);
                    out.push(u64::from(*code));
                }
            }
        }
    }
}

/// Encodes `w` and a task; `None` when a type cannot be encoded (a literal
/// past 64 bits).
fn encode_world(w: &World, task: impl FnOnce(&mut Enc, &mut Vec<u64>)) -> Option<Vec<u8>> {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode(w, task)));
    std::panic::set_hook(quiet);
    r.ok()
}

// ------------------------------------------------------------------ session cases

/// The enum `ty` names and its variants with their field types.
fn variants(w: &World, ty: &Type) -> Vec<(String, Vec<Type>)> {
    let ty = vesper::numeric::default_type(ty);
    match w.adts.get(ty.name()) {
        Some(a) if a.is_enum => a
            .variants
            .iter()
            .map(|v| {
                (
                    v.name.clone(),
                    v.fields.iter().map(|f| f.ty.clone()).collect(),
                )
            })
            .collect(),
        _ => vec![],
    }
}

fn is_named(t: &Type, names: &[&str]) -> bool {
    matches!(t, Type::Named(n, a) if a.is_empty() && names.contains(&n.as_str()))
}

/// A binding of `sig` that `validate_op` accepts when the declaration
/// allows one: value, error and cancellation constructors picked from
/// its result enum by shape.
fn synth_op(w: &World, sig: &Function) -> OperationBinding {
    let read = sig.params.len() == 1;
    let vs = variants(w, &sig.result);
    let value = vs
        .iter()
        .find(|(_, fs)| {
            if read {
                matches!(fs.as_slice(), [f] if is_named(f, &["u64"]))
            } else {
                fs.is_empty() || matches!(fs.as_slice(), [f] if *f == Type::unit())
            }
        })
        .or(vs.first())
        .map_or("Value".to_string(), |(n, _)| n.clone());
    let error = vs
        .iter()
        .find(|(n, fs)| *n != value && matches!(fs.as_slice(), [f] if is_named(f, &["u32", "u64"])))
        .map_or("Error".to_string(), |(n, _)| n.clone());
    let cancel = vs
        .iter()
        .find(|(n, fs)| *n != value && *n != error && fs.is_empty())
        .map_or(Cancellation::ErrorCode(ECANCELED), |(n, _)| {
            Cancellation::Constructor(n.clone())
        });
    OperationBinding::new(
        &sig.name,
        if read {
            Operation::Read
        } else {
            Operation::Yield
        },
        &value,
        &error,
        cancel,
    )
}

/// One binding per non-`Fail` instance of `f`'s row (each name once).
fn synth(w: &World, f: &Function) -> Vec<EffectBinding> {
    let mut out: Vec<EffectBinding> = vec![];
    for i in &f.effects.instances {
        if i.name == "Fail" || out.iter().any(|b| b.effect == i.name) {
            continue;
        }
        let ops = w
            .effects
            .get(&i.name)
            .map(|d| d.ops.iter().map(|s| synth_op(w, s)).collect())
            .unwrap_or_default();
        out.push(EffectBinding {
            effect: i.name.clone(),
            ops,
        });
    }
    out
}

/// The base bindings and mutants that make the refusals fire.
fn mutants(w: &World, base: &[EffectBinding]) -> Vec<Vec<EffectBinding>> {
    let mut out = vec![base.to_vec()];
    let Some(first) = base.first() else {
        let mut ghost = base.to_vec();
        ghost.push(EffectBinding {
            effect: "Ghost".into(),
            ops: vec![],
        });
        out.push(ghost);
        return out;
    };
    let edit = |f: &dyn Fn(&mut EffectBinding)| {
        let mut m = base.to_vec();
        f(&mut m[0]);
        m
    };
    if let Some(op) = first.ops.first() {
        let adt = w
            .effects
            .get(&first.effect)
            .and_then(|d| d.ops.iter().find(|s| s.name == op.op))
            .map(|s| vesper::numeric::default_type(&s.result).name().to_string())
            .unwrap_or_default();
        let qualified = format!("{adt}::{}", op.value);
        out.push(edit(&|b| b.ops[0].value = qualified.clone()));
        out.push(edit(&|b| {
            b.ops.pop();
        }));
        out.push(edit(&|b| b.ops.push(b.ops[0].clone())));
        out.push(edit(&|b| {
            let o = &mut b.ops[0];
            std::mem::swap(&mut o.value, &mut o.error)
        }));
        out.push(edit(&|b| {
            b.ops[0].cancel = Cancellation::Constructor(b.ops[0].value.clone())
        }));
        out.push(edit(&|b| {
            b.ops[0].cancel = Cancellation::Constructor(b.ops[0].error.clone())
        }));
        out.push(edit(&|b| b.ops[0].error = b.ops[0].value.clone()));
        out.push(edit(&|b| b.ops[0].value = "Missing".into()));
        out.push(edit(&|b| b.ops[0].op = "missing".into()));
        out.push(edit(&|b| {
            b.ops[0].operation = match b.ops[0].operation {
                Operation::Read => Operation::Yield,
                Operation::Yield => Operation::Read,
            }
        }));
        out.push(edit(&|b| {
            for o in &mut b.ops {
                o.cancel = Cancellation::ErrorCode(ECANCELED);
            }
        }));
        // A field-carrying constructor as the cancellation.
        let vs = variants(
            w,
            &w.effects
                .get(&first.effect)
                .and_then(|d| d.ops.iter().find(|s| s.name == op.op))
                .map_or(Type::Infer, |s| s.result.clone()),
        );
        if let Some((n, _)) = vs
            .iter()
            .find(|(n, fs)| !fs.is_empty() && *n != op.value && *n != op.error)
        {
            let n = n.clone();
            out.push(edit(&|b| {
                b.ops[0].cancel = Cancellation::Constructor(n.clone())
            }));
        }
    }
    let mut dup = base.to_vec();
    dup.push(first.clone());
    out.push(dup);
    let mut ghost = base.to_vec();
    ghost.push(EffectBinding {
        effect: "Ghost".into(),
        ops: vec![],
    });
    out.push(ghost);
    out.push(base[1..].to_vec());
    out
}

/// A function shaped like a session entry: its row names a declared
/// linear effect.
fn session_shaped(w: &World, f: &Function) -> bool {
    f.effects
        .instances
        .iter()
        .any(|i| w.effects.get(&i.name).is_some_and(|d| d.linear))
}

/// The session cases of a world: every session-shaped function with its
/// mutants, and `main` and the first two other functions with their base
/// bindings (the early refusals).
fn session_cases(w: &World) -> Vec<(String, Vec<EffectBinding>)> {
    let mut out = vec![];
    let mut others = 0;
    for (name, f) in &w.functions {
        let base = synth(w, f);
        if session_shaped(w, f) {
            for m in mutants(w, &base) {
                out.push((name.clone(), m));
            }
        } else if name == "main" || others < 2 {
            if name != "main" {
                others += 1;
            }
            out.push((name.clone(), base));
        }
    }
    out.push(("no_such_entry".into(), vec![]));
    out
}

fn rust_plans(w: &World, cases: &[(String, Vec<EffectBinding>)]) -> Option<String> {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut out = String::new();
        for (entry, bindings) in cases {
            match plan(w, entry, bindings) {
                Ok(p) => out.push_str(&format!("{p:?}\n")),
                Err(e) => out.push_str(&format!("err {e}\n")),
            }
        }
        out
    }));
    std::panic::set_hook(quiet);
    r.ok()
}

fn session_input(w: &World, cases: &[(String, Vec<EffectBinding>)]) -> Option<Vec<u8>> {
    encode_world(w, |e, out| {
        tables(e, w, out);
        out.push(cases.len() as u64);
        for (entry, bindings) in cases {
            out.push(e.name(entry));
            bindings_words(e, bindings, out);
        }
    })
}

/// Refusals of `plan` the corpus cases must make fire (a fragment of each
/// message). `handwritten_refusals_match_rust` covers the read-buffer and
/// constructor-shape refusals; "has no body" and "appears twice in the
/// entry row" are not reached (the checker merges a repeated row instance,
/// `twice` below; no case names a function without a body).
const REFUSALS: &[&str] = &[
    "unknown session entry",
    "session entries take no parameters",
    "session entries cannot be generic",
    "cannot return closures or continuations",
    "recursive suspending call cycle",
    "is bound twice",
    "has no native binding",
    "must be declared linear",
    "has no operation",
    "is bound to Yield but takes",
    "is bound to Read",
    "must be an enum with value, error and cancellation constructors",
    "value and error constructors must differ",
    "has no value constructor",
    "value constructor '",
    "must differ from the value and error constructors",
    "which the entry does not declare",
];

#[test]
fn session_plans_match_rust() {
    let entry = package().entry("control_session_plans", false);
    let (mut bad, mut worlds, mut lines, mut plans, mut skipped) = (0, 0, 0, 0, 0);
    let mut fired = [false; REFUSALS.len()];
    for c in corpus() {
        let w = &c.world;
        let cases = session_cases(w);
        let Some(rust) = rust_plans(w, &cases) else {
            skipped += 1;
            continue;
        };
        let Some(input) = session_input(w, &cases) else {
            skipped += 1;
            continue;
        };
        worlds += 1;
        lines += cases.len();
        plans += rust.lines().filter(|l| l.starts_with("Plan ")).count();
        for (i, r) in REFUSALS.iter().enumerate() {
            if rust.contains(r) {
                fired[i] = true;
            }
        }
        let vesper = entry.text(&input);
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 5 {
                eprintln!("session plans: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "session plans: {bad} of {worlds} worlds differ ({lines} cases, {plans} plans, {skipped} skipped)"
    );
    assert!(worlds > 50, "only {worlds} worlds");
    assert!(plans > 0, "no case produced a plan");
    let missing: Vec<_> = REFUSALS
        .iter()
        .zip(fired)
        .filter(|(_, f)| !f)
        .map(|(r, _)| *r)
        .collect();
    eprintln!("session plans: refusals that never fired: {missing:?}");
    assert!(missing.is_empty(), "the cases miss refusals {missing:?}");
    assert_eq!(bad, 0);
}

/// The native-pulse examples' own bindings (tests/source_native_suspension.rs)
/// produce plans on both sides.
#[test]
fn native_pulse_examples_plan() {
    let entry = package().entry("control_session_plans", false);
    let pulse = vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![
            OperationBinding::new(
                "pause",
                Operation::Yield,
                "Ticked",
                "TickFailed",
                Cancellation::Constructor("TickCancelled".into()),
            ),
            OperationBinding::new(
                "read",
                Operation::Read,
                "Got",
                "IoFailed",
                Cancellation::Constructor("IoCancelled".into()),
            ),
        ],
    }];
    let source = vec![EffectBinding {
        effect: "Source".into(),
        ops: vec![OperationBinding::new(
            "read",
            Operation::Read,
            "Got",
            "IoFailed",
            Cancellation::ErrorCode(ECANCELED),
        )],
    }];
    let mut checked = 0;
    for c in corpus() {
        if !c.name.starts_with("examples/native-pulse/") {
            continue;
        }
        let cases = vec![
            ("main".to_string(), pulse.clone()),
            ("main".to_string(), source.clone()),
        ];
        let rust = rust_plans(&c.world, &cases).expect("plan panicked");
        let vesper = entry.text(&session_input(&c.world, &cases).unwrap());
        assert_eq!(rust, vesper, "{}", c.name);
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} native-pulse examples");
}

// ------------------------------------------------------------------ handler keys

fn rows(w: &World) -> Vec<Effects> {
    fn walk(t: &Type, out: &mut Vec<Effects>) {
        match t {
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                out.push(effects.clone());
                params.iter().for_each(|(_, t)| walk(t, out));
                walk(result, out);
            }
            Type::Named(_, a) | Type::Tuple(a) => a.iter().for_each(|t| walk(t, out)),
            Type::Row(e) => out.push(e.clone()),
            _ => {}
        }
    }
    let mut out: Vec<Effects> = w.functions.values().map(|f| f.effects.clone()).collect();
    for t in world_types(w) {
        walk(&t, &mut out);
    }
    out
}

#[test]
fn handler_keys_match_rust() {
    let entry = package().entry("control_handler_keys", false);
    let bad = differential(
        "handler keys",
        |c| {
            let mut out = String::new();
            for r in rows(&c.world) {
                out.push_str("R\n");
                for e in &r.instances {
                    out.push_str(&format!("K {}\n", vesper::native_handlers::effect_key(e)));
                }
            }
            for t in world_types(&c.world) {
                out.push_str(&format!("T {:?}\n", vesper::native_handlers::Kind::of(&t)));
            }
            if encode_world(&c.world, |_, _| {}).is_none() {
                return String::new();
            }
            out
        },
        |c| {
            let w = &c.world;
            let rs = rows(w);
            let ts = world_types(w);
            let Some(input) = encode_world(w, |e, out| {
                out.push(rs.len() as u64);
                for r in &rs {
                    out.push(e.ty(&Type::Row(r.clone())));
                }
                out.push(ts.len() as u64);
                for t in &ts {
                    out.push(e.ty(t));
                }
            }) else {
                return String::new();
            };
            entry.text(&input)
        },
    );
    assert_eq!(bad, 0);
}

/// Refusals the corpus never reaches: a mixed read buffer, missing and
/// mis-shaped constructors.
const REFUSING: &str = r#"
copy struct Word {
    v: u64
}
copy struct Mixed {
    v: u64,
    flag: bool
}
copy struct Pair {
    a: Word,
    b: Mixed
}
copy enum Io {
    Got(u64),
    IoFailed(u64),
    IoCancelled,
    Odd(bool),
    Extra(u64)
}
linear effect Source {
    fn read(inout buf: Word) -> Io;
    fn mixed(inout buf: Pair) -> Io;
}
fn main() -> u64 eff [Source] {
    let mut w = Word {
        v: 0
    };
    let a = Source::read(inout w);
    0
}
fn twice() -> u64 eff [Source, Source] {
    0
}
"#;

#[test]
fn handwritten_refusals_match_rust() {
    let entry = package().entry("control_session_plans", false);
    let parsed = vesper::parser::parse(REFUSING);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let w = &checked.world;
    let op = |op: &str, read: bool, value: &str, error: &str, cancel: Cancellation| {
        OperationBinding::new(
            op,
            if read {
                Operation::Read
            } else {
                Operation::Yield
            },
            value,
            error,
            cancel,
        )
    };
    let ok_read = op(
        "read",
        true,
        "Got",
        "IoFailed",
        Cancellation::Constructor("IoCancelled".into()),
    );
    let ok_mixed = op("mixed", true, "Got", "IoFailed", Cancellation::ErrorCode(9));
    let with = |first: OperationBinding| {
        vec![EffectBinding {
            effect: "Source".into(),
            ops: vec![first, ok_mixed.clone()],
        }]
    };
    let cancel = |c: &str| Cancellation::Constructor(c.into());
    let cases = vec![
        (
            "main".to_string(),
            with(op("read", true, "Got", "Nope", cancel("IoCancelled"))),
        ),
        (
            "main".to_string(),
            with(op("read", true, "Got", "IoFailed", cancel("Nope"))),
        ),
        (
            "main".to_string(),
            with(op("read", true, "Got", "Odd", cancel("IoCancelled"))),
        ),
        (
            "main".to_string(),
            with(op("read", true, "Got", "IoFailed", cancel("Extra"))),
        ),
        (
            "main".to_string(),
            with(op("read", true, "Got", "IoCancelled", cancel("Extra"))),
        ),
        ("main".to_string(), with(ok_read.clone())),
        (
            "main".to_string(),
            vec![EffectBinding {
                effect: "Source".into(),
                ops: vec![ok_read.clone()],
            }],
        ),
        ("twice".to_string(), with(ok_read.clone())),
        ("Io::Got".to_string(), vec![]),
    ];
    let rust = rust_plans(w, &cases).expect("plan panicked");
    eprintln!("{rust}");
    let vesper = entry.text(&session_input(w, &cases).unwrap());
    assert_eq!(rust, vesper);
    for r in [
        "read buffer",
        "has no error constructor",
        "has no cancellation constructor",
        "error constructor '",
        "must not carry fields",
    ] {
        assert!(rust.contains(r), "no case refused with {r:?}");
    }
}
