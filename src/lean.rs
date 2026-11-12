//! Lean extraction through structured syntax. Mutable parameters become explicit
//! returned state; unhandled effects become monadic evidence, not axioms.
use crate::lean_nodes::{
    arith_term, effectful_arrow, global, ident, in_range, literal, source_name, source_ref, ty,
    ty_monad,
};
use crate::{ast::*, check::World};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};
use vesper_lean_syntax::{
    Argument, BinaryOp, Binder, BinderKind, Constructor, Declaration, Definition, DefinitionKind,
    DoStatement as DS, Field, Ident, Instance, LeanOption, LetBinding, MatchArm, Module,
    ModuleName, Name, OptionValue, Pattern as LP, Projection, Sort, Term, TypeDeclaration, UnaryOp,
};

type Statements = Vec<DS>;
fn fixed(s: &str) -> Ident {
    ident(s).expect("fixed compiler identifier")
}
fn reference(id: &Ident) -> Term {
    Term::Ref(Name::qualified([id.clone()]).expect("one component"))
}
fn named(s: &str) -> Term {
    source_ref(s).expect("fixed compiler reference")
}
fn app(function: Term, args: impl IntoIterator<Item = Term>) -> Term {
    Term::app(function, args)
}
fn named_app(function: Term, named: Vec<(&str, Term)>, positional: Vec<Term>) -> Term {
    Term::App {
        function: Box::new(function),
        arguments: named
            .into_iter()
            .map(|(name, value)| Argument::Named {
                name: fixed(name),
                value,
            })
            .chain(positional.into_iter().map(Argument::Positional))
            .collect(),
    }
}
fn arrow(a: Term, b: Term) -> Term {
    Term::Arrow(Box::new(a), Box::new(b))
}
fn arrows(args: Vec<Term>, result: Term) -> Term {
    args.into_iter()
        .rev()
        .fold(result, |result, arg| arrow(arg, result))
}
fn type0() -> Term {
    Term::Sort(Sort::Type(0))
}
fn monad_ty() -> Term {
    arrow(type0(), type0())
}
fn explicit(name: Ident, ty: Term) -> Binder {
    Binder::explicit(name, ty)
}
fn implicit(name: &str, ty: Term) -> Binder {
    Binder {
        name: Some(fixed(name)),
        ty: Box::new(ty),
        kind: BinderKind::Implicit,
    }
}
fn instance(name: Option<Ident>, ty: Term) -> Binder {
    Binder {
        name,
        ty: Box::new(ty),
        kind: BinderKind::Instance,
    }
}
fn inferred(names: Vec<Ident>, body: Term) -> Term {
    lambda(
        names
            .into_iter()
            .map(|n| explicit(n, Term::Infer))
            .collect(),
        body,
    )
}
fn lambda(binders: Vec<Binder>, body: Term) -> Term {
    Term::Lambda {
        binders,
        body: Box::new(body),
    }
}
fn ascribe(value: Term, ty: Term) -> Term {
    Term::Ascribe {
        value: Box::new(value),
        ty: Box::new(ty),
    }
}
fn pure(value: Term) -> Term {
    app(global(&["Pure", "pure"]), [value])
}
fn unit() -> Term {
    crate::lean_nodes::unit()
}
fn sequence(statements: Statements, result: Term) -> Term {
    Term::Do {
        statements,
        result: Box::new(result),
    }
}
fn let_local(out: &mut Statements, name: Ident, value: Term, ty: Option<Term>, mutable: bool) {
    out.push(DS::Let {
        binding: LetBinding {
            pattern: LP::Bind(name),
            ty: ty.map(Box::new),
            value,
        },
        mutable,
    });
}
fn bind_local(out: &mut Statements, name: Ident, action: Term, ty: Option<Term>) {
    out.push(match ty {
        Some(ty) => DS::BindTyped {
            pattern: LP::Bind(name),
            ty,
            action,
        },
        None => DS::Bind {
            pattern: LP::Bind(name),
            action,
        },
    });
}
fn project(base: Term, field: &str) -> Result<Term, String> {
    Ok(Term::Project {
        base: Box::new(base),
        projection: Projection::Field(ident(field)?),
    })
}
fn pair_get(base: Term, index: u32) -> Term {
    Term::Project {
        base: Box::new(base),
        projection: Projection::Tuple(NonZeroU32::new(index).expect("positive projection")),
    }
}
fn tuple_type(ts: &[Term]) -> Term {
    crate::lean_nodes::product(ts.to_vec())
}
fn tuple_term(ts: &[Term]) -> Term {
    crate::lean_nodes::tuple(ts.to_vec())
}
fn tuple_get(t: Term, i: usize, n: usize) -> Term {
    if n <= 1 {
        return t;
    }
    let mut t = t;
    for _ in 0..i {
        t = pair_get(t, 2);
    }
    if i + 1 < n {
        t = pair_get(t, 1);
    }
    t
}
fn ctor_pattern(name: Name, arguments: Vec<LP>) -> LP {
    LP::Constructor { name, arguments }
}
fn fixed_name(parts: &[&str]) -> Name {
    Name::qualified(parts.iter().map(|s| fixed(s))).expect("nonempty fixed name")
}
fn effect_name(effect: &str) -> Result<Name, String> {
    source_name(&format!("Effect_{effect}"))
}
fn effect_ref(effect: &str) -> Result<Term, String> {
    Ok(Term::Ref(effect_name(effect)?))
}
fn effect_operation(effect: &str, op: &str) -> Result<Term, String> {
    Ok(Term::Ref(
        Name::qualified([ident(&format!("Effect_{effect}"))?, ident(op)?])
            .map_err(|e| e.to_string())?,
    ))
}
fn fail_action(value: Term) -> Term {
    let impossible = fixed("$impossible");
    sequence(
        vec![DS::Bind {
            pattern: LP::Bind(impossible.clone()),
            action: app(
                effect_operation("Fail", "raise").expect("fixed effect"),
                [value],
            ),
        }],
        Term::Eliminate {
            value: Box::new(reference(&impossible)),
        },
    )
}
fn overflow_action() -> Term {
    fail_action(Term::Ref(fixed_name(&["ArithOverflow", "mk"])))
}
fn runtime_failure(variant: &str) -> Term {
    fail_action(Term::Ref(fixed_name(&["RuntimeError", variant])))
}
fn known_type(t: &Type) -> bool {
    match t {
        Type::Infer => false,
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().all(known_type),
        Type::Function { params, result, .. } => {
            params.iter().all(|(_, t)| known_type(t)) && known_type(result)
        }
        _ => true,
    }
}

fn prelude() -> Result<Vec<Declaration>, String> {
    let (s, o, m, a) = (named("S"), named("O"), named("m"), named("A"));
    let cps = |args: Vec<Term>| app(named("HandlerCPS"), args);
    let monad = app(global(&["Monad"]), [m.clone()]);
    let mut declarations = vec![Declaration::Definition(Definition {
        name: source_name("HandlerCPS")?,
        binders: vec![
            explicit(fixed("S"), type0()),
            explicit(fixed("O"), type0()),
            explicit(fixed("m"), monad_ty()),
            explicit(fixed("A"), type0()),
        ],
        result: type0(),
        body: arrow(
            arrows(vec![a.clone(), s.clone()], app(m.clone(), [o.clone()])),
            arrow(s.clone(), app(m.clone(), [o.clone()])),
        ),
        kind: DefinitionKind::Abbrev,
    })];
    let pure_body = inferred(
        vec![fixed("a"), fixed("k"), fixed("s")],
        app(named("k"), [named("a"), named("s")]),
    );
    let bind_body = inferred(
        vec![fixed("action"), fixed("next"), fixed("k"), fixed("s")],
        app(
            named("action"),
            [
                inferred(
                    vec![fixed("a"), fixed("s'")],
                    app(named("next"), [named("a"), named("k"), named("s'")]),
                ),
                named("s"),
            ],
        ),
    );
    declarations.push(Declaration::Instance(Instance {
        name: None,
        binders: vec![
            implicit("S", type0()),
            implicit("O", type0()),
            implicit("m", monad_ty()),
            instance(None, monad.clone()),
        ],
        ty: app(
            global(&["Monad"]),
            [cps(vec![s.clone(), o.clone(), m.clone()])],
        ),
        body: Term::Record {
            fields: vec![(fixed("pure"), pure_body), (fixed("bind"), bind_body)],
        },
    }));
    declarations.push(Declaration::Definition(Definition {
        name: source_name("liftAction")?,
        binders: vec![
            implicit("S", type0()),
            implicit("O", type0()),
            implicit("m", monad_ty()),
            implicit("A", type0()),
            instance(None, monad),
            explicit(fixed("action"), app(m.clone(), [a.clone()])),
        ],
        result: cps(vec![s, o, m, a]),
        body: inferred(
            vec![fixed("k"), fixed("s")],
            sequence(
                vec![DS::Bind {
                    pattern: LP::Bind(fixed("value")),
                    action: named("action"),
                }],
                app(named("k"), [named("value"), named("s")]),
            ),
        ),
        kind: DefinitionKind::Def,
    }));
    declarations.push(Declaration::Definition(Definition {
        name: source_name("$arithmetic_semantics")?,
        binders: vec![],
        result: global(&["String"]),
        body: Term::String(crate::numeric::ARITHMETIC_SEMANTICS.into()),
        kind: DefinitionKind::Def,
    }));
    declarations.push(Declaration::Definition(Definition {
        name: source_name("inI64")?,
        binders: vec![explicit(fixed("x"), global(&["Int"]))],
        result: global(&["Bool"]),
        body: app(
            global(&["Decidable", "decide"]),
            [Term::binary(
                BinaryOp::And,
                Term::binary(
                    BinaryOp::Le,
                    literal(&Literal::Int(i64::MIN.into())),
                    named("x"),
                ),
                Term::binary(
                    BinaryOp::Le,
                    named("x"),
                    literal(&Literal::Int(i64::MAX.into())),
                ),
            )],
        ),
        kind: DefinitionKind::Def,
    }));
    Ok(declarations)
}
fn adt_groups(world: &World) -> Vec<Vec<&Adt>> {
    fn dependencies<'a>(t: &'a Type, names: &mut Vec<&'a str>) {
        match t {
            Type::Named(n, ts) => {
                names.push(n);
                for t in ts {
                    dependencies(t, names);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    dependencies(t, names);
                }
            }
            Type::Function { params, result, .. } => {
                for (_, t) in params {
                    dependencies(t, names);
                }
                dependencies(result, names);
            }
            _ => {}
        }
    }
    let mut graph = petgraph::graphmap::DiGraphMap::<&str, ()>::new();
    for a in world
        .adts
        .values()
        .filter(|a| a.name != "Option" && a.name != "Result")
    {
        graph.add_node(&a.name);
    }
    for a in world.adts.values() {
        if !graph.contains_node(a.name.as_str()) {
            continue;
        }
        for field in a.variants.iter().flat_map(|v| &v.fields) {
            let mut names = vec![];
            dependencies(&field.ty, &mut names);
            for n in names {
                if graph.contains_node(n) {
                    graph.add_edge(a.name.as_str(), n, ());
                }
            }
        }
    }
    petgraph::algo::tarjan_scc(&graph)
        .into_iter()
        .map(|names| names.into_iter().map(|n| &world.adts[n]).collect())
        .collect()
}

