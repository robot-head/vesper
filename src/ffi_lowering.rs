//! Vesper Bridge in the compiler (design §13): `.vsys` imports, `Ffi[Lib]`
//! capability effects, native foreign calls and `isolated(mpk)` trampolines.
//!
//! # Surface
//!
//! A package imports a `.vsys` library out of band, the way Loom's manifest
//! names its sys libraries and capability ceiling (§13.3): the caller hands
//! [`check_module`] an [`Imports`] set, one `.vsys` source per library, each
//! with the handler tier the package grants it ([`Handler::Direct`] or
//! [`Handler::IsolatedMpk`]). The source then names the library in two
//! places, with no new syntax:
//!
//! ```text
//! fn sum(a: u32, b: u32) -> u32 eff [Ffi[Fixture]] {
//!     Fixture::bridge_sum(a, b)
//! }
//! ```
//!
//! * `eff [Ffi[Lib]]` is the lexical capability of library `Lib`. Every
//!   foreign call needs it in the enclosing row, exactly like any other
//!   effect operation: the ordinary checker rejects a call whose row lacks
//!   it, so pure code (`eff []`) can never reach a foreign symbol. No source
//!   handler can discharge it (`handle Ffi[Lib]` is rejected); the entry
//!   function may carry it, and the package's import is what grants it.
//! * `Lib::symbol(args)` calls the `.vsys` function `symbol`.
//!
//! # Types
//!
//! | `.vsys` | source |
//! | --- | --- |
//! | `u8`, `u32`, `u64`, `i64` | the same type |
//! | `usize` | `u64` |
//! | `bool` (a `uint8_t` carrier) | `bool` |
//! | `enum E: uN { .. }` | a generated `copy enum E` |
//! | `wire struct W { .. }` | a generated `copy struct W` (padding hidden, `ByteArray[N]` as an `N`-tuple of `u8`) |
//! | `nocapture in/out/inout p: T` | `in`/`inout` argument of `T`'s source type |
//! | `Option[T]` pointer | `Option[T]` (`None` is `NULL`) |
//! | `nocapture in/inout s: CSlice[T]` | `in`/`inout` homogeneous tuple of `T` (its arity is the length) |
//! | `Option[CSlice[T]]` | `Option` of such a tuple (`None` is `(NULL, 0)`) |
//! | `ForeignVal[T]` result, `T` total | `T` (narrow results widen: `i32` to `i64`, `u16` to `u32`) |
//! | `ForeignVal[bool]`, `ForeignVal[E]` | `Result[T, ForeignInvalid]`, validated at the boundary |
//!
//! Parameters of `i8`, `i16`, `i32` and `u16`, `ForeignVal[Ptr[..]]` results,
//! wire fields without a source carrier, and first-class uses of a foreign
//! function are rejected with an [`FfiError`]. A slice's tuple arity is fixed
//! at each call site: the frontend checks the module once with slice
//! parameters left open, reads each slice argument's tuple type, and checks
//! the module again against one operation per distinct arity, so the World
//! the backend sees is fully concrete. The source language has no unbounded
//! arrays yet; tuples are its only fixed-length sequences.
//!
//! # Lowering
//!
//! Each imported library becomes an ordinary effect declaration (named
//! `Ffi$Lib`, which no source identifier can spell) whose operations are its
//! functions. The native backend lowers an operation of such an effect to a
//! call of the C symbol through a hook in `src/native.rs`
//! (`ffi_lowering/native.rs`): scalars are narrowed to their C carrier,
//! `in`/`inout` structs, scalars and slices are marshalled into a C-layout
//! stack buffer and copied back after the call when the callee may write
//! them. Structs and slices whose source layout already is the C layout
//! (only 64-bit native-endian fields) are passed by address without a copy.
//! Bool and enum results are validated before they become source values.
//!
//! An `isolated(mpk)` call (Linux x86-64 only) is wrapped in a generated
//! trampoline: it reads `PKRU`, sets the access- and write-disable bits of
//! the package's protection key, calls the foreign function, and restores the
//! saved `PKRU`. The key's bits come from the exported data symbol
//! [`isolation_symbol`]`(Lib)`, which the host sets after `pkey_alloc`; a
//! zero mask, or one that touches key 0, traps before the call (fail closed).
//! Memory the host tags with that key therefore faults when the foreign code
//! touches it. This is protection-key isolation of the tagged pages only: the
//! trampoline does not pivot to a sandbox stack and does not stop foreign
//! code from executing `WRPKRU` itself.
//!
//! Rust ownership of these types says nothing about the Vesper source typing
//! rules; those are the checker's, and the `.vsys` contract (`nocapture`,
//! direction) is trusted, not observed.
use crate::ast::*;
use crate::check::{self, Checked, World};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::rc::Rc;
use vesper_ffi::sys::{
    self, Access, FunctionDescriptor, LibraryDescriptor, LinkMode, ParameterType, PointerAccess,
    ValueType,
};

