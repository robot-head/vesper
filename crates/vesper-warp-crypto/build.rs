//! Compile the Vesper library `warp_crypto` (the Loom package in vesper/,
//! which depends on `@vesper/crypto`) into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("warp_crypto", "vesper").binding("port/binding.rs");
    // The divergence check: one deliberate change per ported area, which
    // the differential tests must catch (port/check.sh asserts that they
    // fail). One at a time with `VESPER_WARP_CRYPTO_DIVERGE=<area>`.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_WARP_CRYPTO_DIVERGE");
        let only = std::env::var("VESPER_WARP_CRYPTO_DIVERGE").ok();
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
    // schedule: the key-update label loses a letter.
    (
        "schedule",
        "in \"quic ku\".as_bytes()",
        "in \"quic k\".as_bytes()",
    ),
    // nonce: the packet number skips the fifth IV byte.
    ("nonce", "let m = if i < 4 {", "let m = if i < 5 {"),
    // header: long headers protect five first-byte bits.
    (
        "header",
        "        15\n    } else {\n        31",
        "        31\n    } else {\n        31",
    ),
    // sealer: a reused packet number reports a regression.
    (
        "sealer",
        "if wrapping_add(pn, 1) == s.next_pn {",
        "if pn == s.next_pn {",
    ),
    // update: a promotion retires the previous key at once.
    (
        "update",
        "wrapping_add(now_ns, k.retire_window_ns)",
        "wrapping_add(now_ns, 0)",
    ),
    // retry: the v1 nonce's last byte.
    (
        "retry",
        "461599d35d632bf2239825bb",
        "461599d35d632bf2239825bc",
    ),
];
