//! `.wit` world to Vesper capability traits and Rust host bindings (§30.2).
//!
//! [`generate`] parses one WIT package with `wit-parser` (the Bytecode
//! Alliance parser Wasmtime itself uses), selects a world and emits two
//! texts from the same resolved model:
//!
//! * **Vesper source.** Each host resource becomes a `linear struct` handle
//!   (`rep_idx`, `generation`: the §30.2 `WitOwn` shape). Each imported
//!   interface becomes a `linear effect` whose operations are the
//!   interface's functions; an `own<T>` parameter is passed `sink`, a
//!   `borrow<T>` parameter (including a method's receiver) is passed `in`,
//!   and every resource gets an explicit `<res>_drop(sink ..)` operation, so
//!   the only exits of a linear handle are consuming calls. The world's
//!   exports become a `trait` the guest implements, whose methods carry the
//!   imported effects in their row.
//! * **Rust host bindings.** One module per imported interface with a
//!   `Host` trait (one method per function, constructor, method, static
//!   function and resource destructor) and an `add_to_linker` that wires the
//!   trait into a `wasmtime::component::Linker`; records, enums and variants
//!   derive Wasmtime's component traits; and an `Exports` struct of typed
//!   export functions.
//!
//! The supported subset is the one both outputs can express faithfully.
//! Everything else is a typed [`WitGenError::Unsupported`] naming the item,
//! never a lossy mapping: `list`, `map`, fixed-length lists, `flags`,
//! `future`, `stream`, `error-context`, async functions, getters/setters, and
//! guest-exported resources. Generation is deterministic: the same source
//! always yields byte-identical text (the golden tests depend on it).
//!
//! The Vesper output is a declaration of the capability surface. That the
//! Vesper checker enforces `Mode::Linear` for these handles is a property of
//! the checker, not of this generator; and the Rust bindings are runtime
//! glue whose linear discipline is the host's (see [`crate::table`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use wit_parser::{
    Function, FunctionKind, Handle, InterfaceId, Resolve, Type, TypeDefKind, TypeId, TypeOwner,
    WorldItem, WorldKey,
};

/// Largest WIT source [`generate`] accepts.
pub const MAX_WIT_BYTES: usize = 1 << 20;

/// Both generated texts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WitBindings {
    /// The fully qualified world name, e.g. `vesper:nexus/plugin@0.1.0`.
    pub world: String,
    /// Vesper capability traits and linear handles.
    pub vesper: String,
    /// Rust host bindings for Wasmtime 50 components.
    pub rust: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WitGenError {
    /// The source exceeds [`MAX_WIT_BYTES`].
    TooLarge { len: usize, max: usize },
    /// `wit-parser` rejected the package.
    Parse(String),
    /// The requested world does not exist or is ambiguous.
    World(String),
    /// A construct outside the supported subset.
    Unsupported {
        item: String,
        construct: &'static str,
    },
    /// Two WIT items map to the same generated name.
    NameCollision {
        name: String,
        first: String,
        second: String,
    },
}
impl fmt::Display for WitGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { len, max } => write!(f, "WIT source is {len} bytes (max {max})"),
            Self::Parse(e) => write!(f, "WIT parse error: {e}"),
            Self::World(e) => write!(f, "WIT world selection: {e}"),
            Self::Unsupported { item, construct } => {
                write!(f, "{item}: {construct} is not supported by the generator")
            }
            Self::NameCollision {
                name,
                first,
                second,
            } => write!(f, "{first} and {second} both generate `{name}`"),
        }
    }
}
impl std::error::Error for WitGenError {}

/// Generate Vesper and Rust bindings for `world` (a plain name such as
/// `plugin`, or a qualified one) of the single WIT package in `source`.
/// `None` selects the package's only world.
pub fn generate(source: &str, world: Option<&str>) -> Result<WitBindings, WitGenError> {
    if source.len() > MAX_WIT_BYTES {
        return Err(WitGenError::TooLarge {
            len: source.len(),
            max: MAX_WIT_BYTES,
        });
    }
    let mut resolve = Resolve::default();
    let pkg = resolve
        .push_source("input.wit", source)
        .map_err(|e| WitGenError::Parse(format!("{e:#}")))?;
    let world_id = resolve
        .select_world(&[pkg], world)
        .map_err(|e| WitGenError::World(format!("{e:#}")))?;
    let model = Model::build(&resolve, world_id)?;
    Ok(WitBindings {
        world: model.world_name.clone(),
        vesper: model.vesper(),
        rust: model.rust(),
    })
}

// ---------------------------------------------------------------------------
// Resolved model
// ---------------------------------------------------------------------------

/// A WIT value type in the supported subset.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Prim(Prim),
    String,
    Option(Box<Ty>),
    Result(Option<Box<Ty>>, Option<Box<Ty>>),
    Tuple(Vec<Ty>),
    /// A named record, enum or variant (index into `Model::types`).
    Named(usize),
    Own(usize),
    Borrow(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prim {
    Bool,
    U8,
    U16,
    U32,
    U64,
    S8,
    S16,
    S32,
    S64,
    F32,
    F64,
    Char,
}
impl Prim {
    fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::S8 => "i8",
            Self::S16 => "i16",
            Self::S32 => "i32",
            Self::S64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Char => "char",
        }
    }
}

/// Vesper mode of a value: `copy`, affine (plain), or `linear`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Copy,
    Affine,
    Linear,
}

#[derive(Clone, Debug)]
enum Shape {
    Record(Vec<(String, Ty)>),
    Enum(Vec<String>),
    Variant(Vec<(String, Option<Ty>)>),
}

#[derive(Clone, Debug)]
struct NamedType {
    /// WIT kebab name.
    wit: String,
    /// Index of the owning scope in `Model::scopes`.
    scope: usize,
    shape: Shape,
    docs: Vec<String>,
}

#[derive(Clone, Debug)]
struct ResourceDef {
    wit: String,
    scope: usize,
    docs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FnKind {
    Free,
    Constructor(usize),
    Method(usize),
    Static(usize),
}

#[derive(Clone, Debug)]
struct Func {
    /// The import/export name Wasmtime links by (`[method]blob.tag`).
    wit: String,
    /// Generated snake-case operation name (`blob_tag`).
    op: String,
    params: Vec<(String, Ty)>,
    result: Option<Ty>,
    docs: Vec<String>,
}

#[derive(Clone, Debug)]
struct Scope {
    /// `Some("vesper:nexus/host@0.1.0")` for an interface, `None` for the
    /// world itself.
    qualified: Option<String>,
    /// WIT short name (`host`), or the world's name.
    wit: String,
    /// Rust module name.
    module: String,
    /// Vesper effect or trait name.
    upper: String,
}

#[derive(Clone, Debug)]
struct ImportIface {
    scope: usize,
    funcs: Vec<Func>,
    resources: Vec<usize>,
    docs: Vec<String>,
}

#[derive(Clone, Debug)]
struct ExportIface {
    /// `None` for functions exported directly from the world.
    scope: Option<usize>,
    funcs: Vec<Func>,
}

struct Model {
    world_name: String,
    world_upper: String,
    world_docs: Vec<String>,
    scopes: Vec<Scope>,
    types: Vec<NamedType>,
    resources: Vec<ResourceDef>,
    imports: Vec<ImportIface>,
    exports: Vec<ExportIface>,
}

struct Builder<'a> {
    resolve: &'a Resolve,
    scopes: Vec<Scope>,
    scope_of_iface: BTreeMap<InterfaceId, usize>,
    world_scope: Option<usize>,
    world_short: String,
    types: Vec<NamedType>,
    type_index: BTreeMap<TypeId, usize>,
    resources: Vec<ResourceDef>,
    resource_index: BTreeMap<TypeId, usize>,
    /// Generated top-level names (shared Vesper namespace) to their origin.
    names: BTreeMap<String, String>,
    /// Resources exported by the guest are not supported.
    exporting: bool,
}

