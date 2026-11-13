//! Real userfaultfd lazy restore (§37.5) where the host permits it.
//!
//! Each test probes first. Where userfaultfd is refused (non-Linux, sysctl
//! `vm.unprivileged_userfaultfd=0` without `CAP_SYS_PTRACE`, seccomp) the test
//! prints the typed reason and passes without claiming the kernel path ran.
//! Set `VESPER_REQUIRE_UFFD=1` to turn that skip into a failure.
use vesper_aether::fork::*;
use vesper_aether::host::*;
use vesper_aether::page_source::{PageFill, PageSource};
use vesper_aether::*;
use vesper_loom_cas::{Domain, Store};

fn available(test: &str) -> bool {
    let probe = probe_userfaultfd();
    match &probe.route {
        Ok(route) => {
            eprintln!("{test}: userfaultfd available via {route:?} ({probe:?})");
            true
        }
        Err(reason) => {
            assert!(
                std::env::var_os("VESPER_REQUIRE_UFFD").is_none(),
                "{test}: VESPER_REQUIRE_UFFD set but userfaultfd unavailable: {reason:?}"
            );
            eprintln!("{test}: userfaultfd UNAVAILABLE, kernel path not exercised: {probe:?}");
            false
        }
    }
}

fn patterned(seed: u8) -> Box<[u8; PAGE_BYTES]> {
    let mut b = Box::new([0u8; PAGE_BYTES]);
    for (i, v) in b.iter_mut().enumerate() {
        *v = seed.wrapping_add((i % 251) as u8);
    }
    b
}

#[test]
fn probe_is_typed_everywhere() {
    let probe = probe_userfaultfd();
    if cfg!(not(target_os = "linux")) {
        assert_eq!(probe.route, Err(Unavailable::UnsupportedPlatform));
    }
    if let Err(Unavailable::UserfaultfdDenied { errno, .. }) = &probe.route {
        assert_ne!(*errno, 0);
    }
    available("probe_is_typed_everywhere");
}

#[test]
fn restores_cas_snapshot_pages_on_first_touch() {
    if !available("restores_cas_snapshot_pages_on_first_touch") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut blob = Vec::new();
    for seed in [10u8, 20, 30] {
        blob.extend_from_slice(&*patterned(seed));
    }
    let id = store.put_blob(Domain::Artifact, &blob).unwrap().id;
    let page = |n: u64| CasPage {
        object: id,
        object_len: 3 * PAGE_SIZE_4K,
        offset: n * PAGE_SIZE_4K,
    };
    let entries = [SlotSnapshot {
        slot: MemorySlot {
            vm_id: 1,
            slot_id: 0,
            guest_start: 0x10_0000,
            host_virtual_start: 0x7000_0000,
            byte_len: 4 * PAGE_SIZE_4K,
            permission: Permission::Private,
        },
        pages: vec![
            PageMapping::PrivateCow(page(2)),
            PageMapping::Zero,
            PageMapping::PrivateCow(page(0)),
            PageMapping::PrivateCow(page(1)),
        ],
    }];
    let map = SnapshotMap::validate(&entries, Limits::default()).unwrap();
    let source = map.slot_source(&store, 1, 0).unwrap();
    let region = LazyRegion::new(4).unwrap();
    let (pages, stats) = region
        .serve(&source, 64, |t| {
            // Touch out of order; the write fault on page 1 is served first.
            assert!(t.write_u8(PAGE_BYTES + 5, 0xee));
            let pages: Vec<_> = [3, 0, 2, 1].map(|i| t.read_page(i).unwrap()).into();
            assert_eq!(t.read_page(4), None);
            assert_eq!(t.read_u8(4 * PAGE_BYTES), None);
            pages
        })
        .unwrap();
    assert_eq!(pages[0], patterned(20));
    assert_eq!(pages[1], patterned(30));
    assert_eq!(pages[2], patterned(10));
    let mut expected_zero = Box::new([0u8; PAGE_BYTES]);
    expected_zero[5] = 0xee;
    assert_eq!(pages[3], expected_zero);
    assert_eq!(stats.faults, 4);
    assert_eq!(stats.copied, 3);
    assert_eq!(stats.zeroed, 1);
    eprintln!("CAS lazy restore served {stats:?} via {:?}", region.route());
}

struct Counting {
    pages: usize,
}
impl PageSource for Counting {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error> {
        if index % 5 == 4 {
            return Ok(PageFill::Zero);
        }
        Ok(PageFill::Owned(patterned(index as u8)))
    }
}

#[test]
fn concurrent_touchers_see_each_page_once() {
    if !available("concurrent_touchers_see_each_page_once") {
        return;
    }
    let pages = 64;
    let region = LazyRegion::new(pages).unwrap();
    let source = Counting { pages };
    let ((), stats) = region
        .serve(&source, 4 * pages as u64, |t| {
            std::thread::scope(|s| {
                for worker in 0..4 {
                    s.spawn(move || {
                        for k in 0..pages {
                            let i = (k * 7 + worker * 13) % pages;
                            let got = t.read_page(i).unwrap();
                            if i % 5 == 4 {
                                assert_eq!(*got, [0; PAGE_BYTES]);
                            } else {
                                assert_eq!(got, patterned(i as u8));
                            }
                        }
                    });
                }
            });
        })
        .unwrap();
    assert_eq!(stats.copied + stats.zeroed, pages as u64);
    assert_eq!(stats.faults, stats.copied + stats.zeroed + stats.raced);
}

struct Failing;
impl PageSource for Failing {
    fn page_count(&self) -> usize {
        3
    }
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error> {
        if index == 1 {
            Err(Error::CorruptObject)
        } else {
            Ok(PageFill::Zero)
        }
    }
}

