//! `vesper build --lib`: a native library that Rust (or C) code links
//! (port wave P0 lane 6, #878; export ABI v2: port wave P1 lane 1, #898;
//! survey `docs/maintenance/vesper-port-survey.md`).
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
//! * Scalars (every integer kind of `vesper.arith.checked`, `bool`, `Unit`)
//!   are passed and returned as one 64-bit integer, sign-extended for signed
//!   kinds and zero-extended for unsigned ones; `f64` is a double and `f32`
//!   a float, passed and returned in float registers (#1087). Inside an
//!   aggregate or a sequence buffer an `f32` is the low half of its word
//!   (the high half is unspecified), so a `Slice[f32]` or `ChunkSeq[f32]`
//!   is copied at the boundary like other elements whose stride is not
//!   their Rust size.
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
//! # ABI v2 (`vesper.native.export-abi/2`)
//!
//! A library that uses any of the forms below is a v2 library; one that uses
//! none of them keeps the v1 interface and binding byte for byte.
//!
//! * `inout` parameters of every exportable type: a pointer to the value's
//!   words (one word for a scalar), updated in place.
//! * `ChunkSeq[T]` of a copy element type is one word, a pointer to the
//!   header `{len, cap, data}` with elements packed at
//!   [`crate::seq::elem_bytes`]. It crosses as a result (the caller owns it and
//!   frees it with `vesper_<lib>_seq_free`), as an `in` parameter (a header
//!   the caller lends), or as an `inout` parameter (a pointer to the handle
//!   word of a sequence made with `vesper_<lib>_seq_new`). A by-value
//!   (`sink`) parameter is a sequence the caller makes with
//!   `vesper_<lib>_seq_new` and gives away: the callee owns it and frees it
//!   when it drops it.
//! * `in`/`inout Slice[T]` of a copy element type is the view
//!   `{header, start, len}`, passed as a pointer to its three words.
//! * A non-copy struct or enum (affine, linear, or holding sequences or
//!   boxes) crosses as an *owned handle*: its words live in caller memory and
//!   own the Vesper allocations they point to. `vesper_<lib>_<Type>_drop`
//!   (exported for every non-linear handle type) frees them. `in` and
//!   `inout` handle parameters are borrowed; a by-value parameter consumes the
//!   handle. Fields may be copy values, sequences, `Box[T]`, other handle
//!   types and `Option` of those, so recursive enums cross too. A field may
//!   also be a `ChunkSeq` of non-copy elements (handles, strings, boxes) or
//!   a boxed trait object `Box[dyn Trait]` ([`AbiType::Object`], #1087):
//!   the binding shows only a sequence's length and whether a box slot is
//!   full, and the drop glue drops every element (a boxed object through
//!   its vtable). Such a sequence never crosses on its own.
//! * Monomorphic instances of generic functions named by `@export instance`
//!   directives or `--instance` (see [`directives`]).
//! * `Ffi[Lib]` rows of `.vsys` libraries named by `@export vsys`
//!   directives: the object imports the library's symbols, which the linking
//!   crate provides.
//!
//! Every v2 library exports `vesper_<lib>_seq_new(cap, stride)`,
//! `vesper_<lib>_seq_free(seq)` and `vesper_<lib>_live_allocations()` (the
//! number of live sequence and box allocations, for leak tests).
//!
//! Still refused, with an [`ExportError`] that names the function: generic
//! functions without an instance, effects other than `Fail` and imported
//! `.vsys` libraries, closures, region handles, slices of non-copy
//! elements, sequences of non-copy elements and boxes outside a handle
//! type, and `dyn` parameters.
//!
//! # ABI v3 (`vesper.native.export-abi/3`)
//!
//! A library whose source has the directive `// @export abi 3` is a v3
//! library. The object is the v2 object; v3 changes what the Rust side
//! passes. Every copy struct of the library gets a `#[repr(C)]` *mirror*
//! (`repr::<Type>` in the binding, [`Mirror`]): its fields are the Vesper
//! words in layout order, a plain `i64`/`u64`/`isize`/`usize`/`f64` for a
//! full-word scalar, `vesper_port_abi::Word<T>` for a narrower scalar, a
//! `bool`, `Unit` or a field-less enum (canonical word), a nested mirror for
//! a nested copy struct, and `vesper_port_abi::Words<T, N>` (converted on
//! access) for any other copy value. The binding asserts the mirror's size,
//! alignment and every field offset at compile time.
//!
//! * An `in` or `inout` parameter of a mirrored struct is a pointer to the
//!   caller's mirror (`&T`/`&mut T`), with no conversion and no scratch
//!   copy. Vesper code never writes an `in` aggregate; a `sink` one gets a
//!   copy of the mirror, as its storage may be reused. An `in` handle is
//!   lent the same way, as a pointer to the caller's own handle words
//!   (#1087); `inout` handles already were.
//! * `in`/`inout Slice[T]` of a mirrored struct is the view over the
//!   caller's `&[T]`/`&mut [T]`, with no heap copy.
//! * A mirrored struct result is written straight into a mirror.
//! * Copy enums and tuples have no fixed Rust layout (payload validity,
//!   tuple layout), so they keep the v2 conversion; the interface says so
//!   ([`Mirror::Converted`]).
//! * With `// @export abi 3 options` (#991), an `Option[T]`/`Result[T, E]`
//!   of copy payloads is lent the same way as a mirrored struct, as a
//!   `vesper_port_abi::OptionMirror`/`ResultMirror`: the tag word followed
//!   by the payload's cell (its mirror, a word, a `Word` or a `Words`), so
//!   any bits are a value and no conversion runs. The interface marks it
//!   `"option_mirrors": true`. Struct fields of those types stay `Words`.
//!
//! Directives come from the root package's root module (`src/lib.ves`),
//! wherever it sorts among the package's files (`src/main.rs`).
//!
//! The machine-readable [`LibraryInterface`] describes every symbol; the
//! Rust binding generator ([`rust_binding`]) turns it into `extern "C"`
//! declarations plus safe wrappers built on the `vesper-port-abi` crate.
use super::*;
use crate::layout::{Layout, LayoutKind};
use serde::{Deserialize, Serialize};
use std::fmt;

