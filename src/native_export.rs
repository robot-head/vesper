//! `vesper build --lib`: a native library that Rust (or C) code links
//! (port wave P0 lane 6, #878; survey `docs/maintenance/vesper-port-survey.md`).
//!
//! Every `pub fn` of the module becomes one C-ABI symbol
//! `vesper_<library>_<function>` in a single object. The object needs no
//! Vesper runtime library: like executables, it carries the runtime pieces it
//! uses as local functions and imports only libc.
//!
//! # ABI (`vesper.native.export-abi/1`)
//!
//! The exported symbol is the function's native code itself, with the host C
//! calling convention of the target (System V, AAPCS64/Apple, or Windows x64).
//! Values use the eight-byte-word layouts of [`crate::layout`]: every scalar
//! is one word, products are their fields in order, and enums are a tag word
//! (the variant index) followed by the variant's fields.
//!
//! * Scalars (`u8 u32 u64 i64 bool Unit`) are passed and returned as one
//!   64-bit integer, `f64` as a double.
//! * An aggregate parameter (tuple, struct, enum) is passed as a pointer to
//!   its words. The callee may overwrite them (a `sink` parameter's storage
//!   can be reused), so callers pass a scratch copy.
//! * An aggregate result is written through a pointer to its words that is
//!   passed before every other parameter; the integer return value is then
//!   meaningless.
//! * A function whose row is `Fail[E1], .., Fail[En]` takes one more pointer
//!   after its parameters: the failure record. Its first word is the status
//!   word, which the caller sets to 0. On return, 0 means success and `k`
//!   means the function raised the `k`-th `Fail` of its row, whose error value
//!   is in the words after the status word. The result is valid only when the
//!   status is 0.
//!
//! Generic functions, `inout` parameters, effects other than `Fail`
//! (capabilities, foreign libraries, user effects), and types without a fixed
//! word layout (strings, closures, region handles, boxes, sequences, affine
//! or linear data) are refused with an [`ExportError`]. Sequences and strings
//! follow once their source types exist (P0 lanes 3 and 7).
//!
//! The machine-readable [`LibraryInterface`] describes every symbol; the
//! Rust binding generator ([`rust_binding`]) turns it into `extern "C"`
//! declarations plus safe wrappers built on the `vesper-port-abi` crate.
use super::*;
use crate::layout::{Layout, LayoutKind};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Version of the export ABI and of the [`LibraryInterface`] schema.
pub const EXPORT_ABI: &str = "vesper.native.export-abi/1";

/// Builtin types whose Rust counterparts live in `vesper-port-abi`, with
/// the variant order that crate hard-codes.
const BUILTIN_TYPES: [(&str, &[&str]); 4] = [
    ("Option", &["None", "Some"]),
    ("Result", &["Ok", "Err"]),
    ("ArithOverflow", &["ArithOverflow"]),
    ("RuntimeError", &["DivisionByZero", "IndexOutOfBounds"]),
];

