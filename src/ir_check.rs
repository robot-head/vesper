//! Ownership abstract interpretation over basic-block Place IR. Unlike a
//! binding-only check, this accounts for linear temporaries on evaluation edges.
use crate::{
    ast::*,
    check::{Place, World},
    ir::{self, FunctionIr, Instruction, Op, PlaceGraph, Terminator, ValueId},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotState {
    ty: Type,
    pass: Pass,
    moved: BTreeSet<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ValueState {
    ty: Type,
    owned: bool,
    consumed: bool,
    loan: Option<(Vec<Place>, Pass)>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    slots: BTreeMap<String, SlotState>,
    values: BTreeMap<ValueId, ValueState>,
}
pub fn validate(world: &World, program: &ir::Program) -> Vec<Diagnostic> {
    let mut diagnostics = evidence(program);
    for f in program.functions.values() {
        let mut checker = Flow {
            world,
            bounds: f.bounds.clone(),
            external_captures: f.external_captures.clone(),
            graph: PlaceGraph::default(),
            diagnostics: vec![],
        };
        checker.function(f);
        diagnostics.extend(checker.diagnostics);
    }
    let mut seen = BTreeSet::new();
    diagnostics
        .retain(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())));
    diagnostics
}
/// Verify state-token SSA independently of ownership. A branch's output is
/// supplied to each successor's token phi; no token from a sibling path can be
/// used as if it were the current state.
pub fn evidence(program: &ir::Program) -> Vec<Diagnostic> {
    let mut errors = vec![];
    for f in program.functions.values() {
        let mut definitions = BTreeSet::new();
        let mut incoming = vec![vec![]; f.blocks.len()];
        for b in &f.blocks {
            if !definitions.insert(b.entry_token) {
                errors.push(Diagnostic::new(
                    "E0411",
                    "duplicate state-token parameter",
                    0..0,
                ));
            }
        }
        for (source, b) in f.blocks.iter().enumerate() {
            let mut current = b.entry_token;
            for i in &b.instructions {
                let action = crate::ir::state_action(i);
                let read = matches!(i.op, Op::Read { .. });
                if action || read {
                    if let Some((input, output)) = i.token {
                        if input != current
                            || (action && output == input)
                            || (read && input != output)
                        {
                            errors.push(Diagnostic::new(
                                "E0411",
                                "instruction uses stale or invalid state-token evidence",
                                i.span.clone(),
                            ));
                        }
                        if action && !definitions.insert(output) {
                            errors.push(Diagnostic::new(
                                "E0411",
                                "state token is defined twice",
                                i.span.clone(),
                            ));
                        }
                        current = output;
                    } else {
                        errors.push(Diagnostic::new(
                            "E0411",
                            "stateful instruction lacks evidence",
                            i.span.clone(),
                        ));
                    }
                } else if i.token.is_some() {
                    errors.push(Diagnostic::new(
                        "E0411",
                        "pure instruction unexpectedly defines state evidence",
                        i.span.clone(),
                    ));
                }
            }
            if current != b.outgoing_token {
                errors.push(Diagnostic::new(
                    "E0411",
                    "block returns stale state evidence",
                    0..0,
                ));
            }
            let targets = match &b.terminator {
                Terminator::Jump { block, .. } => vec![*block],
                Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
                _ => vec![],
            };
            for target in targets {
                if let Some(edges) = incoming.get_mut(target) {
                    edges.push((source, current));
                } else {
                    errors.push(Diagnostic::new(
                        "E0411",
                        "control-flow edge targets a missing block",
                        0..0,
                    ));
                }
            }
        }
        for (id, b) in f.blocks.iter().enumerate() {
            let mut actual = b.incoming_tokens.clone();
            actual.sort();
            incoming[id].sort();
            if actual != incoming[id] {
                errors.push(Diagnostic::new(
                    "E0411",
                    "state-token phi does not match predecessor evidence",
                    0..0,
                ));
            }
        }
    }
    errors
}
struct Flow<'a> {
    world: &'a World,
    bounds: BTreeMap<String, Mode>,
    external_captures: BTreeSet<String>,
    graph: PlaceGraph,
    diagnostics: Vec<Diagnostic>,
}
impl Flow<'_> {
    fn mode(&self, t: &Type) -> Mode {
        self.world.mode(t, &self.bounds)
    }
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::new("E0410", message, span));
    }
    fn consume(&mut self, s: &mut State, id: ValueId, span: &Span) {
        let Some(v) = s.values.get_mut(&id) else {
            self.error(format!("IR uses undefined value %{id}"), span.clone());
            return;
        };
        if self.mode(&v.ty) == Mode::Copy {
            return;
        }
        if !v.owned {
            self.error(
                "a borrowed non-copy temporary cannot be moved into an owned value",
                span.clone(),
            );
        }
        if v.consumed {
            self.error(
                format!("IR temporary %{id} is consumed more than once"),
                span.clone(),
            );
        }
        v.consumed = true;
        v.loan = None;
    }
    fn access(&mut self, s: &mut State, p: &Place, pass: Pass, span: &Span) {
        self.graph.insert(p);
        let Some(slot) = s.slots.get(&p.root).cloned() else {
            self.error(
                format!("IR uses undefined place '{}'", p.display()),
                span.clone(),
            );
            return;
        };
        if slot
            .moved
            .iter()
            .any(|q| q.starts_with(&p.path) || p.path.starts_with(q))
        {
            self.error(
                format!("IR uses moved place '{}'", p.display()),
                span.clone(),
            );
        }
        let ty = self.world.field(&slot.ty, &p.path).unwrap_or(Type::Infer);
        let exclusive = pass == Pass::Inout || (pass == Pass::Sink && self.mode(&ty) != Mode::Copy);
        for value in s.values.values() {
            if let Some((loan, mode)) = &value.loan
                && !value.consumed
                && loan.iter().any(|loan| self.graph.overlaps(p, loan))
                && (exclusive || *mode == Pass::Inout)
            {
                self.error(
                    format!(
                        "IR access to '{}' overlaps active loan '{}'",
                        p.display(),
                        loan.iter()
                            .map(Place::display)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    span.clone(),
                );
            }
        }
        if pass == Pass::Inout && slot.pass == Pass::In {
            self.error("IR mutates a shared parameter", span.clone());
        }
        if pass == Pass::Sink && self.mode(&ty) != Mode::Copy {
            if slot.pass != Pass::Sink {
                self.error("IR moves from a borrowed parameter", span.clone());
            }
            if !p.path.is_empty() && self.mode(&slot.ty) == Mode::Linear {
                self.error(
                    "IR must destructure a linear aggregate before moving fields",
                    span.clone(),
                );
            }
            s.slots
                .get_mut(&p.root)
                .unwrap()
                .moved
                .insert(p.path.clone());
        }
    }
    fn bind(&mut self, s: &mut State, p: &Pattern, t: &Type) {
        match &p.kind {
            PatternKind::Bind(n) => {
                s.slots.insert(
                    n.clone(),
                    SlotState {
                        ty: t.clone(),
                        pass: Pass::Sink,
                        moved: BTreeSet::new(),
                    },
                );
            }
            PatternKind::Wild => {
                if self.mode(t) == Mode::Linear {
                    self.error("IR wildcard discards a linear value", p.span.clone());
                }
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = t {
                    for (p, t) in ps.iter().zip(ts) {
                        self.bind(s, p, t);
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let args = if let Type::Named(_, args) = t {
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
                            self.bind(s, p, &substitute(&f.ty, &vars));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn frame(&mut self, s: &State, span: &Span) {
        for (n, v) in &s.slots {
            if v.pass == Pass::Sink
                && self.mode(&v.ty) == Mode::Linear
                && !v.moved.contains(&vec![])
            {
                self.error(
                    format!("abortable effect may abandon linear place '{n}'"),
                    span.clone(),
                );
            }
        }
        for (id, v) in &s.values {
            if v.owned && !v.consumed && self.mode(&v.ty) == Mode::Linear {
                self.error(
                    format!("abortable effect may abandon linear temporary %{id}"),
                    span.clone(),
                );
            }
            if !v.consumed && v.loan.as_ref().is_some_and(|(_, p)| *p == Pass::Inout) {
                self.error(
                    "abortable effect crosses a mutable temporary loan",
                    span.clone(),
                );
            }
        }
    }
    fn end(&mut self, s: &State, span: &Span) {
        for (n, v) in &s.slots {
            if v.pass == Pass::Sink
                && self.mode(&v.ty) == Mode::Linear
                && !v.moved.contains(&vec![])
                && !self.external_captures.contains(n)
            {
                self.error(
                    format!("linear place '{n}' remains live at IR return"),
                    span.clone(),
                );
            }
        }
        for (id, v) in &s.values {
            if v.owned && !v.consumed && self.mode(&v.ty) == Mode::Linear {
                self.error(
                    format!("linear temporary %{id} remains live at IR return"),
                    span.clone(),
                );
            }
        }
    }
    fn instruction(&mut self, s: &mut State, i: &Instruction) {
        let mut owned = true;
        let mut loan = None;
        match &i.op {
            Op::Read {
                root,
                path,
                pass,
                projections,
            } => {
                let p = Place {
                    root: root.clone(),
                    path: path.clone(),
                };
                let places = if projections.is_empty() {
                    vec![p]
                } else {
                    projections
                        .iter()
                        .map(|fields| {
                            let mut p = p.clone();
                            p.path.extend(fields.clone());
                            p
                        })
                        .collect::<Vec<_>>()
                };
                if s.slots.contains_key(root) || !self.world.functions.contains_key(root) {
                    for p in &places {
                        self.access(s, p, *pass, &i.span);
                    }
                }
                if *pass != Pass::Sink {
                    owned = false;
                    loan = Some((places, *pass));
                }
            }
            Op::Bind { pattern, value } => {
                let t = s
                    .values
                    .get(value)
                    .map(|v| v.ty.clone())
                    .unwrap_or(Type::Infer);
                self.consume(s, *value, &i.span);
                self.bind(s, pattern, &t);
            }
            Op::Write { place, value, .. } => {
                self.consume(s, *value, &i.span);
                let mut path = place.split('.');
                let p = Place {
                    root: path.next().unwrap_or("").into(),
                    path: path.map(str::to_owned).collect(),
                };
                self.access(s, &p, Pass::Inout, &i.span);
                if let Some(slot) = s.slots.get(&p.root) {
                    let t = self.world.field(&slot.ty, &p.path).unwrap_or(Type::Infer);
                    if self.mode(&t) == Mode::Linear {
                        self.error(
                            "IR assignment overwrites a linear obligation",
                            i.span.clone(),
                        );
                    }
                }
            }
            Op::Unary(_, v) => self.consume(s, *v, &i.span),
            Op::Binary(_, a, b) => {
                self.consume(s, *a, &i.span);
                self.consume(s, *b, &i.span);
            }
            Op::Tuple(vs) => {
                for v in vs {
                    self.consume(s, *v, &i.span)
                }
            }
            Op::Data(_, fs) => {
                for (_, v) in fs {
                    self.consume(s, *v, &i.span)
                }
            }
            Op::Project(v, _) => {
                if s.values
                    .get(v)
                    .is_some_and(|v| self.mode(&v.ty) == Mode::Linear)
                {
                    self.error(
                        "projection would abandon a temporary linear aggregate",
                        i.span.clone(),
                    );
                }
                self.consume(s, *v, &i.span);
            }
            Op::Index(v, n) => {
                self.consume(s, *v, &i.span);
                self.consume(s, *n, &i.span);
            }
            Op::Call { callee, args, .. } => {
                for a in args {
                    if a.pass == Pass::Sink {
                        self.consume(s, a.value, &i.span);
                    } else if let Some(v) = s.values.get_mut(&a.value) {
                        v.consumed = true;
                        v.loan = None;
                    }
                }
                let effectful = if let Some(f) = self.world.functions.get(callee) {
                    let vars = self
                        .world
                        .calls
                        .get(&(i.span.start, i.span.end))
                        .cloned()
                        .unwrap_or_default();
                    let row = substitute_effects(&f.effects, &vars);
                    row.tail.is_some()
                        || row
                            .instances
                            .iter()
                            .any(|e| self.world.effects.get(&e.name).is_some_and(|d| !d.linear))
                } else {
                    callee
                        .split_once("::")
                        .and_then(|(e, _)| self.world.effects.get(e))
                        .is_some_and(|e| !e.linear)
                };
                if effectful {
                    self.frame(s, &i.span);
                }
            }
            Op::Indirect { callee, args } => {
                let row = s.values.get(callee).and_then(|v| {
                    if let Type::Function { effects, .. } = &v.ty {
                        Some(effects.clone())
                    } else {
                        None
                    }
                });
                self.consume(s, *callee, &i.span);
                for a in args {
                    if a.pass == Pass::Sink {
                        self.consume(s, a.value, &i.span);
                    } else if let Some(v) = s.values.get_mut(&a.value) {
                        v.consumed = true;
                        v.loan = None;
                    }
                }
                if row.is_some_and(|r| {
                    r.tail.is_some()
                        || r.instances
                            .iter()
                            .any(|e| self.world.effects.get(&e.name).is_some_and(|d| !d.linear))
                }) {
                    self.frame(s, &i.span);
                }
            }
            Op::Try(v) => {
                self.consume(s, *v, &i.span);
                self.frame(s, &i.span);
            }
            Op::Closure { moves, .. } | Op::Handle { moves, .. } => {
                for p in moves {
                    self.access(s, p, Pass::Sink, &i.span);
                }
            }
            Op::Constant(_) | Op::Unit => {}
        }
        // Primitive failures (RuntimeError and ArithOverflow) abort like Fail.
        if i.fallible
            || self.world.partial_ops.contains(&(i.span.start, i.span.end))
            || self
                .world
                .overflow_ops
                .contains(&(i.span.start, i.span.end))
        {
            self.frame(s, &i.span);
        }
        if let Op::Handle {
            state_value: Some(value),
            ..
        } = &i.op
        {
            self.consume(s, *value, &i.span);
        }
        if let Op::Handle { expr, .. } = &i.op
            && self
                .world
                .handler_effects
                .get(&(expr.span.start, expr.span.end))
                .is_some_and(|row| {
                    row.tail.is_some()
                        || row
                            .instances
                            .iter()
                            .any(|e| self.world.effects.get(&e.name).is_some_and(|e| !e.linear))
                })
        {
            self.frame(s, &i.span);
        }
        // Bind/Write have Unit results regardless of the expression used for their ranges.
        let ty = if matches!(i.op, Op::Bind { .. } | Op::Write { .. }) {
            Type::unit()
        } else {
            i.ty.clone()
        };
        s.values.insert(
            i.dest,
            ValueState {
                ty,
                owned,
                consumed: false,
                loan,
            },
        );
    }
    fn merge(&mut self, a: &State, b: &State, span: &Span) -> State {
        let mut out = a.clone();
        for (n, slot) in &b.slots {
            if let Some(old) = out.slots.get_mut(n) {
                let x = self.mode(&old.ty) == Mode::Linear
                    && old.pass == Pass::Sink
                    && !old.moved.contains(&vec![]);
                let y = self.mode(&slot.ty) == Mode::Linear
                    && slot.pass == Pass::Sink
                    && !slot.moved.contains(&vec![]);
                if x != y {
                    self.error(
                        format!("linear place '{n}' has inconsistent ownership at a CFG join"),
                        span.clone(),
                    );
                }
                old.moved.extend(slot.moved.clone());
            } else {
                if self.mode(&slot.ty) == Mode::Linear
                    && slot.pass == Pass::Sink
                    && !slot.moved.contains(&vec![])
                {
                    self.error(
                        format!("linear place '{n}' survives only one CFG edge"),
                        span.clone(),
                    );
                }
                out.slots.insert(n.clone(), slot.clone());
            }
        }
        for (id, value) in &b.values {
            if let Some(old) = out.values.get_mut(id) {
                if self.mode(&old.ty) == Mode::Linear && old.owned && old.consumed != value.consumed
                {
                    self.error(
                        format!("linear temporary %{id} has inconsistent ownership at a CFG join"),
                        span.clone(),
                    );
                }
                old.consumed |= value.consumed;
                if old.consumed {
                    old.loan = None;
                }
            } else {
                out.values.insert(*id, value.clone());
            }
        }
        out
    }
    fn function(&mut self, f: &FunctionIr) {
        if f.blocks.is_empty() {
            return;
        }
        let mut initial = State::default();
        for p in &f.params {
            initial.slots.insert(
                p.name.clone(),
                SlotState {
                    ty: p.ty.clone(),
                    pass: p.pass,
                    moved: BTreeSet::new(),
                },
            );
        }
        let mut incoming = BTreeMap::from([(0, initial)]);
        let mut queue = VecDeque::from([0]);
        let mut visits = 0;
        while let Some(block) = queue.pop_front() {
            visits += 1;
            if visits > f.blocks.len().saturating_mul(32).max(32) {
                self.error("IR ownership dataflow did not converge", 0..0);
                break;
            }
            let mut state = incoming[&block].clone();
            let b = &f.blocks[block];
            let mut diverges = false;
            for i in &b.instructions {
                self.instruction(&mut state, i);
                if i.ty.name() == "Never" {
                    diverges = true;
                    break;
                }
            }
            if diverges {
                continue;
            }
            let span = b
                .instructions
                .last()
                .map(|i| i.span.clone())
                .unwrap_or(0..0);
            let edges = match &b.terminator {
                Terminator::Return(v) => {
                    self.consume(&mut state, *v, &span);
                    self.end(&state, &span);
                    vec![]
                }
                Terminator::Jump { block, args } => vec![(*block, args.clone())],
                Terminator::Branch { yes, no, .. } => vec![(*yes, vec![]), (*no, vec![])],
                Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| (*b, vec![])).collect(),
                Terminator::Unreachable => vec![],
            };
            for (target, args) in edges {
                let mut edge = state.clone();
                for (arg, param) in args.iter().zip(&f.blocks[target].params) {
                    self.consume(&mut edge, *arg, &span);
                    edge.values.insert(
                        *param,
                        ValueState {
                            ty: f.values[param].clone(),
                            owned: true,
                            consumed: false,
                            loan: None,
                        },
                    );
                }
                let next = if let Some(old) = incoming.get(&target) {
                    self.merge(old, &edge, &span)
                } else {
                    edge
                };
                if incoming.get(&target) != Some(&next) {
                    incoming.insert(target, next);
                    if !queue.contains(&target) {
                        queue.push_back(target);
                    }
                }
            }
        }
    }
}
