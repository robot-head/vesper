//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("chrono", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One module at a time (`VESPER_CHRONO_DIVERGE=formatter`), or all.
        println!("cargo:rerun-if-env-changed=VESPER_CHRONO_DIVERGE");
        let only = std::env::var("VESPER_CHRONO_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests .aspect/port-checks.axl names fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // formatter: the trigger ID (0x7D) is a trace stream.
    (
        "formatter",
        "                if cur > 0 && cur < 112 {",
        "                if cur > 0 && cur < 112 || cur == 125 {",
    ),
];
