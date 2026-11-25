//! Self-hosting SH4 lane 10 (#1284), part 1: the control family's plans in
//! `@vesper/codegen-control` against the Rust compiler, over the corpus.
//!
//! - Session plans: `native::session::plan` on every corpus function that
//!   could be a session entry, with bindings synthesized from its row and
//!   the result enums of its operations, plus mutants of those bindings
//!   that make each refusal fire. The Rust side writes `{:?}` of the plan
//!   or the message; the Vesper side writes the same text.
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
/// constructor-shape refusals and a closure result (which only crates/
/// reaches, and Bazel's corpus has no crates/); "has no body" and "appears twice in the
/// entry row" are not reached (the checker merges a repeated row instance,
/// `twice` below; no case names a function without a body).
const REFUSALS: &[&str] = &[
    "unknown session entry",
    "session entries take no parameters",
    "session entries cannot be generic",
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
fn maker() -> fn(u64) -> u64 eff [Source] {
    |x: u64| x
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
        ("maker".to_string(), with(ok_read.clone())),
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
        "cannot return closures or continuations",
    ] {
        assert!(rust.contains(r), "no case refused with {r:?}");
    }
}

// ------------------------------------------------------------------ freshness

mod freshness {
    use super::*;
    use cranelift_codegen::ir::{
        self, AbiParam, ExtFuncData, ExternalName, InstBuilder, InstructionData, MemFlags, Opcode,
        Signature, UserExternalName, UserFuncName, ValueDef, types,
    };
    use cranelift_codegen::isa::CallConv;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use std::collections::HashMap;
    use vesper::native::session::{
        FreshnessInput, capture_freshness_inputs, check_data_base_freshness, object,
    };
    use vesper_fiber_abi::abi::offsets;

    fn list(out: &mut Vec<u64>, v: impl IntoIterator<Item = u64>) {
        let v: Vec<u64> = v.into_iter().collect();
        out.push(v.len() as u64);
        out.extend(v);
    }

