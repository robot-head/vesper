//! Under `vesper-port`, compile the sample driver package in vesper/ into
//! a static library through the port harness and check its committed
//! binding (docs/maintenance/porting-a-crate.md). The driver imports the
//! three `.vsys` libraries this crate implements.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(vesper_port)");
    #[cfg(feature = "vesper-port")]
    vesper_port_harness::Port::new("stddriver", "vesper")
        .binding("port/binding.rs")
        .build();
}
