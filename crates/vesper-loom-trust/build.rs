//! Compile the Vesper library `loom_trust` (the Loom package in vesper/,
//! which depends on `@vesper/crypto`) into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_trust", "vesper").binding("port/binding.rs");
    // The divergence check: one deliberate change per ported area, which
    // the differential tests must catch (port/check.sh asserts that they
    // fail). One at a time with `VESPER_LOOM_TRUST_DIVERGE=<area>`.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_LOOM_TRUST_DIVERGE");
        let only = std::env::var("VESPER_LOOM_TRUST_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence edits (mirrored in BUILD.bazel): each area, the text it
/// replaces and the wrong text.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // semver: a SemVer number keeps one leading zero.
    (
        "semver",
        "if value == 0 && len > 0 {",
        "if value == 0 && len > 1 {",
    ),
    // object: a source blob passes as a source tree.
    ("object", "if source != 6 {", "if source / 4 != 1 {"),
    // message: the signing domain names another version.
    (
        "message",
        "\"vesper.loom.builder-attestation.v1\"",
        "\"vesper.loom.builder-attestation.v2\"",
    ),
    // quorum: each key counts as an operator.
    (
        "quorum",
        "if distinct.len() < policy.minimum_operators {",
        "if keys.len() < policy.minimum_operators {",
    ),
    // interval: a key is valid at its end.
    (
        "interval",
        "|| now >= b.valid_until ||",
        "|| now > b.valid_until ||",
    ),
    // conflation: `8` instead of `7` is a lookalike of `t`.
    ("conflation", "} else if l == 55 {", "} else if l == 56 {"),
    // distance: a transposition costs two.
    (
        "distance",
        "wrapping_sub(j, 2))), 1));",
        "wrapping_sub(j, 2))), 2));",
    ),
    // screen: the trust-cost maximum itself is excessive.
    (
        "screen",
        "if cost > widen_u64(policy.max_trust_cost) {",
        "if cost >= widen_u64(policy.max_trust_cost) {",
    ),
];
