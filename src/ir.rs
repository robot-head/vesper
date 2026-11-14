//! Basic-block Place IR. Calls and effects carry ordered tokens. The PCG records
//! field-tree ancestry rather than treating an aggregate as one indivisible loan.
use crate::{
    ast::*,
    check::{Checked, Place, World},
};
use petgraph::{
    Direction,
    graph::{DiGraph, NodeIndex},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub type ValueId = u32;
#[derive(Debug, Clone, Serialize)]
pub struct Argument {
    pub pass: Pass,
    pub value: ValueId,
    pub place: Option<Place>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Instruction {
    pub dest: ValueId,
    pub op: Op,
    pub ty: Type,
    pub span: Span,
    pub token: Option<(u32, u32)>,
    /// The checker could not prove this primitive operation total: it may
    /// raise `Fail[RuntimeError]` or `Fail[ArithOverflow]`. Fallible
    /// operations are ordered state-token actions and abortable frames.
    pub fallible: bool,
}
#[derive(Debug, Clone, Serialize)]
pub enum Op {
    Constant(Literal),
    Unit,
    Read {
        root: String,
        path: Vec<String>,
        pass: Pass,
        projections: Vec<Vec<String>>,
    },
    Bind {
        pattern: Pattern,
        value: ValueId,
    },
    Write {
        place: String,
        op: String,
        value: ValueId,
    },
    Unary(String, ValueId),
    Binary(String, ValueId, ValueId),
    Tuple(Vec<ValueId>),
    Data(String, Vec<(String, ValueId)>),
    Project(ValueId, String),
    Index(ValueId, ValueId),
    Call {
        callee: String,
        args: Vec<Argument>,
        types: Vec<Type>,
        capability: Option<String>,
    },
    Indirect {
        callee: ValueId,
        args: Vec<Argument>,
    },
    Try(ValueId),
    Closure {
        expr: Expr,
        aliases: BTreeMap<String, String>,
        moves: Vec<Place>,
    },
    Handle {
        state_value: Option<ValueId>,
        expr: Expr,
        aliases: BTreeMap<String, String>,
        moves: Vec<Place>,
    },
    /// A total vector operation (§14.3, lane 28) on its by-value operands.
    /// Backends that do not know an operation reject it with a typed error.
    Vector {
        op: crate::vector::VectorOp,
        args: Vec<ValueId>,
    },
}
#[derive(Debug, Clone, Serialize)]
pub enum Terminator {
    Return(ValueId),
    Jump {
        block: usize,
        args: Vec<ValueId>,
    },
    Branch {
        condition: ValueId,
        yes: usize,
        no: usize,
    },
    Match {
        value: ValueId,
        arms: Vec<(Pattern, usize)>,
    },
    Unreachable,
}
#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub entry_token: u32,
    pub incoming_tokens: Vec<(usize, u32)>,
    pub outgoing_token: u32,
    pub params: Vec<ValueId>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}
#[derive(Debug, Clone, Serialize)]
pub struct FunctionIr {
    pub name: String,
    pub params: Vec<Param>,
    pub result: Type,
    pub effects: Effects,
    pub bounds: BTreeMap<String, Mode>,
    pub external_captures: BTreeSet<String>,
    pub blocks: Vec<Block>,
    pub values: BTreeMap<ValueId, Type>,
    pub locals: BTreeMap<String, Type>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Program {
    pub functions: BTreeMap<String, FunctionIr>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaceGraph {
    pub graph: DiGraph<String, ()>,
    pub nodes: BTreeMap<String, NodeIndex>,
}
impl PlaceGraph {
    pub fn insert(&mut self, p: &Place) -> NodeIndex {
        let mut name = p.root.clone();
        let mut parent = if let Some(n) = self.nodes.get(&name) {
            *n
        } else {
            let n = self.graph.add_node(name.clone());
            self.nodes.insert(name.clone(), n);
            n
        };
        for field in &p.path {
            name.push('.');
            name.push_str(field);
            let child = if let Some(n) = self.nodes.get(&name) {
                *n
            } else {
                let n = self.graph.add_node(name.clone());
                self.nodes.insert(name.clone(), n);
                self.graph.add_edge(parent, n, ());
                n
            };
            parent = child;
        }
        parent
    }
    pub fn overlaps(&self, a: &Place, b: &Place) -> bool {
        let (Some(a), Some(b)) = (self.nodes.get(&a.display()), self.nodes.get(&b.display()))
        else {
            return a.overlaps(b);
        };
        a == b
            || petgraph::algo::has_path_connecting(&self.graph, *a, *b, None)
            || petgraph::algo::has_path_connecting(&self.graph, *b, *a, None)
    }
    pub fn roots(&self) -> Vec<&str> {
        self.graph
            .node_indices()
            .filter(|n| {
                self.graph
                    .neighbors_directed(*n, Direction::Incoming)
                    .next()
                    .is_none()
            })
            .map(|n| self.graph[n].as_str())
            .collect()
    }
}
pub fn lower(checked: &Checked) -> Program {
    lower_world(&checked.world)
}
pub fn lower_world(world: &World) -> Program {
    let mut program = Program::default();
    let mut pending = world
        .functions
        .values()
        .cloned()
        .map(|f| {
            let caps = f
                .effects
                .instances
                .iter()
                .filter_map(|e| e.label.clone().map(|l| (l, e.name.clone())))
                .collect();
            (f, BTreeSet::new(), caps)
        })
        .collect::<Vec<_>>();
    while let Some((f, external_captures, capabilities)) = pending.pop() {
        if program.functions.contains_key(&f.name) {
            continue;
        }
        if let Some(body) = &f.body {
            let mut l = Lower {
                blocks: vec![block()],
                current: 0,
                next: 0,
                token: 0,
                types: &world.types,
                world,
                values: BTreeMap::new(),
                locals: f
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), p.ty.clone()))
                    .collect(),
                names: f
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), p.name.clone()))
                    .collect(),
                generics: f.generics.clone(),
                permissions: f
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), (p.pass, p.projections.clone())))
                    .collect(),
                nested: vec![],
                capabilities,
            };
            let value = l.expr(body);
            l.blocks[l.current].terminator = Terminator::Return(value);
            pending.extend(l.nested);
            program.functions.insert(
                f.name.clone(),
                FunctionIr {
                    name: f.name.clone(),
                    params: f.params.clone(),
                    result: f.result.clone(),
                    effects: f.effects.clone(),
                    bounds: f
                        .generics
                        .iter()
                        .filter(|g| !g.row)
                        .map(|g| (g.name.clone(), g.bound))
                        .collect(),
                    external_captures,
                    blocks: l.blocks,
                    values: l.values,
                    locals: l.locals,
                },
            );
        }
    }
    thread_tokens(&mut program);
    program
}
fn block() -> Block {
    Block {
        entry_token: 0,
        incoming_tokens: vec![],
        outgoing_token: 0,
        params: vec![],
        instructions: vec![],
        terminator: Terminator::Unreachable,
    }
}
/// Whether an instruction consumes and produces state-token evidence: writes,
/// ownership binds, calls, handlers and every potentially failing primitive.
pub fn state_action(i: &Instruction) -> bool {
    matches!(
        i.op,
        Op::Bind { .. }
            | Op::Write { .. }
            | Op::Call { .. }
            | Op::Indirect { .. }
            | Op::Try(_)
            | Op::Handle { .. }
            | Op::Closure { .. }
            | Op::Index(..)
    ) || i.fallible
        || matches!(&i.op, Op::Binary(op, ..) if matches!(op.as_str(), "/" | "%"))
}
/// Give each CFG merge a state-token parameter and attach predecessor evidence.
/// Reads observe a token; writes, ownership binds and calls produce fresh ones.
pub fn thread_tokens(program: &mut Program) {
    for f in program.functions.values_mut() {
        let mut next = f.blocks.len() as u32;
        for (id, b) in f.blocks.iter_mut().enumerate() {
            b.entry_token = id as u32;
            b.incoming_tokens.clear();
            let mut current = b.entry_token;
            for i in &mut b.instructions {
                let observation = matches!(i.op, Op::Read { .. });
                let action = state_action(i);
                i.token = if action {
                    let output = next;
                    next += 1;
                    let input = current;
                    current = output;
                    Some((input, output))
                } else if observation {
                    Some((current, current))
                } else {
                    None
                };
            }
            b.outgoing_token = current;
        }
        let mut edges = vec![];
        for (id, b) in f.blocks.iter().enumerate() {
            let targets = match &b.terminator {
                Terminator::Jump { block, .. } => vec![*block],
                Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
                _ => vec![],
            };
            for target in targets {
                edges.push((target, id, b.outgoing_token));
            }
        }
        for (target, source, token) in edges {
            f.blocks[target].incoming_tokens.push((source, token));
        }
    }
}
struct Lower<'a> {
    blocks: Vec<Block>,
    current: usize,
    next: u32,
    token: u32,
    types: &'a TypeMap,
    world: &'a World,
    names: BTreeMap<String, String>,
    locals: BTreeMap<String, Type>,
    values: BTreeMap<ValueId, Type>,
    capabilities: BTreeMap<String, String>,
    generics: Vec<Generic>,
    permissions: BTreeMap<String, (Pass, Vec<Vec<String>>)>,
    nested: Vec<(Function, BTreeSet<String>, BTreeMap<String, String>)>,
}
impl Lower<'_> {
    fn capture_params(&self, names: BTreeSet<String>, borrowed: bool, span: &Span) -> Vec<Param> {
        names
            .into_iter()
            .filter_map(|name| {
                self.names.get(&name).and_then(|slot| {
                    self.locals.get(slot).map(|ty| {
                        let (pass, projections) = if borrowed {
                            self.permissions
                                .get(slot)
                                .cloned()
                                .unwrap_or((Pass::Sink, vec![]))
                        } else {
                            (Pass::Sink, vec![])
                        };
                        Param {
                            name,
                            ty: ty.clone(),
                            pass,
                            projections,
                            span: span.clone(),
                        }
                    })
                })
            })
            .collect()
    }
    fn add_nested(
        &mut self,
        kind: &str,
        body: &Expr,
        params: Vec<Param>,
        result: Type,
        effects: Effects,
        external: bool,
    ) {
        let captures = if external {
            params
                .iter()
                .filter(|p| p.pass == Pass::Sink)
                .map(|p| p.name.clone())
                .collect()
        } else {
            BTreeSet::new()
        };
        self.nested.push((
            Function {
                name: nested_name(kind, &body.span),
                params,
                result,
                effects,
                generics: self.generics.clone(),
                body: Some(body.clone()),
                span: body.span.clone(),
            },
            captures,
            self.capabilities.clone(),
        ));
    }
    fn prepare_nested(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Closure { params, body, .. } => {
                let mut names = crate::check::free_names(body);
                for p in params {
                    names.remove(&p.name);
                }
                let mut captured = self.capture_params(names, false, &e.span);
                if let Some(Type::Function {
                    params: typed,
                    result,
                    effects,
                    ..
                }) = self.types.get(&(e.span.start, e.span.end))
                {
                    let mut actual = params.clone();
                    for (p, (pass, t)) in actual.iter_mut().zip(typed) {
                        p.pass = *pass;
                        p.ty = t.clone();
                    }
                    captured.extend(actual);
                    self.add_nested(
                        "closure",
                        body,
                        captured,
                        *result.clone(),
                        effects.clone(),
                        false,
                    );
                }
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let body_type = self
                    .types
                    .get(&(body.span.start, body.span.end))
                    .cloned()
                    .unwrap_or(Type::Infer);
                let output = self
                    .types
                    .get(&(e.span.start, e.span.end))
                    .cloned()
                    .unwrap_or(Type::Infer);
                let outgoing = self
                    .world
                    .handler_effects
                    .get(&(e.span.start, e.span.end))
                    .cloned()
                    .unwrap_or_default();
                let captured =
                    self.capture_params(crate::check::free_names(body), true, &body.span);
                let outer_capabilities = self.capabilities.clone();
                if let Some(label) = &effect.label {
                    self.capabilities.insert(label.clone(), effect.name.clone());
                }
                self.add_nested(
                    "handler_body",
                    body,
                    captured,
                    body_type.clone(),
                    outgoing.clone(),
                    true,
                );
                self.capabilities = outer_capabilities;
                let state_type = state
                    .as_ref()
                    .and_then(|(_, s)| self.types.get(&(s.span.start, s.span.end)))
                    .cloned()
                    .unwrap_or_else(Type::unit);
                let ret_type = if state.is_some() {
                    Type::Tuple(vec![body_type, state_type.clone()])
                } else {
                    body_type
                };
                let mut ret_params = self.capture_params(
                    crate::check::clause_free_names(ret),
                    false,
                    &ret.body.span,
                );
                pattern_params(
                    self.world,
                    &ret.pats[0],
                    &ret_type,
                    Pass::Sink,
                    &mut ret_params,
                );
                self.add_nested(
                    "handler_clause",
                    &ret.body,
                    ret_params,
                    output.clone(),
                    outgoing.clone(),
                    false,
                );
                if let Some(decl) = self.world.effects.get(&effect.name) {
                    let vars = decl
                        .generics
                        .iter()
                        .zip(&effect.args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    for clause in ops {
                        let Some(sig) = decl.ops.iter().find(|f| f.name == clause.name) else {
                            continue;
                        };
                        let mut ps = self.capture_params(
                            crate::check::clause_free_names(clause),
                            false,
                            &clause.body.span,
                        );
                        for (p, param) in clause.pats.iter().zip(&sig.params) {
                            pattern_params(
                                self.world,
                                p,
                                &substitute(&param.ty, &vars),
                                param.pass,
                                &mut ps,
                            );
                            if let PatternKind::Bind(n) = &p.kind
                                && let Some(p) = ps.iter_mut().find(|p| &p.name == n)
                            {
                                p.projections = param.projections.clone();
                            }
                        }
                        let mut index = sig.params.len();
                        if state.is_some() {
                            if let Some(p) = clause.pats.get(index) {
                                pattern_params(self.world, p, &state_type, Pass::Sink, &mut ps);
                            }
                            index += 1;
                        }
                        let mut kparams = vec![(Pass::Sink, substitute(&sig.result, &vars))];
                        if state.is_some() {
                            kparams.push((Pass::Sink, state_type.clone()));
                        }
                        let ktype = Type::Function {
                            params: kparams,
                            result: Box::new(output.clone()),
                            effects: outgoing.clone(),
                            mode: if decl.linear {
                                Mode::Linear
                            } else {
                                Mode::Affine
                            },
                        };
                        if let Some(p) = clause.pats.get(index) {
                            pattern_params(self.world, p, &ktype, Pass::Sink, &mut ps);
                        }
                        self.add_nested(
                            "handler_clause",
                            &clause.body,
                            ps,
                            output.clone(),
                            outgoing.clone(),
                            false,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    fn fresh(&mut self) -> u32 {
        let n = self.next;
        self.next += 1;
        n
    }
    /// The checked vector operation a reserved path call denotes.
    fn vector_op(
        &self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        e: &Expr,
    ) -> Option<crate::vector::VectorOp> {
        let ty = |e: &Expr| {
            self.types
                .get(&(e.span.start, e.span.end))
                .cloned()
                .unwrap_or(Type::Infer)
        };
        let exprs = args.iter().map(|a| &a.expr).collect::<Vec<_>>();
        let arg_types = exprs.iter().map(|e| ty(e)).collect::<Vec<_>>();
        crate::vector::VectorOp::resolve(name, types, &exprs, &arg_types, &ty(e))
    }
    fn emit(&mut self, op: Op, e: &Expr, effect: bool) -> u32 {
        let dest = self.fresh();
        let token = if effect {
            let old = self.token;
            self.token += 1;
            Some((old, self.token))
        } else {
            None
        };
        let mut ty = crate::numeric::default_type(
            self.types
                .get(&(e.span.start, e.span.end))
                .unwrap_or(&Type::Infer),
        );
        if ty == Type::Infer
            && let Op::Read { root, path, .. } = &op
            && let Some(root_ty) = self.locals.get(root)
            && let Some(t) = self.world.field(root_ty, path)
        {
            ty = crate::numeric::default_type(&t);
        }
        if matches!(op, Op::Bind { .. } | Op::Write { .. }) {
            ty = Type::unit();
        }
        let key = (e.span.start, e.span.end);
        let fallible = matches!(
            op,
            Op::Binary(..) | Op::Unary(..) | Op::Write { .. } | Op::Index(..)
        ) && (self.world.partial_ops.contains(&key)
            || self.world.overflow_ops.contains(&key));
        self.values.insert(dest, ty.clone());
        self.blocks[self.current].instructions.push(Instruction {
            dest,
            op,
            ty,
            span: e.span.clone(),
            token,
            fallible,
        });
        dest
    }
    fn new_block(&mut self) -> usize {
        let n = self.blocks.len();
        self.blocks.push(block());
        n
    }
    fn place(&self, e: &Expr) -> Option<Place> {
        let mut p = Place::from_expr(e)?;
        p.root = self.names.get(&p.root)?.clone();
        Some(p)
    }
    fn rename_pattern(&mut self, p: &Pattern, ty: &Type) -> Pattern {
        let mut p = p.clone();
        match &mut p.kind {
            PatternKind::Bind(n) => {
                let source = n.clone();
                let unique = format!("$slot{}_{}", self.fresh(), source);
                self.names.insert(source, unique.clone());
                self.locals
                    .insert(unique.clone(), crate::numeric::default_type(ty));
                *n = unique;
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = ty {
                    for (p, t) in ps.iter_mut().zip(ts) {
                        *p = self.rename_pattern(p, t);
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let args = if let Type::Named(_, args) = ty {
                        args.as_slice()
                    } else {
                        &[]
                    };
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    for (n, p) in fields {
                        if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                            *p = self.rename_pattern(p, &substitute(&f.ty, &vars));
                        }
                    }
                }
            }
            _ => {}
        }
        p
    }
    fn argument(&mut self, e: &Expr, pass: Pass, projections: Vec<Vec<String>>) -> Argument {
        let place = self.place(e);
        let value = if let Some(p) = &place {
            self.emit(
                Op::Read {
                    root: p.root.clone(),
                    path: p.path.clone(),
                    pass,
                    projections,
                },
                e,
                false,
            )
        } else {
            self.expr(e)
        };
        Argument { pass, value, place }
    }
    fn expr(&mut self, e: &Expr) -> u32 {
        if matches!(e.kind, ExprKind::Closure { .. } | ExprKind::Handle { .. }) {
            self.prepare_nested(e);
        }
        if let ExprKind::Handle {
            state: Some((_, initial)),
            ..
        } = &e.kind
        {
            let before = self
                .blocks
                .iter()
                .map(|b| b.instructions.len())
                .collect::<Vec<_>>();
            let state_value = self.expr(initial);
            let mut initialized = vec![];
            for (index, block) in self.blocks.iter().enumerate() {
                for i in block
                    .instructions
                    .iter()
                    .skip(before.get(index).copied().unwrap_or(0))
                {
                    match &i.op {
                        Op::Read {
                            root,
                            path,
                            pass: Pass::Sink,
                            projections,
                        } => {
                            if projections.is_empty() {
                                initialized.push(Place {
                                    root: root.clone(),
                                    path: path.clone(),
                                });
                            } else {
                                for projection in projections {
                                    let mut path = path.clone();
                                    path.extend(projection.clone());
                                    initialized.push(Place {
                                        root: root.clone(),
                                        path,
                                    });
                                }
                            }
                        }
                        Op::Closure { moves, .. } | Op::Handle { moves, .. } => {
                            initialized.extend(moves.clone())
                        }
                        _ => {}
                    }
                }
            }
            let moves = self
                .world
                .moves
                .get(&(e.span.start, e.span.end))
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    self.names.get(&p.root).map(|root| Place {
                        root: root.clone(),
                        path: p.path.clone(),
                    })
                })
                .filter(|p| !initialized.contains(p))
                .collect();
            return self.emit(
                Op::Handle {
                    state_value: Some(state_value),
                    expr: e.clone(),
                    aliases: self.names.clone(),
                    moves,
                },
                e,
                true,
            );
        }
        match &e.kind {
            ExprKind::Literal(l) => self.emit(Op::Constant(l.clone()), e, false),
            ExprKind::Name(n) => {
                if !self.names.contains_key(n) && self.world.constructor(n).is_some() {
                    self.emit(Op::Data(n.clone(), vec![]), e, false)
                } else {
                    self.emit(
                        Op::Read {
                            root: self.names.get(n).cloned().unwrap_or_else(|| n.clone()),
                            path: vec![],
                            pass: Pass::Sink,
                            projections: vec![],
                        },
                        e,
                        false,
                    )
                }
            }
            ExprKind::Tuple(es) => {
                let vs = es.iter().map(|e| self.expr(e)).collect();
                self.emit(Op::Tuple(vs), e, false)
            }
            ExprKind::Struct { name, fields } => {
                let fields = fields
                    .iter()
                    .map(|(n, e)| (n.clone(), self.expr(e)))
                    .collect();
                self.emit(Op::Data(name.clone(), fields), e, false)
            }
            ExprKind::Field(base, field) => {
                if let Some(p) = self.place(e) {
                    self.emit(
                        Op::Read {
                            root: p.root,
                            path: p.path,
                            pass: Pass::Sink,
                            projections: vec![],
                        },
                        e,
                        false,
                    )
                } else {
                    let v = self.expr(base);
                    self.emit(Op::Project(v, field.clone()), e, false)
                }
            }
            ExprKind::Index(base, index) => {
                let b = self.expr(base);
                let i = self.expr(index);
                self.emit(Op::Index(b, i), e, false)
            }
            ExprKind::Unary(op, v) => {
                let v = self.expr(v);
                self.emit(Op::Unary(op.clone(), v), e, false)
            }
            ExprKind::Binary(op, a, b) if op == "&&" || op == "||" => {
                let lit = Expr {
                    kind: ExprKind::Literal(Literal::Bool(op == "||")),
                    span: e.span.clone(),
                };
                let (then, otherwise) = if op == "&&" {
                    (b.clone(), Box::new(lit))
                } else {
                    (Box::new(lit), b.clone())
                };
                self.expr(&Expr {
                    kind: ExprKind::If {
                        condition: a.clone(),
                        then,
                        otherwise: Some(otherwise),
                    },
                    span: e.span.clone(),
                })
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr(a);
                let b = self.expr(b);
                self.emit(Op::Binary(op.clone(), a, b), e, false)
            }
            ExprKind::Assign(op, p, v) => {
                let value = self.expr(v);
                let place = self.place(p).map(|p| p.display()).unwrap_or_default();
                self.emit(
                    Op::Write {
                        place,
                        op: op.clone(),
                        value,
                    },
                    e,
                    true,
                )
            }
            ExprKind::Block { statements, tail } => {
                let outer = self.names.clone();
                for s in statements {
                    match s {
                        Statement::Let { pat, value, .. } => {
                            let v = self.expr(value);
                            let t = self.values[&v].clone();
                            let p = self.rename_pattern(pat, &t);
                            self.emit(
                                Op::Bind {
                                    pattern: p,
                                    value: v,
                                },
                                value,
                                false,
                            );
                        }
                        Statement::Expr(statement) => {
                            self.expr(statement);
                            if self
                                .types
                                .get(&(statement.span.start, statement.span.end))
                                .is_some_and(|t| t.name() == "Never")
                            {
                                self.names = outer;
                                return self.emit(Op::Unit, e, false);
                            }
                        }
                    }
                }
                let value = if let Some(t) = tail {
                    self.expr(t)
                } else {
                    self.emit(Op::Unit, e, false)
                };
                self.names = outer;
                value
            }
            ExprKind::Call {
                callee,
                args,
                types,
            } => {
                let mut receiver = None;
                let mut capability = None;
                let name = match &callee.kind {
                    ExprKind::Name(n) if !self.names.contains_key(n) => Some(n.clone()),
                    ExprKind::Field(base, method) => {
                        let t = self
                            .types
                            .get(&(base.span.start, base.span.end))
                            .cloned()
                            .unwrap_or(Type::Infer);
                        let method_name = format!("{}::{method}", t.name());
                        if self.world.functions.contains_key(&method_name) {
                            receiver = Some(base.as_ref());
                            Some(method_name)
                        } else if let ExprKind::Name(label) = &base.kind {
                            if let Some(effect) = self.capabilities.get(label) {
                                capability = Some(label.clone());
                                Some(format!("{effect}::{method}"))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(name) = name {
                    if receiver.is_none()
                        && !self.world.functions.contains_key(&name)
                        && let Some(op) = self.vector_op(&name, types, args, e)
                    {
                        let operands = args
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| crate::vector::is_operand(&name, *i, args.len()))
                            // Lanes are copied by value; a string operand
                            // is a shared `in` borrow.
                            .map(|(_, a)| {
                                let string = self
                                    .types
                                    .get(&(a.expr.span.start, a.expr.span.end))
                                    .is_some_and(|t| t.name() == "String");
                                if string {
                                    self.argument(&a.expr, Pass::In, vec![]).value
                                } else {
                                    self.expr(&a.expr)
                                }
                            })
                            .collect();
                        return self.emit(Op::Vector { op, args: operands }, e, false);
                    }
                    // Wrapping intrinsics are total binary operators in IR.
                    if crate::numeric::wrapping_intrinsic(&name).is_some()
                        && !self.world.functions.contains_key(&name)
                        && args.len() == 2
                    {
                        let a = self.expr(&args[0].expr);
                        let b = self.expr(&args[1].expr);
                        return self.emit(Op::Binary(name, a, b), e, false);
                    }
                    // Bitwise intrinsics of the secret operations
                    // (`capability_effects`) are total operators too.
                    if !self.world.functions.contains_key(&name) {
                        if crate::capability_effects::bit_intrinsic(&name).is_some()
                            && args.len() == 2
                        {
                            let a = self.expr(&args[0].expr);
                            let b = self.expr(&args[1].expr);
                            return self.emit(Op::Binary(name, a, b), e, false);
                        }
                        if name == crate::capability_effects::WIDEN_INTRINSIC && args.len() == 1 {
                            let a = self.expr(&args[0].expr);
                            return self.emit(Op::Unary(name, a), e, false);
                        }
                    }
                    if self.world.constructor(&name).is_some() {
                        let fields = args
                            .iter()
                            .enumerate()
                            .map(|(i, a)| (i.to_string(), self.expr(&a.expr)))
                            .collect();
                        return self.emit(Op::Data(name, fields), e, false);
                    }
                    let params = self
                        .world
                        .functions
                        .get(&name)
                        .map(|f| f.params.clone())
                        .or_else(|| {
                            name.split_once("::").and_then(|(effect, op)| {
                                self.world
                                    .effects
                                    .get(effect)
                                    .and_then(|d| d.ops.iter().find(|f| f.name == op))
                                    .map(|f| f.params.clone())
                            })
                        })
                        .unwrap_or_default();
                    let mut es = vec![];
                    if let Some(base) = receiver {
                        es.push(base);
                    }
                    es.extend(args.iter().map(|a| &a.expr));
                    let actual = es
                        .into_iter()
                        .enumerate()
                        .map(|(i, e)| {
                            self.argument(
                                e,
                                params.get(i).map(|p| p.pass).unwrap_or(Pass::In),
                                params
                                    .get(i)
                                    .map(|p| p.projections.clone())
                                    .unwrap_or_default(),
                            )
                        })
                        .collect();
                    self.emit(
                        Op::Call {
                            callee: name,
                            args: actual,
                            types: types.clone(),
                            capability,
                        },
                        e,
                        true,
                    )
                } else {
                    let callable = self.expr(callee);
                    let modes = if let Some(Type::Function { params, .. }) =
                        self.types.get(&(callee.span.start, callee.span.end))
                    {
                        params.iter().map(|(m, _)| *m).collect::<Vec<_>>()
                    } else {
                        vec![]
                    };
                    let actual = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            self.argument(&a.expr, modes.get(i).copied().unwrap_or(a.pass), vec![])
                        })
                        .collect();
                    self.emit(
                        Op::Indirect {
                            callee: callable,
                            args: actual,
                        },
                        e,
                        true,
                    )
                }
            }
            ExprKind::Try(v) => {
                let v = self.expr(v);
                self.emit(Op::Try(v), e, true)
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.expr(condition);
                let yes = self.new_block();
                let no = self.new_block();
                let join = self.new_block();
                let output = self.fresh();
                self.values.insert(
                    output,
                    crate::numeric::default_type(
                        self.types
                            .get(&(e.span.start, e.span.end))
                            .unwrap_or(&Type::Infer),
                    ),
                );
                self.blocks[join].params.push(output);
                self.blocks[self.current].terminator = Terminator::Branch { condition, yes, no };
                self.current = yes;
                let a = self.expr(then);
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: join,
                    args: vec![a],
                };
                self.current = no;
                let b = if let Some(other) = otherwise {
                    self.expr(other)
                } else {
                    self.emit(Op::Unit, e, false)
                };
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: join,
                    args: vec![b],
                };
                self.current = join;
                output
            }
            ExprKind::Match { value, arms } => {
                let value = self.expr(value);
                let join = self.new_block();
                let out = self.fresh();
                self.values.insert(
                    out,
                    crate::numeric::default_type(
                        self.types
                            .get(&(e.span.start, e.span.end))
                            .unwrap_or(&Type::Infer),
                    ),
                );
                self.blocks[join].params.push(out);
                let branches = arms
                    .iter()
                    .map(|(p, _)| (p.clone(), self.new_block()))
                    .collect::<Vec<_>>();
                self.blocks[self.current].terminator = Terminator::Match {
                    value,
                    arms: branches.clone(),
                };
                let outer = self.names.clone();
                for ((p, body), (_, b)) in arms.iter().zip(branches) {
                    self.current = b;
                    self.names = outer.clone();
                    let pattern = self.rename_pattern(p, &self.values[&value].clone());
                    self.emit(
                        Op::Bind { pattern, value },
                        &Expr {
                            kind: ExprKind::Tuple(vec![]),
                            span: arms[0].1.span.clone(),
                        },
                        false,
                    );
                    let v = self.expr(body);
                    self.blocks[self.current].terminator = Terminator::Jump {
                        block: join,
                        args: vec![v],
                    };
                }
                self.current = join;
                self.names = outer;
                out
            }
            ExprKind::Closure { .. } => self.emit(
                Op::Closure {
                    expr: e.clone(),
                    aliases: self.names.clone(),
                    moves: self
                        .world
                        .moves
                        .get(&(e.span.start, e.span.end))
                        .into_iter()
                        .flatten()
                        .filter_map(|p| {
                            self.names.get(&p.root).map(|root| Place {
                                root: root.clone(),
                                path: p.path.clone(),
                            })
                        })
                        .collect(),
                },
                e,
                false,
            ),
            ExprKind::Handle { .. } => self.emit(
                Op::Handle {
                    state_value: None,
                    expr: e.clone(),
                    aliases: self.names.clone(),
                    moves: self
                        .world
                        .moves
                        .get(&(e.span.start, e.span.end))
                        .into_iter()
                        .flatten()
                        .filter_map(|p| {
                            self.names.get(&p.root).map(|root| Place {
                                root: root.clone(),
                                path: p.path.clone(),
                            })
                        })
                        .collect(),
                },
                e,
                true,
            ),
        }
    }
}

pub fn nested_name(kind: &str, span: &Span) -> String {
    format!("${kind}_{}_{}", span.start, span.end)
}
fn pattern_params(world: &World, p: &Pattern, ty: &Type, pass: Pass, out: &mut Vec<Param>) {
    match &p.kind {
        PatternKind::Bind(name) => out.push(Param {
            name: name.clone(),
            ty: ty.clone(),
            pass,
            projections: vec![],
            span: p.span.clone(),
        }),
        PatternKind::Tuple(ps) => {
            if let Type::Tuple(ts) = ty {
                for (p, t) in ps.iter().zip(ts) {
                    pattern_params(world, p, t, pass, out);
                }
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            if let Some((a, v)) = world.constructor(name) {
                let args = if let Type::Named(_, ts) = ty {
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
                        pattern_params(world, p, &substitute(&f.ty, &vars), pass, out);
                    }
                }
            }
        }
        _ => {}
    }
}
