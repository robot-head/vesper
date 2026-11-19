//! The package vesper-pact is the Vesper port; the seed copy that the
//! compiler links (vesper-pact-seed) does not build this test.

#[test]
fn the_package_is_the_vesper_port() {
    assert_eq!(vesper_pact::IMPLEMENTATION, "vesper");
}