#[path = "native_export/binding.rs"]
mod binding;
#[path = "native_export/directives.rs"]
pub mod directives;
#[path = "native_export/glue.rs"]
mod glue;
pub use binding::{BindingOptions, rust_binding};
pub use directives::{Directives, Instance, VsysImport};

/// Version of the export ABI and of the [`LibraryInterface`] schema that a
/// library using only the v1 forms carries.
pub const EXPORT_ABI: &str = "vesper.native.export-abi/1";
/// Version of a library that uses any v2 form.
pub const EXPORT_ABI_V2: &str = "vesper.native.export-abi/2";
/// Version of a library that opts into in-place mirrors
/// (`// @export abi 3`).
pub const EXPORT_ABI_V3: &str = "vesper.native.export-abi/3";

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
    /// A parameter or result type without a fixed word layout.
    Type {
        function: String,
        ty: String,
        reason: String,
    },
    /// An `@export instance` directive or `--instance` that cannot be
    /// exported.
    Instance { instance: String, reason: String },
    /// A malformed `@export` directive.
    Directive(String),
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
                "cannot export generic function '{function}'; name an instance with `// @export instance {function}[..]` or --instance"
            ),
            Self::Effect { function, effect } => write!(
                f,
                "cannot export '{function}': effect {effect} is not exportable (only Fail[E] and imported .vsys libraries are)"
            ),
            Self::Type {
                function,
                ty,
                reason,
            } => write!(f, "cannot export '{function}': type {ty} {reason}"),
            Self::Instance { instance, reason } => {
                write!(f, "cannot export instance '{instance}': {reason}")
            }
            Self::Directive(e) => write!(f, "{e}"),
            Self::Compile(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// A type at the export boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbiType {
    /// An integer kind (`u8`..`u64`, `usize`, `i8`..`i64`, `isize`), `f64`,
    /// `bool` or `Unit`.
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
    /// v2: `ChunkSeq[T]`, one word (a header pointer).
    Seq {
        elem: Box<AbiType>,
    },
    /// v2: `Slice[T]`, the three-word view `{header, start, len}`.
    Slice {
        elem: Box<AbiType>,
    },
    /// v2: `Box[T]`, one word (a pointer to `T`'s words); only inside a
    /// handle type.
    Box {
        inner: Box<AbiType>,
    },
    /// v2: `String` (`owned`) or the `Str` view, the three-word value
    /// `{len, cap, data}` of valid UTF-8. Capacity 0 marks bytes the value
    /// does not own (a literal, a view, or bytes Rust lent).
    Str {
        owned: bool,
    },
    /// v2: a trait object `dyn Trait[args]` (#1087), only as the pointee
    /// of a `Box` inside a handle type. The box word is the address of the
    /// block `{vtable, value}` (manuscript §7.3); Rust never reads it, and
    /// the handle's drop glue drops the value through the vtable.
    Object {
        name: String,
    },
}

fn is_false(v: &bool) -> bool {
    !*v
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
    /// v2: a non-copy type, which crosses as an owned handle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub handle: bool,
    /// v2: a linear handle; only a consuming call releases it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub linear: bool,
    /// v2: the exported drop glue of a non-linear handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_symbol: Option<String>,
    /// v2: the value owns a sequence, directly or through its fields, so a
    /// consuming call is refused (native code does not free the sequences
    /// inside a dropped handle yet).
    #[serde(default, skip_serializing_if = "is_false")]
    pub owns_sequence: bool,
    /// v3: how a copy type is held on the Rust side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<Mirror>,
}

/// v3: the Rust storage of a copy type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mirror {
    /// A `#[repr(C)]` mirror in the Vesper layout, lent in place.
    ReprC,
    /// No fixed Rust layout matches; values are converted as in v2.
    Converted { reason: String },
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
    /// v2: `in`, `inout` or `sink`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<String>,
    /// v3: the pointer is the caller's mirror storage (a mirrored struct,
    /// or the elements of a slice of one), lent without a copy.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_place: bool,
}

impl ExportParam {
    /// The parameter's passing mode (`in` when the interface predates v2).
    pub fn pass(&self) -> &str {
        self.pass.as_deref().unwrap_or("in")
    }
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
    /// v2: the generic function and type arguments this instance compiles,
    /// as `f[T1, T2]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_of: Option<String>,
    /// v2: the imported `.vsys` libraries the row names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<String>,
    /// v3: the result is written straight into a mirror.
    #[serde(default, skip_serializing_if = "is_false")]
    pub result_in_place: bool,
}

/// v2: the runtime entry points every v2 library exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSymbols {
    /// `seq_new(capacity, stride) -> seq`: an empty sequence.
    pub seq_new: String,
    /// `seq_free(seq)`: free a sequence the caller owns.
    pub seq_free: String,
    /// `live_allocations() -> i64`: live sequence and box allocations.
    pub live_allocations: String,
}

/// v2: an imported `.vsys` library whose symbols the linking crate provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignLibrary {
    pub name: String,
    /// `direct` or `isolated(mpk)`.
    pub handler: String,
    /// The C symbols of the library's functions.
    pub symbols: Vec<String>,
    /// The Rust module that defines the symbols, which the binding
    /// references so that every linker keeps them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_path: Option<String>,
}

/// The machine-readable description of a `vesper build --lib` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryInterface {
    pub abi: String,
    pub library: String,
    pub types: BTreeMap<String, TypeDecl>,
    pub functions: Vec<ExportedFunction>,
    /// v2: the runtime entry points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeSymbols>,
    /// v2: imported `.vsys` libraries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignLibrary>,
    /// v3 with `// @export abi 3 options`: `Option`/`Result` values of copy
    /// payloads cross as `OptionMirror`/`ResultMirror` (#991).
    #[serde(default, skip_serializing_if = "is_false")]
    pub option_mirrors: bool,
}

impl LibraryInterface {
    /// Whether the library uses export ABI v2 or later (v3 objects are v2
    /// objects).
    pub fn is_v2(&self) -> bool {
        self.abi == EXPORT_ABI_V2 || self.is_v3()
    }

