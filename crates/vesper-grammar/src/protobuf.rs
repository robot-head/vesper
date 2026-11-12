//! Schema-driven canonical Protobuf host profile. Standard Protobuf has no
//! universal canonical encoding. This policy preserves explicit singular
//! presence/defaults and ordered repeated elements. See PROFILE and module docs.
//!
//! Fields are ascending, unknowns forbidden, singular duplicates forbidden.
//! Repeated numeric scalars have exactly one nonempty packed segment. Other
//! repeated fields have contiguous ordered unpacked occurrences. Empty repeated
//! values are represented by absence. Schemas are flat, sorted and acyclic.
//! No maps, groups, int32/int64, enums, floats, oneofs, extensions, source macros,
//! Regions, SIMD, constant-time or proof claims are supplied.
//!
//! PROFILE is selected out-of-band with a fixed validated schema; it is not a
//! Protobuf field or a claim that ordinary Protobuf serialization is canonical.
//! Every singular field is optional with explicit presence: absence differs
//! from present zero/false/empty bytes/string/message. One FieldValue represents
//! each repeated element; no entries represents an empty repetition. Nested
//! messages are recursively validated and exact within their declared lengths.
//! Strings use valid UTF-8 without Unicode normalization. Reserved schema field
//! numbers 19000..=19999 are unsupported. Root and length-delimited wire extents
//! cannot exceed i32::MAX bytes even if caller resource limits are increased.
//!
//! parse_exact consumes the whole supplied slice, but raw Protobuf has no
//! self-contained framing. A complete field-boundary prefix can itself be a valid
//! different optional message; use an outer length frame for transport boundaries.
//! Unknown, duplicate, out-of-order, non-minimal and alternate packed forms fail.
//!
//! Schema validation borrows descriptors and uses one fallible bounded temporary
//! metadata vector. Parse results own only bounded field metadata; strings/bytes
//! borrow actual input. Before each vector growth the cumulative requested
//! capacity bytes, relocation work and node budget are checked. Limits describe
//! requested payload, not allocator rounding, caller allocations or process RSS.
//! Encoding borrows typed values and fully preflights without metadata allocation;
//! its second pass recomputes bounded nested lengths without recursive cloning.
//! Work units are deterministic accounting, not time or CPU-instruction counts.
//!
//! ```
//! use vesper_grammar::protobuf::{Schema, MessageSchema, FieldSchema, Kind, Limits};
//! let definitions = [MessageSchema { fields: vec![FieldSchema {
//!     number: 1, kind: Kind::U64, repeated: false,
//! }] }];
//! let schema = Schema::validate(&definitions, 0, Limits::default())?;
//! let value = schema.parse_exact(&[0x08, 0x96, 0x01])?;
//! let mut output = [0; 3];
//! assert_eq!(schema.encode_into(&value, &mut output)?, 3);
//! assert_eq!(output, [0x08, 0x96, 0x01]);
//! # Ok::<(), vesper_grammar::protobuf::Error>(())
//! ```
//! Recursive logical values deliberately have no Clone implementation:
//! ```compile_fail
//! use vesper_grammar::protobuf::Value;
//! fn duplicate(value: Value<'_>) { let _copy = value.clone(); }
//! ```
use core::fmt;