fn docs(d: &wit_parser::Docs) -> Vec<String> {
    d.contents
        .as_deref()
        .map(|c| c.lines().map(|l| l.trim_end().to_string()).collect())
        .unwrap_or_default()
}

impl<'a> Builder<'a> {
    fn claim(&mut self, name: &str, origin: String) -> Result<(), WitGenError> {
        if let Some(first) = self.names.get(name) {
            return Err(WitGenError::NameCollision {
                name: name.to_string(),
                first: first.clone(),
                second: origin,
            });
        }
        self.names.insert(name.to_string(), origin);
        Ok(())
    }

    fn scope_for_owner(&mut self, owner: TypeOwner) -> Result<usize, WitGenError> {
        match owner {
            TypeOwner::Interface(id) => self.interface_scope(id),
            TypeOwner::World(_) => Ok(self.world_scope()),
            TypeOwner::None => Err(WitGenError::Unsupported {
                item: "anonymous type".into(),
                construct: "a type with no owner",
            }),
        }
    }

    fn world_scope(&mut self) -> usize {
        if let Some(s) = self.world_scope {
            return s;
        }
        let module = rust_ident(&snake(&self.world_short));
        self.scopes.push(Scope {
            qualified: None,
            wit: self.world_short.clone(),
            module,
            upper: format!("{}Imports", upper_camel(&self.world_short)),
        });
        let s = self.scopes.len() - 1;
        self.world_scope = Some(s);
        s
    }

    fn interface_scope(&mut self, id: InterfaceId) -> Result<usize, WitGenError> {
        if let Some(s) = self.scope_of_iface.get(&id) {
            return Ok(*s);
        }
        let iface = &self.resolve.interfaces[id];
        let qualified = self.resolve.id_of(id).ok_or(WitGenError::Unsupported {
            item: "inline interface".into(),
            construct: "an interface without a name",
        })?;
        let wit = iface.name.clone().unwrap_or_default();
        let module = rust_ident(&snake(&wit));
        if let Some(other) = self.scopes.iter().find(|s| s.module == module) {
            return Err(WitGenError::NameCollision {
                name: module,
                first: other.qualified.clone().unwrap_or_else(|| other.wit.clone()),
                second: qualified,
            });
        }
        self.scopes.push(Scope {
            qualified: Some(qualified),
            upper: upper_camel(&wit),
            wit,
            module,
        });
        let s = self.scopes.len() - 1;
        self.scope_of_iface.insert(id, s);
        Ok(s)
    }

    fn ty(&mut self, t: &Type, item: &str) -> Result<Ty, WitGenError> {
        Ok(match t {
            Type::Bool => Ty::Prim(Prim::Bool),
            Type::U8 => Ty::Prim(Prim::U8),
            Type::U16 => Ty::Prim(Prim::U16),
            Type::U32 => Ty::Prim(Prim::U32),
            Type::U64 => Ty::Prim(Prim::U64),
            Type::S8 => Ty::Prim(Prim::S8),
            Type::S16 => Ty::Prim(Prim::S16),
            Type::S32 => Ty::Prim(Prim::S32),
            Type::S64 => Ty::Prim(Prim::S64),
            Type::F32 => Ty::Prim(Prim::F32),
            Type::F64 => Ty::Prim(Prim::F64),
            Type::Char => Ty::Prim(Prim::Char),
            Type::String => Ty::String,
            Type::ErrorContext => return Err(unsupported(item, "error-context")),
            Type::Id(id) => self.type_id(*id, item)?,
        })
    }

    fn type_id(&mut self, id: TypeId, item: &str) -> Result<Ty, WitGenError> {
        let def = &self.resolve.types[id];
        Ok(match &def.kind {
            TypeDefKind::Type(inner) => self.ty(inner, item)?,
            TypeDefKind::Option(inner) => Ty::Option(Box::new(self.ty(inner, item)?)),
            TypeDefKind::Result(r) => Ty::Result(
                r.ok.as_ref()
                    .map(|t| self.ty(t, item).map(Box::new))
                    .transpose()?,
                r.err
                    .as_ref()
                    .map(|t| self.ty(t, item).map(Box::new))
                    .transpose()?,
            ),
            TypeDefKind::Tuple(t) => Ty::Tuple(
                t.types
                    .iter()
                    .map(|t| self.ty(t, item))
                    .collect::<Result<_, _>>()?,
            ),
            TypeDefKind::Handle(Handle::Own(r)) => Ty::Own(self.resource(*r, item)?),
            TypeDefKind::Handle(Handle::Borrow(r)) => Ty::Borrow(self.resource(*r, item)?),
            TypeDefKind::Resource => Ty::Own(self.resource(id, item)?),
            TypeDefKind::Record(_) | TypeDefKind::Enum(_) | TypeDefKind::Variant(_) => {
                Ty::Named(self.named(id)?)
            }
            TypeDefKind::Flags(_) => return Err(unsupported(item, "flags")),
            TypeDefKind::List(_) => return Err(unsupported(item, "list")),
            TypeDefKind::Map(..) => return Err(unsupported(item, "map")),
            TypeDefKind::FixedLengthList(..) => {
                return Err(unsupported(item, "fixed-length list"));
            }
            TypeDefKind::Future(_) => return Err(unsupported(item, "future")),
            TypeDefKind::Stream(_) => return Err(unsupported(item, "stream")),
            TypeDefKind::Unknown => return Err(unsupported(item, "unresolved type")),
        })
    }

    fn resource(&mut self, id: TypeId, item: &str) -> Result<usize, WitGenError> {
        let id = self.dealias(id);
        if let Some(i) = self.resource_index.get(&id) {
            return Ok(*i);
        }
        if self.exporting {
            return Err(unsupported(item, "a resource first used by an export"));
        }
        let def = &self.resolve.types[id];
        let wit = def.name.clone().unwrap_or_default();
        let scope = self.scope_for_owner(def.owner)?;
        let name = upper_camel(&wit);
        let origin = format!("resource `{wit}`");
        self.claim(&name, origin)?;
        self.resources.push(ResourceDef {
            wit,
            scope,
            docs: docs(&def.docs),
        });
        let i = self.resources.len() - 1;
        self.resource_index.insert(id, i);
        Ok(i)
    }

