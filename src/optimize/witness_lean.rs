//! Structured, bounded preparation of independent mathematical Lean replay.
//! Preparation is not kernel acceptance or a proof of Rust/compiler refinement.
#![forbid(unsafe_code)]

use super::certificate::{RewriteOutcome, propose_rewrite};
use super::witness::{
    self, Equality, IntOp, ProofId, Reason, RewriteCertificate, TermId, TermNode, WitnessError,
    WitnessLimits,
};
use crate::ast::{Expr, ExprKind, Literal, Pass, Type, TypeMap};
use crate::numeric::IntegerKind;
use std::{fmt, mem::size_of};
use vesper_lean_syntax::{
    Argument, BinaryOp, Declaration, Definition, DefinitionKind, Ident, Module, ModuleName, Name,
    Term, UnaryOp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportLimits {
    pub max_declarations: usize,
    pub max_ast_nodes: usize,
    pub max_ast_depth: usize,
    pub max_name_bytes: usize,
    pub max_source_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl ExportLimits {
    pub const HARD: Self = Self {
        max_declarations: 65536,
        max_ast_nodes: 1048576,
        max_ast_depth: 64,
        max_name_bytes: 4 * 1024 * 1024,
        max_source_bytes: 16 * 1024 * 1024,
        max_metadata_bytes: 256 * 1024 * 1024,
        max_work: 32_000_000,
    };
}
impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_declarations: 16384,
            max_ast_nodes: 262144,
            max_ast_depth: 32,
            max_name_bytes: 1024 * 1024,
            max_source_bytes: 4 * 1024 * 1024,
            max_metadata_bytes: 64 * 1024 * 1024,
            max_work: 8_000_000,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayScope {
    MathModelEq,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayManifest {
    pub format_version: u16,
    pub model_version: u16,
    pub schema_version: u16,
    pub arithmetic_version: u16,
    pub module_name: &'static str,
    pub theorem: &'static str,
    pub scope: ReplayScope,
    pub kind: IntegerKind,
    pub input: TermId,
    pub output: TermId,
    pub conclusion: ProofId,
    pub term_count: usize,
    pub proof_count: usize,
    pub declaration_count: usize,
    pub ast_node_count: usize,
    pub ast_depth: usize,
    pub name_bytes: usize,
    pub source_bytes_upper_bound: usize,
    pub model_blake3: [u8; 32],
    pub certificate_blake3: [u8; 32],
    pub binding_blake3: [u8; 32],
}
pub struct PreparedLean {
    module: Module,
    manifest: ReplayManifest,
    budget: Budget,
}
impl fmt::Debug for PreparedLean {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedLean")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}
pub struct RenderedLean {
    pub source: String,
    pub source_blake3: [u8; 32],
    pub manifest: ReplayManifest,
}
pub struct PreparedReplay {
    pub candidate: Expr,
    pub certificate: RewriteCertificate,
    pub lean: PreparedLean,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    InvalidLimits,
    Witness(WitnessError),
    Limit,
    Allocation,
    LeanSyntax,
}
impl From<WitnessError> for ExportError {
    fn from(e: WitnessError) -> Self {
        Self::Witness(e)
    }
}
impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Witness(e) => Some(e),
            _ => None,
        }
    }
}

const RULES: [&str; 24] = [
    "r01_add_zero",
    "r02_zero_add",
    "r03_sub_zero",
    "r04_mul_one",
    "r05_one_mul",
    "r06_mul_zero",
    "r07_zero_mul",
    "r08_self_sub",
    "r09_wadd_zero",
    "r10_wsub_zero",
    "r11_wmul_one",
    "r12_wadd_comm",
    "r13_wmul_comm",
    "r14_wadd_assoc",
    "r15_wmul_assoc",
    "r16_wadd_cancel",
    "r17_wmul_zero",
    "r18_wself_sub",
    "r19_checked_add_constants",
    "r20_checked_sub_constants",
    "r21_checked_mul_constants",
    "r22_wrapping_add_constants",
    "r23_wrapping_sub_constants",
    "r24_wrapping_mul_constants",
];
const AUDIT_ROOTS: [&str; 33] = [
    "Vesper.RewriteWitness.r01_add_zero",
    "Vesper.RewriteWitness.r02_zero_add",
    "Vesper.RewriteWitness.r03_sub_zero",
    "Vesper.RewriteWitness.r04_mul_one",
    "Vesper.RewriteWitness.r05_one_mul",
    "Vesper.RewriteWitness.r06_mul_zero",
    "Vesper.RewriteWitness.r07_zero_mul",
    "Vesper.RewriteWitness.r08_self_sub",
    "Vesper.RewriteWitness.r09_wadd_zero",
    "Vesper.RewriteWitness.r10_wsub_zero",
    "Vesper.RewriteWitness.r11_wmul_one",
    "Vesper.RewriteWitness.r12_wadd_comm",
    "Vesper.RewriteWitness.r13_wmul_comm",
    "Vesper.RewriteWitness.r14_wadd_assoc",
    "Vesper.RewriteWitness.r15_wmul_assoc",
    "Vesper.RewriteWitness.r16_wadd_cancel",
    "Vesper.RewriteWitness.r17_wmul_zero",
    "Vesper.RewriteWitness.r18_wself_sub",
    "Vesper.RewriteWitness.r19_checked_add_constants",
    "Vesper.RewriteWitness.r20_checked_sub_constants",
    "Vesper.RewriteWitness.r21_checked_mul_constants",
    "Vesper.RewriteWitness.r22_wrapping_add_constants",
    "Vesper.RewriteWitness.r23_wrapping_sub_constants",
    "Vesper.RewriteWitness.r24_wrapping_mul_constants",
    "Vesper.RewriteWitness.value_fits",
    "Vesper.RewriteWitness.literal_exact",
    "Vesper.RewriteWitness.value_toInt_injective",
    "Vesper.RewriteWitness.literal_roundtrip",
    "Vesper.RewriteWitness.total_const",
    "Vesper.RewriteWitness.total_var",
    "Vesper.RewriteWitness.total_of_eq",
    "Vesper.RewriteWitness.congr_checked",
    "Vesper.RewriteWitness.congr_wrapping",
];
pub fn model_source() -> &'static str {
    include_str!("../../lean/RewriteWitness.lean")
}
pub fn model_audit_roots() -> &'static [&'static str] {
    &AUDIT_ROOTS
}

