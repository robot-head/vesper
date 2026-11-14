//! The safetensors container: reading (from memory or a read-only mapping)
//! and writing.
//!
//! Layout: an unsigned little-endian `u64` header length `N`, `N` bytes of
//! UTF-8 JSON, then the data buffer. The header maps each tensor name to
//! `{"dtype", "shape", "data_offsets": [begin, end]}` (offsets relative to the
//! data buffer) plus an optional `"__metadata__"` string-to-string map.
//!
//! Validation follows the reference implementation's rules and is stricter
//! where it would otherwise be ambiguous: the header is at most
//! [`MAX_HEADER_BYTES`] and must start with `{` (trailing spaces are allowed
//! as padding); duplicate keys are rejected; every tensor's byte length must
//! equal `product(shape) * dtype size`; and the tensors, sorted by offset,
//! must tile the data buffer exactly with no gaps, overlaps or trailing bytes.
use std::path::Path;
use std::sync::Arc;

use crate::json::{self, Value};
use crate::mmap::{Mapping, WeightBytes};
use crate::quant::{Matrix, WeightFormat};
use crate::Error;

pub const MAX_HEADER_BYTES: u64 = 100_000_000;
pub const MAX_TENSORS: usize = 1 << 20;
pub const MAX_RANK: usize = 8;