    fn dealias(&self, mut id: TypeId) -> TypeId {
        // `use` creates alias chains; they are finite (wit-parser rejects
        // cycles), but bound the walk anyway.
        for _ in 0..64 {
            match &self.resolve.types[id].kind {
                TypeDefKind::Type(Type::Id(next)) => id = *next,
                _ => break,
            }
        }
        id
    }

    fn named(&mut self, id: TypeId) -> Result<usize, WitGenError> {
        let id = self.dealias(id);
        if let Some(i) = self.type_index.get(&id) {
            return Ok(*i);
        }
        let def = &self.resolve.types[id];
        let wit = def.name.clone().ok_or(WitGenError::Unsupported {
            item: "anonymous type".into(),
            construct: "an anonymous record, enum or variant",
        })?;
        let item = format!("type `{wit}`");
        let scope = self.scope_for_owner(def.owner)?;
        let kind = def.kind.clone();
        let type_docs = docs(&def.docs);
        let shape = match &kind {
            TypeDefKind::Record(r) => Shape::Record(
                r.fields
                    .iter()
                    .map(|f| Ok((f.name.clone(), self.ty(&f.ty, &item)?)))
                    .collect::<Result<_, WitGenError>>()?,
            ),
            TypeDefKind::Enum(e) => Shape::Enum(e.cases.iter().map(|c| c.name.clone()).collect()),
            TypeDefKind::Variant(v) => Shape::Variant(
                v.cases
                    .iter()
                    .map(|c| {
                        Ok((
                            c.name.clone(),
                            c.ty.map(|t| self.ty(&t, &item)).transpose()?,
                        ))
                    })
                    .collect::<Result<_, WitGenError>>()?,
            ),
            _ => unreachable!("named() is only called for records, enums and variants"),
        };
        if let Shape::Record(fields) = &shape
            && fields.iter().any(|(_, t)| matches!(t, Ty::Borrow(_)))
        {
            return Err(unsupported(&item, "a borrow handle stored in a record"));
        }
        if let Shape::Variant(cases) = &shape
            && cases.iter().any(|(_, t)| matches!(t, Some(Ty::Borrow(_))))
        {
            return Err(unsupported(&item, "a borrow handle stored in a variant"));
        }
        let name = upper_camel(&wit);
        self.claim(&name, item)?;
        self.types.push(NamedType {
            wit,
            scope,
            shape,
            docs: type_docs,
        });
        let i = self.types.len() - 1;
        self.type_index.insert(id, i);
        Ok(i)
    }

    fn func(&mut self, f: &Function, context: &str) -> Result<Func, WitGenError> {
        let item = format!("{context} function `{}`", f.name);
        let kind = match f.kind {
            FunctionKind::Freestanding => FnKind::Free,
            FunctionKind::Constructor(r) => FnKind::Constructor(self.resource(r, &item)?),
            FunctionKind::Method(r) => FnKind::Method(self.resource(r, &item)?),
            FunctionKind::Static(r) => FnKind::Static(self.resource(r, &item)?),
            FunctionKind::AsyncFreestanding
            | FunctionKind::AsyncMethod(_)
            | FunctionKind::AsyncStatic(_) => return Err(unsupported(&item, "an async function")),
            _ => return Err(unsupported(&item, "a getter or setter")),
        };
        let item_name = f.item_name();
        let op = match kind {
            FnKind::Free => snake(item_name),
            FnKind::Constructor(r) => format!("{}_new", snake(&self.resources[r].wit)),
            FnKind::Method(r) | FnKind::Static(r) => {
                format!("{}_{}", snake(&self.resources[r].wit), snake(item_name))
            }
        };
        let mut params = Vec::with_capacity(f.params.len());
        let mut seen = BTreeSet::new();
        for (i, p) in f.params.iter().enumerate() {
            let ty = self.ty(&p.ty, &item)?;
            // A method's implicit receiver is named `self` in WIT; it is the
            // resource itself in the generated signature.
            let name = if i == 0 && matches!(kind, FnKind::Method(_)) {
                match kind {
                    FnKind::Method(r) => snake(&self.resources[r].wit),
                    _ => unreachable!(),
                }
            } else {
                snake(&p.name)
            };
            if !seen.insert(name.clone()) {
                return Err(WitGenError::NameCollision {
                    name,
                    first: format!("{item} parameter"),
                    second: format!("{item} parameter `{}`", p.name),
                });
            }
            params.push((name, ty));
        }
        let mut result = f.result.as_ref().map(|t| self.ty(t, &item)).transpose()?;
        if let FnKind::Constructor(r) = kind {
            // Constructors return `own<T>`; wit-parser reports it as such.
            result = Some(Ty::Own(r));
        }
        if result.as_ref().is_some_and(contains_borrow) {
            return Err(unsupported(&item, "a borrow handle in a result"));
        }
        Ok(Func {
            wit: f.name.clone(),
            op,
            params,
            result,
            docs: docs(&f.docs),
        })
    }
}

/// Whether a type mentions a resource handle directly (named types carry
/// their own paths).
fn ty_has_handle(t: &Ty) -> bool {
    match t {
        Ty::Own(_) | Ty::Borrow(_) => true,
        Ty::Option(t) => ty_has_handle(t),
        Ty::Result(a, b) => {
            a.as_deref().is_some_and(ty_has_handle) || b.as_deref().is_some_and(ty_has_handle)
        }
        Ty::Tuple(ts) => ts.iter().any(ty_has_handle),
        _ => false,
    }
}

fn contains_borrow(t: &Ty) -> bool {
    match t {
        Ty::Borrow(_) => true,
        Ty::Option(t) => contains_borrow(t),
        Ty::Result(a, b) => {
            a.as_deref().is_some_and(contains_borrow) || b.as_deref().is_some_and(contains_borrow)
        }
        Ty::Tuple(ts) => ts.iter().any(contains_borrow),
        _ => false,
    }
}

fn unsupported(item: &str, construct: &'static str) -> WitGenError {
    WitGenError::Unsupported {
        item: item.to_string(),
        construct,
    }
}

