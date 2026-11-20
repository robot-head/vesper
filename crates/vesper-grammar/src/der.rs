//! Strict, bounded DER profile. This is not an ASN.1 SET, X.509, PKCS#8,
//! secret/constant-time parser, or a generic ASN.1 schema compiler.
//!
//! Supported: primitive BOOLEAN, arbitrary minimal signed two's-complement
//! INTEGER bytes, BIT STRING (preserving bit length), OCTET STRING, UTF8String,
//! IA5String, PrintableString, NULL, OID,
//! SEQUENCE, and explicitly schema-typed SET OF. [`Schema::Any`] never infers
//! SET OF from tag 17; ASN.1 SET is unsupported. All non-universal tags and
//! unimplemented universal strings/times/types return an explicit unsupported
//! error. OID subidentifiers, including the combined first two arcs, fit u64;
//! larger legal ASN.1 OIDs are outside this profile and return overflow.
//!
//! Parsing borrows payloads and uses a bounded flat node index after validation.
//! Schema and value visits count every shared DAG occurrence, not just distinct
//! references. Depth has a hard ceiling of 64. Direct typed encoding validates
//! before allocation, sorts SET OF by complete canonical child DER octets, and
//! retains duplicates as an unordered multiset. The conservative intermediate
//! reservation sums every node's complete encoded length: this bounds retained
//! child buffers plus parent output during construction. Metadata separately
//! reserves two Vec controls plus two usize per encoded node. These are logical
//! payload reservations, excluding allocator rounding, capacity overhead and
//! caller-owned schema/value memory; not RSS or general OOM guarantees.
//!
//! Encoders use private scratch and copy to the caller only after success. All
//! returned failures preserve caller output, source owners and output suffixes.
//! Prefix parsing returns the exact consumed extent; only [`parse_exact`] rejects
//! trailing bytes. No cryptographic authenticity or formal bijection proof is
//! inferred from accepted canonical encodings.
#![forbid(unsafe_code)]
#[cfg(not(vesper_port))]
use core::cmp::Ordering;

#[cfg(not(vesper_port))]
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// ITU-T X.690 (BER/DER) identifier and length octets.
#[cfg_attr(vesper_port, allow(dead_code))]
mod x690 {
    /// Bits 8-7 of the identifier octet: the tag class (§8.1.2.2).
    pub const CLASS_SHIFT: u32 = 6;
    /// Bit 6: constructed encoding (§8.1.2.5).
    pub const CONSTRUCTED: u8 = 0x20;
    /// Bits 5-1: the tag number, or `HIGH_TAG` for the high-tag-number form
    /// (§8.1.2.4).
    pub const TAG_NUMBER_MASK: u8 = 0x1f;
    pub const HIGH_TAG: u64 = 31;
    /// Base-128 subidentifiers and high tag numbers: bit 8 says more follow.
    pub const MORE: u8 = 0x80;
    pub const SEVEN_BITS: u8 = 0x7f;
    pub const BASE128: u64 = 128;
    /// Lengths below `LONG_FORM` use the short form; a long form's first
    /// octet is `LONG_FORM | count` (§8.1.3.4, §8.1.3.5).
    pub const LONG_FORM: u8 = 0x80;
    /// DER encodes TRUE as all ones (X.690 §11.1).
    pub const TRUE: u8 = 0xff;
    /// A BIT STRING's initial octet counts at most seven unused bits.
    pub const MAX_UNUSED_BITS: u8 = 7;
    /// Universal tag numbers (X.680 §8.6).
    pub const BOOLEAN: u64 = 1;
    pub const INTEGER: u64 = 2;
    pub const BIT_STRING: u64 = 3;
    pub const OCTET_STRING: u64 = 4;
    pub const NULL: u64 = 5;
    pub const OBJECT_IDENTIFIER: u64 = 6;
    pub const UTF8_STRING: u64 = 12;
    pub const SEQUENCE: u64 = 16;
    pub const SET: u64 = 17;
    pub const PRINTABLE_STRING: u64 = 19;
    pub const IA5_STRING: u64 = 22;
    /// The most base-128 octets a high tag number may use here (fits `u64`).
    pub const MAX_TAG_OCTETS: usize = 10;
    /// Two's-complement INTEGER contents: the sign bit of an octet, and the
    /// octet a negative value's redundant leading octet would be (§8.3.2).
    pub const SIGN_BIT: u8 = 0x80;
    pub const ALL_ONES: u8 = 0xff;
}
use x690::*;

