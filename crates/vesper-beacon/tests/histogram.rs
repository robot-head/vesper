//! The public-API half of the in-crate histogram unit tests
//! (src/histogram.rs), so it runs against both builds: the Rust seed and
//! the Vesper port (port wave P1 lane 4, #901).
use core::num::NonZeroU64;
use vesper_beacon::*;

#[test]
fn all_ranges_partition_u64_and_bound_error() {
    let mut next = 0_u128;
    for i in 0..HISTOGRAM_BINS {
        let r = FastLogHistogram::bin_range(i).unwrap();
        assert_eq!(u128::from(r.lower), next);
        assert!(r.lower <= r.upper);
        for value in [r.lower, r.lower + (r.upper - r.lower) / 2, r.upper] {
            assert_eq!(FastLogHistogram::bucket_index(value), i);
            if value != 0 {
                assert!(u128::from(value - r.estimate()) * 8 < u128::from(value));
            }
        }
        next = u128::from(r.upper) + 1;
    }
    assert_eq!(next, 1_u128 << 64);
    assert_eq!(FastLogHistogram::bin_range(HISTOGRAM_BINS), None);
    assert_eq!(FastLogHistogram::bin_range(usize::MAX), None);
    assert_eq!(FastLogHistogram::bucket_index(8), 8);
    assert_eq!(FastLogHistogram::bucket_index(16), 16);
    assert_eq!(FastLogHistogram::bucket_index(u64::MAX), 495);
}

#[test]
fn records_track_counts_exemplars_sum_min_and_max() {
    let mut h = FastLogHistogram::default();
    assert_eq!(
        (h.sum(), h.min(), h.max(), h.total_count()),
        (0, None, None, 0)
    );
    assert_eq!(h.count(HISTOGRAM_BINS), None);
    assert_eq!(h.exemplar(HISTOGRAM_BINS), None);
    let values = [0, 1, 15, 16, 17, 1_000, 1_000_000, u64::MAX, u64::MAX];
    for (n, v) in values.into_iter().enumerate() {
        let id = NonZeroU64::new(n as u64 + 1).unwrap();
        h.record_with_exemplar(v, id).unwrap();
        let i = FastLogHistogram::bucket_index(v);
        // The latest exemplar of a bin wins.
        assert_eq!(h.exemplar(i), Some(id));
    }
    assert_eq!(h.total_count(), values.len() as u64);
    assert_eq!(h.sum(), values.iter().map(|v| u128::from(*v)).sum::<u128>());
    assert_eq!((h.min(), h.max()), (Some(0), Some(u64::MAX)));
    assert_eq!(h.count(495), Some(2));
    assert_eq!(h.count(0), Some(1));
    assert_eq!(h.count(2), Some(0));
    assert_eq!(h.exemplar(2), None);
    let counted: u64 = (0..HISTOGRAM_BINS).map(|i| h.count(i).unwrap()).sum();
    assert_eq!(counted, h.total_count());
}

#[test]
fn footprint_is_fixed() {
    assert_eq!(
        FastLogHistogram::FOOTPRINT_BYTES,
        core::mem::size_of::<FastLogHistogram>()
    );
    // The Rust build's u128 sum is 16-byte aligned; the port keeps it as
    // two words.
    let expected = match vesper_beacon::IMPLEMENTATION {
        "vesper" => 7976,
        _ => 7984,
    };
    assert_eq!(FastLogHistogram::FOOTPRINT_BYTES, expected);
}