fn add(a: usize, b: usize) -> Result<usize, ExportError> {
    a.checked_add(b).ok_or(ExportError::Limit)
}
fn mul(a: usize, b: usize) -> Result<usize, ExportError> {
    a.checked_mul(b).ok_or(ExportError::Limit)
}
fn bound(a: usize, max: usize) -> Result<(), ExportError> {
    if a > max {
        Err(ExportError::Limit)
    } else {
        Ok(())
    }
}
fn vec_space<T>(n: usize) -> Result<Vec<T>, ExportError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| ExportError::Allocation)?;
    Ok(v)
}
struct Budget {
    limits: ExportLimits,
    metadata: usize,
    work: usize,
}
impl Budget {
    fn new(limits: ExportLimits) -> Result<Self, ExportError> {
        let h = ExportLimits::HARD;
        for (a, b) in [
            (limits.max_declarations, h.max_declarations),
            (limits.max_ast_nodes, h.max_ast_nodes),
            (limits.max_ast_depth, h.max_ast_depth),
            (limits.max_name_bytes, h.max_name_bytes),
            (limits.max_source_bytes, h.max_source_bytes),
            (limits.max_metadata_bytes, h.max_metadata_bytes),
            (limits.max_work, h.max_work),
        ] {
            if a == 0 || a > b {
                return Err(ExportError::InvalidLimits);
            }
        }
        Ok(Self {
            limits,
            metadata: 0,
            work: 0,
        })
    }
    fn work(&mut self, n: usize) -> Result<(), ExportError> {
        let next = add(self.work, n)?;
        bound(next, self.limits.max_work)?;
        self.work = next;
        Ok(())
    }
    fn metadata(&mut self, n: usize) -> Result<(), ExportError> {
        let next = add(self.metadata, n)?;
        bound(next, self.limits.max_metadata_bytes)?;
        self.metadata = next;
        Ok(())
    }
}
impl PreparedLean {
    pub fn module(&self) -> &Module {
        &self.module
    }
    pub fn manifest(&self) -> &ReplayManifest {
        &self.manifest
    }
    pub fn into_module(self) -> Module {
        self.module
    }
    pub fn render(self) -> Result<RenderedLean, ExportError> {
        // AST/all printer temporaries/output growth were prepaid before AST
        // construction. This is the sole serialization, never a measurement pass.
        bound(self.budget.metadata, self.budget.limits.max_metadata_bytes)?;
        let source = self.module.render().map_err(|_| ExportError::LeanSyntax)?;
        bound(source.len(), self.manifest.source_bytes_upper_bound)?;
        bound(source.len(), self.budget.limits.max_source_bytes)?;
        // The complete upper bound was charged for this hash before AST
        // construction, so rendering does not reset or charge it a second time.
        let source_blake3 = *blake3::hash(source.as_bytes()).as_bytes();
        Ok(RenderedLean {
            source,
            source_blake3,
            manifest: self.manifest,
        })
    }
}

