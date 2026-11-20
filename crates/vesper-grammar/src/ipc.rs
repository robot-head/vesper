//! Actual Arrow IPC streams: metadata V4/V5, flat signed i64 fields, no
//! dictionaries, compression, nested arrays, variadic buffers or custom metadata.
//! FlatBuffer objects/offsets and stream framing are checked before borrowed
//! columns are published. Padding is opaque (Arrow IPC is not canonical).
//! Mandatory relative eight-byte IPC alignment is independent of optional
//! relative/host-address 64-byte suitability. No unsafe casts or SIMD occur.
#[cfg(not(vesper_port))]
use crate::arrow::{self, BufferDesc, FieldNode};
use crate::arrow::{AlignmentProfile, I64Column};
#[cfg(not(vesper_port))]
use core::mem::size_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Configuration,
    InputLimit,
    MetadataLimit,
    CountLimit,
    NameLimit,
    DepthLimit,
    WorkLimit,
    AllocationLimit,
    AllocationFailed,
    Overflow,
    UnexpectedEof,
    Framing,
    Flatbuffer,
    Alignment,
    Aliasing,
    Utf8,
    Schema,
    MissingSchema,
    UnsupportedVersion,
    UnsupportedHeader,
    UnsupportedType,
    UnsupportedEndian,
    UnsupportedDictionary,
    UnsupportedCompression,
    UnsupportedMetadata,
    UnsupportedFeature,
    UnsupportedNested,
    UnsupportedVariadic,
    Descriptor,
    NonNullable,
    TrailingBytes,
    Column(crate::ErrorKind),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = core::result::Result<T, Error>;
