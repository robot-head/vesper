//! Bounded proof production for the actual AST egraph candidate.
//!
//! Search and the forest below are untrusted. The parent must run the separate
//! witness checker with the returned cumulative budget before installing output.
//! Egg explanations stay disabled, including their recursive debug expansions.

use super::witness::{
    Equality, IntOp, ProofId, Reason, RewriteCertificate, Substitution, Term, TermId, TermNode,
    WitnessBudget, WitnessError, WitnessLimits, bind_source, build_candidate,
};
use super::{Graph, Math, rules};
use crate::ast::{Expr, TypeMap};
use crate::numeric::IntegerKind;
use egg::{
    Applier, AstSize, ENodeOrVar, Extractor, Id, Language, PatternAst, RecExpr, Rewrite,
    RewriteScheduler, Runner, SearchMatches, StopReason, Subst, Symbol, Var,
};
use std::sync::{Arc, Mutex};

const RAW_NODES: usize = 2048;
const MATCHES: usize = 128;
const RECORDS: usize = 4096;
const ITERATIONS: usize = 8;
const RULE_COUNT: usize = 18;
// Every catalog RHS has at most two operator nodes. Include both pattern
// instantiations and constants conservatively before entering the applier.
const APPLICATION_NODES: usize = 8;

pub(super) struct RewriteProposal {
    pub(super) candidate: Expr,
    pub(super) certificate: RewriteCertificate,
    pub(super) budget: WitnessBudget,
}

pub(super) enum RewriteOutcome {
    Barrier,
    Unchanged,
    Candidate(RewriteProposal),
}

fn checked_add(a: usize, b: usize) -> Result<usize, WitnessError> {
    a.checked_add(b).ok_or(WitnessError::Limit)
}

fn checked_mul(a: usize, b: usize) -> Result<usize, WitnessError> {
    a.checked_mul(b).ok_or(WitnessError::Limit)
}

fn grow<T>(
    values: &mut Vec<T>,
    max: usize,
    budget: &mut WitnessBudget,
) -> Result<(), WitnessError> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let target = if values.capacity() == 0 {
        max.min(4)
    } else {
        checked_mul(values.capacity(), 2)?.min(max)
    };
    if target <= values.len() {
        return Err(WitnessError::Limit);
    }
    budget.reserve(values, target - values.len())
}

fn initialized<T: Clone>(
    count: usize,
    value: T,
    budget: &mut WitnessBudget,
) -> Result<Vec<T>, WitnessError> {
    let mut values = Vec::new();
    budget.reserve(&mut values, count)?;
    budget.work(count)?;
    values.resize(count, value);
    Ok(values)
}

/// Canonical shallow terms and a proof-carrying union forest. Forest roots are
/// only a producer search aid; every installed edge has an explicit equality.
struct Builder {
    kind: IntegerKind,
    limits: WitnessLimits,
    terms: Vec<Term>,
    proofs: Vec<Equality>,
    parent: Vec<TermId>,
    edge: Vec<Option<ProofId>>,
    rank: Vec<u8>,
    leaf: Vec<Option<TermId>>,
}

impl Builder {
    fn new(kind: IntegerKind, limits: WitnessLimits) -> Self {
        Self {
            kind,
            limits,
            terms: Vec::new(),
            proofs: Vec::new(),
            parent: Vec::new(),
            edge: Vec::new(),
            rank: Vec::new(),
            leaf: Vec::new(),
        }
    }

    fn intern(
        &mut self,
        node: TermNode,
        budget: &mut WitnessBudget,
    ) -> Result<TermId, WitnessError> {
        budget.work(checked_add(self.terms.len(), 1)?)?;
        let term = Term {
            kind: self.kind,
            node,
        };
        if let Some(index) = self.terms.iter().position(|existing| *existing == term) {
            return TermId::try_from(index).map_err(|_| WitnessError::Limit);
        }
        if self.terms.len() >= self.limits.max_terms {
            return Err(WitnessError::Limit);
        }
        if let Some(children) = children(node) {
            if children.iter().any(|id| *id as usize >= self.terms.len()) {
                return Err(WitnessError::NonTopologicalTerm);
            }
        }
        let id = TermId::try_from(self.terms.len()).map_err(|_| WitnessError::Limit)?;
        grow(&mut self.terms, self.limits.max_terms, budget)?;
        grow(&mut self.parent, self.limits.max_terms, budget)?;
        grow(&mut self.edge, self.limits.max_terms, budget)?;
        grow(&mut self.rank, self.limits.max_terms, budget)?;
        grow(&mut self.leaf, self.limits.max_terms, budget)?;
        self.terms.push(term);
        self.parent.push(id);
        self.edge.push(None);
        self.rank.push(0);
        self.leaf.push(is_leaf(node).then_some(id));
        Ok(id)
    }

    fn proof(
        &mut self,
        equality: Equality,
        budget: &mut WitnessBudget,
    ) -> Result<ProofId, WitnessError> {
        budget.work(1)?;
        if self.proofs.len() >= self.limits.max_proofs {
            return Err(WitnessError::Limit);
        }
        grow(&mut self.proofs, self.limits.max_proofs, budget)?;
        let id = ProofId::try_from(self.proofs.len()).map_err(|_| WitnessError::Limit)?;
        self.proofs.push(equality);
        Ok(id)
    }

