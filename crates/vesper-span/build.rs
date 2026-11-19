//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("span", "vesper")
        .binding("port/binding.rs")
        .rust_types("crate::port");
    // One divergent edit per ported area, one area at a time
    // (`VESPER_SPAN_DIVERGE=index`) or all of them; port/test-port.sh asserts
    // that the tests catch each one.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_SPAN_DIVERGE");
        let only = std::env::var("VESPER_SPAN_DIVERGE").ok();
        EDITS
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks (asserted by port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The line table and conversions: 3-byte chars count two UTF-16 units.
    (
        "index",
        "            character = if b >= 240 {",
        "            character = if b >= 224 {",
    ),
    // The edit checks: an insertion at the end of another edit is allowed.
    (
        "edits",
        "|| (start == end && start == b_end)",
        "|| (start == end && start == b_end && false)",
    ),
    // The diagnostic checks: a diagnostic without a primary label passes.
    (
        "diagnostics",
        "        if !primary {\n            Fail::raise(Error::InvalidDiagnostic);",
        "        if false {\n            Fail::raise(Error::InvalidDiagnostic);",
    ),
];
