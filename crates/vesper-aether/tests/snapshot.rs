use vesper_aether::*;
use vesper_loom_cas::{Domain, ObjectKind, Store, Tree, blob_id};

fn private(vm: u64, slot_id: u32, gpa: u64, hva: u64, pages: Vec<PageMapping>) -> SlotSnapshot {
    SlotSnapshot {
        slot: MemorySlot {
            vm_id: vm,
            slot_id,
            guest_start: gpa,
            host_virtual_start: hva,
            byte_len: pages.len() as u64 * PAGE_SIZE_4K,
            permission: Permission::Private,
        },
        pages,
    }
}
fn ro(vm: u64, hva: u64, p: CasPage) -> SlotSnapshot {
    SlotSnapshot {
        slot: MemorySlot {
            vm_id: vm,
            slot_id: 0,
            guest_start: 0,
            host_virtual_start: hva,
            byte_len: PAGE_SIZE_4K,
            permission: Permission::ReadOnlyCas {
                object: p.object,
                object_len: p.object_len,
                offset: p.offset,
            },
        },
        pages: vec![PageMapping::ReadOnlyCas(p)],
    }
}
fn page() -> CasPage {
    CasPage {
        object: blob_id(Domain::Artifact, &[7; PAGE_BYTES]),
        object_len: PAGE_SIZE_4K,
        offset: 0,
    }
}
fn map(entries: &[SlotSnapshot]) -> Result<SnapshotMap, Error> {
    SnapshotMap::validate(entries, Limits::default())
}

