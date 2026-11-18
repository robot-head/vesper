//! Under `vesper-port`, compile vesper/pilot.ves into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("pilot", "vesper/pilot.ves")
            .binding("port/binding.rs")
            .rust_types("crate");
        // The divergence check: one comparison is off by one, so the port
        // run of tests/api.rs must fail (CI asserts that it does).
        #[cfg(feature = "vesper-port-divergent")]
        let port = port.edit("c >= 48 && c <= 57", "c >= 48 && c <= 58");
        port.build();
        // The export ABI v2 library (#898). Its divergent build changes one
        // thing per exported form, plus one Vesper-side free (a sequence
        // parked in a dropped `Box` is never freed); tests/kit.rs and
        // tests/kit_memory.rs must each catch theirs.
        let kit = vesper_port_harness::Port::new("kit", "vesper/kit.ves")
            .binding("port/kit.rs")
            .rust_types("crate::kit");
        #[cfg(feature = "vesper-port-divergent")]
        let kit = KIT_EDITS
            .iter()
            .fold(kit, |kit, (from, to)| kit.edit(from, to));
        kit.build();
        // The export ABI v3 library (#934): copy structs lent in place
        // through `#[repr(C)]` mirrors.
        vesper_port_harness::Port::new("gauge", "vesper/gauge.ves")
            .binding("port/gauge.rs")
            .rust_types("crate::gauge")
            .build();
    }
}

/// The kit's divergence edits, one per form (mirrored in BUILD.bazel).
#[cfg(feature = "vesper-port-divergent")]
const KIT_EDITS: &[(&str, &str)] = &[
    // Sequence results: `sequence_results` fails.
    (
        "        s.push(wrapping_mul(i, i));",
        "        s.push(wrapping_mul(i, wrapping_add(i, 1)));",
    ),
    // Slices: `slice_parameters` fails.
    ("if c >= 48 && c <= 57 {", "if c >= 48 && c <= 58 {"),
    // Strings: `strings` fails.
    ("    s.push_char(33);", "    s.push_char(63);"),
    // Owned handles: `owned_handles` fails.
    (
        "c.total = wrapping_add(c.total, v);",
        "c.total = wrapping_add(c.total, wrapping_add(v, 1));",
    ),
    // Linear handles: `linear_handles` fails.
    ("    id\n}", "    wrapping_add(id, 1)\n}"),
    // Boxes and recursive enums: `recursive_enum_handles` fails.
    (
        "Tree::Leaf(v) => v,",
        "Tree::Leaf(v) => wrapping_add(v, 1),",
    ),
    // Generic instances: `generic_instances` fails.
    ("Greater => {", "Less => {"),
    // `.vsys` imports: `vsys_imports` fails.
    (
        "wrapping_add(base, Clock::pilot_clock_ticks())",
        "wrapping_sub(base, Clock::pilot_clock_ticks())",
    ),
    // One Vesper-side free omitted: tests/kit_memory.rs fails. The reset
    // parks a fresh sequence in a dropped `Box`, whose payload native code
    // does not free yet (#985's "what is left").
    (
        "c.hits.clear();",
        "let parked = Box::new(ChunkSeq::with_capacity[u64](4));\n    c.hits.clear();",
    ),
];
