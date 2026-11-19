//! Integer literals typed from their use (P3b gaps lane 7, #1092; manuscript
//! ch. 3, "Integer literal typing").
//!
//! An unsuffixed integer literal already takes its kind from its context
//! (an annotation, a parameter, the other operand, the other branch). This
//! module extends that to the bindings a literal initializes: a `let`
//! without an annotation whose value is an unsuffixed literal (or a tuple,
//! `if` or `match` of them), and a `for` pattern over a range of them, takes
//! the kind of the binding's uses later in the function, as if the program
//! had annotated it. It stays `i64` (the literal default) when no use
//! constrains it, or when its uses disagree.
//!
//! The checker runs a function as before first. Only a function that fails
//! and has such an *open* binding is checked again: probe runs, in which a
//! use of an open binding reads as an integer literal, record the kind each
//! use settles to; the open bindings with exactly one non-default kind take
//! it as an annotation, and the function is checked once more. A program
//! that checks without the rule therefore keeps its types and meaning, and
//! the rule never turns a checking program into a failing one.
use super::{Checker, Env};
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};

/// How many probe runs may refine the hints (a binding initialized from
/// another open binding needs one more run per link of the chain).
const PROBE_ROUNDS: usize = 4;

/// The open-binding bookkeeping of the function being checked.
#[derive(Debug, Clone, Default)]
pub(super) struct LiteralTyping {
    /// Pattern spans of the bindings whose type defaulted from a literal.
    open: BTreeSet<(usize, usize)>,
    /// Uses of open bindings: the use's span and the binding's span.
    uses: Vec<((usize, usize), (usize, usize))>,
    /// The kinds open bindings take from their uses, by pattern span.
    hints: BTreeMap<(usize, usize), Type>,
    /// In a probe run, a use of an open binding reads as a literal.
    probe: bool,
}

fn key(span: &Span) -> (usize, usize) {
    (span.start, span.end)
}

impl Checker<'_> {
    /// Checks `f` with the rule of the [module docs](self).
    pub(super) fn function_with_literal_typing(&mut self, f: &Function) {
        let diagnostics = self.diagnostics.len();
        let observations = self.observations.len();
        self.literals = LiteralTyping::default();
        self.function_body(f);
        if self.diagnostics.len() == diagnostics || self.literals.open.is_empty() {
            self.literals = LiteralTyping::default();
            return;
        }
        // The state before `f` (only failing functions pay for the copy):
        // what this run added is keyed by spans in `f` or appended after the
        // marks.
        let base = {
            let mut base = self.clone();
            base.diagnostics.truncate(diagnostics);
            base.observations.truncate(observations);
            let inside = |k: &(usize, usize)| f.span.start <= k.0 && k.1 <= f.span.end;
            base.types.retain(|k, _| !inside(k));
            base.calls.retain(|k, _| !inside(k));
            base.moves.retain(|k, _| !inside(k));
            base.handler_effects.retain(|k, _| !inside(k));
            base.partial_ops.retain(|k| !inside(k));
            base.overflow_ops.retain(|k| !inside(k));
            base.slice_args.retain(|k, _| !inside(k));
            base.resolved.retain(|k, _| !inside(k));
            base
        };
        let mut hints = BTreeMap::new();
        for _ in 0..PROBE_ROUNDS {
            let mut probe = base.clone();
            probe.literals = LiteralTyping {
                hints: hints.clone(),
                probe: true,
                ..LiteralTyping::default()
            };
            probe.function_body(f);
            let found = probe.literals.settled_kinds(&probe.types);
            let mut next = hints.clone();
            for (binding, kind) in found {
                next.entry(binding).or_insert(kind);
            }
            if next == hints {
                break;
            }
            hints = next;
        }
        if hints.is_empty() {
            self.literals = LiteralTyping::default();
            return;
        }
        let mut hinted = base;
        hinted.literals = LiteralTyping {
            hints,
            ..LiteralTyping::default()
        };
        hinted.function_body(f);
        if hinted.diagnostics.len() < self.diagnostics.len() {
            *self = hinted;
        }
        self.literals = LiteralTyping::default();
    }

    /// The annotation the hints give an unannotated `let` pattern, if any.
    pub(super) fn literal_annotation(&self, pat: &Pattern) -> Option<Type> {
        if self.literals.hints.is_empty() {
            return None;
        }
        fn go(hints: &BTreeMap<(usize, usize), Type>, p: &Pattern) -> Option<Type> {
            match &p.kind {
                PatternKind::Bind(_) => hints.get(&key(&p.span)).cloned(),
                PatternKind::Tuple(ps) => {
                    let parts: Vec<Option<Type>> = ps.iter().map(|p| go(hints, p)).collect();
                    parts.iter().any(Option::is_some).then(|| {
                        Type::Tuple(
                            parts
                                .into_iter()
                                .map(|t| t.unwrap_or(Type::Infer))
                                .collect(),
                        )
                    })
                }
                _ => None,
            }
        }
        go(&self.literals.hints, pat)
    }

    /// The hinted kind of a `for` pattern binding, if any.
    pub(super) fn literal_element(&self, pat: &Pattern) -> Option<Type> {
        match &pat.kind {
            PatternKind::Bind(_) => self.literals.hints.get(&key(&pat.span)).cloned(),
            _ => None,
        }
    }

    /// Records the bindings of `pat` whose part of `actual` (the value's
    /// type before defaulting) is an unsuffixed literal.
    pub(super) fn open_literal_bindings(&mut self, pat: &Pattern, actual: &Type) {
        match (&pat.kind, actual) {
            (PatternKind::Bind(_), Type::IntLiteral(_)) => {
                self.literals.open.insert(key(&pat.span));
            }
            (PatternKind::Tuple(ps), Type::Tuple(ts)) => {
                for (p, t) in ps.iter().zip(ts) {
                    self.open_literal_bindings(p, t);
                }
            }
            _ => {}
        }
    }

    /// In a probe run, the type a use of binding `name` reads as: an integer
    /// literal (or the expected integer kind) for an open binding.
    pub(super) fn literal_use(
        &mut self,
        name: &str,
        use_span: &Span,
        env: &Env,
        expected: &Type,
        ty: Type,
    ) -> Type {
        if !self.literals.probe {
            return ty;
        }
        let Some(b) = env.bindings.get(name) else {
            return ty;
        };
        let binding = key(&b.span);
        if !self.literals.open.contains(&binding)
            || crate::numeric::IntegerKind::from_type(&ty).is_none()
        {
            return ty;
        }
        self.literals.uses.push((key(use_span), binding));
        if crate::numeric::IntegerKind::from_type(expected).is_some()
            && !matches!(expected, Type::IntLiteral(_))
        {
            expected.clone()
        } else {
            Type::IntLiteral(b.integer.unwrap_or(0))
        }
    }
}