#[derive(Clone, Copy)]
enum Component {
    Static(&'static str),
    Index(u8, u32),
}
#[derive(Clone, Copy)]
struct Spec {
    prefix: &'static [&'static str],
    last: Component,
    rooted: bool,
}
fn model(last: &'static str) -> Spec {
    Spec {
        prefix: &["Vesper", "RewriteWitness"],
        last: Component::Static(last),
        rooted: true,
    }
}
fn kind_spec(k: IntegerKind) -> Spec {
    Spec {
        prefix: &["Vesper", "RewriteWitness", "Kind"],
        last: Component::Static(match k {
            IntegerKind::U8 => "u8",
            IntegerKind::U32 => "u32",
            IntegerKind::U64 => "u64",
            IntegerKind::I64 => "i64",
        }),
        rooted: true,
    }
}
fn op_spec(op: IntOp) -> Spec {
    Spec {
        prefix: &["Vesper", "RewriteWitness", "IntOp"],
        last: Component::Static(match op {
            IntOp::Add => "add",
            IntOp::Sub => "sub",
            IntOp::Mul => "mul",
        }),
        rooted: true,
    }
}
fn index_spec(prefix: u8, id: u32, rooted: bool) -> Spec {
    Spec {
        prefix: if rooted {
            &["Vesper", "ReplayWitness"]
        } else {
            &[]
        },
        last: Component::Index(prefix, id),
        rooted,
    }
}
fn standard(prefix: &'static [&'static str], last: &'static str) -> Spec {
    Spec {
        prefix,
        last: Component::Static(last),
        rooted: true,
    }
}
fn digits(mut n: u128) -> usize {
    let mut len = 1;
    while n >= 10 {
        n /= 10;
        len += 1;
    }
    len
}
fn component_len(c: Component) -> usize {
    match c {
        Component::Static(s) => s.len(),
        Component::Index(_, n) => 1 + digits(n as u128),
    }
}
fn string_for(c: Component) -> Result<String, ExportError> {
    let mut s = String::new();
    s.try_reserve_exact(component_len(c))
        .map_err(|_| ExportError::Allocation)?;
    match c {
        Component::Static(value) => s.push_str(value),
        Component::Index(prefix, mut n) => {
            s.push(char::from(prefix));
            let mut bytes = [0u8; 10];
            let mut start = bytes.len();
            loop {
                start -= 1;
                bytes[start] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            for byte in &bytes[start..] {
                s.push(char::from(*byte));
            }
        }
    }
    Ok(s)
}
fn make_name(spec: Spec) -> Result<Name, ExportError> {
    let mut parts = vec_space(spec.prefix.len() + 1)?;
    for &part in spec.prefix {
        parts.push(
            Ident::new(string_for(Component::Static(part))?)
                .map_err(|_| ExportError::LeanSyntax)?,
        );
    }
    parts.push(Ident::new(string_for(spec.last)?).map_err(|_| ExportError::LeanSyntax)?);
    let name = Name::qualified(parts).map_err(|_| ExportError::LeanSyntax)?;
    Ok(if spec.rooted { name.rooted() } else { name })
}

#[derive(Clone, Copy, Default)]
struct Stats {
    nodes: usize,
    depth: usize,
    names: usize,
    bytes: usize,
    metadata: usize,
}
// Structural nodes are Module/ModuleName, Declaration, Term, Argument, Name
// and Ident. Vec/Box indirections and literal/string storage are payloads,
// metered in bytes rather than additional AST levels.
impl Stats {
    fn combine(self, b: Self) -> Result<Self, ExportError> {
        Ok(Self {
            nodes: add(self.nodes, b.nodes)?,
            depth: self.depth.max(b.depth),
            names: add(self.names, b.names)?,
            bytes: add(self.bytes, b.bytes)?,
            metadata: add(self.metadata, b.metadata)?,
        })
    }
}
fn name_stats(spec: Spec) -> Result<Stats, ExportError> {
    let parts = spec.prefix.len() + 1;
    let mut names = component_len(spec.last);
    for part in spec.prefix {
        names = add(names, part.len())?;
    }
    Ok(Stats {
        nodes: parts + 1,
        depth: 2, // Name -> Ident; components are flat sibling leaves.
        names,
        bytes: add(
            add(names, mul(parts, 4)?)?,
            parts - 1 + if spec.rooted { 7 } else { 0 },
        )?,
        metadata: add(
            size_of::<Name>(),
            add(mul(parts, 2 * size_of::<Ident>())?, mul(names, 4)?)?,
        )?,
    })
}
struct Piece {
    stats: Stats,
    ast: Option<Term>,
}
struct Maker {
    build: bool,
}
impl Maker {
    fn reference(&self, spec: Spec) -> Result<Piece, ExportError> {
        let mut stats = name_stats(spec)?;
        stats.nodes = add(stats.nodes, 1)?;
        stats.depth = add(stats.depth, 1)?; // Term::Ref -> Name -> Ident.
        stats.metadata = add(stats.metadata, size_of::<Term>())?;
        Ok(Piece {
            stats,
            ast: if self.build {
                Some(Term::Ref(make_name(spec)?))
            } else {
                None
            },
        })
    }
    fn node(&self, stats: Stats, ast: Option<Term>) -> Piece {
        Piece { stats, ast }
    }
    fn nat(&self, n: u128) -> Result<Piece, ExportError> {
        let len = digits(n);
        Ok(self.node(
            Stats {
                nodes: 1,
                depth: 1,
                names: 0,
                bytes: len,
                metadata: size_of::<Term>() + 4 * len,
            },
            if self.build {
                Some(Term::Nat(n.into()))
            } else {
                None
            },
        ))
    }
    fn fixed(&self, decide: bool) -> Piece {
        self.node(
            Stats {
                nodes: 1,
                depth: 1,
                names: 0,
                bytes: if decide { 11 } else { 3 },
                metadata: size_of::<Term>(),
            },
            if self.build {
                Some(if decide { Term::ByDecide } else { Term::Rfl })
            } else {
                None
            },
        )
    }
    fn app<const N: usize>(
        &self,
        function: Piece,
        kind: Option<Piece>,
        args: [Piece; N],
    ) -> Result<Piece, ExportError> {
        let count = N + usize::from(kind.is_some());
        let mut stats = function.stats;
        let mut maxdepth = function.stats.depth;
        for arg in &args {
            stats = stats.combine(arg.stats)?;
            maxdepth = maxdepth.max(add(arg.stats.depth, 1)?); // Argument -> value.
        }
        if let Some(k) = &kind {
            stats = stats.combine(k.stats)?;
            // A named Argument has both its Ident leaf and value as children.
            maxdepth = maxdepth.max(add(k.stats.depth.max(1), 1)?);
        }
        stats.nodes = add(stats.nodes, 1 + count + if kind.is_some() { 1 } else { 0 })?;
        stats.depth = add(maxdepth, 1)?;
        stats.bytes = add(stats.bytes, 2 + count + if kind.is_some() { 11 } else { 0 })?;
        stats.names = add(stats.names, usize::from(kind.is_some()))?;
        stats.metadata = add(
            stats.metadata,
            size_of::<Term>()
                + count * size_of::<Argument>()
                + if kind.is_some() {
                    size_of::<Ident>() + 4
                } else {
                    0
                },
        )?;
        let ast = if self.build {
            let mut arguments = vec_space(count)?;
            if let Some(k) = kind {
                arguments.push(Argument::Named {
                    name: Ident::new("k").map_err(|_| ExportError::LeanSyntax)?,
                    value: k.ast.ok_or(ExportError::LeanSyntax)?,
                });
            }
            for arg in args {
                arguments.push(Argument::Positional(
                    arg.ast.ok_or(ExportError::LeanSyntax)?,
                ));
            }
            Some(Term::App {
                function: Box::new(function.ast.ok_or(ExportError::LeanSyntax)?),
                arguments,
            })
        } else {
            None
        };
        Ok(self.node(stats, ast))
    }
    fn integer(&self, n: i128) -> Result<Piece, ExportError> {
        let magnitude = self.nat(n.unsigned_abs())?;
        let ty = self.reference(standard(&[], "Int"))?;
        let mut stats = magnitude.stats.combine(ty.stats)?;
        stats.nodes = add(stats.nodes, 1)?;
        stats.depth = add(stats.depth, 1)?;
        stats.bytes = add(stats.bytes, 5)?;
        stats.metadata = add(stats.metadata, size_of::<Term>())?;
        let mut ast = if self.build {
            Some(Term::Ascribe {
                value: Box::new(magnitude.ast.ok_or(ExportError::LeanSyntax)?),
                ty: Box::new(ty.ast.ok_or(ExportError::LeanSyntax)?),
            })
        } else {
            None
        };
        if n < 0 {
            stats.nodes = add(stats.nodes, 1)?;
            stats.depth = add(stats.depth, 1)?;
            stats.bytes = add(stats.bytes, 3)?;
            stats.metadata = add(stats.metadata, size_of::<Term>())?;
            if self.build {
                ast = Some(Term::Unary {
                    op: UnaryOp::Neg,
                    value: Box::new(ast.ok_or(ExportError::LeanSyntax)?),
                });
            }
        }
        Ok(self.node(stats, ast))
    }
    fn eq(&self, left: Piece, right: Piece) -> Result<Piece, ExportError> {
        let mut stats = left.stats.combine(right.stats)?;
        stats.nodes = add(stats.nodes, 1)?;
        stats.depth = add(stats.depth, 1)?;
        stats.bytes = add(stats.bytes, 5)?;
        stats.metadata = add(stats.metadata, size_of::<Term>())?;
        let ast = if self.build {
            Some(Term::Binary {
                op: BinaryOp::Eq,
                left: Box::new(left.ast.ok_or(ExportError::LeanSyntax)?),
                right: Box::new(right.ast.ok_or(ExportError::LeanSyntax)?),
            })
        } else {
            None
        };
        Ok(self.node(stats, ast))
    }
    fn node_ref(&self, id: u32) -> Result<Piece, ExportError> {
        self.reference(index_spec(b'n', id, true))
    }
    fn proof_ref(&self, id: u32) -> Result<Piece, ExportError> {
        self.reference(index_spec(b'p', id, true))
    }
    fn operation(
        &self,
        k: IntegerKind,
        wrapping: bool,
        op: IntOp,
        children: [u32; 2],
    ) -> Result<Piece, ExportError> {
        self.app(
            self.reference(model(if wrapping { "wrapping" } else { "checked" }))?,
            Some(self.reference(kind_spec(k))?),
            [
                self.reference(op_spec(op))?,
                self.node_ref(children[0])?,
                self.node_ref(children[1])?,
            ],
        )
    }
    fn leaf_total(&self, c: &RewriteCertificate, id: u32) -> Result<Piece, ExportError> {
        let term = get_term(c, id)?;
        match term.node {
            TermNode::Const(n) => self.app(
                self.reference(model("total_const"))?,
                None,
                [
                    self.reference(kind_spec(term.kind))?,
                    self.integer(n)?,
                    self.fixed(true),
                ],
            ),
            TermNode::InputVar(v) => self.app(
                self.reference(model("total_var"))?,
                None,
                [self.reference(kind_spec(term.kind))?, self.nat(v as u128)?],
            ),
            _ => Err(WitnessError::UnprovenTotality.into()),
        }
    }
    fn total(
        &self,
        c: &RewriteCertificate,
        id: u32,
        guard: Option<u32>,
    ) -> Result<Piece, ExportError> {
        let Some(guard) = guard else {
            return self.leaf_total(c, id);
        };
        let premise = c
            .proofs
            .get(guard as usize)
            .ok_or(WitnessError::InvalidProofIndex)?;
        let (leaf, proof) = if premise.from == id {
            (premise.to, self.proof_ref(guard)?)
        } else if premise.to == id {
            (
                premise.from,
                self.app(
                    self.reference(standard(&["Eq"], "symm"))?,
                    None,
                    [self.proof_ref(guard)?],
                )?,
            )
        } else {
            return Err(WitnessError::UnprovenTotality.into());
        };
        self.app(
            self.reference(model("total_of_eq"))?,
            Some(self.reference(kind_spec(get_term(c, id)?.kind))?),
            [proof, self.leaf_total(c, leaf)?],
        )
    }
    fn rule(
        &self,
        c: &RewriteCertificate,
        step: &Equality,
        rule: u16,
        s: witness::Substitution,
        guard: Option<u32>,
    ) -> Result<Piece, ExportError> {
        let lemma = *RULES
            .get(rule.checked_sub(1).ok_or(WitnessError::UnknownRule)? as usize)
            .ok_or(WitnessError::UnknownRule)?;
        let kind = self.reference(kind_spec(get_term(c, step.from)?.kind))?;
        let [x, y, z] = s.bindings;
        let function = self.reference(model(lemma))?;
        match rule {
            1..=5 | 9..=11 => self.app(function, Some(kind), [self.node_ref(x)?]),
            6..=8 | 17..=18 => self.app(
                function,
                Some(kind),
                [self.node_ref(x)?, self.total(c, x, guard)?],
            ),
            12..=13 => self.app(function, Some(kind), [self.node_ref(x)?, self.node_ref(y)?]),
            14..=15 => self.app(
                function,
                Some(kind),
                [self.node_ref(x)?, self.node_ref(y)?, self.node_ref(z)?],
            ),
            16 => self.app(
                function,
                Some(kind),
                [
                    self.node_ref(x)?,
                    self.node_ref(y)?,
                    self.total(c, y, guard)?,
                ],
            ),
            19..=24 => {
                let (TermNode::Const(a), TermNode::Const(b), TermNode::Const(r)) = (
                    get_term(c, x)?.node,
                    get_term(c, y)?.node,
                    get_term(c, step.to)?.node,
                ) else {
                    return Err(WitnessError::InvalidRuleInstance.into());
                };
                self.app(
                    function,
                    Some(kind),
                    [
                        self.integer(a)?,
                        self.integer(b)?,
                        self.integer(r)?,
                        self.fixed(true),
                        self.fixed(true),
                        self.fixed(true),
                        self.fixed(true),
                    ],
                )
            }
            _ => Err(WitnessError::UnknownRule.into()),
        }
    }
    fn proof(&self, c: &RewriteCertificate, step: &Equality) -> Result<Piece, ExportError> {
        match step.reason {
            Reason::Refl => Ok(self.fixed(false)),
            Reason::Rule {
                rule,
                substitution,
                guard,
            } => self.rule(c, step, rule, substitution, guard),
            Reason::Symmetry { proof } => self.app(
                self.reference(standard(&["Eq"], "symm"))?,
                None,
                [self.proof_ref(proof)?],
            ),
            Reason::Transitivity { first, second } => self.app(
                self.reference(standard(&["Eq"], "trans"))?,
                None,
                [self.proof_ref(first)?, self.proof_ref(second)?],
            ),
            Reason::Congruence { children } => {
                let (wrapping, op) = match get_term(c, step.from)?.node {
                    TermNode::Checked { op, .. } => (false, op),
                    TermNode::Wrapping { op, .. } => (true, op),
                    _ => return Err(WitnessError::InvalidEquality.into()),
                };
                self.app(
                    self.reference(model(if wrapping {
                        "congr_wrapping"
                    } else {
                        "congr_checked"
                    }))?,
                    Some(self.reference(kind_spec(get_term(c, step.from)?.kind))?),
                    [
                        self.reference(op_spec(op))?,
                        self.proof_ref(children[0])?,
                        self.proof_ref(children[1])?,
                    ],
                )
            }
        }
    }
}
fn get_term(c: &RewriteCertificate, id: u32) -> Result<&witness::Term, ExportError> {
    c.terms
        .get(id as usize)
        .ok_or(ExportError::Witness(WitnessError::InvalidTermIndex))
}

fn declaration_stats(spec: Spec, a: Stats, b: Stats) -> Result<Stats, ExportError> {
    let mut stats = a.combine(b)?.combine(name_stats(spec)?)?;
    stats.nodes = add(stats.nodes, 1)?;
    stats.depth = add(stats.depth, 1)?;
    stats.bytes = add(stats.bytes, 32)?;
    stats.metadata = add(stats.metadata, size_of::<Declaration>())?;
    Ok(stats)
}
struct Plan {
    stats: Stats,
    declarations: usize,
}
fn lower(
    c: &RewriteCertificate,
    build: bool,
    budget: &mut Budget,
) -> Result<(Plan, Option<Module>), ExportError> {
    let count = add(add(c.terms.len(), c.proofs.len())?, 1)?;
    bound(add(count, 1)?, budget.limits.max_declarations)?;
    let maker = Maker { build };
    let mut declarations = if build { vec_space(count)? } else { Vec::new() };
    let mut stats = Stats::default();
    for (id, term) in c.terms.iter().enumerate() {
        if !build {
            budget.work(32)?;
        }
        let result = maker.app(
            maker.reference(model("Sem"))?,
            None,
            [maker.reference(kind_spec(term.kind))?],
        )?;
        let body = match term.node {
            TermNode::Const(n) => maker.app(
                maker.reference(model("const"))?,
                None,
                [
                    maker.reference(kind_spec(term.kind))?,
                    maker.integer(n)?,
                    maker.fixed(true),
                ],
            )?,
            TermNode::InputVar(v) => maker.app(
                maker.reference(model("var"))?,
                None,
                [
                    maker.reference(kind_spec(term.kind))?,
                    maker.nat(v as u128)?,
                ],
            )?,
            TermNode::Checked { op, children } => {
                maker.operation(term.kind, false, op, children)?
            }
            TermNode::Wrapping { op, children } => {
                maker.operation(term.kind, true, op, children)?
            }
        };
        let spec = index_spec(b'n', id as u32, false);
        stats = stats.combine(declaration_stats(spec, result.stats, body.stats)?)?;
        if build {
            declarations.push(Declaration::Definition(Definition {
                name: make_name(spec)?,
                binders: Vec::new(),
                result: result.ast.ok_or(ExportError::LeanSyntax)?,
                body: body.ast.ok_or(ExportError::LeanSyntax)?,
                kind: DefinitionKind::Def,
            }));
        }
    }
    for (id, step) in c.proofs.iter().enumerate() {
        if !build {
            budget.work(32)?;
        }
        let proposition = maker.eq(maker.node_ref(step.from)?, maker.node_ref(step.to)?)?;
        let proof = maker.proof(c, step)?;
        let spec = index_spec(b'p', id as u32, false);
        stats = stats.combine(declaration_stats(spec, proposition.stats, proof.stats)?)?;
        if build {
            declarations.push(Declaration::Theorem {
                name: make_name(spec)?,
                binders: Vec::new(),
                proposition: proposition.ast.ok_or(ExportError::LeanSyntax)?,
                proof: proof.ast.ok_or(ExportError::LeanSyntax)?,
            });
        }
    }
    let proposition = maker.eq(maker.node_ref(c.input)?, maker.node_ref(c.output)?)?;
    let proof = maker.proof_ref(c.conclusion)?;
    let final_name = Spec {
        prefix: &[],
        last: Component::Static("replay"),
        rooted: false,
    };
    stats = stats.combine(declaration_stats(
        final_name,
        proposition.stats,
        proof.stats,
    )?)?;
    if build {
        declarations.push(Declaration::Theorem {
            name: make_name(final_name)?,
            binders: Vec::new(),
            proposition: proposition.ast.ok_or(ExportError::LeanSyntax)?,
            proof: proof.ast.ok_or(ExportError::LeanSyntax)?,
        });
    }
    let namespace = Spec {
        prefix: &["Vesper"],
        last: Component::Static("ReplayWitness"),
        rooted: false,
    };
    let ns = name_stats(namespace)?;
    // Module + Namespace declaration + the fixed ModuleName import node.
    // ModuleName's String components are payload, not AST Ident nodes.
    stats.nodes = add(stats.nodes, add(ns.nodes, 3)?)?;
    stats.depth = add(stats.depth.max(ns.depth), 2)?;
    stats.names = add(stats.names, add(ns.names, "RewriteWitness".len())?)?;
    stats.bytes = add(stats.bytes, add(mul(ns.bytes, 2)?, 128)?)?;
    stats.metadata = add(
        stats.metadata,
        add(
            ns.metadata,
            2 * size_of::<Declaration>() + size_of::<Module>() + size_of::<ModuleName>() + 128,
        )?,
    )?;
    let module = if build {
        let mut outer = vec_space(1)?;
        outer.push(Declaration::Namespace {
            name: make_name(namespace)?,
            declarations,
        });
        let mut imports = vec_space(1)?;
        imports.push(ModuleName::new("RewriteWitness").map_err(|_| ExportError::LeanSyntax)?);
        Some(Module {
            imports,
            declarations: outer,
        })
    } else {
        None
    };
    Ok((
        Plan {
            stats,
            declarations: count + 1,
        },
        module,
    ))
}

fn kind_tag(k: IntegerKind) -> u8 {
    match k {
        IntegerKind::U8 => 0,
        IntegerKind::U32 => 1,
        IntegerKind::U64 => 2,
        IntegerKind::I64 => 3,
    }
}
fn op_tag(op: IntOp) -> u8 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}
const CERT_DOMAIN: &[u8] = b"vesper.replay.certificate/1\0";
const BIND_DOMAIN: &[u8] = b"vesper.replay.binding/1\0";
fn certificate_bytes(c: &RewriteCertificate) -> Result<usize, ExportError> {
    let mut n = CERT_DOMAIN.len() + 4 + 16 + 12;
    for term in &c.terms {
        n = add(
            n,
            2 + match term.node {
                TermNode::Const(_) => 16,
                TermNode::InputVar(_) => 4,
                _ => 9,
            },
        )?;
    }
    for p in &c.proofs {
        n = add(
            n,
            9 + match p.reason {
                Reason::Refl => 0,
                Reason::Rule { guard, .. } => 16 + 4 * usize::from(guard.is_some()),
                Reason::Congruence { .. } | Reason::Transitivity { .. } => 8,
                Reason::Symmetry { .. } => 4,
            },
        )?;
    }
    Ok(n)
}
fn certificate_hash(c: &RewriteCertificate) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(CERT_DOMAIN);
    h.update(&c.schema_version.to_le_bytes());
    h.update(&c.arithmetic_version.to_le_bytes());
    h.update(&(c.terms.len() as u64).to_le_bytes());
    h.update(&(c.proofs.len() as u64).to_le_bytes());
    for id in [c.input, c.output, c.conclusion] {
        h.update(&id.to_le_bytes());
    }
    for term in &c.terms {
        h.update(&[kind_tag(term.kind)]);
        match term.node {
            TermNode::Const(n) => {
                h.update(&[0]);
                h.update(&n.to_le_bytes());
            }
            TermNode::InputVar(id) => {
                h.update(&[1]);
                h.update(&id.to_le_bytes());
            }
            TermNode::Checked { op, children } | TermNode::Wrapping { op, children } => {
                h.update(&[
                    if matches!(term.node, TermNode::Checked { .. }) {
                        2
                    } else {
                        3
                    },
                    op_tag(op),
                ]);
                for child in children {
                    h.update(&child.to_le_bytes());
                }
            }
        }
    }
    for proof in &c.proofs {
        h.update(&proof.from.to_le_bytes());
        h.update(&proof.to.to_le_bytes());
        match proof.reason {
            Reason::Refl => {
                h.update(&[0]);
            }
            Reason::Rule {
                rule,
                substitution,
                guard,
            } => {
                h.update(&[1]);
                h.update(&rule.to_le_bytes());
                h.update(&[substitution.len]);
                for id in substitution.bindings {
                    h.update(&id.to_le_bytes());
                }
                if let Some(id) = guard {
                    h.update(&[1]);
                    h.update(&id.to_le_bytes());
                } else {
                    h.update(&[0]);
                }
            }
            Reason::Congruence { children } => {
                h.update(&[2]);
                for id in children {
                    h.update(&id.to_le_bytes());
                }
            }
            Reason::Symmetry { proof } => {
                h.update(&[3]);
                h.update(&proof.to_le_bytes());
            }
            Reason::Transitivity { first, second } => {
                h.update(&[4]);
                h.update(&first.to_le_bytes());
                h.update(&second.to_le_bytes());
            }
        }
    }
    *h.finalize().as_bytes()
}

