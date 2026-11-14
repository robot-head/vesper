//! Bounded declarative C ABI contracts, separate from wire layouts.
//!
//! A descriptor records assertions about a foreign library. It neither loads
//! code nor authenticates an artifact, proves `nocapture`, or grants authority.
//! Calls must still cross an appropriately authorized, unsafe foreign boundary.
mod assume;
mod clayout;
mod foreign;
mod parser;

pub use assume::{
    Assumption, AssumptionCheck, BoundaryReport, Checkability, DeclaredAssumption,
    GuardObservation, MAX_GUARD_BYTES, Outcome, Position,
};
pub use clayout::{C_LAYOUT_SCHEMA, CLayoutMismatch, MAX_PROBE_OUTPUT};
pub use foreign::{
    CallFailure, CheckedAddress, ForeignValue, InvalidForeignValue, PreparedCall, RawArgument,
    ValidatedReturn, ValidatedValue, ValidationError,
};
use std::{collections::BTreeSet, fmt, ops::Range, sync::Arc};

pub const SCHEMA: &str = "vesper.vsys/1";

/// Work/allocation limits. Limits may be lowered, never raised above defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub source_bytes: usize,
    pub tokens: usize,
    pub functions: usize,
    pub parameters: usize,
    pub enums: usize,
    pub variants: usize,
    pub string_bytes: usize,
    pub canonical_bytes: usize,
    /// `wire struct` declarations usable as single-pointer pointees.
    pub wire_structs: usize,
    /// Fields per `wire struct`.
    pub wire_fields: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            source_bytes: 256 * 1024,
            tokens: 32768,
            functions: 256,
            parameters: 32,
            enums: 64,
            variants: 256,
            string_bytes: 4096,
            canonical_bytes: 1024 * 1024,
            wire_structs: 64,
            wire_fields: 256,
        }
    }
}
impl Limits {
    fn check(self) -> Result<Self, Error> {
        let hard = Self::default();
        for (value, cap) in [
            (self.source_bytes, hard.source_bytes),
            (self.tokens, hard.tokens),
            (self.functions, hard.functions),
            (self.parameters, hard.parameters),
            (self.enums, hard.enums),
            (self.variants, hard.variants),
            (self.string_bytes, hard.string_bytes),
            (self.canonical_bytes, hard.canonical_bytes),
            (self.wire_structs, hard.wire_structs),
            (self.wire_fields, hard.wire_fields),
        ] {
            if value == 0 || value > cap {
                return Err(Error::new(ErrorKind::Limit, 0..0, "invalid parser limits"));
            }
        }
        Ok(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Limit,
    Duplicate,
    Unsupported,
    Target,
    Link,
    Type,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub span: Range<usize>,
    pub message: String,
}
impl Error {
    fn new(kind: ErrorKind, span: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            kind,
            span,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}..{}",
            self.message, self.span.start, self.span.end
        )
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    LinuxX86_64,
    LinuxAarch64,
    DarwinAarch64,
    WindowsX86_64,
}
impl Target {
    pub fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::LinuxAarch64 => "aarch64-unknown-linux-gnu",
            Self::DarwinAarch64 => "aarch64-apple-darwin",
            Self::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }
    pub fn convention(self) -> CallingConvention {
        match self {
            Self::LinuxX86_64 => CallingConvention::SysV64,
            Self::LinuxAarch64 => CallingConvention::Aapcs64,
            Self::DarwinAarch64 => CallingConvention::Darwin64,
            Self::WindowsX86_64 => CallingConvention::Win64,
        }
    }
    pub fn pointer_bits(self) -> u8 {
        64
    }
    pub fn little_endian(self) -> bool {
        true
    }
    pub fn is_host(self) -> bool {
        // Architecture and OS alone also match GNU x32/AArch64 ILP32 builds.
        // Every accepted descriptor has a 64-bit little-endian data model.
        if !cfg!(all(target_pointer_width = "64", target_endian = "little")) {
            return false;
        }
        match self {
            Self::LinuxX86_64 => cfg!(all(
                target_os = "linux",
                target_arch = "x86_64",
                target_env = "gnu"
            )),
            Self::LinuxAarch64 => cfg!(all(
                target_os = "linux",
                target_arch = "aarch64",
                target_env = "gnu"
            )),
            Self::DarwinAarch64 => cfg!(all(target_os = "macos", target_arch = "aarch64")),
            Self::WindowsX86_64 => cfg!(all(
                target_os = "windows",
                target_arch = "x86_64",
                target_env = "msvc"
            )),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallingConvention {
    SysV64,
    Aapcs64,
    Darwin64,
    Win64,
}
impl CallingConvention {
    pub fn name(self) -> &'static str {
        match self {
            Self::SysV64 => "sysv64",
            Self::Aapcs64 => "aapcs64",
            Self::Darwin64 => "darwin64",
            Self::Win64 => "win64",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    Static,
    Dynamic,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDescriptor {
    mode: LinkMode,
    name: String,
    artifact_blake3: [u8; 32],
    header: Option<String>,
    pkg_config: Option<String>,
}
impl LinkDescriptor {
    pub fn mode(&self) -> LinkMode {
        self.mode
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Expected artifact content hash, not a proof of origin or actual linkage.
    pub fn artifact_blake3(&self) -> &[u8; 32] {
        &self.artifact_blake3
    }
    pub fn header(&self) -> Option<&str> {
        self.header.as_deref()
    }
    pub fn pkg_config(&self) -> Option<&str> {
        self.pkg_config.as_deref()
    }
    /// Compare concrete library bytes with the declared content digest. This
    /// establishes content equality only, not provenance, authority or safety.
    pub fn verify_artifact(&self, bytes: &[u8]) -> Result<(), ArtifactMismatch> {
        let actual = *blake3::hash(bytes).as_bytes();
        if actual == self.artifact_blake3 {
            Ok(())
        } else {
            Err(ArtifactMismatch {
                expected: self.artifact_blake3,
                actual,
            })
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMismatch {
    pub expected: [u8; 32],
    pub actual: [u8; 32],
}
impl fmt::Display for ArtifactMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "foreign artifact content differs from the declared BLAKE3 digest"
        )
    }
}
impl std::error::Error for ArtifactMismatch {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    Usize,
}
impl Scalar {
    pub fn bits(self) -> u8 {
        match self {
            Self::U8 | Self::I8 => 8,
            Self::U16 | Self::I16 => 16,
            Self::U32 | Self::I32 => 32,
            Self::U64 | Self::I64 | Self::Usize => 64,
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, Self::I8 | Self::I16 | Self::I32 | Self::I64)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::Usize => "usize",
        }
    }
}
/// Immutable enum data is shared across all parameter/return occurrences.
/// Cloning a descriptor never expands its variant table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDescriptor {
    shared: Arc<EnumData>,
}
#[derive(Debug, PartialEq, Eq)]
struct EnumData {
    name: String,
    scalar: Scalar,
    variants: Vec<(String, u64)>,
}
impl EnumDescriptor {
    fn new(name: String, scalar: Scalar, variants: Vec<(String, u64)>) -> Self {
        Self {
            shared: Arc::new(EnumData {
                name,
                scalar,
                variants,
            }),
        }
    }
    pub fn name(&self) -> &str {
        &self.shared.name
    }
    pub fn scalar(&self) -> Scalar {
        self.shared.scalar
    }
    pub fn variants(&self) -> &[(String, u64)] {
        &self.shared.variants
    }
}
/// Logical validity refinements over explicitly specified total-bit C carriers.
/// Bool uses uint8_t, enums their stated unsigned integer, never C `_Bool`/enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueType {
    Unit,
    Scalar(Scalar),
    Bool,
    Enum(EnumDescriptor),
    Address { pointee: Scalar, nullable: bool },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    In,
    Inout,
}
/// Direction of a single-element `nocapture` pointer parameter.
///
/// `In` is a shared (const) pointee the callee only reads. `Out` is a unique
/// pointee the callee must write before returning and must not read first;
/// `Inout` is a unique pointee the callee may read and write. The direction is
/// a declared contract: nothing here observes what foreign code does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerAccess {
    In,
    Out,
    Inout,
}
impl PointerAccess {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Inout => "inout",
        }
    }
    /// Whether the foreign callee may write through the pointer.
    pub fn writes(self) -> bool {
        !matches!(self, Self::In)
    }
}
/// A `wire struct` declared in a `.vsys` library and compiled by
/// [`crate::Registry`]: zero implicit padding, total bit validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireStructDescriptor {
    shared: Arc<WireStructData>,
}
#[derive(Debug, PartialEq, Eq)]
struct WireStructData {
    declaration: crate::Declaration,
    layout: crate::WireLayout,
}
impl WireStructDescriptor {
    pub fn name(&self) -> &str {
        &self.shared.declaration.name
    }
    /// The declaration as written (field order significant).
    pub fn declaration(&self) -> &crate::Declaration {
        &self.shared.declaration
    }
    /// The checked byte layout, including nested structs.
    pub fn layout(&self) -> &crate::WireLayout {
        &self.shared.layout
    }
}
/// What a single-element pointer parameter points to, or the element type of
/// a `CSlice`: a total-bit integer or a `wire struct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pointee {
    /// One total-bit integer with its natural C alignment.
    Scalar(Scalar),
    /// One `wire struct` with its compiled layout.
    Struct(WireStructDescriptor),
}
impl Pointee {
    /// Exact byte size of the pointee.
    pub fn size(&self) -> usize {
        match self {
            Self::Scalar(s) => usize::from(s.bits() / 8),
            Self::Struct(w) => w.layout().size(),
        }
    }
    /// Required address alignment of the pointee.
    pub fn alignment(&self) -> usize {
        match self {
            Self::Scalar(s) => usize::from(s.bits() / 8),
            Self::Struct(w) => w.layout().alignment(),
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Scalar(s) => s.name(),
            Self::Struct(w) => w.name(),
        }
    }
}
/// Typed layout of one single-element pointer parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerLayout {
    pub size: usize,
    pub alignment: usize,
    pub access: PointerAccess,
    pub nullable: bool,
}
/// Typed layout of one pointer/length slice parameter. The byte stride of the
/// C array is `element_size`; the base address must be `alignment`-aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceLayout {
    pub element_size: usize,
    pub alignment: usize,
    pub access: Access,
    pub nullable: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterType {
    Value(ValueType),
    /// A borrowed, noncapturing pointer followed by element count (`size_t`).
    /// Elements are total-bit integers or `wire struct`s. `nullable` only
    /// when declared `Option[CSlice[T]]`; then `(NULL, 0)` means `None`.
    Slice {
        element: Pointee,
        access: Access,
        nullable: bool,
    },
    /// A borrowed, noncapturing pointer to exactly one scalar or wire struct,
    /// passed as one C pointer. `nullable` only when declared `Option[T]`.
    Pointer {
        pointee: Pointee,
        access: PointerAccess,
        nullable: bool,
    },
}
impl ParameterType {
    /// Size, alignment and direction of a single-element pointer parameter.
    pub fn pointer_layout(&self) -> Option<PointerLayout> {
        match self {
            Self::Pointer {
                pointee,
                access,
                nullable,
            } => Some(PointerLayout {
                size: pointee.size(),
                alignment: pointee.alignment(),
                access: *access,
                nullable: *nullable,
            }),
            _ => None,
        }
    }
    /// Element size, alignment, direction and nullability of a slice.
    pub fn slice_layout(&self) -> Option<SliceLayout> {
        match self {
            Self::Slice {
                element,
                access,
                nullable,
            } => Some(SliceLayout {
                element_size: element.size(),
                alignment: element.alignment(),
                access: *access,
                nullable: *nullable,
            }),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterDescriptor {
    name: String,
    ty: ParameterType,
}
impl ParameterDescriptor {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn ty(&self) -> &ParameterType {
        &self.ty
    }
}
/// Raw machine ABI carrier. Pointer constness/access stays in ParameterType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiType {
    Integer(Scalar),
    Pointer,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSignature {
    pub convention: CallingConvention,
    pub parameters: Vec<AbiType>,
    pub result: Option<AbiType>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDescriptor {
    symbol: String,
    parameters: Vec<ParameterDescriptor>,
    result: ValueType,
    convention: CallingConvention,
}
impl FunctionDescriptor {
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    pub fn parameters(&self) -> &[ParameterDescriptor] {
        &self.parameters
    }
    pub fn result(&self) -> &ValueType {
        &self.result
    }
    pub fn abi(&self) -> AbiSignature {
        let mut parameters = vec![];
        for p in &self.parameters {
            match &p.ty {
                ParameterType::Value(t) => {
                    parameters.push(carrier(t).expect("validated non-unit parameter"))
                }
                ParameterType::Slice { .. } => {
                    parameters.push(AbiType::Pointer);
                    parameters.push(AbiType::Integer(Scalar::Usize));
                }
                ParameterType::Pointer { .. } => parameters.push(AbiType::Pointer),
            }
        }
        AbiSignature {
            convention: self.convention,
            parameters,
            result: carrier(&self.result),
        }
    }
}
fn carrier(t: &ValueType) -> Option<AbiType> {
    Some(match t {
        ValueType::Unit => return None,
        ValueType::Scalar(s) => AbiType::Integer(*s),
        ValueType::Bool => AbiType::Integer(Scalar::U8),
        ValueType::Enum(e) => AbiType::Integer(e.scalar()),
        ValueType::Address { .. } => AbiType::Pointer,
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorId([u8; 32]);
impl DescriptorId {
    pub fn digest(&self) -> &[u8; 32] {
        &self.0
    }
}
impl fmt::Display for DescriptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEMA}:blake3:")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryDescriptor {
    name: String,
    target: Target,
    link: LinkDescriptor,
    enums: Vec<EnumDescriptor>,
    wire_structs: Vec<WireStructDescriptor>,
    functions: Vec<FunctionDescriptor>,
    canonical: Vec<u8>,
    identity: DescriptorId,
}
impl LibraryDescriptor {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn target(&self) -> Target {
        self.target
    }
    pub fn link(&self) -> &LinkDescriptor {
        &self.link
    }
    pub fn enums(&self) -> &[EnumDescriptor] {
        &self.enums
    }
    pub fn functions(&self) -> &[FunctionDescriptor] {
        &self.functions
    }
    /// Declared `wire struct` pointees, sorted by name.
    pub fn wire_structs(&self) -> &[WireStructDescriptor] {
        &self.wire_structs
    }
    pub fn wire_struct(&self, name: &str) -> Option<&WireStructDescriptor> {
        self.wire_structs.iter().find(|w| w.name() == name)
    }
    pub fn function(&self, symbol: &str) -> Option<&FunctionDescriptor> {
        self.functions
            .binary_search_by(|f| f.symbol.as_str().cmp(symbol))
            .ok()
            .map(|i| &self.functions[i])
    }
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
    pub fn identity(&self) -> DescriptorId {
        self.identity
    }
}
/// Parse, validate and canonicalize one bounded `.vsys` library.
pub fn parse(source: &str, limits: Limits) -> Result<LibraryDescriptor, Error> {
    parser::parse(source, limits.check()?)
}

fn canonicalize(library: &LibraryDescriptor, limit: usize) -> Result<Vec<u8>, Error> {
    struct Encoder {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Encoder {
        fn bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
            let length = u32::try_from(bytes.len())
                .map_err(|_| Error::new(ErrorKind::Limit, 0..0, "canonical field too large"))?;
            if self
                .bytes
                .len()
                .checked_add(4)
                .and_then(|n| n.checked_add(bytes.len()))
                .is_none_or(|n| n > self.limit)
            {
                return Err(Error::new(
                    ErrorKind::Limit,
                    0..0,
                    "canonical descriptor budget exceeded",
                ));
            }
            self.bytes.extend_from_slice(&length.to_le_bytes());
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn text(&mut self, text: &str) -> Result<(), Error> {
            self.bytes(text.as_bytes())
        }
        fn count(&mut self, count: usize) -> Result<(), Error> {
            self.bytes(&(count as u64).to_le_bytes())
        }
        fn optional(&mut self, text: Option<&str>) -> Result<(), Error> {
            self.text(if text.is_some() { "some" } else { "none" })?;
            if let Some(t) = text {
                self.text(t)?;
            }
            Ok(())
        }
        fn value(&mut self, t: &ValueType) -> Result<(), Error> {
            match t {
                ValueType::Unit => self.text("unit"),
                ValueType::Scalar(s) => {
                    self.text("scalar")?;
                    self.text(s.name())
                }
                ValueType::Bool => self.text("bool-u8"),
                ValueType::Enum(e) => {
                    self.text("enum")?;
                    self.text(e.name())
                }
                ValueType::Address { pointee, nullable } => {
                    self.text(if *nullable {
                        "nullable-address"
                    } else {
                        "address"
                    })?;
                    self.text(pointee.name())
                }
            }
        }
    }
    let mut e = Encoder {
        bytes: vec![],
        limit,
    };
    e.text(SCHEMA)?;
    e.text(&library.name)?;
    e.text(library.target.triple())?;
    e.text(library.target.convention().name())?;
    e.text("pointer64-little-endian")?;
    e.text(match library.link.mode {
        LinkMode::Static => "static",
        LinkMode::Dynamic => "dynamic",
    })?;
    e.text(&library.link.name)?;
    e.bytes(&library.link.artifact_blake3)?;
    e.optional(library.link.header.as_deref())?;
    e.optional(library.link.pkg_config.as_deref())?;
    e.count(library.enums.len())?;
    for enumeration in &library.enums {
        e.text(enumeration.name())?;
        e.text(enumeration.scalar().name())?;
        e.count(enumeration.variants().len())?;
        for (name, value) in enumeration.variants() {
            e.text(name)?;
            e.bytes(&value.to_le_bytes())?;
        }
    }
    e.count(library.functions.len())?;
    for f in &library.functions {
        e.text(&f.symbol)?;
        e.count(f.parameters.len())?;
        for p in &f.parameters {
            e.text(&p.name)?;
            match &p.ty {
                ParameterType::Value(t) => {
                    e.text("value")?;
                    e.value(t)?;
                }
                ParameterType::Slice {
                    element,
                    access,
                    nullable,
                } => {
                    let access = match access {
                        Access::In => "in",
                        Access::Inout => "inout",
                    };
                    match (element, nullable) {
                        // The original form keeps its exact canonical bytes.
                        (Pointee::Scalar(s), false) => {
                            e.text("nocapture-slice")?;
                            e.text(access)?;
                            e.text(s.name())?;
                        }
                        _ => {
                            e.text("nocapture-slice/2")?;
                            e.text(access)?;
                            e.text(if *nullable { "option" } else { "nonnull" })?;
                            match element {
                                Pointee::Scalar(s) => {
                                    e.text("scalar")?;
                                    e.text(s.name())?;
                                }
                                Pointee::Struct(w) => {
                                    e.text("wire-struct")?;
                                    e.text(w.name())?;
                                }
                            }
                        }
                    }
                }
                ParameterType::Pointer {
                    pointee,
                    access,
                    nullable,
                } => {
                    e.text("nocapture-pointer")?;
                    e.text(access.keyword())?;
                    e.text(if *nullable { "option" } else { "nonnull" })?;
                    match pointee {
                        Pointee::Scalar(s) => {
                            e.text("scalar")?;
                            e.text(s.name())?;
                        }
                        Pointee::Struct(w) => {
                            e.text("wire-struct")?;
                            e.text(w.name())?;
                        }
                    }
                }
            }
        }
        e.value(&f.result)?;
    }
    // Appended only when present, so descriptors without wire structs keep
    // the exact canonical bytes (and identities) of the earlier schema.
    if !library.wire_structs.is_empty() {
        e.text("wire-structs")?;
        e.count(library.wire_structs.len())?;
        for w in &library.wire_structs {
            let d = w.declaration();
            e.text(&d.name)?;
            e.bytes(&(w.layout().size() as u64).to_le_bytes())?;
            e.bytes(&(w.layout().alignment() as u64).to_le_bytes())?;
            e.count(d.fields.len())?;
            for field in &d.fields {
                e.text(&field.name)?;
                match &field.ty {
                    crate::WireType::Integer(i) => {
                        e.text("integer")?;
                        e.text(if i.signed { "i" } else { "u" })?;
                        e.bytes(&i.bits.to_le_bytes())?;
                        e.text(match i.endian {
                            crate::Endian::Native => "native",
                            crate::Endian::Little => "le",
                            crate::Endian::Big => "be",
                        })?;
                    }
                    crate::WireType::ByteArray(n) => {
                        e.text("bytes")?;
                        e.bytes(&(*n as u64).to_le_bytes())?;
                    }
                    crate::WireType::ZeroPad(n) => {
                        e.text("zero-pad")?;
                        e.bytes(&(*n as u64).to_le_bytes())?;
                    }
                    crate::WireType::Struct(n) => {
                        e.text("struct")?;
                        e.text(n)?;
                    }
                    other => {
                        return Err(Error::new(
                            ErrorKind::Type,
                            0..0,
                            format!("unexpected wire field type {other:?}"),
                        ));
                    }
                }
            }
        }
    }
    Ok(e.bytes)
}
fn unique<'a>(values: impl IntoIterator<Item = &'a str>, message: &str) -> Result<(), Error> {
    let mut names = BTreeSet::new();
    for n in values {
        if !names.insert(n) {
            return Err(Error::new(ErrorKind::Duplicate, 0..0, message));
        }
    }
    Ok(())
}
