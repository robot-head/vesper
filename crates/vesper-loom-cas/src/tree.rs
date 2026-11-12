//! Canonical Merkle directory trees.
//!
//! A tree lists the entries of one directory. Its encoding (schema v1) is:
//!
//! ```text
//! "VLTR" | u32 LE version=1 | u64 LE count |
//!   count × ( u16 LE name_len | name (UTF-8) | u8 kind | 32-byte digest | u64 LE size )
//! ```
//!
//! Entries are strictly ascending by name bytes, kinds are `1` (file), `2`
//! (executable file) and `3` (directory, size 0). Child identities use the
//! tree's own domain. Decoding rejects anything that would re-encode
//! differently, so every tree has exactly one encoding and one identity.
//!
//! Schema v1 intentionally has no symlink, hardlink or special-file entries.

use std::collections::BTreeMap;

use crate::error::{NameError, TreeError};
use crate::id::{Digest, Domain, ObjectHasher, ObjectId, ObjectKind};

pub const TREE_SCHEMA_VERSION: u32 = 1;
/// Upper bound on entries in one directory, to bound decoding work.
pub const MAX_TREE_ENTRIES: u64 = 1 << 20;
/// Upper bound on directory nesting during ingestion and materialization.
pub const MAX_TREE_DEPTH: usize = 128;
const MAX_NAME_BYTES: usize = 255;
const MAGIC: &[u8; 4] = b"VLTR";

/// Validate one path component against the portable naming policy.
///
/// The policy is deliberately stricter than any single host file system so
/// that a tree stored on Linux materializes identically on macOS and Windows:
/// no separators, no `.`/`..`, no NUL/control characters, no Windows-reserved
/// characters or device names, and no trailing dot or space.
pub fn validate_name(name: &str) -> Result<(), NameError> {
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

/// What a tree entry refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File { executable: bool },
    Directory,
}

impl EntryKind {
    fn tag(self) -> u8 {
        match self {
            EntryKind::File { executable: false } => 1,
            EntryKind::File { executable: true } => 2,
            EntryKind::Directory => 3,
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(EntryKind::File { executable: false }),
            2 => Some(EntryKind::File { executable: true }),
            3 => Some(EntryKind::Directory),
            _ => None,
        }
    }

    /// Kind of the object an entry of this kind points to.
    pub fn object_kind(self) -> ObjectKind {
        match self {
            EntryKind::File { .. } => ObjectKind::Blob,
            EntryKind::Directory => ObjectKind::Tree,
        }
    }
}

/// One directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeEntry {
    pub kind: EntryKind,
    pub digest: Digest,
    /// File size in bytes; always 0 for directories.
    pub size: u64,
}

/// A canonical directory listing within one domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    domain: Domain,
    entries: BTreeMap<String, TreeEntry>,
    folded: BTreeMap<String, String>,
}

impl Tree {
    pub fn new(domain: Domain) -> Self {
        Tree {
            domain,
            entries: BTreeMap::new(),
            folded: BTreeMap::new(),
        }
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries.get(name)
    }

    /// Entries in canonical (byte-ascending) order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &TreeEntry)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Identity of the child an entry points to.
    pub fn child_id(&self, entry: &TreeEntry) -> ObjectId {
        ObjectId::new(self.domain, entry.kind.object_kind(), entry.digest)
    }

    pub fn insert_file(
        &mut self,
        name: &str,
        blob: ObjectId,
        size: u64,
        executable: bool,
    ) -> Result<(), TreeError> {
        self.insert(name, blob, EntryKind::File { executable }, size)
    }

    pub fn insert_dir(&mut self, name: &str, tree: ObjectId) -> Result<(), TreeError> {
        self.insert(name, tree, EntryKind::Directory, 0)
    }

    fn insert(
        &mut self,
        name: &str,
        child: ObjectId,
        kind: EntryKind,
        size: u64,
    ) -> Result<(), TreeError> {
        validate_name(name).map_err(|reason| TreeError::InvalidName {
            name: name.to_owned(),
            reason,
        })?;
        if child.domain != self.domain || child.kind != kind.object_kind() {
            return Err(TreeError::ChildMismatch {
                name: name.to_owned(),
                child,
            });
        }
        let folded = name.to_lowercase();
        if let Some(first) = self.folded.get(&folded) {
            return Err(TreeError::CaseCollision {
                first: first.clone(),
                second: name.to_owned(),
            });
        }
        self.folded.insert(folded, name.to_owned());
        self.entries.insert(
            name.to_owned(),
            TreeEntry {
                kind,
                digest: child.digest,
                size,
            },
        );
        Ok(())
    }

