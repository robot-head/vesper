//! Random operation sequences on the WIT table checked against a simple
//! reference model, plus targeted linear-owner tests.

use std::collections::HashMap;
use vesper_nexus::table::{
    TenantId, WitBorrow, WitHandle, WitOwn, WitResourceTable, WitTableError,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Reference: live handles with owner, payload and lend count; everything
/// else is invalid.
#[derive(Default)]
struct Model {
    live: HashMap<WitHandle, (TenantId, u64, u32)>,
    ever: Vec<WitHandle>,
}

const CAP: u32 = 16;
const TENANTS: u64 = 3;

fn expect_lookup(model: &Model, tenant: TenantId, h: WitHandle) -> Result<u64, WitTableError> {
    match model.live.get(&h) {
        None => Err(WitTableError::InvalidOrStaleHandle(h)),
        Some((owner, _, _)) if *owner != tenant => {
            Err(WitTableError::UnauthorizedTenant { actual: tenant })
        }
        Some((_, v, _)) => Ok(*v),
    }
}

fn candidate(rng: &mut Rng, model: &Model) -> WitHandle {
    match rng.below(4) {
        // A previously issued handle (live or stale).
        0 | 1 if !model.ever.is_empty() => model.ever[rng.below(model.ever.len() as u64) as usize],
        // A forged neighbour of an issued handle.
        2 if !model.ever.is_empty() => {
            let h = model.ever[rng.below(model.ever.len() as u64) as usize];
            WitHandle {
                idx: h.idx ^ (rng.below(3) as u32),
                generation: h.generation.wrapping_add(rng.below(3) as u32),
            }
        }
        _ => WitHandle::from_bits(rng.next()),
    }
}

#[test]
fn random_sequences_agree_with_the_reference_model() {
    let mut checked = 0u64;
    // Outcome coverage: full, drop ok, drop blocked, borrow ok, borrow
    // unauthorized, borrow stale/forged, transfer ok, transfer blocked.
    let mut seen = [0u64; 8];
    for seed in 1..=64u64 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed);
        let mut table = WitResourceTable::<u64>::with_capacity(CAP);
        let mut model = Model::default();
        let mut owners: Vec<WitOwn> = Vec::new();
        let mut borrows: Vec<WitBorrow> = Vec::new();
        for step in 0..2_000u64 {
            let tenant = rng.below(TENANTS);
            match rng.below(9) {
                0 | 1 => {
                    let r = table.insert_own(tenant, step);
                    if model.live.len() as u32 == CAP {
                        let (v, e) = r.unwrap_err();
                        assert_eq!((v, e), (step, WitTableError::TableFull { capacity: CAP }));
                        seen[0] += 1;
                    } else {
                        let own = r.unwrap();
                        let h = own.handle();
                        assert!(!model.live.contains_key(&h));
                        assert!(!model.ever.contains(&h), "handle reused: {h:?}");
                        model.live.insert(h, (tenant, step, 0));
                        model.ever.push(h);
                        owners.push(own);
                    }
                }
                2 => {
                    let h = candidate(&mut rng, &model);
                    assert_eq!(
                        table.lookup(tenant, h).copied(),
                        expect_lookup(&model, tenant, h)
                    );
                }
                3 if !owners.is_empty() => {
                    let own = owners.swap_remove(rng.below(owners.len() as u64) as usize);
                    let h = own.handle();
                    let (owner, v, lends) = model.live[&h];
                    assert_eq!(owner, own.tenant());
                    match table.drop_own(own) {
                        Ok(got) => {
                            assert_eq!((got, lends), (v, 0));
                            model.live.remove(&h);
                            seen[1] += 1;
                        }
                        Err((own, e)) => {
                            assert_eq!(
                                e,
                                WitTableError::ResourceHasActiveBorrows {
                                    idx: h.idx,
                                    active_borrows: lends
                                }
                            );
                            owners.push(own);
                            seen[2] += 1;
                        }
                    }
                }
                4 | 5 => {
                    // Borrow a held own (as its tenant or another) or a candidate.
                    let (t, h) = if !owners.is_empty() && rng.below(2) == 0 {
                        let o = &owners[rng.below(owners.len() as u64) as usize];
                        let t = if rng.below(4) == 0 {
                            tenant
                        } else {
                            o.tenant()
                        };
                        (t, o.handle())
                    } else {
                        (tenant, candidate(&mut rng, &model))
                    };
                    let want = expect_lookup(&model, t, h);
                    match table.begin_borrow(t, h) {
                        Ok(b) => {
                            assert!(want.is_ok());
                            assert_eq!(table.get(&b).copied(), want);
                            model.live.get_mut(&h).unwrap().2 += 1;
                            borrows.push(b);
                            seen[3] += 1;
                        }
                        Err(e) => {
                            seen[if matches!(e, WitTableError::UnauthorizedTenant { .. }) {
                                4
                            } else {
                                5
                            }] += 1;
                            assert_eq!(Err(e), want)
                        }
                    }
                }
                6 if !borrows.is_empty() => {
                    let b = borrows.swap_remove(rng.below(borrows.len() as u64) as usize);
                    let h = b.handle();
                    table.end_borrow(b).unwrap();
                    model.live.get_mut(&h).unwrap().2 -= 1;
                }
                7 if !owners.is_empty() => {
                    let at = rng.below(owners.len() as u64) as usize;
                    let own = owners.swap_remove(at);
                    let h = own.handle();
                    let to = rng.below(TENANTS);
                    let (_, v, lends) = model.live[&h];
                    match table.transfer(own, to) {
                        Ok(moved) => {
                            assert_eq!(lends, 0);
                            let nh = moved.handle();
                            assert_eq!(nh.idx, h.idx);
                            assert_ne!(nh.generation, h.generation);
                            model.live.remove(&h);
                            model.live.insert(nh, (to, v, 0));
                            model.ever.push(nh);
                            owners.push(moved);
                            seen[6] += 1;
                        }
                        Err((own, e)) => {
                            assert!(lends > 0, "{e:?}");
                            owners.push(own);
                            seen[7] += 1;
                        }
                    }
                }
                _ => {
                    // Raw end_borrow on a candidate without a matching lend.
                    let h = candidate(&mut rng, &model);
                    let r = table.end_borrow_raw(tenant, h);
                    match expect_lookup(&model, tenant, h) {
                        Err(e) => assert_eq!(r, Err(e)),
                        Ok(_) if model.live[&h].2 == 0 => {
                            assert_eq!(r, Err(WitTableError::NoActiveBorrow(h)))
                        }
                        Ok(_) => {
                            // A real lend is outstanding; restore it.
                            r.unwrap();
                            table.begin_borrow_raw(tenant, h).unwrap();
                        }
                    }
                }
            }
            // Global invariants against the model.
            assert_eq!(table.live_count(), model.live.len());
            for (h, (owner, _, lends)) in &model.live {
                let s = table.slot_state(h.idx).unwrap();
                assert_eq!(
                    (s.generation, s.owner_tenant, s.active_borrows, s.live),
                    (h.generation, *owner, *lends, true)
                );
            }
            checked += 1;
        }
        for b in borrows {
            table.end_borrow(b).unwrap();
        }
        for o in owners {
            table.drop_own(o).unwrap();
        }
        assert_eq!(table.live_count(), 0);
        assert_eq!(table.leaked_obligations(), 0);
    }
    assert_eq!(checked, 64 * 2_000);
    assert!(seen.iter().all(|n| *n > 100), "outcome coverage {seen:?}");
}

