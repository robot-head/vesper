//! SIMD == scalar property tests for kernels, lookup, batch update and idle
//! scan, over random tables with empty slots, rejected duplicates and
//! generation reuse.

mod common;

use common::{FixedHasher, Rng, supported_kernels};
use std::collections::HashMap;
use vesper_warp_shard::simd::scalar;
use vesper_warp_shard::{
    BatchError, ConnEvent, ConnEventDesc, ConnState, ConnTable, ConnectionId, InsertError, IsaPath,
    Kernels, LANES, MAX_BATCH, PN_LIMIT, SlotHandle, TableConfig,
};

const EDGE: [u64; 8] = [
    0,
    1,
    u32::MAX as u64,
    1 << 32,
    i64::MAX as u64,
    1 << 63,
    u64::MAX - 1,
    u64::MAX,
];

fn lanes_u64(rng: &mut Rng) -> [u64; LANES] {
    std::array::from_fn(|_| {
        if rng.chance(1, 3) {
            EDGE[rng.below(8) as usize]
        } else {
            rng.next() >> rng.below(64)
        }
    })
}

#[test]
fn kernels_match_scalar() {
    let mut rng = Rng(1);
    for k in supported_kernels() {
        for _ in 0..20_000 {
            let a = lanes_u64(&mut rng);
            let mut b = lanes_u64(&mut rng);
            // Force some equal lanes so the equal-high-dword path is hit.
            for l in 0..LANES {
                if rng.chance(1, 4) {
                    b[l] = a[l];
                } else if rng.chance(1, 4) {
                    b[l] = (a[l] & !0xffff_ffff) | (rng.next() & 0xffff_ffff);
                }
            }
            assert_eq!(k.gt_mask_u64(&a, &b), scalar::gt_mask_u64(&a, &b), "{k:?}");
            let bound = b[0];
            assert_eq!(
                k.le_mask_u64(&a, bound),
                !scalar::gt_mask_u64(&a, &[bound; LANES])
            );
            let (mut x, mut y) = (a, a);
            k.max_u64(&mut x, &b);
            scalar::max_u64(&mut y, &b);
            assert_eq!(x, y);
            let (mut x, mut y) = (a, a);
            k.add_u64(&mut x, &b);
            scalar::add_u64(&mut y, &b);
            assert_eq!(x, y);
            let (mut x, mut y) = (a, a);
            k.sub_u64(&mut x, &b);
            scalar::sub_u64(&mut y, &b);
            assert_eq!(x, y);
            let w: [u32; LANES] = std::array::from_fn(|i| {
                if rng.chance(1, 3) {
                    7
                } else {
                    (a[i] as u32) | (i as u32 & 1)
                }
            });
            for key in [7, w[3], u32::MAX, 0] {
                assert_eq!(k.eq_mask_u32(&w, key), scalar::eq_mask_u32(&w, key));
            }
        }
    }
}

#[test]
fn reports_isa_paths() {
    let detected = IsaPath::detect();
    let paths: Vec<_> = supported_kernels()
        .iter()
        .map(|k| k.path().name())
        .collect();
    eprintln!("warp-shard ISA: detected={detected} supported={paths:?}");
    assert!(detected.is_supported());
    assert!(Kernels::new(IsaPath::Scalar).is_ok());
    #[cfg(target_arch = "x86_64")]
    {
        assert!(IsaPath::Sse2.is_supported());
        assert!(!IsaPath::Neon.is_supported());
        assert!(Kernels::new(IsaPath::Neon).is_err());
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert!(IsaPath::Neon.is_supported());
        assert_eq!(detected, IsaPath::Neon);
        assert!(!IsaPath::Avx2.is_supported());
    }
}

fn cid(len: u8, rng: &mut Rng, small_space: bool) -> ConnectionId {
    let mut b = [0u8; 20];
    for x in b.iter_mut().take(usize::from(len)) {
        *x = if small_space {
            rng.below(3) as u8
        } else {
            rng.next() as u8
        };
    }
    ConnectionId::from_slice(&b[..usize::from(len)]).unwrap()
}

