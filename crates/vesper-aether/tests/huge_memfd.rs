//! 2 MiB huge-page snapshot memfds (§37.1.3): `MFD_HUGETLB | MFD_HUGE_2MB`,
//! sealed, mapped `MAP_PRIVATE` by several clones and served with
//! `UFFDIO_CONTINUE` under `UFFD_FEATURE_MINOR_HUGETLBFS`, one 2 MiB fault per
//! huge page. Each test probes first: without hugetlbfs or free pages in the
//! pool (`vm.nr_hugepages`) it passes with a printed reason
//! (`VESPER_REQUIRE_HUGEPAGES=1` turns that into a failure).
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::sync::Arc;
use vesper_aether::PAGE_BYTES;
use vesper_aether::host::*;
use vesper_aether::hotset::HotSet;

/// Two huge pages of 4 KiB pages.
const PAGES: usize = 2 * PAGES_PER_HUGE_PAGE;

fn byte(page: usize, offset: usize) -> u8 {
    (page as u8).wrapping_mul(31).wrapping_add((offset % 253) as u8)
}

fn fill(index: usize, page: &mut [u8; PAGE_BYTES]) -> Result<bool, vesper_aether::Error> {
    if index % 7 == 6 {
        return Ok(false); // zero page
    }
    for (o, b) in page.iter_mut().enumerate() {
        *b = byte(index, o);
    }
    Ok(true)
}

fn want(page: usize, offset: usize) -> u8 {
    if page % 7 == 6 { 0 } else { byte(page, offset) }
}

fn skip(test: &str, why: impl std::fmt::Debug) {
    assert!(
        std::env::var_os("VESPER_REQUIRE_HUGEPAGES").is_none(),
        "{test}: VESPER_REQUIRE_HUGEPAGES set but huge pages unavailable: {why:?}"
    );
    eprintln!("{test}: huge pages UNAVAILABLE, path not exercised: {why:?}");
}

/// A sealed huge memfd, or `None` (with the reason printed).
fn huge(test: &str) -> Option<Arc<SealedMemfd>> {
    if probe_userfaultfd().route.is_err() {
        skip(test, probe_userfaultfd());
        return None;
    }
    match SealedMemfd::build_huge(PAGES, fill) {
        Ok(m) => Some(Arc::new(m)),
        Err(UffdError::Unavailable(u)) => {
            skip(test, u);
            None
        }
        Err(e) => panic!("{test}: {e:?}"),
    }
}

fn region(test: &str, memfd: &Arc<SealedMemfd>) -> Option<MinorRegion> {
    match MinorRegion::map_private(memfd) {
        Ok(r) => Some(r),
        Err(e @ (UffdError::Unavailable(_) | UffdError::Os { op: "mmap", .. })) => {
            skip(test, e);
            None
        }
        Err(e) => panic!("{test}: {e:?}"),
    }
}

#[test]
fn huge_memfd_is_sealed_and_holds_the_snapshot() {
    let Some(m) = huge("huge_sealed") else {
        return;
    };
    assert!(m.is_huge());
    assert_eq!(m.page_count(), PAGES);
    assert_eq!(m.seals().unwrap() & SNAPSHOT_SEALS, SNAPSHOT_SEALS);
    assert!(m.try_map_shared_writable().is_some(), "no writable shared map");
    for i in [0, 1, 6, 511, 512, 1000, PAGES - 1] {
        let p = m.read_page(i).unwrap();
        assert_eq!(p[0], want(i, 0), "page {i}");
        assert_eq!(p[PAGE_BYTES - 1], want(i, PAGE_BYTES - 1));
    }
    // Not a whole number of huge pages.
    assert_eq!(
        SealedMemfd::build_huge(PAGES_PER_HUGE_PAGE + 1, fill).unwrap_err(),
        UffdError::InvalidRegion
    );
}

