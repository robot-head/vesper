//! Hot-set plans: coalescing into 2 MiB batches and the validated on-disk form.
use vesper_aether::hotset::*;

#[test]
fn runs_coalesce_and_split_at_the_batch_size() {
    let h = HotSet::new(4096, &[7, 3, 4, 5, 5, 100, 101, 4095]).unwrap();
    assert_eq!(h.pages(), &[3, 4, 5, 7, 100, 101, 4095]);
    assert_eq!(
        h.runs(512).unwrap(),
        vec![
            PageRun { start: 3, pages: 3 },
            PageRun { start: 7, pages: 1 },
            PageRun {
                start: 100,
                pages: 2
            },
            PageRun {
                start: 4095,
                pages: 1
            },
        ]
    );
    let big = HotSet::new(2000, &(0..1300).collect::<Vec<_>>()).unwrap();
    let runs = big.runs(PREFETCH_BATCH_PAGES).unwrap();
    assert_eq!(
        runs.iter().map(|r| r.pages).collect::<Vec<_>>(),
        vec![512, 512, 276]
    );
    assert_eq!(runs.iter().map(|r| r.pages).sum::<usize>(), 1300);
    assert!(runs.iter().all(|r| r.pages <= PREFETCH_BATCH_PAGES));
    assert_eq!(big.runs(0), Err(HotSetError::BadBatch));
    assert_eq!(
        HotSet::new(10, &[10]),
        Err(HotSetError::OutOfRange { index: 10 })
    );
}

#[test]
fn runs_cover_exactly_the_set_for_many_inputs() {
    // Deterministic pseudo-random sets: the runs are disjoint, ascending,
    // bounded, and their union is the set.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..200 {
        let n = 1 + (x % 3000) as usize;
        let mut idx = Vec::new();
        for _ in 0..(x % 900) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            idx.push((x % n as u64) as usize);
        }
        let h = HotSet::new(n, &idx).unwrap();
        let batch = 1 + (x % 600) as usize;
        let runs = h.runs(batch).unwrap();
        let mut covered = Vec::new();
        for r in &runs {
            assert!(r.pages >= 1 && r.pages <= batch);
            covered.extend(r.start..r.start + r.pages);
        }
        assert_eq!(covered, h.pages());
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
}

#[test]
fn fault_order_keeps_first_touches_up_to_the_limit() {
    let h = HotSet::from_fault_order(50, &[9, 2, 9, 30, 2, 1], 3).unwrap();
    assert_eq!(h.pages(), &[2, 9, 30]);
    assert_eq!(
        HotSet::from_fault_order(5, &[1, 7], 10),
        Err(HotSetError::OutOfRange { index: 7 })
    );
}

#[test]
fn encoded_form_is_validated() {
    let h = HotSet::new(100, &[1, 5, 99]).unwrap();
    let e = h.encode();
    assert_eq!(HotSet::decode(&e, 100).unwrap(), h);
    assert_eq!(HotSet::decode(&e[..10], 100), Err(HotSetError::Truncated));
    assert_eq!(
        HotSet::decode(&e[..e.len() - 1], 100),
        Err(HotSetError::Truncated)
    );
    let mut t = e.clone();
    t.push(0);
    assert_eq!(HotSet::decode(&t, 100), Err(HotSetError::TrailingBytes));
    let mut m = e.clone();
    m[0] = b'X';
    assert_eq!(HotSet::decode(&m, 100), Err(HotSetError::BadMagic));
    assert!(HotSet::decode(&e, 99).is_err(), "page count must match");
    let mut d = e.clone();
    d[20..24].copy_from_slice(&1u32.to_le_bytes()); // second index == first
    assert_eq!(HotSet::decode(&d, 100), Err(HotSetError::NotCanonical));
    let mut o = e.clone();
    o[24..28].copy_from_slice(&100u32.to_le_bytes());
    assert_eq!(
        HotSet::decode(&o, 100),
        Err(HotSetError::OutOfRange { index: 100 })
    );
    let mut c = e.clone();
    c[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(HotSet::decode(&c, 100), Err(HotSetError::TooMany));
    for n in 0..e.len() {
        let _ = HotSet::decode(&e[..n], 100);
    }
}