fn state(rng: &mut Rng) -> ConnState {
    let mut s = ConnState::new(
        ConnectionId::from_u64(rng.next()),
        rng.next() as u32,
        rng.below(1000),
    );
    s.bytes_in_flight = if rng.chance(1, 4) {
        u32::MAX - rng.below(5000) as u32
    } else {
        rng.below(100_000) as u32
    };
    s.largest_rx_pn = if rng.chance(1, 4) {
        None
    } else {
        Some(rng.below(PN_LIMIT))
    };
    s.next_tx_pn = rng.below(PN_LIMIT);
    s
}

/// A random table plus a model of what it should contain. Uses a small CID
/// alphabet for some lengths so duplicate inserts actually happen.
fn churn(
    k: IsaPath,
    cid_len: u8,
    capacity: u32,
    seed: u64,
) -> (
    ConnTable<FixedHasher>,
    HashMap<ConnectionId, SlotHandle>,
    Vec<SlotHandle>,
) {
    let mut t = ConnTable::with_hasher(
        TableConfig::new(capacity, cid_len).with_isa(k),
        FixedHasher::default(),
    )
    .unwrap();
    let mut rng = Rng(seed);
    let mut model: HashMap<ConnectionId, SlotHandle> = HashMap::new();
    let mut stale = Vec::new();
    let mut order: Vec<ConnectionId> = Vec::new();
    let small = cid_len <= 2;
    for _ in 0..capacity as usize * 3 {
        if rng.chance(2, 3) {
            let c = cid(cid_len, &mut rng, small);
            let r = t.insert(&c, &state(&mut rng));
            match (model.contains_key(&c), r) {
                (true, Err(InsertError::DuplicateCid)) => {}
                (false, Ok(h)) => {
                    model.insert(c, h);
                    order.push(c);
                }
                (false, Err(InsertError::Full)) => assert_eq!(model.len() as u32, capacity),
                other => panic!("unexpected insert outcome {other:?}"),
            }
        } else if !model.is_empty() {
            let key = order.swap_remove(rng.below(order.len() as u64) as usize);
            let h = model.remove(&key).unwrap();
            let (got, _) = t.remove(h).unwrap();
            assert_eq!(got, key);
            assert!(t.remove(h).is_err());
            stale.push(h);
        }
    }
    assert_eq!(t.len() as usize, model.len());
    (t, model, stale)
}

#[test]
fn lookup_index_scan_and_scalar_agree() {
    for k in supported_kernels() {
        for (cid_len, capacity, seed) in [
            (8, 1000, 11),
            (1, 40, 12),
            (2, 300, 13),
            (20, 500, 14),
            (5, 64, 15),
        ] {
            let (t, model, stale) = churn(k.path(), cid_len, capacity, seed);
            for (c, &h) in &model {
                let b = c.as_bytes();
                assert_eq!(t.lookup(b).unwrap(), Some(h));
                assert_eq!(t.lookup_cid_simd(b).unwrap(), Some(h));
                assert_eq!(t.lookup_cid_scalar(b).unwrap(), Some(h));
                assert_eq!(t.local_cid(h).unwrap(), *c);
            }
            let mut rng = Rng(seed ^ 0xabc);
            for _ in 0..2000 {
                let c = cid(cid_len, &mut rng, cid_len <= 2);
                let b = c.as_bytes();
                let want = model.get(&c).copied();
                assert_eq!(t.lookup(b).unwrap(), want);
                assert_eq!(t.lookup_scan_with(k, b).unwrap(), want);
                assert_eq!(t.lookup_cid_scalar(b).unwrap(), want);
            }
            // Removed handles stay stale even when their slot was reused.
            for h in stale {
                assert!(t.get(h).is_err() || model.values().any(|&m| m == h));
            }
            let wrong = vec![0u8; usize::from(cid_len) + 1];
            assert!(t.lookup(&wrong).is_err());
            assert!(t.lookup_cid_simd(&wrong).is_err());
        }
    }
}