#[test]
fn dropped_owners_and_borrows_are_counted_leaks() {
    let mut t = WitResourceTable::with_capacity(4);
    let own = t.insert_own(1, "x").unwrap();
    let b = t.begin_borrow(1, own.handle()).unwrap();
    drop(b);
    assert_eq!(t.leaked_obligations(), 1);
    // The leaked borrow still blocks the drop: nothing is silently released.
    let (own, e) = t.drop_own(own).unwrap_err();
    assert!(matches!(e, WitTableError::ResourceHasActiveBorrows { .. }));
    drop(own);
    assert_eq!(t.leaked_obligations(), 2);
    assert_eq!(t.live_count(), 1);
}

#[test]
fn owners_from_another_table_are_refused() {
    let mut t1 = WitResourceTable::with_capacity(2);
    let mut t2 = WitResourceTable::with_capacity(2);
    let a = t1.insert_own(1, 10u8).unwrap();
    let b = t2.insert_own(1, 20u8).unwrap();
    // Same (idx, generation) in both tables; t2 must not accept t1's owner.
    assert_eq!(a.handle(), b.handle());
    let (a, e) = t2.drop_own(a).unwrap_err();
    assert_eq!(e, WitTableError::ForeignOwner);
    // A lend from t1 cannot read t2's payload at the same (idx, generation).
    let lend = t1.begin_borrow(1, a.handle()).unwrap();
    assert_eq!(t2.get(&lend), Err(WitTableError::ForeignOwner));
    assert_eq!(t1.get(&lend), Ok(&10));
    t1.end_borrow(lend).unwrap();
    assert_eq!(t1.drop_own(a).unwrap(), 10);
    assert_eq!(t2.drop_own(b).unwrap(), 20);
}

#[test]
fn teardown_reclaims_and_stales_host_owners() {
    let mut t = WitResourceTable::with_capacity(4);
    let a = t.insert_own(1, 1).unwrap();
    let b = t.insert_own(2, 2).unwrap();
    let lend = t.begin_borrow(1, a.handle()).unwrap();
    let mut got = Vec::new();
    assert_eq!(t.reclaim_tenant(1, |v| got.push(v)), 1);
    assert_eq!(got, vec![1]);
    assert_eq!(
        t.lookup(1, a.handle()),
        Err(WitTableError::InvalidOrStaleHandle(a.handle()))
    );
    t.forget_reclaimed(a).unwrap();
    t.forget_reclaimed_borrow(lend).unwrap();
    let (b, e) = t.forget_reclaimed(b).unwrap_err();
    assert_eq!(e, WitTableError::StillLive(b.handle()));
    t.drop_own(b).unwrap();
    assert_eq!(t.leaked_obligations(), 0);
}

#[test]
fn guest_lowering_moves_the_obligation_to_teardown() {
    let mut t = WitResourceTable::with_capacity(2);
    let own = t.insert_own(7, ()).unwrap();
    let h = t.lower_to_guest(own);
    assert_eq!(t.leaked_obligations(), 0);
    let back = t.lift_from_guest(7, h).unwrap();
    assert_eq!(
        t.lift_from_guest(8, h).unwrap_err(),
        WitTableError::UnauthorizedTenant { actual: 8 }
    );
    t.drop_own(back).unwrap();
    assert!(t.lift_from_guest(7, h).is_err());
}
