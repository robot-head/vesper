//! Hot sets persisted next to the CAS snapshot (§37.1.3).
//!
//! A CAS snapshot is a set of [`SlotSnapshot`]s whose pages are Loom CAS
//! objects. [`persist_hot_sets`] stores, in the same store:
//!
//! - the snapshot *manifest*: a canonical encoding of every slot's guest
//!   layout, permission and page identities (not its host virtual address or
//!   VM id, which differ between restores of the same snapshot), and
//! - one encoded [`HotSet`] per slot that has one,
//!
//! and binds them in a CAS tree (`manifest`, `hotset-<slot_id>`), the
//! *bundle*, whose identity names "this snapshot with these hot sets". A new
//! recording makes a new bundle; nothing is overwritten.
//!
//! [`load_hot_sets`] treats the bundle as untrusted input: it must be an
//! artifact tree with exactly a manifest and well-formed `hotset-<n>`
//! entries, the manifest must be byte-for-byte the manifest of the snapshot
//! the caller is restoring (a hot set recorded for another snapshot is
//! [`HotSetStoreError::ForeignSnapshot`]), every slot named must exist, sizes
//! are bounded before any blob is read, every blob is verified by the store,
//! and every hot set is fully validated against its slot's page count.
#![forbid(unsafe_code)]

use crate::hotset::{HotSet, HotSetError, MAX_HOT_PAGES};
use crate::{Limits, PageMapping, Permission, SlotSnapshot, SnapshotMap};
use std::collections::BTreeMap;
use vesper_loom_cas::{CasError, Domain, ObjectId, ObjectKind, Store, Tree, TreeError, blob_id};

const MANIFEST_MAGIC: &[u8; 4] = b"AMF1";
/// The bundle entry holding the snapshot manifest.
pub const MANIFEST_ENTRY: &str = "manifest";
const HOTSET_PREFIX: &str = "hotset-";
/// Largest encoded hot set accepted (16-byte header, 4 bytes per page).
const MAX_HOTSET_BYTES: u64 = 16 + 4 * MAX_HOT_PAGES as u64;

#[derive(Debug)]
pub enum HotSetStoreError {
    /// The slots are not a valid snapshot.
    Snapshot(crate::Error),
    Cas(CasError),
    Tree(TreeError),
    HotSet { slot_id: u32, error: HotSetError },
    /// A hot set for a slot the snapshot does not have.
    UnknownSlot(u32),
    /// A hot set whose page count differs from its slot's.
    PageCount { slot_id: u32 },
    /// The bundle id is not an artifact-domain tree.
    NotABundle,
    MissingManifest,
    /// The bundle's manifest is not this snapshot's.
    ForeignSnapshot,
    /// An entry that is not `manifest` or a canonical `hotset-<slot_id>` file.
    UnexpectedEntry(String),
    /// An entry larger than any valid manifest or hot set.
    Oversized(String),
}
impl From<CasError> for HotSetStoreError {
    fn from(value: CasError) -> Self {
        Self::Cas(value)
    }
}
impl From<TreeError> for HotSetStoreError {
    fn from(value: TreeError) -> Self {
        Self::Tree(value)
    }
}

/// The bundle and manifest identities a persist produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistedHotSets {
    pub bundle: ObjectId,
    pub manifest: ObjectId,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}
fn put_object(out: &mut Vec<u8>, id: &ObjectId) {
    put_str(out, id.domain.as_str());
    put_str(out, id.kind.as_str());
    out.extend_from_slice(id.digest.as_bytes());
}

/// The canonical manifest of a snapshot: for every slot (in the given order)
/// its id, guest range, permission and page identities. Validates the slots
/// as a [`SnapshotMap`] first.
pub fn snapshot_manifest(slots: &[SlotSnapshot]) -> Result<Vec<u8>, HotSetStoreError> {
    SnapshotMap::validate(slots, Limits::default()).map_err(HotSetStoreError::Snapshot)?;
    let mut out = Vec::new();
    out.extend_from_slice(MANIFEST_MAGIC);
    out.extend_from_slice(&(slots.len() as u32).to_le_bytes());
    for s in slots {
        out.extend_from_slice(&s.slot.slot_id.to_le_bytes());
        out.extend_from_slice(&s.slot.guest_start.to_le_bytes());
        out.extend_from_slice(&s.slot.byte_len.to_le_bytes());
        match s.slot.permission {
            Permission::Private => out.push(0),
            Permission::ReadOnlyCas {
                object,
                object_len,
                offset,
            } => {
                out.push(1);
                put_object(&mut out, &object);
                out.extend_from_slice(&object_len.to_le_bytes());
                out.extend_from_slice(&offset.to_le_bytes());
            }
        }
        out.extend_from_slice(&(s.pages.len() as u32).to_le_bytes());
        for p in &s.pages {
            let (tag, page) = match p {
                PageMapping::Zero => (0, None),
                PageMapping::ReadOnlyCas(c) => (1, Some(c)),
                PageMapping::PrivateCow(c) => (2, Some(c)),
            };
            out.push(tag);
            if let Some(c) = page {
                put_object(&mut out, &c.object);
                out.extend_from_slice(&c.object_len.to_le_bytes());
                out.extend_from_slice(&c.offset.to_le_bytes());
            }
        }
    }
    Ok(out)
}