fn err(kind: ErrorKind, offset: usize) -> Error {
    Error { kind, offset }
}
#[cfg(not(vesper_port))]
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(err(ErrorKind::Overflow, a))
}
#[cfg(not(vesper_port))]
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(err(ErrorKind::Overflow, a))
}
#[cfg(not(vesper_port))]
fn nonnegative(n: i64, at: usize) -> Result<usize> {
    usize::try_from(n).map_err(|_| err(ErrorKind::Descriptor, at))
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_total_metadata_bytes: usize,
    pub max_fields: usize,
    pub max_batches: usize,
    pub max_rows_per_batch: usize,
    pub max_total_rows: usize,
    pub max_name_bytes: usize,
    pub max_metadata_objects: usize,
    pub max_depth: usize,
    /// Abstract byte/element/overlap work units, not CPU instructions or time.
    pub max_work: usize,
    /// Cumulative requested metadata vector payload capacities, not exact RSS.
    pub max_allocation_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 32 * 1024 * 1024,
            max_metadata_bytes: 1024 * 1024,
            max_total_metadata_bytes: 4 * 1024 * 1024,
            max_fields: 64,
            max_batches: 256,
            max_rows_per_batch: 1_048_576,
            max_total_rows: 4_194_304,
            max_name_bytes: 256,
            max_metadata_objects: 2048,
            max_depth: 8,
            max_work: 64 * 1024 * 1024,
            max_allocation_bytes: 8 * 1024 * 1024,
        }
    }
}
/// Ceilings [`Limits::validate`] enforces.
const HARD_INPUT_BYTES: usize = 128 * 1024 * 1024;
const HARD_METADATA_BYTES: usize = 4 * 1024 * 1024;
const HARD_TOTAL_METADATA_BYTES: usize = 16 * 1024 * 1024;
const HARD_FIELDS: usize = 1024;
const HARD_BATCHES: usize = 4096;
const HARD_ROWS_PER_BATCH: usize = 8_388_608;
const HARD_TOTAL_ROWS: usize = 16_777_216;
const HARD_NAME_BYTES: usize = 4096;
const HARD_METADATA_OBJECTS: usize = 8192;
const HARD_WORK: usize = 512 * 1024 * 1024;
const HARD_ALLOCATION_BYTES: usize = 64 * 1024 * 1024;
const HARD_DEPTH: usize = 32;
/// FlatBuffers vtables: two `u16` sizes, then at most `(MAX_VTABLE_BYTES -
/// 4) / 2` field slots (Arrow's tables have at most 32 fields).
const VTABLE_HEADER_BYTES: usize = 4;
const MAX_VTABLE_BYTES: usize = 68;
/// Arrow `Message.fbs` structs: `FieldNode` and `Buffer` are two `i64`s,
/// aligned to eight bytes; body buffers are eight-byte aligned, and the
/// 64-byte profiles check `ALIGN_64` (Arrow columnar format, "Buffer
/// Alignment and Padding").
const I64_BYTES: usize = 8;
const STRUCT_ALIGN: usize = 8;
const FIELD_NODE_BYTES: usize = 16;
const BUFFER_BYTES: usize = 16;
const BUFFER_ALIGN: usize = 8;
const ALIGN_64: usize = 64;
/// The only integer columns read are 64-bit signed (`Int { bitWidth: 64,
/// is_signed: true }`).
const INT64_BITS: i32 = 64;
/// The most `Schema.features` entries read.
const MAX_FEATURES: usize = 64;
/// Arrow IPC encapsulated messages (columnar format, "Encapsulated message
/// format"): a `0xFFFFFFFF` continuation marker and a u32 metadata length,
/// or (before 0.15) the length alone.
#[cfg_attr(vesper_port, allow(dead_code))]
const CONTINUATION: u32 = u32::MAX;
#[cfg_attr(vesper_port, allow(dead_code))]
const U32_BYTES: usize = 4;
#[cfg_attr(vesper_port, allow(dead_code))]
const LEGACY_PREFIX_BYTES: usize = 4;
#[cfg_attr(vesper_port, allow(dead_code))]
const MODERN_PREFIX_BYTES: usize = 8;
/// `Schema.fbs` `MetadataVersion` values read: V4 and V5.
#[cfg_attr(vesper_port, allow(dead_code))]
const METADATA_V4: u16 = 3;
#[cfg_attr(vesper_port, allow(dead_code))]
const METADATA_V5: u16 = 4;
/// `Message.fbs` `MessageHeader` union tags.
#[cfg_attr(vesper_port, allow(dead_code))]
const HEADER_SCHEMA: u8 = 1;
#[cfg_attr(vesper_port, allow(dead_code))]
const HEADER_DICTIONARY_BATCH: u8 = 2;
#[cfg_attr(vesper_port, allow(dead_code))]
const HEADER_RECORD_BATCH: u8 = 3;
impl Limits {
    pub fn validate(self) -> Result<()> {
        for (value, hard) in [
            (self.max_input_bytes, HARD_INPUT_BYTES),
            (self.max_metadata_bytes, HARD_METADATA_BYTES),
            (self.max_total_metadata_bytes, HARD_TOTAL_METADATA_BYTES),
            (self.max_fields, HARD_FIELDS),
            (self.max_batches, HARD_BATCHES),
            (self.max_rows_per_batch, HARD_ROWS_PER_BATCH),
            (self.max_total_rows, HARD_TOTAL_ROWS),
            (self.max_name_bytes, HARD_NAME_BYTES),
            (self.max_metadata_objects, HARD_METADATA_OBJECTS),
            (self.max_work, HARD_WORK),
            (self.max_allocation_bytes, HARD_ALLOCATION_BYTES),
        ] {
            if value > hard {
                return Err(err(ErrorKind::Configuration, 0));
            }
        }
        if self.max_depth == 0 || self.max_depth > HARD_DEPTH {
            return Err(err(ErrorKind::Configuration, 0));
        }
        Ok(())
    }
}
#[cfg(not(vesper_port))]
struct Budget {
    limits: Limits,
    remaining: usize,
    allocation: usize,
    metadata: usize,
}
#[cfg(not(vesper_port))]
impl Budget {
    fn work(&mut self, n: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or(err(ErrorKind::WorkLimit, 0))?;
        Ok(())
    }
    fn reserve<T>(&mut self, count: usize) -> Result<Vec<T>> {
        let bytes = mul(count, size_of::<T>())?;
        self.allocation = add(self.allocation, bytes)?;
        if self.allocation > self.limits.max_allocation_bytes {
            return Err(err(ErrorKind::AllocationLimit, 0));
        }
        self.work(bytes)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(count)
            .map_err(|_| err(ErrorKind::AllocationFailed, 0))?;
        Ok(result)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> {
    name: &'a str,
    nullable: bool,
}
impl<'a> Field<'a> {
    pub fn name(self) -> &'a str {
        self.name
    }
    pub fn nullable(self) -> bool {
        self.nullable
    }
}
#[derive(Debug)]
pub struct Schema<'a> {
    fields: Vec<Field<'a>>,
    version: u16,
}
impl<'a> Schema<'a> {
    pub fn fields(&self) -> &[Field<'a>] {
        &self.fields
    }
    pub fn metadata_version(&self) -> u16 {
        self.version
    }
}
#[derive(Debug)]
pub struct Batch<'a> {
    rows: usize,
    columns: Vec<I64Column<'a>>,
    body: &'a [u8],
    version: u16,
}
impl<'a> Batch<'a> {
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn columns(&self) -> &[I64Column<'a>] {
        &self.columns
    }
    pub fn body_bytes(&self) -> &'a [u8] {
        self.body
    }
    pub fn metadata_version(&self) -> u16 {
        self.version
    }
}
#[derive(Debug)]
pub struct Stream<'a> {
    schema: Schema<'a>,
    batches: Vec<Batch<'a>>,
    work_used: usize,
    metadata_allocation_bytes: usize,
}
impl<'a> Stream<'a> {
    pub fn schema(&self) -> &Schema<'a> {
        &self.schema
    }
    pub fn batches(&self) -> &[Batch<'a>] {
        &self.batches
    }
    pub fn work_used(&self) -> usize {
        self.work_used
    }
    pub fn metadata_allocation_bytes(&self) -> usize {
        self.metadata_allocation_bytes
    }
}
#[derive(Debug)]
pub struct Parsed<'a> {
    stream: Stream<'a>,
    consumed: usize,
    tail: &'a [u8],
}
impl<'a> Parsed<'a> {
    pub fn stream(&self) -> &Stream<'a> {
        &self.stream
    }
    pub fn into_stream(self) -> Stream<'a> {
        self.stream
    }
    pub fn consumed(&self) -> usize {
        self.consumed
    }
    pub fn tail(&self) -> &'a [u8] {
        self.tail
    }
}

