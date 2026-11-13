//! Rust crate front end over `syn`.
//!
//! Public free functions and public inherent methods are lifted:
//!
//! * `&T` / `&self` become `in`, `&mut T` / `&mut self` become `inout`, owned
//!   non-`Copy` values become `sink`, and `Copy` scalars stay plain values;
//! * when a borrowed parameter's type is a crate struct and the body only
//!   reaches it through field projections, the parameter gets a FieldTree
//!   footprint (`inout state.{last_crc, record_count}: DecoderStats`). A body
//!   scan is a conservative syntactic summary: a method call on a field of a
//!   unique borrow counts as a write, any whole-value use, shadowing or macro
//!   mention drops the footprint.
//!
//! Refused with a typed reason: raw pointers without a `vesper-migrate:`
//! annotation, interior mutability (`Cell`, `RefCell`, atomics, locks and
//! structs containing them), self-referential lifetimes (`&'a mut T<'a>`),
//! returned borrows, types that carry lifetimes, generics, `impl`/`dyn`
//! traits, function pointers and data-carrying enums.
//!
//! Only `#[no_mangle] pub extern "C" fn` items get `.vsys` entries; other
//! functions get Vesper declarations and a [`RefusalReason::NotCAbi`] note.
//!
//! Raw-pointer annotation (a doc line on the function, valid Rust):
//! `/// vesper-migrate: <param> in|out|inout [len <len_param>]`. `len` folds
//! the following `usize` parameter into a slice (`in`/`inout` only); without
//! it the pointer is a single non-null element, lifted to
//! `nocapture in|out|inout name: T` for an integer or wire-struct pointee.
//! Two annotated raw pointers to the same type, one of them written, are
//! refused as [`RefusalReason::AliasingPointers`]. In C-ABI functions,
//! `&T`/`&mut T` of an integer or struct lift to `nocapture in|inout`
//! single pointers (Rust references are non-null and never alias a `&mut`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::Visit;
use syn::{
    Attribute, BinOp, Expr, Fields, FnArg, GenericArgument, ImplItem, Item, Member, Pat,
    PathArguments, ReturnType, Signature, Type, Visibility,
};
use vesper_ffi::{Declaration, Endian, Field, Integer, WireType};

use crate::{
    Access, ErrorKind, LiftedEnum, LiftedField, LiftedFunction, LiftedParam, LiftedStruct, Limits,
    MigrateError, Migration, Mode, Origin, PointerAccess, Refusal, RefusalReason, Scalar, SysParam,
    SysPointee, SysSignature, SysValue, lift::LegacyRustParamSummary,
};

const INTERIOR: &[&str] = &[
    "Cell",
    "RefCell",
    "UnsafeCell",
    "OnceCell",
    "LazyCell",
    "SyncUnsafeCell",
    "Mutex",
    "RwLock",
    "OnceLock",
    "LazyLock",
    "Rc",
    "Arc",
    "Weak",
];

type Why = (RefusalReason, String);

#[derive(Debug, Clone, PartialEq, Eq)]
enum RTy {
    Unit,
    Scalar(Scalar),
    Bool,
    /// Vesper-only primitive (`isize`, `u128`, `char`, `f32`, ...).
    Prim(String, RefusalReason),
    Named(String),
    Str,
    Ref {
        mutable: bool,
        inner: Box<RTy>,
    },
    Slice(Box<RTy>),
    Array(Box<RTy>, u64),
    Raw {
        mutable: bool,
        inner: Box<RTy>,
    },
    Std(String, Vec<RTy>),
    Tuple(Vec<RTy>),
}

impl RTy {
    fn vesper(&self) -> String {
        match self {
            Self::Unit => "Unit".into(),
            Self::Scalar(s) => s.name().into(),
            Self::Bool => "bool".into(),
            Self::Prim(p, _) => p.clone(),
            Self::Named(n) => n.clone(),
            Self::Str => "Str".into(),
            Self::Ref { inner, .. } | Self::Raw { inner, .. } => inner.vesper(),
            Self::Slice(e) if **e == Self::Scalar(Scalar::U8) => "ByteSlice".into(),
            Self::Slice(e) => format!("Slice[{}]", e.vesper()),
            Self::Array(e, n) => format!("[{}; {n}]", e.vesper()),
            Self::Std(n, args) if args.is_empty() => n.clone(),
            Self::Std(n, args) => format!(
                "{n}[{}]",
                args.iter().map(Self::vesper).collect::<Vec<_>>().join(", ")
            ),
            Self::Tuple(ts) => format!(
                "({})",
                ts.iter().map(Self::vesper).collect::<Vec<_>>().join(", ")
            ),
        }
    }
    fn contains_borrow(&self) -> bool {
        match self {
            Self::Ref { .. } | Self::Raw { .. } => true,
            Self::Slice(e) | Self::Array(e, _) => e.contains_borrow(),
            Self::Std(_, a) | Self::Tuple(a) => a.iter().any(Self::contains_borrow),
            _ => false,
        }
    }
    fn is_copy_value(&self) -> bool {
        matches!(
            self,
            Self::Scalar(_) | Self::Bool | Self::Unit | Self::Prim(..)
        )
    }
}

/// One module's items, with the module path for messages.
struct ModuleItems {
    items: Vec<Item>,
}

struct Crate {
    name: String,
    modules: Vec<ModuleItems>,
    notes: Vec<Refusal>,
}

fn file_error(path: &Path, e: syn::Error) -> MigrateError {
    let line = e.span().start().line;
    MigrateError::new(
        ErrorKind::Syntax,
        Some(line.max(1)),
        format!("{}: {e}", path.display()),
    )
}

fn parse_file(path: &Path, source: &str, limits: Limits) -> Result<syn::File, MigrateError> {
    if source.len() > limits.source_bytes {
        return Err(MigrateError::new(
            ErrorKind::Limit,
            None,
            format!("{}: source byte budget exceeded", path.display()),
        ));
    }
    crate::check_nesting(source, limits.nesting)?;
    syn::parse_file(source).map_err(|e| file_error(path, e))
}

fn is_pub(v: &Visibility) -> bool {
    matches!(v, Visibility::Public(_))
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// `#[cfg]` / `#[cfg_attr]` items depend on a build configuration that this
/// front end does not evaluate, so they are refused rather than treated as
/// unconditional exports.
fn conditional(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
}

fn cfg_refusal(item: String) -> Refusal {
    Refusal::new(
        item,
        RefusalReason::UnsupportedItem,
        "conditional (#[cfg]/#[cfg_attr]) item: target and feature configuration is not evaluated",
    )
}

fn item_attrs(item: &Item) -> Option<(&[Attribute], String)> {
    Some(match item {
        Item::Struct(s) => (&s.attrs, format!("struct {}", s.ident)),
        Item::Enum(e) => (&e.attrs, format!("enum {}", e.ident)),
        Item::Type(x) => (&x.attrs, format!("type {}", x.ident)),
        Item::Fn(f) => (&f.attrs, format!("fn {}", f.sig.ident)),
        Item::Impl(i) => (
            &i.attrs,
            format!("impl {}", quote::ToTokens::to_token_stream(&i.self_ty)),
        ),
        Item::Trait(x) => (&x.attrs, format!("trait {}", x.ident)),
        Item::Const(x) => (&x.attrs, format!("const {}", x.ident)),
        Item::Static(x) => (&x.attrs, format!("static {}", x.ident)),
        Item::Union(x) => (&x.attrs, format!("union {}", x.ident)),
        _ => return None,
    })
}

fn no_mangle(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("no_mangle")
            || (a.path().is_ident("unsafe")
                && a.meta
                    .require_list()
                    .is_ok_and(|l| l.tokens.to_string().trim() == "no_mangle"))
    })
}