#[path = "ffi_lowering/types.rs"]
mod types;
use types::ParamKind;

/// Schema of a [`ForeignObject`]'s link manifest.
pub const SCHEMA: &str = "vesper.bridge-lowering/1";
/// Prefix of the effect each imported library becomes. `$` is not a source
/// identifier character, so no source declaration can collide with it.
pub const EFFECT_PREFIX: &str = "Ffi$";
/// The source capability constructor: `eff [Ffi[Lib]]`.
pub const CAPABILITY: &str = "Ffi";
/// Generated error payload of a failed boundary validation.
pub const INVALID_TYPE: &str = "ForeignInvalid";
/// Largest slice (tuple arity) a foreign call may pass.
pub const MAX_SLICE_ELEMENTS: usize = 4096;
/// Trap code of a call through an `isolated(mpk)` trampoline whose
/// protection-key mask was never configured (or would revoke key 0).
pub const ISOLATION_TRAP: u8 = 5;

/// Handler tier a package grants an imported library (§13.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Handler {
    /// `unsafe handle Ffi[Lib] direct`: a plain native call.
    Direct,
    /// `handle Ffi[Lib] isolated(mpk)`: the call runs with the package's
    /// protection key revoked (Linux x86-64 `WRPKRU`).
    IsolatedMpk,
}
impl Handler {
    pub fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::IsolatedMpk => "isolated(mpk)",
        }
    }
}

/// What went wrong, as a stable category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiErrorKind {
    /// The `.vsys` source does not parse or validate.
    Vsys,
    /// The source module does not parse.
    Parse,
    /// Two imports name the same library or declare one name differently.
    DuplicateLibrary,
    /// A generated declaration collides with a source declaration.
    NameCollision,
    /// `Ffi[Lib]` names a library that is not imported.
    UnknownLibrary,
    /// `Lib::f` names no function of the imported library.
    UnknownForeignFunction,
    /// A `.vsys` type that has no source carrier or lowering yet.
    UnsupportedType,
    /// A slice argument that is not a homogeneous tuple (or is too long).
    SliceArgument,
    /// `handle Ffi[Lib]` in source.
    ForeignHandler,
    /// A labelled `eff [l: Ffi[Lib]]` or malformed `Ffi[..]` instance.
    MalformedCapability,
    /// A foreign function used as a value instead of being called.
    ForeignValue,
    /// `Realtime` code may not call foreign code (§32.2).
    Realtime,
    /// The ordinary checker rejected the elaborated module.
    Check,
    /// The library's `.vsys` target differs from the object target.
    TargetMismatch,
    /// The handler tier is not available for the target.
    IsolationUnavailable,
    /// A linked artifact's bytes differ from the declared BLAKE3 digest.
    ArtifactMismatch,
    /// The native backend failed.
    Codegen,
}

/// A typed Bridge lowering error. `span` is a source byte range when the
/// error is about the module; `diagnostics` holds the checker's errors.
#[derive(Debug, Clone, PartialEq)]
pub struct FfiError {
    pub kind: FfiErrorKind,
    pub message: String,
    pub span: Option<Span>,
    pub diagnostics: Vec<Diagnostic>,
}
impl FfiError {
    fn new(kind: FfiErrorKind, message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            kind,
            message: message.into(),
            span,
            diagnostics: vec![],
        }
    }
}
impl fmt::Display for FfiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        for d in &self.diagnostics {
            write!(f, "\n  [{}] {} at {:?}", d.code, d.message, d.span)?;
        }
        Ok(())
    }
}
impl std::error::Error for FfiError {}