#[derive(Default)]
struct AstCount {
    nodes: usize,
    bytes: usize,
}
fn type_encoding(ty: &Type) -> Result<(u8, Option<i128>), ExportError> {
    match ty {
        Type::IntLiteral(n) => Ok((5, Some(*n))),
        Type::Named(name, args) if args.is_empty() => Ok((
            match name.as_str() {
                "u8" => 1,
                "u32" => 2,
                "u64" => 3,
                "i64" => 4,
                _ => return Err(WitnessError::TypeMismatch.into()),
            },
            None,
        )),
        _ => Err(WitnessError::TypeMismatch.into()),
    }
}
fn expr_tag(e: &Expr) -> Result<u8, ExportError> {
    Ok(match &e.kind {
        ExprKind::Literal(Literal::Int(_)) => 0,
        ExprKind::Name(_) => 1,
        ExprKind::Binary(op, ..) => match op.as_str() {
            "+" => 2,
            "-" => 3,
            "*" => 4,
            _ => return Err(WitnessError::UnsupportedAst.into()),
        },
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Name(n) => match n.as_str() {
                "wrapping_add" => 5,
                "wrapping_sub" => 6,
                "wrapping_mul" => 7,
                _ => return Err(WitnessError::UnsupportedAst.into()),
            },
            _ => return Err(WitnessError::UnsupportedAst.into()),
        },
        _ => return Err(WitnessError::UnsupportedAst.into()),
    })
}
fn update(h: &mut Option<&mut blake3::Hasher>, bytes: &[u8]) {
    if let Some(h) = h {
        h.update(bytes);
    }
}
fn span_hash(h: &mut Option<&mut blake3::Hasher>, e: &Expr) -> Result<(), ExportError> {
    update(
        h,
        &u64::try_from(e.span.start)
            .map_err(|_| ExportError::Limit)?
            .to_le_bytes(),
    );
    update(
        h,
        &u64::try_from(e.span.end)
            .map_err(|_| ExportError::Limit)?
            .to_le_bytes(),
    );
    Ok(())
}
fn name_hash(h: &mut Option<&mut blake3::Hasher>, name: &str) -> Result<(), ExportError> {
    update(
        h,
        &u64::try_from(name.len())
            .map_err(|_| ExportError::Limit)?
            .to_le_bytes(),
    );
    update(h, name.as_bytes());
    Ok(())
}
fn ast_pass(
    e: &Expr,
    types: &TypeMap,
    max_nodes: usize,
    budget: &mut Budget,
    mut hash: Option<&mut blake3::Hasher>,
) -> Result<AstCount, ExportError> {
    budget.metadata(mul(max_nodes, size_of::<&Expr>())?)?;
    let mut stack = vec_space(max_nodes)?;
    stack.push(e);
    let mut count = AstCount::default();
    while let Some(e) = stack.pop() {
        budget.work(1)?;
        count.nodes = add(count.nodes, 1)?;
        bound(count.nodes, max_nodes)?;
        let tag = expr_tag(e)?;
        let ty = types
            .get(&(e.span.start, e.span.end))
            .ok_or(WitnessError::MissingType)?;
        let (type_tag, representative) = type_encoding(ty)?;
        count.bytes = add(count.bytes, 18 + 16 * usize::from(representative.is_some()))?;
        update(&mut hash, &[tag]);
        span_hash(&mut hash, e)?;
        update(&mut hash, &[type_tag]);
        if let Some(n) = representative {
            update(&mut hash, &n.to_le_bytes());
        }
        match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => {
                count.bytes = add(count.bytes, 16)?;
                update(&mut hash, &n.to_le_bytes());
            }
            ExprKind::Name(name) => {
                count.bytes = add(count.bytes, add(8, name.len())?)?;
                name_hash(&mut hash, name)?;
            }
            ExprKind::Binary(_, a, b) => {
                stack.push(b);
                stack.push(a);
            }
            ExprKind::Call {
                callee,
                types: generic,
                args,
            } => {
                if !generic.is_empty()
                    || args.len() != 2
                    || args.iter().any(|a| a.pass != Pass::In || a.explicit)
                {
                    return Err(WitnessError::UnsupportedAst.into());
                }
                let ExprKind::Name(name) = &callee.kind else {
                    return Err(WitnessError::UnsupportedAst.into());
                };
                if types.contains_key(&(callee.span.start, callee.span.end)) {
                    return Err(WitnessError::InvalidIntrinsicOrigin.into());
                }
                count.nodes = add(count.nodes, 1)?;
                bound(count.nodes, max_nodes)?;
                count.bytes = add(count.bytes, add(25, name.len())?)?;
                name_hash(&mut hash, name)?;
                span_hash(&mut hash, callee)?;
                update(&mut hash, &[0]);
                stack.push(&args[1].expr);
                stack.push(&args[0].expr);
            }
            _ => return Err(WitnessError::UnsupportedAst.into()),
        }
        // A binary DFS stack is bounded by the validated total occurrence cap.
        if stack.len() > max_nodes {
            return Err(ExportError::Limit);
        }
    }
    Ok(count)
}
fn binding_hash(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    c: &RewriteCertificate,
    limits: WitnessLimits,
    budget: &mut Budget,
) -> Result<[u8; 32], ExportError> {
    let a = ast_pass(source, types, limits.max_input_nodes, budget, None)?;
    let b = ast_pass(candidate, types, limits.max_output_nodes, budget, None)?;
    budget.work(add(BIND_DOMAIN.len() + 4 + 18, add(a.bytes, b.bytes)?)?)?;
    let mut h = blake3::Hasher::new();
    h.update(BIND_DOMAIN);
    h.update(&c.schema_version.to_le_bytes());
    h.update(&c.arithmetic_version.to_le_bytes());
    h.update(&[0]);
    h.update(&(a.nodes as u64).to_le_bytes());
    ast_pass(source, types, limits.max_input_nodes, budget, Some(&mut h))?;
    h.update(&[1]);
    h.update(&(b.nodes as u64).to_le_bytes());
    ast_pass(
        candidate,
        types,
        limits.max_output_nodes,
        budget,
        Some(&mut h),
    )?;
    Ok(*h.finalize().as_bytes())
}

