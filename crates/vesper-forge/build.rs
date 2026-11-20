//! Under `vesper-port`, link the Vesper port in vesper/ and set
//! cfg(vesper_port); see "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md. Without it this package builds the
//! Rust code, like the seed package (seed/), which has no build script.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The harness sets the cfg under `vesper-port`; declare it either way.
    println!("cargo:rustc-check-cfg=cfg({})", vesper_port_harness::CFG);
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("forge", "vesper").binding("port/binding.rs");
        // One divergent edit per ported area, one area at a time
        // (`VESPER_FORGE_DIVERGE=parse`) or all of them; port/test-port.sh
        // asserts that the tests catch each one.
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_FORGE_DIVERGE");
            let only = std::env::var("VESPER_FORGE_DIVERGE").ok();
            EDITS
                .iter()
                .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence checks, one per ported area (mirrored in BUILD.bazel and
/// asserted by port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // delimiters: a closing bracket may close any opening delimiter.
    (
        "delimiters",
        "            if wrapping_add(top, 3) != k {",
        "            if top == 0 {",
    ),
    // parse: `with` is no longer reserved, so it can name a binder.
    (
        "parse",
        "\n        && !text_is(in toks, in text, p.pos, in \"with\") {",
        " {",
    ),
    // resolve: a binding scope covers tokens newer than its binder, so a
    // caller's `let` captures macro-generated references.
    (
        "resolve",
        "            if rank <= sq(in ctx, wrapping_add(wrapping_mul(j, 2), 1)) {",
        "            if rank <= 18446744073709551615 {",
    ),
    // registry: package-private definitions are visible to dependents.
    (
        "registry",
        "    if def_word(in w, in g, d, 3) != 0 {",
        "    if def_word(in w, in g, d, 3) > 1 {",
    ),
];
