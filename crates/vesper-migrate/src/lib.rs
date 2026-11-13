//! `vesper migrate`: incremental C/Rust migration (design §29.3 and the
//! lifter half of §29.5).
//!
//! Two front ends analyse legacy *signatures*:
//!
//! * [`c::analyze_header`] parses a preprocessor-free subset of a C header with
//!   the `lang-c` parser (functions, structs, enums, typedefs, pointers with
//!   const-ness, arrays and fixed-width integers);
//! * [`rust::analyze_source`] / [`rust::analyze_crate_dir`] parse a Rust crate
//!   with `syn` and lift borrow summaries to Vesper parameter modes (`in`,
//!   `inout`, `sink`) with FieldTree footprints where the function body shows
//!   which fields a borrowed parameter projects.
//!
//! Both produce a [`Migration`], from which [`synth`] renders a declarative
//! Bridge `.vsys` sys-spec (re-parsed by `vesper_ffi::sys::parse` before it is
//! returned), Vesper extern declarations, and `vesper_ffi` wire-layout
//! declarations. Anything the signature cannot justify is an explicit
//! [`Refusal`] with a typed [`RefusalReason`].
//!
//! [`shadow`] runs a legacy implementation and its replacement over generated
//! inputs and reports the first divergence. Finite differential testing is not
//! an equivalence proof, and MPK/PKRU isolation is reported unavailable.
//!
//! The lifted modes are summaries of what a *signature and body scan* show.
//! They are not an implementation of the Vesper source typing rules, and Rust
//! ownership in this crate is not evidence that those rules hold.
#![forbid(unsafe_code)]

pub mod c;
pub mod lift;
pub mod rust;
pub mod shadow;
pub mod synth;

use std::fmt;

pub use vesper_ffi::sys::{Access, Scalar};

/// Schema tag carried by every rendered migration artefact.
pub const SCHEMA: &str = "vesper.migrate/1";

/// Resource budgets for untrusted legacy sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum bytes of one source file.
    pub source_bytes: usize,
    /// Maximum total bytes across all files of one crate.
    pub total_bytes: usize,
    /// Maximum number of files read for one crate.
    pub files: usize,
    /// Maximum bracket nesting depth (guards recursive-descent parsers).
    pub nesting: usize,
    /// Maximum number of top-level items (functions, structs, enums, typedefs).
    pub items: usize,
    /// Maximum parameters per function.
    pub parameters: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            source_bytes: 1 << 20,
            total_bytes: 8 << 20,
            files: 256,
            nesting: 128,
            items: 4096,
            parameters: 64,
        }
    }
}

/// Category of a hard failure (the whole input is rejected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A budget in [`Limits`] was exceeded.
    Limit,
    /// The source is not valid C/Rust in the supported subset.
    Syntax,
    /// A preprocessor directive outside the documented subset.
    Preprocessor,
    /// Reading a crate directory failed.
    Io,
    /// Two exported items share one name.
    Duplicate,
    /// A synthesized artefact failed to round-trip through its consumer.
    Synthesis,
    /// The caller passed inconsistent options.
    Options,
}

/// A typed hard failure. Untrusted input never panics; it produces this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateError {
    pub kind: ErrorKind,
    /// 1-based line in the offending file, when known.
    pub line: Option<usize>,
    pub message: String,
}

impl MigrateError {
    pub(crate) fn new(kind: ErrorKind, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            kind,
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{:?} error at line {line}: {}", self.kind, self.message),
            None => write!(f, "{:?} error: {}", self.kind, self.message),
        }
    }
}

impl std::error::Error for MigrateError {}

/// Why an item could not be lifted. Each refusal leaves the item out of the
/// generated artefacts rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefusalReason {
    /// A raw pointer without a `vesper-migrate:` annotation stating its access.
    RawPointerWithoutAnnotation,
    /// `Cell`, `RefCell`, `UnsafeCell`, atomics, locks: shared mutation that a
    /// borrow summary cannot express as `in`/`inout`.
    InteriorMutability,
    /// One lifetime ties a borrow to its own referent (`&'a mut T<'a>`).
    SelfReferentialLifetime,
    /// A returned borrow cannot be expressed in a lifetime-free signature.
    ReturnedBorrow,
    /// A type that carries lifetime parameters (it stores borrows).
    BorrowingType,
    /// Type or const generics, `impl Trait`, `dyn Trait`.
    Generic,
    /// The function is not a C-ABI symbol, so no `.vsys` entry is possible.
    NotCAbi,
    /// An integer whose width differs across the supported targets
    /// (`long`, `isize` in C ABI position, plain `char`).
    PlatformDependentWidth,
    /// Floating point carriers are not supported by Bridge sys-specs.
    Floating,
    /// Function pointers, closures and callbacks.
    FunctionPointer,
    /// Variadic C functions.
    Variadic,
    /// C unions and untagged overlays.
    Union,
    /// C bit-fields.
    Bitfield,
    /// The wire layout compiler rejected the struct (padding, unsupported field).
    WireLayout,
    /// A C enum or `_Bool` whose ABI carrier is implementation-defined.
    ImplementationDefinedCarrier,
    /// Aggregates passed by value or by pointer, which `.vsys` does not model.
    Aggregate,
    /// An unresolved, opaque or `void` pointee.
    OpaquePointer,
    /// A single-element pointer parameter (an out-parameter). Bridge `.vsys`
    /// accepts pointer parameters only as `nocapture` pointer/length slices.
    PointerParameter,
    /// An item form outside the supported subset (globals, macros, traits, ...).
    UnsupportedItem,
    /// A type form outside the supported subset.
    UnsupportedType,
    /// A malformed `vesper-migrate:` annotation.
    BadAnnotation,
}