fn export_bound(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    c: &RewriteCertificate,
    witness_limits: WitnessLimits,
    mut budget: Budget,
) -> Result<PreparedLean, ExportError> {
    budget.metadata(add(
        mul(c.terms.capacity(), size_of::<witness::Term>())?,
        mul(c.proofs.capacity(), size_of::<Equality>())?,
    )?)?;
    let (plan, _) = lower(c, false, &mut budget)?;
    bound(plan.stats.nodes, budget.limits.max_ast_nodes)?;
    bound(plan.stats.depth, budget.limits.max_ast_depth)?;
    bound(plan.stats.names, budget.limits.max_name_bytes)?;
    bound(plan.stats.bytes, budget.limits.max_source_bytes)?;
    // Cumulative formatting includes EVERY subtree/declaration, output geometric
    // growth and name/argument vectors; not merely peak/max-declaration storage.
    let render_copies = mul(plan.stats.bytes, mul(plan.stats.depth + 1, 16)?)?;
    budget.metadata(add(
        mul(plan.stats.metadata, 4)?,
        add(render_copies, mul(plan.stats.bytes, 4)?)?,
    )?)?;
    budget.work(add(
        mul(plan.stats.nodes, 8)?,
        add(plan.stats.metadata, render_copies)?,
    )?)?;
    let binding_blake3 = binding_hash(source, candidate, types, c, witness_limits, &mut budget)?;
    budget.work(add(c.terms.len(), c.proofs.len())?)?;
    budget.work(certificate_bytes(c)?)?;
    let certificate_blake3 = certificate_hash(c);
    budget.work(model_source().len())?;
    let model_blake3 = *blake3::hash(model_source().as_bytes()).as_bytes();
    // Reserve work for rendering's final hash before allocating any AST payload.
    budget.work(plan.stats.bytes)?;
    let (_, module) = lower(c, true, &mut budget)?;
    let manifest = ReplayManifest {
        format_version: 1,
        model_version: 1,
        schema_version: c.schema_version,
        arithmetic_version: c.arithmetic_version,
        module_name: "ReplayGenerated",
        theorem: "Vesper.ReplayWitness.replay",
        scope: ReplayScope::MathModelEq,
        kind: get_term(c, c.input)?.kind,
        input: c.input,
        output: c.output,
        conclusion: c.conclusion,
        term_count: c.terms.len(),
        proof_count: c.proofs.len(),
        declaration_count: plan.declarations,
        ast_node_count: plan.stats.nodes,
        ast_depth: plan.stats.depth,
        name_bytes: plan.stats.names,
        source_bytes_upper_bound: plan.stats.bytes,
        model_blake3,
        certificate_blake3,
        binding_blake3,
    };
    Ok(PreparedLean {
        module: module.ok_or(ExportError::LeanSyntax)?,
        manifest,
        budget,
    })
}