#[derive(Debug, Clone)]
struct Import {
    descriptor: LibraryDescriptor,
    handler: Handler,
}

/// The `.vsys` libraries a package imports, with their handler tiers.
#[derive(Debug, Clone, Default)]
pub struct Imports {
    libraries: BTreeMap<String, Import>,
}
impl Imports {
    pub fn new() -> Self {
        Self::default()
    }
    /// Parse one bounded `.vsys` library and import it.
    pub fn add(&mut self, vsys: &str, handler: Handler) -> Result<&LibraryDescriptor, FfiError> {
        let descriptor = sys::parse(vsys, sys::Limits::default())
            .map_err(|e| FfiError::new(FfiErrorKind::Vsys, e.to_string(), None))?;
        self.add_descriptor(descriptor, handler)
    }
    /// Import an already validated library.
    pub fn add_descriptor(
        &mut self,
        descriptor: LibraryDescriptor,
        handler: Handler,
    ) -> Result<&LibraryDescriptor, FfiError> {
        let name = descriptor.name().to_owned();
        if self.libraries.contains_key(&name) {
            return Err(FfiError::new(
                FfiErrorKind::DuplicateLibrary,
                format!("library '{name}' is imported twice"),
                None,
            ));
        }
        let import = self.libraries.entry(name).or_insert(Import {
            descriptor,
            handler,
        });
        Ok(&import.descriptor)
    }
    pub fn libraries(&self) -> impl Iterator<Item = (&LibraryDescriptor, Handler)> {
        self.libraries.values().map(|i| (&i.descriptor, i.handler))
    }
}

/// Whether `name` is the effect of an imported library.
pub fn is_foreign_effect(name: &str) -> bool {
    name.starts_with(EFFECT_PREFIX)
}
/// The effect name of library `library`.
pub fn effect_name(library: &str) -> String {
    format!("{EFFECT_PREFIX}{library}")
}
/// The exported `u32` data symbol holding the `PKRU` bits an
/// `isolated(mpk)` call of `library` revokes.
pub fn isolation_symbol(library: &str) -> String {
    format!("vesper_bridge_pkru_revoke_{library}")
}
/// The `PKRU` access- and write-disable bits of protection key `key`
/// (1..=15; key 0 holds the host's own stack and heap and is never revoked).
pub fn pkru_revoke_mask(key: u32) -> Result<u32, FfiError> {
    if !(1..=15).contains(&key) {
        return Err(FfiError::new(
            FfiErrorKind::IsolationUnavailable,
            format!("protection key {key} is not revocable (expected 1..=15)"),
            None,
        ));
    }
    Ok(0b11 << (2 * key))
}

/// One checked foreign call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCall {
    pub span: Span,
    pub library: String,
    pub symbol: String,
    pub handler: Handler,
    /// Element counts of the call's slice arguments, in parameter order.
    pub slice_lengths: Vec<usize>,
}