    /// Whether the library uses export ABI v3 (in-place mirrors).
    pub fn is_v3(&self) -> bool {
        self.abi == EXPORT_ABI_V3
    }

    /// Whether `ty` is a copy struct with a `#[repr(C)]` mirror.
    pub fn mirrored(&self, ty: &AbiType) -> bool {
        mirrored(ty, &self.types)
    }

    /// Whether a value of `ty` crosses as a mirror: a mirrored struct, or
    /// (with `option_mirrors`) an `Option`/`Result` of copy payloads.
    pub fn lent(&self, ty: &AbiType) -> bool {
        lent(ty, &self.types, self.option_mirrors)
    }
}

fn valid_library_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The exported functions of a checked module, in name order. `public` is
/// the module's `pub` item names (`Module::public_items`); every one of them
/// that is a function must be exportable, and all refusals are returned
/// together.
pub fn plan(
    world: &World,
    public: &BTreeSet<String>,
    library: &str,
) -> Result<LibraryInterface, Vec<ExportError>> {
    plan_with(world, public, library, &Directives::default())
}

/// [`plan`] with generic instances and `.vsys` imports.
pub fn plan_with(
    world: &World,
    public: &BTreeSet<String>,
    library: &str,
    directives: &Directives,
) -> Result<LibraryInterface, Vec<ExportError>> {
    if !valid_library_name(library) {
        return Err(vec![ExportError::LibraryName(library.into())]);
    }
    let mut planner = Planner {
        world,
        library,
        types: BTreeMap::new(),
        v2: false,
        foreign: directives.vsys.iter().map(|v| v.library.clone()).collect(),
    };
    let mut errors = vec![];
    let mut functions = vec![];
    for (name, f) in &world.functions {
        if name.contains("::")
            || world.elaboration.generated.contains_key(name)
            || !public.contains(name)
        {
            continue;
        }
        if !f.generics.is_empty() && directives.instances.iter().any(|i| &i.function == name) {
            continue;
        }
        if let Some((g, tr)) = world
            .dyn_objects
            .params
            .get(name)
            .and_then(|p| p.iter().next())
        {
            // Second-class trait objects have no export ABI yet (§7.1).
            let param = f
                .params
                .iter()
                .find(|p| p.ty.name() == g)
                .map_or("?", |p| p.name.as_str());
            errors.push(ExportError::Type {
                function: name.clone(),
                ty: format!("dyn {tr}"),
                reason: format!(
                    "parameter '{param}' is a trait object, which the export ABI does not pass yet"
                ),
            });
            continue;
        }
        match planner.function(f, &f.name, None) {
            Ok(e) => functions.push(e),
            Err(e) => errors.push(e),
        }
    }
    // Instances are compiled as generated monomorphic wrappers
    // (`instance_wrappers`), which export like any `pub fn`.
    for f in &mut functions {
        if let Some(i) = directives.instances.iter().find(|i| i.alias() == f.name) {
            f.instance_of = Some(i.spelling());
        }
    }
    if functions.is_empty() && errors.is_empty() {
        errors.push(ExportError::NoExports);
    }
    let mut symbols = BTreeSet::new();
    for f in &functions {
        if !symbols.insert(f.symbol.clone()) {
            errors.push(ExportError::Instance {
                instance: f.instance_of.clone().unwrap_or_else(|| f.name.clone()),
                reason: format!("symbol {} is exported twice", f.symbol),
            });
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    functions.sort_by(|a, b| a.name.cmp(&b.name));
    planner.mark_owned_sequences();
    for f in &functions {
        for p in &f.params {
            if p.pass() == "sink"
                && let AbiType::Named { name, .. } = &p.ty
                && planner.types.get(name).is_some_and(|d| d.owns_sequence)
            {
                errors.push(ExportError::Type {
                    function: f.name.clone(),
                    ty: name.clone(),
                    reason: "is a handle that owns a sequence, taken by value; native code does not free the sequences inside a dropped handle yet, so take it `inout`".into(),
                });
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let v3 = directives.abi == Some(3);
    let options = v3 && directives.option_mirrors;
    let v2 = v3 || planner.v2 || !directives.vsys.is_empty() || !directives.instances.is_empty();
    if v3 {
        mark_mirrors(&mut planner.types);
        for f in &mut functions {
            for p in &mut f.params {
                p.in_place = match (&p.ty, p.pass()) {
                    (AbiType::Slice { elem }, _) => mirrored(elem, &planner.types),
                    // An `in` handle is lent as the caller's own words
                    // (#1087): Vesper never writes an `in` value (E0307).
                    (AbiType::Named { name, .. }, "in")
                        if planner.types.get(name).is_some_and(|d| d.handle) =>
                    {
                        true
                    }
                    (ty, "in" | "inout") => lent(ty, &planner.types, options),
                    _ => false,
                };
            }
            f.result_in_place = lent(&f.result, &planner.types, options);
        }
    }
    if v2 {
        for f in &mut functions {
            for p in &mut f.params {
                p.pass.get_or_insert_with(|| "in".into());
            }
        }
    }
    let foreign = directives
        .vsys
        .iter()
        .map(|v| ForeignLibrary {
            name: v.library.clone(),
            handler: v.handler.name().into(),
            symbols: v.symbols.clone(),
            rust_path: v.rust_path.clone(),
        })
        .collect();
    Ok(LibraryInterface {
        abi: if v3 {
            EXPORT_ABI_V3
        } else if v2 {
            EXPORT_ABI_V2
        } else {
            EXPORT_ABI
        }
        .into(),
        library: library.into(),
        types: planner.types,
        functions,
        runtime: v2.then(|| RuntimeSymbols {
            seq_new: format!("vesper_{library}_seq_new"),
            seq_free: format!("vesper_{library}_seq_free"),
            live_allocations: format!("vesper_{library}_live_allocations"),
        }),
        foreign,
        option_mirrors: options,
    })
}

struct Planner<'a> {
    world: &'a World,
    library: &'a str,
    types: BTreeMap<String, TypeDecl>,
    /// Whether any v2 form was used.
    v2: bool,
    /// Imported `.vsys` library names.
    foreign: Vec<String>,
}

const SEQ_OF_HANDLES: &str = "has non-copy elements (handles, boxes or strings); such a sequence crosses only inside a handle type, whose binding does not read its elements";

/// Where an [`AbiType`] occurs, for the position rules of the v2 forms.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Param(Pass),
    Result,
    Error,
}

impl Planner<'_> {
    fn function(
        &mut self,
        f: &Function,
        export_name: &str,
        instance_of: Option<String>,
    ) -> Result<ExportedFunction, ExportError> {
        let function = export_name.to_string();
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
        let mut foreign = vec![];
        let prefix = crate::ffi_lowering::effect_name("");
        for e in &f.effects.instances {
            if e.name == "Fail" && e.label.is_none() && e.args.len() == 1 {
                let t = self.ty(&function, &e.args[0])?;
                self.position(&function, &e.args[0], &t, Position::Error)?;
                errors.push(t);
            } else if let Some(library) = e.name.strip_prefix(prefix.as_str()).filter(|l| {
                e.label.is_none() && e.args.is_empty() && self.foreign.iter().any(|f| f == l)
            }) {
                foreign.push(library.to_string());
            } else {
                return Err(ExportError::Effect {
                    function,
                    effect: e.name.clone(),
                });
            }
        }
        if crate::fip::uses_caller_slot(self.world, f) {
            return Err(ExportError::Type {
                function,
                ty: f.result.to_string(),
                reason: "is returned through a caller Box slot".into(),
            });
        }
        let mut params = vec![];
        for p in &f.params {
            let ty = self.ty(&function, &p.ty)?;
            self.position(&function, &p.ty, &ty, Position::Param(p.pass))?;
            if p.pass == Pass::Inout {
                self.v2 = true;
            }
            params.push(ExportParam {
                name: p.name.clone(),
                pointer: layout(self.world, &function, &p.ty)?.aggregate() || p.pass == Pass::Inout,
                ty,
                pass: (p.pass != Pass::In).then(|| pass_name(p.pass).into()),
                in_place: false,
            });
        }
        let result = self.ty(&function, &f.result)?;
        self.position(&function, &f.result, &result, Position::Result)?;
        let result_layout = layout(self.world, &function, &f.result)?;
        let fail_record_words = if errors.is_empty() {
            None
        } else {
            Some(fail_record_size(self.world, f).map_err(ExportError::Compile)? / 8)
        };
        if !foreign.is_empty() {
            self.v2 = true;
        }
        Ok(ExportedFunction {
            symbol: format!("vesper_{}_{function}", self.library),
            name: function,
            params,
            result,
            result_words: result_layout.size / 8,
            result_pointer: result_layout.aggregate(),
            errors,
            fail_record_words,
            instance_of,
            foreign,
            result_in_place: false,
        })
    }

    /// Whether `ty` owns memory (a sequence, slice, box or handle).
    fn owning(&self, ty: &AbiType) -> bool {
        owning(ty, &self.types)
    }

    /// The position rules: where each owning form may cross.
    fn position(
        &self,
        function: &str,
        source: &Type,
        ty: &AbiType,
        position: Position,
    ) -> Result<(), ExportError> {
        let refuse = |reason: &str| ExportError::Type {
            function: function.into(),
            ty: source.to_string(),
            reason: reason.into(),
        };
        if !self.owning(ty) {
            return Ok(());
        }
        match position {
            Position::Error => Err(refuse("is not a copy type; error types must be copy")),
            Position::Param(pass) => match (ty, pass) {
                (AbiType::Slice { .. }, _) => Ok(()),
                (AbiType::Seq { elem }, _) if self.owning(elem) => Err(refuse(SEQ_OF_HANDLES)),
                // A by-value sequence is the callee's to free (#932).
                (AbiType::Seq { .. }, _) => Ok(()),
                (AbiType::Named { name, .. }, _) if self.handle(name) => Ok(()),
                (AbiType::Str { .. }, Pass::In) => Ok(()),
                (AbiType::Str { owned: true }, Pass::Inout | Pass::Sink) => Ok(()),
                (AbiType::Str { .. }, _) => Err(refuse(
                    "is a `Str` view, which crosses only as an `in` parameter",
                )),
                _ => Err(refuse(
                    "owns memory; only sequences, slices, strings and handle types cross as parameters",
                )),
            },
            Position::Result => self.result_shape(ty).map_err(|r| refuse(&r)),
        }
    }

    fn handle(&self, name: &str) -> bool {
        self.types.get(name).is_some_and(|d| d.handle)
    }

    /// Owning results: sequences, handles, and `Option`/tuples of them.
    fn result_shape(&self, ty: &AbiType) -> Result<(), String> {
        match ty {
            _ if !self.owning(ty) => Ok(()),
            AbiType::Seq { elem } if self.owning(elem) => Err(SEQ_OF_HANDLES.into()),
            AbiType::Seq { .. } | AbiType::Str { owned: true } => Ok(()),
            AbiType::Named { name, .. } if self.handle(name) => Ok(()),
            AbiType::Named { name, args } if name == "Option" => self.result_shape(&args[0]),
            AbiType::Tuple { items } => items.iter().try_for_each(|t| self.result_shape(t)),
            AbiType::Box { .. } => {
                Err("is a box; return a handle type that holds it instead".into())
            }
            _ => Err("owns memory in a form no result can carry".into()),
        }
    }

    /// The forms a field of a handle type may have.
    fn field_shape(&self, ty: &AbiType) -> Result<(), String> {
        match ty {
            _ if !self.owning(ty) => Ok(()),
            AbiType::Seq { .. } | AbiType::Str { owned: true } => Ok(()),
            AbiType::Named { name, .. } if self.handle(name) => Ok(()),
            AbiType::Named { name, args } if name == "Option" => self.field_shape(&args[0]),
            AbiType::Box { inner } if matches!(**inner, AbiType::Object { .. }) => Ok(()),
            AbiType::Box { inner } => self.field_shape(inner),
            _ => Err(
                "is a field form handles do not carry yet (use a sequence, a String, a box, a handle type or an Option of one)"
                    .into(),
            ),
        }
    }

    fn ty(&mut self, function: &str, ty: &Type) -> Result<AbiType, ExportError> {
        let refuse = |reason: &str| ExportError::Type {
            function: function.into(),
            ty: ty.to_string(),
            reason: reason.into(),
        };
        let world = self.world;
        // An opaque `.vsys` extern-type handle (#1065) stays inside Vesper:
        // its library, not the export binding, owns and finalizes it.
        if let Type::Named(n, args) = ty
            && args.is_empty()
            && world.adts.get(n).is_some_and(|a| {
                a.variants.iter().any(|v| {
                    v.fields
                        .iter()
                        .any(|f| f.name == crate::ffi_lowering::HANDLE_FIELD)
                })
            })
        {
            return Err(refuse(
                "is an opaque .vsys extern-type handle; keep it inside the Vesper library (a handle crosses only to its own imports)",
            ));
        }
        match ty {
            Type::Tuple(items) if items.is_empty() => Ok(AbiType::Scalar {
                name: "Unit".into(),
            }),
            Type::Tuple(items) => Ok(AbiType::Tuple {
                items: items
                    .iter()
                    .map(|t| self.ty(function, t))
                    .collect::<Result<_, _>>()?,
            }),
            Type::Named(n, args)
                if args.is_empty() && crate::numeric::is_scalar_name(n) && n != "Never" =>
            {
                Ok(AbiType::Scalar { name: n.clone() })
            }
            t if crate::seq::is_seq_or_slice(t) => {
                let elem = crate::seq::elem(t).expect("sequence element");
                let abi = self.ty(function, elem)?;
                if world.mode(elem, &BTreeMap::new()) != Mode::Copy || self.owning(&abi) {
                    // A `ChunkSeq` of handles or boxed objects (#1087) is
                    // a field form: the handle that holds it stays opaque
                    // to Rust, and its drop glue drops every element.
                    let field = crate::seq::is_seq_type(t)
                        && !matches!(abi, AbiType::Slice { .. } | AbiType::Str { owned: false })
                        && self.field_shape(&abi).is_ok();
                    if !field {
                        return Err(refuse(
                            "has non-copy elements; only sequences and slices of copy elements cross the boundary (a handle type may hold a ChunkSeq of handles, boxes or strings)",
                        ));
                    }
                }
                self.v2 = true;
                let elem = Box::new(abi);
                Ok(if crate::seq::is_seq_type(t) {
                    AbiType::Seq { elem }
                } else {
                    AbiType::Slice { elem }
                })
            }
            Type::Named(n, args)
                if args.is_empty()
                    && (n == crate::strings::STRING_TYPE || n == crate::strings::STR_TYPE) =>
            {
                self.v2 = true;
                Ok(AbiType::Str {
                    owned: n == crate::strings::STRING_TYPE,
                })
            }
            t if crate::fip::is_box_type(world, t) => {
                let inner = crate::fip::box_inner(world, t).expect("box inner").clone();
                self.v2 = true;
                // A boxed trait object (#1087) is one word inside a
                // handle; its vtable, not the binding, knows the value.
                let inner = if crate::dyn_trait::is_object(&inner) {
                    AbiType::Object {
                        name: inner.to_string(),
                    }
                } else {
                    self.ty(function, &inner)?
                };
                Ok(AbiType::Box {
                    inner: Box::new(inner),
                })
            }
            Type::Named(n, args) => {
                let Some(adt) = world.adts.get(n) else {
                    return Err(refuse("has no fixed word layout"));
                };
                if adt.generics.len() != args.len() {
                    return Err(refuse("has unresolved type arguments"));
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
                            .map(|t| self.ty(function, t))
                            .collect::<Result<_, _>>()?,
                    });
                }
                if !args.is_empty() {
                    return Err(refuse(
                        "is a generic user type; export a monomorphic type instead",
                    ));
                }
                if !self.types.contains_key(n) {
                    let mode = world.mode(ty, &BTreeMap::new());
                    let l = layout(world, function, ty)?;
                    let handle = mode != Mode::Copy;
                    if handle {
                        self.v2 = true;
                    }
                    // Insert before recursing so a recursive type cannot
                    // loop (recursion goes through a box, whose layout is
                    // one word).
                    self.types.insert(
                        n.clone(),
                        TypeDecl {
                            name: n.clone(),
                            is_enum: adt.is_enum,
                            words: l.size / 8,
                            variants: vec![],
                            handle,
                            linear: mode == Mode::Linear,
                            drop_symbol: (handle && mode != Mode::Linear)
                                .then(|| format!("vesper_{}_{n}_drop", self.library)),
                            owns_sequence: false,
                            mirror: None,
                        },
                    );
                    let fields_of = |fields: &[crate::layout::LayoutField],
                                     planner: &mut Self|
                     -> Result<Vec<FieldDecl>, ExportError> {
                        fields
                            .iter()
                            .map(|f| {
                                let ty = planner.ty(function, &f.ty)?;
                                if handle {
                                    planner.field_shape(&ty).map_err(|reason| {
                                        ExportError::Type {
                                            function: function.into(),
                                            ty: format!("{n}.{}", f.name),
                                            reason,
                                        }
                                    })?;
                                }
                                Ok(FieldDecl {
                                    name: f.name.clone(),
                                    ty,
                                    offset: f.offset as u32 / 8,
                                })
                            })
                            .collect()
                    };
                    let variants = match &l.kind {
                        LayoutKind::Product(fields) => vec![VariantDecl {
                            name: n.clone(),
                            fields: fields_of(fields, self)?,
                        }],
                        LayoutKind::Enum(variants) => variants
                            .iter()
                            .map(|v| {
                                Ok(VariantDecl {
                                    name: v.name.clone(),
                                    fields: fields_of(&v.fields, self)?,
                                })
                            })
                            .collect::<Result<_, ExportError>>()?,
                        LayoutKind::Scalar => return Err(refuse("has no word layout")),
                    };
                    self.types.get_mut(n).expect("inserted above").variants = variants;
                }
                Ok(AbiType::Named {
                    name: n.clone(),
                    args: vec![],
                })
            }
            _ => Err(refuse("has no fixed word layout")),
        }
    }

    /// Mark every handle type that owns a sequence, through fields, boxes
    /// and nested handles (a fixpoint, since types may be recursive).
    fn mark_owned_sequences(&mut self) {
        loop {
            let mut changed = false;
            let names = self.types.keys().cloned().collect::<Vec<_>>();
            for name in names {
                if self.types[&name].owns_sequence {
                    continue;
                }
                let owns = self.types[&name]
                    .variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .any(|f| holds_sequence(&f.ty, &self.types));
                if owns {
                    self.types.get_mut(&name).expect("listed").owns_sequence = true;
                    changed = true;
                }
            }
            if !changed {
                return;
            }
        }
    }
}

