//! The frozen Rust GGUF header reader (bug fixes only until self-hosting
//! Stage 2). The package vesper-tensor reads headers with the Vesper port
//! (vesper/src/store.ves); this seed copy, which the compiler links, keeps
//! the Rust reader. Moved here from src/gguf.rs by SH6 lane 11 (#1406).
use std::collections::HashSet;
use std::sync::Arc;

use crate::gguf::{
    alignment_of, GgmlType, Gguf, TensorInfo, Value, MAGIC, MAX_ARRAY, MAX_DIMS, MAX_METADATA,
    MAX_STRING, MAX_TENSORS,
};
use crate::mmap::Mapping;
use crate::Error;

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

pub(crate) fn parse(mapping: Arc<Mapping>) -> Result<Gguf, Error> {
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
    Ok(Gguf::from_parts(
        mapping, version, alignment, metadata, tensors,
    ))
}
