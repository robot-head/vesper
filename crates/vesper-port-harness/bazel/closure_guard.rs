//! Bazel half of the seed-copy guard (see BUILD.bazel and
//! tests/closure_guard.rs): each file is the `genquery` result listing the
//! `vesper_port_object` targets in a dependency closure.
fn ports(var: &str) -> Vec<String> {
    let path = std::env::var(var).unwrap_or_else(|_| panic!("{var} is not set"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn compiler_and_verified_backend_link_no_port_object() {
    for var in ["PORTS_IN_VESPER", "PORTS_IN_CODEGEN_VERIFIED"] {
        assert_eq!(
            ports(var),
            Vec::<String>::new(),
            "{var}: a port object is in the closure; crates inside the compiler's closure \
             must depend on the seed package (docs/maintenance/porting-a-crate.md)"
        );
    }
}

#[test]
fn the_query_finds_a_port_outside_the_closure() {
    let mut found = ports("PORTS_IN_BEACON_ARROW");
    found.sort();
    assert_eq!(
        found,
        [
            "//crates/vesper-beacon-arrow:beacon_arrow_vesper",
            "//crates/vesper-beacon:beacon_vesper",
        ]
    );
}