/// v3: give every copy type its Rust storage. Every copy struct with at
/// least one field is mirrored (each field cell is one or more words);
/// enums and empty structs are converted.
fn mark_mirrors(types: &mut BTreeMap<String, TypeDecl>) {
    for decl in types.values_mut().filter(|d| !d.handle) {
        decl.mirror = Some(if decl.is_enum {
            Mirror::Converted {
                reason: "an enum's payload words have no fixed Rust layout (tag and payload validity), so it is converted as in v2".into(),
            }
        } else if decl.variants[0].fields.is_empty() {
            Mirror::Converted {
                reason: "a struct without fields is one ignored word".into(),
            }
        } else {
            Mirror::ReprC
        });
    }
}

/// Whether `ty` is a copy struct with a `#[repr(C)]` mirror (v3).
pub(crate) fn mirrored(ty: &AbiType, types: &BTreeMap<String, TypeDecl>) -> bool {
    matches!(ty, AbiType::Named { name, .. }
        if types.get(name).is_some_and(|d| d.mirror == Some(Mirror::ReprC)))
}

/// Whether `ty` crosses as a mirror (v3): a mirrored struct or, when the
/// library opted in (`abi 3 options`), an `Option[T]`/`Result[T, E]` whose
/// payloads are copy values. A payload cell is a nested mirror, a word, a
/// `Word` or a `Words`, so every copy payload has one.
pub(crate) fn lent(ty: &AbiType, types: &BTreeMap<String, TypeDecl>, options: bool) -> bool {
    mirrored(ty, types)
        || options
            && matches!(ty, AbiType::Named { name, args }
                if matches!(name.as_str(), "Option" | "Result") && !owning(ty, types)
                    && args.len() == if name == "Option" { 1 } else { 2 })
}

