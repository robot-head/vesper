//! Under `vesper-port`, compile the Vesper library `h3` (the Loom package in
//! vesper/) into a static library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("h3", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::qpack::port::abi");
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            // One module at a time (`VESPER_H3_DIVERGE=huffman`), or all.
            println!("cargo:rerun-if-env-changed=VESPER_H3_DIVERGE");
            let only = std::env::var("VESPER_H3_DIVERGE").ok();
            DIVERGENT_EDITS
                .iter()
                .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests that cover its module fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // huffman.rs: the last byte is padded with zeros instead of EOS ones.
    (
        "huffman",
        "out.try_set(at, wrapping_u8(shl(acc, pad) | wrapping_sub(bit(pad), 1)));",
        "out.try_set(at, wrapping_u8(shl(acc, pad)));",
    ),
    // prefix.rs: a multi-byte integer reports one byte too few.
    (
        "prefix",
        "used: wrapping_sub(inc(i), at)",
        "used: wrapping_sub(i, at)",
    ),
    // mod.rs Table: the oldest entry's absolute index is one too low.
    (
        "table",
        "wrapping_sub(t.inserted, table_len(in t))",
        "wrapping_sub(t.inserted, inc(table_len(in t)))",
    ),
    // encoder.rs: the Encoded Insert Count drops its `+ 1`.
    ("encoder", "inc(ric % full)", "ric % full"),
    // decoder.rs: post-base indices are read one entry too far.
    (
        "decoder",
        "let a = wrapping_add(base, index);",
        "let a = wrapping_add(base, inc(index));",
    ),
    // varint (frame.rs put_varint): 64 is written in one byte.
    ("varint", "if v < VI1_LIMIT {", "if v <= VI1_LIMIT {"),
    // frame.rs: a one-varint payload may have trailing bytes.
    ("frame", "if n.used == p.len() {", "if n.used <= p.len() {"),
    // priority.rs: urgency 8 is accepted.
    (
        "priority",
        "item.kind == ITEM_INTEGER && item.mag <= MAX_URGENCY",
        "item.kind == ITEM_INTEGER && item.mag <= URGENCY_LEVELS",
    ),
    // capsule.rs: the close code's first byte lands one byte low.
    (
        "capsule",
        "(widen_u64(byte(in v, 0)) << 24)",
        "(widen_u64(byte(in v, 0)) << 16)",
    ),
    // message.rs: two Host fields are accepted.
    ("message", "if hosts > 1 {", "if hosts > 2 {"),
];
