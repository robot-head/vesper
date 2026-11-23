//! Frozen Rust derivation-input canonical encoding, decoding and hashing.

use std::collections::BTreeMap;

use crate::derivation::{
    DerivationInputs, DerivationKey, InputSchema, MAX_LABEL_BYTES, MAX_MAP_ENTRIES,
};
use crate::error::DerivationError;
use crate::id::{DERIVATION_CONTEXT, Digest, Domain, OBJECT_ID_BYTES, ObjectId};

const MAGIC: &[u8; 4] = b"VLDI";

impl DerivationInputs {
    pub(crate) fn encode_rust(&self) -> Result<Vec<u8>, DerivationError> {
        self.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.schema.number().to_le_bytes());
        out.push(self.output_domain.tag());
        put_str(&mut out, &self.builder);
        put_str(&mut out, &self.builder_version);
        match &self.target {
            None => out.push(0),
            Some(t) => {
                out.push(1);
                put_str(&mut out, t);
            }
        }
        out.extend_from_slice(&(self.options.len() as u32).to_le_bytes());
        for (k, v) in &self.options {
            put_str(&mut out, k);
            put_str(&mut out, v);
        }
        out.extend_from_slice(&(self.inputs.len() as u32).to_le_bytes());
        for (name, id) in &self.inputs {
            put_str(&mut out, name);
            id.encode_into(&mut out);
        }
        Ok(out)
    }

    pub(crate) fn decode_rust(bytes: &[u8]) -> Result<Self, DerivationError> {
        let mut r = Reader { bytes, pos: 0 };
        if r.take(4)? != MAGIC {
            return Err(DerivationError::BadMagic);
        }
        let schema = match r.u32()? {
            1 => InputSchema::V1,
            other => return Err(DerivationError::UnsupportedSchema(other)),
        };
        let tag = r.u8()?;
        let output_domain = Domain::from_tag(tag).ok_or(DerivationError::UnknownDomain(tag))?;
        let builder = r.string()?;
        let builder_version = r.string()?;
        let target = match r.u8()? {
            0 => None,
            1 => Some(r.string()?),
            _ => return Err(DerivationError::BadMagic),
        };
        let n = r.count("options")?;
        let mut options = BTreeMap::new();
        let mut last: Option<String> = None;
        for _ in 0..n {
            let k = r.string()?;
            let v = r.string()?;
            ascending(&mut last, &k, "options")?;
            options.insert(k, v);
        }
        let n = r.count("inputs")?;
        let mut inputs = BTreeMap::new();
        let mut last: Option<String> = None;
        for _ in 0..n {
            let name = r.string()?;
            let id = ObjectId::decode(r.take(OBJECT_ID_BYTES)?)
                .ok_or(DerivationError::InvalidObjectId)?;
            ascending(&mut last, &name, "inputs")?;
            inputs.insert(name, id);
        }
        if r.pos != bytes.len() {
            return Err(DerivationError::TrailingBytes);
        }
        let inputs = DerivationInputs {
            schema,
            output_domain,
            builder,
            builder_version,
            target,
            options,
            inputs,
        };
        inputs.validate()?;
        Ok(inputs)
    }
}

impl DerivationKey {
    pub(crate) fn of_encoding_rust(encoding: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(DERIVATION_CONTEXT);
        hasher.update(encoding);
        DerivationKey(Digest(*hasher.finalize().as_bytes()))
    }
}

fn ascending(
    last: &mut Option<String>,
    key: &str,
    field: &'static str,
) -> Result<(), DerivationError> {
    if let Some(prev) = last
        && prev.as_bytes() >= key.as_bytes()
    {
        return Err(DerivationError::NotSorted { field });
    }
    *last = Some(key.to_owned());
    Ok(())
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DerivationError> {
        let end = self.pos.checked_add(n).ok_or(DerivationError::Truncated)?;
        let s = self
            .bytes
            .get(self.pos..end)
            .ok_or(DerivationError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, DerivationError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DerivationError> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    fn count(&mut self, field: &'static str) -> Result<usize, DerivationError> {
        let n = self.u32()? as usize;
        if n > MAX_MAP_ENTRIES {
            return Err(DerivationError::TooManyEntries {
                field,
                limit: MAX_MAP_ENTRIES,
            });
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String, DerivationError> {
        let len = self.u32()? as usize;
        if len > MAX_LABEL_BYTES {
            return Err(DerivationError::InvalidLabel {
                field: "string",
                value: format!("<{len} bytes>"),
            });
        }
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|_| DerivationError::InvalidLabel {
            field: "string",
            value: String::from_utf8_lossy(raw).into_owned(),
        })
    }
}