#[test]
fn generation_reuse_invalidates_old_handles() {
    let mut t = ConnTable::with_hasher(TableConfig::new(1, 8), FixedHasher::default()).unwrap();
    let s = ConnState::new(ConnectionId::EMPTY, 10, 0);
    let a = t.insert(&ConnectionId::from_u64(1), &s).unwrap();
    t.remove(a).unwrap();
    let b = t.insert(&ConnectionId::from_u64(2), &s).unwrap();
    assert_eq!(a.index(), b.index());
    assert_ne!(a.generation(), b.generation());
    assert!(t.get(a).is_err());
    assert_eq!(
        t.apply_batch(&[ConnEventDesc {
            slot: a,
            event: ConnEvent::PacketSent { bytes: 1 }
        }]),
        Err(BatchError::StaleHandle { index: 0 })
    );
    assert_eq!(t.lookup(&1u64.to_be_bytes()).unwrap(), None);
    assert_eq!(t.lookup(&2u64.to_be_bytes()).unwrap(), Some(b));
    assert_eq!(
        t.insert(&ConnectionId::from_u64(3), &s),
        Err(InsertError::Full)
    );
    assert_eq!(
        t.insert(&ConnectionId::from_u64(2), &s),
        Err(InsertError::DuplicateCid)
    );
    assert!(t.get(SlotHandle::from_raw(99, 0)).is_err());
}

fn random_batch(rng: &mut Rng, live: &[SlotHandle], stale: &[SlotHandle]) -> Vec<ConnEventDesc> {
    let n = rng.below(MAX_BATCH as u64 + 1) as usize;
    // Concentrate on a few slots so aggregation and shared tiles are exercised.
    let hot: Vec<SlotHandle> = (0..1 + rng.below(6))
        .map(|_| live[rng.below(live.len() as u64) as usize])
        .collect();
    (0..n)
        .map(|_| {
            let slot = if !stale.is_empty() && rng.chance(1, 400) {
                stale[rng.below(stale.len() as u64) as usize]
            } else if rng.chance(3, 4) {
                hot[rng.below(hot.len() as u64) as usize]
            } else {
                live[rng.below(live.len() as u64) as usize]
            };
            let big = rng.chance(1, 40);
            let bytes = if big {
                u32::MAX - rng.below(10) as u32
            } else {
                rng.below(1500) as u32
            };
            let event = match rng.below(5) {
                0 => ConnEvent::PacketReceived {
                    pn: if rng.chance(1, 300) {
                        PN_LIMIT + rng.below(5)
                    } else {
                        rng.below(PN_LIMIT)
                    },
                },
                1 => ConnEvent::PacketSent { bytes },
                2 => ConnEvent::BytesAcked { bytes },
                3 => ConnEvent::BytesLost { bytes },
                _ => ConnEvent::CwndUpdate {
                    cwnd_bytes: rng.next() as u32,
                },
            };
            ConnEventDesc { slot, event }
        })
        .collect()
}

fn snapshot(t: &ConnTable<FixedHasher>, live: &[SlotHandle]) -> Vec<ConnState> {
    live.iter().map(|&h| t.get(h).unwrap()).collect()
}

#[test]
fn batch_simd_matches_scalar() {
    for k in supported_kernels() {
        let (mut a, model, stale) = churn(k.path(), 8, 400, 21);
        let (mut b, model_b, _) = churn(IsaPath::Scalar, 8, 400, 21);
        let live: Vec<SlotHandle> = model.values().copied().collect();
        assert_eq!(model, model_b);
        let mut rng = Rng(99);
        let (mut ok, mut errs) = (0, 0);
        for _ in 0..4000 {
            let batch = random_batch(&mut rng, &live, &stale);
            let before = snapshot(&a, &live);
            let ra = a.apply_batch(&batch);
            let rb = b.apply_batch_scalar(&batch);
            assert_eq!(ra, rb, "{k:?} {batch:?}");
            if ra.is_err() {
                errs += 1;
                assert_eq!(snapshot(&a, &live), before, "failed batch mutated table");
            } else {
                ok += 1;
            }
            assert_eq!(snapshot(&a, &live), snapshot(&b, &live));
        }
        assert!(ok > 500 && errs > 200, "ok {ok} errs {errs}");
        let too_long = vec![
            ConnEventDesc {
                slot: live[0],
                event: ConnEvent::PacketSent { bytes: 0 }
            };
            MAX_BATCH + 1
        ];
        assert_eq!(
            a.apply_batch(&too_long),
            Err(BatchError::TooLong { len: 65 })
        );
        assert_eq!(
            b.apply_batch_scalar(&too_long),
            Err(BatchError::TooLong { len: 65 })
        );
    }
}