impl Checker<'_> {
    /// The expected type of the value assigned to `place`: in a probe run,
    /// a value assigned to an open binding is one of its uses, typed from
    /// its own operands.
    pub(super) fn literal_assignment(
        &mut self,
        place: &super::Place,
        value: &Expr,
        env: &Env,
        expected: &Type,
    ) -> Type {
        if !self.literals.probe || !place.path.is_empty() {
            return expected.clone();
        }
        let Some(b) = env.bindings.get(&place.root) else {
            return expected.clone();
        };
        let binding = key(&b.span);
        if !self.literals.open.contains(&binding) {
            return expected.clone();
        }
        self.literals.uses.push((key(&value.span), binding));
        Type::Infer
    }

    /// Forgets the use of an open binding at `e`: its context accepts any
    /// kind (a conversion's operand).
    pub(super) fn literal_unconstrained(&mut self, e: &Expr) {
        if self.literals.probe {
            let at = key(&e.span);
            self.literals.uses.retain(|(u, _)| *u != at);
        }
    }
}

impl LiteralTyping {
    /// The one kind other than the default each open binding's uses
    /// settled to in a probe run.
    fn settled_kinds(&self, types: &super::TypeMap) -> BTreeMap<(usize, usize), Type> {
        let default = Type::named("i64");
        let mut kinds: BTreeMap<(usize, usize), Vec<Type>> = BTreeMap::new();
        for (at, binding) in &self.uses {
            if let Some(t) = types.get(at)
                && crate::numeric::IntegerKind::from_type(t).is_some()
                && !matches!(t, Type::IntLiteral(_))
                && *t != default
            {
                let seen = kinds.entry(*binding).or_default();
                if !seen.contains(t) {
                    seen.push(t.clone());
                }
            }
        }
        kinds
            .into_iter()
            .filter(|(_, ks)| ks.len() == 1)
            .filter_map(|(b, ks)| Some((b, ks.into_iter().next()?)))
            .collect()
    }
}