    fn reflexive(
        &mut self,
        term: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<ProofId, WitnessError> {
        self.proof(
            Equality {
                from: term,
                to: term,
                reason: Reason::Refl,
            },
            budget,
        )
    }

    fn reverse(
        &mut self,
        proof: ProofId,
        budget: &mut WitnessBudget,
    ) -> Result<ProofId, WitnessError> {
        let old = self.proofs[proof as usize];
        self.proof(
            Equality {
                from: old.to,
                to: old.from,
                reason: Reason::Symmetry { proof },
            },
            budget,
        )
    }

    fn transitive(
        &mut self,
        first: ProofId,
        second: ProofId,
        budget: &mut WitnessBudget,
    ) -> Result<ProofId, WitnessError> {
        let a = self.proofs[first as usize];
        let b = self.proofs[second as usize];
        if a.to != b.from {
            return Err(WitnessError::InvalidEquality);
        }
        self.proof(
            Equality {
                from: a.from,
                to: b.to,
                reason: Reason::Transitivity { first, second },
            },
            budget,
        )
    }

    fn root(&self, mut id: TermId, budget: &mut WitnessBudget) -> Result<TermId, WitnessError> {
        for _ in 0..=self.parent.len() {
            budget.work(1)?;
            let next = *self
                .parent
                .get(id as usize)
                .ok_or(WitnessError::InvalidTermIndex)?;
            if next == id {
                return Ok(id);
            }
            id = next;
        }
        Err(WitnessError::InvalidEquality)
    }

    /// Optional ordered proof from a term to its current root. No recursive
    /// flattening or path compression can erase the reasons for a parent edge.
    fn path(
        &mut self,
        mut id: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<Option<ProofId>, WitnessError> {
        let mut path = None;
        for _ in 0..=self.parent.len() {
            budget.work(1)?;
            let next = self.parent[id as usize];
            if next == id {
                return Ok(path);
            }
            let edge = self.edge[id as usize].ok_or(WitnessError::InvalidEquality)?;
            path = Some(match path {
                Some(previous) => self.transitive(previous, edge, budget)?,
                None => edge,
            });
            id = next;
        }
        Err(WitnessError::InvalidEquality)
    }

    fn equality(
        &mut self,
        from: TermId,
        to: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<ProofId, WitnessError> {
        if self.root(from, budget)? != self.root(to, budget)? {
            return Err(WitnessError::InvalidEquality);
        }
        let a = self.path(from, budget)?;
        let b = self.path(to, budget)?;
        match (a, b) {
            (Some(a), Some(b)) => {
                let reversed = self.reverse(b, budget)?;
                self.transitive(a, reversed, budget)
            }
            (Some(a), None) => Ok(a),
            (None, Some(b)) => self.reverse(b, budget),
            (None, None) => self.reflexive(from, budget),
        }
    }

    fn join(&mut self, proof: ProofId, budget: &mut WitnessBudget) -> Result<bool, WitnessError> {
        let equality = self.proofs[proof as usize];
        let a = self.root(equality.from, budget)?;
        let b = self.root(equality.to, budget)?;
        if a == b {
            return Ok(false);
        }
        let mut edge = proof;
        if let Some(path) = self.path(equality.from, budget)? {
            let reversed = self.reverse(path, budget)?;
            edge = self.transitive(reversed, edge, budget)?;
        }
        if let Some(path) = self.path(equality.to, budget)? {
            edge = self.transitive(edge, path, budget)?;
        }
        let (child, parent, edge) = if self.rank[a as usize] <= self.rank[b as usize] {
            if self.rank[a as usize] == self.rank[b as usize] {
                self.rank[b as usize] = self.rank[b as usize]
                    .checked_add(1)
                    .ok_or(WitnessError::Limit)?;
            }
            (a, b, edge)
        } else {
            (b, a, self.reverse(edge, budget)?)
        };
        self.parent[child as usize] = parent;
        self.edge[child as usize] = Some(edge);
        self.leaf[parent as usize] = self.leaf[parent as usize].or(self.leaf[child as usize]);
        Ok(true)
    }

    fn guard(
        &mut self,
        term: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<Option<Option<ProofId>>, WitnessError> {
        if is_leaf(self.terms[term as usize].node) {
            return Ok(Some(None));
        }
        let root = self.root(term, budget)?;
        let Some(leaf) = self.leaf[root as usize] else {
            return Ok(None);
        };
        Ok(Some(Some(self.equality(term, leaf, budget)?)))
    }

    fn congruence(&mut self, budget: &mut WitnessBudget) -> Result<bool, WitnessError> {
        let mut any = false;
        loop {
            let count = self.terms.len();
            budget.work(checked_mul(count, count)?)?;
            let mut changed = false;
            for a in 0..count {
                let Some(ca) = children(self.terms[a].node) else {
                    continue;
                };
                for b in 0..a {
                    if !same_operator(self.terms[a].node, self.terms[b].node) {
                        continue;
                    }
                    if self.root(a as TermId, budget)? == self.root(b as TermId, budget)? {
                        continue;
                    }
                    let cb = children(self.terms[b].node).ok_or(WitnessError::InvalidEquality)?;
                    if self.root(ca[0], budget)? != self.root(cb[0], budget)?
                        || self.root(ca[1], budget)? != self.root(cb[1], budget)?
                    {
                        continue;
                    }
                    let first = self.equality(ca[0], cb[0], budget)?;
                    let second = self.equality(ca[1], cb[1], budget)?;
                    let proof = self.proof(
                        Equality {
                            from: a as TermId,
                            to: b as TermId,
                            reason: Reason::Congruence {
                                children: [first, second],
                            },
                        },
                        budget,
                    )?;
                    changed |= self.join(proof, budget)?;
                }
            }
            any |= changed;
            if !changed {
                return Ok(any);
            }
        }
    }

    fn fold_prefix(
        &mut self,
        count: usize,
        root: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<TermId, WitnessError> {
        let mut mapped = initialized(count, 0, budget)?;
        for index in 0..count {
            budget.work(1)?;
            let original = self.terms[index].node;
            let Some(old) = children(original) else {
                mapped[index] = index as TermId;
                continue;
            };
            let next = [mapped[old[0] as usize], mapped[old[1] as usize]];
            let rebuilt = self.intern(with_children(original, next)?, budget)?;
            if rebuilt != index as TermId {
                let first = self.equality(old[0], next[0], budget)?;
                let second = self.equality(old[1], next[1], budget)?;
                let proof = self.proof(
                    Equality {
                        from: index as TermId,
                        to: rebuilt,
                        reason: Reason::Congruence {
                            children: [first, second],
                        },
                    },
                    budget,
                )?;
                self.join(proof, budget)?;
            }
            let constants = match (
                self.terms[next[0] as usize].node,
                self.terms[next[1] as usize].node,
            ) {
                (TermNode::Const(a), TermNode::Const(b)) => Some((a, b)),
                _ => None,
            };
            let folded = constants.and_then(|(a, b)| match original {
                TermNode::Checked { op, .. } => self.kind.checked(op_name(op), a, b).ok(),
                TermNode::Wrapping { op, .. } => Some(self.kind.wrapping(op_name(op), a, b)),
                _ => None,
            });
            mapped[index] = if let Some(value) = folded {
                let output = self.intern(TermNode::Const(value), budget)?;
                let rule = match original {
                    TermNode::Checked { op, .. } => 19 + op_index(op),
                    TermNode::Wrapping { op, .. } => 22 + op_index(op),
                    _ => return Err(WitnessError::InvalidRuleInstance),
                };
                let proof = self.proof(
                    Equality {
                        from: rebuilt,
                        to: output,
                        reason: Reason::Rule {
                            rule,
                            substitution: Substitution {
                                bindings: [next[0], next[1], 0],
                                len: 2,
                            },
                            guard: None,
                        },
                    },
                    budget,
                )?;
                self.join(proof, budget)?;
                output
            } else {
                rebuilt
            };
        }
        Ok(mapped[root as usize])
    }

    fn instance(
        &mut self,
        rule: u16,
        x: TermId,
        y: TermId,
        z: TermId,
        budget: &mut WitnessBudget,
    ) -> Result<(TermId, TermId, Option<TermId>), WitnessError> {
        let zero = self.intern(TermNode::Const(0), budget)?;
        let one = self.intern(TermNode::Const(1), budget)?;
        let c = |this: &mut Self, op, children, budget: &mut WitnessBudget| {
            this.intern(TermNode::Checked { op, children }, budget)
        };
        let w = |this: &mut Self, op, children, budget: &mut WitnessBudget| {
            this.intern(TermNode::Wrapping { op, children }, budget)
        };
        use IntOp::{Add, Mul, Sub};
        Ok(match rule {
            1 => (c(self, Add, [x, zero], budget)?, x, None),
            2 => (c(self, Add, [zero, x], budget)?, x, None),
            3 => (c(self, Sub, [x, zero], budget)?, x, None),
            4 => (c(self, Mul, [x, one], budget)?, x, None),
            5 => (c(self, Mul, [one, x], budget)?, x, None),
            6 => (c(self, Mul, [x, zero], budget)?, zero, Some(x)),
            7 => (c(self, Mul, [zero, x], budget)?, zero, Some(x)),
            8 => (c(self, Sub, [x, x], budget)?, zero, Some(x)),
            9 => (w(self, Add, [x, zero], budget)?, x, None),
            10 => (w(self, Sub, [x, zero], budget)?, x, None),
            11 => (w(self, Mul, [x, one], budget)?, x, None),
            12 => (
                w(self, Add, [x, y], budget)?,
                w(self, Add, [y, x], budget)?,
                None,
            ),
            13 => (
                w(self, Mul, [x, y], budget)?,
                w(self, Mul, [y, x], budget)?,
                None,
            ),
            14 | 15 => {
                let op = if rule == 14 { Add } else { Mul };
                let a = w(self, op, [x, y], budget)?;
                let b = w(self, op, [y, z], budget)?;
                (
                    w(self, op, [a, z], budget)?,
                    w(self, op, [x, b], budget)?,
                    None,
                )
            }
            16 => {
                let a = w(self, Add, [x, y], budget)?;
                (w(self, Sub, [a, y], budget)?, x, Some(y))
            }
            17 => (w(self, Mul, [x, zero], budget)?, zero, Some(x)),
            18 => (w(self, Sub, [x, x], budget)?, zero, Some(x)),
            _ => return Err(WitnessError::UnknownRule),
        })
    }
}

fn children(node: TermNode) -> Option<[TermId; 2]> {
    match node {
        TermNode::Checked { children, .. } | TermNode::Wrapping { children, .. } => Some(children),
        _ => None,
    }
}
fn with_children(node: TermNode, children: [TermId; 2]) -> Result<TermNode, WitnessError> {
    match node {
        TermNode::Checked { op, .. } => Ok(TermNode::Checked { op, children }),
        TermNode::Wrapping { op, .. } => Ok(TermNode::Wrapping { op, children }),
        _ => Err(WitnessError::InvalidTermIndex),
    }
}
fn is_leaf(node: TermNode) -> bool {
    matches!(node, TermNode::Const(_) | TermNode::InputVar(_))
}
fn same_operator(a: TermNode, b: TermNode) -> bool {
    matches!((a, b), (TermNode::Checked { op: a, .. }, TermNode::Checked { op: b, .. })
        | (TermNode::Wrapping { op: a, .. }, TermNode::Wrapping { op: b, .. }) if a == b)
}
fn op_name(op: IntOp) -> &'static str {
    match op {
        IntOp::Add => "+",
        IntOp::Sub => "-",
        IntOp::Mul => "*",
    }
}
fn op_index(op: IntOp) -> u16 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}
fn rule_arity(rule: u16) -> usize {
    match rule {
        12 | 13 | 16 => 2,
        14 | 15 => 3,
        _ => 1,
    }
}

#[derive(Clone, Copy)]
struct Record {
    rule: u16,
    bindings: [Id; 3],
    len: u8,
}

struct Capture {
    budget: Option<WitnessBudget>,
    records: Vec<Record>,
    error: Option<WitnessError>,
    node_capacity: usize,
}
impl Capture {
    fn nodes(&mut self, required: usize) -> Result<(), WitnessError> {
        if required > RAW_NODES {
            return Err(WitnessError::Limit);
        }
        if required <= self.node_capacity {
            return Ok(());
        }
        let target = required
            .max(checked_mul(self.node_capacity, 2)?)
            .min(RAW_NODES);
        let nodes = checked_mul(target, 512)?;
        // At most N classes can be merged; each cleanup can copy <=N node/
        // parent entries. Charge that quadratic cumulative request envelope at
        // every full geometric replacement credit, never only the net delta.
        let cleanup = checked_mul(checked_mul(target, target)?, 128)?;
        self.budget
            .as_mut()
            .ok_or(WitnessError::Limit)?
            .metadata(checked_add(nodes, cleanup)?)?;
        self.node_capacity = target;
        Ok(())
    }
    fn fail(&mut self, error: WitnessError) {
        self.error.get_or_insert(error);
    }
    fn charge(&mut self, work: usize) -> bool {
        if self.error.is_some() {
            return false;
        }
        match self
            .budget
            .as_mut()
            .ok_or(WitnessError::Limit)
            .and_then(|budget| budget.work(work))
        {
            Ok(()) => true,
            Err(error) => {
                self.fail(error);
                false
            }
        }
    }
}

struct RecordingApplier {
    inner: Arc<dyn Applier<Math, ()> + Send + Sync>,
    rule: u16,
    variables: [Var; 3],
    capture: Arc<Mutex<Capture>>,
}
impl Applier<Math, ()> for RecordingApplier {
    fn get_pattern_ast(&self) -> Option<&PatternAst<Math>> {
        self.inner.get_pattern_ast()
    }
    fn vars(&self) -> Vec<Var> {
        self.inner.vars()
    }
    fn apply_one(
        &self,
        graph: &mut Graph,
        class: Id,
        substitution: &Subst,
        pattern: Option<&PatternAst<Math>>,
        name: Symbol,
    ) -> Vec<Id> {
        let Ok(mut state) = self.capture.lock() else {
            return Vec::new();
        };
        let work = graph
            .nodes()
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(32));
        if !work.is_some_and(|work| state.charge(work)) {
            state.fail(WitnessError::Limit);
            return Vec::new();
        }
        if graph
            .nodes()
            .len()
            .checked_add(APPLICATION_NODES)
            .is_none_or(|n| n > RAW_NODES)
            || state.records.len() >= RECORDS
        {
            state.fail(WitnessError::Limit);
            return Vec::new();
        }
        let arity = rule_arity(self.rule);
        if let Err(error) = state.nodes(graph.nodes().len() + APPLICATION_NODES) {
            state.fail(error);
            return Vec::new();
        }
        let mut bindings = [Id::from(0); 3];
        for index in 0..arity {
            let Some(id) = substitution.get(self.variables[index]) else {
                state.fail(WitnessError::InvalidSubstitution);
                return Vec::new();
            };
            if usize::from(*id) >= graph.nodes().len() {
                state.fail(WitnessError::InvalidSubstitution);
                return Vec::new();
            }
            bindings[index] = *id;
        }
        let Capture {
            budget, records, ..
        } = &mut *state;
        let Some(budget) = budget.as_mut() else {
            return Vec::new();
        };
        if let Err(error) = grow(records, RECORDS, budget) {
            state.fail(error);
            return Vec::new();
        }
        // Persistent node data is prepaid by geometric credits. This call's
        // transient parent-list and small pattern/result vectors are separate.
        let allocation = graph
            .nodes()
            .len()
            .checked_mul(128)
            .and_then(|n| n.checked_add(256));
        if let Err(error) = allocation
            .ok_or(WitnessError::Limit)
            .and_then(|bytes| budget.metadata(bytes))
        {
            state.fail(error);
            return Vec::new();
        }
        drop(state);
        let result = self
            .inner
            .apply_one(graph, class, substitution, pattern, name);
        if !result.is_empty() {
            if let Ok(mut state) = self.capture.lock() {
                state.records.push(Record {
                    rule: self.rule,
                    bindings,
                    len: arity as u8,
                });
            }
        }
        result
    }
}

struct BoundedScheduler {
    capture: Arc<Mutex<Capture>>,
}
impl RewriteScheduler<Math, ()> for BoundedScheduler {
    fn search_rewrite<'a>(
        &mut self,
        _: usize,
        graph: &Graph,
        rewrite: &'a Rewrite<Math, ()>,
    ) -> Vec<SearchMatches<'a, Math>> {
        let Ok(mut state) = self.capture.lock() else {
            return Vec::new();
        };
        let Some(pattern) = rewrite.searcher.get_pattern_ast() else {
            state.fail(WitnessError::UnknownRule);
            return Vec::new();
        };
        let pattern_nodes = pattern.as_ref();
        // A frozen LHS has <=2 concrete nodes (including constant leaves).
        // Choosing those raw nodes fixes every variable binding. Their count
        // product bounds matches; canonicalization cannot invent a concrete
        // operator/constant absent from the append-only original nodes.
        let scan = graph
            .nodes()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(pattern_nodes.len()));
        if !scan.is_some_and(|scan| state.charge(scan)) {
            state.fail(WitnessError::Limit);
            return Vec::new();
        }
        let mut concrete = 0usize;
        let mut combinations = 1usize;
        for node in pattern_nodes {
            if let ENodeOrVar::ENode(pattern_node) = node {
                concrete += 1;
                if concrete > 2 {
                    state.fail(WitnessError::UnknownRule);
                    return Vec::new();
                }
                let count = graph
                    .nodes()
                    .iter()
                    .filter(|node| node.matches(pattern_node))
                    .count();
                combinations = match combinations.checked_mul(count) {
                    Some(count) => count,
                    None => {
                        state.fail(WitnessError::Limit);
                        return Vec::new();
                    }
                };
            }
        }
        if combinations == 0 {
            return Vec::new();
        }
        if concrete == 0 {
            state.fail(WitnessError::UnknownRule);
            return Vec::new();
        }
        let work = graph
            .nodes()
            .len()
            .checked_mul(combinations)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(32));
        if !work.is_some_and(|work| state.charge(work)) {
            state.fail(WitnessError::Limit);
            return Vec::new();
        }
        if let Err(error) = state
            .budget
            .as_mut()
            .ok_or(WitnessError::Limit)
            // Matcher machines allocate bounded register/scratch storage per
            // visited class, even without a match. Match/outer result vectors
            // have full geometric capacity requests, not just live lengths.
            .and_then(|budget| {
                // Charge potential failed branches too, not just the capped
                // number of successful substitutions returned by egg.
                let scratch = checked_mul(checked_add(graph.nodes().len(), combinations)?, 512)?;
                budget.metadata(scratch)
            })
        {
            state.fail(error);
            return Vec::new();
        }
        drop(state);
        let matches = rewrite.search_with_limit(graph, MATCHES);
        let count = matches
            .iter()
            .map(|found| found.substs.len())
            .sum::<usize>();
        if count >= MATCHES {
            if let Ok(mut state) = self.capture.lock() {
                state.fail(WitnessError::Limit);
            }
            Vec::new()
        } else {
            matches
        }
    }
}

