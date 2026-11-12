//! Page-acceptance tracker: typed errors for double, overlapping and
//! unaligned accepts and unaccepted reads; property test against a
//! reference bitmap; linear-owner leak accounting.

mod support;

use support::SplitMix;
use vesper_aegis::gpa::{PAGE_4K, SIM_PAGE_ALREADY_ACCEPTED, SimulatedGuestMemory};
use vesper_aegis::*;

const P: u64 = PAGE_4K;

#[test]
fn announce_accept_split_merge_and_errors() {
    let mut t = GpaAcceptanceTracker::new();
    let mut mem = SimulatedGuestMemory::default();
    mem.host_write(0x10_000, 0xAB);
    // Unaligned, empty, overflowing ranges.
    assert!(matches!(
        t.announce(0x10_001, 1),
        Err(TeeError::UnalignedOrZeroGpaRange { .. })
    ));
    assert!(matches!(
        t.announce(0x10_000, 0),
        Err(TeeError::UnalignedOrZeroGpaRange { .. })
    ));
    assert!(matches!(
        t.announce(!0xFFF, 2),
        Err(TeeError::UnalignedOrZeroGpaRange { .. })
    ));

    let r = t.announce(0x10_000, 8).unwrap();
    // Read before acceptance.
    assert_eq!(
        mem.guest_read(&t, 0x10_000),
        Err(TeeError::UnacceptedPageAccess {
            gpa_page_4k: 0x10_000
        })
    );
    // Overlapping announcement while pending.
    assert_eq!(
        t.announce(0x10_000 + 7 * P, 4).unwrap_err(),
        TeeError::OverlappingGpaRange {
            gpa_page_4k: 0x10_000 + 7 * P
        }
    );
    // Split at a bad point returns the owner.
    let (r, e) = r.split(8).unwrap_err();
    assert_eq!(
        e,
        TeeError::BadSplit {
            at: 8,
            page_count_4k: 8
        }
    );
    let (a, b) = r.split(3).unwrap();
    assert_eq!((a.gpa_base(), a.page_count_4k()), (0x10_000, 3));
    assert_eq!((b.gpa_base(), b.page_count_4k()), (0x10_000 + 3 * P, 5));
    let r = b.merge(a).unwrap();
    assert_eq!((r.gpa_base(), r.page_count_4k()), (0x10_000, 8));
    let (a, b) = r.split(4).unwrap();
    let slab_b = t.accept(b, &mut mem).unwrap();
    let slab_a = t.accept(a, &mut mem).unwrap();
    // Adjacent accepts merge into one interval; slabs merge too.
    assert_eq!(t.accepted_intervals(), 1);
    let slab = slab_a.merge(slab_b).unwrap();
    assert_eq!((slab.gpa_base, slab.page_count_4k), (0x10_000, 8));
    // Accepted pages are zeroed (stale host byte gone) and readable.
    assert_eq!(mem.guest_read(&t, 0x10_000), Ok(0));
    t.check_access(0x10_000, 8 * P).unwrap();
    assert_eq!(
        t.check_access(0x10_000, 8 * P + 1),
        Err(TeeError::UnacceptedPageAccess {
            gpa_page_4k: 0x10_000 + 8 * P
        })
    );
    // Host re-announces accepted memory: double accept.
    assert_eq!(
        t.announce(0x10_000 + P, 2).unwrap_err(),
        TeeError::HypervisorGpaDoubleAccept {
            gpa_page_4k: 0x10_000 + P
        }
    );
    // Partial overlap with accepted memory.
    assert_eq!(
        t.announce(0x10_000 + 7 * P, 2).unwrap_err(),
        TeeError::OverlappingGpaRange {
            gpa_page_4k: 0x10_000 + 7 * P
        }
    );
    // The simulated hardware also refuses a second accept.
    use vesper_aegis::gpa::PageAcceptor;
    assert_eq!(mem.accept_4k(0x10_000), Err(SIM_PAGE_ALREADY_ACCEPTED));
    // Non-adjacent merge returns both owners.
    let x = t.announce(0x100_000, 1).unwrap();
    let y = t.announce(0x200_000, 1).unwrap();
    let (x, y, e) = x.merge(y).unwrap_err();
    assert_eq!(e, TeeError::NotAdjacent);
    t.relinquish(x).unwrap();
    t.relinquish(y).unwrap();
    assert_eq!(t.leaked_ranges(), 0);
}