fn repr(attrs: &[Attribute]) -> Vec<String> {
    let mut out = vec![];
    for a in attrs.iter().filter(|a| a.path().is_ident("repr")) {
        let parsed = a.parse_nested_meta(|m| {
            out.push(
                m.path
                    .get_ident()
                    .map_or_else(|| "<path>".to_string(), |i| i.to_string()),
            );
            if m.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in m.input);
                let _: TokenStream = content.parse()?;
            }
            Ok(())
        });
        if parsed.is_err() {
            // Unknown syntax is never mistaken for a plain `repr(C)`.
            out.push("<unparsed>".into());
        }
    }
    out
}

fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(l) => match &l.lit {
                    syn::Lit::Str(s) => Some(s.value()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Load a crate rooted at `dir` (reads `Cargo.toml` for the package name and
/// follows `pub mod x;` from `src/lib.rs`).
fn load_dir(dir: &Path, limits: Limits) -> Result<Crate, MigrateError> {
    let io = |p: &Path, e: std::io::Error| {
        MigrateError::new(ErrorKind::Io, None, format!("{}: {e}", p.display()))
    };
    let manifest = dir.join("Cargo.toml");
    let name = match fs::read_to_string(&manifest) {
        Ok(text) => package_name(&text).ok_or_else(|| {
            MigrateError::new(ErrorKind::Syntax, None, "Cargo.toml has no [package] name")
        })?,
        Err(e) => return Err(io(&manifest, e)),
    };
    let mut modules = vec![];
    let mut notes = vec![];
    let mut total = 0usize;
    let mut files = 0usize;
    // (file, directory for child modules)
    let mut queue: Vec<(PathBuf, PathBuf, String)> = vec![(
        dir.join("src").join("lib.rs"),
        dir.join("src"),
        "crate".into(),
    )];
    while let Some((file, child_dir, _)) = queue.pop() {
        files += 1;
        if files > limits.files {
            return Err(MigrateError::new(
                ErrorKind::Limit,
                None,
                "crate file budget exceeded",
            ));
        }
        let meta = fs::metadata(&file).map_err(|e| io(&file, e))?;
        if meta.len() > limits.source_bytes as u64 {
            return Err(MigrateError::new(
                ErrorKind::Limit,
                None,
                format!("{}: source byte budget exceeded", file.display()),
            ));
        }
        let source = fs::read_to_string(&file).map_err(|e| io(&file, e))?;
        total = total.saturating_add(source.len());
        if total > limits.total_bytes {
            return Err(MigrateError::new(
                ErrorKind::Limit,
                None,
                "crate byte budget exceeded",
            ));
        }
        let parsed = parse_file(&file, &source, limits)?;
        let mut items = vec![];
        collect_items(
            parsed.items,
            &child_dir,
            &mut items,
            &mut queue,
            &mut modules,
            true,
            &mut notes,
        )?;
        modules.push(ModuleItems { items });
    }
    Ok(Crate {
        name,
        modules,
        notes,
    })
}

fn collect_items(
    items: Vec<Item>,
    child_dir: &Path,
    out: &mut Vec<Item>,
    queue: &mut Vec<(PathBuf, PathBuf, String)>,
    modules: &mut Vec<ModuleItems>,
    resolve: bool,
    notes: &mut Vec<Refusal>,
) -> Result<(), MigrateError> {
    for item in items {
        match item {
            Item::Mod(m) if is_pub(&m.vis) => {
                let name = m.ident.to_string();
                if has_attr(&m.attrs, "path") {
                    notes.push(Refusal::new(
                        format!("mod {name}"),
                        RefusalReason::UnsupportedItem,
                        "`#[path]` modules are not followed",
                    ));
                    continue;
                }
                if conditional(&m.attrs) {
                    notes.push(cfg_refusal(format!("mod {name}")));
                    continue;
                }
                match m.content {
                    Some((_, inner)) => {
                        let mut sub = vec![];
                        collect_items(
                            inner,
                            &child_dir.join(&name),
                            &mut sub,
                            queue,
                            modules,
                            resolve,
                            notes,
                        )?;
                        modules.push(ModuleItems { items: sub });
                    }
                    None => {
                        let flat = child_dir.join(format!("{name}.rs"));
                        let nested = child_dir.join(&name).join("mod.rs");
                        let file = if !resolve || flat.is_file() {
                            flat
                        } else {
                            nested
                        };
                        queue.push((file, child_dir.join(&name), name));
                    }
                }
            }
            other => out.push(other),
        }
    }
    Ok(())
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_package = l == "[package]";
            continue;
        }
        if in_package && let Some(rest) = l.strip_prefix("name") {
            {
                let v = rest
                    .trim_start()
                    .strip_prefix('=')?
                    .trim()
                    .trim_matches('"');
                if !v.is_empty()
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    return Some(v.replace('-', "_"));
                }
                return None;
            }
        }
    }
    None
}

/// Analyse a crate directory containing `Cargo.toml` and `src/lib.rs`.
pub fn analyze_crate_dir(dir: &Path, limits: Limits) -> Result<Migration, MigrateError> {
    let krate = load_dir(dir, limits)?;
    analyze(krate, limits)
}

/// Analyse a single Rust source file as the root of crate `crate_name`.
/// Out-of-line `pub mod x;` declarations are refused (use
/// [`analyze_crate_dir`] to follow them).
pub fn analyze_source(
    crate_name: &str,
    source: &str,
    limits: Limits,
) -> Result<Migration, MigrateError> {
    let parsed = parse_file(Path::new("<source>"), source, limits)?;
    let mut items = vec![];
    let mut modules = vec![];
    let mut queue = vec![];
    let mut notes = vec![];
    collect_items(
        parsed.items,
        Path::new(""),
        &mut items,
        &mut queue,
        &mut modules,
        false,
        &mut notes,
    )?;
    let mut krate = Crate {
        name: crate_name.to_owned(),
        modules,
        notes,
    };
    krate.modules.push(ModuleItems { items });
    let mut m = analyze(krate, limits)?;
    for (_, _, name) in queue {
        m.refusals.push(Refusal::new(
            format!("mod {name}"),
            RefusalReason::UnsupportedItem,
            "out-of-line module not followed from a single source; analyse the crate directory",
        ));
    }
    m.refusals.sort();
    Ok(m)
}

