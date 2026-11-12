//! Emitted-code authority gate ([T-MacroConfinement], strengthened).
//!
//! Inputs are a *trusted* resolved operation/call/handler graph supplied by
//! the compiler, the macro policies (`emits`) and caller capability ceilings.
//! Every operation that any expansion contributed to is checked, whichever
//! package defines its target: injected dependency calls and handler
//! constructions are checked exactly like calls into the macro's own package.
//!
//! Authority requirements are computed transitively over the graph (cycles
//! are fine; reachability is a fixpoint). Residual effect rows are carried
//! for information only: a locally handled effect does not remove the
//! authority its implementation needs. Unknown nodes, unresolved edges and
//! handler constructions without a rule fail closed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::identity::{ExpansionId, SessionId};
use crate::registry::{DefId, PackageId};
use crate::resolve::{OperationKind, ResolvedFragment};
use crate::token::Origin;

/// Version of serialized graphs, policies and audits.
pub const AUTHORITY_SCHEMA_VERSION: u32 = 1;

/// Package-qualified effect identity: `Net` of package A is not `Net` of B.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectId {
    pub package: PackageId,
    pub name: String,
}

impl EffectId {
    pub fn new(package: PackageId, name: impl Into<String>) -> Self {
        Self {
            package,
            name: name.into(),
        }
    }
}

impl fmt::Display for EffectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.package, self.name)
    }
}

/// A typed authority argument, compared exactly.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AuthorityArg {
    Def(DefId),
    Text(String),
    Int(u64),
}

impl fmt::Display for AuthorityArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Def(d) => write!(f, "{d}"),
            Self::Text(s) => write!(f, "{s:?}"),
            Self::Int(n) => write!(f, "{n}"),
        }
    }
}

/// One concrete authority, e.g. `Ffi[LibA]`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Authority {
    pub effect: EffectId,
    pub args: Vec<AuthorityArg>,
}

impl Authority {
    pub fn new(effect: EffectId) -> Self {
        Self {
            effect,
            args: Vec::new(),
        }
    }
    pub fn with_arg(mut self, a: AuthorityArg) -> Self {
        self.args.push(a);
        self
    }
}

impl fmt::Display for Authority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.effect)?;
        if !self.args.is_empty() {
            f.write_str("[")?;
            for (i, a) in self.args.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{a}")?;
            }
            f.write_str("]")?;
        }
        Ok(())
    }
}

/// A permission entry in an `emits` policy or a ceiling.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AuthorityGrant {
    Exact(Authority),
    /// The effect with any arguments.
    AnyArguments(EffectId),
}

impl AuthorityGrant {
    pub fn permits(&self, a: &Authority) -> bool {
        match self {
            Self::Exact(g) => g == a,
            Self::AnyArguments(e) => *e == a.effect,
        }
    }
}

fn permitted(grants: &[AuthorityGrant], a: &Authority) -> bool {
    grants.iter().any(|g| g.permits(a))
}

