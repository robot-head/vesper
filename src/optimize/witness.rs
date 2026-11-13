//! Bounded, egg-independent checking of the pure integer AST rewrite profile.
#![forbid(unsafe_code)]

use crate::ast::{Arg, Expr, ExprKind, Literal, Pass, Span, Type, TypeMap};
use crate::numeric::IntegerKind;
use std::fmt;
use std::mem::size_of;

pub type TermId = u32;
pub type ProofId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntOp {
    Add,
    Sub,
    Mul,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermNode {
    Const(i128),
    InputVar(u32),
    Checked { op: IntOp, children: [TermId; 2] },
    Wrapping { op: IntOp, children: [TermId; 2] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    pub kind: IntegerKind,
    pub node: TermNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Substitution {
    pub bindings: [TermId; 3],
    pub len: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Refl,
    Rule {
        rule: u16,
        substitution: Substitution,
        guard: Option<ProofId>,
    },
    Congruence {
        children: [ProofId; 2],
    },
    Symmetry {
        proof: ProofId,
    },
    Transitivity {
        first: ProofId,
        second: ProofId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Equality {
    pub from: TermId,
    pub to: TermId,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteCertificate {
    pub schema_version: u16,
    pub arithmetic_version: u16,
    pub terms: Vec<Term>,
    pub proofs: Vec<Equality>,
    pub input: TermId,
    pub output: TermId,
    pub conclusion: ProofId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WitnessLimits {
    pub max_terms: usize,
    pub max_proofs: usize,
    pub max_input_nodes: usize,
    pub max_output_nodes: usize,
    pub max_depth: usize,
    pub max_variables: usize,
    pub max_name_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}

impl WitnessLimits {
    pub const HARD: Self = Self {
        max_terms: 16384,
        max_proofs: 32768,
        max_input_nodes: 2048,
        max_output_nodes: 4096,
        max_depth: 128,
        max_variables: 512,
        max_name_bytes: 65536,
        max_metadata_bytes: 16 * 1024 * 1024,
        max_work: 8_000_000,
    };
}

impl Default for WitnessLimits {
    fn default() -> Self {
        Self {
            max_terms: 4096,
            max_proofs: 8192,
            max_input_nodes: 512,
            max_output_nodes: 1024,
            max_depth: 64,
            max_variables: 128,
            max_name_bytes: 16384,
            max_metadata_bytes: 4 * 1024 * 1024,
            max_work: 2_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessError {
    InvalidLimits,
    Limit,
    Allocation,
    UnsupportedAst,
    MissingType,
    TypeMismatch,
    ConstantOutOfRange,
    UnknownVariable,
    InvalidIntrinsicOrigin,
    UnsupportedVersion,
    InvalidTermIndex,
    NonTopologicalTerm,
    InvalidProofIndex,
    NonTopologicalProof,
    UnknownRule,
    InvalidSubstitution,
    InvalidRuleInstance,
    UnprovenTotality,
    InvalidEquality,
    InputMismatch,
    OutputMismatch,
}

impl fmt::Display for WitnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WitnessError {}

/// One move-only cumulative attempt budget; producer phases cannot reset it.
pub(super) struct WitnessBudget {
    limits: WitnessLimits,
    work: usize,
    metadata: usize,
    names: usize,
}

fn add(a: usize, b: usize) -> Result<usize, WitnessError> {
    a.checked_add(b).ok_or(WitnessError::Limit)
}
fn mul(a: usize, b: usize) -> Result<usize, WitnessError> {
    a.checked_mul(b).ok_or(WitnessError::Limit)
}
fn limit(value: usize, max: usize) -> Result<(), WitnessError> {
    if value > max {
        Err(WitnessError::Limit)
    } else {
        Ok(())
    }
}

impl WitnessBudget {
    pub(super) fn new(limits: WitnessLimits) -> Result<Self, WitnessError> {
        let hard = WitnessLimits::HARD;
        let pairs = [
            (limits.max_terms, hard.max_terms),
            (limits.max_proofs, hard.max_proofs),
            (limits.max_input_nodes, hard.max_input_nodes),
            (limits.max_output_nodes, hard.max_output_nodes),
            (limits.max_depth, hard.max_depth),
            (limits.max_variables, hard.max_variables),
            (limits.max_name_bytes, hard.max_name_bytes),
            (limits.max_metadata_bytes, hard.max_metadata_bytes),
            (limits.max_work, hard.max_work),
        ];
        if pairs.iter().any(|&(value, max)| value == 0 || value > max) {
            return Err(WitnessError::InvalidLimits);
        }
        Ok(Self {
            limits,
            work: 0,
            metadata: 0,
            names: 0,
        })
    }
    pub(super) fn limits(&self) -> WitnessLimits {
        self.limits
    }
    pub(super) fn work(&mut self, amount: usize) -> Result<(), WitnessError> {
        let next = add(self.work, amount)?;
        limit(next, self.limits.max_work)?;
        self.work = next;
        Ok(())
    }
    pub(super) fn metadata(&mut self, amount: usize) -> Result<(), WitnessError> {
        let next = add(self.metadata, amount)?;
        limit(next, self.limits.max_metadata_bytes)?;
        self.work(amount)?;
        self.metadata = next;
        Ok(())
    }
    fn names(&mut self, amount: usize) -> Result<(), WitnessError> {
        let next = add(self.names, amount)?;
        limit(next, self.limits.max_name_bytes)?;
        self.work(amount)?;
        self.names = next;
        Ok(())
    }
    pub(super) fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), WitnessError> {
        let required = add(values.len(), additional)?;
        if required <= values.capacity() {
            return Ok(());
        }
        let capacity = required
            .max(
                values
                    .capacity()
                    .checked_mul(2)
                    .ok_or(WitnessError::Limit)?,
            )
            .max(1);
        self.metadata(mul(capacity, size_of::<T>())?)?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| WitnessError::Allocation)
    }
}

struct Origin<'s> {
    op: IntOp,
    callee: &'s Expr,
}

pub(super) struct BoundSource<'s> {
    kind: IntegerKind,
    terms: Vec<Term>,
    root: TermId,
    names: Vec<&'s str>,
    origins: Vec<Origin<'s>>,
    span: Span,
}

impl<'s> BoundSource<'s> {
    pub(super) fn kind(&self) -> IntegerKind {
        self.kind
    }
    pub(super) fn terms(&self) -> &[Term] {
        &self.terms
    }
    pub(super) fn root(&self) -> TermId {
        self.root
    }
    pub(super) fn variable_count(&self) -> usize {
        self.names.len()
    }
    pub(super) fn name(&self, id: u32) -> Option<&'s str> {
        self.names.get(id as usize).copied()
    }
    fn origin(&self, op: IntOp) -> Option<&'s Expr> {
        self.origins
            .iter()
            .find(|origin| origin.op == op)
            .map(|origin| origin.callee)
    }
}

fn closed_kind(ty: &Type) -> Option<IntegerKind> {
    let Type::Named(name, args) = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    Some(match name.as_str() {
        "u8" => IntegerKind::U8,
        "u32" => IntegerKind::U32,
        "u64" => IntegerKind::U64,
        "i64" => IntegerKind::I64,
        _ => return None,
    })
}
fn checked_op(name: &str) -> Option<IntOp> {
    match name {
        "+" => Some(IntOp::Add),
        "-" => Some(IntOp::Sub),
        "*" => Some(IntOp::Mul),
        _ => None,
    }
}
fn wrapping_op(name: &str) -> Option<IntOp> {
    match name {
        "wrapping_add" => Some(IntOp::Add),
        "wrapping_sub" => Some(IntOp::Sub),
        "wrapping_mul" => Some(IntOp::Mul),
        _ => None,
    }
}
fn op_name(op: IntOp, wrapping: bool) -> &'static str {
    match (op, wrapping) {
        (IntOp::Add, false) => "+",
        (IntOp::Sub, false) => "-",
        (IntOp::Mul, false) => "*",
        (IntOp::Add, true) => "wrapping_add",
        (IntOp::Sub, true) => "wrapping_sub",
        (IntOp::Mul, true) => "wrapping_mul",
    }
}
fn call_parts(e: &Expr) -> Result<(IntOp, &Expr, [&Expr; 2]), WitnessError> {
    let ExprKind::Call {
        callee,
        types,
        args,
    } = &e.kind
    else {
        return Err(WitnessError::UnsupportedAst);
    };
    let ExprKind::Name(name) = &callee.kind else {
        return Err(WitnessError::InvalidIntrinsicOrigin);
    };
    let op = wrapping_op(name).ok_or(WitnessError::UnsupportedAst)?;
    if !types.is_empty()
        || args.len() != 2
        || args.iter().any(|arg| arg.pass != Pass::In || arg.explicit)
    {
        return Err(WitnessError::UnsupportedAst);
    }
    Ok((op, callee, [&args[0].expr, &args[1].expr]))
}
fn source_type(e: &Expr, kind: IntegerKind, types: &TypeMap) -> Result<(), WitnessError> {
    let ty = types
        .get(&(e.span.start, e.span.end))
        .ok_or(WitnessError::MissingType)?;
    if closed_kind(ty) == Some(kind)
        || matches!(
            (&e.kind, ty),
            (ExprKind::Literal(Literal::Int(_)), Type::IntLiteral(_))
        )
    {
        Ok(())
    } else {
        Err(WitnessError::TypeMismatch)
    }
}
fn intern(
    terms: &mut Vec<Term>,
    term: Term,
    budget: &mut WitnessBudget,
) -> Result<TermId, WitnessError> {
    for (id, existing) in terms.iter().enumerate() {
        budget.work(1)?;
        if *existing == term {
            return Ok(id as TermId);
        }
    }
    limit(add(terms.len(), 1)?, budget.limits.max_terms)?;
    budget.reserve(terms, 1)?;
    let id = terms.len() as TermId;
    terms.push(term);
    Ok(id)
}

enum SourceFrame<'s> {
    Enter(&'s Expr, usize),
    Finish(IntOp, bool),
}

pub(super) fn bind_source<'s>(
    source: &'s Expr,
    types: &TypeMap,
    budget: &mut WitnessBudget,
) -> Result<BoundSource<'s>, WitnessError> {
    let kind = closed_kind(
        types
            .get(&(source.span.start, source.span.end))
            .ok_or(WitnessError::MissingType)?,
    )
    .ok_or(WitnessError::TypeMismatch)?;
    let mut bound = BoundSource {
        kind,
        terms: Vec::new(),
        root: 0,
        names: Vec::new(),
        origins: Vec::new(),
        span: source.span.clone(),
    };
    let mut stack = Vec::new();
    let mut values = Vec::new();
    budget.reserve(&mut stack, 1)?;
    stack.push(SourceFrame::Enter(source, 1));
    let mut nodes = 0;
    while let Some(frame) = stack.pop() {
        budget.work(1)?;
        let term = match frame {
            SourceFrame::Finish(op, wrapping) => {
                let right = values.pop().ok_or(WitnessError::UnsupportedAst)?;
                let left = values.pop().ok_or(WitnessError::UnsupportedAst)?;
                Term {
                    kind,
                    node: if wrapping {
                        TermNode::Wrapping {
                            op,
                            children: [left, right],
                        }
                    } else {
                        TermNode::Checked {
                            op,
                            children: [left, right],
                        }
                    },
                }
            }
            SourceFrame::Enter(e, depth) => {
                nodes = add(nodes, 1)?;
                limit(nodes, budget.limits.max_input_nodes)?;
                limit(depth, budget.limits.max_depth)?;
                source_type(e, kind, types)?;
                match &e.kind {
                    ExprKind::Literal(Literal::Int(value)) => {
                        if !kind.fits(*value) {
                            return Err(WitnessError::ConstantOutOfRange);
                        }
                        Term {
                            kind,
                            node: TermNode::Const(*value),
                        }
                    }
                    ExprKind::Name(name) => {
                        budget.names(name.len())?;
                        let mut found = None;
                        for (id, previous) in bound.names.iter().enumerate() {
                            budget.work(add(previous.len(), name.len())?)?;
                            if *previous == name {
                                found = Some(id as u32);
                                break;
                            }
                        }
                        let id = if let Some(id) = found {
                            id
                        } else {
                            limit(add(bound.names.len(), 1)?, budget.limits.max_variables)?;
                            budget.reserve(&mut bound.names, 1)?;
                            let id = bound.names.len() as u32;
                            bound.names.push(name);
                            id
                        };
                        Term {
                            kind,
                            node: TermNode::InputVar(id),
                        }
                    }
                    ExprKind::Binary(name, left, right) => {
                        let op = checked_op(name).ok_or(WitnessError::UnsupportedAst)?;
                        budget.reserve(&mut stack, 3)?;
                        stack.push(SourceFrame::Finish(op, false));
                        stack.push(SourceFrame::Enter(right, add(depth, 1)?));
                        stack.push(SourceFrame::Enter(left, add(depth, 1)?));
                        continue;
                    }
                    ExprKind::Call { .. } => {
                        let (op, callee, args) = call_parts(e)?;
                        if types.contains_key(&(callee.span.start, callee.span.end)) {
                            return Err(WitnessError::InvalidIntrinsicOrigin);
                        }
                        nodes = add(nodes, 1)?;
                        limit(nodes, budget.limits.max_input_nodes)?;
                        limit(add(depth, 1)?, budget.limits.max_depth)?;
                        budget.names(op_name(op, true).len())?;
                        budget.reserve(&mut bound.origins, 1)?;
                        bound.origins.push(Origin { op, callee });
                        budget.reserve(&mut stack, 3)?;
                        stack.push(SourceFrame::Finish(op, true));
                        stack.push(SourceFrame::Enter(args[1], add(depth, 1)?));
                        stack.push(SourceFrame::Enter(args[0], add(depth, 1)?));
                        continue;
                    }
                    _ => return Err(WitnessError::UnsupportedAst),
                }
            }
        };
        let id = intern(&mut bound.terms, term, budget)?;
        budget.reserve(&mut values, 1)?;
        values.push(id);
    }
    if values.len() != 1 {
        return Err(WitnessError::UnsupportedAst);
    }
    bound.root = values[0];
    Ok(bound)
}

fn term(terms: &[Term], id: TermId) -> Result<&Term, WitnessError> {
    terms.get(id as usize).ok_or(WitnessError::InvalidTermIndex)
}
fn binary(node: TermNode) -> Option<(bool, IntOp, [TermId; 2])> {
    match node {
        TermNode::Checked { op, children } => Some((false, op, children)),
        TermNode::Wrapping { op, children } => Some((true, op, children)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Shape {
    depth: usize,
    nodes: usize,
    names: usize,
    bytes: usize,
}
fn capped_sum(a: usize, b: usize, cap: usize) -> usize {
    a.saturating_add(b).min(cap + 1)
}

fn validate_terms(
    source: &BoundSource<'_>,
    terms: &[Term],
    budget: &mut WitnessBudget,
) -> Result<Vec<Shape>, WitnessError> {
    if terms.is_empty() {
        return Err(WitnessError::InvalidTermIndex);
    }
    limit(terms.len(), budget.limits.max_terms)?;
    budget.metadata(mul(terms.len(), size_of::<Term>())?)?;
    let mut shapes: Vec<Shape> = Vec::new();
    budget.reserve(&mut shapes, terms.len())?;
    for (id, current) in terms.iter().enumerate() {
        budget.work(8)?;
        if current.kind != source.kind {
            return Err(WitnessError::TypeMismatch);
        }
        let shape = match current.node {
            TermNode::Const(value) => {
                if !current.kind.fits(value) {
                    return Err(WitnessError::ConstantOutOfRange);
                }
                Shape {
                    depth: 1,
                    nodes: 1,
                    names: 0,
                    bytes: 0,
                }
            }
            TermNode::InputVar(variable) => {
                let name = source.name(variable).ok_or(WitnessError::UnknownVariable)?;
                Shape {
                    depth: 1,
                    nodes: 1,
                    names: name.len(),
                    bytes: name.len(),
                }
            }
            node => {
                let (wrapping, op, children) = binary(node).ok_or(WitnessError::UnsupportedAst)?;
                for child in children {
                    term(terms, child)?;
                    if child as usize >= id {
                        return Err(WitnessError::NonTopologicalTerm);
                    }
                }
                let [a, b] = children.map(|child| shapes[child as usize]);
                let depth = add(1, a.depth.max(b.depth))?;
                limit(depth, budget.limits.max_depth)?;
                let nodes = capped_sum(
                    usize::from(wrapping) + 1,
                    capped_sum(a.nodes, b.nodes, budget.limits.max_output_nodes),
                    budget.limits.max_output_nodes,
                );
                let names = capped_sum(
                    usize::from(wrapping) * op_name(op, true).len(),
                    capped_sum(a.names, b.names, budget.limits.max_name_bytes),
                    budget.limits.max_name_bytes,
                );
                // Conservative inline/box/vector/string payload allocation envelope.
                let overhead =
                    8 * size_of::<Expr>() + 2 * size_of::<Arg>() + op_name(op, wrapping).len();
                let bytes = capped_sum(
                    overhead,
                    capped_sum(a.bytes, b.bytes, budget.limits.max_metadata_bytes),
                    budget.limits.max_metadata_bytes,
                );
                Shape {
                    depth,
                    nodes,
                    names,
                    bytes,
                }
            }
        };
        shapes.push(shape);
    }
    Ok(shapes)
}

fn compare_input(
    source: &BoundSource<'_>,
    terms: &[Term],
    input: TermId,
    budget: &mut WitnessBudget,
) -> Result<(), WitnessError> {
    let mut stack = Vec::new();
    budget.reserve(&mut stack, 1)?;
    stack.push((source.root, input));
    let mut nodes = 0;
    while let Some((left, right)) = stack.pop() {
        budget.work(1)?;
        let a = term(&source.terms, left)?;
        let b = term(terms, right)?;
        if a.kind != b.kind {
            return Err(WitnessError::InputMismatch);
        }
        nodes = add(nodes, 1)?;
        if let (Some((aw, ao, ac)), Some((bw, bo, bc))) = (binary(a.node), binary(b.node)) {
            if aw != bw || ao != bo {
                return Err(WitnessError::InputMismatch);
            }
            nodes = add(nodes, usize::from(aw))?;
            budget.reserve(&mut stack, 2)?;
            stack.push((ac[1], bc[1]));
            stack.push((ac[0], bc[0]));
        } else if a.node != b.node {
            return Err(WitnessError::InputMismatch);
        }
        limit(nodes, budget.limits.max_input_nodes)?;
    }
    Ok(())
}

fn compare_output(
    source: &BoundSource<'_>,
    candidate: &Expr,
    terms: &[Term],
    output: TermId,
    budget: &mut WitnessBudget,
) -> Result<(), WitnessError> {
    if candidate.span != source.span {
        return Err(WitnessError::OutputMismatch);
    }
    let mut stack = Vec::new();
    budget.reserve(&mut stack, 1)?;
    stack.push((candidate, output, 1usize));
    let mut nodes = 0;
    while let Some((e, id, depth)) = stack.pop() {
        budget.work(1)?;
        nodes = add(nodes, 1)?;
        limit(depth, budget.limits.max_depth)?;
        // Backends use value spans to recover coercion types. Only the checked
        // common-kind root span is permitted for generated value occurrences.
        // The separately inspected intrinsic callee retains its real origin.
        if e.span != source.span {
            return Err(WitnessError::OutputMismatch);
        }
        budget.metadata(size_of::<Expr>())?;
        let current = term(terms, id)?;
        match (&e.kind, current.node) {
            (ExprKind::Literal(Literal::Int(actual)), TermNode::Const(expected))
                if *actual == expected => {}
            (ExprKind::Name(actual), TermNode::InputVar(variable)) => {
                budget.metadata(actual.capacity())?;
                let expected = source.name(variable).ok_or(WitnessError::UnknownVariable)?;
                budget.names(actual.len())?;
                budget.work(expected.len())?;
                if actual != expected {
                    return Err(WitnessError::OutputMismatch);
                }
            }
            (ExprKind::Binary(actual, left, right), TermNode::Checked { op, children }) => {
                budget.metadata(actual.capacity())?;
                if checked_op(actual) != Some(op) {
                    return Err(WitnessError::OutputMismatch);
                }
                budget.reserve(&mut stack, 2)?;
                stack.push((right, children[1], add(depth, 1)?));
                stack.push((left, children[0], add(depth, 1)?));
            }
            (ExprKind::Call { .. }, TermNode::Wrapping { op, children }) => {
                let ExprKind::Call {
                    callee,
                    types,
                    args,
                } = &e.kind
                else {
                    return Err(WitnessError::OutputMismatch);
                };
                budget.metadata(size_of::<Expr>())?;
                budget.metadata(mul(types.capacity(), size_of::<Type>())?)?;
                budget.metadata(mul(args.capacity(), size_of::<Arg>())?)?;
                if let ExprKind::Name(name) = &callee.kind {
                    budget.metadata(name.capacity())?;
                }
                let (actual, callee, args) =
                    call_parts(e).map_err(|_| WitnessError::OutputMismatch)?;
                if actual != op {
                    return Err(WitnessError::OutputMismatch);
                }
                nodes = add(nodes, 1)?;
                limit(add(depth, 1)?, budget.limits.max_depth)?;
                budget.names(op_name(op, true).len())?;
                let mut found = false;
                for origin in &source.origins {
                    budget.work(1 + op_name(op, true).len())?;
                    if origin.op == op && origin.callee == callee {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(WitnessError::InvalidIntrinsicOrigin);
                }
                budget.reserve(&mut stack, 2)?;
                stack.push((args[1], children[1], add(depth, 1)?));
                stack.push((args[0], children[0], add(depth, 1)?));
            }
            _ => return Err(WitnessError::OutputMismatch),
        }
        limit(nodes, budget.limits.max_output_nodes)?;
    }
    Ok(())
}

enum BuildFrame {
    Enter(TermId),
    Finish(bool, IntOp),
}

pub(super) fn build_candidate(
    source: &BoundSource<'_>,
    terms: &[Term],
    root: TermId,
    budget: &mut WitnessBudget,
) -> Result<Expr, WitnessError> {
    let shapes = validate_terms(source, terms, budget)?;
    let shape = *shapes
        .get(root as usize)
        .ok_or(WitnessError::InvalidTermIndex)?;
    limit(shape.nodes, budget.limits.max_output_nodes)?;
    limit(shape.names, budget.limits.max_name_bytes)?;
    budget.names(shape.names)?;
    budget.metadata(shape.bytes)?;
    budget.work(mul(shape.nodes, 8)?)?;
    // Check all reachable origin requirements before cloning any source name.
    let mut origins = Vec::new();
    budget.reserve(&mut origins, terms.len())?;
    origins.resize(terms.len(), false);
    origins[root as usize] = true;
    for id in (0..terms.len()).rev() {
        budget.work(1)?;
        if !origins[id] {
            continue;
        }
        if let Some((wrapping, op, children)) = binary(terms[id].node) {
            if wrapping {
                budget.work(source.origins.len())?;
            }
            if wrapping && source.origin(op).is_none() {
                return Err(WitnessError::InvalidIntrinsicOrigin);
            }
            for child in children {
                origins[child as usize] = true;
            }
        }
    }
    let mut stack = Vec::new();
    let mut values: Vec<Expr> = Vec::new();
    // Prepay every possible origin-inventory scan during expanded construction
    // before the first name/callee/payload allocation is materialized.
    budget.work(mul(shape.nodes, source.origins.len())?)?;
    budget.reserve(&mut stack, add(mul(shape.nodes, 2)?, 1)?)?;
    budget.reserve(&mut values, shape.nodes)?;
    stack.push(BuildFrame::Enter(root));
    while let Some(frame) = stack.pop() {
        let kind = match frame {
            BuildFrame::Enter(id) => match terms[id as usize].node {
                TermNode::Const(value) => ExprKind::Literal(Literal::Int(value)),
                TermNode::InputVar(variable) => ExprKind::Name(
                    source
                        .name(variable)
                        .ok_or(WitnessError::UnknownVariable)?
                        .to_owned(),
                ),
                node => {
                    let (wrapping, op, children) =
                        binary(node).ok_or(WitnessError::UnsupportedAst)?;
                    stack.push(BuildFrame::Finish(wrapping, op));
                    stack.push(BuildFrame::Enter(children[1]));
                    stack.push(BuildFrame::Enter(children[0]));
                    continue;
                }
            },
            BuildFrame::Finish(wrapping, op) => {
                let right = values.pop().ok_or(WitnessError::OutputMismatch)?;
                let left = values.pop().ok_or(WitnessError::OutputMismatch)?;
                if wrapping {
                    let original = source
                        .origin(op)
                        .ok_or(WitnessError::InvalidIntrinsicOrigin)?;
                    let ExprKind::Name(name) = &original.kind else {
                        return Err(WitnessError::InvalidIntrinsicOrigin);
                    };
                    let callee = Expr {
                        kind: ExprKind::Name(name.clone()),
                        span: original.span.clone(),
                    };
                    let mut args = Vec::new();
                    args.try_reserve_exact(2)
                        .map_err(|_| WitnessError::Allocation)?;
                    args.push(Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: left,
                    });
                    args.push(Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: right,
                    });
                    ExprKind::Call {
                        callee: Box::new(callee),
                        types: Vec::new(),
                        args,
                    }
                } else {
                    ExprKind::Binary(
                        op_name(op, false).to_owned(),
                        Box::new(left),
                        Box::new(right),
                    )
                }
            }
        };
        values.push(Expr {
            kind,
            span: source.span.clone(),
        });
    }
    if values.len() != 1 {
        return Err(WitnessError::OutputMismatch);
    }
    values.pop().ok_or(WitnessError::OutputMismatch)
}

fn prior(proofs: &[Equality], id: ProofId, current: usize) -> Result<&Equality, WitnessError> {
    let proof = proofs
        .get(id as usize)
        .ok_or(WitnessError::InvalidProofIndex)?;
    if id as usize >= current {
        return Err(WitnessError::NonTopologicalProof);
    }
    Ok(proof)
}
fn constant(terms: &[Term], id: TermId, value: i128) -> bool {
    matches!(terms[id as usize].node, TermNode::Const(actual) if actual == value)
}
fn application(
    terms: &[Term],
    id: TermId,
    wrapping: bool,
    op: IntOp,
    a: TermId,
    b: TermId,
) -> bool {
    binary(terms[id as usize].node) == Some((wrapping, op, [a, b]))
}
fn total_leaf(terms: &[Term], id: TermId) -> bool {
    matches!(
        terms[id as usize].node,
        TermNode::Const(_) | TermNode::InputVar(_)
    )
}

fn check_rule(
    terms: &[Term],
    proofs: &[Equality],
    index: usize,
    step: &Equality,
    rule: u16,
    substitution: Substitution,
    guard: Option<ProofId>,
) -> Result<(), WitnessError> {
    let arity = match rule {
        1..=11 | 17..=18 => 1,
        12..=13 | 16 | 19..=24 => 2,
        14..=15 => 3,
        _ => return Err(WitnessError::UnknownRule),
    };
    if substitution.len as usize != arity
        || substitution.bindings[arity..].iter().any(|&id| id != 0)
    {
        return Err(WitnessError::InvalidSubstitution);
    }
    for &id in &substitution.bindings[..arity] {
        term(terms, id)?;
    }
    let [x, y, z] = substitution.bindings;
    let guarded = match rule {
        6..=8 | 17..=18 => Some(x),
        16 => Some(y),
        _ => None,
    };
    if let Some(erased) = guarded {
        if let Some(id) = guard {
            let premise = prior(proofs, id, index)?;
            if !((premise.from == erased && total_leaf(terms, premise.to))
                || (premise.to == erased && total_leaf(terms, premise.from)))
            {
                return Err(WitnessError::UnprovenTotality);
            }
        } else if !total_leaf(terms, erased) {
            return Err(WitnessError::UnprovenTotality);
        }
    } else if guard.is_some() {
        return Err(WitnessError::InvalidRuleInstance);
    }
    let from = step.from;
    let to = step.to;
    let source_children = binary(terms[from as usize].node).map(|(_, _, children)| children);
    let target_children = binary(terms[to as usize].node).map(|(_, _, children)| children);
    let valid = match rule {
        1 | 3 | 4 | 6 | 9 | 10 | 11 | 17 => {
            let (wrapping, op, identity) = match rule {
                1 => (false, IntOp::Add, 0),
                3 => (false, IntOp::Sub, 0),
                4 => (false, IntOp::Mul, 1),
                6 => (false, IntOp::Mul, 0),
                9 => (true, IntOp::Add, 0),
                10 => (true, IntOp::Sub, 0),
                11 => (true, IntOp::Mul, 1),
                _ => (true, IntOp::Mul, 0),
            };
            source_children.is_some_and(|[a, b]| {
                a == x
                    && constant(terms, b, identity)
                    && application(terms, from, wrapping, op, a, b)
            }) && if matches!(rule, 6 | 17) {
                constant(terms, to, 0)
            } else {
                to == x
            }
        }
        2 | 5 | 7 => {
            let (op, identity) = if rule == 2 {
                (IntOp::Add, 0)
            } else {
                (IntOp::Mul, i128::from(rule == 5))
            };
            source_children.is_some_and(|[a, b]| {
                b == x && constant(terms, a, identity) && application(terms, from, false, op, a, b)
            }) && if rule == 7 {
                constant(terms, to, 0)
            } else {
                to == x
            }
        }
        8 | 18 => application(terms, from, rule == 18, IntOp::Sub, x, x) && constant(terms, to, 0),
        12 | 13 => {
            let op = if rule == 12 { IntOp::Add } else { IntOp::Mul };
            application(terms, from, true, op, x, y) && application(terms, to, true, op, y, x)
        }
        14 | 15 => {
            let op = if rule == 14 { IntOp::Add } else { IntOp::Mul };
            source_children
                .zip(target_children)
                .is_some_and(|([left, right], [a, b])| {
                    right == z
                        && a == x
                        && application(terms, from, true, op, left, z)
                        && application(terms, left, true, op, x, y)
                        && application(terms, to, true, op, x, b)
                        && application(terms, b, true, op, y, z)
                })
        }
        16 => {
            source_children.is_some_and(|[a, b]| {
                b == y
                    && application(terms, from, true, IntOp::Sub, a, y)
                    && application(terms, a, true, IntOp::Add, x, y)
            }) && to == x
        }
        19..=24 => {
            let op = match (rule - 19) % 3 {
                0 => IntOp::Add,
                1 => IntOp::Sub,
                _ => IntOp::Mul,
            };
            let (TermNode::Const(a), TermNode::Const(b), TermNode::Const(result)) = (
                terms[x as usize].node,
                terms[y as usize].node,
                terms[to as usize].node,
            ) else {
                return Err(WitnessError::InvalidRuleInstance);
            };
            let wrapping = rule >= 22;
            let exact = match op {
                IntOp::Add => a.checked_add(b),
                IntOp::Sub => a.checked_sub(b),
                IntOp::Mul => a.checked_mul(b),
            };
            let expected = if wrapping {
                // Low 64/32/8 bits of wrapping i128 arithmetic are exactly the
                // requested modular result, even for U64 products >i128::MAX.
                Some(terms[from as usize].kind.wrap(match op {
                    IntOp::Add => a.wrapping_add(b),
                    IntOp::Sub => a.wrapping_sub(b),
                    IntOp::Mul => a.wrapping_mul(b),
                }))
            } else {
                exact.filter(|&value| terms[from as usize].kind.fits(value))
            };
            application(terms, from, wrapping, op, x, y) && expected == Some(result)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(WitnessError::InvalidRuleInstance)
    }
}

pub(super) fn verify_with_budget(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    budget: &mut WitnessBudget,
) -> Result<(), WitnessError> {
    if certificate.schema_version != 1 || certificate.arithmetic_version != 1 {
        return Err(WitnessError::UnsupportedVersion);
    }
    limit(certificate.terms.len(), budget.limits.max_terms)?;
    limit(certificate.proofs.len(), budget.limits.max_proofs)?;
    budget.metadata(mul(certificate.proofs.capacity(), size_of::<Equality>())?)?;
    budget.metadata(mul(certificate.terms.capacity(), size_of::<Term>())?)?;
    let bound = bind_source(source, types, budget)?;
    let shapes = validate_terms(&bound, &certificate.terms, budget)?;
    term(&certificate.terms, certificate.input)?;
    term(&certificate.terms, certificate.output)?;
    compare_input(&bound, &certificate.terms, certificate.input, budget)?;
    for (index, step) in certificate.proofs.iter().enumerate() {
        budget.work(64)?;
        let from = term(&certificate.terms, step.from)?;
        let to = term(&certificate.terms, step.to)?;
        match step.reason {
            Reason::Refl => {
                if from != to {
                    return Err(WitnessError::InvalidEquality);
                }
            }
            Reason::Rule {
                rule,
                substitution,
                guard,
            } => check_rule(
                &certificate.terms,
                &certificate.proofs,
                index,
                step,
                rule,
                substitution,
                guard,
            )?,
            Reason::Congruence { children } => {
                let (Some((aw, ao, ac)), Some((bw, bo, bc))) = (binary(from.node), binary(to.node))
                else {
                    return Err(WitnessError::InvalidEquality);
                };
                if aw != bw || ao != bo {
                    return Err(WitnessError::InvalidEquality);
                }
                for lane in 0..2 {
                    let premise = prior(&certificate.proofs, children[lane], index)?;
                    if premise.from != ac[lane] || premise.to != bc[lane] {
                        return Err(WitnessError::InvalidEquality);
                    }
                }
            }
            Reason::Symmetry { proof } => {
                let premise = prior(&certificate.proofs, proof, index)?;
                if premise.from != step.to || premise.to != step.from {
                    return Err(WitnessError::InvalidEquality);
                }
            }
            Reason::Transitivity { first, second } => {
                let a = prior(&certificate.proofs, first, index)?;
                let b = prior(&certificate.proofs, second, index)?;
                if a.from != step.from || b.to != step.to || a.to != b.from {
                    return Err(WitnessError::InvalidEquality);
                }
            }
        }
    }
    let conclusion = certificate
        .proofs
        .get(certificate.conclusion as usize)
        .ok_or(WitnessError::InvalidProofIndex)?;
    if conclusion.from != certificate.input || conclusion.to != certificate.output {
        return Err(WitnessError::InvalidEquality);
    }
    // Public callers may supply a candidate allocated before entering the
    // checker. Its logical represented data still counts with checker scratch.
    let output_shape = shapes[certificate.output as usize];
    limit(output_shape.nodes, budget.limits.max_output_nodes)?;
    limit(output_shape.names, budget.limits.max_name_bytes)?;
    budget.metadata(add(output_shape.bytes, size_of::<Expr>())?)?;
    compare_output(
        &bound,
        candidate,
        &certificate.terms,
        certificate.output,
        budget,
    )
}

/// Verify an untrusted certificate against the actual checked source and output.
/// Failure never mutates either AST. This function performs no egg operation.
pub fn verify_rewrite(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    limits: WitnessLimits,
) -> Result<(), WitnessError> {
    verify_with_budget(
        source,
        candidate,
        types,
        certificate,
        &mut WitnessBudget::new(limits)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable_source() -> (Expr, TypeMap) {
        let source = Expr {
            kind: ExprKind::Name("x".into()),
            span: 1..2,
        };
        let mut types = TypeMap::new();
        types.insert((1, 2), IntegerKind::U64.ty());
        (source, types)
    }

    #[test]
    fn shared_dag_expansion_is_refused_before_materialization() {
        let (source, types) = variable_source();
        let before = source.clone();
        let mut budget = WitnessBudget::new(WitnessLimits::default()).unwrap();
        let bound = bind_source(&source, &types, &mut budget).unwrap();
        let mut terms = bound.terms().to_vec();
        let mut root = bound.root();
        for _ in 0..24 {
            let next = terms.len() as TermId;
            terms.push(Term {
                kind: IntegerKind::U64,
                node: TermNode::Checked {
                    op: IntOp::Add,
                    children: [root, root],
                },
            });
            root = next;
        }
        assert_eq!(
            build_candidate(&bound, &terms, root, &mut budget),
            Err(WitnessError::Limit)
        );
        assert_eq!(source, before);
    }

    #[test]
    fn output_builder_copies_real_intrinsic_origin_and_rechecks_cumulatively() {
        let source = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Name("wrapping_add".into()),
                    span: 3..15,
                }),
                types: vec![],
                args: vec![
                    Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: Expr {
                            kind: ExprKind::Name("x".into()),
                            span: 16..17,
                        },
                    },
                    Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: Expr {
                            kind: ExprKind::Literal(Literal::Int(0)),
                            span: 18..19,
                        },
                    },
                ],
            },
            span: 1..20,
        };
        let mut types = TypeMap::new();
        for span in [(1, 20), (16, 17), (18, 19)] {
            types.insert(span, IntegerKind::U8.ty());
        }
        let mut budget = WitnessBudget::new(WitnessLimits::default()).unwrap();
        let bound = bind_source(&source, &types, &mut budget).unwrap();
        let candidate = build_candidate(&bound, bound.terms(), bound.root(), &mut budget).unwrap();
        let ExprKind::Call { callee, args, .. } = &candidate.kind else {
            panic!("actual call");
        };
        assert_eq!(callee.span, 3..15);
        assert_eq!(args[0].expr.span, source.span);
        assert_eq!(args[1].expr.span, source.span);
        let certificate = RewriteCertificate {
            schema_version: 1,
            arithmetic_version: 1,
            terms: bound.terms().to_vec(),
            proofs: vec![Equality {
                from: bound.root(),
                to: bound.root(),
                reason: Reason::Refl,
            }],
            input: bound.root(),
            output: bound.root(),
            conclusion: 0,
        };
        let used = budget.work;
        verify_with_budget(&source, &candidate, &types, &certificate, &mut budget).unwrap();
        assert!(budget.work > used);
        budget.work = budget.limits.max_work;
        assert_eq!(
            verify_with_budget(&source, &candidate, &types, &certificate, &mut budget),
            Err(WitnessError::Limit)
        );
    }

    #[test]
    fn failed_logical_reservation_does_not_grow_the_vector() {
        let limits = WitnessLimits {
            max_metadata_bytes: 1,
            ..WitnessLimits::default()
        };
        let mut budget = WitnessBudget::new(limits).unwrap();
        let mut values: Vec<u64> = Vec::new();
        assert_eq!(budget.reserve(&mut values, 1), Err(WitnessError::Limit));
        assert_eq!(values.capacity(), 0);
    }
}