/// Ceilings [`Limits`] may not exceed.
#[cfg(not(vesper_port))]
const MAX_NODES: usize = 16384;
#[cfg(not(vesper_port))]
const MAX_SCHEMA_NODES: usize = 4096;
#[cfg(not(vesper_port))]
const MAX_DEPTH: usize = 64;
#[cfg(not(vesper_port))]
const MAX_WORK: usize = 128 * 1024 * 1024;
#[cfg(not(vesper_port))]
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
#[cfg(not(vesper_port))]
const MAX_INTERMEDIATE_BYTES: usize = 64 * 1024 * 1024;
const ENCODE_METADATA: usize =
    2 * core::mem::size_of::<Vec<u8>>() + 2 * core::mem::size_of::<usize>();
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_value_bytes: usize,
    pub max_nodes: usize,
    pub max_schema_nodes: usize,
    pub max_depth: usize,
    pub max_work: usize,
    pub max_metadata_bytes: usize,
    pub max_output_bytes: usize,
    pub max_intermediate_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_value_bytes: 1024 * 1024,
            max_nodes: 4096,
            max_schema_nodes: 1024,
            max_depth: 32,
            max_work: 16 * 1024 * 1024,
            max_metadata_bytes: 512 * 1024,
            max_output_bytes: 1024 * 1024,
            max_intermediate_bytes: 8 * 1024 * 1024,
        }
    }
}
impl Limits {
    #[cfg(not(vesper_port))]
    fn validate(self) -> Result<()> {
        if self.max_input_bytes > MAX_BYTES
            || self.max_value_bytes > MAX_BYTES
            || self.max_output_bytes > MAX_BYTES
            || self.max_nodes > MAX_NODES
            || self.max_schema_nodes > MAX_SCHEMA_NODES
            || self.max_depth > MAX_DEPTH
            || self.max_work > MAX_WORK
            || self.max_metadata_bytes > MAX_METADATA_BYTES
            || self.max_intermediate_bytes > MAX_INTERMEDIATE_BYTES
        {
            return Err(error(ErrorKind::InvalidLimits, 0));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidLimits,
    InputLimit,
    ValueLimit,
    NodeLimit,
    SchemaLimit,
    DepthLimit,
    WorkLimit,
    MetadataLimit,
    OutputLimit,
    IntermediateLimit,
    AllocationFailed,
    Overflow,
    UnexpectedEof,
    TrailingBytes,
    IndefiniteLength,
    NonCanonicalTag,
    NonCanonicalLength,
    Constructed,
    Boolean,
    Integer,
    BitString,
    ObjectIdentifier,
    Utf8String,
    Ia5String,
    PrintableString,
    Null,
    SetOrder,
    SchemaMismatch,
    SetSchemaRequired,
    EndOfContents,
    OutputTooSmall,
    UnsupportedTag { class: u8, number: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
}
pub type Result<T> = core::result::Result<T, Error>;
fn error(kind: ErrorKind, offset: usize) -> Error {
    Error { kind, offset }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}
impl std::error::Error for Error {}
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(error(ErrorKind::Overflow, 0))
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(error(ErrorKind::Overflow, 0))
}
fn bound(value: usize, max: usize, kind: ErrorKind) -> Result<()> {
    if value > max {
        Err(error(kind, 0))
    } else {
        Ok(())
    }
}
#[cfg(not(vesper_port))]
struct Meter {
    limits: Limits,
    work: usize,
    nodes: usize,
}
#[cfg(not(vesper_port))]
impl Meter {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            work: 0,
            nodes: 0,
        }
    }
    fn charge(&mut self, n: usize) -> Result<()> {
        self.work = add(self.work, n)?;
        bound(self.work, self.limits.max_work, ErrorKind::WorkLimit)
    }
    fn node(&mut self, depth: usize) -> Result<()> {
        self.charge(1)?;
        bound(depth, self.limits.max_depth, ErrorKind::DepthLimit)?;
        self.nodes = add(self.nodes, 1)?;
        bound(self.nodes, self.limits.max_nodes, ErrorKind::NodeLimit)
    }
}
/// Borrowed schema: Sequence is an exact ordered field list; SequenceOf is
/// homogeneous repetition. Tag 17 requires SetOf explicitly; ASN.1 SET is absent.
/// Any accepts only supported primitives and recursively untyped SEQUENCE.
pub enum Schema<'s> {
    Any,
    Boolean,
    Integer,
    BitString,
    OctetString,
    Null,
    ObjectIdentifier,
    Utf8String,
    Ia5String,
    PrintableString,
    Sequence(&'s [Schema<'s>]),
    SequenceOf(&'s Schema<'s>),
    SetOf(&'s Schema<'s>),
}
#[cfg(not(vesper_port))]
fn schema_check(schema: &Schema<'_>, meter: &mut Meter) -> Result<()> {
    fn visit(
        schema: &Schema<'_>,
        depth: usize,
        count: &mut usize,
        meter: &mut Meter,
    ) -> Result<()> {
        meter.charge(1)?;
        bound(depth, meter.limits.max_depth, ErrorKind::DepthLimit)?;
        *count = add(*count, 1)?;
        bound(
            *count,
            meter.limits.max_schema_nodes,
            ErrorKind::SchemaLimit,
        )?;
        match schema {
            Schema::Sequence(fields) => {
                bound(
                    fields.len(),
                    meter.limits.max_schema_nodes,
                    ErrorKind::SchemaLimit,
                )?;
                for field in *fields {
                    visit(field, depth + 1, count, meter)?;
                }
            }
            Schema::SequenceOf(child) | Schema::SetOf(child) => {
                visit(child, depth + 1, count, meter)?
            }
            _ => {}
        }
        Ok(())
    }
    visit(schema, 1, &mut 0, meter)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Boolean,
    Integer,
    BitString,
    OctetString,
    Null,
    ObjectIdentifier,
    Utf8String,
    Ia5String,
    PrintableString,
    Sequence,
    SetOf,
}
impl Kind {
    /// The identifier octet (universal class, so bits 8-7 are zero).
    fn tag(self) -> u8 {
        let number = match self {
            Self::Boolean => BOOLEAN,
            Self::Integer => INTEGER,
            Self::BitString => BIT_STRING,
            Self::OctetString => OCTET_STRING,
            Self::Null => NULL,
            Self::ObjectIdentifier => OBJECT_IDENTIFIER,
            Self::Utf8String => UTF8_STRING,
            Self::Ia5String => IA5_STRING,
            Self::PrintableString => PRINTABLE_STRING,
            Self::Sequence => return CONSTRUCTED | SEQUENCE as u8,
            Self::SetOf => return CONSTRUCTED | SET as u8,
        };
        number as u8
    }
}
#[cfg(not(vesper_port))]
fn matches_schema(kind: Kind, schema: &Schema<'_>) -> bool {
    matches!(
        (kind, schema),
        (Kind::Boolean, Schema::Boolean)
            | (Kind::Integer, Schema::Integer)
            | (Kind::BitString, Schema::BitString)
            | (Kind::OctetString, Schema::OctetString)
            | (Kind::Null, Schema::Null)
            | (Kind::ObjectIdentifier, Schema::ObjectIdentifier)
            | (Kind::Utf8String, Schema::Utf8String)
            | (Kind::Ia5String, Schema::Ia5String)
            | (Kind::PrintableString, Schema::PrintableString)
            | (Kind::Sequence, Schema::Sequence(_) | Schema::SequenceOf(_))
            | (Kind::SetOf, Schema::SetOf(_))
    ) || matches!(schema, Schema::Any) && kind != Kind::SetOf
}
#[derive(Debug)]
struct Record {
    kind: Kind,
    start: usize,
    content: usize,
    end: usize,
    next: usize,
    depth: usize,
}
#[derive(Debug)]
pub struct Document<'a> {
    input: &'a [u8],
    records: Vec<Record>,
}
impl<'a> Document<'a> {
    pub fn raw(&self) -> &'a [u8] {
        self.input
    }
    pub fn node_count(&self) -> usize {
        self.records.len()
    }
    /// Requested flat node-record payload, excluding allocator overhead/capacity.
    pub fn metadata_bytes(&self) -> usize {
        self.records.len() * core::mem::size_of::<Record>()
    }

    pub fn root(&self) -> Node<'_, 'a> {
        Node {
            doc: self,
            index: 0,
        }
    }
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = Node<'_, 'a>> {
        (0..self.records.len()).map(|index| Node { doc: self, index })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Node<'d, 'a> {
    doc: &'d Document<'a>,
    index: usize,
}
impl<'d, 'a> Node<'d, 'a> {
    pub fn kind(self) -> Kind {
        self.doc.records[self.index].kind
    }
    pub fn raw(self) -> &'a [u8] {
        let r = &self.doc.records[self.index];
        &self.doc.input[r.start..r.end]
    }
    pub fn content(self) -> &'a [u8] {
        let r = &self.doc.records[self.index];
        &self.doc.input[r.content..r.end]
    }
    pub fn children(self) -> Children<'d, 'a> {
        Children {
            doc: self.doc,
            index: self.index + 1,
            end: self.doc.records[self.index].next,
        }
    }
    pub fn boolean(self) -> Option<bool> {
        (self.kind() == Kind::Boolean).then(|| self.content()[0] == TRUE)
    }
    pub fn integer_bytes(self) -> Option<&'a [u8]> {
        (self.kind() == Kind::Integer).then(|| self.content())
    }
    #[cfg(not(vesper_port))]
    pub fn as_i64(self) -> Result<i64> {
        let bytes = self
            .integer_bytes()
            .ok_or(error(ErrorKind::SchemaMismatch, 0))?;
        if bytes.len() > 8 {
            return Err(error(ErrorKind::Overflow, 0));
        }
        let mut value = if bytes[0] & SIGN_BIT != 0 { -1_i64 } else { 0 };
        for byte in bytes {
            value = (value << 8) | i64::from(*byte);
        }
        Ok(value)
    }
    #[cfg(vesper_port)]
    pub fn as_i64(self) -> Result<i64> {
        let bytes = self
            .integer_bytes()
            .ok_or(error(ErrorKind::SchemaMismatch, 0))?;
        let value = crate::port::vesper::der_as_i64(bytes).map_err(port::error)?;
        Ok(value as i64)
    }
    pub fn bit_string(self) -> Option<(u8, &'a [u8])> {
        (self.kind() == Kind::BitString).then(|| (self.content()[0], &self.content()[1..]))
    }
    pub fn octets(self) -> Option<&'a [u8]> {
        (self.kind() == Kind::OctetString).then(|| self.content())
    }
    pub fn utf8_string(self) -> Option<&'a str> {
        (self.kind() == Kind::Utf8String)
            .then(|| core::str::from_utf8(self.content()).expect("validated UTF8String"))
    }
    pub fn ia5_string(self) -> Option<&'a str> {
        (self.kind() == Kind::Ia5String)
            .then(|| core::str::from_utf8(self.content()).expect("validated IA5String"))
    }
    pub fn printable_string(self) -> Option<&'a str> {
        (self.kind() == Kind::PrintableString)
            .then(|| core::str::from_utf8(self.content()).expect("validated PrintableString"))
    }
    pub fn oid_bytes(self) -> Option<&'a [u8]> {
        (self.kind() == Kind::ObjectIdentifier).then(|| self.content())
    }
}
pub struct Children<'d, 'a> {
    doc: &'d Document<'a>,
    index: usize,
    end: usize,
}
impl<'d, 'a> Iterator for Children<'d, 'a> {
    type Item = Node<'d, 'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.end {
            return None;
        }
        let node = Node {
            doc: self.doc,
            index: self.index,
        };
        self.index = self.doc.records[self.index].next;
        Some(node)
    }
}
#[cfg(not(vesper_port))]
fn byte(input: &[u8], pos: &mut usize, end: usize, meter: &mut Meter) -> Result<u8> {
    meter.charge(1)?;
    if *pos >= end {
        return Err(error(ErrorKind::UnexpectedEof, *pos));
    }
    let b = input[*pos];
    *pos += 1;
    Ok(b)
}
#[cfg(not(vesper_port))]
fn header(
    input: &[u8],
    start: usize,
    end: usize,
    meter: &mut Meter,
) -> Result<(Kind, usize, usize)> {
    let mut pos = start;
    let first = byte(input, &mut pos, end, meter)?;
    let class = first >> CLASS_SHIFT;
    let constructed = first & CONSTRUCTED != 0;
    let mut number = u64::from(first & TAG_NUMBER_MASK);
    if number == HIGH_TAG {
        number = 0;
        let mut count = 0;
        loop {
            let b = byte(input, &mut pos, end, meter)?;
            if count == 0 && b & SEVEN_BITS == 0 {
                return Err(error(ErrorKind::NonCanonicalTag, pos - 1));
            }
            number = number
                .checked_mul(BASE128)
                .and_then(|n| n.checked_add(u64::from(b & SEVEN_BITS)))
                .ok_or(error(ErrorKind::Overflow, pos - 1))?;
            count += 1;
            if b & MORE == 0 {
                break;
            }
            if count >= MAX_TAG_OCTETS {
                return Err(error(ErrorKind::Overflow, pos - 1));
            }
        }
        if number < HIGH_TAG {
            return Err(error(ErrorKind::NonCanonicalTag, start));
        }
    }
    let length = byte(input, &mut pos, end, meter)?;
    let len = if length < LONG_FORM {
        usize::from(length)
    } else {
        let count = usize::from(length & SEVEN_BITS);
        if count == 0 {
            return Err(error(ErrorKind::IndefiniteLength, pos - 1));
        }
        if count > core::mem::size_of::<usize>() {
            return Err(error(ErrorKind::Overflow, pos - 1));
        }
        let mut len = 0usize;
        for i in 0..count {
            let b = byte(input, &mut pos, end, meter)?;
            if i == 0 && b == 0 {
                return Err(error(ErrorKind::NonCanonicalLength, pos - 1));
            }
            len = len
                .checked_mul(1 << u8::BITS)
                .and_then(|n| n.checked_add(usize::from(b)))
                .ok_or(error(ErrorKind::Overflow, pos - 1))?;
        }
        if len < usize::from(LONG_FORM) {
            return Err(error(ErrorKind::NonCanonicalLength, start));
        }
        len
    };
    bound(len, meter.limits.max_value_bytes, ErrorKind::ValueLimit)?;
    let finish = add(pos, len)?;
    if finish > end {
        return Err(error(ErrorKind::UnexpectedEof, end));
    }
    if class != 0 {
        return Err(error(ErrorKind::UnsupportedTag { class, number }, start));
    }
    if number == 0 {
        return Err(error(ErrorKind::EndOfContents, start));
    }
    let kind = match number {
        BOOLEAN => Kind::Boolean,
        INTEGER => Kind::Integer,
        BIT_STRING => Kind::BitString,
        OCTET_STRING => Kind::OctetString,
        NULL => Kind::Null,
        OBJECT_IDENTIFIER => Kind::ObjectIdentifier,
        UTF8_STRING => Kind::Utf8String,
        PRINTABLE_STRING => Kind::PrintableString,
        IA5_STRING => Kind::Ia5String,
        SEQUENCE => Kind::Sequence,
        SET => Kind::SetOf,
        _ => return Err(error(ErrorKind::UnsupportedTag { class, number }, start)),
    };
    if constructed != matches!(kind, Kind::Sequence | Kind::SetOf) {
        return Err(error(ErrorKind::Constructed, start));
    }
    Ok((kind, pos, finish))
}
#[cfg(not(vesper_port))]
fn primitive(kind: Kind, content: &[u8], offset: usize, meter: &mut Meter) -> Result<()> {
    meter.charge(content.len())?;
    bound(
        content.len(),
        meter.limits.max_value_bytes,
        ErrorKind::ValueLimit,
    )?;
    let invalid = match kind {
        Kind::Boolean => content.len() != 1 || !matches!(content[0], 0 | TRUE),
        Kind::Integer => {
            content.is_empty()
                || content.len() > 1
                    && ((content[0] == 0 && content[1] & SIGN_BIT == 0)
                        || (content[0] == ALL_ONES && content[1] & SIGN_BIT != 0))
        }
        Kind::BitString => {
            content.is_empty()
                || content[0] > MAX_UNUSED_BITS
                || (content.len() == 1 && content[0] != 0)
                || (content.len() > 1
                    && content[content.len() - 1] & ((1u8 << content[0]) - 1) != 0)
        }
        Kind::Null => !content.is_empty(),
        Kind::OctetString => false,
        Kind::Utf8String => core::str::from_utf8(content).is_err(),
        Kind::Ia5String => !content.is_ascii(),
        Kind::PrintableString => !content
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b" '()+,-./:=?".contains(b)),
        Kind::ObjectIdentifier => {
            if content.is_empty() {
                true
            } else {
                let mut value = 0u64;
                let mut first = true;
                for (i, b) in content.iter().copied().enumerate() {
                    if first && b == MORE {
                        return Err(error(ErrorKind::ObjectIdentifier, offset + i));
                    }
                    value = value
                        .checked_mul(BASE128)
                        .and_then(|n| n.checked_add(u64::from(b & SEVEN_BITS)))
                        .ok_or(error(ErrorKind::Overflow, offset + i))?;
                    if b & MORE == 0 {
                        value = 0;
                        first = true;
                    } else {
                        first = false;
                    }
                }
                !first
            }
        }
        Kind::Sequence | Kind::SetOf => return Err(error(ErrorKind::SchemaMismatch, offset)),
    };
    if invalid {
        Err(error(
            match kind {
                Kind::Boolean => ErrorKind::Boolean,
                Kind::Integer => ErrorKind::Integer,
                Kind::BitString => ErrorKind::BitString,
                Kind::ObjectIdentifier => ErrorKind::ObjectIdentifier,
                Kind::Utf8String => ErrorKind::Utf8String,
                Kind::Ia5String => ErrorKind::Ia5String,
                Kind::PrintableString => ErrorKind::PrintableString,
                Kind::Null => ErrorKind::Null,
                _ => ErrorKind::SchemaMismatch,
            },
            offset,
        ))
    } else {
        Ok(())
    }
}
#[cfg(not(vesper_port))]
fn parse_node(
    input: &[u8],
    start: usize,
    end: usize,
    schema: &Schema<'_>,
    depth: usize,
    meter: &mut Meter,
    tape: &mut Option<&mut Vec<Record>>,
) -> Result<usize> {
    meter.node(depth)?;
    let (kind, content, finish) = header(input, start, end, meter)?;
    if kind == Kind::SetOf && !matches!(schema, Schema::SetOf(_)) {
        return Err(error(ErrorKind::SetSchemaRequired, start));
    }
    if !matches_schema(kind, schema) {
        return Err(error(ErrorKind::SchemaMismatch, start));
    }
    let index = if let Some(records) = tape {
        let index = records.len();
        records.push(Record {
            kind,
            start,
            content,
            end: finish,
            next: 0,
            depth,
        });
        index
    } else {
        0
    };
    if matches!(kind, Kind::Sequence | Kind::SetOf) {
        let mut pos = content;
        let mut field = 0;
        let mut previous: Option<(usize, usize)> = None;
        while pos < finish {
            let child_schema = match schema {
                Schema::Sequence(fields) => fields
                    .get(field)
                    .ok_or(error(ErrorKind::SchemaMismatch, pos))?,
                Schema::SequenceOf(child) | Schema::SetOf(child) => child,
                _ => &Schema::Any,
            };
            let next = parse_node(input, pos, finish, child_schema, depth + 1, meter, tape)?;
            if kind == Kind::SetOf {
                if let Some((a, b)) = previous {
                    meter.charge(add(b - a, next - pos)?)?;
                    if input[a..b] > input[pos..next] {
                        return Err(error(ErrorKind::SetOrder, pos));
                    }
                }
                previous = Some((pos, next));
            }
            pos = next;
            field += 1;
        }
        if let Schema::Sequence(fields) = schema
            && field != fields.len()
        {
            return Err(error(ErrorKind::SchemaMismatch, pos));
        }
    } else {
        primitive(kind, &input[content..finish], content, meter)?;
    }
    if let Some(records) = tape {
        let next = records.len();
        records[index].next = next;
    }
    Ok(finish)
}
/// Exact DER prefix extent, with a document borrowing only that validated prefix.
/// The caller retains input and can inspect/parse `&input[consumed..]` explicitly.
#[cfg(not(vesper_port))]
pub fn parse_prefix<'a>(
    input: &'a [u8],
    schema: &Schema<'_>,
    limits: Limits,
) -> Result<(Document<'a>, usize)> {
    limits.validate()?;
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    let mut meter = Meter::new(limits);
    schema_check(schema, &mut meter)?;
    let before_parse = meter.work;
    let consumed = parse_node(input, 0, input.len(), schema, 1, &mut meter, &mut None)?;
    // Both deterministic passes are admitted before allocating metadata.
    let parse_work = meter.work - before_parse;
    bound(
        add(meter.work, parse_work)?,
        limits.max_work,
        ErrorKind::WorkLimit,
    )?;
    let count = meter.nodes;
    bound(
        mul(count, core::mem::size_of::<Record>())?,
        limits.max_metadata_bytes,
        ErrorKind::MetadataLimit,
    )?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(count)
        .map_err(|_| error(ErrorKind::AllocationFailed, 0))?;
    meter.nodes = 0;
    parse_node(
        input,
        0,
        consumed,
        schema,
        1,
        &mut meter,
        &mut Some(&mut records),
    )?;
    Ok((
        Document {
            input: &input[..consumed],
            records,
        },
        consumed,
    ))
}
#[cfg(vesper_port)]
pub fn parse_prefix<'a>(
    input: &'a [u8],
    schema: &Schema<'_>,
    limits: Limits,
) -> Result<(Document<'a>, usize)> {
    let sch = port::flatten_schema(schema);
    let words = crate::port::vesper::der_parse_prefix(input, &sch, &port::limits(limits))
        .map_err(port::error)?;
    let (consumed, recs) = words.split_last().expect("consumed length");
    let consumed = *consumed as usize;
    let records = recs
        .as_chunks::<6>()
        .0
        .iter()
        .map(|r| Record {
            kind: port::KINDS[r[0] as usize],
            start: r[1] as usize,
            content: r[2] as usize,
            end: r[3] as usize,
            next: r[4] as usize,
            depth: r[5] as usize,
        })
        .collect();
    Ok((
        Document {
            input: &input[..consumed],
            records,
        },
        consumed,
    ))
}
pub fn parse_exact<'a>(
    input: &'a [u8],
    schema: &Schema<'_>,
    limits: Limits,
) -> Result<Document<'a>> {
    let (document, consumed) = parse_prefix(input, schema, limits)?;
    if consumed != input.len() {
        return Err(error(ErrorKind::TrailingBytes, consumed));
    }
    Ok(document)
}
/// Primitive bytes must be canonical for their declared type. SET OF input order
/// is arbitrary: the encoder sorts complete canonical child encodings. Duplicates
/// are retained (multiset semantics). No ASN.1 SET variant is provided.
pub enum Value<'a> {
    Boolean(bool),
    Integer(&'a [u8]),
    BitString { unused_bits: u8, bytes: &'a [u8] },
    OctetString(&'a [u8]),
    Null,
    ObjectIdentifier(&'a [u8]),
    Utf8String(&'a str),
    Ia5String(&'a str),
    PrintableString(&'a str),
    Sequence(&'a [Value<'a>]),
    SetOf(&'a [Value<'a>]),
}
impl Value<'_> {
    fn kind(&self) -> Kind {
        match self {
            Self::Boolean(_) => Kind::Boolean,
            Self::Integer(_) => Kind::Integer,
            Self::BitString { .. } => Kind::BitString,
            Self::OctetString(_) => Kind::OctetString,
            Self::Null => Kind::Null,
            Self::ObjectIdentifier(_) => Kind::ObjectIdentifier,
            Self::Utf8String(_) => Kind::Utf8String,
            Self::Ia5String(_) => Kind::Ia5String,
            Self::PrintableString(_) => Kind::PrintableString,
            Self::Sequence(_) => Kind::Sequence,
            Self::SetOf(_) => Kind::SetOf,
        }
    }
}
#[cfg(not(vesper_port))]
fn length_octets(len: usize) -> usize {
    if len < usize::from(LONG_FORM) {
        1
    } else {
        1 + (usize::BITS - len.leading_zeros()).div_ceil(8) as usize
    }
}
#[cfg(not(vesper_port))]
fn tlv_size(len: usize) -> Result<usize> {
    add(add(1, length_octets(len))?, len)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodingPlan {
    pub encoded_bytes: usize,
    pub nodes: usize,
    pub metadata_bytes: usize,
    pub intermediate_bytes: usize,
}
#[cfg(not(vesper_port))]
fn measure(
    value: &Value<'_>,
    schema: &Schema<'_>,
    depth: usize,
    meter: &mut Meter,
    sum: &mut usize,
) -> Result<usize> {
    meter.node(depth)?;
    let kind = value.kind();
    if kind == Kind::SetOf && !matches!(schema, Schema::SetOf(_)) {
        return Err(error(ErrorKind::SetSchemaRequired, 0));
    }
    if !matches_schema(kind, schema) {
        return Err(error(ErrorKind::SchemaMismatch, 0));
    }
    let content = match value {
        Value::Boolean(_) => {
            meter.charge(1)?;
            1
        }
        Value::Integer(bytes) | Value::ObjectIdentifier(bytes) | Value::OctetString(bytes) => {
            primitive(kind, bytes, 0, meter)?;
            bytes.len()
        }
        Value::Utf8String(text) | Value::Ia5String(text) | Value::PrintableString(text) => {
            primitive(kind, text.as_bytes(), 0, meter)?;
            text.len()
        }
        Value::Null => 0,
        Value::BitString { unused_bits, bytes } => {
            meter.charge(bytes.len())?;
            if *unused_bits > 7
                || bytes.is_empty() && *unused_bits != 0
                || !bytes.is_empty() && bytes[bytes.len() - 1] & ((1u8 << unused_bits) - 1) != 0
            {
                return Err(error(ErrorKind::BitString, 0));
            }
            add(1, bytes.len())?
        }
        Value::Sequence(values) | Value::SetOf(values) => {
            bound(values.len(), meter.limits.max_nodes, ErrorKind::NodeLimit)?;
            if let Schema::Sequence(fields) = schema
                && fields.len() != values.len()
            {
                return Err(error(ErrorKind::SchemaMismatch, 0));
            }
            let mut size = 0;
            for (i, child) in values.iter().enumerate() {
                let child_schema = match schema {
                    Schema::Sequence(fields) => &fields[i],
                    Schema::SequenceOf(child) | Schema::SetOf(child) => child,
                    _ => &Schema::Any,
                };
                size = add(size, measure(child, child_schema, depth + 1, meter, sum)?)?;
            }
            if kind == Kind::SetOf {
                meter.charge(mul(values.len(), add(size, values.len())?)?)?;
            }
            size
        }
    };
    bound(content, meter.limits.max_value_bytes, ErrorKind::ValueLimit)?;
    let size = tlv_size(content)?;
    *sum = add(*sum, size)?;
    bound(
        *sum,
        meter.limits.max_intermediate_bytes,
        ErrorKind::IntermediateLimit,
    )?;
    Ok(size)
}
#[cfg(not(vesper_port))]
pub fn encoding_plan(
    value: &Value<'_>,
    schema: &Schema<'_>,
    limits: Limits,
) -> Result<EncodingPlan> {
    limits.validate()?;
    let mut meter = Meter::new(limits);
    schema_check(schema, &mut meter)?;
    let mut intermediate = 0;
    let size = measure(value, schema, 1, &mut meter, &mut intermediate)?;
    bound(size, limits.max_input_bytes, ErrorKind::InputLimit)?;
    bound(size, limits.max_output_bytes, ErrorKind::OutputLimit)?;
    let metadata = mul(meter.nodes, ENCODE_METADATA)?;
    bound(
        metadata,
        limits.max_metadata_bytes,
        ErrorKind::MetadataLimit,
    )?;
    meter.charge(intermediate)?;
    meter.charge(size)?;
    Ok(EncodingPlan {
        encoded_bytes: size,
        nodes: meter.nodes,
        metadata_bytes: metadata,
        intermediate_bytes: intermediate,
    })
}
#[cfg(vesper_port)]
pub fn encoding_plan(
    value: &Value<'_>,
    schema: &Schema<'_>,
    limits: Limits,
) -> Result<EncodingPlan> {
    let (vals, arena) = port::flatten_value(value);
    let sch = port::flatten_schema(schema);
    let p = crate::port::vesper::der_encoding_plan(&vals, &arena, &sch, &port::limits(limits))
        .map_err(port::error)?;
    Ok(EncodingPlan {
        encoded_bytes: p.a as usize,
        nodes: p.b as usize,
        metadata_bytes: p.c as usize,
        intermediate_bytes: p.d as usize,
    })
}
pub fn encoded_len(value: &Value<'_>, schema: &Schema<'_>, limits: Limits) -> Result<usize> {
    Ok(encoding_plan(value, schema, limits)?.encoded_bytes)
}
#[cfg(not(vesper_port))]
fn reserve<T>(count: usize) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| error(ErrorKind::AllocationFailed, 0))?;
    Ok(out)
}
#[cfg(not(vesper_port))]
fn push_header(out: &mut Vec<u8>, kind: Kind, len: usize) {
    out.push(kind.tag());
    if len < usize::from(LONG_FORM) {
        out.push(len as u8);
    } else {
        let count = length_octets(len) - 1;
        out.push(LONG_FORM | count as u8);
        for i in (0..count).rev() {
            out.push((len >> (i * 8)) as u8);
        }
    }
}
#[cfg(not(vesper_port))]
fn build(value: &Value<'_>) -> Result<Vec<u8>> {
    let kind = value.kind();
    let mut children: Vec<Vec<u8>> = match value {
        Value::Sequence(values) | Value::SetOf(values) => reserve(values.len())?,
        _ => Vec::new(),
    };
    if let Value::Sequence(values) | Value::SetOf(values) = value {
        for child in *values {
            children.push(build(child)?);
        }
    }
    if kind == Kind::SetOf {
        for i in 1..children.len() {
            let mut j = i;
            while j > 0 && children[j].cmp(&children[j - 1]) == Ordering::Less {
                children.swap(j, j - 1);
                j -= 1;
            }
        }
    }
    let content = match value {
        Value::Boolean(_) => 1,
        Value::Integer(b) | Value::ObjectIdentifier(b) | Value::OctetString(b) => b.len(),
        Value::Utf8String(text) | Value::Ia5String(text) | Value::PrintableString(text) => {
            text.len()
        }
        Value::Null => 0,
        Value::BitString { bytes, .. } => add(bytes.len(), 1)?,
        _ => children.iter().try_fold(0, |n, c| add(n, c.len()))?,
    };
    let mut out = reserve(tlv_size(content)?)?;
    push_header(&mut out, kind, content);
    match value {
        Value::Boolean(b) => out.push(if *b { TRUE } else { 0 }),
        Value::Integer(b) | Value::ObjectIdentifier(b) | Value::OctetString(b) => {
            out.extend_from_slice(b)
        }
        Value::Utf8String(text) | Value::Ia5String(text) | Value::PrintableString(text) => {
            out.extend_from_slice(text.as_bytes())
        }
        Value::Null => {}
        Value::BitString { unused_bits, bytes } => {
            out.push(*unused_bits);
            out.extend_from_slice(bytes);
        }
        _ => {
            for child in children {
                out.extend_from_slice(&child);
            }
        }
    }
    Ok(out)
}
/// All validation, size/work/schema/node/metadata/intermediate limits and output
/// capacity checks precede allocation. All returned failures leave output intact.
#[cfg(not(vesper_port))]
pub fn encode_into(
    value: &Value<'_>,
    schema: &Schema<'_>,
    output: &mut [u8],
    limits: Limits,
) -> Result<usize> {
    let plan = encoding_plan(value, schema, limits)?;
    if output.len() < plan.encoded_bytes {
        return Err(error(ErrorKind::OutputTooSmall, 0));
    }
    let encoded = build(value)?;
    output[..encoded.len()].copy_from_slice(&encoded);
    Ok(encoded.len())
}
#[cfg(vesper_port)]
pub fn encode_into(
    value: &Value<'_>,
    schema: &Schema<'_>,
    output: &mut [u8],
    limits: Limits,
) -> Result<usize> {
    let (vals, arena) = port::flatten_value(value);
    let sch = port::flatten_schema(schema);
    let n =
        crate::port::vesper::der_encode_into(&vals, &arena, &sch, &port::limits(limits), output)
            .map_err(port::error)?;
    Ok(n as usize)
}
#[cfg(not(vesper_port))]
fn build_node(node: Node<'_, '_>) -> Result<Vec<u8>> {
    let mut children: Vec<Vec<u8>> = reserve(node.children().count())?;
    for child in node.children() {
        children.push(build_node(child)?);
    }
    let constructed = matches!(node.kind(), Kind::Sequence | Kind::SetOf);
    let content = if constructed {
        children
            .iter()
            .try_fold(0, |n, child| add(n, child.len()))?
    } else {
        node.content().len()
    };
    let mut out = reserve(tlv_size(content)?)?;
    push_header(&mut out, node.kind(), content);
    if constructed {
        for child in children {
            out.extend_from_slice(&child);
        }
    } else {
        out.extend_from_slice(node.content());
    }
    Ok(out)
}
/// Reconstructs canonical tags/lengths and recursively re-encodes the validated
/// node tree. Typed SET OF evidence is retained by Document; no schema is newly
/// supplied or walked here. Budgets apply again before allocation/output writes.
#[cfg(not(vesper_port))]
pub fn reencode_into(document: &Document<'_>, output: &mut [u8], limits: Limits) -> Result<usize> {
    limits.validate()?;
    bound(
        document.input.len(),
        limits.max_input_bytes,
        ErrorKind::InputLimit,
    )?;
    bound(
        document.input.len(),
        limits.max_output_bytes,
        ErrorKind::OutputLimit,
    )?;
    bound(
        document.records.len(),
        limits.max_nodes,
        ErrorKind::NodeLimit,
    )?;
    bound(
        mul(
            document.records.len(),
            ENCODE_METADATA.max(core::mem::size_of::<Record>()),
        )?,
        limits.max_metadata_bytes,
        ErrorKind::MetadataLimit,
    )?;
    let mut intermediate = 0;
    for record in &document.records {
        bound(record.depth, limits.max_depth, ErrorKind::DepthLimit)?;
        bound(
            record.end - record.content,
            limits.max_value_bytes,
            ErrorKind::ValueLimit,
        )?;
        intermediate = add(intermediate, record.end - record.start)?;
        bound(
            intermediate,
            limits.max_intermediate_bytes,
            ErrorKind::IntermediateLimit,
        )?;
    }
    bound(
        add(
            add(document.input.len(), intermediate)?,
            document.records.len(),
        )?,
        limits.max_work,
        ErrorKind::WorkLimit,
    )?;
    if output.len() < document.input.len() {
        return Err(error(ErrorKind::OutputTooSmall, 0));
    }
    let encoded = build_node(document.root())?;
    output[..encoded.len()].copy_from_slice(&encoded);
    Ok(encoded.len())
}
#[cfg(vesper_port)]
pub fn reencode_into(document: &Document<'_>, output: &mut [u8], limits: Limits) -> Result<usize> {
    let mut recs = Vec::with_capacity(document.records.len() * 6);
    for r in &document.records {
        recs.extend([
            r.kind as u64,
            r.start as u64,
            r.content as u64,
            r.end as u64,
            r.next as u64,
            r.depth as u64,
        ]);
    }
    let n = crate::port::vesper::der_reencode_into(
        &recs,
        document.input,
        &port::limits(limits),
        output,
    )
    .map_err(port::error)?;
    Ok(n as usize)
}

/// The port-backed build's DER glue: schema and value graphs (shared nodes
/// once, node 0 the root), limits and failures. See `vesper/src/der.ves`.
#[cfg(vesper_port)]
mod port {
    use super::{ENCODE_METADATA, Error, ErrorKind, Kind, Limits, Record, Schema, Value};
    use crate::port_binding::GErr;
    use std::collections::HashMap;

    pub(super) const KINDS: [Kind; 11] = [
        Kind::Boolean,
        Kind::Integer,
        Kind::BitString,
        Kind::OctetString,
        Kind::Null,
        Kind::ObjectIdentifier,
        Kind::Utf8String,
        Kind::Ia5String,
        Kind::PrintableString,
        Kind::Sequence,
        Kind::SetOf,
    ];

    const ERRORS: [ErrorKind; 31] = [
        ErrorKind::InvalidLimits,
        ErrorKind::InputLimit,
        ErrorKind::ValueLimit,
        ErrorKind::NodeLimit,
        ErrorKind::SchemaLimit,
        ErrorKind::DepthLimit,
        ErrorKind::WorkLimit,
        ErrorKind::MetadataLimit,
        ErrorKind::OutputLimit,
        ErrorKind::IntermediateLimit,
        ErrorKind::AllocationFailed,
        ErrorKind::Overflow,
        ErrorKind::UnexpectedEof,
        ErrorKind::TrailingBytes,
        ErrorKind::IndefiniteLength,
        ErrorKind::NonCanonicalTag,
        ErrorKind::NonCanonicalLength,
        ErrorKind::Constructed,
        ErrorKind::Boolean,
        ErrorKind::Integer,
        ErrorKind::BitString,
        ErrorKind::ObjectIdentifier,
        ErrorKind::Utf8String,
        ErrorKind::Ia5String,
        ErrorKind::PrintableString,
        ErrorKind::Null,
        ErrorKind::SetOrder,
        ErrorKind::SchemaMismatch,
        ErrorKind::SetSchemaRequired,
        ErrorKind::EndOfContents,
        ErrorKind::OutputTooSmall,
    ];

    pub(super) fn error(e: GErr) -> Error {
        let kind = match ERRORS.get(e.kind as usize) {
            Some(kind) => *kind,
            None => ErrorKind::UnsupportedTag {
                class: e.a as u8,
                number: e.b,
            },
        };
        super::error(kind, e.offset as usize)
    }

    pub(super) fn limits(l: Limits) -> [u64; 11] {
        [
            l.max_input_bytes as u64,
            l.max_value_bytes as u64,
            l.max_nodes as u64,
            l.max_schema_nodes as u64,
            l.max_depth as u64,
            l.max_work as u64,
            l.max_metadata_bytes as u64,
            l.max_output_bytes as u64,
            l.max_intermediate_bytes as u64,
            core::mem::size_of::<Record>() as u64,
            ENCODE_METADATA as u64,
        ]
    }

    /// Numbers nodes by address in discovery order (iteratively, so deep
    /// caller graphs cannot exhaust the stack) and writes each record.
    fn graph<'n, T>(
        root: &'n T,
        mut record: impl FnMut(&'n T, &mut Vec<&'n T>, &mut Vec<u64>),
    ) -> Vec<u64> {
        let mut ids: HashMap<*const T, u64> = HashMap::new();
        let mut order: Vec<&'n T> = vec![root];
        ids.insert(root, 0);
        let mut bodies: Vec<u64> = Vec::new();
        let mut offsets: Vec<u64> = Vec::new();
        let mut children: Vec<&'n T> = Vec::new();
        let mut i = 0;
        while i < order.len() {
            let node = order[i];
            offsets.push(bodies.len() as u64);
            children.clear();
            let at = bodies.len();
            record(node, &mut children, &mut bodies);
            // Child ids follow the record's operand words in order.
            let first = bodies.len() - children.len();
            debug_assert!(first >= at);
            for (slot, child) in bodies[first..].iter_mut().zip(children.drain(..)) {
                let next = order.len() as u64;
                let id = *ids.entry(child as *const T).or_insert(next);
                if id == next {
                    order.push(child);
                }
                *slot = id;
            }
            i += 1;
        }
        let n = order.len();
        let mut words = Vec::with_capacity(1 + n + bodies.len());
        words.push(n as u64);
        words.extend(offsets.iter().map(|o| o + 1 + n as u64));
        words.extend(bodies);
        words
    }

    pub(super) fn flatten_schema(schema: &Schema<'_>) -> Vec<u64> {
        graph(schema, |s, children, out| match s {
            Schema::Sequence(fields) => {
                out.extend([10, fields.len() as u64]);
                for f in *fields {
                    out.push(0);
                    children.push(f);
                }
            }
            Schema::SequenceOf(child) | Schema::SetOf(child) => {
                out.push(if matches!(s, Schema::SetOf(_)) {
                    12
                } else {
                    11
                });
                out.push(0);
                children.push(child);
            }
            other => out.push(match other {
                Schema::Any => 0,
                Schema::Boolean => 1,
                Schema::Integer => 2,
                Schema::BitString => 3,
                Schema::OctetString => 4,
                Schema::Null => 5,
                Schema::ObjectIdentifier => 6,
                Schema::Utf8String => 7,
                Schema::Ia5String => 8,
                _ => 9,
            }),
        })
    }

    /// The value graph and the arena of its primitive payloads.
    pub(super) fn flatten_value(value: &Value<'_>) -> (Vec<u64>, Vec<u8>) {
        // Shared payload slices are copied once.
        let mut arena = crate::port::Arena::new(true);
        let words = graph(value, |v, children, out| {
            out.push(v.kind() as u64);
            let mut bytes = |out: &mut Vec<u64>, b: &[u8]| out.extend(arena.put(b));
            match v {
                Value::Boolean(b) => out.push(u64::from(*b)),
                Value::Null => {}
                Value::BitString {
                    unused_bits,
                    bytes: b,
                } => {
                    out.push(u64::from(*unused_bits));
                    bytes(out, b);
                }
                Value::Integer(b) | Value::OctetString(b) | Value::ObjectIdentifier(b) => {
                    bytes(out, b)
                }
                Value::Utf8String(t) | Value::Ia5String(t) | Value::PrintableString(t) => {
                    bytes(out, t.as_bytes())
                }
                Value::Sequence(items) | Value::SetOf(items) => {
                    out.push(items.len() as u64);
                    for item in *items {
                        out.push(0);
                        children.push(item);
                    }
                }
            }
        });
        (words, arena.bytes)
    }
}