/// One item that was deliberately not lifted, with a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Refusal {
    /// The item (`fn name`, `struct Name`, `fn name: sys`, ...).
    pub item: String,
    pub reason: RefusalReason,
    pub detail: String,
}

impl Refusal {
    pub(crate) fn new(
        item: impl Into<String>,
        reason: RefusalReason,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            item: item.into(),
            reason,
            detail: detail.into(),
        }
    }
}

/// Vesper parameter mode (§2 of the design): shared borrow, unique borrow,
/// consumed owner, or a plain copied value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    In,
    Inout,
    Sink,
    /// A `Copy` scalar passed by value; no borrow is involved.
    Value,
}

impl Mode {
    pub fn keyword(self) -> Option<&'static str> {
        match self {
            Self::In => Some("in"),
            Self::Inout => Some("inout"),
            Self::Sink => Some("sink"),
            Self::Value => None,
        }
    }
}

/// A `.vsys` value carrier, mirroring `vesper_ffi::sys::ValueType` by name so
/// that the rendered text is checked by that parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysValue {
    Unit,
    Scalar(Scalar),
    Bool,
    Enum(String),
    Address { pointee: Scalar, nullable: bool },
}

/// A `.vsys` parameter: a value or a `nocapture` pointer/length slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysParam {
    Value(SysValue),
    Slice { element: Scalar, access: Access },
}

/// The C-ABI view of one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysSignature {
    pub params: Vec<(String, SysParam)>,
    pub result: SysValue,
}

/// One lifted parameter of the Vesper extern declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftedParam {
    pub name: String,
    pub mode: Mode,
    /// FieldTree footprint (`x.{a, b.c}`); `None` means the whole value.
    pub footprint: Option<Vec<String>>,
    /// Vesper surface type (`u32`, `ByteSlice`, `DecoderStats`, ...).
    pub ty: String,
}

/// A function lifted from either front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftedFunction {
    pub name: String,
    pub params: Vec<LiftedParam>,
    /// Vesper surface result type (`Unit` when absent).
    pub result: String,
    /// The `.vsys` signature, or why the function has none.
    pub sys: Result<SysSignature, Refusal>,
    /// Facts the lifter assumed rather than proved (shown in reports).
    pub assumptions: Vec<String>,
}

/// Field of a lifted struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftedField {
    pub name: String,
    pub ty: String,
}

/// A struct lifted from either front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftedStruct {
    pub name: String,
    pub fields: Vec<LiftedField>,
    /// Native-endian wire declaration for `vesper_ffi::Registry`, or why the
    /// struct has no checked byte layout.
    pub wire: Result<vesper_ffi::Declaration, Refusal>,
}

/// An enum lifted from either front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftedEnum {
    pub name: String,
    pub variants: Vec<(String, i128)>,
    /// Unsigned fixed-width ABI carrier, or why the enum has none.
    pub carrier: Result<Scalar, Refusal>,
}

/// Where a migration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    CHeader { header: String },
    RustCrate { name: String },
}

/// The complete result of analysing one legacy library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub origin: Origin,
    pub functions: Vec<LiftedFunction>,
    pub structs: Vec<LiftedStruct>,
    pub enums: Vec<LiftedEnum>,
    /// Items refused entirely (no Vesper declaration at all).
    pub refusals: Vec<Refusal>,
}

impl Migration {
    pub fn function(&self, name: &str) -> Option<&LiftedFunction> {
        self.functions.iter().find(|f| f.name == name)
    }
    pub fn structure(&self, name: &str) -> Option<&LiftedStruct> {
        self.structs.iter().find(|s| s.name == name)
    }
    pub fn enumeration(&self, name: &str) -> Option<&LiftedEnum> {
        self.enums.iter().find(|e| e.name == name)
    }
    /// Every refusal, including partial ones (no `.vsys` entry, no wire
    /// layout, no enum carrier), sorted for stable reports.
    pub fn all_refusals(&self) -> Vec<&Refusal> {
        let mut all: Vec<&Refusal> = self.refusals.iter().collect();
        all.extend(self.functions.iter().filter_map(|f| f.sys.as_ref().err()));
        all.extend(self.structs.iter().filter_map(|s| s.wire.as_ref().err()));
        all.extend(self.enums.iter().filter_map(|e| e.carrier.as_ref().err()));
        all.sort();
        all
    }
}

/// Reject inputs nested deeper than `limit` before handing them to a
/// recursive-descent parser, so hostile input cannot overflow the stack.
pub(crate) fn check_nesting(source: &str, limit: usize) -> Result<(), MigrateError> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    // Angle brackets are only a heuristic (they are also operators), so their
    // depth resets at every statement or block boundary.
    let mut angle = 0usize;
    let mut line = 1usize;
    let mut i = 0usize;
    let too_deep = |line| {
        MigrateError::new(
            ErrorKind::Limit,
            Some(line),
            format!("nesting depth exceeds {limit}"),
        )
    };
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        match b {
            b'\n' => line += 1,
            b'(' | b'[' | b'{' => {
                depth += 1;
                if b == b'{' {
                    angle = 0;
                }
                if depth > limit {
                    return Err(too_deep(line));
                }
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if b == b'}' {
                    angle = 0;
                }
            }
            b';' => angle = 0,
            b'<' if matches!(next, Some(b'<') | Some(b'=')) => i += 1,
            b'-' | b'=' if next == Some(b'>') => i += 1,
            b'<' => {
                angle += 1;
                if angle > limit {
                    return Err(too_deep(line));
                }
            }
            b'>' => angle = angle.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    Ok(())
}