/// What the native hook needs about one library.
#[derive(Debug, Clone)]
pub(crate) struct LibraryPlan {
    pub name: String,
    pub handler: Handler,
    pub descriptor: LibraryDescriptor,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct Plan {
    /// By effect name.
    pub libraries: BTreeMap<String, LibraryPlan>,
    /// Libraries the object target cannot call, by library name.
    pub blocked: BTreeMap<String, FfiError>,
}
/// One foreign operation as the native hook lowers it.
pub(crate) struct OpPlan {
    pub library: String,
    pub handler: Handler,
    pub function: FunctionDescriptor,
}
thread_local! {
    static PLAN: RefCell<Option<Rc<Plan>>> = const { RefCell::new(None) };
    /// Libraries whose calls the backend actually emitted.
    static USED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    /// The typed error behind a backend failure in [`op_plan`].
    static FAULT: RefCell<Option<FfiError>> = const { RefCell::new(None) };
}
struct PlanGuard;
impl Drop for PlanGuard {
    fn drop(&mut self) {
        PLAN.with(|p| *p.borrow_mut() = None);
        USED.with(|u| u.borrow_mut().clear());
        FAULT.with(|f| *f.borrow_mut() = None);
    }
}
/// The plan of foreign operation `op` of effect `effect`, while
/// [`ForeignChecked::object`] is compiling.
pub(crate) fn op_plan(effect: &str, op: &str) -> Result<OpPlan, String> {
    PLAN.with(|p| {
        let plan = p.borrow();
        let plan = plan.as_ref().ok_or(
            "foreign calls compile only through vesper::ffi_lowering (the library plan is missing)",
        )?;
        let library = plan
            .libraries
            .get(effect)
            .ok_or_else(|| format!("no imported library for effect '{effect}'"))?;
        USED.with(|u| u.borrow_mut().insert(library.name.clone()));
        if let Some(e) = plan.blocked.get(&library.name) {
            FAULT.with(|f| *f.borrow_mut() = Some(e.clone()));
            return Err(e.message.clone());
        }
        let symbol = op.split_once('$').map_or(op, |(s, _)| s);
        let function = library
            .descriptor
            .function(symbol)
            .ok_or_else(|| format!("library '{}' has no function '{symbol}'", library.name))?
            .clone();
        Ok(OpPlan {
            library: library.name.clone(),
            handler: library.handler,
            function,
        })
    })
}

/// A module whose foreign calls were resolved and checked.
#[derive(Debug, Clone)]
pub struct ForeignChecked {
    pub checked: Checked,
    calls: Vec<ForeignCall>,
    plan: Plan,
}
/// One `.vsys` link input of an emitted object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInput {
    pub library: String,
    pub mode: LinkMode,
    pub name: String,
    pub artifact_blake3: [u8; 32],
    pub handler: Handler,
}
/// An object file with foreign calls and what linking it requires.
#[derive(Debug, Clone)]
pub struct ForeignObject {
    pub bytes: Vec<u8>,
    pub clif: String,
    /// Libraries the object calls, sorted by name.
    pub link: Vec<LinkInput>,
    /// Exported `PKRU` mask symbols of the isolated libraries it calls.
    pub isolation_symbols: Vec<String>,
}
impl ForeignObject {
    /// Check that `bytes` are the artifact `library`'s `.vsys` pinned.
    pub fn verify_artifact(&self, library: &str, bytes: &[u8]) -> Result<(), FfiError> {
        let input = self
            .link
            .iter()
            .find(|l| l.library == library)
            .ok_or_else(|| {
                FfiError::new(
                    FfiErrorKind::UnknownLibrary,
                    format!("object does not link '{library}'"),
                    None,
                )
            })?;
        if blake3::hash(bytes).as_bytes() != &input.artifact_blake3 {
            return Err(FfiError::new(
                FfiErrorKind::ArtifactMismatch,
                format!("artifact of '{library}' differs from its declared BLAKE3 digest"),
                None,
            ));
        }
        Ok(())
    }
    /// A stable textual link manifest ([`SCHEMA`]).
    pub fn manifest(&self) -> String {
        let mut out = format!("{SCHEMA}\n");
        for l in &self.link {
            let digest: String = l
                .artifact_blake3
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            out.push_str(&format!(
                "library {} {} {} blake3:{digest} {}\n",
                l.library,
                match l.mode {
                    LinkMode::Static => "static",
                    LinkMode::Dynamic => "dynamic",
                },
                l.name,
                l.handler.name()
            ));
        }
        for s in &self.isolation_symbols {
            out.push_str(&format!("isolation {s}\n"));
        }
        out
    }
}
impl ForeignChecked {
    pub fn world(&self) -> &World {
        &self.checked.world
    }
    /// Every foreign call site, in source order.
    pub fn calls(&self) -> &[ForeignCall] {
        &self.calls
    }
    /// Compile `entry` (exported under its own name) to an object for
    /// `target` (default: the host).
    pub fn object(&self, entry: &str, target: Option<&str>) -> Result<ForeignObject, FfiError> {
        let codegen = |m: String| FfiError::new(FfiErrorKind::Codegen, m, None);
        let triple: target_lexicon::Triple = match target {
            Some(t) => t
                .parse()
                .map_err(|e| codegen(format!("invalid target: {e}")))?,
            None => target_lexicon::Triple::host(),
        };
        // Only libraries the entry reaches are linked and checked: the
        // backend records each library whose call it emits.
        let mut plan = self.plan.clone();
        for library in self.plan.libraries.values() {
            if let Err(e) = target_support(library, &triple) {
                plan.blocked.insert(library.name.clone(), e);
            }
        }
        PLAN.with(|p| *p.borrow_mut() = Some(Rc::new(plan)));
        let _guard = PlanGuard;
        let (bytes, clif) = crate::native::object_for_target(&self.checked.world, entry, target)
            .map_err(|m| {
                FAULT
                    .with(|f| f.borrow_mut().take())
                    .unwrap_or_else(|| codegen(m))
            })?;
        let used = USED.with(|u| u.borrow().clone());
        let mut link = vec![];
        let mut isolation_symbols = vec![];
        for library in self.plan.libraries.values() {
            if !used.contains(&library.name) {
                continue;
            }
            if library.handler == Handler::IsolatedMpk {
                isolation_symbols.push(isolation_symbol(&library.name));
            }
            let l = library.descriptor.link();
            link.push(LinkInput {
                library: library.name.clone(),
                mode: l.mode(),
                name: l.name().to_owned(),
                artifact_blake3: *l.artifact_blake3(),
                handler: library.handler,
            });
        }
        Ok(ForeignObject {
            bytes,
            clif,
            link,
            isolation_symbols,
        })
    }
}