    /// Canonical encoding.
    pub fn encode(&self) -> Vec<u8> {
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

    /// Identity of this tree.
    pub fn id(&self) -> ObjectId {
        let mut hasher = ObjectHasher::new(self.domain, ObjectKind::Tree);
        hasher.update(&self.encode());
        hasher.finish()
    }

    /// Strict decoder: only canonical encodings are accepted.
    pub fn decode(domain: Domain, bytes: &[u8]) -> Result<Tree, TreeError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::blob_id;

    #[test]
    fn rejects_adversarial_names() {
        let cases: &[(&str, NameError)] = &[
            ("", NameError::Empty),
            (".", NameError::DotOrDotDot),
            ("..", NameError::DotOrDotDot),
            ("../etc", NameError::Separator),
            ("a/b", NameError::Separator),
            ("a\\b", NameError::Separator),
            ("C:", NameError::ForbiddenCharacter),
            ("file:stream", NameError::ForbiddenCharacter),
            ("a\0b", NameError::NulOrControl),
            ("tab\there", NameError::NulOrControl),
            ("trailing.", NameError::TrailingDotOrSpace),
            ("trailing ", NameError::TrailingDotOrSpace),
            ("CON", NameError::WindowsReserved),
            ("nul.txt", NameError::WindowsReserved),
            ("com1", NameError::WindowsReserved),
            ("Lpt9.log", NameError::WindowsReserved),
        ];
        for (name, reason) in cases {
            assert_eq!(validate_name(name), Err(*reason), "{name:?}");
        }
        assert_eq!(validate_name(&"x".repeat(256)), Err(NameError::TooLong));
        for ok in [
            "main.vsp",
            ".hidden",
            "Loom.toml",
            "comma,ok",
            "console",
            "com10",
            "λ.vsp",
        ] {
            assert_eq!(validate_name(ok), Ok(()), "{ok:?}");
        }
    }

    #[test]
    fn rejects_case_collisions_and_duplicates() {
        let blob = blob_id(Domain::Source, b"x");
        let mut tree = Tree::new(Domain::Source);
        tree.insert_file("Readme", blob, 1, false).unwrap();
        assert!(matches!(
            tree.insert_file("README", blob, 1, false),
            Err(TreeError::CaseCollision { .. })
        ));
        assert!(matches!(
            tree.insert_file("Readme", blob, 1, false),
            Err(TreeError::CaseCollision { .. })
        ));
    }

    #[test]
    fn round_trips_and_rejects_noncanonical_encodings() {
        let a = blob_id(Domain::Source, b"a");
        let b = blob_id(Domain::Source, b"b");
        let mut tree = Tree::new(Domain::Source);
        tree.insert_file("b", b, 1, true).unwrap();
        tree.insert_file("a", a, 1, false).unwrap();
        let bytes = tree.encode();
        assert_eq!(Tree::decode(Domain::Source, &bytes).unwrap(), tree);

        // Swap the two entries: same set, non-canonical order.
        let entry_len = 2 + 1 + 1 + 32 + 8;
        let header = 16;
        let mut swapped = bytes[..header].to_vec();
        swapped.extend_from_slice(&bytes[header + entry_len..]);
        swapped.extend_from_slice(&bytes[header..header + entry_len]);
        assert!(matches!(
            Tree::decode(Domain::Source, &swapped),
            Err(TreeError::NotSorted { .. })
        ));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Tree::decode(Domain::Source, &trailing),
            Err(TreeError::TrailingBytes)
        );
        assert_eq!(
            Tree::decode(Domain::Source, &bytes[..bytes.len() - 1]),
            Err(TreeError::Truncated)
        );

        let mut bad_kind = bytes.clone();
        bad_kind[header + 2 + 1] = 9; // kind tag of the first entry ("a")
        assert_eq!(
            Tree::decode(Domain::Source, &bad_kind),
            Err(TreeError::UnknownEntryKind(9))
        );

        let mut bad_version = bytes.clone();
        bad_version[4] = 2;
        assert_eq!(
            Tree::decode(Domain::Source, &bad_version),
            Err(TreeError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn decoder_rejects_traversal_names() {
        let mut tree = Tree::new(Domain::Source);
        tree.insert_file("aa", blob_id(Domain::Source, b""), 0, false)
            .unwrap();
        let mut bytes = tree.encode();
        // Overwrite the two-byte name with "..".
        bytes[18] = b'.';
        bytes[19] = b'.';
        assert!(matches!(
            Tree::decode(Domain::Source, &bytes),
            Err(TreeError::InvalidName {
                reason: NameError::DotOrDotDot,
                ..
            })
        ));
    }

    #[test]
    fn directory_entries_must_have_zero_size() {
        let mut child = Tree::new(Domain::Source);
        child
            .insert_file("f", blob_id(Domain::Source, b""), 0, false)
            .unwrap();
        let mut tree = Tree::new(Domain::Source);
        tree.insert_dir("d", child.id()).unwrap();
        let mut bytes = tree.encode();
        let size_offset = bytes.len() - 8;
        bytes[size_offset] = 1;
        assert!(matches!(
            Tree::decode(Domain::Source, &bytes),
            Err(TreeError::DirectorySize { .. })
        ));
    }
}