#[cfg(not(vesper_port))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Message,
    Schema,
    Field,
    Int,
    RecordBatch,
}
#[cfg(not(vesper_port))]
impl Kind {
    fn width(self, index: usize) -> Option<usize> {
        let widths: &[usize] = match self {
            Self::Message => &[2, 1, 4, 8, 4],
            Self::Schema => &[2, 4, 4, 4],
            Self::Field => &[4, 1, 1, 4, 4, 4, 4],
            Self::Int => &[4, 1],
            Self::RecordBatch => &[8, 4, 4, 4, 4],
        };
        widths.get(index).copied()
    }
}
#[cfg(not(vesper_port))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Root,
    Table(Kind),
    Vtable,
    Vector(usize),
    String,
}
#[cfg(not(vesper_port))]
#[derive(Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
    role: Role,
}
#[cfg(not(vesper_port))]
#[derive(Clone, Copy)]
struct Table {
    pos: usize,
    vtable: usize,
    slots: usize,
}
#[cfg(not(vesper_port))]
struct Fb<'a, 'b> {
    bytes: &'a [u8],
    base: usize,
    budget: &'b mut Budget,
    regions: Vec<Region>,
}
#[cfg(not(vesper_port))]
impl<'a, 'b> Fb<'a, 'b> {
    fn new(bytes: &'a [u8], base: usize, budget: &'b mut Budget) -> Result<Self> {
        let cap = budget.limits.max_metadata_objects.min(bytes.len() / 4 + 1);
        let regions = budget.reserve(cap)?;
        let mut result = Self {
            bytes,
            base,
            budget,
            regions,
        };
        result.claim(0, 4, Role::Root)?;
        Ok(result)
    }
    fn fail(&self, kind: ErrorKind, at: usize) -> Error {
        err(kind, self.base.checked_add(at).unwrap_or(self.base))
    }
    fn bytes(&mut self, at: usize, len: usize) -> Result<&'a [u8]> {
        let end = add(at, len)?;
        self.budget.work(len)?;
        self.bytes
            .get(at..end)
            .ok_or_else(|| self.fail(ErrorKind::Flatbuffer, at))
    }
    fn u8(&mut self, at: usize) -> Result<u8> {
        Ok(self.bytes(at, 1)?[0])
    }
    fn u16(&mut self, at: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.bytes(at, 2)?.try_into().expect("two bytes"),
        ))
    }
    fn u32(&mut self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes(at, 4)?.try_into().expect("four bytes"),
        ))
    }
    fn i32(&mut self, at: usize) -> Result<i32> {
        Ok(self.u32(at)? as i32)
    }
    fn i64(&mut self, at: usize) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.bytes(at, 8)?.try_into().expect("eight bytes"),
        ))
    }
    fn claim(&mut self, start: usize, len: usize, role: Role) -> Result<()> {
        let end = add(start, len)?;
        if end > self.bytes.len() {
            return Err(self.fail(ErrorKind::Flatbuffer, start));
        }
        for region in &self.regions {
            self.budget.work(1)?;
            if start < region.end && region.start < end {
                if start == region.start
                    && end == region.end
                    && (role == region.role
                        || (len == 4
                            && matches!(role, Role::Vector(_))
                            && matches!(region.role, Role::Vector(_))))
                {
                    return Ok(());
                }
                return Err(self.fail(ErrorKind::Aliasing, start));
            }
        }
        if self.regions.len() >= self.budget.limits.max_metadata_objects
            || self.regions.len() >= self.regions.capacity()
        {
            return Err(self.fail(ErrorKind::CountLimit, start));
        }
        self.regions.push(Region { start, end, role });
        Ok(())
    }
    fn offset(&mut self, at: usize) -> Result<usize> {
        let value = self.u32(at)? as usize;
        if value < 4 {
            return Err(self.fail(ErrorKind::Flatbuffer, at));
        }
        let target = add(at, value)?;
        if !target.is_multiple_of(4) || add(target, 4)? > self.bytes.len() {
            return Err(self.fail(ErrorKind::Flatbuffer, at));
        }
        Ok(target)
    }
    fn table(&mut self, pos: usize, kind: Kind, depth: usize) -> Result<Table> {
        if depth > self.budget.limits.max_depth {
            return Err(self.fail(ErrorKind::DepthLimit, pos));
        }
        if !pos.is_multiple_of(4) {
            return Err(self.fail(ErrorKind::Alignment, pos));
        }
        let distance = self.i32(pos)?;
        let vtable = if distance < 0 {
            add(pos, distance.unsigned_abs() as usize)?
        } else {
            pos.checked_sub(distance as usize)
                .ok_or_else(|| self.fail(ErrorKind::Flatbuffer, pos))?
        };
        if !vtable.is_multiple_of(2) {
            return Err(self.fail(ErrorKind::Alignment, vtable));
        }
        let length = self.u16(vtable)? as usize;
        let object = self.u16(add(vtable, 2)?)? as usize;
        if length < VTABLE_HEADER_BYTES
            || !length.is_multiple_of(2)
            || length > MAX_VTABLE_BYTES
            || object < VTABLE_HEADER_BYTES
        {
            return Err(self.fail(ErrorKind::Flatbuffer, vtable));
        }
        self.claim(vtable, length, Role::Vtable)?;
        self.claim(pos, object, Role::Table(kind))?;
        let slots = (length - 4) / 2;
        for index in 0..slots {
            let off = self.u16(vtable + 4 + index * 2)? as usize;
            if off == 0 {
                continue;
            }
            let width = kind
                .width(index)
                .ok_or_else(|| self.fail(ErrorKind::UnsupportedMetadata, vtable + 4 + index * 2))?;
            if off < 4 || add(off, width)? > object || !add(pos, off)?.is_multiple_of(width.min(8))
            {
                return Err(self.fail(ErrorKind::Flatbuffer, pos + off));
            }
            for prior in 0..index {
                let previous = self.u16(vtable + 4 + prior * 2)? as usize;
                if previous != 0 {
                    let old = kind.width(prior).ok_or_else(|| {
                        self.fail(ErrorKind::UnsupportedMetadata, vtable + 4 + prior * 2)
                    })?;
                    if off < previous + old && previous < off + width {
                        return Err(self.fail(ErrorKind::Aliasing, pos + off));
                    }
                }
            }
        }
        Ok(Table { pos, vtable, slots })
    }
    fn field(&mut self, table: Table, index: usize) -> Result<Option<usize>> {
        if index >= table.slots {
            return Ok(None);
        }
        let offset = self.u16(table.vtable + 4 + index * 2)? as usize;
        Ok((offset != 0).then_some(table.pos + offset))
    }
    fn scalar8(&mut self, table: Table, index: usize, default: u8) -> Result<u8> {
        match self.field(table, index)? {
            Some(at) => self.u8(at),
            None => Ok(default),
        }
    }
    fn scalar16(&mut self, table: Table, index: usize, default: u16) -> Result<u16> {
        match self.field(table, index)? {
            Some(at) => self.u16(at),
            None => Ok(default),
        }
    }
    fn scalar64(&mut self, table: Table, index: usize, default: i64) -> Result<i64> {
        match self.field(table, index)? {
            Some(at) => self.i64(at),
            None => Ok(default),
        }
    }
    fn vector(
        &mut self,
        field: Option<usize>,
        width: usize,
        alignment: usize,
        max: usize,
    ) -> Result<(usize, usize)> {
        let Some(field) = field else {
            return Ok((0, 0));
        };
        let target = self.offset(field)?;
        let count = self.u32(target)? as usize;
        if count > max {
            return Err(self.fail(ErrorKind::CountLimit, target));
        }
        let start = add(target, 4)?;
        if count != 0 && !start.is_multiple_of(alignment) {
            return Err(self.fail(ErrorKind::Alignment, start));
        }
        self.claim(target, add(4, mul(count, width)?)?, Role::Vector(width))?;
        Ok((start, count))
    }
    fn empty_metadata(&mut self, table: Table, index: usize) -> Result<()> {
        let field = self.field(table, index)?;
        let (_, count) = self.vector(field, 4, 4, self.budget.limits.max_fields)?;
        if count != 0 {
            return Err(self.fail(ErrorKind::UnsupportedMetadata, table.pos));
        }
        Ok(())
    }
    fn string(&mut self, field: Option<usize>) -> Result<&'a str> {
        let Some(field) = field else {
            return Ok("");
        };
        let target = self.offset(field)?;
        let len = self.u32(target)? as usize;
        if len > self.budget.limits.max_name_bytes {
            return Err(self.fail(ErrorKind::NameLimit, target));
        }
        let start = add(target, 4)?;
        self.claim(target, add(5, len)?, Role::String)?;
        if self.u8(add(start, len)?)? != 0 {
            return Err(self.fail(ErrorKind::Flatbuffer, start + len));
        }
        let bytes = self.bytes(start, len)?;
        core::str::from_utf8(bytes).map_err(|_| self.fail(ErrorKind::Utf8, start))
    }
    fn root(&mut self) -> Result<Table> {
        let at = self.offset(0)?;
        self.table(at, Kind::Message, 1)
    }
    fn header(&mut self, message: Table, kind: Kind) -> Result<Table> {
        let field = self
            .field(message, 2)?
            .ok_or_else(|| self.fail(ErrorKind::Flatbuffer, message.pos))?;
        let at = self.offset(field)?;
        self.table(at, kind, 2)
    }
}