// ----- graph -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphEdge {
    Call(DefId),
    /// The definition constructs a handler for `effect` implemented by `handler`.
    Handler {
        effect: DefId,
        handler: DefId,
    },
    /// A reference the compiler could not resolve (e.g. late-bound). Fails closed.
    Unresolved {
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityNode {
    pub def: DefId,
    /// Authority the definition's own body uses directly.
    pub direct: BTreeSet<Authority>,
    /// Lexical residual row after local handlers. Informational only.
    pub residual_row: BTreeSet<EffectId>,
    pub edges: Vec<GraphEdge>,
}

impl AuthorityNode {
    pub fn new(def: DefId) -> Self {
        Self {
            def,
            direct: BTreeSet::new(),
            residual_row: BTreeSet::new(),
            edges: Vec::new(),
        }
    }
    pub fn direct(mut self, a: Authority) -> Self {
        self.direct.insert(a);
        self
    }
    pub fn calls(mut self, d: DefId) -> Self {
        self.edges.push(GraphEdge::Call(d));
        self
    }
    pub fn edge(mut self, e: GraphEdge) -> Self {
        self.edges.push(e);
        self
    }
}

/// Authority needed to *construct* a handler for an effect declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandlerRule {
    pub effect: DefId,
    pub requires: BTreeSet<Authority>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphWire {
    schema_version: u32,
    graph_version: String,
    nodes: Vec<AuthorityNode>,
    handler_rules: Vec<HandlerRule>,
}

/// Trusted, versioned resolved-operation/call/handler graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GraphWire", into = "GraphWire")]
pub struct AuthorityGraph {
    graph_version: String,
    nodes: BTreeMap<DefId, AuthorityNode>,
    handler_rules: BTreeMap<DefId, BTreeSet<Authority>>,
}

impl TryFrom<GraphWire> for AuthorityGraph {
    type Error = ForgeError;
    fn try_from(w: GraphWire) -> Result<Self, ForgeError> {
        check_schema("authority graph", w.schema_version)?;
        let mut g = Self::new(w.graph_version);
        for n in w.nodes {
            g.add_node(n)?;
        }
        for r in w.handler_rules {
            g.add_handler_rule(r)?;
        }
        Ok(g)
    }
}

impl From<AuthorityGraph> for GraphWire {
    fn from(g: AuthorityGraph) -> Self {
        Self {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            graph_version: g.graph_version,
            nodes: g.nodes.into_values().collect(),
            handler_rules: g
                .handler_rules
                .into_iter()
                .map(|(effect, requires)| HandlerRule { effect, requires })
                .collect(),
        }
    }
}

fn check_schema(what: &'static str, found: u32) -> Result<(), ForgeError> {
    if found != AUTHORITY_SCHEMA_VERSION {
        return Err(ForgeError::Schema {
            what,
            found,
            expected: AUTHORITY_SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Why a requirement could not be computed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GraphFault {
    MissingNode {
        def: DefId,
        path: Vec<DefId>,
    },
    Unresolved {
        at: DefId,
        note: String,
        path: Vec<DefId>,
    },
    MissingHandlerRule {
        effect: DefId,
        path: Vec<DefId>,
    },
}

impl fmt::Display for GraphFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNode { def, path } => {
                write!(
                    f,
                    "definition {def} is not in the authority graph (via {})",
                    Path(path)
                )
            }
            Self::Unresolved { at, note, path } => {
                write!(f, "unresolved edge in {at}: {note} (via {})", Path(path))
            }
            Self::MissingHandlerRule { effect, path } => write!(
                f,
                "no handler-construction rule for effect {effect} (via {})",
                Path(path)
            ),
        }
    }
}

struct Path<'a>(&'a [DefId]);

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("operation");
        }
        for (i, d) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" -> ")?;
            }
            write!(f, "{d}")?;
        }
        Ok(())
    }
}

/// Transitive requirements, each with a witness path of definitions.
pub type Requirements = BTreeMap<Authority, Vec<DefId>>;

impl AuthorityGraph {
    pub fn new(graph_version: impl Into<String>) -> Self {
        Self {
            graph_version: graph_version.into(),
            nodes: BTreeMap::new(),
            handler_rules: BTreeMap::new(),
        }
    }

    pub fn graph_version(&self) -> &str {
        &self.graph_version
    }

    pub fn add_node(&mut self, n: AuthorityNode) -> Result<(), ForgeError> {
        if self.nodes.contains_key(&n.def) {
            return Err(ForgeError::DuplicateEntry {
                what: "authority node",
                id: n.def.to_string(),
            });
        }
        self.nodes.insert(n.def, n);
        Ok(())
    }

    pub fn add_handler_rule(&mut self, r: HandlerRule) -> Result<(), ForgeError> {
        if self.handler_rules.contains_key(&r.effect) {
            return Err(ForgeError::DuplicateEntry {
                what: "handler rule",
                id: r.effect.to_string(),
            });
        }
        self.handler_rules.insert(r.effect, r.requires);
        Ok(())
    }

