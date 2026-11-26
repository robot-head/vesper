//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("kinetic", "vesper")
        .binding("port/binding.rs")
        .rust_types("crate::port");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One edit at a time (an index into EDITS), so each check shows that
        // its own edit is caught; unset applies every edit.
        println!("cargo:rerun-if-env-changed=VESPER_KINETIC_DIVERGENT");
        let only = std::env::var("VESPER_KINETIC_DIVERGENT").ok().map(|i| {
            i.parse::<usize>()
                .expect("VESPER_KINETIC_DIVERGENT is an index")
        });
        EDITS
            .iter()
            .enumerate()
            .filter(|(i, _)| only.is_none_or(|only| only == *i))
            .fold(port, |port, (_, (from, to))| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel and
/// asserted by aspect crate kinetic check).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // morton.rs: a code with bit 30 set decodes instead of failing.
    (
        "if (code & MORTON30_HIGH_BITS) != 0 {",
        "if (code & 0x80000000) != 0 {",
    ),
    // fastmath.rs: the cosine kernel adds the reduction tail instead of subtracting it.
    (
        "w2 + (((1.0 - w2) - hz) + (z * r - x * y))",
        "w2 + (((1.0 - w2) - hz) + (z * r + x * y))",
    ),
    // coloring.rs: Colorer never marks a constraint's first body busy.
    (
        "masks.try_set(ca, g64(in masks, ca) | bit);",
        "masks.try_set(ca, g64(in masks, ca));",
    ),
    // lbvh.rs: pairs of adjacent sorted leaves are dropped.
    ("(c & INDEX_MASK) > p\n", "(c & INDEX_MASK) > inc(p)\n"),
    // sap.rs: sortable keys flip the sign test.
    ("if (bits >> 31) == 1 {", "if (bits >> 31) == 0 {"),
    // crmath.rs: the sine of the third quadrant keeps its sign.
    ("        quadrant >= 2\n", "        quadrant >= 3\n"),
    // crmath.rs (bignum): a magnitude's low word drops its upper half (binary64 mantissas break).
    (
        "    g64(in a.mag.as_slice(), 0)\n",
        "    g64(in a.mag.as_slice(), 0) & 0xffffffff\n",
    ),
    // gjk.rs: a separated distance subtracts only the first radius.
    (
        "            gd0 - radii,\n            normal,\n",
        "            gd0 - ra,\n            normal,\n",
    ),
    // rigid/ccd.rs: conservative advancement never detects a hit.
    ("        if d <= margin {\n", "        if d < -1.0 {\n"),
    // rigid.rs (manifold): narrowphase always reports no contact.
    ("    if d0 > margin {\n", "    if d0 <= margin {\n"),
    // rigid/sleep.rs: quiet islands never fall asleep.
    (
        "if g8(in island_awake, r) == 0 {",
        "if g8(in island_awake, r) != 0 {",
    ),
    // diff.rs: backward sweep skips nonzero adjoints.
    ("if a != 0.0 {", "if a == 0.0 {"),
    // rigid.rs (solve): a joint motor drives the coordinate rate to zero.
    (
        "let target: f32 = if drive == 1 {",
        "let target: f32 = if drive == 3 {",
    ),
    // world.rs (substeps): contacts bounce only when they separate.
    (
        "    let wanted: f32 = if vn_prev < 0.0 {",
        "    let wanted: f32 = if vn_prev > 0.0 {",
    ),
    // soft.rs (substeps): the volume constraint keeps 6 V at 6 V0.
    ("e3z) - 6.0 * rest_volume;", "e3z) - 5.0 * rest_volume;"),
    // couple.rs: contact projection pulls particles into rigid bodies instead of separating.
    (
        "let dl = (-nearest_f32(d)) / wsum;",
        "let dl = nearest_f32(d) / wsum;",
    ),
];