impl Model {
    fn build(resolve: &Resolve, world_id: wit_parser::WorldId) -> Result<Self, WitGenError> {
        let world = &resolve.worlds[world_id];
        let world_name = match world.package {
            Some(pkg) => {
                let p = &resolve.packages[pkg].name;
                let version = p
                    .version
                    .as_ref()
                    .map(|v| format!("@{v}"))
                    .unwrap_or_default();
                format!("{}:{}/{}{}", p.namespace, p.name, world.name, version)
            }
            None => world.name.clone(),
        };
        let mut b = Builder {
            resolve,
            scopes: Vec::new(),
            scope_of_iface: BTreeMap::new(),
            world_scope: None,
            world_short: world.name.clone(),
            types: Vec::new(),
            type_index: BTreeMap::new(),
            resources: Vec::new(),
            resource_index: BTreeMap::new(),
            names: BTreeMap::new(),
            exporting: false,
        };
        let world_upper = upper_camel(&world.name);

        // Imports: interfaces become effects; world-level functions are
        // gathered into one effect named after the world.
        let mut imports: Vec<ImportIface> = Vec::new();
        let mut world_funcs = Vec::new();
        for (key, item) in &world.imports {
            match item {
                WorldItem::Interface { id, .. } => {
                    if matches!(key, WorldKey::Name(_)) {
                        return Err(unsupported(
                            &resolve.name_world_key(key),
                            "an inline or renamed interface import",
                        ));
                    }
                    let scope = b.interface_scope(*id)?;
                    let iface = &resolve.interfaces[*id];
                    let context = format!("import `{}`", b.scopes[scope].wit);
                    // Types and resources declared by the interface, in
                    // declaration order, even if no function mentions them.
                    for (_, &tid) in &iface.types {
                        let t = b.dealias(tid);
                        if resolve.types[t].owner != TypeOwner::Interface(*id) {
                            continue;
                        }
                        match resolve.types[t].kind {
                            TypeDefKind::Resource => {
                                b.resource(t, &context)?;
                            }
                            TypeDefKind::Record(_)
                            | TypeDefKind::Enum(_)
                            | TypeDefKind::Variant(_) => {
                                b.named(t)?;
                            }
                            _ => {
                                b.ty(&Type::Id(t), &context)?;
                            }
                        }
                    }
                    let mut funcs = Vec::new();
                    for f in iface.functions.values() {
                        funcs.push(b.func(f, &context)?);
                    }
                    imports.push(ImportIface {
                        scope,
                        funcs,
                        resources: Vec::new(),
                        docs: docs(&iface.docs),
                    });
                }
                WorldItem::Function(f) => world_funcs.push(f.clone()),
                WorldItem::Type { id, .. } => {
                    let t = b.dealias(*id);
                    match resolve.types[t].kind {
                        TypeDefKind::Resource => {
                            b.resource(t, "world")?;
                        }
                        TypeDefKind::Record(_) | TypeDefKind::Enum(_) | TypeDefKind::Variant(_) => {
                            b.named(t)?;
                        }
                        _ => {
                            b.ty(&Type::Id(t), "world type")?;
                        }
                    }
                }
            }
        }
        if !world_funcs.is_empty() {
            let scope = b.world_scope();
            let funcs = world_funcs
                .iter()
                .map(|f| b.func(f, "world import"))
                .collect::<Result<Vec<_>, _>>()?;
            imports.push(ImportIface {
                scope,
                funcs,
                resources: Vec::new(),
                docs: Vec::new(),
            });
        }
        // A world-level resource with no world-level functions still needs
        // an effect for its constructor/drop.
        if let Some(ws) = b.world_scope
            && !imports.iter().any(|i| i.scope == ws)
            && b.resources.iter().any(|r| r.scope == ws)
        {
            imports.push(ImportIface {
                scope: ws,
                funcs: Vec::new(),
                resources: Vec::new(),
                docs: Vec::new(),
            });
        }

        // Each import's resources, in registration order; interfaces that
        // only declare types get no effect (their types are still emitted).
        for imp in &mut imports {
            imp.resources = (0..b.resources.len())
                .filter(|&r| b.resources[r].scope == imp.scope)
                .collect();
        }
        imports.retain(|i| !i.funcs.is_empty() || !i.resources.is_empty());

        // Exports: the guest implements them; they may use imported types
        // but must not introduce resources of their own.
        b.exporting = true;
        let mut exports = Vec::new();
        let mut direct = Vec::new();
        for (key, item) in &world.exports {
            match item {
                WorldItem::Interface { id, .. } => {
                    if matches!(key, WorldKey::Name(_)) {
                        return Err(unsupported(
                            &resolve.name_world_key(key),
                            "an inline or renamed interface export",
                        ));
                    }
                    let iface = &resolve.interfaces[*id];
                    let name = resolve.id_of(*id).unwrap_or_default();
                    if iface
                        .types
                        .values()
                        .any(|t| matches!(resolve.types[b.dealias(*t)].kind, TypeDefKind::Resource))
                    {
                        return Err(unsupported(
                            &format!("export `{name}`"),
                            "a guest-exported resource",
                        ));
                    }
                    let scope = b.interface_scope(*id)?;
                    let context = format!("export `{}`", b.scopes[scope].wit);
                    let funcs = iface
                        .functions
                        .values()
                        .map(|f| b.func(f, &context))
                        .collect::<Result<Vec<_>, _>>()?;
                    exports.push(ExportIface {
                        scope: Some(scope),
                        funcs,
                    });
                }
                WorldItem::Function(f) => direct.push(b.func(f, "world export")?),
                WorldItem::Type { .. } => {
                    return Err(unsupported("world export", "an exported type"));
                }
            }
        }
        if !direct.is_empty() {
            exports.insert(
                0,
                ExportIface {
                    scope: None,
                    funcs: direct,
                },
            );
        }

        // Effect and trait names share the Vesper namespace with types.
        for imp in &imports {
            let s = &b.scopes[imp.scope];
            let origin = format!(
                "import `{}`",
                s.qualified.clone().unwrap_or_else(|| s.wit.clone())
            );
            let upper = s.upper.clone();
            b.claim(&upper, origin)?;
        }
        for exp in &exports {
            let (upper, origin) = match exp.scope {
                None => (world_upper.clone(), format!("world `{}`", world.name)),
                Some(s) => (
                    format!("{}Export", b.scopes[s].upper),
                    format!("export `{}`", b.scopes[s].wit),
                ),
            };
            b.claim(&upper, origin)?;
        }

        Ok(Model {
            world_name,
            world_upper,
            world_docs: docs(&world.docs),
            scopes: b.scopes,
            types: b.types,
            resources: b.resources,
            imports,
            exports,
        })
    }

    fn mode(&self, t: &Ty) -> Mode {
        match t {
            Ty::Prim(_) => Mode::Copy,
            Ty::String => Mode::Affine,
            Ty::Own(_) => Mode::Linear,
            Ty::Borrow(_) => Mode::Copy,
            Ty::Option(t) => self.mode(t),
            Ty::Result(a, b) => a
                .as_deref()
                .map_or(Mode::Copy, |t| self.mode(t))
                .max(b.as_deref().map_or(Mode::Copy, |t| self.mode(t))),
            Ty::Tuple(ts) => ts.iter().map(|t| self.mode(t)).max().unwrap_or(Mode::Copy),
            Ty::Named(i) => self.type_mode(*i),
        }
    }