    /// One function as freshness.ves reads it: what Cranelift computes
    /// (aliases, value definitions, memory and `park` flags, the load
    /// offsets) and the layout.
    fn words(func: &ir::Function, ctx: ir::Value, park: ir::FuncRef) -> Vec<u64> {
        let dfg = &func.dfg;
        let mut w = vec![ctx.as_u32() as u64];
        let nv = dfg.num_values();
        w.push(nv as u64);
        for i in 0..nv {
            let v = ir::Value::from_u32(i as u32);
            let (def, k) = match dfg.value_def(v) {
                ValueDef::Result(inst, k) => (inst.as_u32() as u64 + 1, k as u64),
                _ => (0, 0),
            };
            w.extend([dfg.resolve_aliases(v).as_u32() as u64, def, k]);
        }
        w.push(dfg.num_insts() as u64);
        let blocks: Vec<ir::Block> = func.layout.blocks().collect();
        let pos: HashMap<ir::Block, u64> = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (*b, i as u64))
            .collect();
        w.push(blocks.len() as u64);
        for b in &blocks {
            list(
                &mut w,
                dfg.block_params(*b).iter().map(|v| v.as_u32() as u64),
            );
            let insts: Vec<ir::Inst> = func.layout.block_insts(*b).collect();
            w.push(insts.len() as u64);
            for inst in insts {
                let data = &dfg.insts[inst];
                let op = data.opcode();
                let memory = matches!(
                    op,
                    Opcode::Load | Opcode::Store | Opcode::Call | Opcode::CallIndirect
                ) || op.can_load()
                    || op.can_store();
                let is_park =
                    matches!(data, InstructionData::Call { func_ref, .. } if *func_ref == park);
                let (load, arg, offset) = match *data {
                    InstructionData::Load {
                        opcode: Opcode::Load,
                        arg,
                        offset,
                        ..
                    } => (
                        true,
                        arg.as_u32() as u64,
                        i64::from(i32::from(offset)) as u64,
                    ),
                    _ => (false, 0, 0),
                };
                let flags = u64::from(memory) | u64::from(is_park) << 1 | u64::from(load) << 2;
                w.extend([inst.as_u32() as u64, flags, arg, offset]);
                list(
                    &mut w,
                    dfg.inst_args(inst).iter().map(|v| v.as_u32() as u64),
                );
                list(
                    &mut w,
                    dfg.inst_results(inst).iter().map(|v| v.as_u32() as u64),
                );
                list(&mut w, dfg.inst_values(inst).map(|v| v.as_u32() as u64));
                let dests = data.branch_destination(&dfg.jump_tables, &dfg.exception_tables);
                w.push(dests.len() as u64);
                for call in dests {
                    w.push(pos[&call.block(&dfg.value_lists)]);
                    let args: Vec<ir::BlockArg> = call.args(&dfg.value_lists).collect();
                    w.push(args.len() as u64);
                    for a in args {
                        match a.as_value() {
                            Some(v) => w.extend([0, v.as_u32() as u64]),
                            None => w.extend([1, 0]),
                        }
                    }
                }
            }
        }
        w
    }

    fn input(funcs: &[FreshnessInput]) -> Vec<u8> {
        let mut out = vec![funcs.len() as u64];
        for (f, ctx, park) in funcs {
            let w = words(f, *ctx, *park);
            out.push(w.len() as u64);
            out.extend(w);
        }
        out.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn rust(funcs: &[FreshnessInput]) -> String {
        funcs
            .iter()
            .map(
                |(f, ctx, park)| match check_data_base_freshness(f, *ctx, *park) {
                    Ok(()) => "ok\n".to_owned(),
                    Err(e) => format!("err {e}\n"),
                },
            )
            .collect()
    }

    /// `fn(ctx)` that loads a base, parks, then reads through `after`: the
    /// old base, a reload from `ctx`, or the park's signal. `through` sends
    /// the derived address through a block parameter; `again` reloads the
    /// base in a loop that parks.
    fn hand(after: &str, through: bool, again: bool) -> FreshnessInput {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let mut func = ir::Function::with_name_signature(UserFuncName::user(0, 0), sig);
        let mut fctx = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let mut park_sig = Signature::new(CallConv::SystemV);
        park_sig.params.push(AbiParam::new(types::I64));
        park_sig.returns.push(AbiParam::new(types::I64));
        let park_sig = b.import_signature(park_sig);
        let name = b
            .func
            .declare_imported_user_function(UserExternalName::new(0, 1));
        let park = b.import_function(ExtFuncData {
            name: ExternalName::user(name),
            signature: park_sig,
            colocated: false,
        });
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ctx = b.block_params(entry)[0];
        let top = if again {
            let top = b.create_block();
            b.ins().jump(top, &[]);
            b.switch_to_block(top);
            Some(top)
        } else {
            None
        };
        let old = b
            .ins()
            .load(types::I64, MemFlags::trusted(), ctx, offsets::CTX_DATA_BASE);
        let field = b.ins().iadd_imm(old, 8);
        let early = b.ins().load(types::I64, MemFlags::trusted(), field, 0);
        let field = if through {
            let next = b.create_block();
            let p = b.append_block_param(next, types::I64);
            b.ins().jump(next, &[field.into()]);
            b.switch_to_block(next);
            p
        } else {
            field
        };
        let call = b.ins().call(park, &[ctx]);
        let signal = b.inst_results(call)[0];
        let address = match after {
            "stale" => field,
            "ctx" => {
                let fresh =
                    b.ins()
                        .load(types::I64, MemFlags::trusted(), ctx, offsets::CTX_DATA_BASE);
                b.ins().iadd_imm(fresh, 8)
            }
            "signal" => {
                let fresh = b.ins().load(
                    types::I64,
                    MemFlags::trusted(),
                    signal,
                    offsets::RESUME_DATA_BASE,
                );
                b.ins().iadd_imm(fresh, 8)
            }
            _ => unreachable!(),
        };
        let late = b.ins().load(types::I64, MemFlags::trusted(), address, 0);
        let sum = b.ins().iadd(early, late);
        if let Some(top) = top {
            let done = b.create_block();
            b.ins().brif(sum, top, &[], done, &[]);
            b.switch_to_block(done);
        }
        b.ins().return_(&[sum]);
        b.seal_all_blocks();
        b.finalize();
        let ctx = func.dfg.block_params(func.layout.entry_block().unwrap())[0];
        (func, ctx, park)
    }

    /// Every session entry the corpus plans, compiled to an object with the
    /// Rust check on, and the hand-built functions.
    fn cases() -> Vec<FreshnessInput> {
        let mut out = Vec::new();
        for after in ["stale", "ctx", "signal"] {
            for through in [false, true] {
                for again in [false, true] {
                    out.push(hand(after, through, again));
                }
            }
        }
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for c in corpus() {
            let w = &c.world;
            for (entry, bindings) in session_cases(w) {
                let planned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    plan(w, &entry, &bindings).is_ok()
                }));
                if !matches!(planned, Ok(true)) {
                    continue;
                }
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    capture_freshness_inputs(|| object(w, &entry, &bindings, None)).1
                }));
                if let Ok(funcs) = r {
                    out.extend(funcs);
                }
            }
        }
        std::panic::set_hook(quiet);
        out
    }

    #[test]
    fn freshness_matches_rust() {
        let entry = package().entry("control_session_freshness", true);
        let funcs = cases();
        let rust = rust(&funcs);
        let vesper = entry.text(&input(&funcs));
        let refused = rust.lines().filter(|l| l.starts_with("err ")).count();
        let mut bad = 0;
        for (i, (r, v)) in rust.lines().zip(vesper.lines()).enumerate() {
            if r != v {
                bad += 1;
                if bad <= 5 {
                    eprintln!("freshness: function {i}:\n  rust:   {r}\n  vesper: {v}");
                }
            }
        }
        assert_eq!(rust.lines().count(), vesper.lines().count());
        eprintln!(
            "freshness: {} functions ({} sessions), {refused} refused; {bad} differ",
            funcs.len(),
            funcs.len() - 12
        );
        assert!(funcs.len() > 12, "no corpus session compiled");
        assert!(refused >= 4, "only {refused} refusals");
        assert_eq!(bad, 0);
    }

    /// What the check costs, Rust against Vesper (the Vesper side also
    /// decodes its input).
    #[test]
    #[ignore = "timing; run with CHECK_HARNESS_SPEED=1 and --ignored"]
    fn freshness_cost() {
        let entry = package().entry("control_session_freshness", true);
        let funcs = cases();
        let bytes = input(&funcs);
        let (r, v) = abba(
            "session freshness",
            &mut || {
                time(|| {
                    std::hint::black_box(rust(&funcs));
                })
            },
            &mut || {
                time(|| {
                    std::hint::black_box(entry.text(&bytes));
                })
            },
        );
        eprintln!(
            "rust {r:?} vesper {v:?} ({:.2}x)",
            v.as_secs_f64() / r.as_secs_f64()
        );
    }
}
