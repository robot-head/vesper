//! Which build the tests run against: the Vesper port under `vesper-port`,
//! the Rust code otherwise (and in the seed copy).

#[test]
fn the_feature_selects_the_build() {
    let expected = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    assert_eq!(vesper_pact::IMPLEMENTATION, expected);
}
