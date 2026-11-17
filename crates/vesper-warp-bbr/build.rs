//! Under `vesper-port`, compile the Vesper port into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The port is one compilation unit split into parts (`vesper fmt` formats
//! files of at most 8192 tokens), concatenated here in the order that
//! `vesper/Loom.toml` documents; BUILD.bazel does the same for Bazel.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &[
    "types", "arith", "pacing", "init", "inputs", "model", "probe", "control",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let mut unit = String::new();
        for part in PARTS {
            let path = dir.join("vesper/src").join(format!("{part}.ves"));
            println!("cargo:rerun-if-changed={}", path.display());
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            unit.push_str(&text);
            unit.push('\n');
        }
        let source = out.join("warp_bbr.ves");
        std::fs::write(&source, unit).unwrap_or_else(|e| panic!("{}: {e}", source.display()));
        let port = vesper_port_harness::Port::new("bbr", &source)
            .binding("port/binding.rs")
            .rust_types("crate");
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT_EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// The divergence check: one deliberate change per ported module (mirrored
/// in BUILD.bazel). `scripts/test-port-harness.sh` and
/// `//crates/vesper-warp-bbr:port_divergence_detected` assert that the
/// named test of each module fails and that an untouched one passes.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str)] = &[
    // pacing: `pacing::tests::pacing_delay_is_ceiling_and_defined_at_zero`.
    ("        wrapping_add(q, 1)", "        wrapping_add(q, 2)"),
    // filter: `filter::tests::cycle_max_keeps_two_cycles`.
    ("    f.current = 0;", "    f.current = 1;"),
    // bbr: `bbr::tests::persistent_congestion_collapses_cwnd_then_floors_at_min_pipe`.
    (
        "sat_add(b.inflight, b.smss)",
        "sat_add(b.inflight, wrapping_add(b.smss, 1))",
    ),
];
