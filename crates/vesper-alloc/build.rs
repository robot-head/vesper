//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("alloc", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = EDITS
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// The divergence checks, one per ported area (mirrored in BUILD.bazel and
/// asserted by scripts/test-port-harness.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // region.rs placement: spare chunks kept by a rewind are never reused.
    (
        "            let s = fit(chunk_at(in b, i), size, align);",
        "            let s = MAX_U64;",
    ),
    // region.rs watermarks: a rewound watermark stays outstanding.
    ("b.marks.truncate(pos);", "b.marks.truncate(inc(pos));"),
    // vec.rs: a linear vector of one element is no live obligation.
    (
        "        && sget(in b, index, SlotWord::Elen)\n        > 0;",
        "        && sget(in b, index, SlotWord::Elen)\n        > 1;",
    ),
];