struct Analyzer {
    structs: BTreeMap<String, syn::ItemStruct>,
    enums: BTreeMap<String, syn::ItemEnum>,
    aliases: BTreeMap<String, Type>,
    /// Types refused entirely; anything mentioning them is refused too.
    refused: BTreeMap<String, Why>,
    enum_carriers: BTreeMap<String, Result<Scalar, Refusal>>,
}

impl Analyzer {
    fn rty(&self, ty: &Type, self_ty: Option<&str>, depth: usize) -> Result<RTy, Why> {
        if depth > 32 {
            return Err((
                RefusalReason::UnsupportedType,
                "type nesting too deep".into(),
            ));
        }
        Ok(match ty {
            Type::Paren(p) => self.rty(&p.elem, self_ty, depth + 1)?,
            Type::Group(g) => self.rty(&g.elem, self_ty, depth + 1)?,
            Type::Tuple(t) if t.elems.is_empty() => RTy::Unit,
            Type::Tuple(t) => RTy::Tuple(
                t.elems
                    .iter()
                    .map(|e| self.rty(e, self_ty, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Reference(r) => {
                if let Some(lt) = &r.lifetime
                    && lt.ident != "static"
                    && mentions_lifetime(&r.elem, &lt.ident.to_string())
                {
                    {
                        return Err((
                            RefusalReason::SelfReferentialLifetime,
                            format!(
                                "`&'{}` borrows a referent that is itself parameterized by `'{}`",
                                lt.ident, lt.ident
                            ),
                        ));
                    }
                }
                let inner = self.rty(&r.elem, self_ty, depth + 1)?;
                if inner.contains_borrow() {
                    return Err((
                        RefusalReason::BorrowingType,
                        "a borrow of a borrow cannot be made lifetime-free".into(),
                    ));
                }
                RTy::Ref {
                    mutable: r.mutability.is_some(),
                    inner: Box::new(inner),
                }
            }
            Type::Ptr(p) => RTy::Raw {
                mutable: p.mutability.is_some(),
                inner: Box::new(self.rty(&p.elem, self_ty, depth + 1)?),
            },
            Type::Slice(s) => RTy::Slice(Box::new(self.rty(&s.elem, self_ty, depth + 1)?)),
            Type::Array(a) => {
                let len = match &a.len {
                    Expr::Lit(l) => match &l.lit {
                        syn::Lit::Int(i) => i.base10_parse::<u64>().ok(),
                        _ => None,
                    },
                    _ => None,
                }
                .ok_or((
                    RefusalReason::UnsupportedType,
                    "array length is not an integer literal".into(),
                ))?;
                RTy::Array(Box::new(self.rty(&a.elem, self_ty, depth + 1)?), len)
            }
            Type::ImplTrait(_) | Type::TraitObject(_) => {
                return Err((
                    RefusalReason::Generic,
                    "`impl Trait`/`dyn Trait` has no concrete layout".into(),
                ));
            }
            Type::BareFn(_) => {
                return Err((
                    RefusalReason::FunctionPointer,
                    "function pointers are unsupported".into(),
                ));
            }
            Type::Path(p) if p.qself.is_none() => {
                let seg = p
                    .path
                    .segments
                    .last()
                    .ok_or((RefusalReason::UnsupportedType, "empty path".into()))?;
                let name = seg.ident.to_string();
                let mut args = vec![];
                if let PathArguments::AngleBracketed(a) = &seg.arguments {
                    for g in &a.args {
                        match g {
                            GenericArgument::Type(t) => {
                                args.push(self.rty(t, self_ty, depth + 1)?)
                            }
                            GenericArgument::Lifetime(_) => {
                                return Err((
                                    RefusalReason::BorrowingType,
                                    format!("`{name}` carries a lifetime, so it stores borrows"),
                                ));
                            }
                            _ => {
                                return Err((
                                    RefusalReason::Generic,
                                    format!("unsupported generic argument on `{name}`"),
                                ));
                            }
                        }
                    }
                } else if !matches!(seg.arguments, PathArguments::None) {
                    return Err((
                        RefusalReason::FunctionPointer,
                        format!("`{name}(..)` closure trait types are unsupported"),
                    ));
                }
                if args.iter().any(RTy::contains_borrow) {
                    return Err((
                        RefusalReason::BorrowingType,
                        format!("`{name}` holds a borrow"),
                    ));
                }
                if INTERIOR.contains(&name.as_str()) || name.starts_with("Atomic") {
                    return Err((
                        RefusalReason::InteriorMutability,
                        format!("`{name}` permits mutation through a shared reference"),
                    ));
                }
                let scalar = |s| {
                    if args.is_empty() {
                        Ok(RTy::Scalar(s))
                    } else {
                        Err((RefusalReason::UnsupportedType, "generic scalar".to_string()))
                    }
                };
                match name.as_str() {
                    "u8" => scalar(Scalar::U8)?,
                    "i8" => scalar(Scalar::I8)?,
                    "u16" => scalar(Scalar::U16)?,
                    "i16" => scalar(Scalar::I16)?,
                    "u32" => scalar(Scalar::U32)?,
                    "i32" => scalar(Scalar::I32)?,
                    "u64" => scalar(Scalar::U64)?,
                    "i64" => scalar(Scalar::I64)?,
                    "usize" => scalar(Scalar::Usize)?,
                    "bool" => RTy::Bool,
                    "str" => RTy::Str,
                    "isize" => RTy::Prim("isize".into(), RefusalReason::PlatformDependentWidth),
                    "u128" | "i128" | "char" => {
                        RTy::Prim(name.clone(), RefusalReason::UnsupportedType)
                    }
                    "f32" | "f64" => RTy::Prim(name.clone(), RefusalReason::Floating),
                    "String" if args.is_empty() => RTy::Std("String".into(), vec![]),
                    "Vec" | "Option" | "Box" if args.len() == 1 => RTy::Std(name.clone(), args),
                    "Result" if args.len() == 2 => RTy::Std(name.clone(), args),
                    "Self" => match self_ty {
                        Some(s) => RTy::Named(s.to_owned()),
                        None => {
                            return Err((
                                RefusalReason::UnsupportedType,
                                "`Self` outside an impl".into(),
                            ));
                        }
                    },
                    _ if p.path.segments.len() == 1
                        || p.path.segments.first().is_some_and(|s| {
                            s.ident == "crate" || s.ident == "self" || s.ident == "super"
                        }) =>
                    {
                        if let Some((r, d)) = self.refused.get(&name) {
                            return Err((*r, format!("uses `{name}`, which was refused: {d}")));
                        }
                        if let Some(t) = self.aliases.get(&name) {
                            return self.rty(&t.clone(), self_ty, depth + 1);
                        }
                        if self.structs.contains_key(&name) || self.enums.contains_key(&name) {
                            if !args.is_empty() {
                                return Err((
                                    RefusalReason::Generic,
                                    format!("generic use of `{name}`"),
                                ));
                            }
                            RTy::Named(name)
                        } else {
                            return Err((
                                RefusalReason::UnsupportedType,
                                format!(
                                    "unknown type `{name}` (not a public crate item or supported std type)"
                                ),
                            ));
                        }
                    }
                    _ => {
                        return Err((
                            RefusalReason::UnsupportedType,
                            format!(
                                "unsupported external type `{}`",
                                quote::ToTokens::to_token_stream(&p.path)
                            ),
                        ));
                    }
                }
            }
            _ => {
                return Err((
                    RefusalReason::UnsupportedType,
                    "unsupported type form".into(),
                ));
            }
        })
    }
}

fn mentions_lifetime(ty: &Type, name: &str) -> bool {
    struct V<'a>(&'a str, bool);
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_lifetime(&mut self, l: &'ast syn::Lifetime) {
            if l.ident == self.0 {
                self.1 = true;
            }
        }
    }
    let mut v = V(name, false);
    v.visit_type(ty);
    v.1
}

fn tokens_mention(ts: &TokenStream, name: &str) -> bool {
    ts.clone().into_iter().any(|t| match t {
        TokenTree::Ident(i) => i == name,
        TokenTree::Group(g) => tokens_mention(&g.stream(), name),
        _ => false,
    })
}

/// Field path of `name.a.b` (looking through parens, derefs and indexing).
fn field_path(e: &Expr, name: &str) -> Option<String> {
    match e {
        Expr::Paren(p) => field_path(&p.expr, name),
        Expr::Index(i) => field_path(&i.expr, name),
        Expr::Field(f) => {
            let member = match &f.member {
                Member::Named(i) => i.to_string(),
                Member::Unnamed(i) => i.index.to_string(),
            };
            if is_name(&f.base, name) {
                Some(member)
            } else {
                field_path(&f.base, name).map(|p| format!("{p}.{member}"))
            }
        }
        _ => None,
    }
}

fn is_name(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Path(p) => p.qself.is_none() && p.path.is_ident(name),
        Expr::Paren(p) => is_name(&p.expr, name),
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => is_name(&u.expr, name),
        _ => false,
    }
}