struct SearchResult {
    graph: Graph,
    best: RecExpr<Math>,
    records: Vec<Record>,
    budget: WitnessBudget,
}

fn run_graph(
    expression: &RecExpr<Math>,
    mut budget: WitnessBudget,
) -> Result<SearchResult, WitnessError> {
    // Static closed rule/iteration objects are small. Graph/extractor storage
    // grows through full-capacity credits before graph growth; each search
    // charges its own possible match data, not every hard-cap slot.
    budget.metadata((RULE_COUNT + ITERATIONS) * 512)?;
    budget.work(expression.as_ref().len())?;
    let mut state = Capture {
        budget: Some(budget),
        records: Vec::new(),
        error: None,
        node_capacity: 0,
    };
    state.nodes(expression.as_ref().len().max(4))?;
    let capture = Arc::new(Mutex::new(state));
    let mut actual = rules();
    if actual.len() != RULE_COUNT {
        return Err(WitnessError::UnknownRule);
    }
    let variables = [
        "?x".parse()
            .map_err(|_| WitnessError::InvalidSubstitution)?,
        "?y".parse()
            .map_err(|_| WitnessError::InvalidSubstitution)?,
        "?z".parse()
            .map_err(|_| WitnessError::InvalidSubstitution)?,
    ];
    for (index, rule) in actual.iter_mut().enumerate() {
        rule.applier = Arc::new(RecordingApplier {
            inner: rule.applier.clone(),
            rule: index as u16 + 1,
            variables,
            capture: capture.clone(),
        });
    }
    let hook_capture = capture.clone();
    let runner = Runner::default()
        .with_iter_limit(ITERATIONS)
        .with_node_limit(RAW_NODES)
        .with_scheduler(BoundedScheduler {
            capture: capture.clone(),
        })
        .with_hook(move |_| {
            let state = hook_capture
                .lock()
                .map_err(|_| "witness capture poisoned".to_owned())?;
            if state.error.is_some() {
                Err("witness budget refused".to_owned())
            } else {
                Ok(())
            }
        })
        .with_expr(expression)
        .run(&actual);
    let mut state = capture.lock().map_err(|_| WitnessError::Limit)?;
    if let Some(error) = state.error {
        return Err(error);
    }
    if !matches!(runner.stop_reason, Some(StopReason::Saturated))
        || runner.egraph.nodes().len() > RAW_NODES
    {
        return Err(WitnessError::Limit);
    }
    let mut budget = state.budget.take().ok_or(WitnessError::Limit)?;
    let records = std::mem::take(&mut state.records);
    drop(state);
    let count = runner.egraph.nodes().len();
    budget.work(checked_mul(checked_mul(count, count)?, 8)?)?;
    let extractor = Extractor::new(&runner.egraph, AstSize);
    let root = *runner.roots.first().ok_or(WitnessError::InvalidEquality)?;
    let cost = extractor.find_best_cost(root);
    if cost == 0 || cost > budget.limits().max_output_nodes {
        return Err(WitnessError::Limit);
    }
    let (_, best) = extractor.find_best(root);
    if best.as_ref().len() > budget.limits().max_terms {
        return Err(WitnessError::Limit);
    }
    Ok(SearchResult {
        graph: runner.egraph,
        best,
        records,
        budget,
    })
}