fn holds_sequence(ty: &AbiType, types: &BTreeMap<String, TypeDecl>) -> bool {
    match ty {
        // Native code frees neither a dropped sequence nor a dropped string.
        AbiType::Seq { .. } | AbiType::Str { owned: true } => true,
        AbiType::Scalar { .. } | AbiType::Slice { .. } | AbiType::Str { owned: false } => false,
        // A boxed object's value may own anything; its vtable drops it.
        AbiType::Object { .. } => false,
        AbiType::Tuple { items } => items.iter().any(|t| holds_sequence(t, types)),
        AbiType::Box { inner } => holds_sequence(inner, types),
        AbiType::Named { name, args } => {
            types.get(name).is_some_and(|d| d.owns_sequence)
                || args.iter().any(|t| holds_sequence(t, types))
        }
    }
}

/// Whether a value of `ty` owns memory (a sequence, a box or a handle).
pub(crate) fn owning(ty: &AbiType, types: &BTreeMap<String, TypeDecl>) -> bool {
    match ty {
        AbiType::Seq { .. }
        | AbiType::Slice { .. }
        | AbiType::Box { .. }
        | AbiType::Str { .. }
        | AbiType::Object { .. } => true,
        AbiType::Scalar { .. } => false,
        AbiType::Tuple { items } => items.iter().any(|t| owning(t, types)),
        AbiType::Named { name, args } => {
            types.get(name).is_some_and(|d| d.handle) || args.iter().any(|t| owning(t, types))
        }
    }
}

