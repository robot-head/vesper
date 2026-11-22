//! Link the Vesper runtime in vesper/ and set cfg(vesper_port)
//! (docs/maintenance/porting-a-crate.md). The Vesper library opens the
//! Vulkan loader at run time, so nothing of Vulkan is linked.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    vesper_port_harness::Port::new("gpu", "vesper")
        .binding("port/binding.rs")
        .build();
}