#[cfg(not(vesper_port))]
fn schema<'a>(fb: &mut Fb<'a, '_>, message: Table, version: u16) -> Result<Schema<'a>> {
    let table = fb.header(message, Kind::Schema)?;
    if fb.scalar16(table, 0, 0)? != 0 {
        return Err(fb.fail(ErrorKind::UnsupportedEndian, table.pos));
    }
    fb.empty_metadata(table, 2)?;
    let features = fb.field(table, 3)?;
    let (start, count) = fb.vector(features, I64_BYTES, STRUCT_ALIGN, MAX_FEATURES)?;
    for i in 0..count {
        if fb.i64(start + i * I64_BYTES)? != 0 {
            return Err(fb.fail(ErrorKind::UnsupportedFeature, start + i * 8));
        }
    }
    let fields = fb.field(table, 1)?;
    let (start, count) = fb.vector(fields, 4, 4, fb.budget.limits.max_fields)?;
    let mut fields = fb.budget.reserve(count)?;
    for i in 0..count {
        let at = fb.offset(start + i * 4)?;
        let field = fb.table(at, Kind::Field, 3)?;
        if fb.field(field, 4)?.is_some() {
            return Err(fb.fail(ErrorKind::UnsupportedDictionary, at));
        }
        let children = fb.field(field, 5)?;
        let (_, children) = fb.vector(children, 4, 4, fb.budget.limits.max_fields)?;
        if children != 0 {
            return Err(fb.fail(ErrorKind::UnsupportedNested, at));
        }
        fb.empty_metadata(field, 6)?;
        if fb.scalar8(field, 2, 0)? != 2 {
            return Err(fb.fail(ErrorKind::UnsupportedType, at));
        }
        let int = fb
            .field(field, 3)?
            .ok_or_else(|| fb.fail(ErrorKind::Flatbuffer, at))?;
        let int = fb.offset(int)?;
        let int = fb.table(int, Kind::Int, 4)?;
        let width = match fb.field(int, 0)? {
            Some(at) => fb.i32(at)?,
            None => 0,
        };
        if width != INT64_BITS || fb.scalar8(int, 1, 0)? != 1 {
            return Err(fb.fail(ErrorKind::UnsupportedType, int.pos));
        }
        let nullable = fb.scalar8(field, 1, 0)?;
        if nullable > 1 {
            return Err(fb.fail(ErrorKind::Flatbuffer, at));
        }
        let name = fb.field(field, 0)?;
        let name = fb.string(name)?;
        fields.push(Field {
            name,
            nullable: nullable == 1,
        });
    }
    Ok(Schema { fields, version })
}
#[cfg(not(vesper_port))]
fn batch<'a>(
    fb: &mut Fb<'a, '_>,
    message: Table,
    body_range: (&'a [u8], usize),
    schema: &Schema<'a>,
    version: u16,
    profile: AlignmentProfile,
    remaining_rows: usize,
) -> Result<Batch<'a>> {
    let (body, body_at) = body_range;
    let table = fb.header(message, Kind::RecordBatch)?;
    if fb.field(table, 3)?.is_some() {
        return Err(fb.fail(ErrorKind::UnsupportedCompression, table.pos));
    }
    let variadic = fb.field(table, 4)?;
    let (_, variadic) = fb.vector(variadic, 8, 8, fb.budget.limits.max_fields)?;
    if variadic != 0 {
        return Err(fb.fail(ErrorKind::UnsupportedVariadic, table.pos));
    }
    let rows = nonnegative(fb.scalar64(table, 0, 0)?, fb.base + table.pos)?;
    if rows > fb.budget.limits.max_rows_per_batch || rows > remaining_rows {
        return Err(fb.fail(ErrorKind::CountLimit, table.pos));
    }
    let nodes = fb.field(table, 1)?;
    let (node_start, node_count) = fb.vector(
        nodes,
        FIELD_NODE_BYTES,
        STRUCT_ALIGN,
        fb.budget.limits.max_fields,
    )?;
    let buffers = fb.field(table, 2)?;
    let (buffer_start, buffer_count) = fb.vector(
        buffers,
        BUFFER_BYTES,
        STRUCT_ALIGN,
        mul(fb.budget.limits.max_fields, 2)?,
    )?;
    if node_count != schema.fields.len()
        || buffer_count != mul(schema.fields.len(), 2)?
        || (schema.fields.is_empty() && !body.is_empty())
    {
        return Err(fb.fail(ErrorKind::Descriptor, table.pos));
    }
    let mut columns = fb.budget.reserve(schema.fields.len())?;
    // Descriptor ranges are retained only temporarily for cross-column alias checks.
    let mut ranges = fb.budget.reserve(buffer_count)?;
    for i in 0..buffer_count {
        let at = buffer_start + i * BUFFER_BYTES;
        let descriptor = BufferDesc {
            offset_bytes: fb.i64(at)?,
            byte_length: fb.i64(at + I64_BYTES)?,
        };
        let range = arrow::checked_buffer_range(body.len() as u64, descriptor, BUFFER_ALIGN)
            .map_err(|e| {
                err(
                    ErrorKind::Column(e.kind),
                    body_at.checked_add(e.offset).unwrap_or(body_at),
                )
            })?;
        for old in &ranges {
            fb.budget.work(1)?;
            let old: &core::ops::Range<usize> = old;
            if !range.is_empty()
                && !old.is_empty()
                && range.start < old.end
                && old.start < range.end
            {
                return Err(err(ErrorKind::Aliasing, body_at + range.start));
            }
        }
        if !range.is_empty()
            && matches!(
                profile,
                AlignmentProfile::Relative64 | AlignmentProfile::HostAddress64
            )
            && (!range.start.is_multiple_of(ALIGN_64)
                || (profile == AlignmentProfile::HostAddress64
                    && !add(body.as_ptr() as usize, range.start)?.is_multiple_of(ALIGN_64)))
        {
            return Err(err(ErrorKind::Alignment, body_at + range.start));
        }
        ranges.push(range);
    }
    for (i, field) in schema.fields.iter().enumerate() {
        let at = node_start + i * FIELD_NODE_BYTES;
        let node = FieldNode {
            row_length: fb.i64(at)?,
            null_count: fb.i64(at + I64_BYTES)?,
        };
        if nonnegative(node.row_length, fb.base + at)? != rows {
            return Err(fb.fail(ErrorKind::Descriptor, at));
        }
        if !field.nullable && node.null_count != 0 {
            return Err(fb.fail(ErrorKind::NonNullable, at));
        }
        let descriptor = |range: &core::ops::Range<usize>| BufferDesc {
            offset_bytes: range.start as i64,
            byte_length: range.len() as i64,
        };
        fb.budget.work(rows)?;
        let column = I64Column::validate(
            body,
            node,
            descriptor(&ranges[i * 2]),
            descriptor(&ranges[i * 2 + 1]),
            AlignmentProfile::Relative8,
            arrow::Limits {
                max_body_bytes: fb.budget.limits.max_input_bytes,
                max_rows: fb.budget.limits.max_rows_per_batch,
                max_work: rows,
            },
        )
        .map_err(|e| {
            err(
                ErrorKind::Column(e.kind),
                body_at.checked_add(e.offset).unwrap_or(body_at),
            )
        })?;
        columns.push(column);
    }
    Ok(Batch {
        rows,
        columns,
        body,
        version,
    })
}

