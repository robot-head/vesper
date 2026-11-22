//! The GGUF container (versions 2 and 3): reading from memory or a read-only
//! mapping, and writing.
//!
//! Layout: magic `GGUF`, `u32` version, `u64` tensor count, `u64` metadata
//! count, the metadata key/value pairs, the tensor infos (name, dimensions
//! with `ne[0]` the contiguous one, GGML type, offset), padding to the
//! alignment (`general.alignment`, default 32), then the tensor data. All
//! integers are little-endian.
//!
//! The reader bounds every count and length, rejects duplicate keys and
//! tensor names, misaligned or overlapping tensor data, invalid UTF-8 and
//! truncation with typed errors, and never allocates more than the input can
//! justify. Tensor bytes are not copied: [`Gguf::matrix`] borrows them from
//! the mapping.
use std::path::Path;
use std::sync::Arc;

use crate::mmap::{Mapping, WeightBytes};
use crate::quant::{Matrix, WeightFormat};
use crate::Error;

pub const MAGIC: &[u8; 4] = b"GGUF";
pub const DEFAULT_ALIGNMENT: usize = 32;
pub const MAX_TENSORS: u64 = 1 << 20;
pub const MAX_METADATA: u64 = 1 << 20;
pub const MAX_ARRAY: u64 = 1 << 24;
pub const MAX_STRING: u64 = 1 << 24;
pub const MAX_DIMS: u32 = 4;

/// A GGML tensor type id with its block geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GgmlType(pub u32);
impl GgmlType {
    // Ids from ggml.h `enum ggml_type`.
    pub const F32: Self = Self(0);
    pub const F16: Self = Self(1);
    pub const Q4_0: Self = Self(2);
    pub const Q4_1: Self = Self(3);
    pub const Q5_0: Self = Self(6);
    pub const Q5_1: Self = Self(7);
    pub const Q8_0: Self = Self(8);
    pub const Q8_1: Self = Self(9);
    pub const Q2_K: Self = Self(10);
    pub const Q3_K: Self = Self(11);
    pub const Q4_K: Self = Self(12);
    pub const Q5_K: Self = Self(13);
    pub const Q6_K: Self = Self(14);
    pub const Q8_K: Self = Self(15);
    pub const I8: Self = Self(24);
    pub const I16: Self = Self(25);
    pub const I32: Self = Self(26);
    pub const I64: Self = Self(27);
    pub const F64: Self = Self(28);
    pub const BF16: Self = Self(30);
    pub const MXFP4: Self = Self(39);

    /// `(elements per block, bytes per block)` for every type this reader
    /// can lay out, whether or not it can compute with it.
    pub fn geometry(self) -> Option<(usize, usize)> {
        // Block sizes from ggml.c `type_traits` (`blck_size`, `type_size`).
        const QK: usize = 32;
        Some(match self {
            Self::F32 => (1, 4),
            Self::F16 => (1, 2),
            Self::Q4_0 => (QK, 18),
            Self::Q4_1 => (QK, 20),
            Self::Q5_0 => (QK, 22),
            Self::Q5_1 => (QK, 24),
            Self::Q8_0 => (QK, 34),
            Self::Q8_1 => (QK, 36),
            Self::Q2_K => (crate::formats::QK_K, 84),
            Self::Q3_K => (crate::formats::QK_K, 110),
            Self::Q4_K => (crate::formats::QK_K, 144),
            Self::Q5_K => (crate::formats::QK_K, 176),
            Self::Q6_K => (crate::formats::QK_K, 210),
            Self::Q8_K => (crate::formats::QK_K, 292),
            Self::I8 => (1, 1),
            Self::I16 => (1, 2),
            Self::I32 => (1, 4),
            Self::I64 => (1, 8),
            Self::F64 => (1, 8),
            Self::BF16 => (1, 2),
            Self::MXFP4 => (QK, 17),
            _ => return None,
        })
    }
    /// The [`WeightFormat`] for types this crate computes with.
    pub fn weight_format(self) -> Result<WeightFormat, Error> {
        Ok(match self {
            Self::F32 => WeightFormat::F32,
            Self::F16 => WeightFormat::F16,
            Self::BF16 => WeightFormat::Bf16,
            Self::Q4_0 => WeightFormat::Q4_0,
            Self::Q8_0 => WeightFormat::Q8_0,
            Self::Q4_K => WeightFormat::Q4K,
            Self::MXFP4 => WeightFormat::Mxfp4,
            Self(other) => {
                return Err(Error::Unsupported(format!("GGML tensor type {other}")));
            }
        })
    }
    pub fn from_weight_format(format: WeightFormat) -> Result<Self, Error> {
        Ok(match format {
            WeightFormat::F32 => Self::F32,
            WeightFormat::F16 => Self::F16,
            WeightFormat::Bf16 => Self::BF16,
            WeightFormat::Q4_0 => Self::Q4_0,
            WeightFormat::Q8_0 => Self::Q8_0,
            WeightFormat::Q4K => Self::Q4_K,
            WeightFormat::Mxfp4 => Self::MXFP4,
            WeightFormat::F8E4M3 | WeightFormat::F8E5M2 => {
                return Err(Error::Unsupported("GGUF has no FP8 tensor type".into()));
            }
        })
    }
}