pub const PROFILE: &str = "vesper.protobuf.explicit-presence/1";
pub const HARD_DEPTH: usize = 64;
/// Protobuf message/length-delimited payload ceiling, independent of caller budgets.
pub const MAX_WIRE_BYTES: usize = i32::MAX as usize;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    U32,
    U64,
    Sint32,
    Sint64,
    Bool,
    Fixed32,
    Fixed64,
    String,
    Bytes,
    Message(usize),
}
impl Kind {
    fn numeric(self) -> bool {
        !matches!(self, Self::String | Self::Bytes | Self::Message(_))
    }
    fn wire(self) -> u8 {
        match self {
            Self::Fixed64 => 1,
            Self::String | Self::Bytes | Self::Message(_) => 2,
            Self::Fixed32 => 5,
            _ => 0,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldSchema {
    pub number: u32,
    pub kind: Kind,
    pub repeated: bool,
}
#[derive(Debug, Eq, PartialEq)]
pub struct MessageSchema {
    pub fields: Vec<FieldSchema>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Message<'a> {
    pub fields: Vec<FieldValue<'a>>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct FieldValue<'a> {
    pub number: u32,
    pub value: Value<'a>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Value<'a> {
    U32(u32),
    U64(u64),
    Sint32(i32),
    Sint64(i64),
    Bool(bool),
    Fixed32(u32),
    Fixed64(u64),
    String(&'a str),
    Bytes(&'a [u8]),
    Message(Message<'a>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Schema,
    FieldNumber,
    RecursiveSchema,
    Order,
    Duplicate,
    UnknownField,
    WireType,
    NonCanonical,
    Truncated,
    Utf8,
    Overflow,
    TypeMismatch,
    InputLimit,
    OutputLimit,
    OutputTooSmall,
    PayloadLimit,
    NodeLimit,
    DepthLimit,
    WorkLimit,
    MetadataLimit,
    AllocationFailed,
}
/// Wire errors carry input byte offsets; schema errors may use a message index,
/// and direct/preflight resource errors use zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
}
impl Error {
    const fn at(kind: ErrorKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at offset {}", self.kind, self.offset)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = core::result::Result<T, Error>;
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(Error::at(ErrorKind::Overflow, 0))
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(Error::at(ErrorKind::Overflow, 0))
}
fn bound(n: usize, max: usize, kind: ErrorKind) -> Result<()> {
    if n > max {
        Err(Error::at(kind, 0))
    } else {
        Ok(())
    }
}
fn field_number(n: u32) -> bool {
    n != 0 && n < (1 << 29) && !(19_000..=19_999).contains(&n)
}
fn var_len(n: u64) -> usize {
    crate::framing::encoded_u64_len(n)
}
fn tag(field: FieldSchema) -> u64 {
    (u64::from(field.number) << 3)
        | u64::from(if field.repeated && field.kind.numeric() {
            2
        } else {
            field.kind.wire()
        })
}
fn zig32(n: i32) -> u64 {
    u64::from(((n as u32) << 1) ^ ((n >> 31) as u32))
}
fn zig64(n: i64) -> u64 {
    ((n as u64) << 1) ^ ((n >> 63) as u64)
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_schema_messages: usize,
    pub max_schema_fields: usize,
    pub max_fields_per_message: usize,
    pub max_depth: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_nodes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_schema_messages: 128,
            max_schema_fields: 1024,
            max_fields_per_message: 128,
            max_depth: 32,
            max_input_bytes: 4 * 1024 * 1024,
            max_output_bytes: 4 * 1024 * 1024,
            max_payload_bytes: 1024 * 1024,
            max_nodes: 65_536,
            max_metadata_bytes: 8 * 1024 * 1024,
            max_work: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Meta {
    color: u8,
    height: usize,
}
/// Validated borrowed schema. No unvalidated recursive schema/value is cloned.
#[derive(Debug)]
pub struct Schema<'s> {
    messages: &'s [MessageSchema],
    root: usize,
    limits: Limits,
}
impl<'s> Schema<'s> {
    pub fn validate(messages: &'s [MessageSchema], root: usize, limits: Limits) -> Result<Self> {
        if messages.is_empty() || root >= messages.len() {
            return Err(Error::at(ErrorKind::Schema, root));
        }
        if limits.max_depth == 0 || limits.max_depth > HARD_DEPTH {
            return Err(Error::at(ErrorKind::DepthLimit, 0));
        }
        bound(
            messages.len(),
            limits.max_schema_messages,
            ErrorKind::Schema,
        )?;
        let mut budget = Budget::new(limits);
        budget.work(messages.len())?;
        let mut total = 0;
        for (id, message) in messages.iter().enumerate() {
            bound(
                message.fields.len(),
                limits.max_fields_per_message,
                ErrorKind::Schema,
            )?;
            total = add(total, message.fields.len())?;
            bound(total, limits.max_schema_fields, ErrorKind::Schema)?;
            budget.work(message.fields.len())?;
            let mut previous = 0;
            for field in &message.fields {
                if !field_number(field.number) {
                    return Err(Error::at(ErrorKind::FieldNumber, id));
                }
                if field.number <= previous {
                    return Err(Error::at(
                        if field.number == previous {
                            ErrorKind::Duplicate
                        } else {
                            ErrorKind::Order
                        },
                        id,
                    ));
                }
                if matches!(field.kind, Kind::Message(child) if child >= messages.len()) {
                    return Err(Error::at(ErrorKind::Schema, id));
                }
                previous = field.number;
            }
        }
        let bytes = mul(messages.len(), size_of::<Meta>())?;
        budget.metadata(bytes)?;
        let mut meta = Vec::new();
        meta.try_reserve_exact(messages.len())
            .map_err(|_| Error::at(ErrorKind::AllocationFailed, 0))?;
        meta.resize(messages.len(), Meta::default());
        for id in 0..messages.len() {
            schema_depth(id, messages, &mut meta, &mut budget, 1)?;
        }
        Ok(Self {
            messages,
            root,
            limits,
        })
    }
    pub fn root(&self) -> usize {
        self.root
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn messages(&self) -> &[MessageSchema] {
        self.messages
    }
    /// Parses every input byte as part of one complete message. Protobuf itself
    /// is not self-framing; use an outer length frame for concatenated messages.
    pub fn parse_exact<'a>(&self, input: &'a [u8]) -> Result<Message<'a>> {
        bound(
            input.len(),
            self.limits.max_input_bytes,
            ErrorKind::InputLimit,
        )?;
        bound(input.len(), MAX_WIRE_BYTES, ErrorKind::Overflow)?;
        let mut budget = Budget::new(self.limits);
        budget.work(input.len())?;
        let mut parser = Parser {
            schema: self,
            input,
            pos: 0,
            budget,
        };
        parser.message(self.root, input.len(), 1)
    }
    pub fn encoded_size(&self, message: &Message<'_>) -> Result<usize> {
        let mut budget = Budget::new(self.limits);
        let size = scan_message(self, self.root, message, &mut budget, 1)?;
        bound(size, self.limits.max_output_bytes, ErrorKind::OutputLimit)?;
        bound(size, MAX_WIRE_BYTES, ErrorKind::Overflow)?;
        budget.work(size)?;
        Ok(size)
    }
    /// Every Result failure preserves every caller output byte and all borrowed
    /// schema/message owners. No output mutation happens before full preflight.
    pub fn encode_into(&self, message: &Message<'_>, output: &mut [u8]) -> Result<usize> {
        let size = self.encoded_size(message)?;
        if output.len() < size {
            return Err(Error::at(ErrorKind::OutputTooSmall, 0));
        }
        let mut writer = Writer {
            schema: self,
            output: &mut output[..size],
            pos: 0,
        };
        writer.message(self.root, message);
        debug_assert_eq!(writer.pos, size);
        Ok(size)
    }
    fn lookup(&self, id: usize, number: u32, budget: &mut Budget) -> Result<FieldSchema> {
        let fields = &self.messages[id].fields;
        let (mut lo, mut hi) = (0, fields.len());
        while lo < hi {
            budget.work(1)?;
            let mid = lo + (hi - lo) / 2;
            match fields[mid].number.cmp(&number) {
                core::cmp::Ordering::Equal => return Ok(fields[mid]),
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        Err(Error::at(ErrorKind::UnknownField, 0))
    }
    fn trusted_lookup(&self, id: usize, number: u32) -> FieldSchema {
        let fields = &self.messages[id].fields;
        fields[fields
            .binary_search_by_key(&number, |f| f.number)
            .expect("validated field")]
    }
}
struct Budget {
    limits: Limits,
    remaining: usize,
    nodes: usize,
    metadata: usize,
}
impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            remaining: limits.max_work,
            nodes: 0,
            metadata: 0,
        }
    }
    fn work(&mut self, n: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or(Error::at(ErrorKind::WorkLimit, 0))?;
        Ok(())
    }
    fn metadata(&mut self, n: usize) -> Result<()> {
        self.metadata = add(self.metadata, n)?;
        bound(
            self.metadata,
            self.limits.max_metadata_bytes,
            ErrorKind::MetadataLimit,
        )
    }
    fn node(&mut self, depth: usize) -> Result<()> {
        bound(depth, self.limits.max_depth, ErrorKind::DepthLimit)?;
        self.nodes = add(self.nodes, 1)?;
        bound(self.nodes, self.limits.max_nodes, ErrorKind::NodeLimit)?;
        self.work(mul(64, depth)?)
    }
    fn reserve(&mut self, fields: &mut Vec<FieldValue<'_>>) -> Result<()> {
        if fields.len() == fields.capacity() {
            let old = fields.capacity();
            let next = if old == 0 { 4 } else { mul(old, 2)? }.min(self.limits.max_nodes);
            if next <= old {
                return Err(Error::at(ErrorKind::NodeLimit, 0));
            }
            self.metadata(mul(next - old, size_of::<FieldValue<'_>>())?)?;
            self.work(mul(fields.len(), size_of::<FieldValue<'_>>())?)?;
            fields
                .try_reserve_exact(next - fields.len())
                .map_err(|_| Error::at(ErrorKind::AllocationFailed, 0))?;
        }
        Ok(())
    }
}
fn schema_depth(
    id: usize,
    messages: &[MessageSchema],
    meta: &mut [Meta],
    budget: &mut Budget,
    depth: usize,
) -> Result<usize> {
    budget.work(1)?;
    if meta[id].color == 1 {
        return Err(Error::at(ErrorKind::RecursiveSchema, id));
    }
    bound(depth, budget.limits.max_depth, ErrorKind::DepthLimit)?;
    if meta[id].color == 2 {
        return Ok(meta[id].height);
    }
    meta[id].color = 1;
    let mut height = 1;
    for field in &messages[id].fields {
        budget.work(1)?;
        if let Kind::Message(child) = field.kind {
            height = height.max(add(
                schema_depth(child, messages, meta, budget, depth + 1)?,
                1,
            )?);
        }
    }
    bound(height, budget.limits.max_depth, ErrorKind::DepthLimit)?;
    meta[id] = Meta { color: 2, height };
    Ok(height)
}
struct Parser<'s, 'd, 'a> {
    schema: &'d Schema<'s>,
    input: &'a [u8],
    pos: usize,
    budget: Budget,
}
impl<'a> Parser<'_, '_, 'a> {
    fn var(&mut self, end: usize) -> Result<u64> {
        let start = self.pos;
        let (value, len) =
            crate::framing::decode(&self.input[start..end], &mut self.budget.remaining).map_err(
                |e| {
                    Error::at(
                        match e.kind {
                            crate::ErrorKind::NonCanonical => ErrorKind::NonCanonical,
                            crate::ErrorKind::Overflow => ErrorKind::Overflow,
                            crate::ErrorKind::WorkLimit => ErrorKind::WorkLimit,
                            _ => ErrorKind::Truncated,
                        },
                        start + e.offset,
                    )
                },
            )?;
        self.pos += len;
        Ok(value)
    }
    fn range(&mut self, end: usize) -> Result<core::ops::Range<usize>> {
        let len = usize::try_from(self.var(end)?)
            .map_err(|_| Error::at(ErrorKind::Overflow, self.pos))?;
        bound(len, MAX_WIRE_BYTES, ErrorKind::Overflow)?;
        bound(
            len,
            self.budget.limits.max_payload_bytes,
            ErrorKind::PayloadLimit,
        )?;
        let start = self.pos;
        let finish = add(start, len)?;
        if finish > end {
            return Err(Error::at(ErrorKind::Truncated, end));
        }
        Ok(start..finish)
    }
    fn message(&mut self, id: usize, end: usize, depth: usize) -> Result<Message<'a>> {
        self.budget.node(depth)?;
        let mut fields = Vec::new();
        let mut previous = 0;
        while self.pos < end {
            let at = self.pos;
            let key = self.var(end)?;
            let number =
                u32::try_from(key >> 3).map_err(|_| Error::at(ErrorKind::FieldNumber, at))?;
            if !field_number(number) {
                return Err(Error::at(ErrorKind::FieldNumber, at));
            }
            let wire = (key & 7) as u8;
            if !matches!(wire, 0 | 1 | 2 | 5) {
                return Err(Error::at(ErrorKind::WireType, at));
            }
            if number < previous {
                return Err(Error::at(ErrorKind::Order, at));
            }
            let field = self
                .schema
                .lookup(id, number, &mut self.budget)
                .map_err(|mut e| {
                    e.offset = at;
                    e
                })?;
            if number == previous && (!field.repeated || field.kind.numeric()) {
                return Err(Error::at(ErrorKind::Duplicate, at));
            }
            let expected = if field.repeated && field.kind.numeric() {
                2
            } else {
                field.kind.wire()
            };
            if wire != expected {
                return Err(Error::at(ErrorKind::WireType, at));
            }
            previous = number;
            if field.repeated && field.kind.numeric() {
                let range = self.range(end)?;
                if range.is_empty() {
                    return Err(Error::at(ErrorKind::NonCanonical, at));
                }
                while self.pos < range.end {
                    self.budget.node(depth)?;
                    self.budget.reserve(&mut fields)?;
                    let value = self.scalar(field.kind, range.end)?;
                    fields.push(FieldValue { number, value });
                }
            } else {
                self.budget.node(depth)?;
                self.budget.reserve(&mut fields)?;
                let value = match field.kind {
                    Kind::Bytes | Kind::String => {
                        let range = self.range(end)?;
                        self.budget.work(range.len())?;
                        self.pos = range.end;
                        let bytes = &self.input[range.clone()];
                        if field.kind == Kind::Bytes {
                            Value::Bytes(bytes)
                        } else {
                            Value::String(core::str::from_utf8(bytes).map_err(|e| {
                                Error::at(ErrorKind::Utf8, range.start + e.valid_up_to())
                            })?)
                        }
                    }
                    Kind::Message(child) => {
                        let range = self.range(end)?;
                        Value::Message(self.message(child, range.end, depth + 1)?)
                    }
                    kind => self.scalar(kind, end)?,
                };
                fields.push(FieldValue { number, value });
            }
        }
        Ok(Message { fields })
    }
    fn scalar(&mut self, kind: Kind, end: usize) -> Result<Value<'a>> {
        if matches!(kind, Kind::Fixed32 | Kind::Fixed64) {
            let len = if kind == Kind::Fixed32 { 4 } else { 8 };
            let next = add(self.pos, len)?;
            if next > end {
                return Err(Error::at(ErrorKind::Truncated, end));
            }
            let bytes = &self.input[self.pos..next];
            self.budget.work(len)?;
            self.pos = next;
            return Ok(if kind == Kind::Fixed32 {
                Value::Fixed32(u32::from_le_bytes(bytes.try_into().expect("fixed32")))
            } else {
                Value::Fixed64(u64::from_le_bytes(bytes.try_into().expect("fixed64")))
            });
        }
        let at = self.pos;
        let n = self.var(end)?;
        Ok(match kind {
            Kind::U32 => {
                Value::U32(u32::try_from(n).map_err(|_| Error::at(ErrorKind::Overflow, at))?)
            }
            Kind::U64 => Value::U64(n),
            Kind::Sint32 => {
                let n = u32::try_from(n).map_err(|_| Error::at(ErrorKind::Overflow, at))?;
                Value::Sint32(((n >> 1) as i32) ^ -((n & 1) as i32))
            }
            Kind::Sint64 => Value::Sint64(((n >> 1) as i64) ^ -((n & 1) as i64)),
            Kind::Bool => match n {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(Error::at(ErrorKind::NonCanonical, at)),
            },
            _ => return Err(Error::at(ErrorKind::TypeMismatch, at)),
        })
    }
}
fn scalar_size(kind: Kind, value: &Value<'_>) -> Result<usize> {
    Ok(match (kind, value) {
        (Kind::U32, Value::U32(n)) => var_len(u64::from(*n)),
        (Kind::U64, Value::U64(n)) => var_len(*n),
        (Kind::Sint32, Value::Sint32(n)) => var_len(zig32(*n)),
        (Kind::Sint64, Value::Sint64(n)) => var_len(zig64(*n)),
        (Kind::Bool, Value::Bool(_)) => 1,
        (Kind::Fixed32, Value::Fixed32(_)) => 4,
        (Kind::Fixed64, Value::Fixed64(_)) => 8,
        _ => return Err(Error::at(ErrorKind::TypeMismatch, 0)),
    })
}
fn scan_value(
    schema: &Schema<'_>,
    kind: Kind,
    value: &Value<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<usize> {
    match (kind, value) {
        (Kind::String, Value::String(text)) => {
            bound(text.len(), MAX_WIRE_BYTES, ErrorKind::Overflow)?;
            bound(
                text.len(),
                budget.limits.max_payload_bytes,
                ErrorKind::PayloadLimit,
            )?;
            budget.work(text.len())?;
            add(
                var_len(u64::try_from(text.len()).map_err(|_| Error::at(ErrorKind::Overflow, 0))?),
                text.len(),
            )
        }
        (Kind::Bytes, Value::Bytes(bytes)) => {
            bound(bytes.len(), MAX_WIRE_BYTES, ErrorKind::Overflow)?;
            bound(
                bytes.len(),
                budget.limits.max_payload_bytes,
                ErrorKind::PayloadLimit,
            )?;
            budget.work(bytes.len())?;
            add(
                var_len(u64::try_from(bytes.len()).map_err(|_| Error::at(ErrorKind::Overflow, 0))?),
                bytes.len(),
            )
        }
        (Kind::Message(child), Value::Message(message)) => {
            let len = scan_message(schema, child, message, budget, depth + 1)?;
            bound(len, MAX_WIRE_BYTES, ErrorKind::Overflow)?;
            bound(
                len,
                budget.limits.max_payload_bytes,
                ErrorKind::PayloadLimit,
            )?;
            add(
                var_len(u64::try_from(len).map_err(|_| Error::at(ErrorKind::Overflow, 0))?),
                len,
            )
        }
        _ => scalar_size(kind, value),
    }
}
fn scan_message(
    schema: &Schema<'_>,
    id: usize,
    message: &Message<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<usize> {
    budget.node(depth)?;
    bound(
        message.fields.len(),
        budget
            .limits
            .max_nodes
            .checked_sub(budget.nodes)
            .ok_or(Error::at(ErrorKind::NodeLimit, 0))?,
        ErrorKind::NodeLimit,
    )?;
    let mut index = 0;
    let mut previous = 0;
    let mut size = 0;
    while index < message.fields.len() {
        let number = message.fields[index].number;
        if !field_number(number) {
            return Err(Error::at(ErrorKind::FieldNumber, 0));
        }
        if number < previous {
            return Err(Error::at(ErrorKind::Order, 0));
        }
        let field = schema.lookup(id, number, budget)?;
        let mut end = index + 1;
        while end < message.fields.len() && message.fields[end].number == number {
            budget.work(1)?;
            end += 1;
        }
        if !field.repeated && end - index != 1 {
            return Err(Error::at(ErrorKind::Duplicate, 0));
        }
        if field.repeated && field.kind.numeric() {
            let mut payload = 0;
            for entry in &message.fields[index..end] {
                budget.node(depth)?;
                payload = add(payload, scalar_size(field.kind, &entry.value)?)?;
            }
            bound(payload, MAX_WIRE_BYTES, ErrorKind::Overflow)?;
            bound(
                payload,
                budget.limits.max_payload_bytes,
                ErrorKind::PayloadLimit,
            )?;
            size = add(
                size,
                add(
                    var_len(tag(field)),
                    add(
                        var_len(
                            u64::try_from(payload)
                                .map_err(|_| Error::at(ErrorKind::Overflow, 0))?,
                        ),
                        payload,
                    )?,
                )?,
            )?;
        } else {
            for entry in &message.fields[index..end] {
                budget.node(depth)?;
                size = add(
                    size,
                    add(
                        var_len(tag(field)),
                        scan_value(schema, field.kind, &entry.value, budget, depth)?,
                    )?,
                )?;
            }
        }
        bound(size, budget.limits.max_output_bytes, ErrorKind::OutputLimit)?;
        previous = number;
        index = end;
    }
    Ok(size)
}
// These second-pass calculations cannot overflow after full bounded preflight.
// They do not allocate or clone. The depth-weighted work reservation covers
// repeated size visits by ancestors while writing nested message lengths.
fn trusted_value_size(schema: &Schema<'_>, kind: Kind, value: &Value<'_>) -> usize {
    match (kind, value) {
        (Kind::String, Value::String(s)) => var_len(s.len() as u64) + s.len(),
        (Kind::Bytes, Value::Bytes(b)) => var_len(b.len() as u64) + b.len(),
        (Kind::Message(child), Value::Message(m)) => {
            let n = trusted_message_size(schema, child, m);
            var_len(n as u64) + n
        }
        _ => scalar_size(kind, value).expect("preflight scalar"),
    }
}
fn trusted_message_size(schema: &Schema<'_>, id: usize, message: &Message<'_>) -> usize {
    let (mut i, mut size) = (0, 0);
    while i < message.fields.len() {
        let field = schema.trusted_lookup(id, message.fields[i].number);
        let mut end = i + 1;
        while end < message.fields.len() && message.fields[end].number == field.number {
            end += 1;
        }
        if field.repeated && field.kind.numeric() {
            let n: usize = message.fields[i..end]
                .iter()
                .map(|f| scalar_size(field.kind, &f.value).expect("validated scalar"))
                .sum();
            size += var_len(tag(field)) + var_len(n as u64) + n;
        } else {
            for f in &message.fields[i..end] {
                size += var_len(tag(field)) + trusted_value_size(schema, field.kind, &f.value);
            }
        }
        i = end;
    }
    size
}
struct Writer<'s, 'd, 'o> {
    schema: &'d Schema<'s>,
    output: &'o mut [u8],
    pos: usize,
}
impl Writer<'_, '_, '_> {
    fn bytes(&mut self, bytes: &[u8]) {
        self.output[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
    }
    fn var(&mut self, mut n: u64) {
        loop {
            self.output[self.pos] = (n as u8 & 127) | if n >= 128 { 128 } else { 0 };
            self.pos += 1;
            n >>= 7;
            if n == 0 {
                break;
            }
        }
    }
    fn message(&mut self, id: usize, message: &Message<'_>) {
        let mut i = 0;
        while i < message.fields.len() {
            let field = self.schema.trusted_lookup(id, message.fields[i].number);
            let mut end = i + 1;
            while end < message.fields.len() && message.fields[end].number == field.number {
                end += 1;
            }
            if field.repeated && field.kind.numeric() {
                let len: usize = message.fields[i..end]
                    .iter()
                    .map(|f| scalar_size(field.kind, &f.value).expect("validated scalar"))
                    .sum();
                self.var(tag(field));
                self.var(len as u64);
                for f in &message.fields[i..end] {
                    self.scalar(&f.value);
                }
            } else {
                for f in &message.fields[i..end] {
                    self.var(tag(field));
                    match (&f.value, field.kind) {
                        (Value::String(text), _) => {
                            self.var(text.len() as u64);
                            self.bytes(text.as_bytes());
                        }
                        (Value::Bytes(bytes), _) => {
                            self.var(bytes.len() as u64);
                            self.bytes(bytes);
                        }
                        (Value::Message(child), Kind::Message(child_id)) => {
                            let len = trusted_message_size(self.schema, child_id, child);
                            self.var(len as u64);
                            self.message(child_id, child);
                        }
                        _ => self.scalar(&f.value),
                    }
                }
            }
            i = end;
        }
    }
    fn scalar(&mut self, value: &Value<'_>) {
        match value {
            Value::U32(n) => self.var(u64::from(*n)),
            Value::U64(n) => self.var(*n),
            Value::Sint32(n) => self.var(zig32(*n)),
            Value::Sint64(n) => self.var(zig64(*n)),
            Value::Bool(b) => self.var(u64::from(*b)),
            Value::Fixed32(n) => self.bytes(&n.to_le_bytes()),
            Value::Fixed64(n) => self.bytes(&n.to_le_bytes()),
            _ => unreachable!("validated scalar"),
        }
    }
}