pub fn export_rewrite(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    witness_limits: WitnessLimits,
    export_limits: ExportLimits,
) -> Result<PreparedLean, ExportError> {
    let budget = Budget::new(export_limits)?;
    witness::verify_rewrite(source, candidate, types, certificate, witness_limits)?;
    export_bound(
        source,
        candidate,
        types,
        certificate,
        witness_limits,
        budget,
    )
}
pub fn prepare_rewrite(
    source: &Expr,
    types: &TypeMap,
    witness_limits: WitnessLimits,
    export_limits: ExportLimits,
) -> Result<Option<PreparedReplay>, ExportError> {
    let budget = Budget::new(export_limits)?;
    match propose_rewrite(source, types, witness_limits)? {
        RewriteOutcome::Barrier | RewriteOutcome::Unchanged => Ok(None),
        RewriteOutcome::Candidate(mut proposal) => {
            witness::verify_with_budget(
                source,
                &proposal.candidate,
                types,
                &proposal.certificate,
                &mut proposal.budget,
            )?;
            let lean = export_bound(
                source,
                &proposal.candidate,
                types,
                &proposal.certificate,
                witness_limits,
                budget,
            )?;
            Ok(Some(PreparedReplay {
                candidate: proposal.candidate,
                certificate: proposal.certificate,
                lean,
            }))
        }
    }
}
