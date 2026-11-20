//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("serde", "vesper")
        .binding("port/binding.rs")
        .rust_types("crate::port_types");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One module at a time (`VESPER_SERDE_DIVERGE=lemire`), or all.
        println!("cargo:rerun-if-env-changed=VESPER_SERDE_DIVERGE");
        let only = std::env::var("VESPER_SERDE_DIVERGE").ok();
        DIVERGENT_EDITS
            .iter()
            .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests that cover its module fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // lemire.ves: every Eisel-Lemire and Clinger result flips its low bit.
    ("lemire", "f.mantissa | sign", "(f.mantissa ^ 1) | sign"),
    // index.ves: the scalar parser refuses `e+` exponents.
    ("index", "if s == ASCII_PLUS || s == ASCII_MINUS {", "if s == ASCII_MINUS {"),
    // stage2.ves: stage 2 refuses fractions.
    (
        "stage2",
        "if p < end && at(in t, p) == ASCII_DOT {",
        "if p < end && at(in t, p) == ASCII_SLASH {",
    ),
    // dragon.ves (encode): negative floats print with a plus sign.
    (
        "encode",
        "out.try_set(p, ASCII_MINUS);",
        "out.try_set(p, ASCII_PLUS);",
    ),
    // grisu.ves: weeding keeps the last digit (the text still round-trips,
    // but is not the closest shortest one).
    (
        "grisu",
        "digits = wrapping_sub(digits, 1);",
        "digits = wrapping_add(digits, 0);",
    ),
    // dragon.ves: the Dragon fallback never rounds its last digit up.
    ("dragon", "        if round {", "        if !round {"),
    // helpers.ves: the escape mask lets a backslash escape a backslash.
    (
        "helpers",
        "carry = backslashes & bit != 0 && !carry;",
        "carry = backslashes & bit != 0;",
    ),
    // binary.ves: StreamVByte writes every value one byte too wide.
    (
        "binary",
        "let tag = bit_shl(wrapping_sub(w, 1), shift);",
        "let tag = bit_shl(w, shift);",
    ),
    // records.ves: tiles store the flags as the status code.
    (
        "records",
        "status_code: wrapping_u16(status),",
        "status_code: wrapping_u16(flags),",
    ),
];