    /// Transitive authority requirements of one operation.
    pub fn requirements(&self, op: &OperationKind) -> Result<Requirements, GraphFault> {
        let mut reqs = Requirements::new();
        let mut parent: BTreeMap<DefId, Option<DefId>> = BTreeMap::new();
        let mut queue = VecDeque::new();
        let root_path = |parent: &BTreeMap<DefId, Option<DefId>>, d: DefId| {
            let mut p = vec![d];
            let mut cur = d;
            while let Some(Some(up)) = parent.get(&cur) {
                p.push(*up);
                cur = *up;
            }
            p.reverse();
            p
        };
        match *op {
            OperationKind::Use { def } => {
                parent.insert(def, None);
                queue.push_back(def);
            }
            OperationKind::Handler { effect, handler } => {
                self.add_rule(effect, &[], &mut reqs)?;
                parent.insert(handler, None);
                queue.push_back(handler);
            }
        }
        while let Some(d) = queue.pop_front() {
            let path = root_path(&parent, d);
            let node = self.nodes.get(&d).ok_or_else(|| GraphFault::MissingNode {
                def: d,
                path: path.clone(),
            })?;
            for a in &node.direct {
                reqs.entry(a.clone()).or_insert_with(|| path.clone());
            }
            for e in &node.edges {
                let next = match e {
                    GraphEdge::Call(c) => *c,
                    GraphEdge::Handler { effect, handler } => {
                        self.add_rule(*effect, &path, &mut reqs)?;
                        *handler
                    }
                    GraphEdge::Unresolved { note } => {
                        return Err(GraphFault::Unresolved {
                            at: d,
                            note: note.clone(),
                            path,
                        });
                    }
                };
                if let std::collections::btree_map::Entry::Vacant(v) = parent.entry(next) {
                    v.insert(Some(d));
                    queue.push_back(next);
                }
            }
        }
        Ok(reqs)
    }

    fn add_rule(
        &self,
        effect: DefId,
        path: &[DefId],
        reqs: &mut Requirements,
    ) -> Result<(), GraphFault> {
        let rule =
            self.handler_rules
                .get(&effect)
                .ok_or_else(|| GraphFault::MissingHandlerRule {
                    effect,
                    path: path.to_vec(),
                })?;
        for a in rule {
            reqs.entry(a.clone()).or_insert_with(|| path.to_vec());
        }
        Ok(())
    }
}

// ----- policies --------------------------------------------------------------

