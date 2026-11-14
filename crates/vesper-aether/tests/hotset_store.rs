//! Hot sets persisted next to the CAS snapshot in a Loom store, and the
//! checks on reading them back (§37.1.3).
use std::collections::BTreeMap;
use vesper_aether::hotset::HotSet;
use vesper_aether::hotset_store::*;
use vesper_aether::*;
use vesper_loom_cas::{Domain, ObjectKind, Store, Tree};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

/// A two-slot snapshot whose pages live in one 4-page CAS blob.
fn snapshot(store: &Store, vm: u64, hva: u64) -> Vec<SlotSnapshot> {
    let blob: Vec<u8> = (0..4 * PAGE_BYTES).map(|i| (i / PAGE_BYTES) as u8 + 1).collect();
    let object = store.put_blob(Domain::Artifact, &blob).unwrap().id;
    let cas = |page: u64| CasPage {
        object,
        object_len: 4 * PAGE_SIZE_4K,
        offset: page * PAGE_SIZE_4K,
    };
    let slot = |slot_id, gpa, hva, pages: Vec<PageMapping>| SlotSnapshot {
        slot: MemorySlot {
            vm_id: vm,
            slot_id,
            guest_start: gpa,
            host_virtual_start: hva,
            byte_len: pages.len() as u64 * PAGE_SIZE_4K,
            permission: Permission::Private,
        },
        pages,
    };
    vec![
        slot(
            0,
            0,
            hva,
            vec![
                PageMapping::PrivateCow(cas(0)),
                PageMapping::Zero,
                PageMapping::PrivateCow(cas(1)),
                PageMapping::PrivateCow(cas(2)),
            ],
        ),
        slot(
            1,
            0x10_0000,
            hva + 0x10_0000,
            vec![PageMapping::PrivateCow(cas(3)), PageMapping::Zero],
        ),
    ]
}

#[test]
fn hot_sets_round_trip_through_the_store() {
    let (_d, store) = store();
    let slots = snapshot(&store, 1, 0x7000_0000);
    let mut hot = BTreeMap::new();
    hot.insert(0, HotSet::from_fault_order(4, &[3, 0, 3, 2], 16).unwrap());
    hot.insert(1, HotSet::new(2, &[1]).unwrap());
    let p = persist_hot_sets(&store, &slots, &hot).unwrap();
    assert_eq!((p.bundle.domain, p.bundle.kind), (Domain::Artifact, ObjectKind::Tree));
    assert!(store.contains(&p.manifest).unwrap());
    let back = load_hot_sets(&store, &p.bundle, &slots).unwrap();
    assert_eq!(back, hot);
    assert_eq!(back[&0].pages(), &[0, 2, 3]);
    // The same snapshot restored into another VM at another host address
    // has the same manifest, so its hot sets load.
    let moved = snapshot(&store, 99, 0x9000_0000);
    assert_eq!(load_hot_sets(&store, &p.bundle, &moved).unwrap(), hot);
    // Persisting again is content addressed: the same bundle.
    assert_eq!(persist_hot_sets(&store, &slots, &hot).unwrap(), p);
    // A new recording is a new bundle; the old one still loads.
    let mut newer = hot.clone();
    newer.insert(0, HotSet::new(4, &[1]).unwrap());
    let q = persist_hot_sets(&store, &slots, &newer).unwrap();
    assert_ne!(q.bundle, p.bundle);
    assert_eq!(q.manifest, p.manifest);
    assert_eq!(load_hot_sets(&store, &p.bundle, &slots).unwrap(), hot);
    // No hot sets at all is a valid bundle.
    let empty = persist_hot_sets(&store, &slots, &BTreeMap::new()).unwrap();
    assert!(load_hot_sets(&store, &empty.bundle, &slots).unwrap().is_empty());
}

#[test]
fn hot_sets_of_another_snapshot_are_refused() {
    let (_d, store) = store();
    let slots = snapshot(&store, 1, 0x7000_0000);
    let mut hot = BTreeMap::new();
    hot.insert(0, HotSet::new(4, &[0]).unwrap());
    let p = persist_hot_sets(&store, &slots, &hot).unwrap();
    // One page identity differs: another snapshot.
    let mut other = slots.clone();
    other[0].pages[1] = other[0].pages[0];
    assert!(matches!(
        load_hot_sets(&store, &p.bundle, &other),
        Err(HotSetStoreError::ForeignSnapshot)
    ));
    // So does a moved guest range.
    let mut moved = slots.clone();
    moved[1].slot.guest_start = 0x20_0000;
    assert!(matches!(
        load_hot_sets(&store, &p.bundle, &moved),
        Err(HotSetStoreError::ForeignSnapshot)
    ));
}