fn graph_node(node: TermNode, map: &[Option<Id>]) -> Result<Math, WitnessError> {
    let mapped = |id: TermId| {
        map.get(id as usize)
            .and_then(|id| *id)
            .ok_or(WitnessError::InvalidTermIndex)
    };
    Ok(match node {
        TermNode::Const(n) => {
            Math::Num(i64::try_from(n).map_err(|_| WitnessError::ConstantOutOfRange)?)
        }
        TermNode::InputVar(id) => Math::Input(id),
        TermNode::Checked { op, children } => {
            let children = [mapped(children[0])?, mapped(children[1])?];
            match op {
                IntOp::Add => Math::Add(children),
                IntOp::Sub => Math::Sub(children),
                IntOp::Mul => Math::Mul(children),
            }
        }
        TermNode::Wrapping { op, children } => {
            let children = [mapped(children[0])?, mapped(children[1])?];
            match op {
                IntOp::Add => Math::WAdd(children),
                IntOp::Sub => Math::WSub(children),
                IntOp::Mul => Math::WMul(children),
            }
        }
    })
}

fn encode_graph(
    terms: &[Term],
    root: TermId,
    budget: &mut WitnessBudget,
) -> Result<Option<RecExpr<Math>>, WitnessError> {
    let mut reachable = initialized(terms.len(), false, budget)?;
    reachable[root as usize] = true;
    for index in (0..terms.len()).rev() {
        budget.work(1)?;
        if !reachable[index] {
            continue;
        }
        if let TermNode::Const(n) = terms[index].node {
            if i64::try_from(n).is_err() {
                return Ok(None);
            }
        }
        if let Some(children) = children(terms[index].node) {
            for child in children {
                if child as usize >= index {
                    return Err(WitnessError::NonTopologicalTerm);
                }
                reachable[child as usize] = true;
            }
        }
    }
    let mut map = initialized(terms.len(), None, budget)?;
    let mut nodes = Vec::new();
    for (index, term) in terms.iter().enumerate() {
        if !reachable[index] {
            continue;
        }
        budget.work(1)?;
        if nodes.len() >= RAW_NODES {
            return Err(WitnessError::Limit);
        }
        let node = graph_node(term.node, &map)?;
        grow(&mut nodes, RAW_NODES, budget)?;
        map[index] = Some(Id::from(nodes.len()));
        nodes.push(node);
    }
    Ok(Some(RecExpr::from(nodes)))
}