#[test]
fn foreign_owner_and_backend_failure() {
    let mut t1 = GpaAcceptanceTracker::new();
    let mut t2 = GpaAcceptanceTracker::new();
    let mut mem = SimulatedGuestMemory::default();
    let r = t1.announce(0, 4).unwrap();
    let (r, e) = t2.accept(r, &mut mem).unwrap_err();
    assert_eq!(e, TeeError::ForeignOwner);
    // Backend fails at page 2: pages 0-1 stay accepted, the rest returned.
    mem.fail_at = Some((2 * P, 0x77));
    let (rest, e) = t1.accept(r, &mut mem).unwrap_err();
    assert_eq!(
        e,
        TeeError::AcceptBackend {
            gpa_page_4k: 2 * P,
            code: 0x77
        }
    );
    assert_eq!((rest.gpa_base(), rest.page_count_4k()), (2 * P, 2));
    assert!(t1.is_accepted(0) && t1.is_accepted(P) && !t1.is_accepted(2 * P));
    assert!(t1.is_pending(2 * P));
    t1.accept(rest, &mut mem).unwrap();
    assert_eq!(t1.accepted_pages(), 4);
}

#[test]
fn dropped_owner_is_counted_as_leaked() {
    let mut t = GpaAcceptanceTracker::new();
    {
        let _lost = t.announce(0, 1).unwrap();
    }
    let r = t.announce(P, 2).unwrap();
    let (a, b) = r.split(1).unwrap();
    drop(a);
    t.relinquish(b).unwrap();
    assert_eq!(t.leaked_ranges(), 2);
}