/// A macro package's declared `macro_emits_effects` for one macro.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MacroPolicyWire", into = "MacroPolicyWire")]
pub struct MacroPolicy {
    pub macro_def: DefId,
    pub version: u64,
    pub emits: Vec<AuthorityGrant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MacroPolicyWire {
    schema_version: u32,
    macro_def: DefId,
    version: u64,
    emits: Vec<AuthorityGrant>,
}

impl TryFrom<MacroPolicyWire> for MacroPolicy {
    type Error = ForgeError;
    fn try_from(w: MacroPolicyWire) -> Result<Self, ForgeError> {
        check_schema("macro policy", w.schema_version)?;
        Ok(Self {
            macro_def: w.macro_def,
            version: w.version,
            emits: w.emits,
        })
    }
}

impl From<MacroPolicy> for MacroPolicyWire {
    fn from(p: MacroPolicy) -> Self {
        Self {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            macro_def: p.macro_def,
            version: p.version,
            emits: p.emits,
        }
    }
}

/// The capability ceiling of the package containing the invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CeilingWire", into = "CeilingWire")]
pub struct CallerCeiling {
    pub package: PackageId,
    pub version: u64,
    pub grants: Vec<AuthorityGrant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CeilingWire {
    schema_version: u32,
    package: PackageId,
    version: u64,
    grants: Vec<AuthorityGrant>,
}

impl TryFrom<CeilingWire> for CallerCeiling {
    type Error = ForgeError;
    fn try_from(w: CeilingWire) -> Result<Self, ForgeError> {
        check_schema("caller ceiling", w.schema_version)?;
        Ok(Self {
            package: w.package,
            version: w.version,
            grants: w.grants,
        })
    }
}

impl From<CallerCeiling> for CeilingWire {
    fn from(c: CallerCeiling) -> Self {
        Self {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            package: c.package,
            version: c.version,
            grants: c.grants,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PolicySet {
    macros: BTreeMap<DefId, MacroPolicy>,
    ceilings: BTreeMap<PackageId, CallerCeiling>,
}

impl PolicySet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn macro_policy(mut self, p: MacroPolicy) -> Self {
        self.macros.insert(p.macro_def, p);
        self
    }
    pub fn ceiling(mut self, c: CallerCeiling) -> Self {
        self.ceilings.insert(c.package, c);
        self
    }
}

// ----- gate ------------------------------------------------------------------

/// Which bound rejected a requirement.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Denial {
    MacroEmits { macro_def: DefId, version: u64 },
    CallerCeiling { package: PackageId, version: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Violation {
    Exceeds {
        token: usize,
        expansion: ExpansionId,
        operation: OperationKind,
        requirement: Authority,
        witness: Vec<DefId>,
        denied_by: Vec<Denial>,
    },
    Graph {
        token: usize,
        expansion: ExpansionId,
        operation: OperationKind,
        fault: GraphFault,
    },
    MissingMacroPolicy {
        expansion: ExpansionId,
        macro_def: DefId,
    },
    MissingCeiling {
        package: PackageId,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceeds {
                token,
                expansion,
                operation,
                requirement,
                witness,
                denied_by,
            } => {
                write!(
                    f,
                    "FORGE-A001 token {token}: {} from expansion {expansion} requires {requirement} (via {}), denied by",
                    OpDisplay(operation),
                    Path(witness)
                )?;
                for d in denied_by {
                    match d {
                        Denial::MacroEmits { macro_def, version } => {
                            write!(f, " emits of macro {macro_def} v{version};")?
                        }
                        Denial::CallerCeiling { package, version } => {
                            write!(f, " ceiling of {package} v{version};")?
                        }
                    }
                }
                Ok(())
            }
            Self::Graph {
                token,
                expansion,
                operation,
                fault,
            } => write!(
                f,
                "FORGE-A002 token {token}: {} from expansion {expansion}: {fault}",
                OpDisplay(operation)
            ),
            Self::MissingMacroPolicy {
                expansion,
                macro_def,
            } => write!(
                f,
                "FORGE-A003 expansion {expansion}: no emits policy for macro {macro_def}"
            ),
            Self::MissingCeiling { package } => {
                write!(f, "FORGE-A004 no capability ceiling for caller {package}")
            }
        }
    }
}

struct OpDisplay<'a>(&'a OperationKind);

impl fmt::Display for OpDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            OperationKind::Use { def } => write!(f, "use of {def}"),
            OperationKind::Handler { effect, handler } => {
                write!(f, "handler {handler} for {effect}")
            }
        }
    }
}

/// Rejection with every violation, sorted and deduplicated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityRejection {
    pub violations: Vec<Violation>,
}

impl fmt::Display for AuthorityRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, v) in self.violations.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{v}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AuthorityRejection {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyStamp {
    pub expansion: ExpansionId,
    pub macro_def: DefId,
    pub macro_policy_version: u64,
    pub caller: PackageId,
    pub caller_ceiling_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationAudit {
    pub token: usize,
    pub operation: OperationKind,
    pub expansions: Vec<ExpansionId>,
    pub requirements: Vec<Authority>,
}

/// Evidence recorded with accepted output. It is conditional on the trusted
/// graph, registry and policies named by version here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityAudit {
    pub schema_version: u32,
    pub session: SessionId,
    pub graph_version: String,
    pub policies: Vec<PolicyStamp>,
    pub operations: Vec<OperationAudit>,
    /// Operations written entirely by source code; they are subject to the
    /// compiler's ordinary checks of that package, not to this gate.
    pub source_operations: Vec<OperationKind>,
}

impl AuthorityAudit {
    /// Canonical JSON (deterministic field and element order).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("audit serializes")
    }

