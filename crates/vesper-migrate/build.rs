//! Compile the Loom package in vesper/ into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md). Under Bazel
//! the object is prebuilt (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("migrate", "vesper").binding("port/binding.rs");
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_MIGRATE_DIVERGE");
            let only = std::env::var("VESPER_MIGRATE_DIVERGE").ok();
            EDITS
                .iter()
                .filter(|(area, _, _)| only.as_deref().is_none_or(|want| want == *area))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    (
        "strip",
        "if wrapping_add(i, 1) >= n {",
        "if false && wrapping_add(i, 1) >= n {",
    ),
    (
        "filter",
        "if last == BYTE_BACKSLASH {",
        "if false && last == BYTE_BACKSLASH {",
    ),
];
