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
use std::collections::HashSet;
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
    pub const F32: Self = Self(0);
    pub const F16: Self = Self(1);
    pub const Q4_0: Self = Self(2);
    pub const Q8_0: Self = Self(8);
    pub const Q4_K: Self = Self(12);
    pub const BF16: Self = Self(30);
    pub const MXFP4: Self = Self(39);

    /// `(elements per block, bytes per block)` for every type this reader
    /// can lay out, whether or not it can compute with it.
    pub fn geometry(self) -> Option<(usize, usize)> {
        Some(match self.0 {
            0 => (1, 4),
            1 => (1, 2),
            2 => (32, 18),
            3 => (32, 20),
            6 => (32, 22),
            7 => (32, 24),
            8 => (32, 34),
            9 => (32, 36),
            10 => (256, 84),
            11 => (256, 110),
            12 => (256, 144),
            13 => (256, 176),
            14 => (256, 210),
            15 => (256, 292),
            24 => (1, 1),
            25 => (1, 2),
            26 => (1, 4),
            27 => (1, 8),
            28 => (1, 8),
            30 => (1, 2),
            39 => (32, 17),
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

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Overflow)?;
        let s = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::Malformed("truncated GGUF"))?;
        self.at = end;
        Ok(s)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn string(&mut self) -> Result<String, Error> {
        let n = self.u64()?;
        if n > MAX_STRING {
            return Err(Error::Limit("GGUF string length"));
        }
        let b = self.take(n as usize)?;
        String::from_utf8(b.to_vec()).map_err(|_| Error::Malformed("GGUF string is not UTF-8"))
    }
    fn value(&mut self, ty: u32, depth: usize) -> Result<Value, Error> {
        Ok(match ty {
            0 => Value::U8(self.array::<1>()?[0]),
            1 => Value::I8(self.array::<1>()?[0] as i8),
            2 => Value::U16(u16::from_le_bytes(self.array()?)),
            3 => Value::I16(i16::from_le_bytes(self.array()?)),
            4 => Value::U32(self.u32()?),
            5 => Value::I32(i32::from_le_bytes(self.array()?)),
            6 => Value::F32(f32::from_le_bytes(self.array()?)),
            7 => match self.array::<1>()?[0] {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(Error::Malformed("GGUF bool")),
            },
            8 => Value::String(self.string()?),
            9 => {
                if depth > 0 {
                    return Err(Error::Unsupported("nested GGUF arrays".into()));
                }
                let elem = self.u32()?;
                let n = self.u64()?;
                if n > MAX_ARRAY {
                    return Err(Error::Limit("GGUF array length"));
                }
                // Every element takes at least one byte: never reserve more
                // than the remaining input can hold.
                let remaining = (self.bytes.len() - self.at) as u64;
                if n > remaining {
                    return Err(Error::Malformed("truncated GGUF array"));
                }
                let mut items = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    items.push(self.value(elem, depth + 1)?);
                }
                Value::Array(items)
            }
            10 => Value::U64(self.u64()?),
            11 => Value::I64(i64::from_le_bytes(self.array()?)),
            12 => Value::F64(f64::from_le_bytes(self.array()?)),
            _ => return Err(Error::Malformed("unknown GGUF metadata type")),
        })
    }
}

impl Gguf {
    /// Maps `path` read-only and parses it.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::parse(Mapping::open(path)?)
    }
    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::parse(Mapping::from_vec(bytes))
    }
    pub fn parse(mapping: Arc<Mapping>) -> Result<Self, Error> {
        let bytes = mapping.bytes();
        let mut r = Reader { bytes, at: 0 };
        if r.take(4)? != MAGIC {
            return Err(Error::Malformed("not a GGUF file"));
        }
        let version = r.u32()?;
        if !(2..=3).contains(&version) {
            return Err(Error::Unsupported(format!("GGUF version {version}")));
        }
        let tensor_count = r.u64()?;
        let kv_count = r.u64()?;
        if tensor_count > MAX_TENSORS {
            return Err(Error::Limit("GGUF tensors"));
        }
        if kv_count > MAX_METADATA {
            return Err(Error::Limit("GGUF metadata entries"));
        }
        let mut metadata: Vec<(String, Value)> = Vec::new();
        let mut keys = HashSet::new();
        for _ in 0..kv_count {
            let key = r.string()?;
            let ty = r.u32()?;
            let value = r.value(ty, 0)?;
            if !keys.insert(key.clone()) {
                return Err(Error::Malformed("duplicate GGUF metadata key"));
            }
            metadata.push((key, value));
        }
        let alignment = alignment_of(&metadata)?;
        let mut infos = Vec::new();
        let mut names = HashSet::new();
        for _ in 0..tensor_count {
            let name = r.string()?;
            let n_dims = r.u32()?;
            if n_dims == 0 || n_dims > MAX_DIMS {
                return Err(Error::Malformed("GGUF tensor rank"));
            }
            let mut dims = Vec::with_capacity(n_dims as usize);
            for _ in 0..n_dims {
                let d = r.u64()?;
                if d == 0 {
                    return Err(Error::Malformed("GGUF zero dimension"));
                }
                dims.push(usize::try_from(d).map_err(|_| Error::Overflow)?);
            }
            let ty = GgmlType(r.u32()?);
            let offset = r.u64()?;
            if !names.insert(name.clone()) {
                return Err(Error::Malformed("duplicate GGUF tensor name"));
            }
            infos.push((name, dims, ty, offset));
        }
        let data_start =
            r.at.checked_next_multiple_of(alignment)
                .ok_or(Error::Overflow)?;
        if data_start > bytes.len() {
            return Err(Error::Malformed("truncated GGUF"));
        }
        let mut tensors = Vec::with_capacity(infos.len());
        for (name, dims, ty, offset) in infos {
            let (block, block_bytes) = ty
                .geometry()
                .ok_or_else(|| Error::Unsupported(format!("GGML tensor type {}", ty.0)))?;
            if !dims[0].is_multiple_of(block) {
                return Err(Error::QuantBlockAlignment {
                    cols: dims[0],
                    block,
                });
            }
            let elements = dims
                .iter()
                .try_fold(1usize, |a, d| a.checked_mul(*d))
                .ok_or(Error::Overflow)?;
            let len = (elements / block)
                .checked_mul(block_bytes)
                .ok_or(Error::Overflow)?;
            let offset = usize::try_from(offset).map_err(|_| Error::Overflow)?;
            if !offset.is_multiple_of(alignment) {
                return Err(Error::Malformed("misaligned GGUF tensor"));
            }
            let start = data_start.checked_add(offset).ok_or(Error::Overflow)?;
            let end = start.checked_add(len).ok_or(Error::Overflow)?;
            if end > bytes.len() {
                return Err(Error::Malformed("GGUF tensor data outside the file"));
            }
            tensors.push(TensorInfo {
                name,
                dims,
                ty,
                offset: start,
                len,
            });
        }
        let mut order: Vec<&TensorInfo> = tensors.iter().collect();
        order.sort_by_key(|t| t.offset);
        for pair in order.windows(2) {
            if pair[0].offset + pair[0].len > pair[1].offset {
                return Err(Error::Malformed("overlapping GGUF tensors"));
            }
        }
        Ok(Self {
            mapping,
            version,
            alignment,
            metadata,
            tensors,
        })
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
fn alignment_of(metadata: &[(String, Value)]) -> Result<usize, Error> {
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