    pub fn from_json(s: &str) -> Result<Self, ForgeError> {
        let a: Self = serde_json::from_str(s).map_err(|e| ForgeError::Decode(e.to_string()))?;
        check_schema("authority audit", a.schema_version)?;
        Ok(a)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedExpansion {
    pub fragment: ResolvedFragment,
    pub audit: AuthorityAudit,
}

/// Checks every expansion-contributed operation of `fragment`.
pub fn check_authority(
    session: SessionId,
    fragment: ResolvedFragment,
    graph: &AuthorityGraph,
    policies: &PolicySet,
) -> Result<AcceptedExpansion, AuthorityRejection> {
    let mut violations = BTreeSet::new();
    let mut stamps = BTreeMap::new();
    for rec in fragment.expansions.values() {
        let macro_policy = policies.macros.get(&rec.macro_def);
        let ceiling = policies.ceilings.get(&rec.root_caller);
        if macro_policy.is_none() {
            violations.insert(Violation::MissingMacroPolicy {
                expansion: rec.id,
                macro_def: rec.macro_def,
            });
        }
        if ceiling.is_none() {
            violations.insert(Violation::MissingCeiling {
                package: rec.root_caller,
            });
        }
        if let (Some(m), Some(c)) = (macro_policy, ceiling) {
            stamps.insert(
                rec.id,
                PolicyStamp {
                    expansion: rec.id,
                    macro_def: rec.macro_def,
                    macro_policy_version: m.version,
                    caller: rec.root_caller,
                    caller_ceiling_version: c.version,
                },
            );
        }
    }

    let mut audits = Vec::new();
    let mut source_operations = Vec::new();
    for op in &fragment.operations {
        let expansions: Vec<ExpansionId> = op
            .origins
            .iter()
            .filter_map(|o| match o {
                Origin::Expansion(x) => Some(*x),
                Origin::Source { .. } => None,
            })
            .collect();
        if expansions.is_empty() {
            source_operations.push(op.kind);
            continue;
        }
        let reqs = match graph.requirements(&op.kind) {
            Ok(r) => r,
            Err(fault) => {
                for &x in &expansions {
                    violations.insert(Violation::Graph {
                        token: op.token,
                        expansion: x,
                        operation: op.kind,
                        fault: fault.clone(),
                    });
                }
                continue;
            }
        };
        for &x in &expansions {
            // The expansion and all enclosing expansions bound the operation.
            let mut chain = Vec::new();
            let mut cur = Some(x);
            while let Some(c) = cur {
                let Some(rec) = fragment.expansions.get(&c) else {
                    break;
                };
                chain.push(rec);
                cur = match rec.site {
                    crate::host::InvocationSite::Expansion(p) => Some(p),
                    crate::host::InvocationSite::Source(_) => None,
                };
            }
            for (req, witness) in &reqs {
                let mut denied_by = Vec::new();
                for rec in &chain {
                    match policies.macros.get(&rec.macro_def) {
                        Some(m) if permitted(&m.emits, req) => {}
                        Some(m) => denied_by.push(Denial::MacroEmits {
                            macro_def: m.macro_def,
                            version: m.version,
                        }),
                        // Already reported as a missing policy.
                        None => {}
                    }
                }
                if let Some(rec) = chain.first() {
                    match policies.ceilings.get(&rec.root_caller) {
                        Some(c) if permitted(&c.grants, req) => {}
                        Some(c) => denied_by.push(Denial::CallerCeiling {
                            package: c.package,
                            version: c.version,
                        }),
                        None => {}
                    }
                }
                if !denied_by.is_empty() {
                    violations.insert(Violation::Exceeds {
                        token: op.token,
                        expansion: x,
                        operation: op.kind,
                        requirement: req.clone(),
                        witness: witness.clone(),
                        denied_by,
                    });
                }
            }
        }
        audits.push(OperationAudit {
            token: op.token,
            operation: op.kind,
            expansions,
            requirements: reqs.into_keys().collect(),
        });
    }

    if !violations.is_empty() {
        return Err(AuthorityRejection {
            violations: violations.into_iter().collect(),
        });
    }
    Ok(AcceptedExpansion {
        audit: AuthorityAudit {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            session,
            graph_version: graph.graph_version.clone(),
            policies: stamps.into_values().collect(),
            operations: audits,
            source_operations,
        },
        fragment,
    })
}