/// safetensors element types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dtype {
    Bool,
    U8,
    I8,
    F8E5M2,
    F8E4M3,
    I16,
    U16,
    F16,
    Bf16,
    I32,
    U32,
    F32,
    F64,
    I64,
    U64,
}
impl Dtype {
    pub fn parse(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "BOOL" => Self::Bool,
            "U8" => Self::U8,
            "I8" => Self::I8,
            "F8_E5M2" => Self::F8E5M2,
            "F8_E4M3" => Self::F8E4M3,
            "I16" => Self::I16,
            "U16" => Self::U16,
            "F16" => Self::F16,
            "BF16" => Self::Bf16,
            "I32" => Self::I32,
            "U32" => Self::U32,
            "F32" => Self::F32,
            "F64" => Self::F64,
            "I64" => Self::I64,
            "U64" => Self::U64,
            other => return Err(Error::Unsupported(format!("safetensors dtype {other}"))),
        })
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "BOOL",
            Self::U8 => "U8",
            Self::I8 => "I8",
            Self::F8E5M2 => "F8_E5M2",
            Self::F8E4M3 => "F8_E4M3",
            Self::I16 => "I16",
            Self::U16 => "U16",
            Self::F16 => "F16",
            Self::Bf16 => "BF16",
            Self::I32 => "I32",
            Self::U32 => "U32",
            Self::F32 => "F32",
            Self::F64 => "F64",
            Self::I64 => "I64",
            Self::U64 => "U64",
        }
    }
    pub const fn size(self) -> usize {
        match self {
            Self::Bool | Self::U8 | Self::I8 | Self::F8E5M2 | Self::F8E4M3 => 1,
            Self::I16 | Self::U16 | Self::F16 | Self::Bf16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 | Self::I64 | Self::U64 => 8,
        }
    }
    /// The weight format of a floating-point dtype usable as a matrix.
    pub fn weight_format(self) -> Result<WeightFormat, Error> {
        Ok(match self {
            Self::F32 => WeightFormat::F32,
            Self::F16 => WeightFormat::F16,
            Self::Bf16 => WeightFormat::Bf16,
            Self::F8E4M3 => WeightFormat::F8E4M3,
            Self::F8E5M2 => WeightFormat::F8E5M2,
            other => {
                return Err(Error::Unsupported(format!(
                    "safetensors dtype {} as weights",
                    other.name()
                )));
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorInfo {
    pub name: String,
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    /// Absolute byte offset of the tensor in the file.
    pub offset: usize,
    pub len: usize,
}

/// A parsed safetensors file whose tensor bytes stay in the mapping.
#[derive(Debug)]
pub struct SafeTensors {
    mapping: Arc<Mapping>,
    tensors: Vec<TensorInfo>,
    metadata: Vec<(String, String)>,
}

fn int(v: &Value) -> Result<usize, Error> {
    match v {
        Value::Int(n) => usize::try_from(*n).map_err(|_| Error::Overflow),
        _ => Err(Error::Malformed("safetensors integer")),
    }
}

impl SafeTensors {
    /// Maps `path` read-only and parses it.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::parse(Mapping::open(path)?)
    }
    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::parse(Mapping::from_vec(bytes))
    }
    pub fn parse(mapping: Arc<Mapping>) -> Result<Self, Error> {
        let bytes = mapping.bytes();
        let n = u64::from_le_bytes(
            bytes
                .get(..8)
                .ok_or(Error::Malformed("safetensors header length"))?
                .try_into()
                .expect("eight bytes"),
        );
        if n > MAX_HEADER_BYTES {
            return Err(Error::Limit("safetensors header bytes"));
        }
        let header_end = 8usize
            .checked_add(usize::try_from(n).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        let header = bytes
            .get(8..header_end)
            .ok_or(Error::Malformed("truncated safetensors header"))?;
        if header.first() != Some(&b'{') {
            return Err(Error::Malformed("safetensors header must be a JSON object"));
        }
        let trimmed =
            &header[..header.len() - header.iter().rev().take_while(|b| **b == b' ').count()];
        let Value::Object(fields) = json::parse(trimmed)? else {
            return Err(Error::Malformed("safetensors header must be a JSON object"));
        };
        if fields.len() > MAX_TENSORS + 1 {
            return Err(Error::Limit("safetensors tensors"));
        }
        let data_len = bytes.len() - header_end;
        let mut tensors = Vec::with_capacity(fields.len());
        let mut metadata = Vec::new();
        for (name, value) in fields {
            if name == "__metadata__" {
                let Value::Object(entries) = value else {
                    return Err(Error::Malformed("safetensors __metadata__"));
                };
                for (k, v) in entries {
                    let Value::String(v) = v else {
                        return Err(Error::Malformed("safetensors metadata value"));
                    };
                    metadata.push((k, v));
                }
                continue;
            }
            let Value::Object(entry) = &value else {
                return Err(Error::Malformed("safetensors tensor entry"));
            };
            if entry
                .iter()
                .any(|(k, _)| !matches!(k.as_str(), "dtype" | "shape" | "data_offsets"))
            {
                return Err(Error::Malformed("unknown safetensors tensor field"));
            }
            let Some(Value::String(dtype)) = value.get("dtype") else {
                return Err(Error::Malformed("safetensors dtype"));
            };
            let dtype = Dtype::parse(dtype)?;
            let Some(Value::Array(shape)) = value.get("shape") else {
                return Err(Error::Malformed("safetensors shape"));
            };
            if shape.len() > MAX_RANK {
                return Err(Error::Limit("safetensors rank"));
            }
            let shape = shape.iter().map(int).collect::<Result<Vec<_>, _>>()?;
            let Some(Value::Array(offsets)) = value.get("data_offsets") else {
                return Err(Error::Malformed("safetensors data_offsets"));
            };
            let [begin, end] = offsets.as_slice() else {
                return Err(Error::Malformed("safetensors data_offsets"));
            };
            let (begin, end) = (int(begin)?, int(end)?);
            if begin > end || end > data_len {
                return Err(Error::Malformed("safetensors data_offsets out of range"));
            }
            let elements = shape
                .iter()
                .try_fold(1usize, |a, d| a.checked_mul(*d))
                .ok_or(Error::Overflow)?;
            if elements.checked_mul(dtype.size()) != Some(end - begin) {
                return Err(Error::Malformed("safetensors tensor byte length"));
            }
            tensors.push(TensorInfo {
                name,
                dtype,
                shape,
                offset: header_end + begin,
                len: end - begin,
            });
        }
        let mut order: Vec<&TensorInfo> = tensors.iter().collect();
        order.sort_by_key(|t| (t.offset, t.len));
        let mut cursor = header_end;
        for t in order {
            if t.offset != cursor {
                return Err(Error::Malformed(
                    "safetensors tensors must tile the data buffer",
                ));
            }
            cursor += t.len;
        }
        if cursor != bytes.len() {
            return Err(Error::Malformed("trailing bytes after safetensors data"));
        }
        Ok(Self {
            mapping,
            tensors,
            metadata,
        })
    }
    pub fn tensors(&self) -> &[TensorInfo] {
        &self.tensors
    }
    pub fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }
    pub fn mapping(&self) -> &Arc<Mapping> {
        &self.mapping
    }
    pub fn get(&self, name: &str) -> Option<&TensorInfo> {
        self.tensors.iter().find(|t| t.name == name)
    }
    fn require(&self, name: &str) -> Result<&TensorInfo, Error> {
        self.get(name)
            .ok_or_else(|| Error::MissingTensor(name.to_string()))
    }
    pub fn bytes(&self, info: &TensorInfo) -> &[u8] {
        &self.mapping.bytes()[info.offset..info.offset + info.len]
    }
    /// A 2-D floating-point tensor as a zero-copy [`Matrix`]. An FP8 tensor
    /// `X` may have a scalar F32 companion `X_scale` (the per-tensor
    /// `weight_scale` convention); its value becomes the matrix scale.
    /// Per-channel or block scales are rejected as unsupported.
    pub fn matrix(&self, name: &str) -> Result<Matrix, Error> {
        let info = self.require(name)?;
        let [rows, cols] = info.shape.as_slice() else {
            return Err(Error::Shape("safetensors matrix must be 2-D"));
        };
        let format = info.dtype.weight_format()?;
        let scale = match format {
            WeightFormat::F8E4M3 | WeightFormat::F8E5M2 => {
                match self.get(&format!("{name}_scale")) {
                    None => 1.0,
                    Some(s) if s.dtype == Dtype::F32 && s.len == 4 => {
                        let b = self.bytes(s);
                        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
                    }
                    Some(_) => {
                        return Err(Error::Unsupported(
                            "FP8 scale that is not one F32 value".into(),
                        ));
                    }
                }
            }
            _ => 1.0,
        };
        Matrix::from_bytes(
            format,
            *rows,
            *cols,
            WeightBytes::range(&self.mapping, info.offset, info.len)?,
            scale,
        )
    }
    /// A 1-D (or scalar) floating-point tensor decoded to FP32 (copied).
    pub fn vector(&self, name: &str) -> Result<Vec<f32>, Error> {
        let info = self.require(name)?;
        if info.shape.len() > 1 {
            return Err(Error::Shape("safetensors vector must be 1-D"));
        }
        let n = info.shape.first().copied().unwrap_or(1);
        if n == 0 {
            return Err(Error::Shape("empty vector"));
        }
        let m = Matrix::from_bytes(
            info.dtype.weight_format()?,
            1,
            n,
            WeightBytes::range(&self.mapping, info.offset, info.len)?,
            1.0,
        )?;
        m.to_f32()
    }
}

/// One tensor to write: name, dtype, shape and its raw little-endian bytes.
#[derive(Clone, Debug)]
pub struct TensorData {
    pub name: String,
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    pub bytes: Vec<u8>,
}

/// Serializes tensors (in the given order) and string metadata. The header
/// is padded with spaces to a multiple of eight bytes, as the reference
/// writer does.
pub fn write(tensors: &[TensorData], metadata: &[(String, String)]) -> Result<Vec<u8>, Error> {
    let mut header = String::from("{");
    let mut first = true;
    if !metadata.is_empty() {
        header.push_str("\"__metadata__\":{");
        for (i, (k, v)) in metadata.iter().enumerate() {
            if i > 0 {
                header.push(',');
            }
            header.push_str(&json::quote(k));
            header.push(':');
            header.push_str(&json::quote(v));
        }
        header.push('}');
        first = false;
    }
    let mut offset = 0usize;
    for t in tensors {
        let elements = t
            .shape
            .iter()
            .try_fold(1usize, |a, d| a.checked_mul(*d))
            .ok_or(Error::Overflow)?;
        if elements.checked_mul(t.dtype.size()) != Some(t.bytes.len()) {
            return Err(Error::Shape("tensor byte length"));
        }
        if !first {
            header.push(',');
        }
        first = false;
        let shape: Vec<String> = t.shape.iter().map(|d| d.to_string()).collect();
        header.push_str(&format!(
            "{}:{{\"dtype\":\"{}\",\"shape\":[{}],\"data_offsets\":[{},{}]}}",
            json::quote(&t.name),
            t.dtype.name(),
            shape.join(","),
            offset,
            offset + t.bytes.len()
        ));
        offset += t.bytes.len();
    }
    header.push('}');
    while !(8 + header.len()).is_multiple_of(8) {
        header.push(' ');
    }
    let mut out = Vec::with_capacity(8 + header.len() + offset);
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for t in tensors {
        out.extend_from_slice(&t.bytes);
    }
    Ok(out)
}
