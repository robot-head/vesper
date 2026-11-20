//! Under `vesper-port`, compile the Vesper build into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The Vesper source is one library split over the files of
//! `vesper/src/` in the order of [`SOURCES`], because `vesper fmt` formats
//! at most 8192 tokens per file and the harness compiles one file. They
//! are joined into `$OUT_DIR/ignis.ves` (BUILD.bazel does the same).

/// The fragments of the Vesper library, in order (mirrored in BUILD.bazel).
#[cfg_attr(not(feature = "vesper-port"), allow(dead_code))]
const SOURCES: &[&str] = &[
    "d3d12.ves",
    "graph.ves",
    "plan_order.ves",
    "plan_state.ves",
    "plan_passes.ves",
    "assemble_slots.ves",
    "assemble.ves",
    "lower.ves",
    "lifecycle.ves",
    "lifecycle_completion.ves",
    "recording.ves",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let mut source = String::new();
        for name in SOURCES {
            let path = format!("vesper/src/{name}");
            println!("cargo:rerun-if-changed={path}");
            source.push_str(&std::fs::read_to_string(&path).expect("read a Vesper fragment"));
        }
        let joined = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ignis.ves");
        // Rewrite only on change, so the harness's rerun check stays quiet.
        if std::fs::read_to_string(&joined).ok().as_deref() != Some(source.as_str()) {
            std::fs::write(&joined, &source).expect("write the joined Vesper library");
        }
        let port = vesper_port_harness::Port::new("ignis", &joined)
            .binding("port/binding.rs")
            .rust_types("crate::port::ffi");
        #[cfg(feature = "vesper-port-divergent")]
        let port = EDITS
            .iter()
            .fold(port, |port, (_, from, to)| port.edit(from, to));
        port.build();
    }
}

/// The divergence edits, one per ported module: (module, from, to). The
/// test each must fail is in tests/divergence.rs (BUILD.bazel mirrors the
/// edits; port/check.sh runs the check under Cargo).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // d3d12: NON_PIXEL_SHADING also covers pixel shading.
    (
        "d3d12",
        "bits = bits | SYNC_VERTEX_SHADING | SYNC_COMPUTE_SHADING;",
        "bits = bits | SYNC_VERTEX_SHADING | SYNC_PIXEL_SHADING | SYNC_COMPUTE_SHADING;",
    ),
    // graph: overlapping uses with different explicit layouts are accepted.
    (
        "graph",
        "let differ = a.has_layout && b.has_layout && a.layout != b.layout;",
        "let differ = false;",
    ),
    // plan: a buffer write no longer orders after the previous writer.
    (
        "plan",
        "has_other: has_prior && access_is_write(access),",
        "has_other: false,",
    ),
    // plan (assembly): every planner signal is one fence value too high.
    (
        "assemble",
        "b: get64(in value_of, p)",
        "b: wrapping_add(get64(in value_of, p), 1)",
    ),
    // multi_backend: COPY maps to the Metal dispatch stage, not blit.
    (
        "multi_backend",
        "out = out | blit;",
        "out = out | dispatch;",
    ),
    // lifecycle: a completion at the observed value is not stale.
    (
        "lifecycle",
        "if value <= get64(in l.observed, index) {",
        "if value < get64(in l.observed, index) {",
    ),
    // recording: the first-queue-only mode publishes every queue.
    (
        "recording",
        "let keep: u64 = if mode == SubmitMode::AcceptFirstQueueUncertain {",
        "let keep: u64 = if mode == SubmitMode::StallUncertain {",
    ),
];