#[test]
fn batch_semantics_examples() {
    let mut t = ConnTable::with_hasher(TableConfig::new(32, 8), FixedHasher::default()).unwrap();
    let mut s = ConnState::new(ConnectionId::EMPTY, 1000, 0);
    s.bytes_in_flight = 100;
    let h = t.insert(&ConnectionId::from_u64(1), &s).unwrap();
    let g = t.insert(&ConnectionId::from_u64(2), &s).unwrap();
    let d = |slot, event| ConnEventDesc { slot, event };
    // Net semantics: ack before send within one batch is fine.
    t.apply_batch(&[
        d(h, ConnEvent::BytesAcked { bytes: 150 }),
        d(h, ConnEvent::PacketSent { bytes: 100 }),
        d(h, ConnEvent::PacketReceived { pn: 9 }),
        d(h, ConnEvent::PacketReceived { pn: 4 }),
        d(h, ConnEvent::CwndUpdate { cwnd_bytes: 5 }),
        d(h, ConnEvent::CwndUpdate { cwnd_bytes: 7 }),
    ])
    .unwrap();
    let st = t.get(h).unwrap();
    assert_eq!(
        (st.bytes_in_flight, st.largest_rx_pn, st.cwnd_bytes),
        (50, Some(9), 7)
    );
    // Underflow on g is reported at g's first descriptor even though h's
    // overflow descriptor comes later.
    let r = t.apply_batch(&[
        d(h, ConnEvent::PacketReceived { pn: 1 }),
        d(g, ConnEvent::BytesLost { bytes: 101 }),
        d(h, ConnEvent::PacketSent { bytes: u32::MAX }),
    ]);
    assert_eq!(r, Err(BatchError::BytesInFlightOverflow { index: 0 }));
    let r = t.apply_batch(&[
        d(g, ConnEvent::BytesLost { bytes: 101 }),
        d(h, ConnEvent::PacketSent { bytes: u32::MAX }),
    ]);
    assert_eq!(r, Err(BatchError::BytesInFlightUnderflow { index: 0 }));
    assert_eq!(
        t.apply_batch(&[d(h, ConnEvent::PacketReceived { pn: PN_LIMIT })]),
        Err(BatchError::PacketNumberRange {
            index: 0,
            pn: PN_LIMIT
        })
    );
    assert_eq!(t.get(h).unwrap(), st);
    t.apply_batch(&[]).unwrap();
}

#[test]
fn idle_scan_simd_matches_scalar_with_resumption() {
    for k in supported_kernels() {
        let (t, model, _) = churn(k.path(), 8, 700, 31);
        let mut rng = Rng(5);
        for _ in 0..200 {
            let now = rng.below(1100);
            let mut want: Vec<SlotHandle> = model
                .values()
                .copied()
                .filter(|&h| t.get(h).unwrap().idle_deadline_ns <= now)
                .collect();
            want.sort_by_key(|h| h.index());
            for chunk in [1usize, 7, 16, 64, 1000] {
                let collect = |kern: Kernels| {
                    let mut got = Vec::new();
                    let mut buf = vec![SlotHandle::from_raw(0, 0); chunk];
                    let mut at = 0;
                    loop {
                        let r = t.scan_idle_with(kern, now, at, &mut buf);
                        got.extend_from_slice(&buf[..r.written]);
                        match r.resume_at {
                            Some(next) => {
                                assert!(r.written == chunk);
                                at = next;
                            }
                            None => break got,
                        }
                    }
                };
                assert_eq!(collect(k), want);
                assert_eq!(collect(Kernels::scalar()), want);
            }
        }
        let mut empty: [SlotHandle; 0] = [];
        let r = t.scan_idle(u64::MAX, 0, &mut empty);
        assert_eq!(r.written, 0);
        assert!(r.resume_at.is_some());
        assert_eq!(
            t.scan_idle(u64::MAX, u32::MAX, &mut [SlotHandle::from_raw(0, 0)])
                .written,
            0
        );
    }
}