#[test]
fn preferring_huge_falls_back_to_small_pages() {
    if probe_userfaultfd().route.is_err() {
        return;
    }
    // 3 pages cannot be huge: small pages, same contents.
    let m = SealedMemfd::build_preferring_huge(3, fill).unwrap();
    assert!(!m.is_huge());
    assert_eq!(m.read_page(2).unwrap()[5], want(2, 5));
    let m = SealedMemfd::build_preferring_huge(PAGES, fill).unwrap();
    eprintln!(
        "prefer_huge: {} MiB memfd is_huge={} (HugePages_Free now {:?})",
        PAGES * PAGE_BYTES >> 20,
        m.is_huge(),
        free_huge_pages()
    );
    assert_eq!(m.read_page(PAGES - 1).unwrap()[9], want(PAGES - 1, 9));
}

#[test]
fn huge_clones_fault_two_mib_at_a_time_and_share_frames() {
    let Some(memfd) = huge("huge_clones") else {
        return;
    };
    let Some(a) = region("huge_clones", &memfd) else {
        return;
    };
    let Some(b) = region("huge_clones", &memfd) else {
        return;
    };
    assert_eq!(a.fault_granule(), 2 << 20);
    let check = |t: &Toucher<'_>| {
        for i in 0..PAGES {
            for o in [0, 1234, PAGE_BYTES - 1] {
                assert_eq!(t.read_u8(i * PAGE_BYTES + o), Some(want(i, o)), "{i}+{o}");
            }
        }
    };
    let ((), sa, order) = a.serve_recording(16, check).unwrap();
    // 1024 pages touched, two faults: one per 2 MiB page.
    assert_eq!((sa.faults, sa.continued, sa.copied), (2, 2, 0));
    assert_eq!(order, vec![0, PAGES_PER_HUGE_PAGE]);
    let ((), sb) = b.serve(16, check).unwrap();
    assert_eq!(sb.faults, 2);
    match (a.frame_number(700), b.frame_number(700)) {
        (Some(x), Some(y)) => {
            assert_eq!(x, y, "both clones map the memfd's huge page");
            // Frames inside one huge page are contiguous.
            assert_eq!(a.frame_number(512).map(|f| f + 188), Some(x));
            eprintln!("huge_clones: pagemap confirms shared huge frames ({x:#x})");
        }
        _ => eprintln!("huge_clones: frame numbers hidden; sharing not observed directly"),
    }
    // A write in A copies on write (a whole huge page); B and the memfd keep
    // the snapshot bytes.
    let (v, s) = a
        .serve(4, |t| {
            assert!(t.write_u8(5 * PAGE_BYTES + 3, 0xEE));
            t.read_u8(5 * PAGE_BYTES + 3).unwrap()
        })
        .unwrap();
    assert_eq!((v, s.faults), (0xEE, 0));
    let (vb, _) = b.serve(4, |t| t.read_u8(5 * PAGE_BYTES + 3).unwrap()).unwrap();
    assert_eq!(vb, want(5, 3));
    assert_eq!(memfd.read_page(5).unwrap()[3], want(5, 3));
}

#[test]
fn huge_hot_set_prefetch_widens_to_whole_huge_pages() {
    let Some(memfd) = huge("huge_prefetch") else {
        return;
    };
    let Some(r) = region("huge_prefetch", &memfd) else {
        return;
    };
    // A hot set inside the second huge page only.
    let hot = HotSet::new(PAGES, &[600, 601, 900]).unwrap();
    let st = r.prefetch(&hot).unwrap();
    assert_eq!(st.prefetched, PAGES_PER_HUGE_PAGE as u64, "one whole huge page");
    let ((), s) = r
        .serve(8, |t| {
            assert_eq!(t.read_u8(600 * PAGE_BYTES), Some(want(600, 0)));
            assert_eq!(t.read_u8(1023 * PAGE_BYTES + 9), Some(want(1023, 9)));
            assert_eq!(t.read_u8(3 * PAGE_BYTES), Some(want(3, 0)));
        })
        .unwrap();
    // Only the first huge page (not prefetched) faulted.
    assert_eq!(s.faults, 1);
}