/// Size of `ty` in words.
pub(crate) fn abi_words(ty: &AbiType, types: &BTreeMap<String, TypeDecl>) -> u32 {
    match ty {
        AbiType::Scalar { .. }
        | AbiType::Seq { .. }
        | AbiType::Box { .. }
        | AbiType::Object { .. } => 1,
        AbiType::Slice { .. } | AbiType::Str { .. } => 3,
        AbiType::Tuple { items } => items
            .iter()
            .map(|t| abi_words(t, types))
            .sum::<u32>()
            .max(1),
        AbiType::Named { name, args } => match name.as_str() {
            "Option" => 1 + abi_words(&args[0], types),
            "Result" => 1 + abi_words(&args[0], types).max(abi_words(&args[1], types)),
            "ArithOverflow" | "RuntimeError" => 1,
            _ => types.get(name).map_or(1, |d| d.words),
        },
    }
}

fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

fn layout(world: &World, function: &str, ty: &Type) -> Result<Layout, ExportError> {
    Layout::of(world, ty).map_err(|reason| ExportError::Type {
        function: function.into(),
        ty: ty.to_string(),
        reason: format!("has no layout: {reason}"),
    })
}

/// Compile every function of `interface` into one object for `target`
/// (the host when `None`). Exported functions keep their export symbols for
/// calls between them; everything else they reach is local. A v2 library
/// also gets its runtime entry points and the drop glue of its handles.
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
    let isa = object_isa_optimized(world, target, optimize)?;
    if windows::is_windows(isa.triple()) {
        return windows::emit_with(isa, "vesper", |m| {
            compile_exports(m, world, interface, &exports, optimize)
        })
        .map(|(bytes, _)| bytes);
    }
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    compile_exports(&mut m, world, interface, &exports, optimize)?;
    m.finish().emit().map_err(|e| e.to_string())
}

