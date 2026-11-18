//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("semver", "vesper").binding("port/binding.rs");
    // One divergent edit per ported module (check.ves, version.ves), the same
    // edits as `:semver_vesper_divergent` in BUILD.bazel; port/test-port.sh
    // asserts that the tests catch each one.
    #[cfg(feature = "vesper-port-divergent")]
    let port = port
        .edit(
            "fn_result_mode(in w, b) > fn_result_mode(in w, a)",
            "fn_result_mode(in w, b) < fn_result_mode(in w, a)",
        )
        .edit(
            "cmp_precedence(in new, after, in old, before) != 1",
            "cmp_precedence(in new, after, in old, before) == -1",
        );
    port.build();
}