#[test]
fn checked_half_open_range_boundaries() {
    let a = PageRange::new(0, PAGE_SIZE_4K).unwrap();
    let b = PageRange::new(PAGE_SIZE_4K, PAGE_SIZE_4K).unwrap();
    assert!(a.contains(0) && a.contains(4095));
    assert!(!a.contains(4096));
    assert!(!a.overlaps(b));
    assert_eq!(PageRange::new(u64::MAX - 4095, 4096), Err(Error::Overflow));
    assert_eq!(PageRange::new(0, 0), Err(Error::EmptyRange));
    assert_eq!(PageRange::new(1, 4096), Err(Error::Unaligned));
    assert_eq!(PageRange::new(0, 4097), Err(Error::Unaligned));
}
#[test]
fn independently_rejects_guest_and_host_overflow() {
    let mut s = private(1, 0, 0, 4096, vec![PageMapping::Zero]);
    s.slot.guest_start = u64::MAX - 4095;
    assert_eq!(map(&[s.clone()]).unwrap_err(), Error::Overflow);
    s.slot.guest_start = 0;
    s.slot.host_virtual_start = u64::MAX - 4095;
    assert_eq!(map(&[s]).unwrap_err(), Error::Overflow);
}
#[test]
fn duplicate_guest_and_host_conflicts_are_distinct() {
    let a = private(1, 0, 0, 4096, vec![PageMapping::Zero]);
    let mut b = private(1, 0, 4096, 8192, vec![PageMapping::Zero]);
    assert_eq!(
        map(&[a.clone(), b.clone()]).unwrap_err(),
        Error::DuplicateSlot
    );
    b.slot.slot_id = 1;
    b.slot.guest_start = 0;
    assert_eq!(
        map(&[a.clone(), b.clone()]).unwrap_err(),
        Error::GuestOverlap
    );
    b.slot.vm_id = 2;
    b.slot.host_virtual_start = 4096;
    assert_eq!(
        map(&[a.clone(), b.clone()]).unwrap_err(),
        Error::HostOverlap
    );
    b.slot.host_virtual_start = 8192;
    assert_eq!(map(&[a, b]).unwrap().slot_count(), 2);
}
#[test]
fn explicit_exact_immutable_sharing_only() {
    let a = ro(1, 4096, page());
    let mut b = ro(2, 4096, page());
    assert_eq!(map(&[a.clone(), b.clone()]).unwrap().slot_count(), 2);
    let different = CasPage {
        object: blob_id(Domain::Artifact, &[8; PAGE_BYTES]),
        ..page()
    };
    b = ro(2, 4096, different);
    assert_eq!(map(&[a.clone(), b]).unwrap_err(), Error::HostOverlap);
    let mut b = private(2, 0, 0, 4096, vec![PageMapping::Zero]);
    assert_eq!(
        map(&[a.clone(), b.clone()]).unwrap_err(),
        Error::HostOverlap
    );
    // A mutable COW page cannot alias a shared immutable HVA either.
    b.pages[0] = PageMapping::PrivateCow(page());
    assert_eq!(map(&[a, b]).unwrap_err(), Error::HostOverlap);
}
#[test]
fn partial_readonly_overlap_is_conservatively_rejected() {
    let p = CasPage {
        object: blob_id(Domain::Artifact, &[9; PAGE_BYTES * 2]),
        object_len: 8192,
        offset: 0,
    };
    let mut a = ro(1, 4096, p);
    a.slot.byte_len = 8192;
    a.pages
        .push(PageMapping::ReadOnlyCas(CasPage { offset: 4096, ..p }));
    let b = ro(2, 8192, CasPage { offset: 4096, ..p });
    assert_eq!(map(&[a, b]).unwrap_err(), Error::HostOverlap);
}
#[test]
fn complete_mapping_and_permission_checks() {
    let mut s = private(1, 0, 0, 4096, vec![PageMapping::Zero]);
    s.pages.clear();
    assert_eq!(map(&[s.clone()]).unwrap_err(), Error::IncompleteMappings);
    s.pages = vec![PageMapping::Zero; 2];
    assert_eq!(map(&[s]).unwrap_err(), Error::IncompleteMappings);
    let mut s = ro(1, 4096, page());
    s.pages[0] = PageMapping::Zero;
    assert_eq!(map(&[s.clone()]).unwrap_err(), Error::PermissionMismatch);
    s.pages[0] = PageMapping::PrivateCow(page());
    assert_eq!(map(&[s]).unwrap_err(), Error::PermissionMismatch);
    let s = private(1, 0, 0, 4096, vec![PageMapping::ReadOnlyCas(page())]);
    assert_eq!(map(&[s]).unwrap_err(), Error::PermissionMismatch);
}
#[test]
fn malformed_and_wrong_identity_metadata_fails_before_resolution() {
    for p in [
        CasPage {
            offset: 1,
            ..page()
        },
        CasPage {
            offset: 4096,
            ..page()
        },
        CasPage {
            object_len: 4097,
            ..page()
        },
        CasPage {
            offset: u64::MAX - 4095,
            ..page()
        },
    ] {
        assert!(map(&[ro(1, 4096, p)]).is_err());
    }
    for domain in [Domain::Source, Domain::Interface] {
        let p = CasPage {
            object: blob_id(domain, &[7; PAGE_BYTES]),
            ..page()
        };
        assert_eq!(map(&[ro(1, 4096, p)]).unwrap_err(), Error::InvalidIdentity);
    }
    let mut p = page();
    p.object.kind = ObjectKind::Tree;
    assert_eq!(map(&[ro(1, 4096, p)]).unwrap_err(), Error::InvalidIdentity);
    let p = CasPage {
        offset: 4096,
        object_len: 8192,
        ..page()
    };
    let mut s = ro(1, 4096, p);
    s.pages[0] = PageMapping::ReadOnlyCas(CasPage { offset: 0, ..p });
    assert_eq!(map(&[s]).unwrap_err(), Error::PermissionMismatch);
}
#[test]
fn resource_budgets_precede_copy_and_io() {
    let s = private(1, 0, 0, 4096, vec![PageMapping::Zero; 2]);
    for limits in [
        Limits {
            slots: 0,
            ..Limits::default()
        },
        Limits {
            pages: 1,
            ..Limits::default()
        },
        Limits {
            guest_bytes: 4096,
            ..Limits::default()
        },
        Limits {
            blob_bytes: u64::MAX,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            SnapshotMap::validate(std::slice::from_ref(&s), limits).unwrap_err(),
            Error::Budget
        );
    }
    let t = private(2, 0, 0, 16384, vec![PageMapping::Zero]);
    assert_eq!(
        SnapshotMap::validate(
            &[s, t],
            Limits {
                pair_checks: 0,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget
    );
    let p = CasPage {
        object_len: 8192,
        ..page()
    };
    assert_eq!(
        SnapshotMap::validate(
            &[ro(1, 4096, p)],
            Limits {
                blob_bytes: 4096,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget
    );
    assert_eq!(map(&[]).unwrap_err(), Error::EmptyLayout);
}
#[test]
fn holes_unknown_vms_and_endpoints_never_silently_zero() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let m = map(&[
        private(1, 0, 0, 16384, vec![PageMapping::Zero]),
        private(1, 1, 8192, 24576, vec![PageMapping::Zero]),
    ])
    .unwrap();
    for (vm, address) in [(2, 0), (1, 4096), (1, 8191), (1, 12288), (1, u64::MAX)] {
        assert_eq!(
            m.resolve(&store, vm, address).unwrap_err(),
            Error::UnknownFault
        );
    }
    let p = m.resolve(&store, 1, 4095).unwrap();
    assert_eq!(p.page_gpa(), 0);
    assert_eq!(p.action(), UffdPageAction::ZeroPage4K);
    assert_eq!(p.bytes(), &[0; PAGE_BYTES]);
}
#[test]
fn small_interval_model_matches_point_enumeration() {
    for a in 0..8 {
        for al in 1..4 {
            for b in 0..8 {
                for bl in 1..4 {
                    let x = PageRange::new(a * 4096, al * 4096).unwrap();
                    let y = PageRange::new(b * 4096, bl * 4096).unwrap();
                    let shared = (0..12).any(|p| x.contains(p * 4096) && y.contains(p * 4096));
                    assert_eq!(x.overlaps(y), shared);
                }
            }
        }
    }
}

#[cfg(any(unix, windows))]
mod actual_cas {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    fn setup() -> (tempfile::TempDir, Store, CasPage) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let mut bytes = vec![3; PAGE_BYTES];
        bytes.extend_from_slice(&[7; PAGE_BYTES]);
        let id = store.put_blob(Domain::Artifact, &bytes).unwrap().id;
        (
            temp,
            store,
            CasPage {
                object: id,
                object_len: 8192,
                offset: 4096,
            },
        )
    }
    fn replace(store: &Store, p: CasPage, bytes: &[u8], readonly: bool) {
        let path = store.object_path(&p.object);
        remove(&path);
        fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if readonly { 0o444 } else { 0o644 }),
        )
        .unwrap();
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(readonly);
            fs::set_permissions(path, permissions).unwrap();
        }
    }
    fn remove(path: &std::path::Path) {
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).unwrap();
        }
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn actual_temp_cas_owned_readonly_and_private_pages() {
        let (_t, store, p) = setup();
        let m = map(&[
            ro(1, 4096, p),
            private(2, 0, 0, 8192, vec![PageMapping::PrivateCow(p)]),
        ])
        .unwrap();
        let a = m.resolve(&store, 1, 42).unwrap();
        let b = m.resolve(&store, 2, 4095).unwrap();
        assert_eq!(a.bytes(), &[7; PAGE_BYTES]);
        assert_eq!(a.bytes(), b.bytes());
        assert_eq!(
            a.action(),
            UffdPageAction::ContinueSharedCas { identity: p }
        );
        assert_eq!(b.action(), UffdPageAction::CopyPrivateCow { identity: p });
        assert_eq!((a.vm_id(), a.slot_id(), a.page_gpa()), (1, 0, 0));
        // Mutation/replacement after verification cannot change the owned output.
        replace(&store, p, &[1; PAGE_BYTES * 2], true);
        assert_eq!(a.bytes(), &[7; PAGE_BYTES]);
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::CorruptObject);
    }
    #[test]
    fn rejects_missing_wrong_length_corrupt_and_writable_objects() {
        let (_t, store, p) = setup();
        let m = map(&[ro(1, 4096, p)]).unwrap();
        replace(&store, p, &[8; PAGE_BYTES * 2], true);
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::CorruptObject);
        replace(&store, p, &[7; PAGE_BYTES], true);
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::WrongLength);
        replace(&store, p, &[7; PAGE_BYTES * 3], true);
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::WrongLength);
        replace(&store, p, &[7; PAGE_BYTES * 2], false);
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::UnsafeObject);
        remove(&store.object_path(&p.object));
        assert_eq!(
            m.resolve(&store, 1, 0).unwrap_err(),
            Error::Io(std::io::ErrorKind::NotFound)
        );
    }
    #[test]
    #[cfg(unix)]
    fn rejects_symlink_hardlink_directory_and_fifo_without_blocking() {
        use std::os::unix::fs::symlink;
        let (t, store, p) = setup();
        let m = map(&[ro(1, 4096, p)]).unwrap();
        let path = store.object_path(&p.object);
        let alias = t.path().join("alias");
        fs::hard_link(&path, &alias).unwrap();
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::UnsafeObject);
        fs::remove_file(&path).unwrap();
        symlink(&alias, &path).unwrap();
        assert!(m.resolve(&store, 1, 0).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::UnsafeObject);
        fs::remove_dir(&path).unwrap();
        // External command creates a FIFO; resolver's O_NONBLOCK prevents hangs.
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(m.resolve(&store, 1, 0).unwrap_err(), Error::UnsafeObject);
    }
    #[test]
    fn metadata_rejects_real_tree_before_attempting_blob_read() {
        let (_t, store, mut p) = setup();
        p.object = store.put_tree(&Tree::new(Domain::Artifact)).unwrap().id;
        assert_eq!(map(&[ro(1, 4096, p)]).unwrap_err(), Error::InvalidIdentity);
    }
}