#[test]
fn malformed_bundles_and_inputs_are_typed_errors() {
    let (_d, store) = store();
    let slots = snapshot(&store, 1, 0x7000_0000);
    // Persist-side validation.
    let mut bad = BTreeMap::new();
    bad.insert(7, HotSet::new(4, &[0]).unwrap());
    assert!(matches!(
        persist_hot_sets(&store, &slots, &bad),
        Err(HotSetStoreError::UnknownSlot(7))
    ));
    let mut bad = BTreeMap::new();
    bad.insert(1, HotSet::new(4, &[0]).unwrap());
    assert!(matches!(
        persist_hot_sets(&store, &slots, &bad),
        Err(HotSetStoreError::PageCount { slot_id: 1 })
    ));
    let mut dup = slots.clone();
    dup[1].slot.slot_id = 0;
    assert!(matches!(
        snapshot_manifest(&dup),
        Err(HotSetStoreError::Snapshot(Error::DuplicateSlot))
    ));
    // A blob id is not a bundle.
    let p = persist_hot_sets(&store, &slots, &BTreeMap::new()).unwrap();
    assert!(matches!(
        load_hot_sets(&store, &p.manifest, &slots),
        Err(HotSetStoreError::NotABundle)
    ));
    let manifest = snapshot_manifest(&slots).unwrap();
    let m = store.put_blob(Domain::Artifact, &manifest).unwrap();
    let put = |tree: &Tree| store.put_tree(tree).unwrap().id;
    // No manifest.
    let t = Tree::new(Domain::Artifact);
    assert!(matches!(
        load_hot_sets(&store, &put(&t), &slots),
        Err(HotSetStoreError::MissingManifest)
    ));
    // Non-canonical and unknown entry names.
    for name in ["hotset-01", "hotset-x", "notes", "hotset-"] {
        let mut t = Tree::new(Domain::Artifact);
        t.insert_file(MANIFEST_ENTRY, m.id, m.size, false).unwrap();
        let b = store.put_blob(Domain::Artifact, &HotSet::new(4, &[0]).unwrap().encode()).unwrap();
        t.insert_file(name, b.id, b.size, false).unwrap();
        assert!(
            matches!(
                load_hot_sets(&store, &put(&t), &slots),
                Err(HotSetStoreError::UnexpectedEntry(n)) if n == name
            ),
            "{name}"
        );
    }
    // A hot set for a slot the snapshot lacks, and a corrupt hot set blob.
    let mut t = Tree::new(Domain::Artifact);
    t.insert_file(MANIFEST_ENTRY, m.id, m.size, false).unwrap();
    let b = store.put_blob(Domain::Artifact, &HotSet::new(4, &[0]).unwrap().encode()).unwrap();
    t.insert_file("hotset-5", b.id, b.size, false).unwrap();
    assert!(matches!(
        load_hot_sets(&store, &put(&t), &slots),
        Err(HotSetStoreError::UnknownSlot(5))
    ));
    let mut t = Tree::new(Domain::Artifact);
    t.insert_file(MANIFEST_ENTRY, m.id, m.size, false).unwrap();
    // Encoded for 4 pages but slot 1 has 2: indices out of range.
    let b = store.put_blob(Domain::Artifact, &HotSet::new(4, &[3]).unwrap().encode()).unwrap();
    t.insert_file("hotset-1", b.id, b.size, false).unwrap();
    assert!(matches!(
        load_hot_sets(&store, &put(&t), &slots),
        Err(HotSetStoreError::HotSet { slot_id: 1, .. })
    ));
    let mut t = Tree::new(Domain::Artifact);
    t.insert_file(MANIFEST_ENTRY, m.id, m.size, false).unwrap();
    let junk = store.put_blob(Domain::Artifact, b"not a hot set").unwrap();
    t.insert_file("hotset-0", junk.id, junk.size, false).unwrap();
    assert!(matches!(
        load_hot_sets(&store, &put(&t), &slots),
        Err(HotSetStoreError::HotSet { slot_id: 0, .. })
    ));
    // A manifest entry of the wrong kind is not this snapshot's.
    let mut t = Tree::new(Domain::Artifact);
    let sub = put(&Tree::new(Domain::Artifact));
    t.insert_dir(MANIFEST_ENTRY, sub).unwrap();
    assert!(matches!(
        load_hot_sets(&store, &put(&t), &slots),
        Err(HotSetStoreError::ForeignSnapshot)
    ));
}