fn translate_math(
    nodes: &[Math],
    variable_count: usize,
    builder: &mut Builder,
    budget: &mut WitnessBudget,
) -> Result<Vec<TermId>, WitnessError> {
    let mut map = Vec::new();
    budget.reserve(&mut map, nodes.len())?;
    for (index, node) in nodes.iter().enumerate() {
        budget.work(1)?;
        // Egg's original node vector is append-only. Never trust a canonical
        // class to justify equality or silently follow a forward/cyclic edge.
        if node.children().iter().any(|id| usize::from(*id) >= index) {
            return Err(WitnessError::NonTopologicalTerm);
        }
        let child = |id: Id| {
            map.get(usize::from(id))
                .copied()
                .ok_or(WitnessError::InvalidTermIndex)
        };
        let node = match node {
            Math::Num(n) => TermNode::Const(*n as i128),
            Math::Input(id) => {
                if *id as usize >= variable_count {
                    return Err(WitnessError::UnknownVariable);
                }
                TermNode::InputVar(*id)
            }
            Math::Add(ids) | Math::Sub(ids) | Math::Mul(ids) => {
                let op = match node {
                    Math::Add(_) => IntOp::Add,
                    Math::Sub(_) => IntOp::Sub,
                    _ => IntOp::Mul,
                };
                TermNode::Checked {
                    op,
                    children: [child(ids[0])?, child(ids[1])?],
                }
            }
            Math::WAdd(ids) | Math::WSub(ids) | Math::WMul(ids) => {
                let op = match node {
                    Math::WAdd(_) => IntOp::Add,
                    Math::WSub(_) => IntOp::Sub,
                    _ => IntOp::Mul,
                };
                TermNode::Wrapping {
                    op,
                    children: [child(ids[0])?, child(ids[1])?],
                }
            }
        };
        map.push(builder.intern(node, budget)?);
    }
    Ok(map)
}

