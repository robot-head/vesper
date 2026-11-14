//! Identity-based lowering of sealed whole-body Expr syntax. This stage creates
//! expanded syntax and an expected core AST, never checked-program authority.
use super::authority::SourceBindings;
use crate::{
    ast::*,
    lexer::{self, TokenKind as K},
};
use std::collections::{BTreeMap, BTreeSet};
use vesper_forge::{
    BinderId, DefId, DefKind, ExpansionId, HygieneHost, Origin, Resolution, TokenKind,
    typed_quote::{ExprShape, ExprStatement, ResolvedBody, TokenProvenance},
};
use vesper_syntax::macro_source::FunctionId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LowerLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_output_bytes: usize,
    pub max_map_entries: usize,
    pub max_identifier_bytes: usize,
    pub max_work: usize,
}
impl Default for LowerLimits {
    fn default() -> Self {
        Self {
            max_nodes: 8192,
            max_depth: 48,
            max_output_bytes: 256 * 1024,
            max_map_entries: 32768,
            max_identifier_bytes: 256,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl LowerLimits {
    pub const HARD: Self = Self {
        max_nodes: 65536,
        max_depth: 128,
        max_output_bytes: 1024 * 1024,
        max_map_entries: 65536,
        max_identifier_bytes: 1024,
        max_work: 128 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), LowerError> {
        let h = Self::HARD;
        for (n, max) in [
            (self.max_nodes, h.max_nodes),
            (self.max_depth, h.max_depth),
            (self.max_output_bytes, h.max_output_bytes),
            (self.max_map_entries, h.max_map_entries),
            (self.max_identifier_bytes, h.max_identifier_bytes),
            (self.max_work, h.max_work),
        ] {
            if n == 0 || n > max {
                return Err(LowerError::InvalidLimits);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Nodes,
    Depth,
    Output,
    Maps,
    Identifier,
    Work,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LowerError {
    InvalidLimits,
    Budget(Resource),
    ForeignHost,
    ForeignSource,
    InvalidInventory(&'static str),
    UnknownGlobal(DefId),
    Unsupported(&'static str),
    InvalidLiteral,
    Allocation,
}
impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "macro lowering: {self:?}")
    }
}
impl std::error::Error for LowerError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentOrigin {
    Source { span: Span },
    Generated { expansion: ExpansionId },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowerSegment {
    pub span: Span,
    pub origin: SegmentOrigin,
}
/// An alpha-renamed declaration identifier derived from a sealed host parameter.
#[derive(Debug)]
pub struct ParameterRename {
    ordinal: usize,
    original: Span,
    name: String,
}
impl ParameterRename {
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    pub fn original(&self) -> &Span {
        &self.original
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
#[derive(Debug)]
pub struct LoweredBody {
    function: Option<FunctionId>,
    parameter_renames: Vec<ParameterRename>,
    source: String,
    expected: Expr,
    segments: Vec<LowerSegment>,
    work_used: usize,
}
impl LoweredBody {
    pub fn function_id(&self) -> Option<FunctionId> {
        self.function
    }
    pub fn parameter_renames(&self) -> &[ParameterRename] {
        &self.parameter_renames
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn expected(&self) -> &Expr {
        &self.expected
    }
    pub fn segments(&self) -> &[LowerSegment] {
        &self.segments
    }
    pub fn work_used(&self) -> usize {
        self.work_used
    }
    /// Structural comparison only, after the coordinator checks the complete
    /// candidate. It ignores Expr/Pattern positions, never semantic fields.
    pub fn shape_matches(&self, actual: &Expr, limits: LowerLimits) -> Result<bool, LowerError> {
        shape_matches(&self.expected, actual, limits)
    }
}
struct Budget {
    remaining: usize,
    nodes: usize,
    limits: LowerLimits,
}
impl Budget {
    fn new(limits: LowerLimits) -> Result<Self, LowerError> {
        limits.validate()?;
        Ok(Self {
            remaining: limits.max_work,
            nodes: 0,
            limits,
        })
    }
    fn charge(&mut self, n: usize) -> Result<(), LowerError> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or(LowerError::Budget(Resource::Work))?;
        Ok(())
    }
    fn node(&mut self, depth: usize) -> Result<usize, LowerError> {
        self.charge(1)?;
        if depth > self.limits.max_depth {
            return Err(LowerError::Budget(Resource::Depth));
        }
        if self.nodes >= self.limits.max_nodes {
            return Err(LowerError::Budget(Resource::Nodes));
        }
        let id = self.nodes;
        self.nodes += 1;
        Ok(id)
    }
}
fn checked_add(a: usize, b: usize) -> Result<usize, LowerError> {
    a.checked_add(b).ok_or(LowerError::Budget(Resource::Work))
}
fn identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), LowerError> {
    v.try_reserve(n).map_err(|_| LowerError::Allocation)
}
struct Builder<'a> {
    body: &'a ResolvedBody,
    host: &'a HygieneHost,
    bindings: &'a SourceBindings,
    budget: Budget,
    refs: BTreeMap<usize, Resolution>,
    binders: BTreeMap<usize, BinderId>,
    names: BTreeMap<BinderId, String>,
    reserved: BTreeSet<String>,
    active: BTreeSet<BinderId>,
    seen_refs: BTreeSet<usize>,
    seen_binders: BTreeSet<BinderId>,
    anchors: Vec<usize>,
    tuple_closes: BTreeMap<usize, usize>,
    parameters: BTreeSet<BinderId>,
    parameter_renames: Vec<ParameterRename>,
    counter: usize,
}
impl<'a> Builder<'a> {
    fn new(
        body: &'a ResolvedBody,
        host: &'a HygieneHost,
        bindings: &'a SourceBindings,
        limits: LowerLimits,
    ) -> Result<Self, LowerError> {
        let mut budget = Budget::new(limits)?;
        body.validate_host(host)
            .map_err(|_| LowerError::ForeignHost)?;
        if body.session() != host.session() {
            return Err(LowerError::ForeignHost);
        }
        budget.charge(body.original_source().len())?;
        if *blake3::hash(body.original_source().as_bytes()).as_bytes() != bindings.source_hash() {
            return Err(LowerError::ForeignSource);
        }
        // Syntax hashes alone do not distinguish two bindings of the same file
        // to different package identities. Validate both supported package records
        // before accepting even a literal-only body with no global references.
        for package in [bindings.source_package(), super::authority::BUILTIN_PACKAGE] {
            let trusted = bindings
                .registry()
                .package(package)
                .ok_or(LowerError::InvalidInventory("trusted package"))?;
            let actual = host
                .registry()
                .package(package)
                .ok_or(LowerError::InvalidInventory("host package"))?;
            if trusted.dependencies.len() != actual.dependencies.len() {
                return Err(LowerError::InvalidInventory("package identity"));
            }
            budget.charge(checked_add(
                checked_add(trusted.name.len(), actual.name.len())?,
                actual.dependencies.len(),
            )?)?;
            if actual != trusted {
                return Err(LowerError::InvalidInventory("package identity"));
            }
        }
        let fragment = body.fragment();
        if fragment.tokens.len() > limits.max_map_entries
            || body.provenance().len() != fragment.tokens.len()
        {
            return Err(LowerError::InvalidInventory("token provenance"));
        }
        if fragment.binders.len() > limits.max_nodes
            || fragment.references.len() > limits.max_nodes
            || fragment.operations.len() > limits.max_map_entries
            || fragment.expansions.len() > limits.max_map_entries
        {
            return Err(LowerError::Budget(Resource::Nodes));
        }
        let mut this = Self {
            body,
            host,
            bindings,
            budget,
            refs: BTreeMap::new(),
            binders: BTreeMap::new(),
            names: BTreeMap::new(),
            reserved: BTreeSet::new(),
            active: BTreeSet::new(),
            seen_refs: BTreeSet::new(),
            seen_binders: BTreeSet::new(),
            anchors: Vec::new(),
            tuple_closes: BTreeMap::new(),
            parameters: BTreeSet::new(),
            parameter_renames: Vec::new(),
            counter: 0,
        };
        for name in bindings.global_names() {
            this.reserve_name(name)?;
        }
        for (i, token) in fragment.tokens.iter().enumerate() {
            this.budget.charge(1)?;
            let text = host.text(token.text());
            this.budget.charge(text.len())?;
            let scopes = token.scopes().iter().count();
            this.budget.charge(
                scopes
                    .checked_mul(2)
                    .ok_or(LowerError::Budget(Resource::Work))?,
            )?;
            host.validate_token(token)
                .map_err(|_| LowerError::InvalidInventory("host token"))?;
            if token.kind() == TokenKind::Ident {
                this.reserve_name(text)?;
            }
            match (&body.provenance()[i], token.origin()) {
                (TokenProvenance::Source { span }, Origin::Source { package })
                    if package == bindings.source_package()
                        && body.original_source().get(span.clone()) == Some(text) => {}
                (TokenProvenance::Generated { expansion, .. }, Origin::Expansion(origin))
                    if *expansion == origin && fragment.expansions.contains_key(expansion) => {}
                _ => return Err(LowerError::InvalidInventory("token origin")),
            }
        }
        for (id, record) in &fragment.expansions {
            this.budget.charge(1)?;
            if id != &record.id
                || host.expansion(*id).map_err(|_| LowerError::ForeignHost)? != record
            {
                return Err(LowerError::InvalidInventory("expansion"));
            }
        }
        for reference in &fragment.references {
            this.budget.charge(1)?;
            if reference.token >= fragment.tokens.len()
                || this
                    .refs
                    .insert(reference.token, reference.resolution)
                    .is_some()
            {
                return Err(LowerError::InvalidInventory("reference"));
            }
        }
        for binder in &fragment.binders {
            this.budget.charge(1)?;
            if binder.token >= fragment.tokens.len()
                || this.binders.insert(binder.token, binder.id).is_some()
                || this.names.contains_key(&binder.id)
            {
                return Err(LowerError::InvalidInventory("binder"));
            }
            let raw = host.text(fragment.tokens[binder.token].text());
            this.budget.charge(body.parameters().len())?;
            let self_parameter = raw == "self"
                && body
                    .parameters()
                    .iter()
                    .any(|parameter| parameter.binder() == binder.id);
            if self_parameter {
                // SelfKw is a real parameter-name token, never a newly allowed
                // body-local keyword binder. The sealed parameter provenance is
                // checked below before its fresh name can reach rendering.
                this.budget.charge(raw.len())?;
                if raw.len() > limits.max_identifier_bytes {
                    return Err(LowerError::Budget(Resource::Identifier));
                }
            } else {
                this.validate_binder(raw)?;
            }
            let name = this.fresh()?;
            this.names.insert(binder.id, name);
        }
        // Parameter declarations are real header tokens appended by the sealed
        // host. Their binders are active before the body, but their spellings are
        // alpha-renamed alongside all body-local binders to prevent global capture.
        this.budget.charge(
            body.parameters()
                .len()
                .checked_mul(
                    std::mem::size_of::<ParameterRename>() + std::mem::size_of::<BinderId>() * 4,
                )
                .ok_or(LowerError::Budget(Resource::Work))?,
        )?;
        reserve(&mut this.parameter_renames, body.parameters().len())?;
        for (ordinal, parameter) in body.parameters().iter().enumerate() {
            this.budget.charge(1)?;
            if parameter.ordinal() != ordinal
                || this.binders.get(&parameter.token()) != Some(&parameter.binder())
                || !this.parameters.insert(parameter.binder())
                || !this.active.insert(parameter.binder())
                || !this.seen_binders.insert(parameter.binder())
            {
                return Err(LowerError::InvalidInventory("parameter binder"));
            }
            let provenance = body
                .provenance()
                .get(parameter.token())
                .ok_or(LowerError::InvalidInventory("parameter token"))?;
            if !matches!(provenance, TokenProvenance::Source { span } if span == parameter.name_span())
            {
                return Err(LowerError::InvalidInventory("parameter provenance"));
            }
            let name = this
                .names
                .get(&parameter.binder())
                .ok_or(LowerError::InvalidInventory("parameter name"))?;
            this.budget.charge(name.len())?;
            this.parameter_renames.push(ParameterRename {
                ordinal,
                original: parameter.name_span().clone(),
                name: name.clone(),
            });
        }
        if body.function_id().is_none() && !body.parameters().is_empty() {
            return Err(LowerError::InvalidInventory("parameter function"));
        }
        for operation in &fragment.operations {
            this.budget.charge(1)?;
            if operation.token >= fragment.tokens.len() {
                return Err(LowerError::InvalidInventory("operation"));
            }
            for origin in &operation.origins {
                this.budget.charge(1)?;
                match origin {
                    Origin::Source { package } if *package == bindings.source_package() => {}
                    Origin::Expansion(id) if fragment.expansions.contains_key(id) => {}
                    _ => return Err(LowerError::InvalidInventory("operation origin")),
                }
            }
        }
        Ok(this)
    }
    fn reserve_name(&mut self, name: &str) -> Result<(), LowerError> {
        if name.len() > self.budget.limits.max_output_bytes {
            return Err(LowerError::Budget(Resource::Identifier));
        }
        self.budget.charge(name.len())?;
        if !self.reserved.contains(name) {
            if self.reserved.len() >= self.budget.limits.max_map_entries {
                return Err(LowerError::Budget(Resource::Maps));
            }
            self.reserved.insert(name.into());
        }
        Ok(())
    }
    fn validate_binder(&mut self, name: &str) -> Result<(), LowerError> {
        if name.len() > self.budget.limits.max_identifier_bytes {
            return Err(LowerError::Budget(Resource::Identifier));
        }
        self.budget.charge(name.len())?;
        if !identifier(name) || name == "_" {
            return Err(LowerError::Unsupported("binding spelling"));
        }
        let lex = lexer::lex_with_limits(
            name,
            lexer::LexLimits {
                max_source_bytes: self.budget.limits.max_identifier_bytes,
                max_tokens: 1,
                max_diagnostics: 1,
            },
        )
        .map_err(|_| LowerError::InvalidInventory("binding token"))?;
        if lex.limit.is_some()
            || !lex.diagnostics.is_empty()
            || lex
                .tokens
                .first()
                .is_none_or(|t| t.kind != K::Lower || t.span != (0..name.len()))
        {
            return Err(LowerError::Unsupported("binding keyword"));
        }
        Ok(())
    }
    fn fresh(&mut self) -> Result<String, LowerError> {
        loop {
            self.budget.charge(1)?;
            let mut n = self.counter;
            let mut digits = 1;
            while n >= 10 {
                n /= 10;
                digits += 1;
            }
            let len = checked_add("__forge_local_".len(), digits)?;
            if len > self.budget.limits.max_identifier_bytes {
                return Err(LowerError::Budget(Resource::Identifier));
            }
            self.budget.charge(len)?;
            let name = format!("__forge_local_{}", self.counter);
            self.counter = self
                .counter
                .checked_add(1)
                .ok_or(LowerError::Budget(Resource::Identifier))?;
            if !self.reserved.contains(&name) {
                if self.reserved.len() >= self.budget.limits.max_map_entries {
                    return Err(LowerError::Budget(Resource::Maps));
                }
                self.reserved.insert(name.clone());
                return Ok(name);
            }
        }
    }
    fn anchor(&mut self, token: usize, depth: usize) -> Result<Span, LowerError> {
        if token >= self.body.fragment().tokens.len() {
            return Err(LowerError::InvalidInventory("shape token"));
        }
        let id = self.budget.node(depth)?;
        if id != self.anchors.len() {
            return Err(LowerError::InvalidInventory("node identity"));
        }
        reserve(&mut self.anchors, 1)?;
        self.anchors.push(token);
        Ok(id..checked_add(id, 1)?)
    }
    fn text(&self, token: usize) -> Result<&str, LowerError> {
        let t = self
            .body
            .fragment()
            .tokens
            .get(token)
            .ok_or(LowerError::InvalidInventory("shape token"))?;
        Ok(self.host.text(t.text()))
    }
    fn punctuation(&mut self, token: usize, text: &str) -> Result<(), LowerError> {
        self.budget.charge(1)?;
        if self.text(token)? != text {
            return Err(LowerError::InvalidInventory("delimiter"));
        }
        Ok(())
    }
    fn expr(&mut self, shape: &ExprShape, depth: usize) -> Result<Expr, LowerError> {
        if depth > self.budget.limits.max_depth {
            return Err(LowerError::Budget(Resource::Depth));
        }
        let (kind, token) = match shape {
            ExprShape::Ref { tokens } => {
                let first = *tokens
                    .first()
                    .ok_or(LowerError::InvalidInventory("empty path"))?;
                let mut previous = None;
                for token in tokens {
                    self.budget.charge(1)?;
                    if *token >= self.body.fragment().tokens.len()
                        || previous.is_some_and(|p| p >= *token)
                    {
                        return Err(LowerError::InvalidInventory("path order"));
                    }
                    previous = Some(*token);
                }
                if !self.seen_refs.insert(first) {
                    return Err(LowerError::InvalidInventory("duplicate reference shape"));
                }
                let resolution = self
                    .refs
                    .get(&first)
                    .copied()
                    .ok_or(LowerError::InvalidInventory("missing resolution"))?;
                let name = match resolution {
                    Resolution::Local(id) => {
                        if tokens.len() != 1 || !self.active.contains(&id) {
                            return Err(LowerError::InvalidInventory("out-of-scope binder"));
                        }
                        let name = self
                            .names
                            .get(&id)
                            .ok_or(LowerError::InvalidInventory("unknown binder"))?;
                        self.budget.charge(name.len())?;
                        name.clone()
                    }
                    Resolution::Global(def) => {
                        let trusted = self
                            .bindings
                            .registry()
                            .definition(def)
                            .ok_or(LowerError::UnknownGlobal(def))?;
                        let actual = self
                            .host
                            .registry()
                            .definition(def)
                            .ok_or(LowerError::UnknownGlobal(def))?;
                        self.budget
                            .charge(checked_add(trusted.path.len(), actual.path.len())?)?;
                        if actual != trusted {
                            return Err(LowerError::InvalidInventory("global registry"));
                        }
                        if !matches!(trusted.kind, DefKind::Function | DefKind::EffectOp) {
                            return Err(LowerError::UnknownGlobal(def));
                        }
                        let name = self
                            .bindings
                            .core_name(def)
                            .map_err(|_| LowerError::UnknownGlobal(def))?;
                        if name.len() > self.budget.limits.max_output_bytes {
                            return Err(LowerError::Budget(Resource::Identifier));
                        }
                        self.budget.charge(name.len())?;
                        name.into()
                    }
                };
                (ExprKind::Name(name), first)
            }
            ExprShape::Literal { token } => {
                let raw = self.text(*token)?;
                let n = raw.len();
                if n > self.budget.limits.max_output_bytes {
                    return Err(LowerError::Budget(Resource::Output));
                }
                self.budget
                    .charge(n.checked_mul(2).ok_or(LowerError::Budget(Resource::Work))?)?;
                let t = &self.body.fragment().tokens[*token];
                let raw = self.text(*token)?;
                let literal = match t.kind() {
                    TokenKind::Int => {
                        let normalized = raw.replace('_', "");
                        let value = if normalized.starts_with("0x") || normalized.starts_with("0X")
                        {
                            u64::from_str_radix(&normalized[2..], 16)
                        } else {
                            normalized.parse::<u64>()
                        }
                        .map_err(|_| LowerError::InvalidLiteral)?;
                        Literal::Int(i128::from(value))
                    }
                    TokenKind::Str => Literal::String(
                        serde_json::from_str::<String>(raw)
                            .map_err(|_| LowerError::InvalidLiteral)?,
                    ),
                    TokenKind::Ident if raw == "true" => Literal::Bool(true),
                    TokenKind::Ident if raw == "false" => Literal::Bool(false),
                    _ => return Err(LowerError::InvalidLiteral),
                };
                (ExprKind::Literal(literal), *token)
            }
            ExprShape::Call {
                callee,
                open,
                close,
                args,
            } => {
                self.punctuation(*open, "(")?;
                self.punctuation(*close, ")")?;
                let ExprShape::Ref { tokens } = callee.as_ref() else {
                    return Err(LowerError::Unsupported("higher-order call"));
                };
                let first = tokens
                    .first()
                    .ok_or(LowerError::InvalidInventory("callee"))?;
                if !matches!(self.refs.get(first), Some(Resolution::Global(_))) {
                    return Err(LowerError::Unsupported("local or indirect call"));
                }
                let callee = Box::new(self.expr(callee, depth + 1)?);
                let mut lowered = Vec::new();
                if args.len() > self.budget.limits.max_nodes {
                    return Err(LowerError::Budget(Resource::Nodes));
                }
                reserve(&mut lowered, args.len())?;
                for arg in args {
                    lowered.push(Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: self.expr(arg, depth + 1)?,
                    });
                }
                (
                    ExprKind::Call {
                        callee,
                        types: Vec::new(),
                        args: lowered,
                    },
                    *open,
                )
            }
            ExprShape::Tuple {
                open,
                close,
                values,
            } => {
                self.punctuation(*open, "(")?;
                self.punctuation(*close, ")")?;
                if open >= close || values.len() > self.budget.limits.max_nodes {
                    return Err(LowerError::InvalidInventory("tuple bounds"));
                }
                let storage = values
                    .len()
                    .checked_mul(std::mem::size_of::<Expr>())
                    .and_then(|n| n.checked_add(std::mem::size_of::<(usize, usize)>() * 2))
                    .ok_or(LowerError::Budget(Resource::Work))?;
                self.budget.charge(storage)?;
                if self.tuple_closes.insert(*open, *close).is_some() {
                    return Err(LowerError::InvalidInventory("tuple delimiter identity"));
                }
                let mut lowered = Vec::new();
                lowered
                    .try_reserve_exact(values.len())
                    .map_err(|_| LowerError::Allocation)?;
                for value in values {
                    lowered.push(self.expr(value, depth + 1)?);
                }
                (ExprKind::Tuple(lowered), *open)
            }
            ExprShape::Block {
                open,
                close,
                statements,
                tail,
            } => {
                self.punctuation(*open, "{")?;
                self.punctuation(*close, "}")?;
                let mut lowered = Vec::new();
                let mut locals = Vec::new();
                if statements.len() > self.budget.limits.max_nodes {
                    return Err(LowerError::Budget(Resource::Nodes));
                }
                reserve(&mut lowered, statements.len())?;
                for statement in statements {
                    self.budget.charge(1)?;
                    match statement {
                        ExprStatement::Let { binder, value } => {
                            let value = self.expr(value, depth + 1)?;
                            let id = *self
                                .binders
                                .get(binder)
                                .ok_or(LowerError::InvalidInventory("missing binder"))?;
                            if !self.seen_binders.insert(id) || !self.active.insert(id) {
                                return Err(LowerError::InvalidInventory("duplicate binder"));
                            }
                            locals.push(id);
                            let name = self
                                .names
                                .get(&id)
                                .ok_or(LowerError::InvalidInventory("binder name"))?;
                            self.budget.charge(name.len())?;
                            let name = name.clone();
                            let pat = Pattern {
                                kind: PatternKind::Bind(name),
                                span: self.anchor(*binder, depth + 1)?,
                            };
                            lowered.push(Statement::Let {
                                pat,
                                ty: Type::Infer,
                                value,
                            });
                        }
                        ExprStatement::Expr(e) => {
                            lowered.push(Statement::Expr(self.expr(e, depth + 1)?))
                        }
                    }
                }
                let tail = tail
                    .as_ref()
                    .map(|e| self.expr(e, depth + 1).map(Box::new))
                    .transpose()?;
                for id in locals {
                    self.active.remove(&id);
                }
                (
                    ExprKind::Block {
                        statements: lowered,
                        tail,
                    },
                    *open,
                )
            }
        };
        Ok(Expr {
            kind,
            span: self.anchor(token, depth)?,
        })
    }
}

fn preflight_shape(shape: &ExprShape, budget: &mut Budget) -> Result<(), LowerError> {
    let mut stack = vec![(shape, 0usize)];
    let mut nodes = 0usize;
    while let Some((shape, depth)) = stack.pop() {
        budget.charge(std::mem::size_of::<Expr>())?;
        if depth > budget.limits.max_depth {
            return Err(LowerError::Budget(Resource::Depth));
        }
        nodes = checked_add(nodes, 1)?;
        if nodes > budget.limits.max_nodes {
            return Err(LowerError::Budget(Resource::Nodes));
        }
        let max_nodes = budget.limits.max_nodes;
        let mut add = |e, level| -> Result<(), LowerError> {
            if stack.len() >= max_nodes {
                return Err(LowerError::Budget(Resource::Nodes));
            }
            reserve(&mut stack, 1)?;
            stack.push((e, level));
            Ok(())
        };
        match shape {
            ExprShape::Ref { .. } | ExprShape::Literal { .. } => {}
            ExprShape::Call { callee, args, .. } => {
                add(callee.as_ref(), depth + 1)?;
                for a in args {
                    add(a, depth + 1)?;
                }
            }
            ExprShape::Tuple { values, .. } => {
                if values.len() > max_nodes {
                    return Err(LowerError::Budget(Resource::Nodes));
                }
                budget.charge(
                    values
                        .len()
                        .checked_mul(std::mem::size_of::<Expr>())
                        .ok_or(LowerError::Budget(Resource::Work))?,
                )?;
                for value in values {
                    add(value, depth + 1)?;
                }
            }
            ExprShape::Block {
                statements, tail, ..
            } => {
                for s in statements {
                    budget.charge(std::mem::size_of::<Statement>())?;
                    nodes = checked_add(nodes, 1)?;
                    if nodes > budget.limits.max_nodes {
                        return Err(LowerError::Budget(Resource::Nodes));
                    }
                    match s {
                        ExprStatement::Let { value, .. } => {
                            budget.charge(std::mem::size_of::<Pattern>())?;
                            nodes = checked_add(nodes, 1)?;
                            add(value, depth + 1)?;
                        }
                        ExprStatement::Expr(e) => add(e, depth + 1)?,
                    }
                }
                if let Some(t) = tail {
                    add(t.as_ref(), depth + 1)?;
                }
            }
        }
    }
    Ok(())
}
struct Emitter<'a> {
    source: String,
    segments: Vec<LowerSegment>,
    body: &'a ResolvedBody,
    anchors: &'a [usize],
    tuple_closes: &'a BTreeMap<usize, usize>,
    budget: &'a mut Budget,
}
impl Emitter<'_> {
    fn emit(&mut self, text: &str, token: usize) -> Result<Span, LowerError> {
        let end = self
            .source
            .len()
            .checked_add(text.len())
            .filter(|n| *n <= self.budget.limits.max_output_bytes)
            .ok_or(LowerError::Budget(Resource::Output))?;
        if self.segments.len() >= self.budget.limits.max_map_entries {
            return Err(LowerError::Budget(Resource::Maps));
        }
        self.budget.charge(checked_add(
            text.len(),
            std::mem::size_of::<LowerSegment>(),
        )?)?;
        let provenance = self
            .body
            .provenance()
            .get(token)
            .ok_or(LowerError::InvalidInventory("render origin"))?;
        let origin = match provenance {
            TokenProvenance::Source { span } => SegmentOrigin::Source { span: span.clone() },
            TokenProvenance::Generated { expansion, .. } => SegmentOrigin::Generated {
                expansion: *expansion,
            },
        };
        // Explicit geometric growth prepays both new storage and old payload
        // copies before either reserve. Tuple punctuation may cause many writes.
        if end > self.source.capacity() {
            let capacity = end
                .max(self.source.capacity().saturating_mul(2))
                .max(32)
                .min(self.budget.limits.max_output_bytes);
            self.budget
                .charge(checked_add(capacity, self.source.len())?)?;
            self.source
                .try_reserve_exact(capacity - self.source.len())
                .map_err(|_| LowerError::Allocation)?;
        }
        if self.segments.len() == self.segments.capacity() {
            let capacity = self
                .segments
                .len()
                .saturating_mul(2)
                .max(8)
                .min(self.budget.limits.max_map_entries);
            self.budget.charge(
                checked_add(capacity, self.segments.len())?
                    .checked_mul(std::mem::size_of::<LowerSegment>())
                    .ok_or(LowerError::Budget(Resource::Work))?,
            )?;
            self.segments
                .try_reserve_exact(capacity - self.segments.len())
                .map_err(|_| LowerError::Allocation)?;
        }
        let span = self.source.len()..end;
        self.source.push_str(text);
        self.segments.push(LowerSegment {
            span: span.clone(),
            origin,
        });
        Ok(span)
    }
    fn anchor(&self, span: &Span) -> Result<usize, LowerError> {
        self.anchors
            .get(span.start)
            .copied()
            .ok_or(LowerError::InvalidInventory("render identity"))
    }
    fn expr(&mut self, e: &mut Expr, depth: usize) -> Result<(), LowerError> {
        self.budget.charge(1)?;
        if depth > self.budget.limits.max_depth {
            return Err(LowerError::Budget(Resource::Depth));
        }
        let token = self.anchor(&e.span)?;
        let start = self.source.len();
        match &mut e.kind {
            ExprKind::Name(name) => {
                self.emit(name, token)?;
            }
            ExprKind::Literal(lit) => match lit {
                Literal::Int(n) => {
                    let text = n.to_string();
                    self.emit(&text, token)?;
                }
                Literal::Bool(b) => {
                    self.emit(if *b { "true" } else { "false" }, token)?;
                }
                Literal::String(value) => {
                    let mut length = 2usize;
                    for ch in value.chars() {
                        self.budget.charge(ch.len_utf8())?;
                        let n = match ch {
                            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                            c if c < '\u{20}' => 6,
                            c => c.len_utf8(),
                        };
                        length = checked_add(length, n)?;
                    }
                    let remaining = self
                        .budget
                        .limits
                        .max_output_bytes
                        .checked_sub(self.source.len())
                        .ok_or(LowerError::Budget(Resource::Output))?;
                    if length > remaining {
                        return Err(LowerError::Budget(Resource::Output));
                    }
                    self.budget.charge(length)?;
                    let text =
                        serde_json::to_string(value).map_err(|_| LowerError::InvalidLiteral)?;
                    if text.len() != length {
                        return Err(LowerError::InvalidInventory("string rendering"));
                    }
                    self.emit(&text, token)?;
                }
                Literal::Float(_) => return Err(LowerError::Unsupported("float literal")),
            },
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                if !types.is_empty() {
                    return Err(LowerError::InvalidInventory("render type arguments"));
                }
                self.expr(callee, depth + 1)?;
                self.emit("(", token)?;
                for (i, arg) in args.iter_mut().enumerate() {
                    if i > 0 {
                        self.emit(", ", token)?;
                    }
                    if arg.pass != Pass::In || arg.explicit {
                        return Err(LowerError::InvalidInventory("render argument pass"));
                    }
                    self.expr(&mut arg.expr, depth + 1)?;
                }
                self.emit(")", token)?;
            }
            ExprKind::Tuple(values) => {
                let close = *self
                    .tuple_closes
                    .get(&token)
                    .ok_or(LowerError::InvalidInventory("tuple closing anchor"))?;
                self.emit("(", token)?;
                let singleton = values.len() == 1;
                for (i, value) in values.iter_mut().enumerate() {
                    if i > 0 {
                        self.emit(", ", token)?;
                    }
                    self.expr(value, depth + 1)?;
                }
                if singleton {
                    self.emit(",", token)?;
                }
                self.emit(")", close)?;
            }
            ExprKind::Block { statements, tail } => {
                self.emit("{", token)?;
                for statement in statements {
                    self.emit(" ", token)?;
                    match statement {
                        Statement::Let { pat, ty, value } => {
                            if *ty != Type::Infer {
                                return Err(LowerError::InvalidInventory("render annotation"));
                            }
                            let binder = self.anchor(&pat.span)?;
                            self.emit("let ", binder)?;
                            let PatternKind::Bind(name) = &pat.kind else {
                                return Err(LowerError::InvalidInventory("render pattern"));
                            };
                            pat.span = self.emit(name, binder)?;
                            self.emit(" = ", binder)?;
                            self.expr(value, depth + 1)?;
                            self.emit(";", binder)?;
                        }
                        Statement::Expr(e) => {
                            self.expr(e, depth + 1)?;
                            self.emit(";", token)?;
                        }
                    }
                }
                if let Some(tail) = tail {
                    self.emit(" ", token)?;
                    self.expr(tail, depth + 1)?;
                }
                self.emit(" }", token)?;
            }
            _ => return Err(LowerError::Unsupported("render expression")),
        }
        e.span = start..self.source.len();
        Ok(())
    }
}
/// Lower only a sealed whole-body resolution under its actual current host and
/// syntax-derived declaration bindings. Semantic checking remains a later stage.
pub fn lower_body(
    body: &ResolvedBody,
    host: &HygieneHost,
    bindings: &SourceBindings,
    limits: LowerLimits,
) -> Result<LoweredBody, LowerError> {
    if !matches!(body.shape(), ExprShape::Block { .. }) {
        return Err(LowerError::Unsupported("whole body must be a block"));
    }
    let mut builder = Builder::new(body, host, bindings, limits)?;
    preflight_shape(body.shape(), &mut builder.budget)?;
    let mut expected = builder.expr(body.shape(), 0)?;
    if builder.seen_refs.len() != builder.refs.len()
        || builder.seen_binders.len() != builder.names.len()
        || builder.active != builder.parameters
    {
        return Err(LowerError::InvalidInventory("unused resolution inventory"));
    }
    let mut emitter = Emitter {
        source: String::new(),
        segments: Vec::new(),
        body,
        anchors: &builder.anchors,
        tuple_closes: &builder.tuple_closes,
        budget: &mut builder.budget,
    };
    emitter.expr(&mut expected, 0)?;
    let source = std::mem::take(&mut emitter.source);
    let segments = std::mem::take(&mut emitter.segments);
    drop(emitter);
    let work_used = limits.max_work - builder.budget.remaining;
    Ok(LoweredBody {
        function: body.function_id(),
        parameter_renames: builder.parameter_renames,
        source,
        expected,
        segments,
        work_used,
    })
}
/// Bounded comparison of exactly the supported semantic AST fields. Source
/// positions are intentionally independent between relative body and full file.
pub fn shape_matches(
    expected: &Expr,
    actual: &Expr,
    limits: LowerLimits,
) -> Result<bool, LowerError> {
    let mut budget = Budget::new(limits)?;
    let mut stack = vec![(expected, actual, 0usize)];
    while let Some((a, b, depth)) = stack.pop() {
        budget.node(depth)?;
        let mut push = |a, b| -> Result<(), LowerError> {
            if stack.len() >= limits.max_nodes {
                return Err(LowerError::Budget(Resource::Nodes));
            }
            reserve(&mut stack, 1)?;
            stack.push((a, b, depth + 1));
            Ok(())
        };
        match (&a.kind, &b.kind) {
            (ExprKind::Name(a), ExprKind::Name(b)) => {
                if a.len() > limits.max_output_bytes || b.len() > limits.max_output_bytes {
                    return Err(LowerError::Budget(Resource::Identifier));
                }
                budget.charge(checked_add(a.len(), b.len())?)?;
                if a != b {
                    return Ok(false);
                }
            }
            (ExprKind::Literal(Literal::Int(a)), ExprKind::Literal(Literal::Int(b))) if a == b => {}
            (ExprKind::Literal(Literal::Bool(a)), ExprKind::Literal(Literal::Bool(b)))
                if a == b => {}
            (ExprKind::Literal(Literal::String(a)), ExprKind::Literal(Literal::String(b))) => {
                if a.len() > limits.max_output_bytes || b.len() > limits.max_output_bytes {
                    return Err(LowerError::Budget(Resource::Output));
                }
                budget.charge(checked_add(a.len(), b.len())?)?;
                if a != b {
                    return Ok(false);
                }
            }
            (
                ExprKind::Call {
                    callee: a,
                    types: at,
                    args: aa,
                },
                ExprKind::Call {
                    callee: b,
                    types: bt,
                    args: ba,
                },
            ) => {
                if !at.is_empty() || !bt.is_empty() || aa.len() != ba.len() {
                    return Ok(false);
                }
                push(a, b)?;
                for (a, b) in aa.iter().zip(ba) {
                    if a.pass != b.pass || a.explicit != b.explicit {
                        return Ok(false);
                    }
                    push(&a.expr, &b.expr)?;
                }
            }
            (ExprKind::Tuple(a), ExprKind::Tuple(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                if a.len() > limits.max_nodes {
                    return Err(LowerError::Budget(Resource::Nodes));
                }
                for (a, b) in a.iter().zip(b) {
                    push(a, b)?;
                }
            }
            (
                ExprKind::Block {
                    statements: aa,
                    tail: at,
                },
                ExprKind::Block {
                    statements: ba,
                    tail: bt,
                },
            ) => {
                if aa.len() != ba.len() || at.is_some() != bt.is_some() {
                    return Ok(false);
                }
                for (a, b) in aa.iter().zip(ba) {
                    budget.node(depth + 1)?;
                    match (a, b) {
                        (Statement::Expr(a), Statement::Expr(b)) => push(a, b)?,
                        (
                            Statement::Let {
                                pat: ap,
                                ty: at,
                                value: av,
                            },
                            Statement::Let {
                                pat: bp,
                                ty: bt,
                                value: bv,
                            },
                        ) => {
                            budget.node(depth + 1)?;
                            if !matches!(at, Type::Infer) || !matches!(bt, Type::Infer) {
                                return Ok(false);
                            }
                            let (PatternKind::Bind(a), PatternKind::Bind(b)) = (&ap.kind, &bp.kind)
                            else {
                                return Ok(false);
                            };
                            if a.len() > limits.max_identifier_bytes
                                || b.len() > limits.max_identifier_bytes
                            {
                                return Err(LowerError::Budget(Resource::Identifier));
                            }
                            budget.charge(checked_add(a.len(), b.len())?)?;
                            if a != b {
                                return Ok(false);
                            }
                            push(av, bv)?;
                        }
                        _ => return Ok(false),
                    }
                }
                if let (Some(a), Some(b)) = (at, bt) {
                    push(a, b)?;
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}
