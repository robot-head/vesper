//! Conservative source-interface compatibility for Vesper design §9.
//!
//! Inputs are resolved, trusted compiler exports, not arbitrary source syntax.
//! Unknown schema features, nominal types, or malformed binders never certify
//! compatibility. The checker describes caller witnesses; it does not claim to
//! synthesize and typecheck executable witness programs or prove contextual
//! equivalence of the Rust compiler.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct MapVisitor<V>(std::marker::PhantomData<V>);
    impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for MapVisitor<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map without duplicate identities")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, V>()? {
                if result.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate identity: {key}"
                    )));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(MapVisitor(std::marker::PhantomData))
}

pub const SCHEMA_VERSION: u32 = 1;
pub const SEMANTICS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Copy,
    Affine,
    Linear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resumption {
    Affine,
    Linear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    In,
    Inout,
    Sink,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    Unit,
    Never,
    Bool,
    U8,
    U32,
    U64,
    I64,
    F64,
    String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Type {
    Primitive {
        name: Primitive,
    },
    Parameter {
        index: usize,
    },
    /// Nominal arguments are invariant until their declaration publishes a
    /// separately verified variance contract.
    Named {
        name: String,
        arguments: Vec<Type>,
    },
    Tuple {
        elements: Vec<Type>,
    },
    Function {
        signature: Box<Function>,
    },
    Unsupported {
        description: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Generic {
    Type {
        bound: Mode,
    },
    /// The minimum continuation guarantee of every row instantiation.
    Row {
        minimum_resumption: Resumption,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "selection", rename_all = "snake_case", deny_unknown_fields)]
pub enum Projection {
    All,
    /// A path selects its complete subtree. Use All for the full root;
    /// empty sets/paths/components are rejected as ambiguous interface data.
    Paths {
        paths: BTreeSet<Vec<String>>,
    },
}
impl Projection {
    pub fn is_subset_of(&self, old: &Self) -> bool {
        let full = |p: &Self| {
            matches!(p, Self::All)
                || matches!(p, Self::Paths { paths } if paths.is_empty() || paths.iter().any(Vec::is_empty))
        };
        if full(old) {
            return true;
        }
        if full(self) {
            return false;
        }
        match (self, old) {
            (_, Self::All) => true,
            (Self::All, _) => false,
            (Self::Paths { paths: new }, Self::Paths { paths: old }) => {
                new.iter().all(|n| old.iter().any(|o| n.starts_with(o)))
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub pass: Pass,
    pub ty: Type,
    pub projection: Projection,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    /// Resolved, stable lexical binder identity, not a display-name wildcard.
    pub identity: String,
    pub effect: String,
    pub arguments: Vec<Type>,
    pub resumption: Resumption,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effects {
    pub capabilities: Vec<Capability>,
    pub row_variables: BTreeSet<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    /// Positional binders make generic names alpha-invariant.
    pub generics: Vec<Generic>,
    pub mode: Mode,
    pub parameters: Vec<Parameter>,
    pub result: Type,
    pub result_mode: Mode,
    pub effects: Effects,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "visibility", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fields {
    Closed,
    Public {
        #[serde(deserialize_with = "unique_map")]
        fields: BTreeMap<String, Type>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "declaration", rename_all = "snake_case", deny_unknown_fields)]
pub enum Declaration {
    Function {
        signature: Function,
    },
    Struct {
        generics: Vec<Generic>,
        mode: Mode,
        fields: Fields,
    },
    Enum {
        generics: Vec<Generic>,
        mode: Mode,
        open: bool,
        #[serde(deserialize_with = "unique_map")]
        variants: BTreeMap<String, Vec<Type>>,
    },
    Effect {
        generics: Vec<Generic>,
        sealed: bool,
        resumption: Resumption,
        #[serde(deserialize_with = "unique_map")]
        operations: BTreeMap<String, Function>,
    },
    Unsupported {
        description: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalType {
    pub arity: usize,
    /// Conservative kind upper bound, already checked by the exporting compiler.
    pub mode: Mode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub schema_version: u32,
    pub semantics_version: u32,
    #[serde(deserialize_with = "unique_map")]
    pub exports: BTreeMap<String, Declaration>,
    #[serde(deserialize_with = "unique_map")]
    pub external_types: BTreeMap<String, ExternalType>,
}
impl Default for Interface {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            semantics_version: SEMANTICS_VERSION,
            exports: BTreeMap::new(),
            external_types: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Patch,
    Minor,
    Major,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    Breaking,
    Unsupported,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub path: String,
    pub rule: String,
    pub kind: IssueKind,
    pub message: String,
    /// Human-readable counterexample scenario, not checked Vesper source.
    pub caller_witness: String,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReborrowAdapter {
    pub function_path: String,
    pub parameter: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub classification: Classification,
    pub issues: Vec<Issue>,
    /// An inout→in change is source-compatible only with these explicit call
    /// adapters. This checker never silently assumes they exist.
    pub required_adapters: Vec<ReborrowAdapter>,
}
impl Report {
    pub fn is_compatible(&self) -> bool {
        matches!(
            self.classification,
            Classification::Patch | Classification::Minor
        ) && self.required_adapters.is_empty()
    }
}

pub fn compare(old: &Interface, new: &Interface) -> Report {
    let mut checker = Checker {
        new,
        issues: vec![],
        adapters: vec![],
    };
    checker.validate(old, "old");
    checker.validate(new, "new");
    for (name, before) in &old.external_types {
        if let Some(after) = new.external_types.get(name)
            && (after.arity != before.arity || after.mode > before.mode)
        {
            checker.breaking(
                name,
                "SV-Nominal",
                "referenced external type contract became stricter",
                "A caller relies on the old nominal type's arity or discard/copy permissions.",
            );
        }
    }
    for (name, before) in &old.exports {
        if let Some(after) = new.exports.get(name) {
            checker.declaration(name, before, after);
        } else {
            checker.breaking(
                name,
                "SV-Export",
                "public declaration removed",
                "A caller still imports this public declaration.",
            );
        }
    }
    let classification = if checker
        .issues
        .iter()
        .any(|i| i.kind == IssueKind::Unsupported)
    {
        Classification::Unknown
    } else if !checker.issues.is_empty() || !checker.adapters.is_empty() {
        Classification::Major
    } else if old == new {
        Classification::Patch
    } else {
        Classification::Minor
    };
    Report {
        classification,
        issues: checker.issues,
        required_adapters: checker.adapters,
    }
}

struct Checker<'a> {
    new: &'a Interface,
    issues: Vec<Issue>,
    adapters: Vec<ReborrowAdapter>,
}
impl Checker<'_> {
    fn breaking(&mut self, path: &str, rule: &str, message: &str, witness: &str) {
        self.issues.push(Issue {
            path: path.into(),
            rule: rule.into(),
            kind: IssueKind::Breaking,
            message: message.into(),
            caller_witness: witness.into(),
        });
    }
    fn unsupported(&mut self, path: &str, message: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            rule: "Unsupported".into(),
            kind: IssueKind::Unsupported,
            message: message.into(),
            caller_witness:
                "The checker cannot certify substitutability for this interface feature.".into(),
        });
    }
    fn validate(&mut self, interface: &Interface, side: &str) {
        if interface.semantics_version != SEMANTICS_VERSION {
            self.unsupported(side, "unsupported language-semantics version");
        }
        if interface.schema_version != SCHEMA_VERSION {
            self.unsupported(side, "unsupported interface schema version");
        }
        for name in interface.external_types.keys() {
            if name.is_empty() || interface.exports.contains_key(name) {
                self.unsupported(side, "invalid or conflicting external type identity");
            }
        }
        for (name, declaration) in &interface.exports {
            if name.is_empty() {
                self.unsupported(side, "empty export identity");
            }
            let path = format!("{side}.{name}");
            match declaration {
                Declaration::Function { signature } => {
                    self.validate_function(interface, signature, &path, 0)
                }
                Declaration::Struct {
                    generics, fields, ..
                } => {
                    if let Fields::Public { fields } = fields {
                        for t in fields.values() {
                            self.validate_type(interface, t, generics, &path, 0);
                        }
                    }
                }
                Declaration::Enum {
                    generics, variants, ..
                } => {
                    for payload in variants.values() {
                        for t in payload {
                            self.validate_type(interface, t, generics, &path, 0);
                        }
                    }
                }
                Declaration::Effect {
                    generics,
                    operations,
                    ..
                } => {
                    // Operation-local generics and effect-declaration binders
                    // require an explicit concatenated binder list in this DTO.
                    for op in operations.values() {
                        if !op.generics.starts_with(generics) {
                            self.unsupported(
                                &path,
                                "operation generics must start with effect declaration binders",
                            );
                        }
                        self.validate_function(interface, op, &path, 0);
                    }
                }
                Declaration::Unsupported { description } => self.unsupported(&path, description),
            }
        }
    }
    fn validate_function(&mut self, interface: &Interface, f: &Function, path: &str, depth: usize) {
        if depth > 128 {
            self.unsupported(path, "function type nesting exceeds 128 levels");
            return;
        }
        for p in &f.parameters {
            self.validate_type(interface, &p.ty, &f.generics, path, depth + 1);
            if let Projection::Paths { paths } = &p.projection
                && (paths.is_empty()
                    || paths
                        .iter()
                        .any(|p| p.is_empty() || p.iter().any(String::is_empty)))
            {
                self.unsupported(path, "projection paths must have nonempty components");
            }
            if p.pass == Pass::Sink && p.projection != Projection::All {
                self.unsupported(
                    path,
                    "sink projections are not supported by this interface schema",
                );
            }
        }
        self.validate_type(interface, &f.result, &f.generics, path, depth + 1);
        if type_mode(interface, &f.result, &f.generics).is_some_and(|kind| kind > f.result_mode) {
            self.unsupported(
                path,
                "exported return mode weakens the result type's modal kind",
            );
        }
        let mut identities = BTreeSet::new();
        for c in &f.effects.capabilities {
            if c.identity.is_empty() || c.effect.is_empty() || !identities.insert(&c.identity) {
                self.unsupported(path, "capability identities must be nonempty and unique");
            }
            if let Some(Declaration::Effect {
                resumption,
                generics,
                ..
            }) = interface.exports.get(&c.effect)
                && (c.resumption != *resumption || c.arguments.len() != generics.len())
            {
                self.unsupported(
                    path,
                    "capability disagrees with its exported effect declaration",
                );
            }
            for t in &c.arguments {
                self.validate_type(interface, t, &f.generics, path, depth + 1);
            }
        }
        for index in &f.effects.row_variables {
            if !matches!(f.generics.get(*index), Some(Generic::Row { .. })) {
                self.unsupported(path, "unbound or non-row effect variable");
            }
        }
    }
    fn validate_type(
        &mut self,
        interface: &Interface,
        t: &Type,
        generics: &[Generic],
        path: &str,
        depth: usize,
    ) {
        if depth > 128 {
            self.unsupported(path, "type nesting exceeds 128 levels");
            return;
        }
        match t {
            Type::Primitive { .. } => (),
            Type::Parameter { index } => {
                if !matches!(generics.get(*index), Some(Generic::Type { .. })) {
                    self.unsupported(path, "unbound or non-type generic parameter");
                }
            }
            Type::Named { name, arguments } => {
                let arity = match interface.exports.get(name) {
                    Some(
                        Declaration::Struct { generics, .. } | Declaration::Enum { generics, .. },
                    ) => Some(generics.len()),
                    _ => interface.external_types.get(name).map(|e| e.arity),
                };
                if arity != Some(arguments.len()) {
                    self.unsupported(path, format!("unknown nominal type or wrong arity: {name}"));
                }
                if let Some(
                    Declaration::Struct {
                        generics: parameters,
                        ..
                    }
                    | Declaration::Enum {
                        generics: parameters,
                        ..
                    },
                ) = interface.exports.get(name)
                {
                    for (argument, parameter) in arguments.iter().zip(parameters) {
                        match parameter {
                            Generic::Type { bound }
                                if type_mode(interface, argument, generics)
                                    .is_some_and(|mode| mode > *bound) =>
                            {
                                self.unsupported(
                                    path,
                                    "nominal type argument exceeds its modal bound",
                                )
                            }
                            Generic::Row { .. } => self.unsupported(
                                path,
                                "nominal row arguments are not supported by this schema",
                            ),
                            _ => (),
                        }
                    }
                }
                for t in arguments {
                    self.validate_type(interface, t, generics, path, depth + 1);
                }
            }
            Type::Tuple { elements } => {
                for t in elements {
                    self.validate_type(interface, t, generics, path, depth + 1);
                }
            }
            Type::Function { signature } => {
                self.validate_function(interface, signature, path, depth + 1)
            }
            Type::Unsupported { description } => self.unsupported(path, description),
        }
    }
    fn generic_bounds(&mut self, path: &str, a: &[Generic], b: &[Generic]) {
        if a.len() != b.len() {
            self.breaking(
                path,
                "SV-Fn",
                "generic binder arity changed",
                "A caller explicitly instantiates the old binder list.",
            );
            return;
        }
        for (a, b) in a.iter().zip(b) {
            let safe = match (a, b) {
                (Generic::Type { bound: a }, Generic::Type { bound: b }) => a <= b,
                (
                    Generic::Row {
                        minimum_resumption: a,
                    },
                    Generic::Row {
                        minimum_resumption: b,
                    },
                ) => b <= a,
                _ => false,
            };
            if !safe {
                self.breaking(path,"SV-Fn","generic constraint strengthened or binder kind changed","Instantiate the old interface with a type or row admitted by its original bound.");
            }
        }
    }
    fn function(&mut self, path: &str, a: &Function, b: &Function, depth: usize) {
        if depth > 128 {
            self.unsupported(path, "subtyping recursion exceeds 128 levels");
            return;
        }
        self.generic_bounds(path, &a.generics, &b.generics);
        if b.mode > a.mode {
            self.breaking(
                path,
                "SV-Fn",
                "function value became less reusable",
                "A caller copies or drops the function value under its old mode.",
            );
        }
        if a.parameters.len() != b.parameters.len() {
            self.breaking(
                path,
                "SV-Fn",
                "parameter arity changed",
                "Call the function with the old argument list.",
            );
        }
        for (index, (old, new)) in a.parameters.iter().zip(&b.parameters).enumerate() {
            let p = format!("{path}.parameter[{index}]");
            let narrowed = new.projection.is_subset_of(&old.projection);
            if !narrowed {
                self.breaking(
                    &p,
                    "SV-Fn",
                    "borrowed projection widened",
                    "Borrow a newly selected field separately at the same call site.",
                );
            }
            if old.pass != new.pass {
                if old.pass == Pass::Inout && new.pass == Pass::In && narrowed {
                    self.adapters.push(ReborrowAdapter {
                        function_path: path.into(),
                        parameter: index,
                    });
                    self.breaking(&p,"SV-Pass","inout-to-in requires an explicit compatibility adapter","A caller uses explicit inout syntax; an old-signature wrapper must reborrow it as in.");
                } else {
                    self.breaking(&p,"SV-Pass","passing mode changed","For sink-to-borrow, a caller relied on consuming a linear argument; for in-to-write/consume, it retains only shared access.");
                }
            }
            let subtype = if old.pass == Pass::Inout {
                old.ty == new.ty
            } else {
                self.subtype(&old.ty, &new.ty, &p, depth + 1)
            };
            if !subtype {
                self.breaking(&p,"SV-Fn","parameter type is incompatible","Pass a value admitted by the old parameter type; mutable places cannot change stored type.");
            }
        }
        if b.result_mode > a.result_mode {
            self.breaking(
                path,
                "SV-Fn",
                "return mode became stricter",
                "Discard or duplicate a returned value as permitted by the old return mode.",
            );
        }
        if !self.subtype(&b.result, &a.result, &format!("{path}.result"), depth + 1) {
            self.breaking(
                path,
                "SV-Fn",
                "return type is incompatible",
                "Use the returned value at the old result type.",
            );
        }
        let subset = b.effects.capabilities.iter().all(|n| {
            a.effects.capabilities.iter().any(|o| {
                n.identity == o.identity
                    && n.effect == o.effect
                    && n.arguments == o.arguments
                    && n.resumption >= o.resumption
            })
        });
        let rows_subset = b.effects.row_variables.is_subset(&a.effects.row_variables);
        if !subset || !rows_subset {
            self.breaking(
                path,
                "SV-EffRow",
                "function requires new capabilities/rows or a weaker capability contract",
                "A caller grants exactly the old capability ceiling.",
            );
        }
        if resumption(&b.effects, &b.generics) < resumption(&a.effects, &a.generics) {
            self.breaking(
                path,
                "SV-EffRow",
                "resumption guarantee weakened",
                "Hold a live linear obligation across a formerly guaranteed-resumptive call.",
            );
        }
    }
    fn subtype(&mut self, sub: &Type, sup: &Type, path: &str, depth: usize) -> bool {
        if depth > 128 {
            self.unsupported(path, "subtyping recursion exceeds 128 levels");
            return false;
        }
        match (sub, sup) {
            (
                Type::Primitive {
                    name: Primitive::Never,
                },
                _,
            ) => true,
            (Type::Unsupported { .. }, _) | (_, Type::Unsupported { .. }) => false,
            (Type::Function { signature: a }, Type::Function { signature: b }) => {
                let before = (self.issues.len(), self.adapters.len());
                // A value of a must be usable at b, so a is the candidate.
                self.function(path, b, a, depth + 1);
                before == (self.issues.len(), self.adapters.len())
            }
            (Type::Tuple { elements: a }, Type::Tuple { elements: b }) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(a, b)| self.subtype(a, b, path, depth + 1))
            }
            _ => sub == sup,
        }
    }
    fn declaration(&mut self, path: &str, a: &Declaration, b: &Declaration) {
        match (a, b) {
            (Declaration::Function { signature: a }, Declaration::Function { signature: b }) => {
                self.function(path, a, b, 0)
            }
            (
                Declaration::Struct {
                    generics: ag,
                    mode: am,
                    fields: af,
                },
                Declaration::Struct {
                    generics: bg,
                    mode: bm,
                    fields: bf,
                },
            ) => {
                self.generic_bounds(path, ag, bg);
                if bm > am {
                    self.breaking(
                        path,
                        "SV-Adt-Closed",
                        "struct kind became stricter",
                        "A caller discards or copies a value under the old kind.",
                    );
                }
                if af != bf {
                    self.breaking(
                        path,
                        "SV-Adt-Closed",
                        "public field contract or field visibility changed",
                        "A caller constructs, projects or destructures the old public field set.",
                    );
                }
            }
            (
                Declaration::Enum {
                    generics: ag,
                    mode: am,
                    open: ao,
                    variants: av,
                },
                Declaration::Enum {
                    generics: bg,
                    mode: bm,
                    open: bo,
                    variants: bv,
                },
            ) => {
                self.generic_bounds(path, ag, bg);
                if bm > am || ao != bo {
                    self.breaking(path,"SV-Enum-Open","enum kind or openness changed incompatibly","A caller exhaustively matches a closed enum or discards unknown variants of an open enum.");
                }
                for (name, payload) in av {
                    if bv.get(name) != Some(payload) {
                        self.breaking(
                            path,
                            "SV-Enum-Open",
                            "existing variant removed or changed",
                            "A caller constructs or pattern-matches this old variant payload.",
                        );
                    }
                }
                for (name, payload) in bv.iter().filter(|(n, _)| !av.contains_key(*n)) {
                    if !ao {
                        self.breaking(
                            path,
                            "SV-Enum-Open",
                            "variant added to a closed enum",
                            "An exhaustive old match has no arm for the added variant.",
                        );
                    }
                    let mode = payload.iter().try_fold(Mode::Copy, |m, t| {
                        type_mode(self.new, t, ag).map(|n| m.max(n))
                    });
                    match mode {
                        Some(m) if m <= *am => (),
                        Some(_) => self.breaking(&format!("{path}.{name}"),"SV-Enum-Open","added payload exceeds the old enum kind","An old wildcard arm discards an unknown variant containing a linear obligation."),
                        None => self.unsupported(path,"cannot establish the added payload's modal kind"),
                    }
                }
            }
            (
                Declaration::Effect {
                    generics: ag,
                    sealed: aseal,
                    resumption: aq,
                    operations: ao,
                },
                Declaration::Effect {
                    generics: bg,
                    sealed: bseal,
                    resumption: bq,
                    operations: bo,
                },
            ) => {
                self.generic_bounds(path, ag, bg);
                if aseal != bseal || (!aseal && aq != bq) || (*aseal && bq < aq) {
                    self.breaking(path,"SV-Effect-Decl","effect openness or resumption contract changed","An external handler may discard an affine continuation, or a caller retains a linear frame across the old effect.");
                }
                if ao.keys().ne(bo.keys()) {
                    self.breaking(
                        path,
                        "SV-Effect-Decl",
                        "effect operation set changed",
                        "An old handler implements exactly the old operation set.",
                    );
                }
                // Even sealed operation signature changes are conservatively
                // invariant until bidirectional handler/caller variance exists.
                for (name, old) in ao {
                    if let Some(new) = bo.get(name)
                        && old != new
                    {
                        self.breaking(&format!("{path}.{name}"),"SV-Effect-Decl","effect operation signature changed","Old callers and external handlers must agree on both sides of this operation signature.");
                    }
                }
            }
            (Declaration::Unsupported { .. }, _) | (_, Declaration::Unsupported { .. }) => (),
            _ => self.breaking(
                path,
                "SV-Export",
                "public declaration kind changed",
                "A caller uses the declaration at its old syntactic and semantic kind.",
            ),
        }
    }
}
fn resumption(effects: &Effects, generics: &[Generic]) -> Resumption {
    effects
        .capabilities
        .iter()
        .map(|e| e.resumption)
        .chain(
            effects
                .row_variables
                .iter()
                .filter_map(|i| match generics.get(*i) {
                    Some(Generic::Row { minimum_resumption }) => Some(*minimum_resumption),
                    _ => None,
                }),
        )
        .min()
        .unwrap_or(Resumption::Linear)
}
fn type_mode(interface: &Interface, ty: &Type, generics: &[Generic]) -> Option<Mode> {
    type_mode_at(interface, ty, generics, 0)
}
fn type_mode_at(
    interface: &Interface,
    ty: &Type,
    generics: &[Generic],
    depth: usize,
) -> Option<Mode> {
    if depth > 128 {
        return None;
    }
    match ty {
        Type::Primitive {
            name: Primitive::String,
        } => Some(Mode::Affine),
        Type::Primitive { .. } => Some(Mode::Copy),
        Type::Parameter { index } => match generics.get(*index)? {
            Generic::Type { bound } => Some(*bound),
            _ => None,
        },
        Type::Function { signature } => Some(signature.mode),
        Type::Tuple { elements } => elements.iter().try_fold(Mode::Copy, |m, t| {
            type_mode_at(interface, t, generics, depth + 1).map(|n| m.max(n))
        }),
        Type::Named { name, arguments } => {
            let declared = match interface.exports.get(name) {
                Some(Declaration::Struct { mode, .. } | Declaration::Enum { mode, .. }) => *mode,
                _ => interface.external_types.get(name)?.mode,
            };
            arguments.iter().try_fold(declared, |m, t| {
                type_mode_at(interface, t, generics, depth + 1).map(|n| m.max(n))
            })
        }
        Type::Unsupported { .. } => None,
    }
}

#[cfg(test)]
mod tests;