fn explain_records(
    builder: &mut Builder,
    raw: &[TermId],
    records: &[Record],
    from: TermId,
    to: TermId,
    budget: &mut WitnessBudget,
) -> Result<ProofId, WitnessError> {
    let mut processed = initialized(records.len(), false, budget)?;
    loop {
        let mut changed = builder.congruence(budget)?;
        if builder.root(from, budget)? == builder.root(to, budget)? {
            return builder.equality(from, to, budget);
        }
        for (index, record) in records.iter().enumerate() {
            budget.work(1)?;
            if processed[index] {
                continue;
            }
            let mut bindings = [0; 3];
            if usize::from(record.len) != rule_arity(record.rule) {
                return Err(WitnessError::InvalidSubstitution);
            }
            for (slot, binding) in bindings.iter_mut().enumerate().take(record.len as usize) {
                *binding = *raw
                    .get(usize::from(record.bindings[slot]))
                    .ok_or(WitnessError::InvalidSubstitution)?;
            }
            let previous_terms = builder.terms.len();
            let (left, right, erased) =
                builder.instance(record.rule, bindings[0], bindings[1], bindings[2], budget)?;
            changed |= builder.terms.len() != previous_terms;
            if builder.root(left, budget)? == builder.root(right, budget)? {
                processed[index] = true;
                continue;
            }
            let guard = match erased {
                Some(term) => match builder.guard(term, budget)? {
                    Some(guard) => guard,
                    None => continue,
                },
                None => None,
            };
            let proof = builder.proof(
                Equality {
                    from: left,
                    to: right,
                    reason: Reason::Rule {
                        rule: record.rule,
                        substitution: Substitution {
                            bindings,
                            len: record.len,
                        },
                        guard,
                    },
                },
                budget,
            )?;
            changed |= builder.join(proof, budget)?;
            processed[index] = true;
        }
        if !changed {
            return Err(WitnessError::InvalidEquality);
        }
    }
}