struct Scan<'a> {
    name: &'a str,
    unique: bool,
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    whole: bool,
}

fn compound(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
            | BinOp::BitXorAssign(_)
            | BinOp::BitAndAssign(_)
            | BinOp::BitOrAssign(_)
            | BinOp::ShlAssign(_)
            | BinOp::ShrAssign(_)
    )
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_expr(&mut self, e: &'ast Expr) {
        match e {
            Expr::Assign(a) => {
                if let Some(p) = field_path(&a.left, self.name) {
                    self.writes.insert(p);
                    self.visit_place_indices(&a.left);
                    self.visit_expr(&a.right);
                    return;
                }
            }
            Expr::Binary(b) if compound(&b.op) => {
                if let Some(p) = field_path(&b.left, self.name) {
                    self.writes.insert(p);
                    self.visit_place_indices(&b.left);
                    self.visit_expr(&b.right);
                    return;
                }
            }
            Expr::Reference(r) => {
                if let Some(p) = field_path(&r.expr, self.name) {
                    if r.mutability.is_some() {
                        self.writes.insert(p);
                    } else {
                        self.reads.insert(p);
                    }
                    self.visit_place_indices(&r.expr);
                    return;
                }
            }
            Expr::MethodCall(m) => {
                if let Some(p) = field_path(&m.receiver, self.name) {
                    // Auto-ref may take `&mut` of the field.
                    if self.unique {
                        self.writes.insert(p);
                    } else {
                        self.reads.insert(p);
                    }
                    self.visit_place_indices(&m.receiver);
                    for a in &m.args {
                        self.visit_expr(a);
                    }
                    return;
                }
            }
            Expr::Field(_) => {
                if let Some(p) = field_path(e, self.name) {
                    self.reads.insert(p);
                    self.visit_place_indices(e);
                    return;
                }
            }
            Expr::Path(_) if is_name(e, self.name) => {
                self.whole = true;
                return;
            }
            _ => {}
        }
        syn::visit::visit_expr(self, e);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if tokens_mention(&m.tokens, self.name) {
            self.whole = true;
        }
    }
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        if p.ident == self.name {
            self.whole = true;
        }
        syn::visit::visit_pat_ident(self, p);
    }
    fn visit_item(&mut self, _: &'ast Item) {}
}

impl Scan<'_> {
    /// Visit the index expressions inside a projected place
    /// (`s.items[s.cursor].x`): they are evaluated, so their accesses count.
    fn visit_place_indices(&mut self, e: &Expr) {
        match e {
            Expr::Paren(p) => self.visit_place_indices(&p.expr),
            Expr::Unary(u) => self.visit_place_indices(&u.expr),
            Expr::Field(f) => self.visit_place_indices(&f.base),
            Expr::Index(i) => {
                self.visit_place_indices(&i.expr);
                self.visit_expr(&i.index);
            }
            _ => {}
        }
    }
}

struct PtrAnnotation {
    access: PointerAccess,
    len: Option<String>,
}