pub fn extract(world: &World) -> Result<String, String> {
    extract_module(world)?.render().map_err(|e| e.to_string())
}
/// Construct the entire module without rendering or reparsing any Lean text.
pub fn extract_module(world: &World) -> Result<Module, String> {
    let mut declarations = prelude()?;
    for group in adt_groups(world) {
        let mut types = vec![];
        for a in group {
            let parameters = a
                .generics
                .iter()
                .filter(|g| !g.row)
                .map(|g| Ok(explicit(ident(&g.name)?, type0())))
                .collect::<Result<Vec<_>, String>>()?;
            types.push(if a.is_enum {
                TypeDeclaration::Inductive {
                    name: source_name(&a.name)?,
                    parameters,
                    sort: type0(),
                    constructors: a
                        .variants
                        .iter()
                        .map(|v| {
                            Ok(Constructor {
                                name: ident(&v.name)?,
                                binders: v
                                    .fields
                                    .iter()
                                    .map(|f| {
                                        Ok(explicit(ident(&format!("f{}", f.name))?, ty(&f.ty)?))
                                    })
                                    .collect::<Result<Vec<_>, String>>()?,
                                result: None,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    deriving: vec![],
                }
            } else {
                TypeDeclaration::Structure {
                    name: source_name(&a.name)?,
                    parameters,
                    fields: a.variants[0]
                        .fields
                        .iter()
                        .map(|f| {
                            Ok(Field {
                                name: ident(&f.name)?,
                                ty: ty(&f.ty)?,
                                default: None,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    deriving: vec![],
                }
            });
        }
        declarations.push(Declaration::MutualTypes(types));
    }
    for effect in world.effects.values() {
        let mut parameters = effect
            .generics
            .iter()
            .map(|g| Ok(explicit(ident(&g.name)?, type0())))
            .collect::<Result<Vec<_>, String>>()?;
        parameters.push(explicit(fixed("m"), monad_ty()));
        let fields = effect
            .ops
            .iter()
            .map(|op| {
                let mut results = vec![ty(&op.result)?];
                results.extend(
                    op.params
                        .iter()
                        .filter(|p| p.pass == Pass::Inout)
                        .map(|p| ty(&p.ty))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                Ok(Field {
                    name: ident(&op.name)?,
                    ty: arrows(
                        op.params
                            .iter()
                            .map(|p| ty(&p.ty))
                            .collect::<Result<Vec<_>, _>>()?,
                        app(named("m"), [tuple_type(&results)]),
                    ),
                    default: None,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        declarations.push(Declaration::Type(TypeDeclaration::Class {
            name: effect_name(&effect.name)?,
            parameters,
            fields,
        }));
    }
    for effect in world.effects.values() {
        let generic_binders = effect
            .generics
            .iter()
            .map(|g| {
                Ok(Binder {
                    name: Some(ident(&g.name)?),
                    ty: Box::new(type0()),
                    kind: BinderKind::Implicit,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let args = effect
            .generics
            .iter()
            .map(|g| source_ref(&g.name))
            .collect::<Result<Vec<_>, _>>()?;
        let source = app(
            effect_ref(&effect.name)?,
            args.iter().cloned().chain([named("m")]),
        );
        let child = app(named("HandlerCPS"), [named("S"), named("O"), named("m")]);
        let destination = app(
            effect_ref(&effect.name)?,
            args.iter().cloned().chain([child]),
        );
        let mut binders = vec![
            implicit("S", type0()),
            implicit("O", type0()),
            implicit("m", monad_ty()),
            instance(None, app(global(&["Monad"]), [named("m")])),
        ];
        binders.extend(generic_binders.clone());
        binders.push(explicit(fixed("evidence"), source.clone()));
        let fields = effect
            .ops
            .iter()
            .map(|op| {
                let ps = op
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, _)| ident(&format!("$arg{i}")))
                    .collect::<Result<Vec<_>, _>>()?;
                let action = named_app(
                    effect_operation(&effect.name, &op.name)?,
                    vec![("self", named("evidence"))],
                    ps.iter().map(reference).collect(),
                );
                let mut parameters = ps;
                parameters.extend([fixed("k"), fixed("s")]);
                Ok((
                    ident(&op.name)?,
                    inferred(
                        parameters,
                        sequence(
                            vec![DS::Bind {
                                pattern: LP::Bind(fixed("result")),
                                action,
                            }],
                            app(named("k"), [named("result"), named("s")]),
                        ),
                    ),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        declarations.push(Declaration::Definition(Definition {
            name: source_name(&format!("lift_{}", effect.name))?,
            binders,
            result: destination.clone(),
            body: Term::Record { fields },
            kind: DefinitionKind::Def,
        }));
        let mut binders = vec![
            implicit("S", type0()),
            implicit("O", type0()),
            implicit("m", monad_ty()),
            instance(None, app(global(&["Monad"]), [named("m")])),
        ];
        binders.extend(generic_binders);
        binders.push(instance(Some(fixed("evidence")), source));
        declarations.push(Declaration::Instance(Instance {
            name: None,
            binders,
            ty: destination,
            body: app(
                source_ref(&format!("lift_{}", effect.name))?,
                [named("evidence")],
            ),
        }));
    }
    let program = crate::ir::lower_world(world);
    let mut functions = vec![];
    for f in world.functions.values() {
        if let Some(nodes) =
            crate::lean_ir::extract_function_nodes(world, &program.functions[&f.name])
        {
            for node in nodes? {
                match node {
                    Declaration::Definition(definition) => functions.push(definition),
                    _ => {
                        return Err("Lean CFG extractor returned a non-function declaration".into());
                    }
                }
            }
            continue;
        }
        let polymorphic = effectful_function(f)
            || effectful_arrow(&f.result)
            || f.params.iter().any(|p| effectful_arrow(&p.ty));
        let mut generator = Generator::new(
            world,
            if polymorphic {
                named("m")
            } else {
                global(&["Id"])
            },
        );
        functions.push(generator.function(f)?);
    }
    if !functions.is_empty() {
        declarations.push(Declaration::MutualDefinitions(functions));
    }
    Ok(Module {
        imports: vec![ModuleName::new("Std").map_err(|e| e.to_string())?],
        declarations: vec![
            Declaration::SetOption {
                option: LeanOption::LinterUnusedVariables,
                value: OptionValue::Bool(false),
            },
            Declaration::Namespace {
                name: source_name("Vesper")?,
                declarations,
            },
        ],
    })
}

fn function_requirements(f: &Function) -> Vec<Effect> {
    fn visit(t: &Type, effects: &mut Vec<Effect>) {
        match t {
            Type::Function {
                result,
                effects: row,
                ..
            } => {
                for e in &row.instances {
                    if !effects.contains(e) {
                        effects.push(e.clone());
                    }
                }
                visit(result, effects);
            }
            Type::Named(_, ts) | Type::Tuple(ts) => {
                for t in ts {
                    visit(t, effects);
                }
            }
            _ => (),
        }
    }
    let mut effects = f.effects.instances.clone();
    visit(&f.result, &mut effects);
    effects
}

#[derive(Clone)]
/// A source binding with a stable identity across nested mutable copies.
struct Local {
    id: usize,
    ty: Type,
    depth: usize,
}
#[derive(Clone)]
/// One lexical CPS handler. Its frame holds the outer places that the handled
/// computation can write, including values observed by aborts and resumptions.
struct Layer {
    state: Term,
    output: Term,
    base: Term,
    child: Term,
    effect: Effect,
    frame: Vec<usize>,
    frame_ty: Term,
    ops: BTreeMap<String, OpInfo>,
    closures: usize,
}
#[derive(Clone)]
/// Operation implementation plus the borrowed arguments requiring frame lenses.
struct OpInfo {
    implementation: Ident,
    aliased: Vec<usize>,
    post_access: bool,
}
#[derive(Clone)]
struct CapBinding {
    effect: Effect,
    term: Term,
    depth: usize,
}
#[derive(Clone)]
/// A borrowed clause parameter and its optional lens into the performer's frame.
struct Alias {
    lens: Ident,
    raw: Ident,
    ty: Type,
    pass: Pass,
    bindings: Vec<(usize, Vec<String>)>,
}
#[derive(Clone)]
/// Direct resumptions thread borrowed arguments and reload the resumed frame.
/// A first-class fallback is available only when no frame or aliases would drop.
struct Resumption {
    resume: Ident,
    state: Ident,
    stateful: bool,
    frame: Ident,
    context: usize,
    depth: usize,
    aliases: Vec<Alias>,
    fallback: Option<Ident>,
}
struct Generator<'a> {
    world: &'a World,
    counter: usize,
    monad: Term,
    caps: BTreeMap<String, CapBinding>,
    layers: Vec<Layer>,
    scope: BTreeMap<String, Local>,
    current: BTreeMap<usize, Ident>,
    used: BTreeSet<Ident>,
    next_binding: usize,
    continuations: BTreeMap<String, Resumption>,
    context: usize,
    next_context: usize,
    body_layers: Vec<usize>,
    closures: usize,
    clause_aliases: BTreeSet<usize>,
    alias_written: bool,
    unlabeled: BTreeMap<String, CapBinding>,
}
struct Saved {
    scope: BTreeMap<String, Local>,
    current: BTreeMap<usize, Ident>,
    caps: BTreeMap<String, CapBinding>,
    continuations: BTreeMap<String, Resumption>,
    context: usize,
    body_layers: Vec<usize>,
    closures: usize,
    clause_aliases: BTreeSet<usize>,
    alias_written: bool,
    unlabeled: BTreeMap<String, CapBinding>,
    monad: Term,
}

fn effectful_type(t: &Type) -> bool {
    matches!(t, Type::Function { effects, .. } if !effects.instances.is_empty() || effects.tail.is_some())
}
fn effectful_function(f: &Function) -> bool {
    !f.effects.instances.is_empty() || f.effects.tail.is_some()
}
fn visit_expr<'e>(e: &'e Expr, f: &mut dyn FnMut(&'e Expr)) {
    f(e);
    let mut go = |e: &'e Expr| visit_expr(e, f);
    match &e.kind {
        ExprKind::Tuple(es) => es.iter().for_each(go),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, e)| go(e)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => go(value),
                    Statement::Expr(e) => go(e),
                }
            }
            if let Some(t) = tail {
                go(t);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            go(callee);
            args.iter().for_each(|a| go(&a.expr));
        }
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => go(e),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            go(a);
            go(b);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            go(condition);
            go(then);
            if let Some(o) = otherwise {
                go(o);
            }
        }
        ExprKind::Match { value, arms } => {
            go(value);
            arms.iter().for_each(|(_, e)| go(e));
        }
        ExprKind::Closure { body, .. } => go(body),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                go(s);
            }
            go(body);
            go(&ret.body);
            ops.iter().for_each(|c| go(&c.body));
        }
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
    }
}
/// Names written by an expression through assignment or mutable arguments,
/// excluding closure bodies and handler clauses, which observe snapshots.
fn written_names(world: &World, e: &Expr, out: &mut BTreeSet<String>) {
    match &e.kind {
        ExprKind::Closure { .. } => {}
        ExprKind::Handle { state, body, .. } => {
            if let Some((_, s)) = state {
                written_names(world, s, out);
            }
            written_names(world, body, out);
        }
        ExprKind::Assign(_, place, value) => {
            if let Some(p) = crate::check::Place::from_expr(place) {
                out.insert(p.root);
            }
            written_names(world, value, out);
        }
        ExprKind::Call { callee, args, .. } => {
            // Argument modes may be omitted, so resolve the callee's declared
            // passes: functions, effect operations, then checked arrow types.
            let field_passes = |op: &str| {
                // The checker does not record a callable type for method
                // sugar. Include matching methods, omitting their receiver,
                // as well as labeled effect operations. Different selectors
                // may have different arities, so retain every argument index.
                // This conservative union only enlarges the saved frame.
                let mut passes = None::<Vec<Pass>>;
                let selectors = world
                    .effects
                    .values()
                    .flat_map(|e| &e.ops)
                    .filter(|f| f.name == op)
                    .map(|f| f.params.iter().map(|p| p.pass).collect::<Vec<_>>())
                    .chain(
                        world
                            .functions
                            .iter()
                            .filter(|(n, _)| n.rsplit_once("::").is_some_and(|(_, m)| m == op))
                            .map(|(_, f)| f.params.iter().skip(1).map(|p| p.pass).collect()),
                    );
                for selector in selectors {
                    let merged = passes.get_or_insert_with(Vec::new);
                    merged.resize(merged.len().max(selector.len()), Pass::In);
                    for (index, pass) in selector.into_iter().enumerate() {
                        if pass == Pass::Inout {
                            merged[index] = pass;
                        }
                    }
                }
                passes
            };
            let arrow = world
                .types
                .get(&(callee.span.start, callee.span.end))
                .and_then(|t| match t {
                    Type::Function { params, .. } => {
                        Some(params.iter().map(|(p, _)| *p).collect::<Vec<_>>())
                    }
                    _ => None,
                });
            let params = match &callee.kind {
                ExprKind::Name(n) => world
                    .functions
                    .get(n)
                    .map(|f| f.params.iter().map(|p| p.pass).collect::<Vec<_>>())
                    .or_else(|| {
                        n.split_once("::")
                            .and_then(|(e, op)| {
                                world.effects.get(e)?.ops.iter().find(|f| f.name == op)
                            })
                            .map(|f| f.params.iter().map(|p| p.pass).collect())
                    })
                    .or(arrow),
                ExprKind::Field(_, op) => arrow.or_else(|| field_passes(op)),
                _ => arrow,
            };
            for (index, a) in args.iter().enumerate() {
                let inout = a.pass == Pass::Inout
                    || params
                        .as_ref()
                        .and_then(|ps| ps.get(index))
                        .is_some_and(|p| *p == Pass::Inout);
                if inout && let Some(p) = crate::check::Place::from_expr(&a.expr) {
                    out.insert(p.root);
                }
                written_names(world, &a.expr, out);
            }
            if let ExprKind::Field(base, _) = &callee.kind {
                // Method receivers are rewritten to the method's first parameter.
                if let Some(p) = crate::check::Place::from_expr(base) {
                    out.insert(p.root);
                }
                written_names(world, base, out);
            } else {
                written_names(world, callee, out);
            }
        }
        _ => {
            let mut children = vec![];
            match &e.kind {
                ExprKind::Tuple(es) => children.extend(es),
                ExprKind::Struct { fields, .. } => children.extend(fields.iter().map(|(_, e)| e)),
                ExprKind::Block { statements, tail } => {
                    for s in statements {
                        match s {
                            Statement::Let { value, .. } => children.push(value),
                            Statement::Expr(e) => children.push(e),
                        }
                    }
                    children.extend(tail.as_deref());
                }
                ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => {
                    children.push(e)
                }
                ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => {
                    children.extend([a.as_ref(), b.as_ref()])
                }
                ExprKind::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    children.extend([condition.as_ref(), then.as_ref()]);
                    children.extend(otherwise.as_deref());
                }
                ExprKind::Match { value, arms } => {
                    children.push(value);
                    children.extend(arms.iter().map(|(_, e)| e));
                }
                _ => {}
            }
            for c in children {
                written_names(world, c, out);
            }
        }
    }
}
/// Whether a clause touches a borrowed parameter after it may have resumed.
/// Source order conservatively approximates evaluation order.
fn post_resume_access(body: &Expr, resume: &str, aliases: &BTreeSet<String>) -> bool {
    let mut first = None::<usize>;
    visit_expr(body, &mut |e| {
        if matches!(&e.kind, ExprKind::Name(n) if n == resume) {
            first = Some(first.map_or(e.span.end, |f| f.min(e.span.end)));
        }
    });
    let Some(first) = first else {
        return false;
    };
    let mut found = false;
    visit_expr(body, &mut |e| {
        if let ExprKind::Name(n) = &e.kind
            && aliases.contains(n)
            && e.span.start >= first
        {
            found = true;
        }
    });
    found
}

impl Generator<'_> {
    fn new(world: &World, monad: Term) -> Generator<'_> {
        Generator {
            world,
            counter: 0,
            monad,
            caps: BTreeMap::new(),
            layers: vec![],
            scope: BTreeMap::new(),
            current: BTreeMap::new(),
            used: BTreeSet::new(),
            next_binding: 0,
            continuations: BTreeMap::new(),
            context: 0,
            next_context: 1,
            body_layers: vec![],
            closures: 0,
            clause_aliases: BTreeSet::new(),
            alias_written: false,
            unlabeled: BTreeMap::new(),
        }
    }
    fn save(&self) -> Saved {
        Saved {
            scope: self.scope.clone(),
            current: self.current.clone(),
            caps: self.caps.clone(),
            continuations: self.continuations.clone(),
            context: self.context,
            body_layers: self.body_layers.clone(),
            closures: self.closures,
            clause_aliases: self.clause_aliases.clone(),
            alias_written: self.alias_written,
            unlabeled: self.unlabeled.clone(),
            monad: self.monad.clone(),
        }
    }
    fn restore(&mut self, s: Saved) {
        self.scope = s.scope;
        self.current = s.current;
        self.caps = s.caps;
        self.continuations = s.continuations;
        self.context = s.context;
        self.body_layers = s.body_layers;
        self.closures = s.closures;
        self.clause_aliases = s.clause_aliases;
        self.alias_written = s.alias_written;
        self.unlabeled = s.unlabeled;
        self.monad = s.monad;
    }
    fn new_context(&mut self) {
        self.context = self.next_context;
        self.next_context += 1;
    }
    fn fresh(&mut self, base: &str) -> Result<Ident, String> {
        let mut candidate = ident(base)?;
        let mut index = 1;
        while self.used.contains(&candidate) {
            candidate = ident(&format!("{base}'{index}"))?;
            index += 1;
        }
        self.used.insert(candidate.clone());
        Ok(candidate)
    }
    fn temp(&mut self) -> Ident {
        self.counter += 1;
        self.fresh(&format!("$t{}", self.counter))
            .expect("generated temporary identifier")
    }
    fn declare(&mut self, n: &str, ty: Type) -> Result<Ident, String> {
        let lean = self.fresh(n)?;
        self.declare_as(n, ty, lean.clone());
        Ok(lean)
    }
    fn declare_as(&mut self, n: &str, ty: Type, lean: Ident) -> usize {
        let id = self.next_binding;
        self.next_binding += 1;
        self.used.insert(lean.clone());
        self.scope.insert(
            n.into(),
            Local {
                id,
                ty,
                depth: self.layers.len(),
            },
        );
        self.current.insert(id, lean);
        id
    }
    fn copy_binding(&mut self, id: usize, out: &mut Statements) -> Result<Ident, String> {
        let old = reference(&self.current[&id]);
        let base = self
            .scope
            .iter()
            .find(|(_, l)| l.id == id)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| format!("$b{id}"));
        let new = self.fresh(&base)?;
        let_local(out, new.clone(), old, None, true);
        self.current.insert(id, new.clone());
        Ok(new)
    }
    fn var(&self, n: &str) -> Option<Term> {
        self.scope.get(n).map(|l| reference(&self.current[&l.id]))
    }
    fn ty(&self, t: &Type) -> Result<Term, String> {
        ty_monad(t, &self.monad)
    }
    fn type_of(&self, e: &Expr) -> Option<Type> {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .map(crate::numeric::default_type)
    }
    fn lift_action(&self, mut action: Term, depth: usize) -> Term {
        for l in self.layers.iter().skip(depth) {
            action = named_app(
                named("liftAction"),
                vec![("S", l.state.clone()), ("O", l.output.clone())],
                vec![action],
            );
        }
        action
    }
    fn adapt_callable(&self, value: Term, t: &Type, depth: usize) -> Result<Term, String> {
        if depth >= self.layers.len() {
            return Ok(value);
        }
        let Type::Function { params, .. } = t else {
            return Ok(value);
        };
        if !effectful_type(t) {
            return Ok(value);
        }
        let args = if params.is_empty() {
            vec![fixed("$unit")]
        } else {
            (0..params.len())
                .map(|i| ident(&format!("$arg{i}")))
                .collect::<Result<Vec<_>, _>>()?
        };
        let call = app(value, args.iter().map(reference));
        Ok(inferred(args, self.lift_action(call, depth)))
    }
    fn evidence(&self, cap: &CapBinding) -> Result<Term, String> {
        let mut term = cap.term.clone();
        for l in self.layers.iter().skip(cap.depth) {
            term = named_app(
                source_ref(&format!("lift_{}", cap.effect.name))?,
                vec![
                    ("S", l.state.clone()),
                    ("O", l.output.clone()),
                    ("m", l.base.clone()),
                ],
                vec![term],
            );
        }
        Ok(term)
    }
    fn action(&mut self, action: Term, out: &mut Statements) -> Result<Term, String> {
        self.action_with(action, true, out)
    }
    fn guard_abort(&self, may_abort: bool) -> Result<(), String> {
        if may_abort && !self.clause_aliases.is_empty() && self.alias_written {
            return Err("Lean extraction cannot order a write to a borrowed operation argument before a potentially aborting effect inside its clause".into());
        }
        Ok(())
    }
    fn action_with(
        &mut self,
        action: Term,
        may_abort: bool,
        out: &mut Statements,
    ) -> Result<Term, String> {
        self.sync(out);
        self.guard_abort(may_abort)?;
        let t = self.temp();
        bind_local(out, t.clone(), action, None);
        Ok(reference(&t))
    }
    fn sync(&self, out: &mut Statements) {
        for &index in &self.body_layers {
            let layer = &self.layers[index];
            if layer.frame.is_empty() {
                continue;
            }
            let values = layer
                .frame
                .iter()
                .map(|id| reference(&self.current[id]))
                .collect::<Vec<_>>();
            let action = ascribe(
                inferred(
                    vec![fixed("$k"), fixed("$s")],
                    app(
                        named("$k"),
                        [
                            unit(),
                            tuple_term(&[pair_get(named("$s"), 1), tuple_term(&values)]),
                        ],
                    ),
                ),
                app(layer.child.clone(), [global(&["Unit"])]),
            );
            out.push(DS::Bind {
                pattern: LP::Wildcard,
                action: self.lift_action(action, index + 1),
            });
        }
    }
    fn field_type(&self, t: &Type, field: &str) -> Option<Type> {
        match t {
            Type::Tuple(ts) => field.parse::<usize>().ok().and_then(|i| ts.get(i).cloned()),
            Type::Named(n, args) => {
                let a = self.world.adts.get(n)?;
                if a.is_enum {
                    return None;
                }
                let vars = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                a.variants[0]
                    .fields
                    .iter()
                    .find(|f| f.name == field)
                    .map(|f| substitute(&f.ty, &vars))
            }
            _ => None,
        }
    }
    fn get_path(&self, base: Term, ty: &Type, path: &[String]) -> Result<Term, String> {
        let mut term = base;
        let mut current = Some(ty.clone());
        for f in path {
            if let Some(Type::Tuple(ts)) = &current
                && let Ok(i) = f.parse::<usize>()
            {
                term = tuple_get(term, i, ts.len());
            } else {
                term = project(term, f)?;
            }
            current = current.and_then(|t| self.field_type(&t, f));
        }
        Ok(term)
    }
    fn set_path(
        &self,
        base: Term,
        ty: &Type,
        path: &[String],
        value: Term,
    ) -> Result<Term, String> {
        let Some(f) = path.first() else {
            return Ok(value);
        };
        let inner_ty = self.field_type(ty, f).unwrap_or(Type::Infer);
        let inner = self.get_path(base.clone(), ty, std::slice::from_ref(f))?;
        let updated = self.set_path(inner, &inner_ty, &path[1..], value)?;
        if let Type::Tuple(ts) = ty
            && let Ok(i) = f.parse::<usize>()
        {
            return Ok(tuple_term(
                &(0..ts.len())
                    .map(|j| {
                        if j == i {
                            updated.clone()
                        } else {
                            tuple_get(base.clone(), j, ts.len())
                        }
                    })
                    .collect::<Vec<_>>(),
            ));
        }
        Ok(Term::Update {
            base: Box::new(base),
            fields: vec![(ident(f)?, updated)],
        })
    }
    fn assign(
        &mut self,
        place: &crate::check::Place,
        value: Term,
        out: &mut Statements,
    ) -> Result<(), String> {
        let local = self
            .scope
            .get(&place.root)
            .cloned()
            .ok_or("assignment to an unknown Lean local")?;
        if self.clause_aliases.contains(&local.id) {
            self.alias_written = true;
        }
        let lean = self.current[&local.id].clone();
        let value = self.set_path(reference(&lean), &local.ty, &place.path, value)?;
        out.push(DS::Assign { local: lean, value });
        Ok(())
    }
    fn place_term(&self, place: &crate::check::Place) -> Result<Term, String> {
        let local = self.scope.get(&place.root).ok_or("unknown Lean place")?;
        self.get_path(reference(&self.current[&local.id]), &local.ty, &place.path)
    }
    fn restore_states(
        &mut self,
        result: Term,
        states: &[(&Param, &Expr)],
        vars: &BTreeMap<String, Type>,
        out: &mut Statements,
    ) -> Result<Term, String> {
        let n = states.len() + 1;
        for (index, (param, arg)) in states.iter().enumerate() {
            let place = crate::check::Place::from_expr(arg)
                .ok_or("a mutable Lean argument requires a place")?;
            let ty = substitute(&param.ty, vars);
            let value = tuple_get(result.clone(), index + 1, n);
            let paths = if param.projections.is_empty() {
                vec![vec![]]
            } else {
                param.projections.clone()
            };
            for path in paths {
                let mut target = place.clone();
                target.path.extend(path.iter().cloned());
                let projected = self.get_path(value.clone(), &ty, &path)?;
                self.assign(&target, projected, out)?;
            }
        }
        Ok(tuple_get(result, 0, n))
    }
    fn operands(&mut self, es: &[&Expr], out: &mut Statements) -> Result<Vec<Term>, String> {
        let mut parts = vec![];
        for e in es {
            let mut buffer = vec![];
            let term = self.expr(e, &mut buffer)?;
            parts.push((buffer, term, matches!(e.kind, ExprKind::Literal(_))));
        }
        let mut terms = vec![];
        for i in 0..parts.len() {
            out.extend(parts[i].0.clone());
            let later = parts[i + 1..].iter().any(|(b, _, _)| !b.is_empty());
            if later && !parts[i].2 {
                let t = self.temp();
                let_local(out, t.clone(), parts[i].1.clone(), None, false);
                terms.push(reference(&t));
            } else {
                terms.push(parts[i].1.clone());
            }
        }
        Ok(terms)
    }
    fn seq(&mut self, e: &Expr) -> Result<Term, String> {
        let saved_scope = self.scope.clone();
        let saved_current = self.current.clone();
        let mut out = vec![];
        let term = self.expr(e, &mut out)?;
        self.scope = saved_scope;
        self.current = saved_current;
        Ok(sequence(out, pure(term)))
    }

    fn pattern_types(&self, p: &Pattern, t: &Type, out: &mut Vec<(String, Type)>) {
        match &p.kind {
            PatternKind::Bind(n) => out.push((n.clone(), t.clone())),
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = t {
                    for (p, t) in ps.iter().zip(ts) {
                        self.pattern_types(p, t, out);
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let args = if let Type::Named(_, ts) = t {
                        ts.clone()
                    } else {
                        vec![]
                    };
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t))
                        .collect();
                    for (n, p) in fields {
                        if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                            self.pattern_types(p, &substitute(&f.ty, &vars), out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    /// Paths of the bindings in an irrefutable borrowed-argument pattern.
    fn pattern_paths(
        &self,
        p: &Pattern,
        t: &Type,
        path: Vec<String>,
        out: &mut Vec<(String, Vec<String>, Type)>,
    ) -> Result<(), String> {
        match &p.kind {
            PatternKind::Bind(n) => out.push((n.clone(), path, t.clone())),
            PatternKind::Wild => {}
            PatternKind::Tuple(ps) => {
                let Type::Tuple(ts) = t else {
                    return Err("tuple pattern lacks a tuple type".into());
                };
                for (i, (p, t)) in ps.iter().zip(ts).enumerate() {
                    let mut next = path.clone();
                    next.push(i.to_string());
                    self.pattern_paths(p, t, next, out)?;
                }
            }
            PatternKind::Constructor {
                name: n, fields, ..
            } => {
                let (a, _) = self.world.constructor(n).ok_or("unknown pattern")?;
                if a.is_enum {
                    return Err("Lean extraction requires irrefutable patterns for borrowed operation arguments".into());
                }
                for (f, p) in fields {
                    let ft = self.field_type(t, f).ok_or("unknown pattern field")?;
                    let mut next = path.clone();
                    next.push(f.clone());
                    self.pattern_paths(p, &ft, next, out)?;
                }
            }
            PatternKind::Literal(_) => {
                return Err("Lean extraction requires irrefutable patterns for borrowed operation arguments".into());
            }
        }
        Ok(())
    }
    fn bind_pattern(
        &mut self,
        p: &Pattern,
        t: &Type,
        value: Term,
        out: &mut Statements,
    ) -> Result<(), String> {
        if let PatternKind::Bind(n) = &p.kind {
            let lean = self.declare(n, t.clone())?;
            let ann = if known_type(t) { self.ty(t).ok() } else { None };
            let_local(out, lean, value, ann, true);
            return Ok(());
        }
        let mut renames = BTreeMap::new();
        let pattern = self.pat(p, &mut renames)?;
        out.push(DS::Let {
            binding: LetBinding {
                pattern,
                ty: None,
                value,
            },
            mutable: false,
        });
        self.declare_renamed(p, t, renames, out)
    }
    fn declare_renamed(
        &mut self,
        p: &Pattern,
        t: &Type,
        renames: BTreeMap<String, Ident>,
        out: &mut Statements,
    ) -> Result<(), String> {
        let mut types = vec![];
        self.pattern_types(p, t, &mut types);
        for (n, t) in types {
            if let Some(tmp) = renames.get(&n) {
                let lean = self.declare(&n, t)?;
                let_local(out, lean, reference(tmp), None, true);
            }
        }
        Ok(())
    }
    fn pat(&mut self, p: &Pattern, renames: &mut BTreeMap<String, Ident>) -> Result<LP, String> {
        Ok(match &p.kind {
            PatternKind::Wild => LP::Wildcard,
            PatternKind::Bind(n) => {
                let t = self.fresh(&format!("{n}$p"))?;
                renames.insert(n.clone(), t.clone());
                LP::Bind(t)
            }
            PatternKind::Literal(l) => match l {
                Literal::Int(n) => LP::Int(*n),
                Literal::Bool(b) => LP::Bool(*b),
                Literal::String(s) => LP::String(s.clone()),
                Literal::Float(_) => {
                    return Err(
                        "Lean extraction does not support floating-point literal patterns".into(),
                    );
                }
            },
            PatternKind::Tuple(ps) => LP::Tuple(
                ps.iter()
                    .map(|p| self.pat(p, renames))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            PatternKind::Constructor {
                name: n,
                fields,
                named,
            } => {
                if *named {
                    LP::Record {
                        fields: fields
                            .iter()
                            .map(|(n, p)| Ok((ident(n)?, self.pat(p, renames)?)))
                            .collect::<Result<Vec<_>, String>>()?,
                    }
                } else {
                    ctor_pattern(
                        self.constructor_name(n)?,
                        fields
                            .iter()
                            .map(|(_, p)| self.pat(p, renames))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
            }
        })
    }
    fn constructor_name(&self, n: &str) -> Result<Name, String> {
        Ok(match n {
            "Ok" => fixed_name(&["Except", "ok"]).rooted(),
            "Err" => fixed_name(&["Except", "error"]).rooted(),
            "Some" => fixed_name(&["Option", "some"]).rooted(),
            "None" => fixed_name(&["Option", "none"]).rooted(),
            _ => {
                let (a, v) = self
                    .world
                    .constructor(n)
                    .ok_or("unknown Lean constructor")?;
                Name::qualified([
                    ident(&a.name)?,
                    if a.is_enum {
                        ident(&v.name)?
                    } else {
                        fixed("mk")
                    },
                ])
                .map_err(|e| e.to_string())?
            }
        })
    }
    fn constructor(&self, n: &str) -> Result<Term, String> {
        Ok(Term::Ref(self.constructor_name(n)?))
    }
    fn function(&mut self, f: &Function) -> Result<Definition, String> {
        let effectful = effectful_function(f);
        let polymorphic = self.monad != global(&["Id"]);
        let mut binders = f
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| {
                Ok(Binder {
                    name: Some(ident(&g.name)?),
                    ty: Box::new(type0()),
                    kind: BinderKind::Implicit,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if polymorphic {
            binders.push(implicit("m", monad_ty()));
            binders.push(instance(None, app(global(&["Monad"]), [named("m")])));
            for (index, e) in function_requirements(f).iter().enumerate() {
                let variable = ident(&format!("$cap{index}"))?;
                if let Some(label) = &e.label {
                    self.caps.insert(
                        label.clone(),
                        CapBinding {
                            effect: e.clone(),
                            term: reference(&variable),
                            depth: 0,
                        },
                    );
                }
                let args = e.args.iter().map(ty).collect::<Result<Vec<_>, _>>()?;
                binders.push(instance(
                    Some(variable),
                    app(effect_ref(&e.name)?, args.into_iter().chain([named("m")])),
                ));
            }
        }
        for p in &f.params {
            let n = ident(&p.name)?;
            binders.push(explicit(n.clone(), self.ty(&p.ty)?));
            self.used.insert(n);
        }
        let mut result = vec![self.ty(&f.result)?];
        for p in f.params.iter().filter(|p| p.pass == Pass::Inout) {
            result.push(self.ty(&p.ty)?);
        }
        let result = tuple_type(&result);
        let result = if effectful {
            app(named("m"), [result])
        } else {
            result
        };
        let mut out = vec![];
        for p in &f.params {
            let lean = ident(&p.name)?;
            let_local(&mut out, lean.clone(), reference(&lean), None, true);
            self.declare_as(&p.name, p.ty.clone(), lean);
        }
        let term = self.expr(f.body.as_ref().ok_or("missing function body")?, &mut out)?;
        let r = self.temp();
        let_local(&mut out, r.clone(), term, Some(self.ty(&f.result)?), false);
        let mut values = vec![reference(&r)];
        for p in f.params.iter().filter(|p| p.pass == Pass::Inout) {
            values.push(self.var(&p.name).ok_or("missing mutable parameter")?);
        }
        let body = sequence(out, pure(tuple_term(&values)));
        let body = if effectful {
            body
        } else {
            app(global(&["Id", "run"]), [body])
        };
        Ok(Definition {
            name: source_name(&f.name)?,
            binders,
            result,
            body,
            kind: DefinitionKind::PartialDef,
        })
    }
    fn expr(&mut self, e: &Expr, out: &mut Statements) -> Result<Term, String> {
        let term = self.expr_inner(e, out)?;
        Ok(if self.type_of(e).is_some_and(|t| t.name() == "Never") {
            app(global(&["Empty", "elim"]), [term])
        } else {
            term
        })
    }
    fn expr_inner(&mut self, e: &Expr, out: &mut Statements) -> Result<Term, String> {
        Ok(match &e.kind {
            ExprKind::Literal(l) => {
                let term = literal(l);
                if let Some(t) = self.type_of(e) {
                    ascribe(term, self.ty(&t)?)
                } else {
                    term
                }
            }
            ExprKind::Name(n) => {
                if let Some(k) = self.continuations.get(n).cloned() {
                    return Ok(reference(&k.fallback.ok_or("Lean extraction cannot pass this continuation as a value: its frame or borrowed arguments must be threaded by a direct resumption")?));
                }
                if self.world.constructor(n).is_some() {
                    self.constructor(n)?
                } else if let Some(local) = self.scope.get(n).cloned() {
                    self.adapt_callable(
                        reference(&self.current[&local.id]),
                        &local.ty,
                        local.depth,
                    )?
                } else if self
                    .world
                    .functions
                    .get(n)
                    .is_some_and(|f| f.params.is_empty())
                {
                    lambda(
                        vec![Binder {
                            name: None,
                            ty: Box::new(global(&["Unit"])),
                            kind: BinderKind::Explicit,
                        }],
                        source_ref(n)?,
                    )
                } else {
                    source_ref(n)?
                }
            }
            ExprKind::Tuple(es) => tuple_term(&self.operands(&es.iter().collect::<Vec<_>>(), out)?),
            ExprKind::Struct { name: n, fields } => {
                let values =
                    self.operands(&fields.iter().map(|(_, e)| e).collect::<Vec<_>>(), out)?;
                ascribe(
                    Term::Record {
                        fields: fields
                            .iter()
                            .zip(values)
                            .map(|((n, _), v)| Ok((ident(n)?, v)))
                            .collect::<Result<Vec<_>, String>>()?,
                    },
                    source_ref(n)?,
                )
            }
            ExprKind::Field(base, n) => {
                let t = self.type_of(base);
                let b = self.expr(base, out)?;
                match t {
                    Some(t) => self.get_path(b, &t, std::slice::from_ref(n))?,
                    None => project(b, n)?,
                }
            }
            ExprKind::Unary(op, v) => {
                let value = self.expr(v, out)?;
                if op == "-"
                    && self
                        .world
                        .overflow_ops
                        .contains(&(e.span.start, e.span.end))
                    && let Some(kind) = self
                        .type_of(v)
                        .as_ref()
                        .and_then(crate::numeric::IntegerKind::from_type)
                {
                    self.sync(out);
                    self.guard_abort(true)?;
                    let t = self.temp();
                    let operand = self.fresh("$operand")?;
                    let ok = in_range("neg", kind, reference(&operand), unit())
                        .expect("integer negation range");
                    let mut statements = vec![];
                    let_local(&mut statements, operand.clone(), value, None, false);
                    let action = sequence(
                        statements,
                        Term::If {
                            condition: Box::new(ok),
                            yes: Box::new(pure(Term::Unary {
                                op: UnaryOp::Neg,
                                value: Box::new(reference(&operand)),
                            })),
                            no: Box::new(overflow_action()),
                        },
                    );
                    bind_local(out, t.clone(), action, None);
                    reference(&t)
                } else {
                    Term::Unary {
                        op: match op.as_str() {
                            "-" => UnaryOp::Neg,
                            "!" => UnaryOp::BoolNot,
                            _ => return Err(format!("unsupported Lean unary operator {op}")),
                        },
                        value: Box::new(value),
                    }
                }
            }
            ExprKind::Binary(op, a, b) if op == "&&" || op == "||" => {
                let left = self.expr(a, out)?;
                let mut rhs = vec![];
                let saved = (self.scope.clone(), self.current.clone());
                let right = self.expr(b, &mut rhs)?;
                (self.scope, self.current) = saved;
                if rhs.is_empty() {
                    Term::binary(
                        if op == "&&" {
                            BinaryOp::BoolAnd
                        } else {
                            BinaryOp::BoolOr
                        },
                        left,
                        right,
                    )
                } else {
                    let t = self.temp();
                    let (yes, no) = if op == "&&" {
                        (sequence(rhs, pure(right)), pure(Term::Bool(false)))
                    } else {
                        (pure(Term::Bool(true)), sequence(rhs, pure(right)))
                    };
                    bind_local(
                        out,
                        t.clone(),
                        Term::If {
                            condition: Box::new(left),
                            yes: Box::new(yes),
                            no: Box::new(no),
                        },
                        Some(global(&["Bool"])),
                    );
                    reference(&t)
                }
            }
            ExprKind::Binary(op, a, b) => {
                let kind = self
                    .type_of(a)
                    .as_ref()
                    .and_then(crate::numeric::IntegerKind::from_type);
                let vs = self.operands(&[a, b], out)?;
                let (a, b) = (vs[0].clone(), vs[1].clone());
                let key = (e.span.start, e.span.end);
                if self.world.partial_ops.contains(&key) || self.world.overflow_ops.contains(&key) {
                    self.sync(out);
                    self.guard_abort(true)?;
                    let t = self.temp();
                    let (left, right) = (self.fresh("$left")?, self.fresh("$right")?);
                    let mut statements = vec![];
                    let_local(&mut statements, left.clone(), a, None, false);
                    let_local(&mut statements, right.clone(), b, None, false);
                    statements.extend(self.failure_checks(
                        op,
                        kind,
                        key,
                        reference(&left),
                        reference(&right),
                    ));
                    let action = sequence(
                        statements,
                        pure(arith_term(op, kind, reference(&left), reference(&right))?),
                    );
                    bind_local(out, t.clone(), action, None);
                    reference(&t)
                } else if matches!(op.as_str(), "<" | "<=" | ">" | ">=") {
                    let comparison = Term::binary(
                        match op.as_str() {
                            "<" => BinaryOp::Lt,
                            "<=" => BinaryOp::Le,
                            ">" => BinaryOp::Gt,
                            _ => BinaryOp::Ge,
                        },
                        a,
                        b,
                    );
                    app(global(&["Decidable", "decide"]), [comparison])
                } else {
                    arith_term(op, kind, a, b)?
                }
            }
            ExprKind::Assign(..) => {
                self.statement(&Statement::Expr(e.clone()), out)?;
                unit()
            }
            ExprKind::Block { statements, tail } => {
                let saved_scope = self.scope.clone();
                let never = |g: &Self, e: &Expr| g.type_of(e).is_some_and(|t| t.name() == "Never");
                let mut diverged = None;
                for st in statements {
                    match st {
                        Statement::Expr(x) if never(self, x) => {
                            diverged = Some(self.expr(x, out)?);
                            break;
                        }
                        Statement::Let { value, .. } if never(self, value) => {
                            diverged = Some(self.expr(value, out)?);
                            break;
                        }
                        _ => self.statement(st, out)?,
                    }
                }
                let result = if let Some(d) = diverged {
                    d
                } else if let Some(t) = tail {
                    self.expr(t, out)?
                } else {
                    unit()
                };
                self.scope = saved_scope;
                result
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition, out)?;
                let yes = self.seq(then)?;
                let no = match otherwise {
                    Some(o) => self.seq(o)?,
                    None => pure(unit()),
                };
                let t = self.temp();
                let ann = self.type_of(e).map(|t| self.ty(&t)).transpose()?;
                bind_local(
                    out,
                    t.clone(),
                    Term::If {
                        condition: Box::new(c),
                        yes: Box::new(yes),
                        no: Box::new(no),
                    },
                    ann,
                );
                reference(&t)
            }
            ExprKind::Match { value, arms } => {
                let scrutinee_type = self.type_of(value).unwrap_or(Type::Infer);
                let v = self.expr(value, out)?;
                let t = self.temp();
                let ann = self.type_of(e).map(|t| self.ty(&t)).transpose()?;
                let mut matches = vec![];
                for (p, body) in arms {
                    let saved_scope = self.scope.clone();
                    let saved_current = self.current.clone();
                    let mut renames = BTreeMap::new();
                    let pattern = self.pat(p, &mut renames)?;
                    let mut statements = vec![];
                    self.declare_renamed(p, &scrutinee_type, renames, &mut statements)?;
                    let result = self.expr(body, &mut statements)?;
                    matches.push(MatchArm {
                        pattern,
                        body: sequence(statements, pure(result)),
                    });
                    self.scope = saved_scope;
                    self.current = saved_current;
                }
                bind_local(
                    out,
                    t.clone(),
                    Term::Match {
                        scrutinee: Box::new(v),
                        arms: matches,
                    },
                    ann,
                );
                reference(&t)
            }
            ExprKind::Call { callee, args, .. } => self.call(e, callee, args, out)?,
            ExprKind::Closure { params, body, .. } => self.closure(e, params, body, out)?,
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => self.handler(e, effect, state, body, ret, ops, out)?,
            ExprKind::Try(v) => {
                let value = self.expr(v, out)?;
                self.sync(out);
                self.guard_abort(true)?;
                let t = self.temp();
                let action = Term::Match {
                    scrutinee: Box::new(value),
                    arms: vec![
                        MatchArm {
                            pattern: ctor_pattern(
                                fixed_name(&["Except", "ok"]).rooted(),
                                vec![LP::Bind(fixed("$value"))],
                            ),
                            body: pure(named("$value")),
                        },
                        MatchArm {
                            pattern: ctor_pattern(
                                fixed_name(&["Except", "error"]).rooted(),
                                vec![LP::Bind(fixed("$error"))],
                            ),
                            body: fail_action(named("$error")),
                        },
                    ],
                };
                bind_local(out, t.clone(), action, None);
                reference(&t)
            }
            ExprKind::Index(base, index) => {
                let Some(Type::Tuple(ts)) = self.type_of(base) else {
                    return Err("Lean indexing requires a typed tuple".into());
                };
                if ts.is_empty() {
                    return Err("cannot extract indexing of an empty tuple".into());
                }
                let vs = self.operands(&[base, index], out)?;
                let tuple = self.fresh("$tuple")?;
                let position = self.fresh("$index")?;
                let projection = |i: usize| tuple_get(reference(&tuple), i, ts.len());
                let mut body = if self.world.partial_ops.contains(&(e.span.start, e.span.end)) {
                    runtime_failure("IndexOutOfBounds")
                } else {
                    pure(projection(ts.len() - 1))
                };
                for i in (0..ts.len()).rev() {
                    body = Term::If {
                        condition: Box::new(Term::binary(
                            BinaryOp::BoolEq,
                            reference(&position),
                            Term::Nat((i as u128).into()),
                        )),
                        yes: Box::new(pure(projection(i))),
                        no: Box::new(body),
                    };
                }
                self.sync(out);
                self.guard_abort(true)?;
                let t = self.temp();
                let mut statements = vec![];
                let_local(&mut statements, tuple, vs[0].clone(), None, false);
                let_local(&mut statements, position, vs[1].clone(), None, false);
                bind_local(out, t.clone(), sequence(statements, body), None);
                reference(&t)
            }
        })
    }
    fn failure_checks(
        &self,
        op: &str,
        kind: Option<crate::numeric::IntegerKind>,
        key: (usize, usize),
        left: Term,
        right: Term,
    ) -> Statements {
        let mut checks = vec![];
        if matches!(op, "/" | "%") && self.world.partial_ops.contains(&key) {
            checks.push(DS::If {
                condition: Term::binary(BinaryOp::BoolEq, right.clone(), Term::Nat(0u64.into())),
                yes: vec![DS::Expr(runtime_failure("DivisionByZero"))],
                no: None,
            });
        }
        if self.world.overflow_ops.contains(&key)
            && let Some(ok) = kind.and_then(|k| in_range(op, k, left, right))
        {
            checks.push(DS::If {
                condition: Term::Unary {
                    op: UnaryOp::BoolNot,
                    value: Box::new(ok),
                },
                yes: vec![DS::Expr(overflow_action())],
                no: None,
            });
        }
        checks
    }
    fn statement(&mut self, st: &Statement, out: &mut Statements) -> Result<(), String> {
        match st {
            Statement::Let { pat, ty: t, value } => {
                let val = self.expr(value, out)?;
                let inferred = if *t == Type::Infer {
                    if let Some(t) = self.type_of(value) {
                        t
                    } else {
                        match &value.kind {
                            ExprKind::Struct { name, .. } => Type::named(name),
                            ExprKind::Literal(l) => crate::check::literal_type(l),
                            ExprKind::Name(n) => self
                                .scope
                                .get(n)
                                .map(|l| l.ty.clone())
                                .unwrap_or(Type::Infer),
                            _ => Type::Infer,
                        }
                    }
                } else {
                    t.clone()
                };
                self.bind_pattern(pat, &inferred, val, out)?;
            }
            Statement::Expr(e) => {
                if let ExprKind::Assign(op, p, v) = &e.kind {
                    let value = self.expr(v, out)?;
                    let place =
                        crate::check::Place::from_expr(p).ok_or("assignment requires a place")?;
                    let new = if op == "=" {
                        value
                    } else {
                        let old = self.place_term(&place)?;
                        let key = (e.span.start, e.span.end);
                        let kind = self
                            .type_of(p)
                            .as_ref()
                            .and_then(crate::numeric::IntegerKind::from_type);
                        if self.world.partial_ops.contains(&key)
                            || self.world.overflow_ops.contains(&key)
                        {
                            self.sync(out);
                            self.guard_abort(true)?;
                            let t = self.temp();
                            let (left, right) = (self.fresh("$left")?, self.fresh("$right")?);
                            let mut statements = vec![];
                            let_local(&mut statements, left.clone(), old, None, false);
                            let_local(&mut statements, right.clone(), value, None, false);
                            statements.extend(self.failure_checks(
                                &op[..1],
                                kind,
                                key,
                                reference(&left),
                                reference(&right),
                            ));
                            let action = sequence(
                                statements,
                                pure(arith_term(
                                    &op[..1],
                                    kind,
                                    reference(&left),
                                    reference(&right),
                                )?),
                            );
                            bind_local(out, t.clone(), action, None);
                            reference(&t)
                        } else {
                            arith_term(&op[..1], kind, old, value)?
                        }
                    };
                    self.assign(&place, new, out)?;
                } else {
                    self.expr(e, out)?;
                }
            }
        }
        Ok(())
    }
    fn call(
        &mut self,
        e: &Expr,
        callee: &Expr,
        args: &[Arg],
        out: &mut Statements,
    ) -> Result<Term, String> {
        if let ExprKind::Field(base, method) = &callee.kind
            && let ExprKind::Name(local) = &base.kind
            && let Some(l) = self.scope.get(local)
        {
            let method_name = format!("{}::{method}", l.ty.name());
            if let Some(f) = self.world.functions.get(&method_name) {
                let mut actual = vec![Arg {
                    pass: f.params.first().map(|p| p.pass).unwrap_or(Pass::In),
                    explicit: false,
                    expr: *base.clone(),
                }];
                actual.extend(args.iter().cloned());
                let callee = Expr {
                    kind: ExprKind::Name(method_name),
                    span: callee.span.clone(),
                };
                return self.call(e, &callee, &actual, out);
            }
        }
        if let ExprKind::Field(base, op) = &callee.kind
            && let ExprKind::Name(label) = &base.kind
            && let Some(cap) = self.caps.get(label).cloned()
        {
            let layer = (cap.depth > 0).then(|| cap.depth - 1).filter(|i| {
                self.layers
                    .get(*i)
                    .is_some_and(|l| l.effect.label.as_deref() == Some(label))
            });
            let evidence = self.evidence(&cap)?;
            return self.perform(&cap.effect.name, op, layer, Some(evidence), args, out);
        }
        let ExprKind::Name(n) = &callee.kind else {
            return Err(
                "Lean extraction of method and indirect calls requires named functions".into(),
            );
        };
        if let Some(op) = crate::numeric::wrapping_intrinsic(n)
            && !self.scope.contains_key(n)
            && !self.world.functions.contains_key(n)
            && args.len() == 2
        {
            let values = self.operands(&args.iter().map(|a| &a.expr).collect::<Vec<_>>(), out)?;
            return arith_term(op, None, values[0].clone(), values[1].clone());
        }
        if let Some(k) = self.continuations.get(n).cloned()
            && k.context == self.context
        {
            return self.resume(&k, args, out);
        }
        if let Some(k) = self.continuations.get(n).cloned() {
            let fallback=k.fallback.ok_or("Lean extraction cannot resume this continuation outside its clause: its frame or borrowed arguments must be threaded by a direct resumption")?;
            let values = self.operands(&args.iter().map(|a| &a.expr).collect::<Vec<_>>(), out)?;
            let action = self.lift_action(app(reference(&fallback), values), k.depth);
            return self.action(action, out);
        }
        if let Some((eff, op)) = n.split_once("::")
            && self.world.effects.contains_key(eff)
        {
            if let Some(cap) = self.unlabeled.get(eff).cloned() {
                let label = cap.effect.label.clone();
                let layer = (cap.depth > 0).then(|| cap.depth - 1).filter(|i| {
                    self.layers
                        .get(*i)
                        .is_some_and(|l| l.effect.label.is_some() && l.effect.label == label)
                });
                let evidence = self.evidence(&cap)?;
                return self.perform(eff, op, layer, Some(evidence), args, out);
            }
            let explicit = match &e.kind {
                ExprKind::Call { types, .. } => crate::numeric::instance_key(eff, types),
                _ => None,
            };
            let typed = explicit.and_then(|key| {
                self.layers.iter().rposition(|l| {
                    crate::numeric::instance_key(&l.effect.name, &l.effect.args).as_ref()
                        == Some(&key)
                })
            });
            let layer = typed.or_else(|| self.layers.iter().rposition(|l| l.effect.name == eff));
            return self.perform(eff, op, layer, None, args, out);
        }
        if self.world.constructor(n).is_some() && !self.scope.contains_key(n) {
            let values = self.operands(&args.iter().map(|a| &a.expr).collect::<Vec<_>>(), out)?;
            return Ok(app(self.constructor(n)?, values));
        }
        let es = args.iter().map(|a| &a.expr).collect::<Vec<_>>();
        if let Some(local) = self.scope.get(n).cloned() {
            let Type::Function { params, .. } = &local.ty else {
                return Err("call of a non-function Lean local".into());
            };
            let params = params.clone();
            let values = self.operands(&es, out)?;
            let values = if values.is_empty() {
                vec![unit()]
            } else {
                values
            };
            let call = app(reference(&self.current[&local.id]), values);
            let states = params
                .iter()
                .zip(args)
                .filter(|((p, _), _)| *p == Pass::Inout)
                .map(|((p, t), a)| {
                    (
                        Param {
                            name: String::new(),
                            ty: t.clone(),
                            pass: *p,
                            projections: vec![],
                            span: e.span.clone(),
                        },
                        &a.expr,
                    )
                })
                .collect::<Vec<_>>();
            let result = if effectful_type(&local.ty) {
                let action = self.lift_action(call, local.depth);
                self.action(action, out)?
            } else if states.is_empty() {
                return Ok(call);
            } else {
                let t = self.temp();
                let_local(out, t.clone(), call, None, false);
                reference(&t)
            };
            let states = states.iter().map(|(p, a)| (p, *a)).collect::<Vec<_>>();
            return self.restore_states(result, &states, &BTreeMap::new(), out);
        }
        let f = self
            .world
            .functions
            .get(n)
            .ok_or_else(|| format!("unknown Lean callee {n}"))?;
        let mut evidence_args = vec![];
        for (index, req) in function_requirements(f).iter().enumerate() {
            if let Some(label) = &req.label
                && let Some(cap) = self.caps.get(label)
            {
                evidence_args.push(Argument::Named {
                    name: ident(&format!("$cap{index}"))?,
                    value: self.evidence(cap)?,
                });
            }
        }
        let solved = self.world.calls.get(&(e.span.start, e.span.end)).cloned();
        let mut arguments = f
            .generics
            .iter()
            .filter(|g| !g.row)
            .filter_map(|g| solved.as_ref().and_then(|s| s.get(&g.name)).map(|t| (g, t)))
            .map(|(g, t)| {
                Ok(Argument::Named {
                    name: ident(&g.name)?,
                    value: self.ty(t)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        arguments.extend(evidence_args);
        arguments.extend(
            self.operands(&es, out)?
                .into_iter()
                .map(Argument::Positional),
        );
        let call = Term::App {
            function: Box::new(source_ref(n)?),
            arguments,
        };
        let states = f
            .params
            .iter()
            .zip(args)
            .filter(|(p, _)| p.pass == Pass::Inout)
            .map(|(p, a)| (p, &a.expr))
            .collect::<Vec<_>>();
        let vars = solved.unwrap_or_default();
        let result = if effectful_function(f) {
            self.action(call, out)?
        } else if states.is_empty() {
            return Ok(call);
        } else {
            let t = self.temp();
            let_local(out, t.clone(), call, None, false);
            reference(&t)
        };
        self.restore_states(result, &states, &vars, out)
    }
    fn perform(
        &mut self,
        effect: &str,
        op: &str,
        layer: Option<usize>,
        evidence: Option<Term>,
        args: &[Arg],
        out: &mut Statements,
    ) -> Result<Term, String> {
        let decl = self
            .world
            .effects
            .get(effect)
            .ok_or("unknown Lean effect")?
            .clone();
        let sig = decl
            .ops
            .iter()
            .find(|f| f.name == op)
            .ok_or("unknown Lean operation")?
            .clone();
        let values = self.operands(&args.iter().map(|a| &a.expr).collect::<Vec<_>>(), out)?;
        let effect_args = layer
            .map(|i| self.layers[i].effect.args.clone())
            .unwrap_or_default();
        let vars = decl
            .generics
            .iter()
            .zip(&effect_args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let direct = layer.filter(|i| {
            self.closures == self.layers[*i].closures
                && self.layers[*i]
                    .ops
                    .get(op)
                    .is_some_and(|o| !o.aliased.is_empty())
        });
        let action = if let Some(index) = direct {
            let layer = self.layers[index].clone();
            let info = layer.ops[op].clone();
            let mut lenses = vec![];
            for &i in &info.aliased {
                let place = crate::check::Place::from_expr(&args[i].expr);
                let lens = match place
                    .as_ref()
                    .and_then(|p| self.scope.get(&p.root).map(|l| (p, l.clone())))
                {
                    Some((p, local)) if layer.frame.contains(&local.id) => {
                        let c = layer.frame.iter().position(|id| *id == local.id).unwrap();
                        let n = layer.frame.len();
                        let pt = self.ty(&substitute(&sig.params[i].ty, &vars))?;
                        let get =
                            self.get_path(tuple_get(named("$w"), c, n), &local.ty, &p.path)?;
                        let param_ty = substitute(&sig.params[i].ty, &vars);
                        let projections = if sig.params[i].projections.is_empty() {
                            vec![vec![]]
                        } else {
                            sig.params[i].projections.clone()
                        };
                        let mut component = tuple_get(named("$w"), c, n);
                        for proj in &projections {
                            let mut path = p.path.clone();
                            path.extend(proj.iter().cloned());
                            component = self.set_path(
                                component,
                                &local.ty,
                                &path,
                                self.get_path(named("$v"), &param_ty, proj)?,
                            )?;
                        }
                        let comps = (0..n)
                            .map(|j| {
                                if j == c {
                                    component.clone()
                                } else {
                                    tuple_get(named("$w"), j, n)
                                }
                            })
                            .collect::<Vec<_>>();
                        app(
                            global(&["Option", "some"]),
                            [tuple_term(&[
                                lambda(vec![explicit(fixed("$w"), layer.frame_ty.clone())], get),
                                lambda(
                                    vec![
                                        explicit(fixed("$w"), layer.frame_ty.clone()),
                                        explicit(fixed("$v"), pt),
                                    ],
                                    tuple_term(&comps),
                                ),
                            ])],
                        )
                    }
                    Some((_, local)) if local.depth > index && info.post_access => {
                        return Err(format!(
                            "Lean extraction cannot observe the final value of '{}' after resuming operation {effect}::{op}: the borrowed place is local to the handled computation",
                            place.unwrap().root
                        ));
                    }
                    _ => global(&["Option", "none"]),
                };
                lenses.push(lens);
            }
            let rt = self.op_value_type(&sig, &vars)?;
            let call = ascribe(
                app(
                    reference(&info.implementation),
                    lenses.into_iter().chain(values),
                ),
                app(layer.child.clone(), [rt]),
            );
            self.lift_action(call, index + 1)
        } else {
            let mut arguments = vec![];
            if let Some(value) = evidence {
                arguments.push(Argument::Named {
                    name: fixed("self"),
                    value,
                });
            }
            arguments.extend(values.into_iter().map(Argument::Positional));
            Term::App {
                function: Box::new(effect_operation(effect, op)?),
                arguments,
            }
        };
        let result = self.action_with(action, !decl.linear, out)?;
        let states = sig
            .params
            .iter()
            .zip(args)
            .filter(|(p, _)| p.pass == Pass::Inout)
            .map(|(p, a)| (p, &a.expr))
            .collect::<Vec<_>>();
        if states.is_empty() {
            return Ok(result);
        }
        self.restore_states(result, &states, &vars, out)
    }
    fn op_value_type(&self, sig: &Function, vars: &BTreeMap<String, Type>) -> Result<Term, String> {
        let mut ts = vec![self.ty(&substitute(&sig.result, vars))?];
        for p in sig.params.iter().filter(|p| p.pass == Pass::Inout) {
            ts.push(self.ty(&substitute(&p.ty, vars))?);
        }
        Ok(tuple_type(&ts))
    }
    fn resume(
        &mut self,
        k: &Resumption,
        args: &[Arg],
        out: &mut Statements,
    ) -> Result<Term, String> {
        let values = self.operands(&args.iter().map(|a| &a.expr).collect::<Vec<_>>(), out)?;
        let mut resumed = vec![values.first().cloned().unwrap_or_else(unit)];
        for a in k.aliases.iter().filter(|a| a.pass == Pass::Inout) {
            resumed.push(self.reconstruct(a)?);
        }
        let user = if k.stateful {
            values
                .get(1)
                .cloned()
                .ok_or("stateful continuation lacks a state")?
        } else {
            unit()
        };
        let t = self.temp();
        bind_local(
            out,
            t.clone(),
            app(
                reference(&k.resume),
                [
                    tuple_term(&resumed),
                    tuple_term(&[user, pair_get(reference(&k.state), 2)]),
                ],
            ),
            None,
        );
        out.push(DS::Assign {
            local: k.frame.clone(),
            value: pair_get(reference(&t), 2),
        });
        for a in &k.aliases {
            let mut refresh = vec![DS::Assign {
                local: a.raw.clone(),
                value: app(named("$get"), [reference(&k.frame)]),
            }];
            for (id, path) in &a.bindings {
                refresh.push(DS::Assign {
                    local: self.current[id].clone(),
                    value: self.get_path(reference(&a.raw), &a.ty, path)?,
                });
            }
            out.push(DS::Match {
                scrutinee: reference(&a.lens),
                arms: vec![
                    (
                        ctor_pattern(
                            fixed_name(&["Option", "some"]).rooted(),
                            vec![LP::Tuple(vec![LP::Bind(fixed("$get")), LP::Wildcard])],
                        ),
                        refresh,
                    ),
                    (
                        ctor_pattern(fixed_name(&["Option", "none"]).rooted(), vec![]),
                        vec![],
                    ),
                ],
            });
        }
        Ok(pair_get(reference(&t), 1))
    }
    fn reconstruct(&self, a: &Alias) -> Result<Term, String> {
        let mut value = reference(&a.raw);
        for (id, path) in &a.bindings {
            value = self.set_path(value, &a.ty, path, reference(&self.current[id]))?;
        }
        Ok(value)
    }
    fn closure(
        &mut self,
        e: &Expr,
        params: &[Param],
        body: &Expr,
        out: &mut Statements,
    ) -> Result<Term, String> {
        let Some(Type::Function {
            params: typed_params,
            result,
            effects,
            ..
        }) = self.world.types.get(&(e.span.start, e.span.end)).cloned()
        else {
            return Err("closure lacks its checked arrow type".into());
        };
        let effectful = !effects.instances.is_empty() || effects.tail.is_some();
        let mut captures = crate::check::free_names(body);
        for p in params {
            captures.remove(&p.name);
        }
        let captures = captures
            .into_iter()
            .filter(|n| self.scope.contains_key(n))
            .collect::<Vec<_>>();
        let mut snapshot = vec![];
        for n in &captures {
            let c = self.fresh(&format!("$capture_{n}"))?;
            let_local(out, c.clone(), self.var(n).unwrap(), None, false);
            snapshot.push(c);
        }
        let saved = self.save();
        self.new_context();
        self.body_layers.clear();
        self.closures += 1;
        let declared = match &e.kind {
            ExprKind::Closure { effects, .. } => effects.instances.clone(),
            _ => vec![],
        };
        for instance in declared.iter().chain(&effects.instances) {
            if let Some(cap) = instance
                .label
                .as_ref()
                .and_then(|l| self.caps.get(l))
                .cloned()
            {
                self.unlabeled.insert(instance.name.clone(), cap);
            }
        }
        self.clause_aliases.clear();
        self.alias_written = false;
        if !effectful {
            self.monad = global(&["Id"]);
        }
        let mut arg_names = vec![];
        let mut arguments = vec![];
        for (p, (_, t)) in params.iter().zip(&typed_params) {
            let a = self.fresh(&format!("{}$a", p.name))?;
            arguments.push(explicit(a.clone(), self.ty(t)?));
            arg_names.push(a);
        }
        if arguments.is_empty() {
            arguments.push(Binder {
                name: None,
                ty: Box::new(global(&["Unit"])),
                kind: BinderKind::Explicit,
            });
        }
        let mut statements = vec![];
        for (n, c) in captures.iter().zip(&snapshot) {
            let local = saved.scope[n].clone();
            let lean = self.fresh(n)?;
            let_local(&mut statements, lean.clone(), reference(c), None, true);
            self.current.insert(local.id, lean);
        }
        for ((p, (_, t)), a) in params.iter().zip(&typed_params).zip(&arg_names) {
            let lean = self.declare(&p.name, t.clone())?;
            let_local(&mut statements, lean, reference(a), None, true);
        }
        let value = self.expr(body, &mut statements)?;
        let r = self.temp();
        let_local(
            &mut statements,
            r.clone(),
            value,
            Some(self.ty(&result)?),
            false,
        );
        let mut results = vec![reference(&r)];
        for (p, (pass, _)) in params.iter().zip(&typed_params) {
            if *pass == Pass::Inout {
                results.push(self.var(&p.name).unwrap());
            }
        }
        let body = sequence(statements, pure(tuple_term(&results)));
        self.restore(saved);
        let t = self.temp();
        let body = if effectful {
            body
        } else {
            app(global(&["Id", "run"]), [body])
        };
        let_local(out, t.clone(), lambda(arguments, body), None, false);
        Ok(reference(&t))
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the lexical handler judgment"
    )]
    fn handler(
        &mut self,
        e: &Expr,
        effect: &Effect,
        state: &Option<(Pattern, Box<Expr>)>,
        body: &Expr,
        ret: &Clause,
        ops: &[Clause],
        out: &mut Statements,
    ) -> Result<Term, String> {
        self.counter += 1;
        let id = self.counter;
        let parent = self.monad.clone();
        let body_type = crate::numeric::default_type(
            self.world
                .types
                .get(&(body.span.start, body.span.end))
                .ok_or("handler body lacks a type")?,
        );
        let output = self.ty(&self.type_of(e).ok_or("handler lacks an output type")?)?;
        let state_type = state
            .as_ref()
            .and_then(|(_, s)| self.type_of(s))
            .unwrap_or_else(Type::unit);
        let user = self.ty(&state_type)?;
        let bt = self.ty(&body_type)?;
        let mut written = BTreeSet::new();
        written_names(self.world, body, &mut written);
        let frame = written
            .iter()
            .filter_map(|n| self.scope.get(n).map(|l| (l.id, l.ty.clone())))
            .collect::<BTreeMap<_, _>>();
        let frame_ids = frame.keys().copied().collect::<Vec<_>>();
        let frame_ty = tuple_type(
            &frame
                .values()
                .map(|t| self.ty(t))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let st = tuple_type(&[user.clone(), frame_ty.clone()]);
        let answer = tuple_type(&[output.clone(), frame_ty.clone()]);
        let child = app(
            named("HandlerCPS"),
            [st.clone(), answer.clone(), parent.clone()],
        );
        let initial = state
            .as_ref()
            .map(|(_, s)| self.expr(s, out))
            .transpose()?
            .unwrap_or_else(unit);
        let frame0 = tuple_term(
            &frame_ids
                .iter()
                .map(|i| reference(&self.current[i]))
                .collect::<Vec<_>>(),
        );
        let decl = self
            .world
            .effects
            .get(&effect.name)
            .ok_or("unknown Lean effect")?
            .clone();
        let vars = decl
            .generics
            .iter()
            .zip(&effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let args = effect
            .args
            .iter()
            .map(|t| self.ty(t))
            .collect::<Result<Vec<_>, _>>()?;
        // Indirect routes (helpers, callbacks, closures) cannot supply lenses.
        // Labels and continuations bound by handlers nested in the body
        // resume or perform lexically rather than through a helper.
        let mut lexical = BTreeSet::new();
        visit_expr(body, &mut |x| {
            if let ExprKind::Handle { effect, ops, .. } = &x.kind {
                lexical.extend(effect.label.clone());
                for c in ops {
                    if let Some(Pattern {
                        kind: PatternKind::Bind(k),
                        ..
                    }) = c.pats.last()
                    {
                        lexical.insert(k.clone());
                    }
                }
            }
        });
        let mut routes = false;
        visit_expr(body, &mut |x| match &x.kind {
            ExprKind::Closure { .. } => routes = true,
            ExprKind::Call { callee, .. } => {
                let effectful = match &callee.kind {
                    ExprKind::Name(n)
                        if lexical.contains(n)
                            || n.split_once("::")
                                .is_some_and(|(e, _)| self.world.effects.contains_key(e)) =>
                    {
                        false
                    }
                    ExprKind::Name(n) => {
                        self.world.functions.get(n).is_some_and(effectful_function)
                            || self
                                .world
                                .types
                                .get(&(callee.span.start, callee.span.end))
                                .is_some_and(effectful_type)
                    }
                    ExprKind::Field(base, m) => {
                        !matches!(&base.kind, ExprKind::Name(l) if self.caps.contains_key(l) || lexical.contains(l) || effect.label.as_deref() == Some(l.as_str()))
                            && self.world.functions.iter().any(|(n, f)| {
                                n.ends_with(&format!("::{m}")) && effectful_function(f)
                            })
                    }
                    _ => true,
                };
                routes |= effectful;
            }
            _ => {}
        });
        let mut statements = vec![];
        let init = self.fresh(&format!("$init{id}"))?;
        let_local(
            &mut statements,
            init.clone(),
            initial,
            Some(user.clone()),
            false,
        );
        let returned = self.fresh(&format!("$return{id}"))?;
        let result_arg = self.fresh(&format!("$result{id}"))?;
        let state_arg = self.fresh(&format!("$state{id}"))?;
        let saved = self.save();
        self.enter_clause();
        let mut clause = vec![];
        self.capture_clause(&crate::check::clause_free_names(ret), &saved, &mut clause)?;
        let return_type = if state.is_some() {
            Type::Tuple(vec![body_type.clone(), state_type.clone()])
        } else {
            body_type.clone()
        };
        let return_value = if state.is_some() {
            tuple_term(&[reference(&result_arg), pair_get(reference(&state_arg), 1)])
        } else {
            reference(&result_arg)
        };
        self.bind_pattern(&ret.pats[0], &return_type, return_value, &mut clause)?;
        let o = self.expr(&ret.body, &mut clause)?;
        let return_fn = inferred(
            vec![result_arg, state_arg.clone()],
            sequence(
                clause,
                pure(tuple_term(&[o, pair_get(reference(&state_arg), 2)])),
            ),
        );
        let_local(
            &mut statements,
            returned.clone(),
            return_fn,
            Some(arrows(
                vec![bt.clone(), st.clone()],
                app(parent.clone(), [answer.clone()]),
            )),
            false,
        );
        self.restore(saved);
        let mut infos = BTreeMap::new();
        let mut fields = vec![];
        for clause_ast in ops {
            let sig = decl
                .ops
                .iter()
                .find(|f| f.name == clause_ast.name)
                .ok_or("unknown Lean operation")?
                .clone();
            let implementation = self.fresh(&format!("$impl{id}_{}", clause_ast.name))?;
            let aliased = sig
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| p.pass != Pass::Sink)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            let mut index = sig.params.len();
            if state.is_some() {
                index += 1;
            }
            let k_name = match clause_ast.pats.get(index).map(|p| &p.kind) {
                Some(PatternKind::Bind(k)) => Some(k.clone()),
                _ => None,
            };
            let mut alias_names = BTreeSet::new();
            for &i in &aliased {
                let mut ps = vec![];
                self.pattern_paths(
                    &clause_ast.pats[i],
                    &substitute(&sig.params[i].ty, &vars),
                    vec![],
                    &mut ps,
                )?;
                alias_names.extend(ps.into_iter().map(|(n, _, _)| n));
            }
            let post_access = k_name
                .as_ref()
                .is_some_and(|k| post_resume_access(&clause_ast.body, k, &alias_names));
            if post_access && routes {
                return Err(format!(
                    "Lean extraction cannot track borrowed arguments of {}::{} after resumption when the handled computation may perform it through a helper, callback, or closure",
                    effect.name, clause_ast.name
                ));
            }
            let value_type = self.op_value_type(&sig, &vars)?;
            let resume = self.fresh(&format!("$resume{id}"))?;
            let st_var = self.fresh(&format!("$st{id}"))?;
            let frame_var = self.fresh(&format!("$frame{id}"))?;
            let saved = self.save();
            let captures = crate::check::clause_free_names(clause_ast);
            self.enter_clause();
            let mut clause = vec![];
            let_local(
                &mut clause,
                frame_var.clone(),
                pair_get(reference(&st_var), 2),
                None,
                true,
            );
            self.capture_clause(&captures, &saved, &mut clause)?;
            let mut parameters = vec![];
            let mut lens_types = vec![];
            let mut lens_names = vec![];
            let mut aliases = vec![];
            for (i, param) in sig.params.iter().enumerate() {
                let pt = substitute(&param.ty, &vars);
                let raw = self.fresh(&format!("$op{id}_{i}"))?;
                parameters.push(explicit(raw.clone(), self.ty(&pt)?));
                if param.pass == Pass::Sink {
                    self.bind_pattern(&clause_ast.pats[i], &pt, reference(&raw), &mut clause)?;
                    continue;
                }
                let lens = self.fresh(&format!("$lens{id}_{i}"))?;
                lens_types.push(app(
                    global(&["Option"]),
                    [tuple_type(&[
                        arrow(frame_ty.clone(), self.ty(&pt)?),
                        arrows(vec![frame_ty.clone(), self.ty(&pt)?], frame_ty.clone()),
                    ])],
                ));
                lens_names.push(lens.clone());
                let whole = self.fresh(&format!("$arg{id}_{i}"))?;
                let_local(&mut clause, whole.clone(), reference(&raw), None, true);
                let mut ps = vec![];
                self.pattern_paths(&clause_ast.pats[i], &pt, vec![], &mut ps)?;
                let mut bindings = vec![];
                for (n, path, t) in ps {
                    let lean = self.declare(&n, t.clone())?;
                    let_local(
                        &mut clause,
                        lean,
                        self.get_path(reference(&whole), &pt, &path)?,
                        None,
                        true,
                    );
                    let bid = self.scope[&n].id;
                    self.clause_aliases.insert(bid);
                    bindings.push((bid, path));
                }
                aliases.push(Alias {
                    lens,
                    raw: whole,
                    ty: pt,
                    pass: param.pass,
                    bindings,
                });
            }
            if state.is_some() {
                self.bind_pattern(
                    &clause_ast.pats[sig.params.len()],
                    &state_type,
                    pair_get(reference(&st_var), 1),
                    &mut clause,
                )?;
            }
            let fallback_ok = frame_ids.is_empty() && aliased.is_empty();
            if let Some(k) = &k_name {
                let fallback = if fallback_ok {
                    let f = self.fresh(k)?;
                    let rt = self.ty(&substitute(&sig.result, &vars))?;
                    let mut binders = vec![explicit(fixed("$value"), rt)];
                    let user_value = if state.is_some() {
                        binders.push(explicit(fixed("$state"), user.clone()));
                        named("$state")
                    } else {
                        unit()
                    };
                    let action = app(
                        reference(&resume),
                        [
                            named("$value"),
                            tuple_term(&[user_value, pair_get(reference(&st_var), 2)]),
                        ],
                    );
                    let value = ascribe(
                        sequence(
                            vec![DS::Bind {
                                pattern: LP::Bind(fixed("$r")),
                                action,
                            }],
                            pure(pair_get(named("$r"), 1)),
                        ),
                        app(parent.clone(), [output.clone()]),
                    );
                    let_local(&mut clause, f.clone(), lambda(binders, value), None, false);
                    Some(f)
                } else {
                    None
                };
                self.continuations.insert(
                    k.clone(),
                    Resumption {
                        resume: resume.clone(),
                        state: st_var.clone(),
                        stateful: state.is_some(),
                        frame: frame_var.clone(),
                        context: self.context,
                        depth: self.layers.len(),
                        aliases: aliases.clone(),
                        fallback,
                    },
                );
            }
            let o = self.expr(&clause_ast.body, &mut clause)?;
            let r = self.temp();
            let_local(&mut clause, r.clone(), o, Some(output.clone()), false);
            for a in aliases.iter().filter(|a| a.pass == Pass::Inout) {
                let value = self.reconstruct(a)?;
                clause.push(DS::Match {
                    scrutinee: reference(&a.lens),
                    arms: vec![
                        (
                            ctor_pattern(
                                fixed_name(&["Option", "some"]).rooted(),
                                vec![LP::Tuple(vec![LP::Wildcard, LP::Bind(fixed("$set"))])],
                            ),
                            vec![DS::Assign {
                                local: frame_var.clone(),
                                value: app(named("$set"), [reference(&frame_var), value]),
                            }],
                        ),
                        (
                            ctor_pattern(fixed_name(&["Option", "none"]).rooted(), vec![]),
                            vec![],
                        ),
                    ],
                });
            }
            let body = sequence(
                clause,
                pure(tuple_term(&[reference(&r), reference(&frame_var)])),
            );
            self.restore(saved);
            let resume_type = arrows(
                vec![value_type, st.clone()],
                app(parent.clone(), [answer.clone()]),
            );
            let mut signature = lens_types.clone();
            signature.extend(
                sig.params
                    .iter()
                    .map(|p| self.ty(&substitute(&p.ty, &vars)))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            signature.push(resume_type);
            signature.push(st.clone());
            let mut binders = lens_names
                .iter()
                .cloned()
                .map(|n| explicit(n, Term::Infer))
                .collect::<Vec<_>>();
            binders.extend(parameters);
            binders.extend([explicit(resume, Term::Infer), explicit(st_var, Term::Infer)]);
            let_local(
                &mut statements,
                implementation.clone(),
                lambda(binders, body),
                Some(arrows(signature, app(parent.clone(), [answer.clone()]))),
                false,
            );
            fields.push((
                ident(&clause_ast.name)?,
                app(
                    reference(&implementation),
                    std::iter::repeat_n(global(&["Option", "none"]), lens_names.len()),
                ),
            ));
            infos.insert(
                clause_ast.name.clone(),
                OpInfo {
                    implementation,
                    aliased,
                    post_access,
                },
            );
        }
        let dictionary = self.fresh(&format!("$handler{id}"))?;
        let dictionary_type = app(
            effect_ref(&effect.name)?,
            args.into_iter().chain([child.clone()]),
        );
        let_local(
            &mut statements,
            dictionary.clone(),
            Term::Record { fields },
            Some(dictionary_type.clone()),
            false,
        );
        statements.push(DS::LetInstance {
            name: None,
            ty: dictionary_type,
            value: reference(&dictionary),
        });
        let computation = self.fresh(&format!("$body{id}"))?;
        let saved = self.save();
        self.new_context();
        self.layers.push(Layer {
            state: st.clone(),
            output: answer.clone(),
            base: parent,
            child: child.clone(),
            effect: effect.clone(),
            frame: frame_ids.clone(),
            frame_ty,
            ops: infos,
            closures: self.closures,
        });
        let index = self.layers.len() - 1;
        self.body_layers.push(index);
        self.monad = child.clone();
        self.unlabeled.remove(&effect.name);
        self.clause_aliases.clear();
        self.alias_written = false;
        if let Some(label) = &effect.label {
            self.caps.insert(
                label.clone(),
                CapBinding {
                    effect: effect.clone(),
                    term: reference(&dictionary),
                    depth: self.layers.len(),
                },
            );
        }
        let mut body_statements = vec![];
        for &bid in &frame_ids {
            self.copy_binding(bid, &mut body_statements)?;
        }
        let v = self.expr(body, &mut body_statements)?;
        let r = self.temp();
        let_local(&mut body_statements, r.clone(), v, Some(bt.clone()), false);
        self.sync(&mut body_statements);
        let computation_body = sequence(body_statements, pure(reference(&r)));
        self.layers.pop();
        self.restore(saved);
        let_local(
            &mut statements,
            computation.clone(),
            computation_body,
            Some(app(child, [bt])),
            false,
        );
        let action = sequence(
            statements,
            app(
                reference(&computation),
                [
                    reference(&returned),
                    tuple_term(&[reference(&init), frame0]),
                ],
            ),
        );
        let t = self.action(action, out)?;
        for (c, bid) in frame_ids.iter().enumerate() {
            out.push(DS::Assign {
                local: self.current[bid].clone(),
                value: tuple_get(pair_get(t.clone(), 2), c, frame_ids.len()),
            });
        }
        Ok(pair_get(t, 1))
    }
    fn enter_clause(&mut self) {
        self.new_context();
        self.body_layers.clear();
        self.clause_aliases.clear();
        self.alias_written = false;
    }
    fn capture_clause(
        &mut self,
        captures: &BTreeSet<String>,
        saved: &Saved,
        out: &mut Statements,
    ) -> Result<(), String> {
        for n in captures {
            if let Some(local) = saved.scope.get(n) {
                let lean = self.fresh(n)?;
                let_local(
                    out,
                    lean.clone(),
                    reference(&saved.current[&local.id]),
                    None,
                    true,
                );
                self.current.insert(local.id, lean);
            }
        }
        Ok(())
    }
}