pub(super) fn propose_rewrite(
    source: &Expr,
    types: &TypeMap,
    limits: WitnessLimits,
) -> Result<RewriteOutcome, WitnessError> {
    let mut budget = WitnessBudget::new(limits)?;
    let bound = match bind_source(source, types, &mut budget) {
        Ok(bound) => bound,
        Err(
            WitnessError::UnsupportedAst
            | WitnessError::MissingType
            | WitnessError::TypeMismatch
            | WitnessError::ConstantOutOfRange
            | WitnessError::UnknownVariable
            | WitnessError::InvalidIntrinsicOrigin,
        ) => return Ok(RewriteOutcome::Barrier),
        Err(error) => return Err(error),
    };
    let mut builder = Builder::new(bound.kind(), limits);
    let variable_count = bound.variable_count();
    let mut source_map = Vec::new();
    budget.reserve(&mut source_map, bound.terms().len())?;
    for (index, term) in bound.terms().iter().enumerate() {
        budget.work(1)?;
        let node = if let Some(children) = children(term.node) {
            if children.iter().any(|id| *id as usize >= index) {
                return Err(WitnessError::NonTopologicalTerm);
            }
            with_children(
                term.node,
                [
                    source_map[children[0] as usize],
                    source_map[children[1] as usize],
                ],
            )?
        } else {
            term.node
        };
        source_map.push(builder.intern(node, &mut budget)?);
    }
    let input = *source_map
        .get(bound.root() as usize)
        .ok_or(WitnessError::InvalidTermIndex)?;
    let original_count = builder.terms.len();
    let folded = builder.fold_prefix(original_count, input, &mut budget)?;
    let output = match encode_graph(&builder.terms, folded, &mut budget)? {
        None => folded, // The sole deliberate fold-only, non-resource profile.
        Some(expression) => {
            let SearchResult {
                graph,
                best,
                records,
                budget: remaining,
            } = run_graph(&expression, budget)?;
            budget = remaining;
            let best_map =
                translate_math(best.as_ref(), variable_count, &mut builder, &mut budget)?;
            let best = *best_map.last().ok_or(WitnessError::InvalidTermIndex)?;
            if best != folded {
                let raw = translate_math(graph.nodes(), variable_count, &mut builder, &mut budget)?;
                explain_records(&mut builder, &raw, &records, folded, best, &mut budget)?;
            }
            best
        }
    };
    if output == input {
        return Ok(RewriteOutcome::Unchanged);
    }
    let conclusion = builder.equality(input, output, &mut budget)?;
    let candidate = build_candidate(&bound, &builder.terms, output, &mut budget)?;
    let certificate = RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms: builder.terms,
        proofs: builder.proofs,
        input,
        output,
        conclusion,
    };
    Ok(RewriteOutcome::Candidate(RewriteProposal {
        candidate,
        certificate,
        budget,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::witness::verify_with_budget;
    use super::*;
    use crate::ast::{ExprKind, Item, Literal};

    fn fixture(program: &str) -> (Expr, TypeMap) {
        let parsed = crate::parser::parse(program);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let function = parsed
            .module
            .items
            .into_iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("fixture function");
        let body = function.body.expect("function body");
        let expression = match body.kind {
            ExprKind::Block { statements, tail } => {
                assert!(statements.is_empty());
                *tail.expect("fixture tail")
            }
            _ => body,
        };
        (expression, checked.expression_types)
    }

    fn proposal(source: &Expr, types: &TypeMap) -> RewriteProposal {
        let outcome =
            propose_rewrite(source, types, WitnessLimits::default()).expect("bounded producer");
        let RewriteOutcome::Candidate(mut proposal) = outcome else {
            panic!("expected genuine produced candidate");
        };
        verify_with_budget(
            source,
            &proposal.candidate,
            types,
            &proposal.certificate,
            &mut proposal.budget,
        )
        .expect("independent gate with remaining producer budget");
        proposal
    }

    fn has_rule(proposal: &RewriteProposal, expected: u16) -> bool {
        proposal.certificate.proofs.iter().any(|proof| {
            matches!(proof.reason,
            Reason::Rule { rule, .. } if rule == expected)
        })
    }

    #[test]
    fn actual_runner_identity_is_gated_by_its_recorded_rule() {
        let (source, types) = fixture("fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 0 }");
        let produced = proposal(&source, &types);
        assert!(matches!(&produced.candidate.kind, ExprKind::Name(name) if name == "x"));
        assert!(has_rule(&produced, 1));
    }

    #[test]
    fn all_six_original_folds_have_explicit_prefix_rules() {
        for (body, rule, value, effect) in [
            ("4 + 5", 19, 9, " eff [Fail[ArithOverflow]]"),
            ("4 - 3", 20, 1, " eff [Fail[ArithOverflow]]"),
            ("3 * 5", 21, 15, " eff [Fail[ArithOverflow]]"),
            ("wrapping_add(250, 9)", 22, 3, ""),
            ("wrapping_sub(0, 1)", 23, 255, ""),
            ("wrapping_mul(16, 16)", 24, 0, ""),
        ] {
            let (source, types) = fixture(&format!("fn f() -> u8{effect} {{ {body} }}"));
            let produced = proposal(&source, &types);
            assert!(has_rule(&produced, rule));
            assert!(
                matches!(produced.candidate.kind, ExprKind::Literal(Literal::Int(n)) if n == value)
            );
        }
    }

    #[test]
    fn high_u64_fold_only_and_eliminated_high_constants_are_witnessed() {
        for (body, expected) in [
            (
                "wrapping_add(9223372036854775807, 2)",
                9223372036854775809i128,
            ),
            (
                "wrapping_sub(18446744073709551615, 18446744073709551615)",
                0,
            ),
        ] {
            let (source, types) = fixture(&format!("fn f() -> u64 {{ {body} }}"));
            let produced = proposal(&source, &types);
            assert!(
                matches!(produced.candidate.kind, ExprKind::Literal(Literal::Int(n)) if n == expected)
            );
        }
    }

    #[test]
    fn checked_overflow_and_resource_refusal_leave_the_complete_input_alone() {
        let (source, types) = fixture("fn f() -> u8 eff [Fail[ArithOverflow]] { 255 + 1 }");
        let before = source.clone();
        assert!(matches!(
            propose_rewrite(&source, &types, WitnessLimits::default()).unwrap(),
            RewriteOutcome::Unchanged
        ));
        let limits = WitnessLimits {
            max_work: 1,
            ..WitnessLimits::default()
        };
        assert!(matches!(
            propose_rewrite(&source, &types, limits),
            Err(WitnessError::Limit)
        ));
        assert_eq!(source, before);
        let (source, types) = fixture("fn f() -> u64 { wrapping_add(wrapping_add(1, 2), 3) }");
        let limits = WitnessLimits {
            max_proofs: 1,
            ..WitnessLimits::default()
        };
        assert!(matches!(
            propose_rewrite(&source, &types, limits),
            Err(WitnessError::Limit)
        ));
    }

    #[test]
    fn prior_identity_and_congruence_support_guarded_erasure() {
        let (source, types) =
            fixture("fn f(x: u8) -> u8 eff [Fail[ArithOverflow]] { (x + 0) * 0 }");
        let produced = proposal(&source, &types);
        assert!(has_rule(&produced, 1));
        assert!(has_rule(&produced, 6));
        assert!(matches!(
            produced.candidate.kind,
            ExprKind::Literal(Literal::Int(0))
        ));
    }

    #[test]
    fn folded_child_prefix_emits_real_congruence_for_its_live_parent() {
        let (source, types) =
            fixture("fn f(x: u8) -> u8 eff [Fail[ArithOverflow]] { (1 + 2) + x }");
        let produced = proposal(&source, &types);
        assert!(has_rule(&produced, 19));
        assert!(
            produced
                .certificate
                .proofs
                .iter()
                .any(|proof| matches!(proof.reason, Reason::Congruence { .. }))
        );
        assert!(matches!(produced.candidate.kind, ExprKind::Binary(_, _, _)));
    }

    #[test]
    fn rebuilt_wrapping_call_keeps_the_real_original_callee() {
        let (source, types) =
            fixture("fn f(x: u64, y: u64) -> u64 { wrapping_mul(wrapping_add(x, 0), y) }");
        let ExprKind::Call {
            callee: original, ..
        } = &source.kind
        else {
            panic!("source call");
        };
        let produced = proposal(&source, &types);
        let ExprKind::Call { callee, .. } = &produced.candidate.kind else {
            panic!("candidate call");
        };
        assert_eq!(callee, original);
        assert_eq!(produced.candidate.span, source.span);
        assert!(has_rule(&produced, 9));
    }

    #[test]
    fn a_guarded_capture_cannot_treat_unproven_class_membership_as_totality() {
        let mut budget = WitnessBudget::new(WitnessLimits::default()).unwrap();
        let mut builder = Builder::new(IntegerKind::U8, WitnessLimits::default());
        let x = builder.intern(TermNode::InputVar(0), &mut budget).unwrap();
        let y = builder.intern(TermNode::InputVar(1), &mut budget).unwrap();
        let sum = builder
            .intern(
                TermNode::Checked {
                    op: IntOp::Add,
                    children: [x, y],
                },
                &mut budget,
            )
            .unwrap();
        let zero = builder.intern(TermNode::Const(0), &mut budget).unwrap();
        let initial = builder
            .intern(
                TermNode::Checked {
                    op: IntOp::Mul,
                    children: [sum, zero],
                },
                &mut budget,
            )
            .unwrap();
        // This is exactly the unsupported claim a class-leaf oracle would
        // supply. No actual equality from the erased sum to a leaf exists.
        let record = Record {
            rule: 6,
            bindings: [Id::from(0); 3],
            len: 1,
        };
        assert!(matches!(
            explain_records(&mut builder, &[sum], &[record], initial, zero, &mut budget),
            Err(WitnessError::InvalidEquality)
        ));
        assert_ne!(
            builder.root(initial, &mut budget).unwrap(),
            builder.root(zero, &mut budget).unwrap()
        );
    }

    #[test]
    fn actual_search_match_cap_refuses_instead_of_allocating_an_unbounded_list() {
        let mut nodes = vec![Math::Num(0)];
        let mut root = None;
        for index in 0..MATCHES {
            let input = Id::from(nodes.len());
            nodes.push(Math::Input(index as u32));
            let term = Id::from(nodes.len());
            nodes.push(Math::Add([input, Id::from(0)]));
            root = Some(match root {
                None => term,
                Some(previous) => {
                    let combined = Id::from(nodes.len());
                    nodes.push(Math::WAdd([previous, term]));
                    combined
                }
            });
        }
        assert_eq!(root, Some(Id::from(nodes.len() - 1)));
        let expression = RecExpr::from(nodes);
        let mut graph = Graph::default();
        graph.add_expr(&expression);
        graph.rebuild();
        let actual = rules();
        let control = actual[0].search_with_limit(&graph, MATCHES);
        assert_eq!(
            control
                .iter()
                .map(|found| found.substs.len())
                .sum::<usize>(),
            MATCHES
        );
        let capture = Arc::new(Mutex::new(Capture {
            budget: Some(WitnessBudget::new(WitnessLimits::HARD).unwrap()),
            records: Vec::new(),
            error: None,
            node_capacity: graph.nodes().len(),
        }));
        let mut scheduler = BoundedScheduler {
            capture: capture.clone(),
        };
        let before = graph.nodes().len();
        assert!(scheduler.search_rewrite(0, &graph, &actual[0]).is_empty());
        assert_eq!(capture.lock().unwrap().error, Some(WitnessError::Limit));
        assert_eq!(graph.nodes().len(), before);
    }

    #[test]
    fn capture_and_raw_node_caps_refuse_before_the_real_applier_runs() {
        for record_full in [true, false] {
            let mut graph = Graph::default();
            let x = graph.add(Math::Input(0));
            let zero = graph.add(Math::Num(0));
            let original = graph.add(Math::Add([x, zero]));
            if !record_full {
                while graph.nodes().len() <= RAW_NODES - APPLICATION_NODES {
                    graph.add(Math::Input(graph.nodes().len() as u32));
                }
            }
            let variables = [
                "?x".parse().unwrap(),
                "?y".parse().unwrap(),
                "?z".parse().unwrap(),
            ];
            let mut substitution = Subst::default();
            substitution.insert(variables[0], x);
            let records = if record_full {
                vec![
                    Record {
                        rule: 1,
                        bindings: [x; 3],
                        len: 1
                    };
                    RECORDS
                ]
            } else {
                Vec::new()
            };
            let capture = Arc::new(Mutex::new(Capture {
                budget: Some(WitnessBudget::new(WitnessLimits::default()).unwrap()),
                records,
                error: None,
                node_capacity: 0,
            }));
            let actual = rules();
            let applier = RecordingApplier {
                inner: actual[0].applier.clone(),
                rule: 1,
                variables,
                capture: capture.clone(),
            };
            let before = graph.nodes().len();
            assert!(
                applier
                    .apply_one(&mut graph, original, &substitution, None, "add-zero".into())
                    .is_empty()
            );
            assert_eq!(capture.lock().unwrap().error, Some(WitnessError::Limit));
            assert_eq!(graph.nodes().len(), before);
            assert_ne!(graph.find(original), graph.find(x));
        }
    }

    #[test]
    fn initial_term_and_depth_refusals_never_turn_into_barrier_retries() {
        let (source, types) = fixture("fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 0 }");
        for limits in [
            WitnessLimits {
                max_terms: 1,
                ..WitnessLimits::default()
            },
            WitnessLimits {
                max_depth: 1,
                ..WitnessLimits::default()
            },
        ] {
            assert!(matches!(
                propose_rewrite(&source, &types, limits),
                Err(WitnessError::Limit)
            ));
        }
    }
}