fn annotations(
    attrs: &[Attribute],
    item: &str,
) -> Result<BTreeMap<String, PtrAnnotation>, Refusal> {
    let mut out = BTreeMap::new();
    for line in doc_lines(attrs) {
        let Some(rest) = line.trim().strip_prefix("vesper-migrate:") else {
            continue;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let bad = || {
            Refusal::new(
                item,
                RefusalReason::BadAnnotation,
                format!(
                    "expected `vesper-migrate: <param> in|out|inout [len <param>]`, got `{}`",
                    line.trim()
                ),
            )
        };
        let (param, access, len) = match words.as_slice() {
            [p, a] => (p, a, None),
            [p, a, "len", l] => (p, a, Some(l.to_string())),
            _ => return Err(bad()),
        };
        let access = match *access {
            "in" => PointerAccess::In,
            "out" => PointerAccess::Out,
            "inout" => PointerAccess::Inout,
            _ => return Err(bad()),
        };
        if access == PointerAccess::Out && len.is_some() {
            // An out slice would expose uninitialized elements.
            return Err(bad());
        }
        if out
            .insert(param.to_string(), PtrAnnotation { access, len })
            .is_some()
        {
            return Err(bad());
        }
    }
    Ok(out)
}

struct RawParam {
    name: String,
    ty: RTy,
    receiver: bool,
    /// A `&'static` borrow: the callee may keep it, so it is not `nocapture`.
    static_ref: bool,
}

impl Analyzer {
    #[allow(clippy::too_many_arguments)]
    fn lift_fn(
        &self,
        display: &str,
        sig: &Signature,
        attrs: &[Attribute],
        body: Option<&syn::Block>,
        self_ty: Option<&str>,
        c_abi: Result<(), Refusal>,
        limits: Limits,
    ) -> Result<LiftedFunction, Refusal> {
        let item = format!("fn {display}");
        let refuse = |(r, d): Why| Refusal::new(&item, r, d);
        if sig.asyncness.is_some() {
            return Err(refuse((
                RefusalReason::UnsupportedItem,
                "async functions return opaque futures".into(),
            )));
        }
        if sig.variadic.is_some() {
            return Err(refuse((
                RefusalReason::Variadic,
                "variadic function".into(),
            )));
        }
        if sig
            .generics
            .params
            .iter()
            .any(|g| !matches!(g, syn::GenericParam::Lifetime(_)))
            || sig.generics.where_clause.is_some()
        {
            return Err(refuse((
                RefusalReason::Generic,
                "type and const generics are not lifted".into(),
            )));
        }
        if sig.inputs.len() > limits.parameters {
            return Err(refuse((
                RefusalReason::UnsupportedItem,
                "parameter budget exceeded".into(),
            )));
        }
        let notes = annotations(attrs, &item)?;
        let mut raw = vec![];
        for input in &sig.inputs {
            match input {
                FnArg::Receiver(r) => {
                    let Some(st) = self_ty else {
                        return Err(refuse((
                            RefusalReason::UnsupportedItem,
                            "receiver outside impl".into(),
                        )));
                    };
                    if r.colon_token.is_some() {
                        return Err(refuse((
                            RefusalReason::UnsupportedType,
                            "typed `self` receivers are not lifted".into(),
                        )));
                    }
                    let base = RTy::Named(st.to_owned());
                    let ty = match &r.reference {
                        Some(_) => RTy::Ref {
                            mutable: r.mutability.is_some(),
                            inner: Box::new(base),
                        },
                        None => base,
                    };
                    let static_ref = r
                        .reference
                        .as_ref()
                        .and_then(|(_, l)| l.as_ref())
                        .is_some_and(|l| l.ident == "static");
                    raw.push(RawParam {
                        name: "self".into(),
                        ty,
                        receiver: true,
                        static_ref,
                    });
                }
                FnArg::Typed(t) => {
                    let Pat::Ident(pi) = &*t.pat else {
                        return Err(refuse((
                            RefusalReason::UnsupportedItem,
                            "destructuring parameters are not lifted".into(),
                        )));
                    };
                    if pi.by_ref.is_some() || pi.subpat.is_some() {
                        return Err(refuse((
                            RefusalReason::UnsupportedItem,
                            "binding-mode parameters are not lifted".into(),
                        )));
                    }
                    let ty = self
                        .rty(&t.ty, self_ty, 0)
                        .map_err(|(r, d)| refuse((r, format!("parameter `{}`: {d}", pi.ident))))?;
                    let static_ref = matches!(
                        &*t.ty,
                        syn::Type::Reference(r)
                            if r.lifetime.as_ref().is_some_and(|l| l.ident == "static")
                    );
                    raw.push(RawParam {
                        name: pi.ident.to_string(),
                        ty,
                        receiver: false,
                        static_ref,
                    });
                }
            }
        }
        for n in notes.keys() {
            if !raw
                .iter()
                .any(|p| &p.name == n && matches!(p.ty, RTy::Raw { .. }))
            {
                return Err(refuse((
                    RefusalReason::BadAnnotation,
                    format!("annotation names `{n}`, which is not a raw-pointer parameter"),
                )));
            }
        }
        let result = match &sig.output {
            ReturnType::Default => RTy::Unit,
            ReturnType::Type(_, t) => self
                .rty(t, self_ty, 0)
                .map_err(|(r, d)| refuse((r, format!("return type: {d}"))))?,
        };
        if matches!(result, RTy::Raw { .. }) {
            return Err(refuse((
                RefusalReason::RawPointerWithoutAnnotation,
                "returns a raw pointer; ownership and lifetime of the pointee are unknown".into(),
            )));
        }
        if result.contains_borrow() {
            return Err(refuse((
                RefusalReason::ReturnedBorrow,
                "returns a borrow; Vesper signatures are lifetime-free".into(),
            )));
        }
        let mut lifted = vec![];
        let mut sys_params = vec![];
        let mut sys_err: Option<Refusal> = c_abi.err();
        let sys_refuse = |why: Why, slot: &mut Option<Refusal>| {
            slot.get_or_insert_with(|| Refusal::new(format!("fn {display}: vsys"), why.0, why.1));
        };
        let mut assumptions = vec![];
        let mut raw_pointers: Vec<(String, SysParam)> = vec![];
        let mut skip_len: Option<String> = None;
        let safe = crate::synth::safe_idents(raw.iter().map(|p| p.name.as_str()));
        for (index, p) in raw.iter().enumerate() {
            if skip_len.as_deref() == Some(p.name.as_str()) {
                skip_len = None;
                continue;
            }
            let pname = if p.receiver {
                p.name.clone()
            } else {
                safe[index].clone()
            };
            match &p.ty {
                RTy::Raw { mutable, inner } => {
                    let Some(note) = notes.get(&p.name) else {
                        return Err(refuse((
                            RefusalReason::RawPointerWithoutAnnotation,
                            format!(
                                "parameter `{}` is a raw pointer; add `/// vesper-migrate: {} in|inout [len <param>]`",
                                p.name, p.name
                            ),
                        )));
                    };
                    if note.access.writes() && !mutable {
                        return Err(refuse((
                            RefusalReason::BadAnnotation,
                            format!(
                                "`{}` is `*const` but annotated {}",
                                p.name,
                                note.access.keyword()
                            ),
                        )));
                    }
                    let mode = if note.access == PointerAccess::In {
                        Mode::In
                    } else {
                        Mode::Inout
                    };
                    let pointee = match &**inner {
                        RTy::Scalar(e) => SysPointee::Scalar(*e),
                        RTy::Named(n) if note.len.is_none() && self.structs.contains_key(n) => {
                            SysPointee::Struct(n.clone())
                        }
                        _ => {
                            return Err(refuse((
                                RefusalReason::OpaquePointer,
                                format!(
                                    "annotated raw pointer `{}` must point to a fixed-width integer (or, without `len`, a crate struct)",
                                    p.name
                                ),
                            )));
                        }
                    };
                    match &note.len {
                        Some(len) => {
                            let SysPointee::Scalar(e) = pointee else {
                                unreachable!("struct pointees are refused with len")
                            };
                            let next = raw.get(index + 1);
                            if !next.is_some_and(|n| {
                                &n.name == len && n.ty == RTy::Scalar(Scalar::Usize)
                            }) {
                                return Err(refuse((
                                    RefusalReason::BadAnnotation,
                                    format!(
                                        "`len {len}` must name the `usize` parameter that immediately follows `{}`",
                                        p.name
                                    ),
                                )));
                            }
                            skip_len = Some(len.clone());
                            assumptions.push(format!(
                                "`{}` + `{len}` form a nocapture slice (from annotation)",
                                p.name
                            ));
                            let param = SysParam::Slice {
                                element: e,
                                access: if note.access == PointerAccess::In {
                                    Access::In
                                } else {
                                    Access::Inout
                                },
                            };
                            raw_pointers.push((pname.clone(), param.clone()));
                            sys_params.push((pname.clone(), param));
                            lifted.push(LiftedParam {
                                name: pname,
                                mode,
                                footprint: None,
                                ty: RTy::Slice(Box::new(RTy::Scalar(e))).vesper(),
                            });
                        }
                        None => {
                            assumptions.push(format!(
                                "`{}` is a non-null nocapture {} element (from annotation)",
                                p.name,
                                note.access.keyword()
                            ));
                            let param = SysParam::Pointer {
                                pointee: pointee.clone(),
                                access: note.access,
                                nullable: false,
                            };
                            raw_pointers.push((pname.clone(), param.clone()));
                            sys_params.push((pname.clone(), param));
                            lifted.push(LiftedParam {
                                name: pname,
                                mode,
                                footprint: None,
                                ty: pointee.name().to_owned(),
                            });
                        }
                    }
                    continue;
                }
                RTy::Ref { mutable, inner } => {
                    let footprint_type =
                        matches!(&**inner, RTy::Named(n) if self.structs.contains_key(n));
                    let lp = if footprint_type {
                        let mut scan = Scan {
                            name: &p.name,
                            unique: *mutable,
                            reads: BTreeSet::new(),
                            writes: BTreeSet::new(),
                            whole: body.is_none(),
                        };
                        if let Some(b) = body {
                            scan.visit_block(b);
                        }
                        LegacyRustParamSummary {
                            param_name: pname.clone(),
                            ty_name: inner.vesper(),
                            is_owned_move: false,
                            is_unique_borrow: *mutable,
                            whole_value_access: scan.whole,
                            read_field_paths: scan.reads.into_iter().collect(),
                            mutated_field_paths: scan.writes.into_iter().collect(),
                        }
                        .lift()
                    } else {
                        LiftedParam {
                            name: pname.clone(),
                            mode: if *mutable { Mode::Inout } else { Mode::In },
                            footprint: None,
                            ty: inner.vesper(),
                        }
                    };
                    let access = if *mutable {
                        PointerAccess::Inout
                    } else {
                        PointerAccess::In
                    };
                    match &**inner {
                        _ if p.static_ref => sys_refuse(
                            (
                                RefusalReason::PointerParameter,
                                format!(
                                    "`{}`: a `&'static` borrow may be kept by the callee, so it is not a `nocapture` pointer",
                                    p.name
                                ),
                            ),
                            &mut sys_err,
                        ),
                        RTy::Scalar(e) => sys_params.push((
                            pname.clone(),
                            SysParam::Pointer {
                                pointee: SysPointee::Scalar(*e),
                                access,
                                nullable: false,
                            },
                        )),
                        RTy::Named(n) if self.structs.contains_key(n) => sys_params.push((
                            pname.clone(),
                            SysParam::Pointer {
                                pointee: SysPointee::Struct(n.clone()),
                                access,
                                nullable: false,
                            },
                        )),
                        RTy::Named(n) => sys_refuse(
                            (
                                RefusalReason::PointerParameter,
                                format!(
                                    "`{}`: borrowed `{n}` is not an integer or wire struct; point to its integer carrier",
                                    p.name
                                ),
                            ),
                            &mut sys_err,
                        ),
                        RTy::Slice(_) | RTy::Str => sys_refuse(
                            (
                                RefusalReason::NotCAbi,
                                format!(
                                    "`{}`: Rust slices are fat pointers, not C ABI values; use an annotated pointer and length",
                                    p.name
                                ),
                            ),
                            &mut sys_err,
                        ),
                        _ => sys_refuse(
                            (
                                RefusalReason::Aggregate,
                                format!(
                                    "`{}`: borrowed aggregates are not modelled by .vsys",
                                    p.name
                                ),
                            ),
                            &mut sys_err,
                        ),
                    }
                    lifted.push(lp);
                }
                owned => {
                    let mode = if owned.is_copy_value() || self.is_copy_enum(owned) {
                        Mode::Value
                    } else {
                        Mode::Sink
                    };
                    match self.sys_value(owned) {
                        Ok(v) => sys_params.push((pname.clone(), SysParam::Value(v))),
                        Err(why) => {
                            sys_refuse((why.0, format!("`{}`: {}", p.name, why.1)), &mut sys_err)
                        }
                    }
                    lifted.push(LiftedParam {
                        name: pname,
                        mode,
                        footprint: None,
                        ty: owned.vesper(),
                    });
                }
            }
        }
        if let Some(len) = skip_len {
            return Err(refuse((
                RefusalReason::BadAnnotation,
                format!("length parameter `{len}` not found"),
            )));
        }
        if let Some(why) = crate::synth::aliasing_pair(&raw_pointers) {
            sys_refuse((RefusalReason::AliasingPointers, why), &mut sys_err);
        }
        let sys_result = match self.sys_value(&result) {
            Ok(v) => Some(v),
            Err(why) => {
                sys_refuse((why.0, format!("return: {}", why.1)), &mut sys_err);
                None
            }
        };
        let sys = match (sys_err, sys_result) {
            (None, Some(result)) => Ok(SysSignature {
                params: sys_params,
                result,
            }),
            (Some(r), _) => Err(r),
            (None, None) => unreachable!("sys_result error is recorded in sys_err"),
        };
        Ok(LiftedFunction {
            name: display.to_owned(),
            params: lifted,
            result: result.vesper(),
            sys,
            assumptions,
        })
    }

    fn is_copy_enum(&self, t: &RTy) -> bool {
        matches!(t, RTy::Named(n) if self.enum_carriers.get(n).is_some_and(|c| c.is_ok()))
    }

    fn sys_value(&self, t: &RTy) -> Result<SysValue, Why> {
        Ok(match t {
            RTy::Unit => SysValue::Unit,
            RTy::Scalar(s) => SysValue::Scalar(*s),
            RTy::Bool => SysValue::Bool,
            RTy::Prim(p, r) => return Err((*r, format!("`{p}` has no .vsys carrier"))),
            RTy::Named(n) => match self.enum_carriers.get(n) {
                Some(Ok(_)) => SysValue::Enum(n.clone()),
                Some(Err(r)) => return Err((r.reason, format!("enum `{n}`: {}", r.detail))),
                None => {
                    return Err((
                        RefusalReason::Aggregate,
                        format!("struct `{n}` by value is not modelled by .vsys"),
                    ));
                }
            },
            _ => {
                return Err((
                    RefusalReason::NotCAbi,
                    format!("`{}` is not a C ABI value", t.vesper()),
                ));
            }
        })
    }
}

fn discriminant(e: &Expr) -> Option<i128> {
    match e {
        Expr::Lit(l) => match &l.lit {
            syn::Lit::Int(i) => i.base10_parse::<i128>().ok(),
            _ => None,
        },
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => discriminant(&u.expr)?.checked_neg(),
        Expr::Paren(p) => discriminant(&p.expr),
        _ => None,
    }
}

fn analyze(krate: Crate, limits: Limits) -> Result<Migration, MigrateError> {
    let mut a = Analyzer {
        structs: BTreeMap::new(),
        enums: BTreeMap::new(),
        aliases: BTreeMap::new(),
        refused: BTreeMap::new(),
        enum_carriers: BTreeMap::new(),
    };
    let mut refusals = krate.notes.clone();
    let mut fns: Vec<(&syn::ItemFn, Option<String>)> = vec![];
    let mut impls: Vec<&syn::ItemImpl> = vec![];
    let mut count = 0usize;
    let dup = |n: &str| {
        MigrateError::new(
            ErrorKind::Duplicate,
            None,
            format!("public item `{n}` defined twice"),
        )
    };
    for m in &krate.modules {
        for item in &m.items {
            count += 1;
            if count > limits.items {
                return Err(MigrateError::new(
                    ErrorKind::Limit,
                    None,
                    "item budget exceeded",
                ));
            }
            if let Some((attrs, label)) = item_attrs(item)
                && conditional(attrs)
            {
                let public = match item {
                    Item::Struct(x) => is_pub(&x.vis),
                    Item::Enum(x) => is_pub(&x.vis),
                    Item::Type(x) => is_pub(&x.vis),
                    Item::Fn(x) => is_pub(&x.vis),
                    Item::Trait(x) => is_pub(&x.vis),
                    Item::Const(x) => is_pub(&x.vis),
                    Item::Static(x) => is_pub(&x.vis),
                    Item::Union(x) => is_pub(&x.vis),
                    Item::Impl(_) => true,
                    _ => false,
                };
                if public {
                    refusals.push(cfg_refusal(label));
                }
                continue;
            }
            match item {
                Item::Struct(s) if is_pub(&s.vis) => {
                    if a.structs.insert(s.ident.to_string(), s.clone()).is_some() {
                        return Err(dup(&s.ident.to_string()));
                    }
                }
                Item::Enum(e) if is_pub(&e.vis) => {
                    if a.enums.insert(e.ident.to_string(), e.clone()).is_some() {
                        return Err(dup(&e.ident.to_string()));
                    }
                }
                Item::Type(t) if is_pub(&t.vis) => {
                    if a.aliases.contains_key(&t.ident.to_string()) {
                        // Aliases are resolved by name without module scope,
                        // so an alias defined twice is refused, never merged.
                        a.refused.insert(
                            t.ident.to_string(),
                            (
                                RefusalReason::UnsupportedType,
                                "type alias defined in more than one module; resolution is not module-scoped".into(),
                            ),
                        );
                    } else if !t.generics.params.is_empty() {
                        a.refused.insert(
                            t.ident.to_string(),
                            (RefusalReason::Generic, "generic type alias".into()),
                        );
                    }
                    a.aliases.insert(t.ident.to_string(), (*t.ty).clone());
                }
                Item::Fn(f) if is_pub(&f.vis) => fns.push((f, None)),
                Item::Impl(i) if i.trait_.is_none() => impls.push(i),
                Item::Trait(t) if is_pub(&t.vis) => refusals.push(Refusal::new(
                    format!("trait {}", t.ident),
                    RefusalReason::UnsupportedItem,
                    "traits are lifted by the Pact bridge, not by vesper migrate",
                )),
                Item::Const(c) if is_pub(&c.vis) => refusals.push(Refusal::new(
                    format!("const {}", c.ident),
                    RefusalReason::UnsupportedItem,
                    "constants are not part of a migrated signature",
                )),
                Item::Static(s) if is_pub(&s.vis) => refusals.push(Refusal::new(
                    format!("static {}", s.ident),
                    RefusalReason::UnsupportedItem,
                    "global state is not lifted",
                )),
                Item::Union(u) if is_pub(&u.vis) => refusals.push(Refusal::new(
                    format!("union {}", u.ident),
                    RefusalReason::Union,
                    "unions have no FieldTree",
                )),
                _ => {}
            }
        }
    }
    // Structural refusals of types, to a fixpoint (a struct containing a
    // refused type is refused).
    for (name, s) in &a.structs {
        if s.generics.lifetimes().next().is_some() {
            a.refused.insert(
                name.clone(),
                (
                    RefusalReason::BorrowingType,
                    "struct has lifetime parameters, so it stores borrows".into(),
                ),
            );
        } else if !s.generics.params.is_empty() {
            a.refused.insert(
                name.clone(),
                (RefusalReason::Generic, "generic struct".into()),
            );
        } else if matches!(s.fields, Fields::Unnamed(_)) {
            a.refused.insert(
                name.clone(),
                (
                    RefusalReason::UnsupportedItem,
                    "tuple structs have no named FieldTree paths".into(),
                ),
            );
        }
    }
    for (name, e) in &a.enums {
        if !e.generics.params.is_empty() {
            a.refused.insert(
                name.clone(),
                (RefusalReason::Generic, "generic enum".into()),
            );
        } else if e.variants.iter().any(|v| !matches!(v.fields, Fields::Unit)) {
            a.refused.insert(
                name.clone(),
                (
                    RefusalReason::UnsupportedItem,
                    "data-carrying enums are not lifted".into(),
                ),
            );
        } else if e.variants.is_empty() {
            a.refused.insert(
                name.clone(),
                (RefusalReason::UnsupportedItem, "empty enum".into()),
            );
        }
    }
    loop {
        let mut newly = vec![];
        for (name, s) in &a.structs {
            if a.refused.contains_key(name) {
                continue;
            }
            for f in s.fields.iter() {
                let fname = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                match a.rty(&f.ty, Some(name), 0) {
                    Err((r, d)) => {
                        newly.push((name.clone(), (r, format!("field `{fname}`: {d}"))));
                        break;
                    }
                    Ok(ty) if ty.contains_borrow() => {
                        newly.push((
                            name.clone(),
                            (
                                RefusalReason::BorrowingType,
                                format!("field `{fname}` stores a reference or raw pointer"),
                            ),
                        ));
                        break;
                    }
                    Ok(_) => {}
                }
            }
        }
        if newly.is_empty() {
            break;
        }
        a.refused.extend(newly);
    }
    // Enums.
    let mut enums = vec![];
    for (name, e) in &a.enums {
        if a.refused.contains_key(name) {
            continue;
        }
        let item = format!("enum {name}");
        let mut next = 0i128;
        let mut variants = vec![];
        let mut bad = None;
        for v in &e.variants {
            let value = match &v.discriminant {
                Some((_, x)) => match discriminant(x) {
                    Some(d) => d,
                    None => {
                        bad = Some(format!(
                            "discriminant of `{}` is not an integer literal",
                            v.ident
                        ));
                        break;
                    }
                },
                None => next,
            };
            variants.push((v.ident.to_string(), value));
            next = value.saturating_add(1);
        }
        let reprs = repr(&e.attrs);
        let carrier = if let Some(d) = bad {
            Err(Refusal::new(&item, RefusalReason::UnsupportedType, d))
        } else {
            let unsigned = reprs.iter().find_map(|r| match r.as_str() {
                "u8" => Some(Scalar::U8),
                "u16" => Some(Scalar::U16),
                "u32" => Some(Scalar::U32),
                "u64" => Some(Scalar::U64),
                _ => None,
            });
            match unsigned {
                Some(s) => {
                    let max = if s.bits() == 64 {
                        u64::MAX as i128
                    } else {
                        (1i128 << s.bits()) - 1
                    };
                    if variants.iter().all(|(_, v)| (0..=max).contains(v)) {
                        Ok(s)
                    } else {
                        Err(Refusal::new(
                            &item,
                            RefusalReason::UnsupportedType,
                            "discriminant outside the carrier range",
                        ))
                    }
                }
                None if reprs.iter().any(|r| r.starts_with('i')) => Err(Refusal::new(
                    &item,
                    RefusalReason::ImplementationDefinedCarrier,
                    "signed #[repr] carriers are not .vsys enum carriers (they must be unsigned)",
                )),
                None => Err(Refusal::new(
                    &item,
                    RefusalReason::ImplementationDefinedCarrier,
                    "a Rust enum without #[repr(u8|u16|u32|u64)] has no stable ABI carrier",
                )),
            }
        };
        a.enum_carriers.insert(name.clone(), carrier.clone());
        enums.push(LiftedEnum {
            name: name.clone(),
            variants,
            carrier,
        });
    }
    // Structs.
    let mut structs = vec![];
    for (name, s) in &a.structs {
        if a.refused.contains_key(name) {
            continue;
        }
        let item = format!("struct {name}");
        let mut fields = vec![];
        let mut wire_fields = vec![];
        let reprs = repr(&s.attrs);
        let mut wire_err: Option<Refusal> = if reprs.iter().all(|r| r == "C") && !reprs.is_empty() {
            None
        } else if reprs.iter().any(|r| r == "C") {
            Some(Refusal::new(
                &item,
                RefusalReason::WireLayout,
                format!(
                    "#[repr({})] modifiers change size or alignment, which the wire declaration does not model",
                    reprs.join(", ")
                ),
            ))
        } else {
            Some(Refusal::new(
                &item,
                RefusalReason::WireLayout,
                "no #[repr(C)]: Rust struct layout is unspecified",
            ))
        };
        for f in s.fields.iter() {
            let fname = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
            let ty = a
                .rty(&f.ty, Some(name), 0)
                .expect("refused structs were filtered");
            let wire = match &ty {
                RTy::Scalar(sc) => {
                    let bits = u16::from(sc.bits());
                    Some(WireType::Integer(if sc.signed() {
                        Integer::signed(bits, Endian::Native)
                    } else {
                        Integer::unsigned(bits, Endian::Native)
                    }))
                }
                RTy::Bool => Some(WireType::Bool),
                RTy::Named(n) if a.structs.contains_key(n) => Some(WireType::Struct(n.clone())),
                RTy::Array(e, n) if matches!(**e, RTy::Scalar(Scalar::U8 | Scalar::I8)) => {
                    usize::try_from(*n).ok().map(WireType::ByteArray)
                }
                _ => None,
            };
            match wire {
                Some(w) => wire_fields.push(Field::new(fname.clone(), w)),
                None => {
                    wire_err.get_or_insert_with(|| {
                        Refusal::new(
                            &item,
                            RefusalReason::WireLayout,
                            format!("field `{fname}` ({}) has no wire layout", ty.vesper()),
                        )
                    });
                }
            }
            fields.push(LiftedField {
                name: fname,
                ty: ty.vesper(),
            });
        }
        let wire = match wire_err {
            Some(r) => Err(r),
            None => Ok(Declaration::new(name.clone(), wire_fields)),
        };
        structs.push(LiftedStruct {
            name: name.clone(),
            fields,
            wire,
        });
    }
    crate::synth::check_wire_layouts(&mut structs);
    for (name, (r, d)) in &a.refused {
        let kind = if a.enums.contains_key(name) {
            "enum"
        } else if a.structs.contains_key(name) {
            "struct"
        } else {
            "type"
        };
        refusals.push(Refusal::new(format!("{kind} {name}"), *r, d.clone()));
    }
    // Functions.
    let mut functions = vec![];
    let mut names = BTreeSet::new();
    for (f, _) in &fns {
        let abi_c = f
            .sig
            .abi
            .as_ref()
            .is_some_and(|abi| abi.name.as_ref().is_none_or(|n| n.value() == "C"));
        let name = f.sig.ident.to_string();
        let c_abi = if !abi_c {
            Err(Refusal::new(
                format!("fn {name}: vsys"),
                RefusalReason::NotCAbi,
                "not `extern \"C\"`; a C-ABI shim is needed before a .vsys entry exists",
            ))
        } else if !no_mangle(&f.attrs) {
            Err(Refusal::new(
                format!("fn {name}: vsys"),
                RefusalReason::NotCAbi,
                "missing #[no_mangle]; the symbol name is mangled",
            ))
        } else {
            Ok(())
        };
        if !names.insert(name.clone()) {
            return Err(dup(&name));
        }
        match a.lift_fn(&name, &f.sig, &f.attrs, Some(&f.block), None, c_abi, limits) {
            Ok(l) => functions.push(l),
            Err(r) => refusals.push(r),
        }
    }
    for i in impls {
        let Type::Path(tp) = &*i.self_ty else {
            continue;
        };
        let Some(seg) = tp.path.segments.last() else {
            continue;
        };
        let ty_name = seg.ident.to_string();
        let methods: Vec<&syn::ImplItemFn> = i
            .items
            .iter()
            .filter_map(|x| match x {
                ImplItem::Fn(f) if is_pub(&f.vis) => Some(f),
                _ => None,
            })
            .collect();
        if methods.is_empty()
            || !a.structs.contains_key(&ty_name) && !a.enums.contains_key(&ty_name)
        {
            continue;
        }
        for m in methods {
            let display = format!("{ty_name}::{}", m.sig.ident);
            if conditional(&m.attrs) {
                refusals.push(cfg_refusal(format!("fn {display}")));
                continue;
            }
            if let Some((r, d)) = a.refused.get(&ty_name) {
                refusals.push(Refusal::new(
                    format!("fn {display}"),
                    *r,
                    format!("receiver type `{ty_name}` was refused: {d}"),
                ));
                continue;
            }
            if !i.generics.params.is_empty() {
                refusals.push(Refusal::new(
                    format!("fn {display}"),
                    RefusalReason::Generic,
                    "generic impl block",
                ));
                continue;
            }
            if !names.insert(display.clone()) {
                return Err(dup(&display));
            }
            let c_abi = Err(Refusal::new(
                format!("fn {display}: vsys"),
                RefusalReason::NotCAbi,
                "methods are not C-ABI symbols",
            ));
            match a.lift_fn(
                &display,
                &m.sig,
                &m.attrs,
                Some(&m.block),
                Some(&ty_name),
                c_abi,
                limits,
            ) {
                Ok(l) => functions.push(l),
                Err(r) => refusals.push(r),
            }
        }
    }
    functions.sort_by(|x, y| x.name.cmp(&y.name));
    refusals.sort();
    let mut m = Migration {
        origin: Origin::RustCrate { name: krate.name },
        functions,
        structs,
        enums,
        refusals,
    };
    crate::synth::check_pointer_pointees(&mut m);
    Ok(m)
}
