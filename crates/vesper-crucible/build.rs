//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("crucible", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One edit at a time (an index into EDITS), so each check shows that
        // its own edit is caught; unset applies every edit.
        println!("cargo:rerun-if-env-changed=VESPER_CRUCIBLE_DIVERGENT");
        let only = std::env::var("VESPER_CRUCIBLE_DIVERGENT").ok().map(|i| {
            i.parse::<usize>()
                .expect("VESPER_CRUCIBLE_DIVERGENT is an index")
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
/// asserted by scripts/test-port-harness.sh, crates/vesper-crucible/port/check.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // rng.rs: a different SplitMix64 increment.
    (
        "r.word = wrapping_add(r.word, 0x9e3779b97f4a7c15);",
        "r.word = wrapping_add(r.word, 0x9e3779b97f4a7c17);",
    ),
    // source.rs draws: replayed values are no longer clamped to the bound.
    (
        "            value: min_u64(replay, max)\n",
        "            value: replay\n",
    ),
    // tape.rs text: hex digits a-f are written one letter off.
    (
        "wrapping_mul(letter, wrapping_sub(HEX_LETTER_BIAS, DIGIT_ZERO))",
        "wrapping_mul(letter, wrapping_sub(HEX_LETTER_BIAS + 1, DIGIT_ZERO))",
    ),
    // shrink.rs: invalid runs are accepted as reproducing failures.
    (
        "    if outcome == Outcome::FailedSameKind {\n",
        "    if outcome <= Outcome::Invalid {\n",
    ),
    // fuzz.rs: the "zero a value" mutation writes 1.
    ("self.cur.try_set(pos, 0);", "self.cur.try_set(pos, 1);"),
    // dpor.rs: unbounded DPOR adds every awake candidate, not the first.
    (
        "if (self.bounded || !any) && !self.in_sleep(n, q) {",
        "if !self.in_sleep(n, q) {",
    ),
];
