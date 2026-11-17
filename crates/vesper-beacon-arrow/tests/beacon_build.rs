//! vesper-beacon-arrow is outside the compiler's dependency closure, so it
//! links the port-backed `vesper-beacon` package, not the seed copy the
//! compiler links (seed-copy rule, docs/maintenance/porting-a-crate.md).
use vesper_beacon::FastLogHistogram;

#[test]
fn arrow_links_the_port_backed_beacon() {
    assert_eq!(vesper_beacon::IMPLEMENTATION, "vesper");
    // `bucket_index` is the function the stub port forwards to Vesper.
    assert_eq!(FastLogHistogram::bucket_index(15), 15);
    assert_eq!(FastLogHistogram::bucket_index(16), 16);
    assert_eq!(FastLogHistogram::bucket_index(u64::MAX), 495);
}
