//! Vesper Bridge wire-layout validation and safe byte-oriented views.
//!
//! Explicit-endian integers are byte-aligned wire wrappers. Native-endian
//! integers retain natural alignment. This distinction permits the specified
//! 61-byte Kafka header without permitting unaligned Rust integer references.
//! Layout compilation rejects implicit internal and tail padding. Successful
//! view construction performs only length/alignment checks and allocates no
//! memory; padding canonicality is a separate explicit validation operation.
#![forbid(unsafe_code)]

pub mod sys;

use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Native,
    Little,
    Big,
}
impl Endian {
    fn little(self) -> bool {
        self == Self::Little || (self == Self::Native && cfg!(target_endian = "little"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Integer {
    pub bits: u16,
    pub signed: bool,
    pub endian: Endian,
}
impl Integer {
    pub const fn unsigned(bits: u16, endian: Endian) -> Self {
        Self {
            bits,
            signed: false,
            endian,
        }
    }
    pub const fn signed(bits: u16, endian: Endian) -> Self {
        Self {
            bits,
            signed: true,
            endian,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireType {
    Integer(Integer),
    ByteArray(usize),
    ZeroPad(usize),
    Struct(String),
    Bool,
    SparseEnum,
    Unsupported(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: WireType,
}
impl Field {
    pub fn new(name: impl Into<String>, ty: WireType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub fields: Vec<Field>,
}
impl Declaration {
    pub fn new(name: impl Into<String>, fields: Vec<Field>) -> Self {
        Self {
            name: name.into(),
            fields,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutErrorKind {
    DuplicateDeclaration,
    DuplicateField,
    EmptyName,
    UnknownStruct(String),
    RecursiveStruct(Vec<String>),
    DepthLimit,
    ComplexityLimit,
    InvalidIntegerWidth(u16),
    UnsupportedType(String),
    ImplicitPadding {
        offset: usize,
        alignment: usize,
        suggested_bytes: usize,
    },
    SizeOverflow,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError {
    pub record: String,
    pub field: Option<String>,
    pub kind: LayoutErrorKind,
}
impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wire layout {}", self.record)?;
        if let Some(field) = &self.field {
            write!(f, ".{field}")?;
        }
        match &self.kind {
            LayoutErrorKind::ImplicitPadding {
                offset,
                alignment,
                suggested_bytes,
            } => write!(
                f,
                ": offset {offset} requires alignment {alignment}; add explicit ZeroPad[{suggested_bytes}]"
            ),
            other => write!(f, ": {other:?}"),
        }
    }
}
impl std::error::Error for LayoutError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Integer(Integer),
    Bytes,
    Padding,
    Struct(Box<WireLayout>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    name: String,
    offset: usize,
    size: usize,
    alignment: usize,
    kind: FieldKind,
}
impl FieldLayout {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn alignment(&self) -> usize {
        self.alignment
    }
    pub fn kind(&self) -> &FieldKind {
        &self.kind
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireLayout {
    name: String,
    size: usize,
    alignment: usize,
    fields: Vec<FieldLayout>,
}
impl WireLayout {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn alignment(&self) -> usize {
        self.alignment
    }
    pub fn fields(&self) -> &[FieldLayout] {
        &self.fields
    }
    pub fn field(&self, name: &str) -> Option<&FieldLayout> {
        self.fields.iter().find(|f| f.name == name)
    }
    fn locate(&self, path: &[&str]) -> Result<(usize, &FieldLayout), ViewError> {
        let mut layout = self;
        let mut offset = 0;
        let Some((last, parents)) = path.split_last() else {
            return Err(ViewError::EmptyPath);
        };
        for name in parents {
            let field = layout
                .field(name)
                .ok_or_else(|| ViewError::UnknownField((*name).into()))?;
            offset += field.offset;
            let FieldKind::Struct(nested) = &field.kind else {
                return Err(ViewError::ExpectedStruct);
            };
            layout = nested;
        }
        let field = layout
            .field(last)
            .ok_or_else(|| ViewError::UnknownField((*last).into()))?;
        Ok((offset + field.offset, field))
    }
    /// Padding bytes have no value semantics. This comparison ignores them,
    /// including nonzero padding in externally supplied initialized byte slices.
    pub fn semantic_eq(&self, left: &[u8], right: &[u8]) -> Result<bool, ViewError> {
        let left = view_wire(self, left)?;
        let right = view_wire(self, right)?;
        Ok(self.equal_bytes(left.bytes, right.bytes))
    }
    fn equal_bytes(&self, left: &[u8], right: &[u8]) -> bool {
        self.fields.iter().all(|field| {
            let range = field.offset..field.offset + field.size;
            match &field.kind {
                FieldKind::Padding => true,
                FieldKind::Struct(nested) => {
                    nested.equal_bytes(&left[range.clone()], &right[range])
                }
                _ => left[range.clone()] == right[range],
            }
        })
    }
    fn check_bytes(&self, bytes: &[u8]) -> Result<(), ViewError> {
        if bytes.len() < self.size {
            return Err(ViewError::ShortBuffer {
                needed: self.size,
                actual: bytes.len(),
            });
        }
        if !(bytes.as_ptr() as usize).is_multiple_of(self.alignment) {
            return Err(ViewError::Misaligned {
                required: self.alignment,
            });
        }
        Ok(())
    }
    fn check_padding(&self, bytes: &[u8], base: usize) -> Result<(), ViewError> {
        for field in &self.fields {
            let range = field.offset..field.offset + field.size;
            match &field.kind {
                FieldKind::Padding => {
                    if let Some(index) = bytes[range.clone()].iter().position(|b| *b != 0) {
                        return Err(ViewError::NonZeroPadding {
                            offset: base + field.offset + index,
                        });
                    }
                }
                FieldKind::Struct(nested) => {
                    nested.check_padding(&bytes[range], base + field.offset)?
                }
                _ => (),
            }
        }
        Ok(())
    }
}

/// Nominal declarations are checked together; recursive wire storage is rejected.
pub struct Registry {
    declarations: BTreeMap<String, Declaration>,
}
impl Registry {
    pub fn new(declarations: impl IntoIterator<Item = Declaration>) -> Result<Self, LayoutError> {
        let mut result = BTreeMap::new();
        for declaration in declarations {
            if declaration.name.is_empty() {
                return Err(LayoutError {
                    record: declaration.name,
                    field: None,
                    kind: LayoutErrorKind::EmptyName,
                });
            }
            if result.contains_key(&declaration.name) {
                return Err(LayoutError {
                    record: declaration.name,
                    field: None,
                    kind: LayoutErrorKind::DuplicateDeclaration,
                });
            }
            result.insert(declaration.name.clone(), declaration);
        }
        Ok(Self {
            declarations: result,
        })
    }
    pub fn compile(&self, root: &str) -> Result<WireLayout, LayoutError> {
        self.visit(root, &mut Vec::new(), &mut 65_536)
    }
    fn visit(
        &self,
        name: &str,
        visiting: &mut Vec<String>,
        budget: &mut usize,
    ) -> Result<WireLayout, LayoutError> {
        let error = |field: Option<&str>, kind| LayoutError {
            record: name.into(),
            field: field.map(str::to_owned),
            kind,
        };
        if visiting.iter().any(|n| n == name) {
            let mut cycle = visiting.clone();
            cycle.push(name.into());
            return Err(error(None, LayoutErrorKind::RecursiveStruct(cycle)));
        }
        if visiting.len() >= 128 {
            return Err(error(None, LayoutErrorKind::DepthLimit));
        }
        let declaration = self
            .declarations
            .get(name)
            .ok_or_else(|| error(None, LayoutErrorKind::UnknownStruct(name.into())))?;
        visiting.push(name.into());
        let mut size = 0usize;
        let mut alignment = 1usize;
        let mut fields = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for field in &declaration.fields {
            if *budget == 0 {
                return Err(error(Some(&field.name), LayoutErrorKind::ComplexityLimit));
            }
            *budget -= 1;
            if field.name.is_empty() {
                return Err(error(Some(&field.name), LayoutErrorKind::EmptyName));
            }
            if !names.insert(&field.name) {
                return Err(error(Some(&field.name), LayoutErrorKind::DuplicateField));
            }
            let (field_size, field_align, kind) = match &field.ty {
                WireType::Integer(integer) => {
                    if !matches!(integer.bits, 8 | 16 | 32 | 64) {
                        return Err(error(
                            Some(&field.name),
                            LayoutErrorKind::InvalidIntegerWidth(integer.bits),
                        ));
                    }
                    let width = integer.bits as usize / 8;
                    (
                        width,
                        if integer.endian == Endian::Native {
                            width
                        } else {
                            1
                        },
                        FieldKind::Integer(*integer),
                    )
                }
                WireType::ByteArray(n) => (*n, 1, FieldKind::Bytes),
                WireType::ZeroPad(n) => (*n, 1, FieldKind::Padding),
                WireType::Struct(n) => {
                    let layout = self.visit(n, visiting, budget)?;
                    (
                        layout.size,
                        layout.alignment,
                        FieldKind::Struct(Box::new(layout)),
                    )
                }
                WireType::Bool => {
                    return Err(error(
                        Some(&field.name),
                        LayoutErrorKind::UnsupportedType(
                            "bool does not admit every bit pattern".into(),
                        ),
                    ));
                }
                WireType::SparseEnum => {
                    return Err(error(
                        Some(&field.name),
                        LayoutErrorKind::UnsupportedType(
                            "sparse enum does not admit every bit pattern".into(),
                        ),
                    ));
                }
                WireType::Unsupported(n) => {
                    return Err(error(
                        Some(&field.name),
                        LayoutErrorKind::UnsupportedType(n.clone()),
                    ));
                }
            };
            if !size.is_multiple_of(field_align) {
                return Err(error(
                    Some(&field.name),
                    LayoutErrorKind::ImplicitPadding {
                        offset: size,
                        alignment: field_align,
                        suggested_bytes: field_align - size % field_align,
                    },
                ));
            }
            let offset = size;
            size = size
                .checked_add(field_size)
                .ok_or_else(|| error(Some(&field.name), LayoutErrorKind::SizeOverflow))?;
            alignment = alignment.max(field_align);
            fields.push(FieldLayout {
                name: field.name.clone(),
                offset,
                size: field_size,
                alignment: field_align,
                kind,
            });
        }
        if !size.is_multiple_of(alignment) {
            return Err(error(
                None,
                LayoutErrorKind::ImplicitPadding {
                    offset: size,
                    alignment,
                    suggested_bytes: alignment - size % alignment,
                },
            ));
        }
        visiting.pop();
        Ok(WireLayout {
            name: name.into(),
            size,
            alignment,
            fields,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    ShortBuffer { needed: usize, actual: usize },
    Misaligned { required: usize },
    EmptyPath,
    UnknownField(String),
    ExpectedStruct,
    ExpectedInteger,
    ExpectedBytes,
    SignednessMismatch,
    OutOfRange,
    NonZeroPadding { offset: usize },
}
impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wire view: {self:?}")
    }
}
impl std::error::Error for ViewError {}

#[derive(Clone, Copy)]
pub struct WireView<'layout, 'bytes> {
    layout: &'layout WireLayout,
    bytes: &'bytes [u8],
}
pub struct WireViewMut<'layout, 'bytes> {
    layout: &'layout WireLayout,
    bytes: &'bytes mut [u8],
}
/// Borrow only the checked header prefix; extra bytes are not part of the view.
pub fn view_wire<'l, 'b>(
    layout: &'l WireLayout,
    bytes: &'b [u8],
) -> Result<WireView<'l, 'b>, ViewError> {
    layout.check_bytes(bytes)?;
    Ok(WireView {
        layout,
        bytes: &bytes[..layout.size],
    })
}
pub fn split_wire<'l, 'b>(
    layout: &'l WireLayout,
    bytes: &'b [u8],
) -> Result<(WireView<'l, 'b>, &'b [u8]), ViewError> {
    layout.check_bytes(bytes)?;
    let (header, payload) = bytes.split_at(layout.size);
    Ok((
        WireView {
            layout,
            bytes: header,
        },
        payload,
    ))
}
pub fn split_wire_mut<'l, 'b>(
    layout: &'l WireLayout,
    bytes: &'b mut [u8],
) -> Result<(WireViewMut<'l, 'b>, &'b mut [u8]), ViewError> {
    layout.check_bytes(bytes)?;
    let (header, payload) = bytes.split_at_mut(layout.size);
    Ok((
        WireViewMut {
            layout,
            bytes: header,
        },
        payload,
    ))
}
/// Construct an all-zero valid value, including explicit padding, in place.
pub fn initialize_wire<'l, 'b>(
    layout: &'l WireLayout,
    bytes: &'b mut [u8],
) -> Result<(WireViewMut<'l, 'b>, &'b mut [u8]), ViewError> {
    let (view, payload) = split_wire_mut(layout, bytes)?;
    view.bytes.fill(0);
    Ok((view, payload))
}
impl<'l, 'b> WireView<'l, 'b> {
    pub fn layout(&self) -> &'l WireLayout {
        self.layout
    }
    pub fn as_bytes(&self) -> &'b [u8] {
        self.bytes
    }
    pub fn validate_padding(&self) -> Result<(), ViewError> {
        self.layout.check_padding(self.bytes, 0)
    }
    pub fn bytes(&self, path: &[&str]) -> Result<&'b [u8], ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        if field.kind != FieldKind::Bytes {
            return Err(ViewError::ExpectedBytes);
        }
        Ok(&self.bytes[offset..offset + field.size])
    }
    pub fn project(&self, path: &[&str]) -> Result<WireView<'l, 'b>, ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        let FieldKind::Struct(layout) = &field.kind else {
            return Err(ViewError::ExpectedStruct);
        };
        view_wire(layout, &self.bytes[offset..offset + field.size])
    }
    fn integer(&self, path: &[&str]) -> Result<(Integer, u64), ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        let FieldKind::Integer(integer) = field.kind else {
            return Err(ViewError::ExpectedInteger);
        };
        let bytes = &self.bytes[offset..offset + field.size];
        let mut value = 0;
        for (i, byte) in bytes.iter().enumerate() {
            let shift = if integer.endian.little() {
                i
            } else {
                bytes.len() - i - 1
            } * 8;
            value |= u64::from(*byte) << shift;
        }
        Ok((integer, value))
    }
    pub fn read_u64(&self, path: &[&str]) -> Result<u64, ViewError> {
        let (integer, value) = self.integer(path)?;
        if integer.signed {
            Err(ViewError::SignednessMismatch)
        } else {
            Ok(value)
        }
    }
    pub fn read_i64(&self, path: &[&str]) -> Result<i64, ViewError> {
        let (integer, value) = self.integer(path)?;
        if !integer.signed {
            return Err(ViewError::SignednessMismatch);
        }
        let shift = 64 - integer.bits;
        Ok(((value << shift) as i64) >> shift)
    }
}
impl<'l> WireViewMut<'l, '_> {
    pub fn layout(&self) -> &'l WireLayout {
        self.layout
    }
    pub fn as_view(&self) -> WireView<'l, '_> {
        WireView {
            layout: self.layout,
            bytes: self.bytes,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes
    }
    pub fn validate_padding(&self) -> Result<(), ViewError> {
        self.as_view().validate_padding()
    }
    pub fn read_u64(&self, path: &[&str]) -> Result<u64, ViewError> {
        self.as_view().read_u64(path)
    }
    pub fn read_i64(&self, path: &[&str]) -> Result<i64, ViewError> {
        self.as_view().read_i64(path)
    }
    pub fn bytes_mut(&mut self, path: &[&str]) -> Result<&mut [u8], ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        if field.kind != FieldKind::Bytes {
            return Err(ViewError::ExpectedBytes);
        }
        Ok(&mut self.bytes[offset..offset + field.size])
    }
    pub fn project_mut(&mut self, path: &[&str]) -> Result<WireViewMut<'l, '_>, ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        let FieldKind::Struct(layout) = &field.kind else {
            return Err(ViewError::ExpectedStruct);
        };
        let (view, _) = split_wire_mut(layout, &mut self.bytes[offset..offset + field.size])?;
        Ok(view)
    }
    fn write_integer(&mut self, offset: usize, integer: Integer, value: u64) {
        let size = integer.bits as usize / 8;
        for i in 0..size {
            let shift = if integer.endian.little() {
                i
            } else {
                size - i - 1
            } * 8;
            self.bytes[offset + i] = (value >> shift) as u8;
        }
    }
    pub fn write_u64(&mut self, path: &[&str], value: u64) -> Result<(), ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        let FieldKind::Integer(integer) = field.kind else {
            return Err(ViewError::ExpectedInteger);
        };
        if integer.signed {
            return Err(ViewError::SignednessMismatch);
        }
        if integer.bits < 64 && value >= 1u64 << integer.bits {
            return Err(ViewError::OutOfRange);
        }
        self.write_integer(offset, integer, value);
        Ok(())
    }
    pub fn write_i64(&mut self, path: &[&str], value: i64) -> Result<(), ViewError> {
        let (offset, field) = self.layout.locate(path)?;
        let FieldKind::Integer(integer) = field.kind else {
            return Err(ViewError::ExpectedInteger);
        };
        if !integer.signed {
            return Err(ViewError::SignednessMismatch);
        }
        let shift = 64 - integer.bits;
        if (value << shift) >> shift != value {
            return Err(ViewError::OutOfRange);
        }
        self.write_integer(offset, integer, value as u64);
        Ok(())
    }
}
