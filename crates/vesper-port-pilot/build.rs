//! Under `vesper-port`, compile vesper/pilot.ves into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("pilot", "vesper/pilot.ves")
            .binding("src/port_binding.rs")
            .rust_types("crate");
        // The divergence check: one comparison is off by one, so the port
        // run of tests/api.rs must fail (CI asserts that it does).
        #[cfg(feature = "vesper-port-divergent")]
        let port = port.edit("c >= 48 && c <= 57", "c >= 48 && c <= 58");
        port.build();
    }
}