#[test]
fn source_errors_and_budget_overruns_fail_without_hanging() {
    if !available("source_errors_and_budget_overruns_fail_without_hanging") {
        return;
    }
    let region = LazyRegion::new(3).unwrap();
    let err = region
        .serve(&Failing, 8, |t| t.read_u8(PAGE_BYTES).unwrap())
        .unwrap_err();
    assert_eq!(
        err,
        UffdError::Source {
            index: 1,
            error: Error::CorruptObject
        }
    );
    // The region was unregistered to release the toucher; it is now refused.
    assert_eq!(
        region.serve(&Failing, 8, |_| ()).unwrap_err(),
        UffdError::InvalidRegion
    );
    let region = LazyRegion::new(3).unwrap();
    let err = region
        .serve(&Counting { pages: 3 }, 1, |t| {
            t.read_u8(0);
            t.read_u8(2 * PAGE_BYTES);
        })
        .unwrap_err();
    assert_eq!(err, UffdError::FaultBudget);
    let region = LazyRegion::new(2).unwrap();
    assert_eq!(
        region.serve(&Counting { pages: 3 }, 8, |_| ()).unwrap_err(),
        UffdError::InvalidRegion
    );
    assert_eq!(LazyRegion::new(0).unwrap_err(), UffdError::InvalidRegion);
}

/// Snapshot fork in the UFFD model: two reseeded clones of one golden image are
/// lazily restored into separate regions; each guest view sees its own
/// generation block and private writes, and an un-reseeded clone is refused.
#[test]
fn forked_clones_restore_with_their_own_generation() {
    if !available("forked_clones_restore_with_their_own_generation") {
        return;
    }
    let genid = VmGenIdLocation {
        page: 2,
        offset: 128,
    };
    let root = RootKey::new([0x5a; 32]);
    let mut auth = ForkAuthority::new(0x1234_5678);
    let sealer = auth.boot_sealer(&root).unwrap();
    let image = quiesce_and_snapshot(
        sealer,
        QuiescedMemory {
            pages: vec![
                Some(patterned(1)),
                None,
                Some(patterned(3)),
                Some(patterned(4)),
            ],
            genid,
            host_tsc_khz: 1_000_000,
            virtio_mmio_count: 2,
        },
    )
    .unwrap();
    let mut views = Vec::new();
    for marker in [0xa1u8, 0xb2] {
        let (mut clone, ticket) = image.fork(&mut auth).unwrap();
        clone.write(3 * PAGE_BYTES + 9, &[marker]).unwrap();
        let unseeded = LazyRegion::new(4).unwrap();
        assert_eq!(
            unseeded.serve(&clone, 8, |t| t.read_u8(0)).unwrap_err(),
            UffdError::Source {
                index: 0,
                error: Error::NotReseeded
            }
        );
        let epoch = ticket.reseed_after_fork(&root, &mut clone).unwrap();
        let region = LazyRegion::new(4).unwrap();
        let (view, stats) = region
            .serve(&clone, 8, |t| {
                (0..4).map(|i| t.read_page(i).unwrap()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!((stats.copied, stats.zeroed), (3, 1));
        let wire = VmGenIdWire::decode(&view[2][128..128 + VMGENID_WIRE_BYTES]).unwrap();
        assert_eq!(wire.vm_gen_lo, epoch.fork_epoch_id);
        assert_eq!(wire.vm_gen_hi, 0x1234_5678);
        assert_eq!(view[3][9], marker);
        assert_eq!(view[0], patterned(1));
        // Only the generation page and the written page left the shared image.
        assert_eq!(clone.private_page_count(), 2);
        assert!(clone.is_shared(0));
        views.push((wire, epoch.fresh_sealer));
    }
    assert_ne!(views[0].0, views[1].0);
    for (_, s) in views {
        s.retire();
    }
}

struct Panicking;
impl PageSource for Panicking {
    fn page_count(&self) -> usize {
        2
    }
    fn fill(&self, _index: usize) -> Result<PageFill<'_>, Error> {
        panic!("page source exploded");
    }
}

#[test]
fn panicking_source_releases_the_toucher_and_propagates() {
    if !available("panicking_source_releases_the_toucher_and_propagates") {
        return;
    }
    let region = LazyRegion::new(2).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        region.serve(&Panicking, 8, |t| t.read_u8(0))
    }));
    let panic = outcome.unwrap_err();
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"page source exploded"));
    assert_eq!(
        region.serve(&Counting { pages: 2 }, 8, |_| ()).unwrap_err(),
        UffdError::InvalidRegion
    );
}

#[test]
fn concurrent_sessions_on_one_region_are_refused() {
    if !available("concurrent_sessions_on_one_region_are_refused") {
        return;
    }
    let region = LazyRegion::new(2).unwrap();
    let source = Counting { pages: 2 };
    let inside = std::sync::Barrier::new(2);
    let done = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        let first = s.spawn(|| {
            region.serve(&source, 8, |t| {
                inside.wait();
                done.wait();
                t.read_u8(0)
            })
        });
        inside.wait();
        assert_eq!(
            region.serve(&source, 8, |_| ()).unwrap_err(),
            UffdError::Busy
        );
        done.wait();
        let (byte, stats) = first.join().unwrap().unwrap();
        assert_eq!(byte, Some(0));
        assert_eq!(stats.copied, 1);
    });
    // The session ended, so the region can serve again.
    let (byte, _) = region
        .serve(&source, 8, |t| t.read_u8(PAGE_BYTES + 1))
        .unwrap();
    assert_eq!(byte, Some(2));
}