pub fn parse_exact(input: &[u8], limits: Limits) -> Result<Stream<'_>> {
    parse_exact_with_alignment(input, limits, AlignmentProfile::Relative8)
}
pub fn parse_prefix(input: &[u8], limits: Limits) -> Result<Parsed<'_>> {
    parse_prefix_with_alignment(input, limits, AlignmentProfile::Relative8)
}
pub fn parse_exact_with_alignment(
    input: &[u8],
    limits: Limits,
    profile: AlignmentProfile,
) -> Result<Stream<'_>> {
    let parsed = parse_prefix_with_alignment(input, limits, profile)?;
    if !parsed.tail.is_empty() {
        return Err(err(ErrorKind::TrailingBytes, parsed.consumed));
    }
    Ok(parsed.stream)
}
#[cfg(not(vesper_port))]
pub fn parse_prefix_with_alignment(
    input: &[u8],
    limits: Limits,
    profile: AlignmentProfile,
) -> Result<Parsed<'_>> {
    limits.validate()?;
    if input.len() > limits.max_input_bytes {
        return Err(err(ErrorKind::InputLimit, 0));
    }
    let mut budget = Budget {
        limits,
        remaining: limits.max_work,
        allocation: 0,
        metadata: 0,
    };
    budget.work(input.len())?;
    let mut pos = 0usize;
    let mut current_schema = None;
    let mut batches = Vec::new();
    let mut total_rows = 0usize;
    let mut framing = None;
    loop {
        let first = read_u32(input, pos, &mut budget)?;
        let modern = first == CONTINUATION;
        let prefix = if modern {
            MODERN_PREFIX_BYTES
        } else {
            LEGACY_PREFIX_BYTES
        };
        let length = if modern {
            read_u32(input, add(pos, U32_BYTES)?, &mut budget)?
        } else {
            first
        };
        if let Some(expected) = framing
            && expected != modern
        {
            return Err(err(ErrorKind::Framing, pos));
        }
        if length == 0 {
            let schema = current_schema.ok_or(err(ErrorKind::MissingSchema, pos))?;
            let consumed = add(pos, prefix)?;
            return Ok(Parsed {
                stream: Stream {
                    schema,
                    batches,
                    work_used: limits.max_work - budget.remaining,
                    metadata_allocation_bytes: budget.allocation,
                },
                consumed,
                tail: &input[consumed..],
            });
        }
        if length > i32::MAX as u32 {
            return Err(err(ErrorKind::Framing, pos));
        }
        let length = length as usize;
        if length > limits.max_metadata_bytes {
            return Err(err(ErrorKind::MetadataLimit, pos));
        }
        budget.metadata = add(budget.metadata, length)?;
        if budget.metadata > limits.max_total_metadata_bytes {
            return Err(err(ErrorKind::MetadataLimit, pos));
        }
        let metadata_at = add(pos, prefix)?;
        let body_at = add(metadata_at, length)?;
        if !body_at.is_multiple_of(BUFFER_ALIGN) {
            return Err(err(ErrorKind::Alignment, body_at));
        }
        let metadata = input
            .get(metadata_at..body_at)
            .ok_or(err(ErrorKind::UnexpectedEof, metadata_at))?;
        let mut fb = Fb::new(metadata, metadata_at, &mut budget)?;
        let message = fb.root()?;
        let version = fb.scalar16(message, 0, 0)?;
        if !matches!(version, METADATA_V4 | METADATA_V5) {
            return Err(fb.fail(ErrorKind::UnsupportedVersion, message.pos));
        }
        fb.empty_metadata(message, 4)?;
        let body_len = nonnegative(fb.scalar64(message, 3, 0)?, metadata_at + message.pos)?;
        if !body_len.is_multiple_of(BUFFER_ALIGN) {
            return Err(err(ErrorKind::Alignment, body_at));
        }
        let end = add(body_at, body_len)?;
        let body = input
            .get(body_at..end)
            .ok_or(err(ErrorKind::UnexpectedEof, body_at))?;
        match fb.scalar8(message, 1, 0)? {
            HEADER_SCHEMA => {
                if current_schema.is_some() || !body.is_empty() {
                    return Err(err(ErrorKind::Schema, pos));
                }
                current_schema = Some(schema(&mut fb, message, version)?);
                batches = fb
                    .budget
                    .reserve(limits.max_batches.min(input.len() / 8 + 1))?;
                framing = Some(modern);
            }
            HEADER_RECORD_BATCH => {
                let schema = current_schema
                    .as_ref()
                    .ok_or(err(ErrorKind::MissingSchema, pos))?;
                if batches.len() >= limits.max_batches {
                    return Err(err(ErrorKind::CountLimit, pos));
                }
                let value = batch(
                    &mut fb,
                    message,
                    (body, body_at),
                    schema,
                    version,
                    profile,
                    limits.max_total_rows - total_rows,
                )?;
                total_rows = add(total_rows, value.rows)?;
                if total_rows > limits.max_total_rows {
                    return Err(err(ErrorKind::CountLimit, pos));
                }
                batches.push(value);
            }
            HEADER_DICTIONARY_BATCH => return Err(err(ErrorKind::UnsupportedDictionary, pos)),
            _ => return Err(err(ErrorKind::UnsupportedHeader, pos)),
        }
        pos = end;
    }
}
#[cfg(vesper_port)]
pub fn parse_prefix_with_alignment(
    input: &[u8],
    limits: Limits,
    profile: AlignmentProfile,
) -> Result<Parsed<'_>> {
    let words = crate::port::vesper::ipc_parse_prefix(input, &port::limits(limits, input, profile))
        .map_err(port::error)?;
    Ok(port::parsed(&words, input))
}
#[cfg(not(vesper_port))]
fn read_u32(input: &[u8], at: usize, budget: &mut Budget) -> Result<u32> {
    let end = add(at, U32_BYTES)?;
    let bytes = input
        .get(at..end)
        .ok_or(err(ErrorKind::UnexpectedEof, at))?;
    budget.work(U32_BYTES)?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

/// The port-backed build's IPC glue: limits, failures and the result
/// stream of `vesper/src/ipc.ves`.
#[cfg(vesper_port)]
mod port {
    use super::{
        AlignmentProfile, Batch, Error, ErrorKind, Field, I64Column, Limits, Parsed, Schema, Stream,
    };
    use crate::port_binding::{ArCol, GErr};
    use core::mem::size_of;

    const KINDS: [ErrorKind; 31] = [
        ErrorKind::Configuration,
        ErrorKind::InputLimit,
        ErrorKind::MetadataLimit,
        ErrorKind::CountLimit,
        ErrorKind::NameLimit,
        ErrorKind::DepthLimit,
        ErrorKind::WorkLimit,
        ErrorKind::AllocationLimit,
        ErrorKind::AllocationFailed,
        ErrorKind::Overflow,
        ErrorKind::UnexpectedEof,
        ErrorKind::Framing,
        ErrorKind::Flatbuffer,
        ErrorKind::Alignment,
        ErrorKind::Aliasing,
        ErrorKind::Utf8,
        ErrorKind::Schema,
        ErrorKind::MissingSchema,
        ErrorKind::UnsupportedVersion,
        ErrorKind::UnsupportedHeader,
        ErrorKind::UnsupportedType,
        ErrorKind::UnsupportedEndian,
        ErrorKind::UnsupportedDictionary,
        ErrorKind::UnsupportedCompression,
        ErrorKind::UnsupportedMetadata,
        ErrorKind::UnsupportedFeature,
        ErrorKind::UnsupportedNested,
        ErrorKind::UnsupportedVariadic,
        ErrorKind::Descriptor,
        ErrorKind::NonNullable,
        ErrorKind::TrailingBytes,
    ];

    pub(super) fn error(e: GErr) -> Error {
        let kind = match KINDS.get(e.kind as usize) {
            Some(kind) => *kind,
            None => ErrorKind::Column(crate::port::kind(e.a)),
        };
        Error {
            kind,
            offset: e.offset as usize,
        }
    }

    /// The vectors the Rust parser reserves: Region (start, end, role).
    #[allow(dead_code)]
    struct Region {
        start: usize,
        end: usize,
        role: (u8, usize),
    }

    pub(super) fn limits(l: Limits, input: &[u8], profile: AlignmentProfile) -> [u64; 19] {
        [
            l.max_input_bytes as u64,
            l.max_metadata_bytes as u64,
            l.max_total_metadata_bytes as u64,
            l.max_fields as u64,
            l.max_batches as u64,
            l.max_rows_per_batch as u64,
            l.max_total_rows as u64,
            l.max_name_bytes as u64,
            l.max_metadata_objects as u64,
            l.max_depth as u64,
            l.max_work as u64,
            l.max_allocation_bytes as u64,
            size_of::<Region>() as u64,
            size_of::<Field<'_>>() as u64,
            size_of::<I64Column<'_>>() as u64,
            size_of::<core::ops::Range<usize>>() as u64,
            size_of::<Batch<'_>>() as u64,
            input.as_ptr() as usize as u64,
            profile as u64,
        ]
    }

    pub(super) fn parsed<'a>(w: &[u64], input: &'a [u8]) -> Parsed<'a> {
        let version = w[0] as u16;
        let nf = w[1] as usize;
        let mut at = 2;
        let mut fields = Vec::with_capacity(nf);
        for _ in 0..nf {
            let (start, len) = (w[at] as usize, w[at + 1] as usize);
            fields.push(Field {
                name: core::str::from_utf8(&input[start..start + len]).expect("validated name"),
                nullable: w[at + 2] != 0,
            });
            at += 3;
        }
        let count = w[at] as usize;
        at += 1;
        let mut batches = Vec::with_capacity(count);
        for _ in 0..count {
            let rows = w[at] as usize;
            let (start, len) = (w[at + 1] as usize, w[at + 2] as usize);
            let body = &input[start..start + len];
            let batch_version = w[at + 3] as u16;
            at += 4;
            let mut columns = Vec::with_capacity(nf);
            for _ in 0..nf {
                let c = ArCol {
                    values_start: w[at],
                    values_end: w[at + 1],
                    has_validity: w[at + 2] != 0,
                    validity_start: w[at + 3],
                    validity_end: w[at + 4],
                    rows: w[at + 5],
                    nulls: w[at + 6],
                };
                columns.push(I64Column::from_parts(body, c));
                at += 7;
            }
            batches.push(Batch {
                rows,
                columns,
                body,
                version: batch_version,
            });
        }
        let consumed = w[at + 2] as usize;
        Parsed {
            stream: Stream {
                schema: Schema { fields, version },
                batches,
                work_used: w[at] as usize,
                metadata_allocation_bytes: w[at + 1] as usize,
            },
            consumed,
            tail: &input[consumed..],
        }
    }
}
