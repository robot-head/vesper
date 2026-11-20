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
// Diagnostic-only evidence. Never part of State equality, transfer, joins or
// IR serialization. If indexing cannot be completed within these limits, omit
// all optional origins rather than trusting a prefix with unseen duplicates.
const MAX_CAUSE_WORK: usize = 65_536;
const MAX_CAUSE_TEXT_BYTES: usize = 1024 * 1024;
const MAX_CAUSE_NAME_BYTES: usize = 1024;
const MAX_RELATED: usize = 3;
#[derive(Clone)]
struct SlotOrigin {
    span: Span,
    parameter: bool,
}
#[derive(Default)]
struct Origins {
    values: BTreeMap<ValueId, Option<Span>>,
    slots: BTreeMap<String, Option<SlotOrigin>>,
}
impl Origins {
    fn build(f: &FunctionIr) -> Self {
        Self::try_build(f).unwrap_or_default()
    }
    fn try_build(f: &FunctionIr) -> Option<Self> {
        let mut out = Self::default();
        let mut work = MAX_CAUSE_WORK;
        let mut text = MAX_CAUSE_TEXT_BYTES;
        fn charge(work: &mut usize) -> Option<()> {
            *work = work.checked_sub(1)?;
            Some(())
        }
        fn slot(
            out: &mut Origins,
            name: &str,
            span: &Span,
            parameter: bool,
            text: &mut usize,
        ) -> Option<()> {
            if name.len() > MAX_CAUSE_NAME_BYTES {
                return None;
            }
            *text = text.checked_sub(name.len())?;
            match out.slots.entry(name.to_owned()) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert((span.start < span.end).then(|| SlotOrigin {
                        span: span.clone(),
                        parameter,
                    }));
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    e.insert(None);
                }
            }
            Some(())
        }
        fn value(out: &mut Origins, id: ValueId, span: Option<&Span>) {
            match out.values.entry(id) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(span.filter(|s| s.start < s.end).cloned());
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    e.insert(None);
                }
            }
        }
        for p in &f.params {
            charge(&mut work)?;
            slot(&mut out, &p.name, &p.span, true, &mut text)?;
        }
        for block in &f.blocks {
            charge(&mut work)?;
            // Phi/block parameters have no unique instruction source span.
            for id in &block.params {
                charge(&mut work)?;
                value(&mut out, *id, None);
            }
            for i in &block.instructions {
                charge(&mut work)?;
                value(&mut out, i.dest, Some(&i.span));
                if let Op::Bind { pattern, .. } = &i.op {
                    let mut pending = vec![pattern];
                    while let Some(p) = pending.pop() {
                        charge(&mut work)?;
                        match &p.kind {
                            PatternKind::Bind(name) => {
                                slot(&mut out, name, &p.span, false, &mut text)?
                            }
                            PatternKind::Tuple(ps) => {
                                for child in ps {
                                    charge(&mut work)?;
                                    pending.push(child);
                                }
                            }
                            PatternKind::Constructor { fields, .. } => {
                                for (_, child) in fields {
                                    charge(&mut work)?;
                                    pending.push(child);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Some(out)
    }
    fn value(&self, id: ValueId) -> Option<(Span, &'static str)> {
        self.values
            .get(&id)?
            .as_ref()
            .map(|span| (span.clone(), "this IR temporary is defined here"))
    }
    fn slot(&self, name: &str) -> Option<(Span, &'static str)> {
        self.slots.get(name)?.as_ref().map(|origin| {
            (
                origin.span.clone(),
                if origin.parameter {
                    "source span for this IR parameter"
                } else {
                    "this IR binding is introduced here"
                },
            )
        })
    }
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
            origins: Origins::build(f),
            local_consumes: BTreeMap::new(),
            reservable: reservable(f),
            scrutinees: scrutinees(f),
            views: element_views(world, f),
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
    origins: Origins,
    // A witness is local to one straight-line block visit, never propagated
    // through a join or guessed from the last static assignment/consumption.
    local_consumes: BTreeMap<ValueId, Span>,
    /// The `inout` loans that a call takes as an argument ([`reservable`]).
    reservable: BTreeSet<ValueId>,
    /// The values that a `match` terminator inspects ([`scrutinees`]).
    scrutinees: BTreeSet<ValueId>,
    /// The `try_get` values of element view matches ([`element_views`]).
    views: BTreeMap<ValueId, Pass>,
}
/// The `try_get`/`try_get_mut` values of `f` that element view matches
/// (§13) inspect, with how their `Some(x)` binds the element: an `in` or
/// `inout` borrow its sequence keeps owning.
fn element_views(world: &World, f: &FunctionIr) -> BTreeMap<ValueId, Pass> {
    if world.element_views.is_empty() {
        return BTreeMap::new();
    }
    f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i.op, Op::Seq { .. }))
        .filter_map(|i| {
            world
                .element_views
                .get(&(i.span.start, i.span.end))
                .map(|pass| (i.dest, *pass))
        })
        .collect()
}
/// The values `f` matches on. A by-value read of a borrowed slot's
/// non-copy place that only a `match` inspects is a borrow (§4.3): the
/// match binds `in` borrows of its payloads.
fn scrutinees(f: &FunctionIr) -> BTreeSet<ValueId> {
    f.blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            crate::ir::Terminator::Match { value, .. } => Some(*value),
            _ => None,
        })
        .collect()
}
/// The values that some call, sequence, tiled or `Box` operation of `f`
/// takes as an `inout` argument. Only their loans are reserved before the
/// call (two-phase borrows); an `inout` loan that no call consumes is
/// active from the start.
fn reservable(f: &FunctionIr) -> BTreeSet<ValueId> {
    let mut out = BTreeSet::new();
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        let args = match &i.op {
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Tiled { args, .. }
            | Op::Seq { args, .. }
            | Op::Fip { args, .. } => args,
            _ => continue,
        };
        out.extend(
            args.iter()
                .filter(|a| a.pass == Pass::Inout)
                .map(|a| a.value),
        );
    }
    out
}
impl Flow<'_> {
    fn mode(&self, t: &Type) -> Mode {
        self.world.mode(t, &self.bounds)
    }
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::new("E0410", message, span));
    }
    fn caused(
        &mut self,
        message: impl Into<String>,
        span: Span,
        causes: impl IntoIterator<Item = (Span, &'static str)>,
        help: &'static str,
    ) {
        let mut diagnostic = Diagnostic::new("E0410", message, span);
        for (span, message) in causes {
            if diagnostic.related.len() == MAX_RELATED {
                break;
            }
            if span.start >= span.end
                || span == diagnostic.span
                || diagnostic.related.iter().any(|label| label.span == span)
            {
                continue;
            }
            diagnostic = diagnostic.with_related(span, message);
        }
        if !diagnostic.related.is_empty() {
            diagnostic = diagnostic.with_help(help);
        }
        self.diagnostics.push(diagnostic);
    }
    fn note_consume(&mut self, id: ValueId, span: &Span) {
        if span.start < span.end && self.origins.values.get(&id).is_some_and(Option::is_some) {
            // Unique origins already cap this map by MAX_CAUSE_WORK entries.
            self.local_consumes
                .entry(id)
                .or_insert_with(|| span.clone());
        }
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
            self.caused(
                "a borrowed non-copy temporary cannot be moved into an owned value",
                span.clone(),
                self.origins.value(id),
                "A borrowed non-copy temporary cannot transfer ownership to an owned value.",
            );
        }
        if v.consumed {
            let prior = self
                .local_consumes
                .get(&id)
                .cloned()
                .map(|span| (span, "an earlier consuming use in this block is here"));
            self.caused(
                format!("IR temporary %{id} is consumed more than once"),
                span.clone(),
                self.origins.value(id).into_iter().chain(prior),
                "A non-copy IR temporary may be consumed only once on an execution path.",
            );
        }
        if v.owned && !v.consumed {
            self.note_consume(id, span);
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
            self.caused(
                format!("IR uses moved place '{}'", p.display()),
                span.clone(),
                self.origins.slot(&p.root),
                "A moved place cannot be used again until ownership is re-established.",
            );
        }
        let ty = self.world.field(&slot.ty, &p.path).unwrap_or(Type::Infer);
        let exclusive = pass == Pass::Inout || (pass == Pass::Sink && self.mode(&ty) != Mode::Copy);
        // Two-phase borrows (§4.3): an `inout` argument loan is only
        // reserved until the call that consumes it activates it
        // ([`Flow::activate`]), so while the call's arguments are evaluated
        // it admits shared reads of its place and conflicts only with
        // writes, moves and `inout` loans.
        for (id, value) in &s.values {
            if let Some((loan, mode)) = &value.loan
                && !value.consumed
                && loan.iter().any(|loan| self.graph.overlaps(p, loan))
                && (exclusive || (*mode == Pass::Inout && !self.reservable.contains(id)))
            {
                self.caused(
                    format!(
                        "IR access to '{}' overlaps active loan '{}'",
                        p.display(),
                        loan.iter()
                            .map(Place::display)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    span.clone(),
                    self.origins.value(*id).map(|(span,_)|(span,"this IR read requests the conflicting access")).into_iter().chain(self.origins.slot(&p.root)),
                    "Overlapping access must respect the shared or exclusive permission of each active loan.",
                );
            }
        }
        if pass == Pass::Inout && slot.pass == Pass::In {
            self.caused(
                "IR mutates a shared parameter",
                span.clone(),
                self.origins.slot(&p.root),
                "Shared parameters do not grant mutation permission.",
            );
        }
        if pass == Pass::Sink && self.mode(&ty) != Mode::Copy {
            if slot.pass != Pass::Sink {
                self.caused(
                    "IR moves from a borrowed parameter",
                    span.clone(),
                    self.origins.slot(&p.root),
                    "Borrowed parameters do not transfer ownership to this function.",
                );
            }
            if !p.path.is_empty() && self.mode(&slot.ty) == Mode::Linear {
                self.caused(
                    "IR must destructure a linear aggregate before moving fields",
                    span.clone(),
                    self.origins.slot(&p.root),
                    "Consume the full linear aggregate through its supported destructuring rules.",
                );
            }
            s.slots
                .get_mut(&p.root)
                .unwrap()
                .moved
                .insert(p.path.clone());
        }
    }
    /// Activates the reserved `inout` loans among a call's `args`. A shared
    /// read made while the arguments were evaluated has been consumed by its
    /// own operation by then, unless it is itself an argument of this call,
    /// as in `s.extend(in s[..])`: such a loan is still live when the callee
    /// runs, so it may not overlap an activated `inout` loan. (An `inout`
    /// loan taken after another is refused when it is taken, in `access`.)
    fn activate(&mut self, s: &State, args: &[ir::Argument], span: &Span) {
        for a in args {
            let Some(ValueState {
                loan: Some((places, Pass::Inout)),
                consumed: false,
                ..
            }) = s.values.get(&a.value)
            else {
                continue;
            };
            for b in args {
                let Some(ValueState {
                    ty,
                    loan: Some((loans, Pass::In)),
                    consumed: false,
                    ..
                }) = s.values.get(&b.value)
                else {
                    continue;
                };
                // A scalar is read into its parameter's word before the call
                // runs (two-phase argument loans, §4.3), so it does not alias
                // the activated `inout` place. Copy aggregates may be lent by
                // address and stay refused.
                if b.pass != Pass::Inout && crate::check::is_scalar_type(ty) {
                    continue;
                }
                if let Some(q) = loans
                    .iter()
                    .find(|q| places.iter().any(|p| self.graph.overlaps(p, q)))
                {
                    self.caused(
                        format!(
                            "IR call activates inout loan '{}' while its argument loan '{}' is live",
                            places
                                .iter()
                                .map(Place::display)
                                .collect::<Vec<_>>()
                                .join(", "),
                            q.display()
                        ),
                        span.clone(),
                        self.origins
                            .value(b.value)
                            .map(|(span, _)| (span, "the overlapping argument loan is taken here")),
                        "A reserved inout loan becomes exclusive when its call runs, so no argument of the same call may borrow its place.",
                    );
                }
            }
        }
    }
    fn bind(&mut self, s: &mut State, p: &Pattern, t: &Type, origin: ValueId, pass: Pass) {
        match &p.kind {
            PatternKind::Bind(n) => {
                s.slots.insert(
                    n.clone(),
                    SlotState {
                        ty: t.clone(),
                        pass,
                        moved: BTreeSet::new(),
                    },
                );
            }
            PatternKind::Wild => {
                if pass == Pass::Sink && self.mode(t) == Mode::Linear {
                    self.caused(
                        "IR wildcard discards a linear value",
                        p.span.clone(),
                        self.origins.value(origin),
                        "Linear components must be consumed rather than discarded by a wildcard.",
                    );
                }
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = t {
                    for (p, t) in ps.iter().zip(ts) {
                        self.bind(s, p, t, origin, pass);
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
                            self.bind(s, p, &substitute(&f.ty, &vars), origin, pass);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn frame(&mut self, s: &State, span: &Span) {
        for (n, v) in &s.slots {
            // A handler body's external captures live in the enclosing
            // frame. An abort that escapes the handler is checked there at
            // the `Handle` instruction; one the handler catches leaves them
            // in place (the checker requires the body not to consume them).
            if v.pass == Pass::Sink
                && self.mode(&v.ty) == Mode::Linear
                && !v.moved.contains(&vec![])
                && !self.external_captures.contains(n)
            {
                self.caused(
                    format!("abortable effect may abandon linear place '{n}'"),
                    span.clone(),
                    self.origins.slot(n),
                    "An abortable operation must not abandon a live owned linear place.",
                );
            }
        }
        for (id, v) in &s.values {
            if v.owned && !v.consumed && self.mode(&v.ty) == Mode::Linear {
                self.caused(
                    format!("abortable effect may abandon linear temporary %{id}"),
                    span.clone(),
                    self.origins.value(*id),
                    "An abortable operation must not abandon a live owned linear temporary.",
                );
            }
            // An `inout` loan may cross an abort (keep-writes, manuscript
            // §4.3 `PostCall_abort`): the loaned place stays in this frame.
        }
    }
    fn end(&mut self, s: &State, span: &Span) {
        for (n, v) in &s.slots {
            if v.pass == Pass::Sink
                && self.mode(&v.ty) == Mode::Linear
                && !v.moved.contains(&vec![])
                && !self.external_captures.contains(n)
            {
                self.caused(
                    format!("linear place '{n}' remains live at IR return"),
                    span.clone(),
                    self.origins.slot(n),
                    "Each owned linear place must be consumed on every returning path.",
                );
            }
        }
        for (id, v) in &s.values {
            if v.owned && !v.consumed && self.mode(&v.ty) == Mode::Linear {
                self.caused(
                    format!("linear temporary %{id} remains live at IR return"),
                    span.clone(),
                    self.origins.value(*id),
                    "Each owned linear temporary must be consumed on every returning path.",
                );
            }
        }
    }
    fn element_view(&self, value: ValueId) -> Option<Pass> {
        self.views.get(&value).copied()
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
                let read_pass = pass;
                let mut pass = *pass;
                if pass == Pass::Sink
                    && self.scrutinees.contains(&i.dest)
                    && s.slots
                        .get(root)
                        .is_some_and(|slot| slot.pass != Pass::Sink)
                    && self.mode(&i.ty) != Mode::Copy
                {
                    pass = Pass::In;
                }
                if s.slots.contains_key(root) || !self.world.functions.contains_key(root) {
                    for p in &places {
                        self.access(s, p, pass, &i.span);
                    }
                }
                if pass != Pass::Sink {
                    owned = false;
                    // A borrowed scrutinee lends its place to the arms'
                    // bindings, which the checker keeps from conflicting
                    // with any write (§4.3); it takes no value loan.
                    if *read_pass != Pass::Sink {
                        loan = Some((places, pass));
                    }
                }
            }
            Op::Bind { pattern, value } => {
                let t = s
                    .values
                    .get(value)
                    .map(|v| v.ty.clone())
                    .unwrap_or(Type::Infer);
                // A borrowed scrutinee binds borrows of its payloads.
                let view = self.element_view(*value);
                let pass = if let Some(pass) = view {
                    pass
                } else if self.scrutinees.contains(value)
                    && s.values.get(value).is_some_and(|v| !v.owned)
                {
                    Pass::In
                } else {
                    self.consume(s, *value, &i.span);
                    Pass::Sink
                };
                self.bind(s, pattern, &t, *value, pass);
            }
            Op::Write { place, op, value } => {
                self.consume(s, *value, &i.span);
                let mut path = place.split('.');
                let p = Place {
                    root: path.next().unwrap_or("").into(),
                    path: path.map(str::to_owned).collect(),
                };
                // A plain assignment re-initializes a place whose old value
                // was moved out (the drop of an overwritten `.vsys` handle
                // moves it into its drop import first, §16.3).
                if op == "="
                    && let Some(slot) = s.slots.get_mut(&p.root)
                {
                    slot.moved.retain(|q| !q.starts_with(&p.path));
                }
                self.access(s, &p, Pass::Inout, &i.span);
                if let Some(slot) = s.slots.get(&p.root) {
                    let t = self.world.field(&slot.ty, &p.path).unwrap_or(Type::Infer);
                    if self.mode(&t) == Mode::Linear {
                        self.caused(
                            "IR assignment overwrites a linear obligation",
                            i.span.clone(),
                            self.origins.slot(&p.root),
                            "Overwriting an owned linear obligation would discard it.",
                        );
                    }
                }
            }
            Op::Unary(_, v) | Op::Exit { value: v, .. } => self.consume(s, *v, &i.span),
            Op::Loop => {}
            Op::Binary(op, a, b) => {
                for v in [a, b] {
                    // A string comparison borrows its operands (§13 strings):
                    // it releases their loans instead of consuming them.
                    if crate::strings::is_comparison(op)
                        && let Some(x) = s.values.get_mut(v)
                        && !x.owned
                        && crate::strings::is_text(&x.ty)
                    {
                        x.consumed = true;
                        x.loan = None;
                    } else {
                        self.consume(s, *v, &i.span);
                    }
                }
            }
            Op::Tuple(vs) => {
                for v in vs {
                    self.consume(s, *v, &i.span)
                }
            }
            Op::Vector { args, .. } => {
                // Operands are `in` reads (lanes are copy, a string is a
                // shared borrow): the operation releases their loans.
                for v in args {
                    if let Some(x) = s.values.get_mut(v) {
                        x.consumed = true;
                        x.loan = None;
                    } else {
                        self.consume(s, *v, &i.span)
                    }
                }
            }
            Op::Tiled { args, .. } | Op::Seq { args, .. } | Op::Fip { args, .. } => {
                // An element view's `try_get` (§13) names an element its
                // sequence keeps owning.
                if matches!(i.op, Op::Seq { .. })
                    && self
                        .world
                        .element_views
                        .contains_key(&(i.span.start, i.span.end))
                {
                    owned = false;
                }
                self.activate(s, args, &i.span);
                for a in args {
                    if a.pass == Pass::Sink {
                        self.consume(s, a.value, &i.span);
                    } else if let Some(v) = s.values.get_mut(&a.value) {
                        v.consumed = true;
                        v.loan = None;
                    }
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
                    self.caused("projection would abandon a temporary linear aggregate", i.span.clone(), self.origins.value(*v), "A linear aggregate must retain all of its ownership obligations when destructured.");
                }
                self.consume(s, *v, &i.span);
            }
            Op::Index(v, n) => {
                self.consume(s, *v, &i.span);
                self.consume(s, *n, &i.span);
            }
            Op::Call { callee, args, .. } => {
                self.activate(s, args, &i.span);
                for a in args {
                    if a.pass == Pass::Sink {
                        self.consume(s, a.value, &i.span);
                    } else if let Some(v) = s.values.get_mut(&a.value) {
                        // Loan release is not an owned-consumption witness.
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
                self.activate(s, args, &i.span);
                for a in args {
                    if a.pass == Pass::Sink {
                        self.consume(s, a.value, &i.span);
                    } else if let Some(v) = s.values.get_mut(&a.value) {
                        // Loan release is not an owned-consumption witness.
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
                    self.caused(format!("linear place '{n}' has inconsistent ownership at a CFG join"), span.clone(), self.origins.slot(n), "Ownership of a linear place must agree on the incoming control-flow edges.");
                }
                old.moved.extend(slot.moved.clone());
            } else {
                if self.mode(&slot.ty) == Mode::Linear
                    && slot.pass == Pass::Sink
                    && !slot.moved.contains(&vec![])
                {
                    self.caused(format!("linear place '{n}' survives only one CFG edge"), span.clone(), self.origins.slot(n), "An owned linear place cannot disappear on another incoming control-flow edge.");
                }
                out.slots.insert(n.clone(), slot.clone());
            }
        }
        for (id, value) in &b.values {
            if let Some(old) = out.values.get_mut(id) {
                if self.mode(&old.ty) == Mode::Linear && old.owned && old.consumed != value.consumed
                {
                    self.caused(format!("linear temporary %{id} has inconsistent ownership at a CFG join"), span.clone(), self.origins.value(*id), "Ownership of a linear temporary must agree on incoming control-flow edges.");
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
            self.local_consumes.clear();
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