#[test]
fn design_raw_api_rejects_double_accept() {
    let mut t = GpaAcceptanceTracker::new();
    t.accept_gpa_range(0x2000, 2).unwrap();
    assert_eq!(
        t.accept_gpa_range(0x3000, 1),
        Err(AegisVerifyError::HypervisorGpaDoubleAccept {
            gpa_page_4k: 0x3000
        })
    );
    assert_eq!(
        t.accept_gpa_range(0x2001, 1),
        Err(AegisVerifyError::UnalignedOrZeroGpaRange)
    );
    assert_eq!(
        t.accept_gpa_range(0x2000, 0),
        Err(AegisVerifyError::UnalignedOrZeroGpaRange)
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Free,
    Pending,
    Accepted,
}

/// Random announce/split/merge/accept/relinquish/read sequences over 64
/// pages, compared operation by operation with a reference bitmap.
#[test]
fn tracker_matches_reference_bitmap() {
    const N: u64 = 64;
    for seed in 0..200 {
        let mut rng = SplitMix(seed);
        let mut t = GpaAcceptanceTracker::new();
        let mut mem = SimulatedGuestMemory::default();
        let mut model = [Page::Free; N as usize];
        let mut owners: Vec<UnacceptedGpaRange> = Vec::new();
        for _ in 0..120 {
            match rng.below(6) {
                0 | 1 => {
                    let s = rng.below(N);
                    let n = 1 + rng.below(8) as u32;
                    let e = (s + u64::from(n)).min(N + 8);
                    let res = t.announce(s * P, n);
                    let pages: Vec<Page> = (s..e)
                        .map(|p| model.get(p as usize).copied().unwrap_or(Page::Free))
                        .collect();
                    let first_busy =
                        (s..e).find(|&p| model.get(p as usize).is_some_and(|&x| x != Page::Free));
                    match res {
                        Ok(r) => {
                            assert!(first_busy.is_none(), "seed {seed}: accepted busy range");
                            for p in s..e {
                                if let Some(x) = model.get_mut(p as usize) {
                                    *x = Page::Pending;
                                }
                            }
                            if e > N {
                                // Pages past N are tracked by the tracker only.
                                t.relinquish(r).unwrap();
                                for p in s..e.min(N) {
                                    model[p as usize] = Page::Free;
                                }
                            } else {
                                owners.push(r);
                            }
                        }
                        Err(TeeError::HypervisorGpaDoubleAccept { gpa_page_4k }) => {
                            assert!(pages.iter().all(|&x| x == Page::Accepted), "seed {seed}");
                            assert_eq!(gpa_page_4k, s * P);
                        }
                        Err(TeeError::OverlappingGpaRange { gpa_page_4k }) => {
                            assert_eq!(Some(gpa_page_4k / P), first_busy, "seed {seed}");
                            assert!(!pages.iter().all(|&x| x == Page::Accepted));
                        }
                        Err(e) => panic!("seed {seed}: unexpected {e:?}"),
                    }
                }
                2 if !owners.is_empty() => {
                    let i = rng.below(owners.len() as u64) as usize;
                    let r = owners.swap_remove(i);
                    let (s, n) = (r.gpa_base() / P, u64::from(r.page_count_4k()));
                    t.accept(r, &mut mem).unwrap();
                    for p in s..s + n {
                        model[p as usize] = Page::Accepted;
                    }
                }
                3 if !owners.is_empty() => {
                    let i = rng.below(owners.len() as u64) as usize;
                    let r = owners.swap_remove(i);
                    let at = rng.below(u64::from(r.page_count_4k()) + 1) as u32;
                    match r.split(at) {
                        Ok((a, b)) => {
                            owners.push(a);
                            owners.push(b);
                        }
                        Err((r, _)) => owners.push(r),
                    }
                }
                4 if owners.len() >= 2 => {
                    let a = owners.swap_remove(0);
                    let b = owners.swap_remove(0);
                    let adjacent = a.gpa_base() + u64::from(a.page_count_4k()) * P == b.gpa_base()
                        || b.gpa_base() + u64::from(b.page_count_4k()) * P == a.gpa_base();
                    match a.merge(b) {
                        Ok(m) => {
                            assert!(adjacent);
                            owners.push(m);
                        }
                        Err((a, b, _)) => {
                            assert!(!adjacent);
                            owners.push(a);
                            owners.push(b);
                        }
                    }
                }
                5 if !owners.is_empty() && rng.below(2) == 0 => {
                    let r = owners.swap_remove(0);
                    let (s, n) = (r.gpa_base() / P, u64::from(r.page_count_4k()));
                    t.relinquish(r).unwrap();
                    for p in s..s + n {
                        model[p as usize] = Page::Free;
                    }
                }
                _ => {
                    let g = rng.below(N) * P + rng.below(P);
                    let len = 1 + rng.below(3 * P);
                    let expect = (g / P..=(g + len - 1) / P).find(|&p| {
                        model.get(p as usize).copied().unwrap_or(Page::Free) != Page::Accepted
                    });
                    match t.check_access(g, len) {
                        Ok(()) => assert!(expect.is_none(), "seed {seed}"),
                        Err(TeeError::UnacceptedPageAccess { gpa_page_4k }) => {
                            assert_eq!(Some(gpa_page_4k / P), expect, "seed {seed}")
                        }
                        Err(e) => panic!("{e:?}"),
                    }
                }
            }
            for p in 0..N {
                let want = model[p as usize];
                assert_eq!(
                    t.is_accepted(p * P),
                    want == Page::Accepted,
                    "seed {seed} page {p}"
                );
                assert_eq!(
                    t.is_pending(p * P),
                    want == Page::Pending,
                    "seed {seed} page {p}"
                );
            }
        }
        for r in owners {
            t.relinquish(r).unwrap();
        }
        assert_eq!(t.leaked_ranges(), 0);
    }
}