/// Whether `library` can be called from an object for `triple`.
fn target_support(library: &LibraryPlan, triple: &target_lexicon::Triple) -> Result<(), FfiError> {
    let declared: target_lexicon::Triple =
        library.descriptor.target().triple().parse().map_err(|e| {
            FfiError::new(
                FfiErrorKind::TargetMismatch,
                format!("invalid .vsys target: {e}"),
                None,
            )
        })?;
    if declared.architecture != triple.architecture
        || declared.operating_system != triple.operating_system
    {
        return Err(FfiError::new(
            FfiErrorKind::TargetMismatch,
            format!(
                "library '{}' targets {} but the object targets {triple}",
                library.name,
                library.descriptor.target().triple()
            ),
            None,
        ));
    }
    if library.handler == Handler::IsolatedMpk
        && (triple.architecture != target_lexicon::Architecture::X86_64
            || triple.operating_system != target_lexicon::OperatingSystem::Linux)
    {
        return Err(FfiError::new(
            FfiErrorKind::IsolationUnavailable,
            format!(
                "isolated(mpk) trampolines are generated for Linux x86-64 (WRPKRU) only; {triple} is not supported (AArch64 POE is not implemented)"
            ),
            None,
        ));
    }
    Ok(())
}

/// Parse `source` and check it against `imports`.
pub fn check_source(source: &str, imports: &Imports) -> Result<ForeignChecked, FfiError> {
    let parsed = crate::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        let mut e = FfiError::new(FfiErrorKind::Parse, "source does not parse", None);
        e.diagnostics = parsed.diagnostics;
        return Err(e);
    }
    check_module(&parsed.module, imports)
}

/// Resolve the module's `Ffi[Lib]` rows and `Lib::f` calls against
/// `imports`, then check the elaborated module with the ordinary checker.
pub fn check_module(module: &Module, imports: &Imports) -> Result<ForeignChecked, FfiError> {
    let generated = types::Generated::new(imports)?;
    generated.check_collisions(module)?;
    // Pass 1: slice parameters open, to read each slice argument's tuple.
    let mut first = module.clone();
    let mut rewriter = Rewriter::new(imports, &generated, None);
    rewriter.module(&mut first);
    rewriter.finish()?;
    let sites = rewriter.slice_sites;
    let mut arities: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    if !sites.is_empty() {
        generated.append(&mut first, None);
        let probe = check::check(&first);
        for site in &sites {
            let mut lengths = vec![];
            for (span, element, nullable) in &site.slices {
                let ty = probe.expression_types.get(&(span.start, span.end));
                lengths.push(slice_length(ty, element, *nullable, span)?);
            }
            arities.insert((site.call.start, site.call.end), lengths);
        }
    }
    // Pass 2: one operation per distinct slice arity.
    let mut second = module.clone();
    let mut rewriter = Rewriter::new(imports, &generated, Some(&arities));
    rewriter.module(&mut second);
    rewriter.finish()?;
    let instances = rewriter.instances;
    let calls = rewriter.calls;
    generated.append(&mut second, Some(&instances));
    let checked = check::check(&second);
    if !checked.diagnostics.is_empty() {
        let mut e = FfiError::new(
            FfiErrorKind::Check,
            format!("{} semantic error(s)", checked.diagnostics.len()),
            None,
        );
        e.diagnostics = checked
            .diagnostics
            .iter()
            .map(|d| {
                let mut d = d.clone();
                d.message = generated.demangle(&d.message);
                d
            })
            .collect();
        return Err(e);
    }
    let mut plan = Plan::default();
    for (name, import) in &imports.libraries {
        plan.libraries.insert(
            effect_name(name),
            LibraryPlan {
                name: name.clone(),
                handler: import.handler,
                descriptor: import.descriptor.clone(),
            },
        );
    }
    Ok(ForeignChecked {
        checked,
        calls,
        plan,
    })
}