/// Why a library cannot be exported. Every refusal names the function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// The library name is not a lowercase identifier.
    LibraryName(String),
    /// The module has no `pub fn`.
    NoExports,
    /// `pub fn f[T]`: generic functions have no single native instance.
    Generic { function: String },
    /// An effect other than unlabeled `Fail[E]`, or an open row.
    Effect { function: String, effect: String },
    /// `inout` parameters have no export representation yet.
    Inout { function: String, param: String },
    /// A parameter or result type without a fixed word layout.
    Type {
        function: String,
        ty: String,
        reason: String,
    },
    /// The source does not parse or check, or code generation failed.
    Compile(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryName(n) => write!(
                f,
                "library name '{n}' must match [a-z][a-z0-9_]* (it prefixes every symbol)"
            ),
            Self::NoExports => write!(f, "library has no `pub fn` to export"),
            Self::Generic { function } => write!(
                f,
                "cannot export generic function '{function}'; export a monomorphic wrapper"
            ),
            Self::Effect { function, effect } => write!(
                f,
                "cannot export '{function}': effect {effect} is not exportable (only Fail[E] is)"
            ),
            Self::Inout { function, param } => write!(
                f,
                "cannot export '{function}': inout parameter '{param}' is not exportable yet"
            ),
            Self::Type {
                function,
                ty,
                reason,
            } => write!(f, "cannot export '{function}': type {ty} {reason}"),
            Self::Compile(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// A type at the export boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbiType {
    /// `u8 u32 u64 i64 f64 bool Unit`.
    Scalar {
        name: String,
    },
    Tuple {
        items: Vec<AbiType>,
    },
    /// A user type from [`LibraryInterface::types`] or a builtin
    /// (`Option`, `Result`, `ArithOverflow`, `RuntimeError`).
    Named {
        name: String,
        args: Vec<AbiType>,
    },
}

/// A user type declared by the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDecl {
    pub name: String,
    #[serde(rename = "enum")]
    pub is_enum: bool,
    /// Size in eight-byte words.
    pub words: u32,
    /// A struct has one variant named after the type.
    pub variants: Vec<VariantDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantDecl {
    pub name: String,
    /// Positional fields are named `0`, `1`, ...
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDecl {
    pub name: String,
    pub ty: AbiType,
    /// Word offset inside the value (after the tag word for enums).
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportParam {
    pub name: String,
    pub ty: AbiType,
    /// Passed as a pointer to its words.
    pub pointer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFunction {
    pub name: String,
    pub symbol: String,
    pub params: Vec<ExportParam>,
    pub result: AbiType,
    /// Size of the result in words.
    pub result_words: u32,
    /// The result is written through a leading pointer parameter.
    pub result_pointer: bool,
    /// The `Fail` error types of the row, in status order (status `k` is
    /// `errors[k - 1]`).
    pub errors: Vec<AbiType>,
    /// Words of the failure record (status word included), when `errors` is
    /// not empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_record_words: Option<u32>,
}

/// The machine-readable description of a `vesper build --lib` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryInterface {
    pub abi: String,
    pub library: String,
    pub types: BTreeMap<String, TypeDecl>,
    pub functions: Vec<ExportedFunction>,
}

/// Whether the declaration of `f` in `source` is `pub fn`. The AST does not
/// record function visibility yet (P0 lane 1 adds it), so this reads the
/// declaration's leading tokens, which start at `f.span`.
fn is_public(source: &str, f: &Function) -> bool {
    let Some(text) = source.get(f.span.start..f.span.end) else {
        return false;
    };
    let text = text.trim_start();
    let text = text
        .strip_prefix("@fip")
        .map(str::trim_start)
        .unwrap_or(text);
    text.strip_prefix("pub")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('@'))
}

fn valid_library_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The exported functions of a checked module, in name order. Every `pub fn`
/// must be exportable: all refusals are returned together.
pub fn plan(
    world: &World,
    source: &str,
    library: &str,
) -> Result<LibraryInterface, Vec<ExportError>> {
    if !valid_library_name(library) {
        return Err(vec![ExportError::LibraryName(library.into())]);
    }
    let mut errors = vec![];
    let mut types = BTreeMap::new();
    let mut functions = vec![];
    for (name, f) in &world.functions {
        if name.contains("::")
            || world.elaboration.generated.contains_key(name)
            || !is_public(source, f)
        {
            continue;
        }
        match export_function(world, library, f, &mut types) {
            Ok(e) => functions.push(e),
            Err(e) => errors.push(e),
        }
    }
    if functions.is_empty() && errors.is_empty() {
        errors.push(ExportError::NoExports);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(LibraryInterface {
        abi: EXPORT_ABI.into(),
        library: library.into(),
        types,
        functions,
    })
}

fn export_function(
    world: &World,
    library: &str,
    f: &Function,
    types: &mut BTreeMap<String, TypeDecl>,
) -> Result<ExportedFunction, ExportError> {
    let function = f.name.clone();
    if !f.generics.is_empty() {
        return Err(ExportError::Generic { function });
    }
    if let Some(tail) = &f.effects.tail {
        return Err(ExportError::Effect {
            function,
            effect: format!("row variable {tail}"),
        });
    }
    let mut errors = vec![];
    for e in &f.effects.instances {
        if e.name == "Fail" && e.label.is_none() && e.args.len() == 1 {
            errors.push(abi_type(world, &function, &e.args[0], types)?);
        } else {
            return Err(ExportError::Effect {
                function,
                effect: e.name.clone(),
            });
        }
    }
    if crate::fip::uses_caller_slot(world, f) {
        return Err(ExportError::Type {
            function,
            ty: f.result.to_string(),
            reason: "is returned through a caller Box slot".into(),
        });
    }
    let mut params = vec![];
    for p in &f.params {
        if p.pass == Pass::Inout {
            return Err(ExportError::Inout {
                function,
                param: p.name.clone(),
            });
        }
        let ty = abi_type(world, &function, &p.ty, types)?;
        params.push(ExportParam {
            name: p.name.clone(),
            pointer: layout(world, &function, &p.ty)?.aggregate(),
            ty,
        });
    }
    let result = abi_type(world, &function, &f.result, types)?;
    let result_layout = layout(world, &function, &f.result)?;
    let fail_record_words = if errors.is_empty() {
        None
    } else {
        Some(fail_record_size(world, f).map_err(ExportError::Compile)? / 8)
    };
    Ok(ExportedFunction {
        symbol: format!("vesper_{library}_{}", f.name),
        name: function,
        params,
        result,
        result_words: result_layout.size / 8,
        result_pointer: result_layout.aggregate(),
        errors,
        fail_record_words,
    })
}

fn layout(world: &World, function: &str, ty: &Type) -> Result<Layout, ExportError> {
    Layout::of(world, ty).map_err(|reason| ExportError::Type {
        function: function.into(),
        ty: ty.to_string(),
        reason: format!("has no layout: {reason}"),
    })
}

fn abi_type(
    world: &World,
    function: &str,
    ty: &Type,
    types: &mut BTreeMap<String, TypeDecl>,
) -> Result<AbiType, ExportError> {
    let refuse = |reason: &str| ExportError::Type {
        function: function.into(),
        ty: ty.to_string(),
        reason: reason.into(),
    };
    match ty {
        Type::Tuple(items) if items.is_empty() => Ok(AbiType::Scalar {
            name: "Unit".into(),
        }),
        Type::Tuple(items) => Ok(AbiType::Tuple {
            items: items
                .iter()
                .map(|t| abi_type(world, function, t, types))
                .collect::<Result<_, _>>()?,
        }),
        Type::Named(n, args)
            if args.is_empty()
                && matches!(
                    n.as_str(),
                    "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit"
                ) =>
        {
            Ok(AbiType::Scalar { name: n.clone() })
        }
        Type::Named(n, args) => {
            let Some(adt) = world.adts.get(n) else {
                return Err(refuse("has no fixed word layout"));
            };
            if adt.generics.len() != args.len() {
                return Err(refuse("has unresolved type arguments"));
            }
            if adt.mode != Mode::Copy {
                return Err(refuse("is not a copy type"));
            }
            if adt.tile_width.is_some() {
                return Err(refuse("is a tiled struct"));
            }
            if let Some((_, order)) = BUILTIN_TYPES.iter().find(|(b, _)| b == n) {
                let names = adt
                    .variants
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>();
                if names != *order {
                    return Err(refuse("has a builtin variant order this ABI does not know"));
                }
                return Ok(AbiType::Named {
                    name: n.clone(),
                    args: args
                        .iter()
                        .map(|t| abi_type(world, function, t, types))
                        .collect::<Result<_, _>>()?,
                });
            }
            if !args.is_empty() {
                return Err(refuse(
                    "is a generic user type; export a monomorphic type instead",
                ));
            }
            if !types.contains_key(n) {
                let l = layout(world, function, ty)?;
                // Insert before recursing so a recursive type cannot loop
                // (its layout above already failed for inline recursion).
                types.insert(
                    n.clone(),
                    TypeDecl {
                        name: n.clone(),
                        is_enum: adt.is_enum,
                        words: l.size / 8,
                        variants: vec![],
                    },
                );
                let fields_of = |fields: &[crate::layout::LayoutField],
                                 types: &mut BTreeMap<String, TypeDecl>|
                 -> Result<Vec<FieldDecl>, ExportError> {
                    fields
                        .iter()
                        .map(|f| {
                            Ok(FieldDecl {
                                name: f.name.clone(),
                                ty: abi_type(world, function, &f.ty, types)?,
                                offset: f.offset as u32 / 8,
                            })
                        })
                        .collect()
                };
                let variants = match &l.kind {
                    LayoutKind::Product(fields) => vec![VariantDecl {
                        name: n.clone(),
                        fields: fields_of(fields, types)?,
                    }],
                    LayoutKind::Enum(variants) => variants
                        .iter()
                        .map(|v| {
                            Ok(VariantDecl {
                                name: v.name.clone(),
                                fields: fields_of(&v.fields, types)?,
                            })
                        })
                        .collect::<Result<_, ExportError>>()?,
                    LayoutKind::Scalar => return Err(refuse("has no word layout")),
                };
                types.get_mut(n).expect("inserted above").variants = variants;
            }
            Ok(AbiType::Named {
                name: n.clone(),
                args: vec![],
            })
        }
        _ => Err(refuse("has no fixed word layout")),
    }
}

/// Compile every function of `interface` into one object for `target`
/// (the host when `None`). Exported functions keep their export symbols for
/// calls between them; everything else they reach is local.
pub fn object(
    world: &World,
    interface: &LibraryInterface,
    target: Option<&str>,
    optimize: bool,
) -> Result<Vec<u8>, String> {
    let exports = interface
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.symbol.clone()))
        .collect::<BTreeMap<_, _>>();
    let isa = object_isa_for(world, target)?;
    if windows::is_windows(isa.triple()) {
        return windows::emit_with(isa, "vesper", |m| {
            compile_exports(m, world, &exports, optimize)
        })
        .map(|(bytes, _)| bytes);
    }
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    compile_exports(&mut m, world, &exports, optimize)?;
    m.finish().emit().map_err(|e| e.to_string())
}

fn compile_exports<M: Module>(
    m: &mut M,
    world: &World,
    exports: &BTreeMap<String, String>,
    optimize: bool,
) -> Result<String, String> {
    let mut all = InstanceMap::new();
    for name in exports.keys() {
        let reached = instances(world, name)?;
        if !reached.contains_key(name) {
            return Err(format!(
                "'{name}' is not compiled as a standalone native function"
            ));
        }
        all.extend(reached);
    }
    if all.len() > 1024 {
        return Err("native monomorphization exceeds 1024 function instances".into());
    }
    let mut program = crate::ir::lower_world(world);
    if optimize {
        crate::ir_opt::optimize(world, &mut program)?;
    } else {
        crate::ir_opt::fip_reuse_pass(world, &mut program)?;
    }
    crate::fip::elaborate_non_fip_drops(world, &mut program);
    let mut ids = BTreeMap::new();
    for (i, (key, (name, vars))) in all.iter().enumerate() {
        let f = concrete(world, name, vars)?;
        let (symbol, linkage) = match exports.get(key) {
            Some(symbol) => (symbol.clone(), Linkage::Export),
            None => (format!("vesper_function_{i}"), Linkage::Local),
        };
        let id = m
            .declare_function(&symbol, linkage, &signature(m, world, &f)?)
            .map_err(|e| e.to_string())?;
        ids.insert(key.clone(), id);
    }
    define_instances(m, world, &all, &ids, &program, DefinitionLinkage::Object)
}

/// A compiled library: its object bytes and interface.
#[derive(Debug, Clone)]
pub struct Library {
    pub object: Vec<u8>,
    pub interface: LibraryInterface,
}

/// Options of [`compile_source`].
#[derive(Debug, Clone)]
pub struct LibraryOptions<'a> {
    /// Prefix of every symbol (`vesper_<library>_<fn>`).
    pub library: &'a str,
    /// Target triple; the host when `None`.
    pub target: Option<&'a str>,
    /// Equality saturation, as `vesper build` without `--no-opt`.
    pub optimize: bool,
}