fn slot_pages(slots: &[SlotSnapshot], slot_id: u32) -> Option<usize> {
    slots
        .iter()
        .find(|s| s.slot.slot_id == slot_id)
        .map(|s| s.pages.len())
}

/// Stores the snapshot manifest and `hot` (slot id -> hot set) and returns
/// the bundle binding them.
pub fn persist_hot_sets(
    store: &Store,
    slots: &[SlotSnapshot],
    hot: &BTreeMap<u32, HotSet>,
) -> Result<PersistedHotSets, HotSetStoreError> {
    let manifest = snapshot_manifest(slots)?;
    for (&slot_id, set) in hot {
        let pages = slot_pages(slots, slot_id).ok_or(HotSetStoreError::UnknownSlot(slot_id))?;
        if set.page_count() != pages {
            return Err(HotSetStoreError::PageCount { slot_id });
        }
    }
    let mut tree = Tree::new(Domain::Artifact);
    let m = store.put_blob(Domain::Artifact, &manifest)?;
    tree.insert_file(MANIFEST_ENTRY, m.id, m.size, false)?;
    for (slot_id, set) in hot {
        let blob = store.put_blob(Domain::Artifact, &set.encode())?;
        tree.insert_file(&format!("{HOTSET_PREFIX}{slot_id}"), blob.id, blob.size, false)?;
    }
    let bundle = store.put_tree(&tree)?;
    Ok(PersistedHotSets {
        bundle: bundle.id,
        manifest: m.id,
    })
}

/// `hotset-<n>` with `n` in canonical decimal form, or `None`.
fn parse_entry(name: &str) -> Option<u32> {
    let n: u32 = name.strip_prefix(HOTSET_PREFIX)?.parse().ok()?;
    (format!("{HOTSET_PREFIX}{n}") == name).then_some(n)
}

/// Reads the hot sets of `bundle` for the snapshot `slots` (see the module
/// docs for what is checked).
pub fn load_hot_sets(
    store: &Store,
    bundle: &ObjectId,
    slots: &[SlotSnapshot],
) -> Result<BTreeMap<u32, HotSet>, HotSetStoreError> {
    if bundle.domain != Domain::Artifact || bundle.kind != ObjectKind::Tree {
        return Err(HotSetStoreError::NotABundle);
    }
    let expected = snapshot_manifest(slots)?;
    let tree = store.read_tree(bundle)?;
    let entry = tree
        .get(MANIFEST_ENTRY)
        .ok_or(HotSetStoreError::MissingManifest)?;
    let want = blob_id(Domain::Artifact, &expected);
    if entry.kind.object_kind() != ObjectKind::Blob
        || entry.size != expected.len() as u64
        || tree.child_id(entry) != want
    {
        return Err(HotSetStoreError::ForeignSnapshot);
    }
    let mut out = BTreeMap::new();
    for (name, entry) in tree.entries() {
        if name == MANIFEST_ENTRY {
            continue;
        }
        let slot_id = parse_entry(name)
            .filter(|_| entry.kind.object_kind() == ObjectKind::Blob)
            .ok_or_else(|| HotSetStoreError::UnexpectedEntry(name.to_owned()))?;
        let pages = slot_pages(slots, slot_id).ok_or(HotSetStoreError::UnknownSlot(slot_id))?;
        if entry.size > MAX_HOTSET_BYTES {
            return Err(HotSetStoreError::Oversized(name.to_owned()));
        }
        let bytes = store.read_blob(&tree.child_id(entry))?;
        let set = HotSet::decode(&bytes, pages)
            .map_err(|error| HotSetStoreError::HotSet { slot_id, error })?;
        out.insert(slot_id, set);
    }
    Ok(out)
}
