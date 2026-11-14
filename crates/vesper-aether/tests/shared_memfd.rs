//! Zero-copy cross-VM snapshot pages: `UFFDIO_CONTINUE` over a sealed shared
//! memfd, hot-set prefetch, and fork clones that remap the snapshot
//! `MAP_PRIVATE` (§37.1.2, §37.1.3). Linux only; each test probes first and
//! passes with a printed reason where userfaultfd is refused (set
//! `VESPER_REQUIRE_UFFD=1` to make that a failure).
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::sync::Arc;
use vesper_aether::entropy::CloneEntropy;
use vesper_aether::fork::*;
use vesper_aether::host::*;
use vesper_aether::hotset::HotSet;
use vesper_aether::mapped_fork::*;
use vesper_aether::page_source::{PageFill, PageSource};
use vesper_aether::{Error, PAGE_BYTES};

fn available(test: &str) -> bool {
    let probe = probe_userfaultfd();
    match &probe.route {
        Ok(route) => {
            eprintln!("{test}: userfaultfd available via {route:?}");
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

/// Minor-fault support is a separate kernel feature (5.14+, shmem).
fn minor_available(test: &str, memfd: &Arc<SealedMemfd>) -> Option<MinorRegion> {
    match MinorRegion::map_private(memfd) {
        Ok(r) => Some(r),
        Err(UffdError::Unavailable(u)) => {
            assert!(
                std::env::var_os("VESPER_REQUIRE_UFFD").is_none(),
                "{test}: minor faults unavailable: {u:?}"
            );
            eprintln!("{test}: UFFD minor faults UNAVAILABLE: {u:?}");
            None
        }
        Err(e) => panic!("{test}: {e:?}"),
    }
}

fn patterned(seed: u8) -> Box<[u8; PAGE_BYTES]> {
    let mut b = Box::new([0u8; PAGE_BYTES]);
    for (i, v) in b.iter_mut().enumerate() {
        *v = seed.wrapping_add((i % 251) as u8);
    }
    b
}

struct Pages(Vec<Option<Box<[u8; PAGE_BYTES]>>>);
impl PageSource for Pages {
    fn page_count(&self) -> usize {
        self.0.len()
    }
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error> {
        match self.0.get(index) {
            Some(Some(p)) => Ok(PageFill::Borrowed(p)),
            Some(None) => Ok(PageFill::Zero),
            None => Err(Error::UnknownFault),
        }
    }
}

fn source(n: usize) -> Pages {
    Pages(
        (0..n)
            .map(|i| (i % 5 != 4).then(|| patterned(i as u8)))
            .collect(),
    )
}

#[test]
fn sealed_memfd_refuses_writes_growth_and_shared_writable_maps() {
    if !available("sealed_memfd") {
        return;
    }
    let src = source(6);
    let m = SealedMemfd::from_source(&src).unwrap();
    assert_eq!(m.seals().unwrap() & SNAPSHOT_SEALS, SNAPSHOT_SEALS);
    assert_eq!(m.try_write(), Some(libc::EPERM));
    assert_eq!(m.try_map_shared_writable(), Some(libc::EPERM));
    for i in 0..6 {
        let want = src
            .fill(i)
            .unwrap()
            .bytes()
            .map_or([0u8; PAGE_BYTES], |b| *b);
        assert_eq!(*m.read_page(i).unwrap(), want, "page {i}");
    }
    assert_eq!(m.read_page(6).unwrap_err(), UffdError::InvalidRegion);
    assert_eq!(
        SealedMemfd::build(0, |_, _| Ok(false)).unwrap_err(),
        UffdError::InvalidRegion
    );
    assert!(matches!(
        SealedMemfd::build(2, |i, _| if i == 1 {
            Err(Error::CorruptObject)
        } else {
            Ok(false)
        }),
        Err(UffdError::Source {
            index: 1,
            error: Error::CorruptObject
        })
    ));
}

#[test]
fn continue_maps_one_shared_page_into_many_private_clones() {
    if !available("continue_shared") {
        return;
    }
    let src = source(8);
    let memfd = Arc::new(SealedMemfd::from_source(&src).unwrap());
    let Some(a) = minor_available("continue_shared", &memfd) else {
        return;
    };
    let b = MinorRegion::map_private(&memfd).unwrap();
    let (pages_a, sa) = a
        .serve(64, |t| {
            (0..8).map(|i| t.read_page(i).unwrap()).collect::<Vec<_>>()
        })
        .unwrap();
    let (pages_b, sb) = b
        .serve(64, |t| {
            (0..8).map(|i| t.read_page(i).unwrap()).collect::<Vec<_>>()
        })
        .unwrap();
    for i in 0..8 {
        let want = src
            .fill(i)
            .unwrap()
            .bytes()
            .map_or([0u8; PAGE_BYTES], |p| *p);
        assert_eq!(*pages_a[i], want);
        assert_eq!(*pages_b[i], want);
    }
    // Every first touch was a minor fault resolved in place, none copied.
    assert_eq!(
        (sa.faults, sa.continued, sa.copied, sa.zeroed),
        (8, 8, 0, 0)
    );
    assert_eq!((sb.faults, sb.continued), (8, 8));
    // Same physical page behind both clones (visible to privileged readers).
    let frames: Vec<_> = (0..8)
        .map(|i| (a.frame_number(i), b.frame_number(i)))
        .collect();
    match frames[0] {
        (Some(_), Some(_)) => {
            for (i, (fa, fb)) in frames.iter().enumerate() {
                assert_eq!(fa, fb, "page {i} shared");
            }
            eprintln!("continue_shared: pagemap confirms shared frames: {frames:?}");
        }
        _ => eprintln!(
            "continue_shared: pagemap frame numbers hidden; zero-copy not observed directly"
        ),
    }
    // A write in clone A is private: B and the memfd keep the snapshot page.
    let before = a.frame_number(3);
    let (v, s) = a
        .serve(4, |t| {
            assert!(t.write_u8(3 * PAGE_BYTES + 7, 0xEE));
            t.read_u8(3 * PAGE_BYTES + 7).unwrap()
        })
        .unwrap();
    assert_eq!(
        (v, s.faults),
        (0xEE, 0),
        "already mapped: plain COW, no uffd fault"
    );
    let (vb, _) = b
        .serve(4, |t| t.read_u8(3 * PAGE_BYTES + 7).unwrap())
        .unwrap();
    assert_eq!(vb, patterned(3)[7]);
    assert_eq!(memfd.read_page(3).unwrap()[7], patterned(3)[7]);
    if let (Some(x), Some(y)) = (before, a.frame_number(3)) {
        assert_ne!(x, y, "COW gave clone A its own frame");
        assert_eq!(b.frame_number(3), before);
    }
}

#[test]
fn first_write_to_an_unmapped_page_is_a_minor_fault_then_cow() {
    if !available("write_minor") {
        return;
    }
    let memfd = Arc::new(SealedMemfd::from_source(&source(2)).unwrap());
    let Some(a) = minor_available("write_minor", &memfd) else {
        return;
    };
    let (v, s) = a
        .serve(4, |t| {
            t.write_u8(5, 9);
            t.read_u8(5).unwrap()
        })
        .unwrap();
    assert_eq!((v, s.faults, s.continued), (9, 1, 1));
    assert_eq!(memfd.read_page(0).unwrap()[5], patterned(0)[5]);
    // Budget overruns fail the session, never hang.
    let b = MinorRegion::map_private(&memfd).unwrap();
    assert_eq!(
        b.serve(1, |t| (t.read_u8(0), t.read_u8(PAGE_BYTES)))
            .unwrap_err(),
        UffdError::FaultBudget
    );
}

#[test]
fn hot_set_prefetch_removes_first_touch_faults() {
    if !available("hot_set_prefetch") {
        return;
    }
    let n = 1200; // > two 2 MiB batches
    let src = source(n);
    let memfd = Arc::new(SealedMemfd::from_source(&src).unwrap());
    let Some(cold) = minor_available("hot_set_prefetch", &memfd) else {
        return;
    };
    // Session 1: the guest touches a working set; record the fault order.
    let working: Vec<usize> = (0..600).chain(900..1000).chain([1199, 3]).collect();
    let (_, s1, order) = cold
        .serve_recording(4096, |t| {
            for &p in &working {
                t.read_u8(p * PAGE_BYTES + 1);
            }
        })
        .unwrap();
    assert_eq!(s1.faults, 701);
    let hot = HotSet::from_fault_order(n, &order, 4096).unwrap();
    assert_eq!(hot.len(), 701);
    let runs = hot.runs(512).unwrap();
    assert_eq!(runs.len(), 4, "{runs:?}"); // 0..512, 512..600, 900..1000, 1199
    // Session 2 (next clone): prefetch, then the same touches fault nowhere.
    let warm = MinorRegion::map_private(&memfd).unwrap();
    let p = warm.prefetch(&hot).unwrap();
    assert_eq!((p.prefetched, p.continued, p.faults), (701, 701, 0));
    let (_, s2) = warm
        .serve(4096, |t| {
            for &q in &working {
                assert_eq!(
                    t.read_u8(q * PAGE_BYTES + 1),
                    src.fill(q).unwrap().bytes().map(|b| b[1]).or(Some(0))
                );
            }
            // One cold page still faults.
            t.read_u8(700 * PAGE_BYTES);
        })
        .unwrap();
    assert_eq!(s2.faults, 1);
    // Prefetching again races on every page and stays correct.
    let again = warm.prefetch(&hot).unwrap();
    assert_eq!((again.raced, again.prefetched), (701, 0));
    // The hot set survives its on-disk form.
    assert_eq!(HotSet::decode(&hot.encode(), n).unwrap(), hot);
    eprintln!("hot_set_prefetch: cold session {s1:?}; warm prefetch {p:?}; warm session {s2:?}");
}

#[test]
fn copy_path_prefetch_for_private_lazy_regions() {
    if !available("copy_prefetch") {
        return;
    }
    let n = 700;
    let src = source(n);
    let region = LazyRegion::new(n).unwrap();
    let hot = HotSet::new(n, &(0..520).collect::<Vec<_>>()).unwrap();
    let p = region.prefetch(&src, &hot).unwrap();
    assert_eq!((p.prefetched, p.copied), (520, 520));
    let (ok, s) = region
        .serve(&src, 1024, |t| {
            (0..n).all(|i| {
                *t.read_page(i).unwrap()
                    == src
                        .fill(i)
                        .unwrap()
                        .bytes()
                        .map_or([0u8; PAGE_BYTES], |b| *b)
            })
        })
        .unwrap();
    assert!(ok);
    assert_eq!(s.faults, (n - 520) as u64);
}

fn image(auth: &mut ForkAuthority, root: &RootKey) -> SnapshotImage {
    let sealer = auth.boot_sealer(root).unwrap();
    quiesce_and_snapshot(
        sealer,
        QuiescedMemory {
            pages: vec![
                Some(patterned(1)),
                None,
                Some(patterned(3)),
                Some(patterned(4)),
            ],
            genid: VmGenIdLocation {
                page: 2,
                offset: 64,
            },
            host_tsc_khz: 2_000_000,
            virtio_mmio_count: 3,
        },
    )
    .unwrap()
}

#[test]
fn mapped_fork_clones_share_the_snapshot_and_reseed_privately() {
    if !available("mapped_fork") {
        return;
    }
    let root = RootKey::new([0x33; 32]);
    let mut auth = ForkAuthority::new(0xfeed);
    let img = image(&mut auth, &root);
    let sealed = SealedSnapshot::seal(&img).unwrap();
    let (mut a, ta) = match sealed.fork_mapped(&mut auth) {
        Ok(v) => v,
        Err(MappedForkError::Uffd(UffdError::Unavailable(u))) => {
            assert!(std::env::var_os("VESPER_REQUIRE_UFFD").is_none(), "{u:?}");
            eprintln!("mapped_fork: minor faults UNAVAILABLE: {u:?}");
            return;
        }
        Err(e) => panic!("{e:?}"),
    };
    let (mut b, tb) = sealed.fork_mapped(&mut auth).unwrap();
    // No guest touch before reseeding.
    assert_eq!(
        a.serve(8, |t| t.read_u8(0)).unwrap_err(),
        MappedForkError::NotReseeded
    );
    // A ticket for the other clone is handed back unconsumed.
    let (err, tb) = *tb
        .reseed_mapped(&root, &mut a, CloneEntropy::gather().unwrap())
        .unwrap_err();
    assert_eq!(err, MappedForkError::Fork(ForkError::TicketMismatch));
    let ea = CloneEntropy::gather().unwrap();
    let sources = ea.sources();
    eprintln!("mapped_fork: clone entropy sources {sources:?}");
    assert!(sources.os || sources.hardware.is_some());
    let ra = ta.reseed_mapped(&root, &mut a, ea).unwrap();
    let rb = tb
        .reseed_mapped(&root, &mut b, CloneEntropy::gather().unwrap())
        .unwrap();
    assert_ne!(ra.fresh_sealer.key_id(), rb.fresh_sealer.key_id());
    assert_ne!(ra.fork_epoch_id, rb.fork_epoch_id);
    for (clone, r) in [(&a, &ra), (&b, &rb)] {
        let (wire, s) = clone.serve(8, |t| read_genid(t, clone).unwrap()).unwrap();
        assert_eq!(wire.vm_gen_lo, r.fork_epoch_id);
        assert_eq!(
            (wire.vm_gen_hi, wire.host_tsc_khz, wire.virtio_mmio_count),
            (0xfeed, 2_000_000, 3)
        );
        assert_eq!(
            s.faults, 0,
            "the generation page was mapped during reseeding"
        );
        let (p0, _) = clone.serve(8, |t| t.read_page(0).unwrap()).unwrap();
        assert_eq!(p0, patterned(1));
    }
    // The sealed snapshot still has the parent's (unwritten) generation page.
    assert_eq!(&sealed.memfd().read_page(2).unwrap()[..], &patterned(3)[..]);
    // Untouched-by-write pages share frames; the generation page does not.
    if let (Some(x), Some(y)) = (a.frame_number(0), b.frame_number(0)) {
        assert_eq!(x, y);
        assert_ne!(a.frame_number(2), b.frame_number(2));
    }
    ra.fresh_sealer.retire();
    rb.fresh_sealer.retire();
}

#[test]
fn clone_entropy_mixes_hardware_and_os_sources() {
    let e = CloneEntropy::gather().unwrap();
    let s = e.sources();
    eprintln!("clone entropy: {s:?}");
    if cfg!(target_os = "linux") {
        assert!(s.os);
    }
    if std::env::var_os("VESPER_REQUIRE_HW_RNG").is_some() {
        assert!(s.hardware.is_some());
    }
    assert!(!s.supplied);
    assert!(CloneEntropy::supplied([1; 32]).sources().supplied);
}
