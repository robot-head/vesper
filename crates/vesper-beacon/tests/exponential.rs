use vesper_beacon::*;

/// Exact check at small scales: at scale `s`, `v` is in bucket `i` iff
/// `2^i < v^(2^s) <= 2^(i+1)`.
fn exact_index(v: u64, s: u32) -> i32 {
    let p = (0..s).fold(u128::from(v), |x, _| x * x);
    // ceil(log2 p) - 1
    (128 - (p - 1).leading_zeros()) as i32 - 1
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn bucket_index_is_exact_at_small_scales() {
    for v in 1..=65_535u64 {
        for s in 1..=3 {
            assert_eq!(
                exp_bucket_index(v, s as i32),
                exact_index(v, s),
                "v={v} s={s}"
            );
        }
        assert_eq!(exp_bucket_index(v, 0), exact_index(v, 0));
    }
}

#[test]
fn bucket_index_matches_floating_point_bounds_at_every_scale() {
    let mut st = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..20_000 {
        let v = xorshift(&mut st) >> (xorshift(&mut st) % 64);
        let v = v.max(1);
        for s in EXP_HISTOGRAM_MIN_SCALE..=EXP_HISTOGRAM_MAX_SCALE {
            let i = exp_bucket_index(v, s);
            let f = (v as f64).log2() * 2f64.powi(s);
            // Bucket i is (i, i + 1] in scaled log space, up to f64 error.
            let tol = 1e-9 * f.abs().max(1.0);
            assert!(
                f > f64::from(i) - tol && f <= f64::from(i + 1) + tol,
                "v={v} s={s} i={i} f={f}"
            );
        }
    }
    for s in EXP_HISTOGRAM_MIN_SCALE..=EXP_HISTOGRAM_MAX_SCALE {
        // 1 = 2^0 is the upper bound of bucket -1 (or 0 once coarse).
        assert_eq!(exp_bucket_index(1, s), if s >= 0 { -1 } else { -1 >> -s });
        for k in 1..64 {
            // Powers of two are the upper bound of their bucket.
            let want = if s >= 0 { (k << s) - 1 } else { (k - 1) >> -s };
            assert_eq!(exp_bucket_index(1u64 << k, s), want, "k={k} s={s}");
        }
    }
}

#[test]
fn starts_at_the_finest_scale_and_downscales_to_fit() {
    let mut h = ExponentialHistogram::default();
    assert_eq!(h.scale(), EXP_HISTOGRAM_MAX_SCALE);
    assert_eq!(h.min(), None);
    h.record(0).unwrap();
    h.record(1000).unwrap();
    assert_eq!(h.scale(), EXP_HISTOGRAM_MAX_SCALE);
    assert_eq!(h.zero_count(), 1);
    assert_eq!(h.offset(), exp_bucket_index(1000, EXP_HISTOGRAM_MAX_SCALE));
    assert_eq!(h.positive_counts(), &[1]);
    h.record(1).unwrap();
    h.record(u64::MAX).unwrap();
    assert!(h.scale() < EXP_HISTOGRAM_MAX_SCALE);
    let s = h.scale();
    let c = h.positive_counts();
    assert!(c.len() <= EXP_HISTOGRAM_BUCKETS);
    assert_eq!(c.iter().sum::<u64>(), 3);
    for v in [1, 1000, u64::MAX] {
        let i = exp_bucket_index(v, s) - h.offset();
        assert!(c[i as usize] >= 1, "v={v} not in its bucket");
    }
    // The window is the narrowest that fits: one scale finer would not.
    let span = |s: i32| exp_bucket_index(u64::MAX, s) - exp_bucket_index(1, s) + 1;
    assert!(span(s) as usize <= EXP_HISTOGRAM_BUCKETS);
    assert!(span(s + 1) as usize > EXP_HISTOGRAM_BUCKETS);
    assert_eq!(h.total_count(), 4);
    assert_eq!(h.sum(), 1001 + u128::from(u64::MAX));
    assert_eq!((h.min(), h.max()), (Some(0), Some(u64::MAX)));
}

#[test]
fn random_observations_keep_every_count_within_the_bound() {
    let mut st = 7;
    let mut h = ExponentialHistogram::default();
    let mut zero = 0;
    for n in 1..=5_000u64 {
        let v = xorshift(&mut st) >> (xorshift(&mut st) % 64);
        if v == 0 {
            zero += 1;
        }
        let before = h.scale();
        h.record(v).unwrap();
        assert!(h.scale() <= before);
        assert!(h.scale() >= EXP_HISTOGRAM_MIN_SCALE);
        assert!(h.positive_counts().len() <= EXP_HISTOGRAM_BUCKETS);
        assert_eq!(h.positive_counts().iter().sum::<u64>() + h.zero_count(), n);
        if v != 0 {
            let i = exp_bucket_index(v, h.scale()) - h.offset();
            assert!(i >= 0 && (i as usize) < h.positive_counts().len());
        }
    }
    assert_eq!(h.zero_count(), zero);
    assert_ne!(*h.positive_counts().first().unwrap(), 0);
    assert_ne!(*h.positive_counts().last().unwrap(), 0);
}

#[test]
fn footprint_is_fixed() {
    assert_eq!(
        ExponentialHistogram::FOOTPRINT_BYTES,
        core::mem::size_of::<ExponentialHistogram>()
    );
    assert!(core::mem::size_of::<ExponentialHistogram>() < 1500);
}
