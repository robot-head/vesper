//! Link the package vesper/app (and its path dependency vesper/math); see
//! docs/maintenance/porting-a-crate.md.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port =
        vesper_port_harness::Port::new("port_build", "vesper/app").binding("port/binding.rs");
    // The same edit as `:port_build_vesper_divergent` in BUILD.bazel.
    #[cfg(feature = "divergent")]
    let port = port.edit("2 * (w + h)", "2 * (w + h) + 1");
    port.build();
}