fn slice_length(
    ty: Option<&Type>,
    element: &Type,
    nullable: bool,
    span: &Span,
) -> Result<usize, FfiError> {
    let wanted = if nullable {
        format!("Option[({element}, ...)]")
    } else {
        format!("({element}, ...)")
    };
    let error = |found: String| {
        FfiError::new(
            FfiErrorKind::SliceArgument,
            format!("a CSlice[{element}] argument must be a tuple {wanted}; found {found}"),
            Some(span.clone()),
        )
    };
    let mut ty = ty.ok_or_else(|| error("an expression of unknown type".into()))?;
    if nullable {
        match ty {
            Type::Named(n, args) if n == "Option" && args.len() == 1 => ty = &args[0],
            other => return Err(error(other.to_string())),
        }
    }
    let n = match ty {
        Type::Tuple(ts) => ts.len(),
        Type::Named(n, _) if n == "Unit" => 0,
        other => return Err(error(other.to_string())),
    };
    if n > MAX_SLICE_ELEMENTS {
        return Err(FfiError::new(
            FfiErrorKind::SliceArgument,
            format!("slice of {n} elements exceeds the {MAX_SLICE_ELEMENTS}-element bound"),
            Some(span.clone()),
        ));
    }
    Ok(n)
}

/// Slice-length vectors used per (library, symbol).
type Instances = BTreeMap<(String, String), BTreeSet<Vec<usize>>>;

struct SliceSite {
    call: Span,
    /// Argument span, element source type, nullability.
    slices: Vec<(Span, Type, bool)>,
}

