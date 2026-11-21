//! Under `vesper-port`, link the Vesper runtime in vesper/ and set
//! cfg(vesper_port) (docs/maintenance/porting-a-crate.md). The Vesper
//! library calls the Vulkan loader directly, so the build links the system
//! libvulkan. Without the feature this crate builds the Rust runtime over ash.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(vesper_port)");
    #[cfg(feature = "vesper-port")]
    {
        vesper_port_harness::Port::new("gpu", "vesper")
            .binding("port/binding.rs")
            .build();
        println!("cargo:rustc-link-lib=dylib=vulkan");
    }
}
