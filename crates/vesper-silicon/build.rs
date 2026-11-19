//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("silicon", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One edit at a time (an index into EDITS), so each check shows that
        // its own edit is caught; unset applies every edit.
        println!("cargo:rerun-if-env-changed=VESPER_SILICON_DIVERGENT");
        let only = std::env::var("VESPER_SILICON_DIVERGENT").ok().map(|i| {
            i.parse::<usize>()
                .expect("VESPER_SILICON_DIVERGENT is an index")
        });
        EDITS
            .iter()
            .enumerate()
            .filter(|(i, _)| only.is_none_or(|only| only == *i))
            .fold(port, |port, (_, (from, to))| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks, one per ported area (mirrored in BUILD.bazel and
/// asserted by crates/vesper-silicon/port/check.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // aia.ves, IMSIC: an identity equal to `eithreshold` is still delivered.
    (
        "if h.threshold == 0 || id < h.threshold {",
        "if h.threshold == 0 || id <= h.threshold {",
    ),
    // aia.ves, APLIC: equal priorities go to the higher source number.
    ("if !found || prio < bp {", "if !found || prio <= bp {"),
    // intc.ves, PLIC: a claim leaves its source pending.
    (
        "set_b(inout f.p_pending, at, false);",
        "set_b(inout f.p_pending, at, true);",
    ),
    // dtb.ves: node names start one byte late.
    (
        "let name = wrapping_add(off_struct, i);",
        "let name = wrapping_add(off_struct, wrapping_add(i, 1));",
    ),
];
