//! Exercise inherited test variables through the real Bazel launcher.
#[test]
fn scoped_environment_reaches_test_process() {
    println!(
        "probe={}",
        std::env::var("VESPER_BAZEL_ENV_PROBE").expect("probe default or inherited value")
    );
    assert!(std::env::var_os("VESPER_BAZEL_UNRELATED_PROBE").is_none());
}