#[test]
fn compaction_preserves_connections_and_relocates_handles() {
    for k in supported_kernels() {
        let (mut t, mut model, _) = churn(k.path(), 8, 500, 41);
        // Punch holes so compaction has work to do and the table is not full.
        let victims: Vec<ConnectionId> = model
            .keys()
            .copied()
            .filter(|c| c.as_bytes()[0] % 3 == 0)
            .collect();
        for c in victims {
            t.remove(model.remove(&c).unwrap()).unwrap();
        }
        assert!(t.len() < t.capacity());
        let before: HashMap<ConnectionId, ConnState> = model
            .iter()
            .map(|(c, &h)| (*c, t.get(h).unwrap()))
            .collect();
        let mut moves = HashMap::new();
        let n = t.compact(|r| {
            moves.insert(r.from, r.to);
        });
        assert_eq!(n, moves.len());
        for (c, h) in model.iter_mut() {
            if let Some(&to) = moves.get(h) {
                assert!(t.get(*h).is_err());
                *h = to;
            }
            assert!(h.index() < t.len());
            assert_eq!(t.get(*h).unwrap(), before[c]);
            assert_eq!(t.lookup(c.as_bytes()).unwrap(), Some(*h));
            assert_eq!(t.lookup_cid_simd(c.as_bytes()).unwrap(), Some(*h));
        }
        // Inserting after compaction fills the lowest free slot.
        let len = t.len();
        let h = t
            .insert(
                &ConnectionId::from_u64(u64::MAX),
                &ConnState::new(ConnectionId::EMPTY, 1, 0),
            )
            .unwrap();
        assert_eq!(h.index(), len);
        assert_eq!(t.compact(|_| {}), 0);
    }
}

#[test]
fn rejects_bad_configs_and_inputs() {
    use vesper_warp_shard::{ConfigError, MAX_CAPACITY, SetError};
    assert_eq!(
        ConnTable::new(TableConfig::new(0, 8)).err(),
        Some(ConfigError::ZeroCapacity)
    );
    assert_eq!(
        ConnTable::new(TableConfig::new(MAX_CAPACITY + 1, 8)).err(),
        Some(ConfigError::CapacityTooLarge {
            requested: MAX_CAPACITY + 1
        })
    );
    assert_eq!(
        ConnTable::new(TableConfig::new(16, 0)).err(),
        Some(ConfigError::CidLength { len: 0 })
    );
    assert_eq!(
        ConnTable::new(TableConfig::new(16, 21)).err(),
        Some(ConfigError::CidLength { len: 21 })
    );
    let unsupported = IsaPath::ALL
        .into_iter()
        .find(|p| !p.is_supported())
        .unwrap();
    assert!(matches!(
        ConnTable::new(TableConfig::new(16, 8).with_isa(unsupported)),
        Err(ConfigError::Isa(_))
    ));
    assert!(ConnectionId::from_slice(&[0; 21]).is_err());
    let mut t = ConnTable::new(TableConfig::new(16, 8)).unwrap();
    let mut s = ConnState::new(ConnectionId::EMPTY, 1, 0);
    assert_eq!(
        t.insert(&ConnectionId::from_slice(&[1; 4]).unwrap(), &s),
        Err(InsertError::CidLength {
            expected: 8,
            got: 4
        })
    );
    s.largest_rx_pn = Some(PN_LIMIT);
    assert_eq!(
        t.insert(&ConnectionId::from_u64(1), &s),
        Err(InsertError::PacketNumberRange { pn: PN_LIMIT })
    );
    s.largest_rx_pn = None;
    let h = t.insert(&ConnectionId::from_u64(1), &s).unwrap();
    s.next_tx_pn = PN_LIMIT + 1;
    assert_eq!(
        t.set(h, &s),
        Err(SetError::PacketNumberRange { pn: PN_LIMIT + 1 })
    );
    s.next_tx_pn = PN_LIMIT;
    t.set(h, &s).unwrap();
    assert_eq!(t.get(h).unwrap().next_tx_pn, PN_LIMIT);
}