/// Parse, check, optimize and compile `source` exactly as
/// `vesper build --lib` does. `path` only labels diagnostics.
pub fn compile_source(
    path: &str,
    source: &str,
    options: &LibraryOptions<'_>,
) -> Result<Library, ExportError> {
    let first = |ds: &[crate::ast::Diagnostic], what: &str| {
        let shown = ds
            .iter()
            .take(8)
            .map(|d| format!("{path}:{}: {}: {}", d.span.start, d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n");
        ExportError::Compile(format!("{} {what} error(s)\n{shown}", ds.len()))
    };
    let mut parsed = crate::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(first(&parsed.diagnostics, "syntax"));
    }
    let mut checked = crate::check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Err(first(&checked.diagnostics, "semantic"));
    }
    if options.optimize {
        crate::optimize::optimize(&mut parsed.module, &checked.expression_types);
        checked = crate::check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(first(&checked.diagnostics, "optimizer validation"));
        }
    }
    compile_world(&checked.world, source, options)
}

/// [`compile_source`] for an already checked (and optimized) module.
pub fn compile_world(
    world: &World,
    source: &str,
    options: &LibraryOptions<'_>,
) -> Result<Library, ExportError> {
    let interface = plan(world, source, options.library).map_err(|errors| {
        ExportError::Compile(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    let object = object(world, &interface, options.target, options.optimize)
        .map_err(ExportError::Compile)?;
    Ok(Library { object, interface })
}

/// Options of [`rust_binding`].
#[derive(Debug, Clone, Default)]
pub struct BindingOptions {
    /// Path of an existing Rust module that already defines the library's
    /// user types with the same names, fields and variants (for example
    /// `crate`): the binding then implements `Abi` for them instead of
    /// declaring its own, so a port forwards to the crate's own types. A
    /// function with several `Fail` types then returns the crate's
    /// `<path>::<CamelName>Error`, whose variants are named after the error
    /// types.
    pub types_path: Option<String>,
}

const RUST_KEYWORDS: [&str; 39] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

fn ident(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.into()
    }
}

fn camel(name: &str) -> String {
    name.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn rust_type(ty: &AbiType, types_path: &str) -> String {
    match ty {
        AbiType::Scalar { name } => match name.as_str() {
            "Unit" => "()".into(),
            other => other.into(),
        },
        AbiType::Tuple { items } if items.len() == 1 => {
            format!("({},)", rust_type(&items[0], types_path))
        }
        AbiType::Tuple { items } => format!(
            "({})",
            items
                .iter()
                .map(|t| rust_type(t, types_path))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AbiType::Named { name, args } => match name.as_str() {
            "Option" | "Result" => format!(
                "{name}<{}>",
                args.iter()
                    .map(|t| rust_type(t, types_path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "ArithOverflow" | "RuntimeError" => format!("vesper_port_abi::{name}"),
            _ => format!("{types_path}{name}"),
        },
    }
}

/// The Rust binding module of `interface`: `extern "C"` declarations of
/// every symbol plus a safe wrapper per function that marshals words with
/// `vesper_port_abi::Abi` and maps the failure status to `Result`. The text
/// is deterministic, so a port can commit it and check it for drift.
pub fn rust_binding(interface: &LibraryInterface, options: &BindingOptions) -> String {
    use std::fmt::Write as _;
    let types_path = match &options.types_path {
        Some(p) => format!("{p}::"),
        None => String::new(),
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// @generated by `vesper build --lib` ({}) for library `{}`.\n// Do not edit: regenerate it from the Vesper source.\n",
        interface.abi, interface.library
    );
    out.push_str("#![allow(clippy::all, unused_imports, unused_mut, non_snake_case, dead_code)]\n\nuse vesper_port_abi::Abi;\n\n");
    // User types: declarations (unless they live at types_path) and Abi impls.
    for decl in interface.types.values() {
        let name = &decl.name;
        if options.types_path.is_none() {
            out.push_str("#[derive(Debug, Clone, Copy, PartialEq)]\n");
            if decl.is_enum {
                let _ = writeln!(out, "pub enum {name} {{");
                for v in &decl.variants {
                    let _ = writeln!(
                        out,
                        "    {}{},",
                        v.name,
                        variant_shape(v, &types_path, false)
                    );
                }
                out.push_str("}\n\n");
            } else {
                let v = &decl.variants[0];
                let shape = variant_shape(v, &types_path, true);
                let _ = writeln!(
                    out,
                    "pub struct {name}{shape}{}\n",
                    if positional(v) || v.fields.is_empty() {
                        ";"
                    } else {
                        ""
                    }
                );
            }
        }
        let full = format!("{types_path}{name}");
        let _ = writeln!(
            out,
            "impl Abi for {full} {{\n    const WORDS: usize = {};",
            decl.words
        );
        out.push_str("    fn put(&self, out: &mut [u64]) {\n");
        if decl.is_enum {
            out.push_str("        match self {\n");
            for (tag, v) in decl.variants.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "            {full}::{}{} => {{\n                out[0] = {tag};",
                    v.name,
                    bind_pattern(v)
                );
                for (i, f) in v.fields.iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "                Abi::put(f{i}, &mut out[{}..]);",
                        f.offset
                    );
                }
                out.push_str("            }\n");
            }
            out.push_str("        }\n    }\n");
        } else {
            for f in &decl.variants[0].fields {
                let _ = writeln!(
                    out,
                    "        Abi::put(&self.{}, &mut out[{}..]);",
                    field_access(&f.name),
                    f.offset
                );
            }
            out.push_str("    }\n");
        }
        out.push_str("    fn get(words: &[u64]) -> Self {\n");
        if decl.is_enum {
            out.push_str("        match words[0] {\n");
            for (tag, v) in decl.variants.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "            {tag} => {full}::{}{},",
                    v.name,
                    construct(v, &types_path)
                );
            }
            let _ = writeln!(
                out,
                "            tag => panic!(\"invalid {name} tag {{tag}} from Vesper\"),"
            );
            out.push_str("        }\n    }\n}\n\n");
        } else {
            let _ = writeln!(
                out,
                "        {full}{}\n    }}\n}}\n",
                construct(&decl.variants[0], &types_path)
            );
        }
    }
    // Compile-time cross-check of every boundary type's word count.
    let mut checked = BTreeSet::new();
    for f in &interface.functions {
        let _ = checked.insert((rust_type(&f.result, &types_path), f.result_words));
    }
    for decl in interface.types.values() {
        let _ = checked.insert((format!("{types_path}{}", decl.name), decl.words));
    }
    for (ty, words) in &checked {
        let _ = writeln!(
            out,
            "const _: () = assert!(<{ty} as Abi>::WORDS == {words});"
        );
    }
    out.push_str("\nunsafe extern \"C\" {\n");
    for f in &interface.functions {
        let mut params = vec![];
        if f.result_pointer {
            params.push("result: *mut u64".to_string());
        }
        for p in &f.params {
            params.push(format!("{}: {}", ident(&p.name), c_type(&p.ty, p.pointer)));
        }
        if f.fail_record_words.is_some() {
            params.push("fail: *mut u64".into());
        }
        let ret = if !f.result_pointer
            && matches!(&f.result, AbiType::Scalar { name } if name == "f64")
        {
            "f64"
        } else {
            "i64"
        };
        let _ = writeln!(out, "    fn {}({}) -> {ret};", f.symbol, params.join(", "));
    }
    out.push_str("}\n");
    for f in &interface.functions {
        out.push('\n');
        let value = rust_type(&f.result, &types_path);
        let error = match f.errors.as_slice() {
            [] => None,
            [one] => Some(rust_type(one, &types_path)),
            _ if options.types_path.is_some() => {
                Some(format!("{types_path}{}Error", camel(&f.name)))
            }
            many => {
                let enum_name = format!("{}Error", camel(&f.name));
                let _ = writeln!(
                    out,
                    "/// The errors `{}` can raise, in status order.\n#[derive(Debug, Clone, Copy, PartialEq)]\npub enum {enum_name} {{",
                    f.name
                );
                for e in many {
                    let _ = writeln!(
                        out,
                        "    {}({}),",
                        error_variant(e),
                        rust_type(e, &types_path)
                    );
                }
                out.push_str("}\n\n");
                Some(enum_name)
            }
        };
        let ret = match &error {
            Some(e) => format!("Result<{value}, {e}>"),
            None => value.clone(),
        };
        let params = f
            .params
            .iter()
            .map(|p| format!("{}: {}", ident(&p.name), rust_type(&p.ty, &types_path)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "/// Calls `{}` of the Vesper library `{}` (`{}`).",
            f.name, interface.library, f.symbol
        );
        let _ = writeln!(out, "pub fn {}({params}) -> {ret} {{", ident(&f.name));
        let mut args = vec![];
        if f.result_pointer {
            let _ = writeln!(out, "    let mut result = vec![0u64; {}];", f.result_words);
            args.push("result.as_mut_ptr()".to_string());
        }
        for p in &f.params {
            let n = ident(&p.name);
            if p.pointer {
                let _ = writeln!(
                    out,
                    "    let mut arg_{0} = vec![0u64; <{1} as Abi>::WORDS];\n    Abi::put(&{n}, &mut arg_{0});",
                    p.name,
                    rust_type(&p.ty, &types_path)
                );
                args.push(format!("arg_{}.as_mut_ptr()", p.name));
            } else {
                args.push(scalar_in(&n, &p.ty));
            }
        }
        if let Some(words) = f.fail_record_words {
            let _ = writeln!(out, "    let mut fail = vec![0u64; {words}];");
            args.push("fail.as_mut_ptr()".into());
        }
        let _ = writeln!(
            out,
            "    // SAFETY: `{}` has exactly this signature in the export ABI; every\n    // pointer is a live, writable buffer of the size the ABI requires.\n    {}unsafe {{ {}({}) }};",
            f.symbol,
            if f.result_pointer { "" } else { "let raw = " },
            f.symbol,
            args.join(", ")
        );
        let produce = if f.result_pointer {
            format!("<{value} as Abi>::get(&result)")
        } else {
            scalar_out("raw", &f.result)
        };
        match f.errors.as_slice() {
            [] => {
                let _ = writeln!(out, "    {produce}");
            }
            errors => {
                out.push_str("    match fail[0] {\n");
                let _ = writeln!(out, "        0 => Ok({produce}),");
                for (k, e) in errors.iter().enumerate() {
                    let decode = format!("<{} as Abi>::get(&fail[1..])", rust_type(e, &types_path));
                    if errors.len() == 1 {
                        let _ = writeln!(out, "        {} => Err({decode}),", k + 1);
                    } else {
                        let _ = writeln!(
                            out,
                            "        {} => Err({}::{}({decode})),",
                            k + 1,
                            error.as_deref().unwrap_or_default(),
                            error_variant(e)
                        );
                    }
                }
                let _ = writeln!(
                    out,
                    "        status => panic!(\"invalid failure status {{status}} from {}\"),",
                    f.symbol
                );
                out.push_str("    }\n");
            }
        }
        out.push_str("}\n");
    }
    out
}

fn error_variant(ty: &AbiType) -> String {
    match ty {
        AbiType::Named { name, args } if args.is_empty() => name.clone(),
        AbiType::Named { name, args } => format!(
            "{name}{}",
            args.iter().map(error_variant).collect::<String>()
        ),
        AbiType::Scalar { name } => camel(name),
        AbiType::Tuple { items } => format!(
            "Tuple{}",
            items.iter().map(error_variant).collect::<String>()
        ),
    }
}

fn positional(v: &VariantDecl) -> bool {
    !v.fields.is_empty()
        && v.fields
            .iter()
            .all(|f| f.name.chars().all(|c| c.is_ascii_digit()))
}

fn field_access(name: &str) -> String {
    ident(name)
}

fn variant_shape(v: &VariantDecl, types_path: &str, public_fields: bool) -> String {
    let vis = if public_fields { "pub " } else { "" };
    if v.fields.is_empty() {
        String::new()
    } else if positional(v) {
        format!(
            "({})",
            v.fields
                .iter()
                .map(|f| format!("{vis}{}", rust_type(&f.ty, types_path)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            " {{ {} }}",
            v.fields
                .iter()
                .map(|f| format!("{vis}{}: {}", ident(&f.name), rust_type(&f.ty, types_path)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn bind_pattern(v: &VariantDecl) -> String {
    if v.fields.is_empty() {
        String::new()
    } else if positional(v) {
        format!(
            "({})",
            (0..v.fields.len())
                .map(|i| format!("f{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            " {{ {} }}",
            v.fields
                .iter()
                .enumerate()
                .map(|(i, f)| format!("{}: f{i}", ident(&f.name)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn construct(v: &VariantDecl, _types_path: &str) -> String {
    if v.fields.is_empty() {
        String::new()
    } else if positional(v) {
        format!(
            "({})",
            v.fields
                .iter()
                .map(|f| format!("Abi::get(&words[{}..])", f.offset))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            " {{ {} }}",
            v.fields
                .iter()
                .map(|f| format!("{}: Abi::get(&words[{}..])", ident(&f.name), f.offset))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn c_type(ty: &AbiType, pointer: bool) -> &'static str {
    if pointer {
        "*mut u64"
    } else if matches!(ty, AbiType::Scalar { name } if name == "f64") {
        "f64"
    } else {
        "i64"
    }
}

fn scalar_in(name: &str, ty: &AbiType) -> String {
    match ty {
        AbiType::Scalar { name: s } if s == "f64" => name.into(),
        AbiType::Scalar { name: s } if s == "Unit" => {
            format!("{{ let () = {name}; 0 }}")
        }
        _ => format!("{name} as i64"),
    }
}

fn scalar_out(raw: &str, ty: &AbiType) -> String {
    match ty {
        AbiType::Scalar { name } => match name.as_str() {
            "f64" | "i64" => raw.into(),
            "bool" => format!("{raw} != 0"),
            "Unit" => format!("{{ let _ = {raw}; }}"),
            other => format!("{raw} as {other}"),
        },
        _ => raw.into(),
    }
}

/// The interface file text: pretty JSON with a trailing newline.
pub fn interface_json(interface: &LibraryInterface) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(interface).map_err(|e| e.to_string())?;
    json.push('\n');
    Ok(json)
}

/// `vesper build --lib`: write the object to `output`, and optionally the
/// interface JSON and the Rust binding. Returns the summary line.
pub fn build_command(
    world: &World,
    source: &str,
    options: &LibraryOptions<'_>,
    output: &std::path::Path,
    interface_path: Option<&std::path::Path>,
    binding: Option<(&std::path::Path, &BindingOptions)>,
) -> Result<String, String> {
    let library = compile_world(world, source, options).map_err(|e| e.to_string())?;
    std::fs::write(output, &library.object).map_err(|e| format!("{}: {e}", output.display()))?;
    if let Some(path) = interface_path {
        let json = interface_json(&library.interface)?;
        std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some((path, options)) = binding {
        std::fs::write(path, rust_binding(&library.interface, options))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(format!(
        "built {} ({} exported function(s), {EXPORT_ABI})",
        output.display(),
        library.interface.functions.len()
    ))
}
