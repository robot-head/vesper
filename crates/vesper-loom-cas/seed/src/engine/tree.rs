//! Frozen Rust name validation and canonical tree codec.

use crate::error::{NameError, TreeError};
use crate::id::{Digest, Domain, ObjectHasher, ObjectId, ObjectKind};
use crate::tree::{EntryKind, MAX_TREE_ENTRIES, TREE_SCHEMA_VERSION, Tree};

const MAX_NAME_BYTES: usize = 255;
const MAGIC: &[u8; 4] = b"VLTR";

pub(crate) fn validate_name_rust(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    if name == "." || name == ".." {
        return Err(NameError::DotOrDotDot);
    }
    for c in name.chars() {
        match c {
            '/' | '\\' => return Err(NameError::Separator),
            ':' | '<' | '>' | '"' | '|' | '?' | '*' => return Err(NameError::ForbiddenCharacter),
            c if c.is_control() => return Err(NameError::NulOrControl),
            _ => {}
        }
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(NameError::TrailingDotOrSpace);
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(NameError::WindowsReserved);
    }
    Ok(())
}

impl Tree {
    pub(crate) fn encode_rust(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.entries.len() * 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&TREE_SCHEMA_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        for (name, entry) in &self.entries {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.push(entry.kind.tag());
            out.extend_from_slice(&entry.digest.0);
            out.extend_from_slice(&entry.size.to_le_bytes());
        }
        out
    }

    pub(crate) fn id_rust(&self) -> ObjectId {
        let mut hasher = ObjectHasher::new(self.domain, ObjectKind::Tree);
        hasher.update(&self.encode());
        hasher.finish()
    }

    pub(crate) fn decode_rust(domain: Domain, bytes: &[u8]) -> Result<Tree, TreeError> {
        let mut r = Reader { bytes, pos: 0 };
        if r.take(4)? != MAGIC {
            return Err(TreeError::BadMagic);
        }
        let version = u32::from_le_bytes(r.array()?);
        if version != TREE_SCHEMA_VERSION {
            return Err(TreeError::UnsupportedVersion(version));
        }
        let count = u64::from_le_bytes(r.array()?);
        if count > MAX_TREE_ENTRIES {
            return Err(TreeError::TooManyEntries {
                count,
                limit: MAX_TREE_ENTRIES,
            });
        }
        let mut tree = Tree::new(domain);
        let mut previous: Option<String> = None;
        for _ in 0..count {
            let len = u16::from_le_bytes(r.array()?) as usize;
            let raw = r.take(len)?;
            let name = std::str::from_utf8(raw).map_err(|_| TreeError::InvalidName {
                name: String::from_utf8_lossy(raw).into_owned(),
                reason: NameError::NotUtf8,
            })?;
            let tag = r.array::<1>()?[0];
            let kind = EntryKind::from_tag(tag).ok_or(TreeError::UnknownEntryKind(tag))?;
            let digest = Digest(r.array()?);
            let size = u64::from_le_bytes(r.array()?);
            if let Some(prev) = &previous
                && prev.as_bytes() >= name.as_bytes()
            {
                return Err(TreeError::NotSorted {
                    previous: prev.clone(),
                    next: name.to_owned(),
                });
            }
            if kind == EntryKind::Directory && size != 0 {
                return Err(TreeError::DirectorySize {
                    name: name.to_owned(),
                });
            }
            tree.insert(
                name,
                ObjectId::new(domain, kind.object_kind(), digest),
                kind,
                size,
            )?;
            previous = Some(name.to_owned());
        }
        if r.pos != bytes.len() {
            return Err(TreeError::TrailingBytes);
        }
        Ok(tree)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], TreeError> {
        let end = self.pos.checked_add(n).ok_or(TreeError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(TreeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], TreeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}