/// A metadata value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    F32(f32),
    Bool(bool),
    String(String),
    Array(Vec<Value>),
    U64(u64),
    I64(i64),
    F64(f64),
}
impl Value {
    const fn type_id(&self) -> u32 {
        match self {
            Self::U8(_) => 0,
            Self::I8(_) => 1,
            Self::U16(_) => 2,
            Self::I16(_) => 3,
            Self::U32(_) => 4,
            Self::I32(_) => 5,
            Self::F32(_) => 6,
            Self::Bool(_) => 7,
            Self::String(_) => 8,
            Self::Array(_) => 9,
            Self::U64(_) => 10,
            Self::I64(_) => 11,
            Self::F64(_) => 12,
        }
    }
    /// Any non-negative integer value as `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Self::U8(v) => Some(v.into()),
            Self::U16(v) => Some(v.into()),
            Self::U32(v) => Some(v.into()),
            Self::U64(v) => Some(v),
            Self::I8(v) => u64::try_from(v).ok(),
            Self::I16(v) => u64::try_from(v).ok(),
            Self::I32(v) => u64::try_from(v).ok(),
            Self::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Self::F32(v) => Some(v),
            Self::F64(v) => Some(v as f32),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorInfo {
    pub name: String,
    /// GGML order: `dims[0]` is the contiguous (column) dimension.
    pub dims: Vec<usize>,
    pub ty: GgmlType,
    /// Absolute byte offset in the file.
    pub offset: usize,
    pub len: usize,
}

/// A parsed GGUF file whose tensor bytes stay in the mapping.
#[derive(Debug)]
pub struct Gguf {
    mapping: Arc<Mapping>,
    version: u32,
    alignment: usize,
    metadata: Vec<(String, Value)>,
    tensors: Vec<TensorInfo>,
}

impl Gguf {
    /// Maps `path` read-only and parses it.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::parse(Mapping::open(path)?)
    }
    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::parse(Mapping::from_vec(bytes))
    }
    /// Checks the header and lays out the tensors. The package
    /// `vesper-tensor` reads the header with the Vesper port
    /// (`vesper/src/store.ves`); the seed copy the compiler links runs the
    /// frozen Rust reader in `seed/src/engine/gguf.rs`.
    pub fn parse(mapping: Arc<Mapping>) -> Result<Self, Error> {
        #[cfg(vesper_port)]
        {
            Self::port_parse(mapping)
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::gguf::parse(mapping)
        }
    }
    /// The parts of a checked file, for the seed's Rust reader.
    #[cfg(not(vesper_port))]
    pub(crate) fn from_parts(
        mapping: Arc<Mapping>,
        version: u32,
        alignment: usize,
        metadata: Vec<(String, Value)>,
        tensors: Vec<TensorInfo>,
    ) -> Self {
        Self {
            mapping,
            version,
            alignment,
            metadata,
            tensors,
        }
    }
    /// The port-backed reader: `vesper/src/store.ves` checks the header and
    /// writes it as a tape, which this turns into metadata and tensor infos.
    /// The tape and the name table grow when the port asks for more room.
    #[cfg(vesper_port)]
    fn port_parse(mapping: Arc<Mapping>) -> Result<Self, Error> {
        let bytes = mapping.bytes();
        // Start the name table at the header's key and tensor counts, which
        // each take at least 8 bytes of input, and the tape at a size most
        // headers fit; either grows when the port asks.
        let count = |at: usize| {
            bytes
                .get(at..at + 8)
                .map_or(0, |b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
        };
        // The counts are not checked yet, so the first sizes are also
        // capped (a corrupt count must fail with the reader's error, not a
        // huge allocation); a larger header makes the port ask for more.
        const FIRST_ENTRIES: u64 = 1 << 12;
        let entries = |at: usize| count(at).min(bytes.len() as u64 / 8).min(FIRST_ENTRIES) as usize;
        let mut slots = (2 * (entries(8) + entries(16)) + 2).next_power_of_two();
        // 6 words per metadata entry, 10 per tensor of rank 4, and 2 more
        // of scratch per tensor.
        let mut words = 4 + 6 * entries(16) + 12 * entries(8);
        let tape = loop {
            let mut tape = vec![0u64; words];
            let mut names = vec![0u64; 2 * slots];
            let mut status = [0u64; 3];
            let r = crate::port::binding::parse_gguf(
                bytes,
                b"general.alignment",
                &mut tape,
                &mut names,
                &mut status,
            );
            if r.is_err() && status[0] == 30 {
                words *= 2;
                continue;
            }
            if r.is_err() && status[0] == 31 {
                slots *= 2;
                continue;
            }
            let n = crate::port::check(r, |_| port_failure(status))?;
            tape.truncate(n as usize);
            break tape;
        };
        Ok(from_tape(mapping.clone(), &tape))
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn alignment(&self) -> usize {
        self.alignment
    }
    pub fn metadata(&self) -> &[(String, Value)] {
        &self.metadata
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn tensors(&self) -> &[TensorInfo] {
        &self.tensors
    }
    pub fn tensor(&self, name: &str) -> Option<&TensorInfo> {
        self.tensors.iter().find(|t| t.name == name)
    }
    pub fn mapping(&self) -> &Arc<Mapping> {
        &self.mapping
    }
    pub fn bytes(&self, info: &TensorInfo) -> &[u8] {
        &self.mapping.bytes()[info.offset..info.offset + info.len]
    }
    /// A 2-D tensor (`dims = [cols, rows]`) as a zero-copy [`Matrix`].
    pub fn matrix(&self, name: &str) -> Result<Matrix, Error> {
        let info = self
            .tensor(name)
            .ok_or_else(|| Error::MissingTensor(name.to_string()))?;
        let [cols, rows] = info.dims.as_slice() else {
            return Err(Error::Shape("GGUF matrix must be 2-D"));
        };
        Matrix::from_bytes(
            info.ty.weight_format()?,
            *rows,
            *cols,
            WeightBytes::range(&self.mapping, info.offset, info.len)?,
            1.0,
        )
    }
    /// A 1-D tensor decoded to FP32 (copied).
    pub fn vector(&self, name: &str) -> Result<Vec<f32>, Error> {
        let info = self
            .tensor(name)
            .ok_or_else(|| Error::MissingTensor(name.to_string()))?;
        let [n] = info.dims.as_slice() else {
            return Err(Error::Shape("GGUF vector must be 1-D"));
        };
        Matrix::from_bytes(
            info.ty.weight_format()?,
            1,
            *n,
            WeightBytes::range(&self.mapping, info.offset, info.len)?,
            1.0,
        )?
        .to_f32()
    }
}

/// The [`Error`] for a failure code of `vesper/src/store.ves`.
#[cfg(vesper_port)]
fn port_failure(status: [u64; 3]) -> Error {
    let [code, a, b] = status;
    match code {
        2 => Error::Malformed("not a GGUF file"),
        3 => Error::Unsupported(format!("GGUF version {a}")),
        4 => Error::Limit("GGUF tensors"),
        5 => Error::Limit("GGUF metadata entries"),
        6 => Error::Limit("GGUF string length"),
        7 => Error::Malformed("GGUF string is not UTF-8"),
        8 => Error::Malformed("GGUF bool"),
        9 => Error::Unsupported("nested GGUF arrays".into()),
        10 => Error::Limit("GGUF array length"),
        11 => Error::Malformed("truncated GGUF array"),
        12 => Error::Malformed("unknown GGUF metadata type"),
        13 => Error::Malformed("duplicate GGUF metadata key"),
        14 => Error::Malformed("general.alignment"),
        15 => Error::Malformed("GGUF tensor rank"),
        16 => Error::Malformed("GGUF zero dimension"),
        17 => Error::Malformed("duplicate GGUF tensor name"),
        18 => Error::Unsupported(format!("GGML tensor type {a}")),
        19 => Error::QuantBlockAlignment {
            cols: a as usize,
            block: b as usize,
        },
        20 => Error::Malformed("misaligned GGUF tensor"),
        21 => Error::Malformed("GGUF tensor data outside the file"),
        22 => Error::Malformed("overlapping GGUF tensors"),
        _ => Error::Malformed("truncated GGUF"),
    }
}

/// Builds the [`Gguf`] a checked tape describes (see `store.ves`).
#[cfg(vesper_port)]
fn from_tape(mapping: Arc<Mapping>, tape: &[u64]) -> Gguf {
    let bytes = mapping.bytes();
    let string = |at: usize| {
        let len = (tape[at] >> 8) as usize;
        let start = tape[at + 1] as usize;
        String::from_utf8(bytes[start..start + len].to_vec()).expect("the port checked UTF-8")
    };
    let scalar = |ty: u64, v: u64| match ty {
        0 => Value::U8(v as u8),
        1 => Value::I8(v as u8 as i8),
        2 => Value::U16(v as u16),
        3 => Value::I16(v as u16 as i16),
        4 => Value::U32(v as u32),
        5 => Value::I32(v as u32 as i32),
        6 => Value::F32(f32::from_bits(v as u32)),
        7 => Value::Bool(v == 1),
        10 => Value::U64(v),
        11 => Value::I64(v as i64),
        _ => Value::F64(f64::from_bits(v)),
    };
    let version = tape[0] as u32;
    let alignment = tape[1] as usize;
    let (kv_count, tensor_count) = (tape[2] as usize, tape[3] as usize);
    let mut at = 4;
    // Array elements, which the port checked in place: strings are a
    // little-endian `u64` length and the bytes, scalars their width.
    let element = |ty: u64, at: &mut usize| {
        if ty == 8 {
            let len = u64::from_le_bytes(bytes[*at..*at + 8].try_into().expect("8 bytes")) as usize;
            let s = String::from_utf8(bytes[*at + 8..*at + 8 + len].to_vec())
                .expect("the port checked UTF-8");
            *at += 8 + len;
            return Value::String(s);
        }
        let width = match ty {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            _ => 8,
        };
        let mut word = [0u8; 8];
        word[..width].copy_from_slice(&bytes[*at..*at + width]);
        *at += width;
        scalar(ty, u64::from_le_bytes(word))
    };
    let mut metadata = Vec::with_capacity(kv_count);
    for _ in 0..kv_count {
        let key = string(at);
        at += 2;
        let (tag, payload) = (tape[at], tape[at + 1]);
        let value = match tag & 255 {
            9 => {
                let mut from = tape[at + 2] as usize;
                at += 2;
                Value::Array((0..payload).map(|_| element(tag >> 8, &mut from)).collect())
            }
            8 => Value::String(string(at)),
            ty => scalar(ty, payload),
        };
        at += 2;
        metadata.push((key, value));
    }
    let mut tensors = Vec::with_capacity(tensor_count);
    for _ in 0..tensor_count {
        let name = string(at);
        let rank = tape[at + 2] as usize;
        let ty = GgmlType(tape[at + 3] as u32);
        let dims = tape[at + 4..at + 4 + rank]
            .iter()
            .map(|&d| d as usize)
            .collect();
        at += 4 + rank;
        tensors.push(TensorInfo {
            name,
            dims,
            ty,
            offset: tape[at] as usize,
            len: tape[at + 1] as usize,
        });
        at += 2;
    }
    Gguf {
        mapping,
        version,
        alignment,
        metadata,
        tensors,
    }
}

/// One tensor to write.
#[derive(Clone, Debug)]
pub struct TensorData {
    pub name: String,
    /// GGML order (`dims[0]` contiguous).
    pub dims: Vec<usize>,
    pub ty: GgmlType,
    pub bytes: Vec<u8>,
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}
fn put_value(out: &mut Vec<u8>, v: &Value) -> Result<(), Error> {
    match v {
        Value::U8(x) => out.push(*x),
        Value::I8(x) => out.push(*x as u8),
        Value::U16(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I16(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::U32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::F32(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::Bool(x) => out.push(u8::from(*x)),
        Value::String(s) => put_string(out, s),
        Value::Array(items) => {
            let ty = items.first().map_or(0, Value::type_id);
            if items
                .iter()
                .any(|i| i.type_id() != ty || matches!(i, Value::Array(_)))
            {
                return Err(Error::Unsupported("mixed or nested GGUF array".into()));
            }
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&(items.len() as u64).to_le_bytes());
            for i in items {
                put_value(out, i)?;
            }
        }
        Value::U64(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::I64(x) => out.extend_from_slice(&x.to_le_bytes()),
        Value::F64(x) => out.extend_from_slice(&x.to_le_bytes()),
    }
    Ok(())
}

/// The data alignment `general.alignment` selects: [`DEFAULT_ALIGNMENT`]
/// when absent, otherwise a non-zero power-of-two `u32`.
pub(crate) fn alignment_of(metadata: &[(String, Value)]) -> Result<usize, Error> {
    match metadata.iter().find(|(k, _)| k == "general.alignment") {
        None => Ok(DEFAULT_ALIGNMENT),
        Some((_, Value::U32(a))) if *a > 0 && a.is_power_of_two() => Ok(*a as usize),
        Some(_) => Err(Error::Malformed("general.alignment")),
    }
}

/// Serializes a version-3 GGUF file, padded to the alignment that
/// `general.alignment` selects ([`DEFAULT_ALIGNMENT`] when absent), so the
/// output reads back with the same offsets.
pub fn write(metadata: &[(String, Value)], tensors: &[TensorData]) -> Result<Vec<u8>, Error> {
    let alignment = alignment_of(metadata)?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
    out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    for (k, v) in metadata {
        put_string(&mut out, k);
        out.extend_from_slice(&v.type_id().to_le_bytes());
        put_value(&mut out, v)?;
    }
    let mut offset = 0usize;
    let mut offsets = Vec::with_capacity(tensors.len());
    for t in tensors {
        let (block, block_bytes) =
            t.ty.geometry()
                .ok_or_else(|| Error::Unsupported(format!("GGML tensor type {}", t.ty.0)))?;
        let elements = t
            .dims
            .iter()
            .try_fold(1usize, |a, d| a.checked_mul(*d))
            .ok_or(Error::Overflow)?;
        if t.dims.is_empty()
            || !t.dims[0].is_multiple_of(block)
            || elements / block * block_bytes != t.bytes.len()
        {
            return Err(Error::Shape("GGUF tensor byte length"));
        }
        put_string(&mut out, &t.name);
        out.extend_from_slice(&(t.dims.len() as u32).to_le_bytes());
        for d in &t.dims {
            out.extend_from_slice(&(*d as u64).to_le_bytes());
        }
        out.extend_from_slice(&t.ty.0.to_le_bytes());
        out.extend_from_slice(&(offset as u64).to_le_bytes());
        offsets.push(offset);
        offset = (offset + t.bytes.len()).next_multiple_of(alignment);
    }
    out.resize(out.len().next_multiple_of(alignment), 0);
    let data_start = out.len();
    for (t, off) in tensors.iter().zip(offsets) {
        out.resize(data_start + off, 0);
        out.extend_from_slice(&t.bytes);
    }
    Ok(out)
}