    fn type_mode(&self, i: usize) -> Mode {
        match &self.types[i].shape {
            Shape::Enum(_) => Mode::Copy,
            Shape::Record(fs) => fs
                .iter()
                .map(|(_, t)| self.mode(t))
                .max()
                .unwrap_or(Mode::Copy),
            Shape::Variant(cs) => cs
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .map(|t| self.mode(t))
                .max()
                .unwrap_or(Mode::Copy),
        }
    }

    // -----------------------------------------------------------------------
    // Vesper
    // -----------------------------------------------------------------------

    fn ves_ty(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(p) => p.name().to_string(),
            Ty::String => "String".into(),
            Ty::Option(t) => format!("Option[{}]", self.ves_ty(t)),
            Ty::Result(a, b) => format!(
                "Result[{}, {}]",
                a.as_deref().map_or("Unit".into(), |t| self.ves_ty(t)),
                b.as_deref().map_or("Unit".into(), |t| self.ves_ty(t))
            ),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter()
                    .map(|t| self.ves_ty(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Named(i) => upper_camel(&self.types[*i].wit),
            Ty::Own(r) | Ty::Borrow(r) => upper_camel(&self.resources[*r].wit),
        }
    }

    fn ves_param(&self, name: &str, t: &Ty) -> String {
        let pass = match t {
            Ty::Borrow(_) => "in ",
            _ if self.mode(t) == Mode::Copy => "",
            _ => "sink ",
        };
        format!("{pass}{}: {}", ves_ident(name), self.ves_ty(t))
    }

    fn ves_sig(&self, f: &Func, receiver: bool, row: &str) -> String {
        let mut params = Vec::new();
        if receiver {
            params.push("in self".to_string());
        }
        params.extend(f.params.iter().map(|(n, t)| self.ves_param(n, t)));
        let mut s = format!("fn {}({})", ves_ident(&f.op), params.join(", "));
        if let Some(r) = &f.result {
            let _ = write!(s, " -> {}", self.ves_ty(r));
        }
        s.push_str(row);
        s.push(';');
        s
    }

    fn vesper(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "// Generated by vesper-nexus from WIT world `{}`. Do not edit.",
            self.world_name
        );
        out.push_str(
            "// WIT own<T> is a linear handle passed `sink`; borrow<T> is an `in` projection.\n",
        );
        for line in &self.world_docs {
            push_comment(&mut out, "", line);
        }
        for r in &self.resources {
            out.push('\n');
            let _ = writeln!(
                out,
                "// Resource `{}` of {}.",
                r.wit,
                self.scope_label(r.scope)
            );
            for line in &r.docs {
                push_comment(&mut out, "", line);
            }
            let _ = writeln!(
                out,
                "linear struct {} {{\n    rep_idx: u32,\n    generation: u32\n}}",
                upper_camel(&r.wit)
            );
        }
        for (i, t) in self.types.iter().enumerate() {
            out.push('\n');
            for line in &t.docs {
                push_comment(&mut out, "", line);
            }
            let mode = match self.type_mode(i) {
                Mode::Copy => "copy ",
                Mode::Affine => "",
                Mode::Linear => "linear ",
            };
            let name = upper_camel(&t.wit);
            match &t.shape {
                Shape::Record(fields) => {
                    let _ = write!(out, "{mode}struct {name} {{");
                    let body = fields
                        .iter()
                        .map(|(f, t)| format!("    {}: {}", ves_ident(&snake(f)), self.ves_ty(t)))
                        .collect::<Vec<_>>();
                    write_block(&mut out, &body);
                }
                Shape::Enum(cases) => {
                    let _ = write!(out, "{mode}enum {name} {{");
                    let body = cases
                        .iter()
                        .map(|c| format!("    {}", upper_camel(c)))
                        .collect::<Vec<_>>();
                    write_block(&mut out, &body);
                }
                Shape::Variant(cases) => {
                    let _ = write!(out, "{mode}enum {name} {{");
                    let body = cases
                        .iter()
                        .map(|(c, t)| match t {
                            Some(t) => format!("    {}({})", upper_camel(c), self.ves_ty(t)),
                            None => format!("    {}", upper_camel(c)),
                        })
                        .collect::<Vec<_>>();
                    write_block(&mut out, &body);
                }
            }
        }
        for imp in &self.imports {
            let s = &self.scopes[imp.scope];
            out.push('\n');
            let _ = writeln!(
                out,
                "// Capabilities imported from {}.",
                self.scope_label(imp.scope)
            );
            for line in &imp.docs {
                push_comment(&mut out, "", line);
            }
            let _ = writeln!(out, "linear effect {} {{", s.upper);
            for f in self.import_ops(imp) {
                match f {
                    Op::Func(f) => {
                        for line in &f.docs {
                            push_comment(&mut out, "    ", line);
                        }
                        let _ = writeln!(out, "    {}", self.ves_sig(f, false, ""));
                    }
                    Op::Drop(r) => {
                        let res = &self.resources[r];
                        let _ = writeln!(
                            out,
                            "    // Drops the handle; rejected while a borrow is live.\n    fn {}_drop(sink {}: {});",
                            snake(&res.wit),
                            ves_ident(&snake(&res.wit)),
                            upper_camel(&res.wit)
                        );
                    }
                }
            }
            out.push_str("}\n");
        }
        let row = if self.imports.is_empty() {
            String::new()
        } else {
            format!(
                " eff [{}]",
                self.imports
                    .iter()
                    .map(|i| self.scopes[i.scope].upper.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        for exp in &self.exports {
            out.push('\n');
            let name = match exp.scope {
                None => {
                    let _ = writeln!(
                        out,
                        "// Exports of world `{}`, implemented by the guest.",
                        self.world_short()
                    );
                    self.world_upper.clone()
                }
                Some(s) => {
                    let _ = writeln!(
                        out,
                        "// Exported {}, implemented by the guest.",
                        self.scope_label(s)
                    );
                    format!("{}Export", self.scopes[s].upper)
                }
            };
            let _ = writeln!(out, "trait {name} {{");
            for f in &exp.funcs {
                for line in &f.docs {
                    push_comment(&mut out, "    ", line);
                }
                let _ = writeln!(out, "    {}", self.ves_sig(f, true, &row));
            }
            out.push_str("}\n");
        }
        out
    }

    fn world_short(&self) -> &str {
        self.world_name
            .rsplit_once('/')
            .map_or(self.world_name.as_str(), |(_, w)| w)
            .split('@')
            .next()
            .unwrap_or_default()
    }

    fn scope_label(&self, s: usize) -> String {
        match &self.scopes[s].qualified {
            Some(q) => format!("interface `{q}`"),
            None => format!("world `{}`", self.scopes[s].wit),
        }
    }

    /// Functions in declaration order, then one drop per resource.
    fn import_ops<'m>(&'m self, imp: &'m ImportIface) -> Vec<Op<'m>> {
        let mut ops: Vec<Op<'m>> = imp.funcs.iter().map(Op::Func).collect();
        ops.extend(imp.resources.iter().map(|&r| Op::Drop(r)));
        ops
    }

    // -----------------------------------------------------------------------
    // Rust
    // -----------------------------------------------------------------------

    /// Path to scope `s`'s items from inside a sibling module (`super::m`),
    /// or from the bindings root (`m`).
    fn rs_path(&self, s: usize, at: Option<usize>, name: &str) -> String {
        match at {
            Some(here) if here == s => name.to_string(),
            Some(_) => format!("super::{}::{name}", self.scopes[s].module),
            None => format!("{}::{name}", self.scopes[s].module),
        }
    }

    fn rs_ty(&self, t: &Ty, at: Option<usize>) -> String {
        match t {
            Ty::Prim(p) => p.name().to_string(),
            Ty::String => "String".into(),
            Ty::Option(t) => format!("Option<{}>", self.rs_ty(t, at)),
            Ty::Result(a, b) => format!(
                "Result<{}, {}>",
                a.as_deref().map_or("()".into(), |t| self.rs_ty(t, at)),
                b.as_deref().map_or("()".into(), |t| self.rs_ty(t, at))
            ),
            Ty::Tuple(ts) => {
                let inner = ts.iter().map(|t| self.rs_ty(t, at)).collect::<Vec<_>>();
                if inner.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
            Ty::Named(i) => {
                self.rs_path(self.types[*i].scope, at, &upper_camel(&self.types[*i].wit))
            }
            Ty::Own(r) | Ty::Borrow(r) => format!(
                "Resource<{}>",
                self.rs_path(
                    self.resources[*r].scope,
                    at,
                    &upper_camel(&self.resources[*r].wit)
                )
            ),
        }
    }

    fn rust(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "// Generated by vesper-nexus from WIT world `{}`. Do not edit.",
            self.world_name
        );
        out.push_str("//\n// Rust host bindings for Wasmtime 50 components. Include them with\n");
        out.push_str("// `include!` inside a module; every path is relative to that module.\n");

        // Group types and resources by scope; imported interfaces first, in
        // import order, then any scope that only holds types.
        let mut order: Vec<usize> = self.imports.iter().map(|i| i.scope).collect();
        for t in &self.types {
            if !order.contains(&t.scope) {
                order.push(t.scope);
            }
        }
        for r in &self.resources {
            if !order.contains(&r.scope) {
                order.push(r.scope);
            }
        }
        for &s in &order {
            out.push('\n');
            self.rust_scope(&mut out, s);
        }
        if !self.exports.is_empty() {
            out.push('\n');
            self.rust_exports(&mut out);
        }
        out
    }

    fn rust_scope(&self, out: &mut String, s: usize) {
        let scope = &self.scopes[s];
        let imp = self.imports.iter().find(|i| i.scope == s);
        let _ = writeln!(out, "/// Bindings for {}.", self.scope_label(s));
        let _ = writeln!(out, "pub mod {} {{", scope.module);
        let mut uses = Vec::new();
        if imp.is_some() {
            uses.push("use wasmtime::StoreContextMut;");
        }
        let needs_resource = self.resources.iter().any(|r| r.scope == s)
            || imp.is_some_and(|i| {
                i.funcs.iter().any(|f| {
                    f.params.iter().any(|(_, t)| ty_has_handle(t))
                        || f.result.as_ref().is_some_and(ty_has_handle)
                })
            })
            || self
                .types
                .iter()
                .any(|t| t.scope == s && self.type_mentions_resource(t));
        let mut component = Vec::new();
        if imp.is_some() {
            component.push("Linker");
        }
        if needs_resource {
            component.push("Resource");
        }
        if imp.is_some_and(|i| !i.resources.is_empty()) {
            component.push("ResourceType");
        }
        let component_use = match component.len() {
            0 => None,
            1 => Some(format!("use wasmtime::component::{};", component[0])),
            _ => Some(format!(
                "use wasmtime::component::{{{}}};",
                component.join(", ")
            )),
        };
        let mut use_lines: Vec<String> = uses.iter().map(|s| s.to_string()).collect();
        if let Some(c) = component_use {
            use_lines.push(c);
        }
        // rustfmt orders `use` items; keep them sorted. Sections are
        // separated by exactly one blank line, as rustfmt leaves them.
        use_lines.sort();
        let mut sections: Vec<String> = Vec::new();
        if !use_lines.is_empty() {
            sections.push(
                use_lines
                    .iter()
                    .map(|u| format!("    {u}\n"))
                    .collect::<String>(),
            );
        }
        if let Some(q) = &scope.qualified {
            sections.push(format!(
                "    /// The instance name Wasmtime links this interface under.\n    pub const INTERFACE: &str = {q:?};\n"
            ));
        }
        for r in self.resources.iter().filter(|r| r.scope == s) {
            let mut sec = format!("    /// Host-side type tag for WIT resource `{}`.\n", r.wit);
            for line in &r.docs {
                push_doc(&mut sec, "    ", line);
            }
            let _ = writeln!(sec, "    pub struct {};", upper_camel(&r.wit));
            sections.push(sec);
        }
        for (i, t) in self.types.iter().enumerate().filter(|(_, t)| t.scope == s) {
            let mut sec = String::new();
            self.rust_type(&mut sec, i, t);
            sections.push(sec);
        }
        if let Some(imp) = imp {
            let mut sec = String::new();
            self.rust_host_trait(&mut sec, imp);
            sections.push(sec);
            let mut sec = String::new();
            self.rust_add_to_linker(&mut sec, imp);
            sections.push(sec);
        }
        out.push_str(&sections.join("\n"));
        out.push_str("}\n");
    }

    fn type_mentions_resource(&self, t: &NamedType) -> bool {
        match &t.shape {
            Shape::Record(fs) => fs.iter().any(|(_, t)| ty_has_handle(t)),
            Shape::Enum(_) => false,
            Shape::Variant(cs) => cs
                .iter()
                .any(|(_, t)| t.as_ref().is_some_and(ty_has_handle)),
        }
    }

    fn rust_type(&self, out: &mut String, i: usize, t: &NamedType) {
        for line in &t.docs {
            push_doc(out, "    ", line);
        }
        let name = upper_camel(&t.wit);
        let copy = self.type_mode(i) == Mode::Copy;
        let derives = if copy {
            "Clone, Copy, Debug, PartialEq"
        } else {
            "Debug"
        };
        let derive = format!(
            "    #[derive(wasmtime::component::ComponentType, wasmtime::component::Lift, wasmtime::component::Lower, {derives})]"
        );
        if derive.chars().count() <= 100 {
            let _ = writeln!(out, "{derive}");
        } else {
            out.push_str("    #[derive(\n");
            for d in [
                "wasmtime::component::ComponentType",
                "wasmtime::component::Lift",
                "wasmtime::component::Lower",
            ]
            .into_iter()
            .chain(derives.split(", "))
            {
                let _ = writeln!(out, "        {d},");
            }
            out.push_str("    )]\n");
        }
        match &t.shape {
            Shape::Record(fields) => {
                out.push_str("    #[component(record)]\n");
                let _ = writeln!(out, "    pub struct {name} {{");
                for (f, ty) in fields {
                    let rs = rust_ident(&snake(f));
                    if rs != *f {
                        let _ = writeln!(out, "        #[component(name = {f:?})]");
                    }
                    let _ = writeln!(out, "        pub {rs}: {},", self.rs_ty(ty, Some(t.scope)));
                }
                out.push_str("    }\n");
            }
            Shape::Enum(cases) => {
                out.push_str("    #[component(enum)]\n");
                let repr = if cases.len() <= 256 {
                    "u8"
                } else if cases.len() <= 65_536 {
                    "u16"
                } else {
                    "u32"
                };
                let _ = writeln!(out, "    #[repr({repr})]");
                let _ = writeln!(out, "    pub enum {name} {{");
                for c in cases {
                    let _ = writeln!(out, "        #[component(name = {c:?})]");
                    let _ = writeln!(out, "        {},", upper_camel(c));
                }
                out.push_str("    }\n");
            }
            Shape::Variant(cases) => {
                out.push_str("    #[component(variant)]\n");
                let _ = writeln!(out, "    pub enum {name} {{");
                for (c, ty) in cases {
                    let _ = writeln!(out, "        #[component(name = {c:?})]");
                    match ty {
                        Some(ty) => {
                            let _ = writeln!(
                                out,
                                "        {}({}),",
                                upper_camel(c),
                                self.rs_ty(ty, Some(t.scope))
                            );
                        }
                        None => {
                            let _ = writeln!(out, "        {},", upper_camel(c));
                        }
                    }
                }
                out.push_str("    }\n");
            }
        }
    }

    fn rs_params(&self, f: &Func, at: Option<usize>) -> Vec<(String, String)> {
        f.params
            .iter()
            .map(|(n, t)| (rust_ident(n), self.rs_ty(t, at)))
            .collect()
    }

    fn rs_ret(&self, f: &Func, at: Option<usize>) -> String {
        f.result.as_ref().map_or("()".into(), |t| self.rs_ty(t, at))
    }

    fn rust_host_trait(&self, out: &mut String, imp: &ImportIface) {
        let _ = writeln!(
            out,
            "    /// The host side of {}:\n    /// one method per function, constructor, method and static function,\n    /// plus one destructor per resource.",
            self.scope_label(imp.scope)
        );
        out.push_str("    pub trait Host {\n");
        for op in self.import_ops(imp) {
            match op {
                Op::Func(f) => {
                    let _ = writeln!(out, "        /// WIT `{}`.", f.wit);
                    for line in &f.docs {
                        push_doc(out, "        ", line);
                    }
                    let mut params = vec!["&mut self".to_string()];
                    params.extend(
                        self.rs_params(f, Some(imp.scope))
                            .into_iter()
                            .map(|(n, t)| format!("{n}: {t}")),
                    );
                    let ret = format!("wasmtime::Result<{}>", self.rs_ret(f, Some(imp.scope)));
                    write_rust_sig(
                        out,
                        "        ",
                        &format!("fn {}", rust_ident(&f.op)),
                        &params,
                        &ret,
                        ";",
                    );
                }
                Op::Drop(r) => {
                    let res = &self.resources[r];
                    let _ = writeln!(
                        out,
                        "        /// Destructor of resource `{}` (guest `resource.drop`).",
                        res.wit
                    );
                    let params = vec![
                        "&mut self".to_string(),
                        format!(
                            "{}: Resource<{}>",
                            rust_ident(&snake(&res.wit)),
                            upper_camel(&res.wit)
                        ),
                    ];
                    write_rust_sig(
                        out,
                        "        ",
                        &format!("fn {}_drop", snake(&res.wit)),
                        &params,
                        "wasmtime::Result<()>",
                        ";",
                    );
                }
            }
        }
        out.push_str("    }\n");
    }

    fn rust_add_to_linker(&self, out: &mut String, imp: &ImportIface) {
        let scope = &self.scopes[imp.scope];
        out.push_str("    /// Define every import of this interface in `linker`, dispatching to\n");
        out.push_str("    /// the `Host` that `get` projects out of the store data.\n");
        out.push_str("    pub fn add_to_linker<T: 'static>(\n");
        out.push_str("        linker: &mut Linker<T>,\n");
        out.push_str("        get: fn(&mut T) -> &mut dyn Host,\n");
        out.push_str("    ) -> wasmtime::Result<()> {\n");
        let target = match &scope.qualified {
            Some(_) => "let mut inst = linker.instance(INTERFACE)?;",
            None => "let mut inst = linker.root();",
        };
        let _ = writeln!(out, "        {target}");
        for &r in &imp.resources {
            let res = &self.resources[r];
            let ty = upper_camel(&res.wit);
            let _ = writeln!(out, "        inst.resource(");
            let _ = writeln!(out, "            {:?},", res.wit);
            let _ = writeln!(out, "            ResourceType::host::<{ty}>(),");
            let _ = writeln!(
                out,
                "            move |mut cx: StoreContextMut<'_, T>, rep: u32| {{"
            );
            let _ = writeln!(
                out,
                "                get(cx.data_mut()).{}_drop(Resource::new_own(rep))",
                snake(&res.wit)
            );
            out.push_str("            },\n        )?;\n");
        }
        for f in &imp.funcs {
            let params = self.rs_params(f, Some(imp.scope));
            let names: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
            let types: Vec<&str> = params.iter().map(|(_, t)| t.as_str()).collect();
            let pat = tuple_text(&names);
            let ty = tuple_text(&types);
            let _ = writeln!(out, "        inst.func_wrap(");
            let _ = writeln!(out, "            {:?},", f.wit);
            let call = format!(
                "get(cx.data_mut()).{}({})",
                rust_ident(&f.op),
                names.join(", ")
            );
            let wrap = if f.result.is_some() {
                ".map(|r| (r,))"
            } else {
                ""
            };
            let closure = format!("move |mut cx: StoreContextMut<'_, T>, {pat}: {ty}|");
            // rustfmt drops the braces when the whole closure fits.
            let one = format!("            {closure} {call}{wrap},");
            if one.chars().count() <= 100 {
                let _ = writeln!(out, "{one}");
                out.push_str("        )?;\n");
                continue;
            }
            if closure.len() + 14 <= 100 {
                let _ = writeln!(out, "            {closure} {{");
            } else {
                out.push_str("            move |mut cx: StoreContextMut<'_, T>,\n");
                let _ = writeln!(out, "                  {pat}: {ty}| {{");
            }
            if call.len() + wrap.len() + 16 <= 100 {
                let _ = writeln!(out, "                {call}{wrap}");
            } else {
                let _ = writeln!(out, "                {call}\n                    {wrap}");
            }
            out.push_str("            },\n        )?;\n");
        }
        out.push_str("        Ok(())\n    }\n");
    }

    fn rust_exports(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "/// Typed exports of world `{}`, looked up once per instance.",
            self.world_short()
        );
        out.push_str("pub struct Exports {\n");
        for exp in &self.exports {
            for f in &exp.funcs {
                let params = self.rs_params_root(f);
                let _ = writeln!(out, "    /// WIT export `{}`.", self.export_label(exp, f));
                let _ = writeln!(
                    out,
                    "    pub {}: wasmtime::component::TypedFunc<{}, {}>,",
                    self.export_field(exp, f),
                    tuple_text(&params.iter().map(String::as_str).collect::<Vec<_>>()),
                    tuple_text(
                        &f.result
                            .iter()
                            .map(|t| self.rs_ty(t, None))
                            .collect::<Vec<_>>()
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                    ),
                );
            }
        }
        out.push_str("}\n\nimpl Exports {\n");
        out.push_str("    /// Look up every export of `instance` with its expected type.\n");
        out.push_str("    pub fn new(\n");
        out.push_str("        mut store: impl wasmtime::AsContextMut,\n");
        out.push_str("        instance: &wasmtime::component::Instance,\n");
        out.push_str("    ) -> wasmtime::Result<Self> {\n");
        for exp in &self.exports {
            if let Some(s) = exp.scope {
                let q = self.scopes[s].qualified.clone().unwrap_or_default();
                let _ = writeln!(
                    out,
                    "        let {} = instance\n            .get_export_index(&mut store, None, {q:?})\n            {}",
                    self.scopes[s].module,
                    missing_export(&q)
                );
            }
        }
        for exp in &self.exports {
            for f in &exp.funcs {
                let field = self.export_field(exp, f);
                let lookup = match exp.scope {
                    None => format!("{:?}", f.wit),
                    Some(s) => {
                        let m = &self.scopes[s].module;
                        let _ = writeln!(
                            out,
                            "        let {field}_index = instance\n            .get_export_index(&mut store, Some(&{m}), {:?})\n            {}",
                            f.wit,
                            missing_export(&self.export_label(exp, f))
                        );
                        format!("{field}_index")
                    }
                };
                let _ = writeln!(
                    out,
                    "        let {field} = instance.get_typed_func(&mut store, {lookup})?;"
                );
            }
        }
        let fields: Vec<String> = self
            .exports
            .iter()
            .flat_map(|e| e.funcs.iter().map(move |f| (e, f)))
            .map(|(e, f)| self.export_field(e, f))
            .collect();
        let one_line = format!("        Ok(Self {{ {} }})", fields.join(", "));
        // rustfmt's default `struct_lit_width` is 18.
        if fields.join(", ").len() <= 18 {
            let _ = writeln!(out, "{one_line}");
        } else {
            out.push_str("        Ok(Self {\n");
            for f in &fields {
                let _ = writeln!(out, "            {f},");
            }
            out.push_str("        })\n");
        }
        out.push_str("    }\n}\n");
    }

    fn rs_params_root(&self, f: &Func) -> Vec<String> {
        f.params.iter().map(|(_, t)| self.rs_ty(t, None)).collect()
    }

    fn export_field(&self, exp: &ExportIface, f: &Func) -> String {
        match exp.scope {
            None => rust_ident(&f.op),
            Some(s) => format!("{}_{}", self.scopes[s].module, f.op),
        }
    }

    fn export_label(&self, exp: &ExportIface, f: &Func) -> String {
        match exp.scope {
            None => f.wit.clone(),
            Some(s) => format!(
                "{}#{}",
                self.scopes[s].qualified.clone().unwrap_or_default(),
                f.wit
            ),
        }
    }
}

enum Op<'m> {
    Func(&'m Func),
    Drop(usize),
}

/// `.ok_or_else(..)?;` for a missing export, laid out as rustfmt does at
/// a 12-column indent.
fn missing_export(name: &str) -> String {
    let one = format!(".ok_or_else(|| wasmtime::format_err!(\"missing export {name}\"))?;");
    if 12 + one.len() <= 100 {
        one
    } else {
        format!(
            ".ok_or_else(|| {{\n                wasmtime::format_err!(\"missing export {name}\")\n            }})?;"
        )
    }
}

fn tuple_text(items: &[&str]) -> String {
    match items.len() {
        0 => "()".into(),
        1 => format!("({},)", items[0]),
        _ => format!("({})", items.join(", ")),
    }
}

/// Write a Rust signature the way rustfmt lays it out: on one line when it
/// fits in 100 columns, otherwise one parameter per line.
fn write_rust_sig(
    out: &mut String,
    indent: &str,
    head: &str,
    params: &[String],
    ret: &str,
    tail: &str,
) {
    let one = format!("{indent}{head}({}) -> {ret}{tail}", params.join(", "));
    if one.chars().count() <= 100 {
        out.push_str(&one);
        out.push('\n');
        return;
    }
    let _ = writeln!(out, "{indent}{head}(");
    for p in params {
        let _ = writeln!(out, "{indent}    {p},");
    }
    let _ = writeln!(out, "{indent}) -> {ret}{tail}");
}

fn write_block(out: &mut String, body: &[String]) {
    if body.is_empty() {
        out.push_str("}\n");
        return;
    }
    out.push('\n');
    out.push_str(&body.join(",\n"));
    out.push_str("\n}\n");
}

fn push_comment(out: &mut String, indent: &str, line: &str) {
    if line.is_empty() {
        let _ = writeln!(out, "{indent}//");
    } else {
        let _ = writeln!(out, "{indent}// {line}");
    }
}

fn push_doc(out: &mut String, indent: &str, line: &str) {
    if line.is_empty() {
        let _ = writeln!(out, "{indent}///");
    } else {
        let _ = writeln!(out, "{indent}/// {line}");
    }
}

/// `peek-raw` -> `peek_raw`. WIT identifiers are kebab-case ASCII.
fn snake(s: &str) -> String {
    s.replace('-', "_")
}

/// `peek-raw` -> `PeekRaw`. WIT allows acronym segments (`HTTP`); they keep
/// their first letter upper and the rest lower.
fn upper_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for part in s.split('-') {
        let mut chars = part.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
            out.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
    }
    out
}

/// Rust keywords get a trailing underscore.
fn rust_ident(s: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "box", "break", "const", "continue", "crate", "do", "dyn", "else",
        "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop",
        "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "self",
        "static", "struct", "super", "trait", "true", "try", "type", "typeof", "unsafe", "unsized",
        "use", "virtual", "where", "while", "yield",
    ];
    if KEYWORDS.contains(&s) {
        format!("{s}_")
    } else {
        s.to_string()
    }
}

/// Vesper keywords (and the Rust ones, so both outputs agree) get a trailing
/// underscore.
fn ves_ident(s: &str) -> String {
    const VESPER: &[&str] = &[
        "copy", "eff", "effect", "handle", "inout", "linear", "model", "resume", "sink", "state",
        "with",
    ];
    let r = rust_ident(s);
    if r != s {
        return r;
    }
    if VESPER.contains(&s) {
        format!("{s}_")
    } else {
        s.to_string()
    }
}