fn compile_exports<M: Module>(
    m: &mut M,
    world: &World,
    interface: &LibraryInterface,
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
    let mut program = crate::ffi_lowering::drops::lower_world(world);
    crate::integer_widths::lower_bit_builtin_calls(world, &mut program);
    if optimize {
        crate::ir_opt::optimize(world, &mut program)?;
        crate::ir_inline::inline(world, &mut program, &crate::ir_inline::Budget::default());
    } else {
        crate::ir_opt::fip_reuse_pass(world, &mut program)?;
    }
    crate::fip::elaborate_non_fip_drops(world, &mut program);
    if optimize {
        // Native only, never the verified backend (Stage 1 perf lane 3).
        crate::bounds_hoist::run(&mut program);
    }
    // Exported symbols keep the failure-record ABI that foreign callers and
    // the v2 glue use; internal calls use the register conventions of [`NativeAbi`] (#935).
    let abi = NativeAbi::plan(world, &all, optimize, |key| exports.contains_key(key))?;
    let mut ids = BTreeMap::new();
    for (i, (key, (name, vars))) in all.iter().enumerate() {
        let f = concrete(world, name, vars)?;
        let (symbol, linkage) = match exports.get(key) {
            Some(symbol) => (symbol.clone(), Linkage::Export),
            None => (format!("vesper_function_{i}"), Linkage::Local),
        };
        let sig = signature_with_abi(m, world, &f, false, abi.of(key))?;
        let id = m
            .declare_function(&symbol, linkage, &sig)
            .map_err(|e| e.to_string())?;
        ids.insert(key.clone(), id);
    }
    let clif = define_instances_observed_with_trace(
        m,
        world,
        &all,
        &ids,
        &program,
        None,
        None,
        CodegenOptions::default(),
        DefinitionLinkage::Object,
        false,
        None,
        optimize,
        &abi,
    )?;
    if interface.is_v2() {
        glue::define(m, world, interface)?;
    }
    Ok(clif)
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
/// `vesper build --lib` does. `path` only labels diagnostics. The source's
/// `@export` directives ([`directives`]) name generic instances and `.vsys`
/// imports.
pub fn compile_source(
    path: &str,
    source: &str,
    options: &LibraryOptions<'_>,
) -> Result<Library, ExportError> {
    compile_source_with(path, source, options, &[])
}

/// [`compile_source`] with extra instances (`--instance`) on top of the
/// source's directives.
pub fn compile_source_with(
    path: &str,
    source: &str,
    options: &LibraryOptions<'_>,
    instances: &[Instance],
) -> Result<Library, ExportError> {
    let parsed = crate::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(diagnostics(path, &parsed.diagnostics, "syntax"));
    }
    compile_module(path, parsed.module, source, options, instances, None)
}

fn diagnostics(path: &str, ds: &[crate::ast::Diagnostic], what: &str) -> ExportError {
    let shown = ds
        .iter()
        .take(8)
        .map(|d| format!("{path}:{}: {}: {}", d.span.start, d.code, d.message))
        .collect::<Vec<_>>()
        .join("\n");
    ExportError::Compile(format!("{} {what} error(s)\n{shown}", ds.len()))
}

/// Check, optimize and compile a parsed module (a single file, or the
/// assembled module of a package). `source` is the text the `@export`
/// directives are read from. The exports are `public` (the module's
/// `public_items` when `None`) plus one generated wrapper per instance.
pub fn compile_module(
    path: &str,
    module: crate::ast::Module,
    source: &str,
    options: &LibraryOptions<'_>,
    instances: &[Instance],
    public: Option<&BTreeSet<String>>,
) -> Result<Library, ExportError> {
    compile_package_module(path, module, source, &[], options, instances, public)
}

/// [`compile_module`] for the assembled module of a package graph, with the
/// `.vsys` libraries its packages carry (`[package] vsys` in each
/// `Loom.toml`, [`crate::packages::Assembled::vsys`]) imported beside the
/// source's own `@export vsys` directives.
pub fn compile_package_module(
    path: &str,
    mut module: crate::ast::Module,
    source: &str,
    packages: &[crate::packages::PackageVsys],
    options: &LibraryOptions<'_>,
    instances: &[Instance],
    public: Option<&BTreeSet<String>>,
) -> Result<Library, ExportError> {
    let mut directives = Directives::parse(source, options.target)?;
    directives.add_packages(packages, options.target)?;
    directives.instances.extend(instances.iter().cloned());
    let mut public = match public {
        Some(p) => p.clone(),
        None => module.public_items.iter().cloned().collect(),
    };
    instance_wrappers(&mut module, source.len(), &directives, &mut public)?;
    if !directives.vsys.is_empty() {
        return compile_foreign(&module, &public, options, &directives);
    }
    let mut checked = crate::check::check(&module);
    if !checked.diagnostics.is_empty() {
        return Err(diagnostics(path, &checked.diagnostics, "semantic"));
    }
    if options.optimize {
        crate::optimize::optimize(&mut module, &checked.expression_types);
        checked = crate::check::check(&module);
        if !checked.diagnostics.is_empty() {
            return Err(diagnostics(
                path,
                &checked.diagnostics,
                "optimizer validation",
            ));
        }
    }
    let interface =
        plan_with(&checked.world, &public, options.library, &directives).map_err(joined)?;
    let object = object(&checked.world, &interface, options.target, options.optimize)
        .map_err(ExportError::Compile)?;
    Ok(Library { object, interface })
}

