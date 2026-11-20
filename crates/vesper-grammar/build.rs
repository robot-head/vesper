//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("grammar", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One module at a time (`VESPER_GRAMMAR_DIVERGE=framing`), or all.
        println!("cargo:rerun-if-env-changed=VESPER_GRAMMAR_DIVERGE");
        let only = std::env::var("VESPER_GRAMMAR_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests that cover its module fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // framing.rs: a two-byte LEB128 value loses its high bits.
    (
        "framing",
        "value = value | shl(byte & LEB128_PAYLOAD, wrapping_mul(i, LEB128_SHIFT));",
        "value = value | shl(byte & LEB128_PAYLOAD, wrapping_mul(i, 6));",
    ),
    // arrow.rs: checked_sum misses overflow toward a positive sum.
    (
        "arrow",
        "if (value >= 0) == (sum >= 0) && (next >= 0) != (sum >= 0) {",
        "if (value >= 0) == (sum >= 0) && (next >= 0) != (sum >= 0) && sum < 0 {",
    ),
    // capnp.rs: the segment table pads the wrong segment counts.
    (
        "capnp",
        "let pad: u64 = if count & 1 == 0 {",
        "let pad: u64 = if count & 1 == 1 {",
    ),
    // capnp_packed.rs: a run tag counts one word too many.
    (
        "capnp_packed",
        "group = wrapping_add(group, additional);",
        "group = wrapping_add(group, wrapping_add(additional, 1));",
    ),
    // grammar.rs: the hard depth ceiling drops from 64 to 3.
    (
        "grammar",
        "max_depth: min(sw(in lim, 4), HARD_MAX_DEPTH),",
        "max_depth: min(sw(in lim, 4), 3),",
    ),
    // grammar/codec.rs: the encoder plans numbers one byte too long at 127.
    (
        "codec",
        "work(inout cx, NUMBER_WORK);\n    let len = leb_len(n);",
        "work(inout cx, NUMBER_WORK);\n    let len = leb_len(wrapping_add(n, 1));",
    ),
    // protobuf.rs: negative int32 values are not sign-extended.
    ("protobuf", "        low | HIGH_32_ONES", "        low"),
    // der.rs: SET OF order is checked backwards.
    ("der", "if less(\n", "if !less(\n"),
    // ipc.rs: metadata V4 (version 3) is refused.
    (
        "ipc",
        "if version != METADATA_V4 && version != METADATA_V5 {",
        "if version != METADATA_V5 {",
    ),
];