/// Rewrites `Ffi[Lib]` rows to the library effect and `Lib::f` calls to its
/// operations, collecting typed errors.
struct Rewriter<'a> {
    imports: &'a Imports,
    generated: &'a types::Generated,
    /// Pass 2 only: slice lengths by call span.
    arities: Option<&'a BTreeMap<(usize, usize), Vec<usize>>>,
    errors: Vec<FfiError>,
    slice_sites: Vec<SliceSite>,
    /// Pass 2: (library, symbol) -> slice-length vectors used.
    instances: Instances,
    calls: Vec<ForeignCall>,
}
impl<'a> Rewriter<'a> {
    fn new(
        imports: &'a Imports,
        generated: &'a types::Generated,
        arities: Option<&'a BTreeMap<(usize, usize), Vec<usize>>>,
    ) -> Self {
        Self {
            imports,
            generated,
            arities,
            errors: vec![],
            slice_sites: vec![],
            instances: BTreeMap::new(),
            calls: vec![],
        }
    }
    fn finish(&mut self) -> Result<(), FfiError> {
        if self.errors.is_empty() {
            return Ok(());
        }
        let mut errors = std::mem::take(&mut self.errors);
        errors.sort_by_key(|e| e.span.as_ref().map(|s| (s.start, s.end)));
        Err(errors.remove(0))
    }
    fn error(&mut self, kind: FfiErrorKind, message: impl Into<String>, span: &Span) {
        self.errors
            .push(FfiError::new(kind, message, Some(span.clone())));
    }
    fn module(&mut self, m: &mut Module) {
        for item in &mut m.items {
            match item {
                Item::Adt(a) => {
                    for v in &mut a.variants {
                        for f in &mut v.fields {
                            self.ty(&mut f.ty, &a.span.clone());
                        }
                    }
                }
                Item::Function(f) => self.function(f),
                Item::Effect(e) => {
                    for op in &mut e.ops {
                        self.function(op);
                    }
                }
                Item::Impl { ty, methods, .. } => {
                    self.ty(ty, &(0..0));
                    for f in methods {
                        self.function(f);
                    }
                }
                Item::Trait(t) => {
                    for f in &mut t.methods {
                        self.function(f);
                    }
                }
                Item::Conformance(c) => {
                    for f in &mut c.methods {
                        self.function(f);
                    }
                }
            }
        }
    }
    fn function(&mut self, f: &mut Function) {
        let span = f.span.clone();
        for p in &mut f.params {
            self.ty(&mut p.ty, &span);
        }
        self.ty(&mut f.result, &span);
        self.effects(&mut f.effects, &span);
        if let Some(body) = &mut f.body {
            self.expr(body);
        }
    }
    fn effects(&mut self, es: &mut Effects, span: &Span) {
        let realtime = es
            .instances
            .iter()
            .any(|e| e.name == crate::capability_effects::REALTIME);
        for e in &mut es.instances {
            for a in &mut e.args {
                self.ty(a, span);
            }
            if e.name != CAPABILITY {
                continue;
            }
            let library = match e.args.as_slice() {
                [Type::Named(l, args)] if args.is_empty() => l.clone(),
                _ => {
                    self.error(
                        FfiErrorKind::MalformedCapability,
                        "a foreign capability is written Ffi[Lib] with one library name",
                        span,
                    );
                    continue;
                }
            };
            if !self.imports.libraries.contains_key(&library) {
                self.error(
                    FfiErrorKind::UnknownLibrary,
                    format!("Ffi[{library}] names a library this package does not import"),
                    span,
                );
                continue;
            }
            if e.label.is_some() {
                self.error(
                    FfiErrorKind::MalformedCapability,
                    format!("Ffi[{library}] cannot be labelled; call Lib::f directly"),
                    span,
                );
                continue;
            }
            if realtime {
                self.error(
                    FfiErrorKind::Realtime,
                    format!("realtime code cannot carry Ffi[{library}] (§32.2)"),
                    span,
                );
                continue;
            }
            e.name = effect_name(&library);
            e.args.clear();
        }
    }
    fn ty(&mut self, t: &mut Type, span: &Span) {
        match t {
            Type::Named(_, args) | Type::Tuple(args) => {
                for a in args {
                    self.ty(a, span);
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for (_, p) in params {
                    self.ty(p, span);
                }
                self.ty(result, span);
                self.effects(effects, span);
            }
            Type::Row(effects) => self.effects(effects, span),
            Type::IntLiteral(_) | Type::Infer => {}
        }
    }
    fn foreign_path<'n>(&self, name: &'n str) -> Option<(&'n str, &'n str)> {
        let (library, symbol) = name.split_once("::")?;
        self.imports
            .libraries
            .contains_key(library)
            .then_some((library, symbol))
    }
    fn expr(&mut self, e: &mut Expr) {
        let span = e.span.clone();
        match &mut e.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Name(n) => {
                if let Some((library, symbol)) = self.foreign_path(n) {
                    let message =
                        format!("foreign function {library}::{symbol} can only be called");
                    self.error(FfiErrorKind::ForeignValue, message, &span);
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                for t in types.iter_mut() {
                    self.ty(t, &span);
                }
                for a in args.iter_mut() {
                    self.expr(&mut a.expr);
                }
                let foreign = match &callee.kind {
                    ExprKind::Name(n) => self
                        .foreign_path(n)
                        .map(|(l, s)| (l.to_owned(), s.to_owned())),
                    _ => None,
                };
                match foreign {
                    Some((library, symbol)) => {
                        if let Some(name) = self.call(&library, &symbol, args, &span) {
                            callee.kind = ExprKind::Name(name);
                        }
                    }
                    None => self.expr(callee),
                }
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    self.expr(e);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields {
                    self.expr(e);
                }
            }
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    match s {
                        Statement::Let { ty, value, .. } => {
                            self.ty(ty, &span);
                            self.expr(value);
                        }
                        Statement::Expr(e) => self.expr(e),
                    }
                }
                if let Some(t) = tail {
                    self.expr(t);
                }
            }
            ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => self.expr(b),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition);
                self.expr(then);
                if let Some(o) = otherwise {
                    self.expr(o);
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value);
                for (_, a) in arms {
                    self.expr(a);
                }
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                for p in params {
                    self.ty(&mut p.ty, &span);
                }
                self.ty(result, &span);
                self.effects(effects, &span);
                self.expr(body);
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                if effect.name == CAPABILITY || self.imports.libraries.contains_key(&effect.name) {
                    self.error(
                        FfiErrorKind::ForeignHandler,
                        "foreign capabilities are granted by the package's import, not handled in source",
                        &span,
                    );
                }
                for a in &mut effect.args {
                    self.ty(a, &span);
                }
                if let Some((_, s)) = state {
                    self.expr(s);
                }
                self.expr(body);
                self.expr(&mut ret.body);
                for c in ops {
                    self.expr(&mut c.body);
                }
            }
        }
    }
    /// The operation name `Lib::symbol` resolves to, or `None` on error.
    fn call(&mut self, library: &str, symbol: &str, args: &[Arg], span: &Span) -> Option<String> {
        let import = &self.imports.libraries[library];
        let Some(function) = import.descriptor.function(symbol) else {
            self.error(
                FfiErrorKind::UnknownForeignFunction,
                format!("library '{library}' declares no function '{symbol}'"),
                span,
            );
            return None;
        };
        let shape = match self.generated.shape(library, function) {
            Ok(shape) => shape,
            Err(message) => {
                self.error(
                    FfiErrorKind::UnsupportedType,
                    format!("{library}::{symbol}: {message}"),
                    span,
                );
                return None;
            }
        };
        let effect = effect_name(library);
        let handler = import.handler;
        let Some(arities) = self.arities else {
            if args.len() == shape.params.len() {
                let slices: Vec<(Span, Type, bool)> = shape
                    .params
                    .iter()
                    .zip(args)
                    .filter_map(|(p, a)| match &p.kind {
                        ParamKind::Slice { element, nullable } => {
                            Some((a.expr.span.clone(), element.clone(), *nullable))
                        }
                        ParamKind::Value(_) => None,
                    })
                    .collect();
                if !slices.is_empty() {
                    self.slice_sites.push(SliceSite {
                        call: span.clone(),
                        slices,
                    });
                }
            }
            return Some(format!("{effect}::{symbol}"));
        };
        let lengths = arities
            .get(&(span.start, span.end))
            .cloned()
            .unwrap_or_default();
        if lengths.len() != shape.slice_count() {
            // Arity mismatch: the checker reports it against the base op.
            self.instances
                .entry((library.to_owned(), symbol.to_owned()))
                .or_default()
                .insert(vec![]);
            return Some(format!("{effect}::{symbol}"));
        }
        self.calls.push(ForeignCall {
            span: span.clone(),
            library: library.to_owned(),
            symbol: symbol.to_owned(),
            handler,
            slice_lengths: lengths.clone(),
        });
        let name = types::op_name(symbol, &lengths);
        self.instances
            .entry((library.to_owned(), symbol.to_owned()))
            .or_default()
            .insert(lengths);
        Some(format!("{effect}::{name}"))
    }
}

/// Source-facing description of a foreign parameter, for tooling.
pub fn describe_parameter(p: &ParameterType) -> String {
    match p {
        ParameterType::Value(v) => describe_value(v),
        ParameterType::Pointer {
            pointee,
            access,
            nullable,
        } => {
            let t = pointee.name().to_owned();
            let t = if *nullable { format!("Option[{t}]") } else { t };
            format!(
                "{} {t}",
                match access {
                    PointerAccess::In => "in",
                    PointerAccess::Out | PointerAccess::Inout => "inout",
                }
            )
        }
        ParameterType::Slice {
            element,
            access,
            nullable,
        } => {
            let t = format!("({}, ...)", element.name());
            let t = if *nullable { format!("Option[{t}]") } else { t };
            format!(
                "{} {t}",
                match access {
                    Access::In => "in",
                    Access::Inout => "inout",
                }
            )
        }
    }
}
fn describe_value(v: &ValueType) -> String {
    match v {
        ValueType::Unit => "Unit".into(),
        ValueType::Scalar(s) => s.name().into(),
        ValueType::Bool => "bool".into(),
        ValueType::Enum(e) => e.name().into(),
        ValueType::Address { pointee, .. } => format!("Ptr[{}]", pointee.name()),
    }
}