/// Append one monomorphic `pub fn <alias>` per instance that calls the
/// generic function at its type arguments, so bounded generics (which
/// exist only as monomorphized clones after checking) export too. The
/// wrappers are parsed past the end of the source, so their spans never
/// collide with the source's.
fn instance_wrappers(
    module: &mut crate::ast::Module,
    source_len: usize,
    directives: &Directives,
    public: &mut BTreeSet<String>,
) -> Result<(), ExportError> {
    if directives.instances.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for instance in &directives.instances {
        let refuse = |reason: String| ExportError::Instance {
            instance: instance.spelling(),
            reason,
        };
        let generic = module.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == instance.function => Some(f),
            _ => None,
        });
        let Some(f) = generic else {
            return Err(refuse(format!("no function '{}'", instance.function)));
        };
        if !public.contains(&instance.function) {
            return Err(refuse(format!("'{}' is not pub", instance.function)));
        }
        if f.generics.len() != instance.args.len() || f.generics.iter().any(|g| g.row) {
            return Err(refuse(format!(
                "'{}' has {} type parameter(s)",
                instance.function,
                f.generics.len()
            )));
        }
        let alias = instance.alias();
        if !valid_library_name(&alias) || public.contains(&alias) {
            return Err(refuse(format!(
                "instance name '{alias}' must be a new name matching [a-z][a-z0-9_]*"
            )));
        }
        let vars = f
            .generics
            .iter()
            .zip(&instance.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let plain = |t: &Type| {
            fn visit(t: &Type) -> bool {
                match t {
                    Type::Named(_, args) | Type::Tuple(args) => args.iter().all(visit),
                    _ => false,
                }
            }
            visit(t) && !matches!(t, Type::Tuple(items) if items.len() == 1)
        };
        let mut params = vec![];
        let mut args = vec![];
        for p in &f.params {
            let ty = crate::ast::substitute(&p.ty, &vars);
            if !plain(&ty) {
                return Err(refuse(format!("parameter '{}' has type {ty}", p.name)));
            }
            // The wrapper keeps the generic's passing modes: an unannotated
            // parameter would default to `in` and could not be consumed.
            let (declared, passed) = match p.pass {
                Pass::In => ("in ", "in "),
                Pass::Inout => ("inout ", "inout "),
                Pass::Sink => ("sink ", ""),
            };
            params.push(format!("{declared}{}: {ty}", p.name));
            args.push(format!("{passed}{}", p.name));
        }
        let result = crate::ast::substitute(&f.result, &vars);
        if !plain(&result) {
            return Err(refuse(format!("the result has type {result}")));
        }
        let effects = crate::ast::substitute_effects(&f.effects, &vars);
        if effects.tail.is_some() {
            return Err(refuse("the effect row is open".into()));
        }
        let row = effects
            .instances
            .iter()
            .map(|e| {
                let label = e
                    .label
                    .as_ref()
                    .map(|l| format!("{l}: "))
                    .unwrap_or_default();
                if e.args.is_empty() {
                    format!("{label}{}", e.name)
                } else {
                    format!(
                        "{label}{}[{}]",
                        e.name,
                        e.args
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            })
            .collect::<Vec<_>>();
        let row = if row.is_empty() {
            String::new()
        } else {
            format!(" eff [{}]", row.join(", "))
        };
        text.push_str(&format!(
            "pub fn {alias}({}) -> {result}{row} {{\n    {}[{}]({})\n}}\n",
            params.join(", "),
            instance.function,
            instance
                .args
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            args.join(", ")
        ));
        public.insert(alias);
    }
    // Parse past the end of the source (and of any package's combined
    // sources, which `source_len` covers).
    let padded = format!("{}\n{text}", " ".repeat(source_len));
    let parsed = crate::parser::parse(&padded);
    if !parsed.diagnostics.is_empty() {
        return Err(ExportError::Directive(format!(
            "generated instance wrappers do not parse: {}",
            parsed
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    module.public_items.extend(parsed.module.public_items);
    module.items.extend(parsed.module.items);
    Ok(())
}

/// A module that imports `.vsys` libraries: checked and compiled through
/// [`crate::ffi_lowering`], which owns the foreign-call plan. Source-level
/// optimization is skipped (the IR optimizer still runs).
fn compile_foreign(
    module: &crate::ast::Module,
    public: &BTreeSet<String>,
    options: &LibraryOptions<'_>,
    directives: &Directives,
) -> Result<Library, ExportError> {
    let mut imports = crate::ffi_lowering::Imports::new();
    for v in &directives.vsys {
        imports
            .add(&v.source, v.handler)
            .map_err(|e| ExportError::Directive(format!("@export vsys {}: {e}", v.library)))?;
    }
    let checked = crate::ffi_lowering::check_module(module, &imports)
        .map_err(|e| ExportError::Compile(e.to_string()))?;
    let interface =
        plan_with(checked.world(), public, options.library, directives).map_err(joined)?;
    let object = checked
        .with_lowering(options.target, |world| {
            object(world, &interface, options.target, options.optimize)
        })
        .map_err(|e| ExportError::Compile(e.to_string()))?;
    Ok(Library { object, interface })
}

fn joined(errors: Vec<ExportError>) -> ExportError {
    ExportError::Compile(
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// [`compile_source`] for an already checked (and optimized) module.
pub fn compile_world(
    world: &World,
    public: &BTreeSet<String>,
    options: &LibraryOptions<'_>,
) -> Result<Library, ExportError> {
    compile_world_with(world, public, options, &Directives::default())
}

/// [`compile_world`] with directives. Generic instances and `.vsys` imports
/// need the parsed module, so they compile only through [`compile_module`].
pub fn compile_world_with(
    world: &World,
    public: &BTreeSet<String>,
    options: &LibraryOptions<'_>,
    directives: &Directives,
) -> Result<Library, ExportError> {
    if !directives.vsys.is_empty() || !directives.instances.is_empty() {
        return Err(ExportError::Directive(
            "@export vsys and instances need the module: compile it with compile_module".into(),
        ));
    }
    let interface = plan_with(world, public, options.library, directives).map_err(joined)?;
    let object = object(world, &interface, options.target, options.optimize)
        .map_err(ExportError::Compile)?;
    Ok(Library { object, interface })
}

/// The interface file text: pretty JSON with a trailing newline.
pub fn interface_json(interface: &LibraryInterface) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(interface).map_err(|e| e.to_string())?;
    json.push('\n');
    Ok(json)
}

/// Write a compiled library: the object to `output`, and optionally the
/// interface JSON and the Rust binding. Returns the summary line.
pub fn write_library(
    library: &Library,
    output: &std::path::Path,
    interface_path: Option<&std::path::Path>,
    binding: Option<(&std::path::Path, &BindingOptions)>,
) -> Result<String, String> {
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
        "built {} ({} exported function(s), {})",
        output.display(),
        library.interface.functions.len(),
        library.interface.abi
    ))
}

/// `vesper build --lib` for a checked world: write the object to `output`,
/// and optionally the interface JSON and the Rust binding. Returns the
/// summary line.
pub fn build_command(
    world: &World,
    public: &BTreeSet<String>,
    options: &LibraryOptions<'_>,
    output: &std::path::Path,
    interface_path: Option<&std::path::Path>,
    binding: Option<(&std::path::Path, &BindingOptions)>,
) -> Result<String, String> {
    let library = compile_world(world, public, options).map_err(|e| e.to_string())?;
    write_library(&library, output, interface_path, binding)
}
