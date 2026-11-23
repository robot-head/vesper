//! Compile the Vesper library `vault` (the Loom package in `vesper/`,
//! which depends on `@vesper/crypto`) into a static library and check the
//! committed binding (`port/binding.rs`, see `docs/maintenance/porting-a-crate.md`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("vault", "vesper").binding("port/binding.rs");
    // Divergence check: one deliberate change per ported area, which the
    // differential tests must catch (`port/check.sh` asserts that they fail).
    // Applied one at a time with `VESPER_VAULT_DIVERGE=<area>`.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_VAULT_DIVERGE");
        let only = std::env::var("VESPER_VAULT_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence edits (mirrored in `BUILD.bazel`): each area, the text it
/// replaces, and the wrong text.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // verify_tag: constant-time 16-byte tag compare inverts its verdict.
    (
        "verify_tag",
        "ct_eq(in expected, in computed)",
        "!ct_eq(in expected, in computed)",
    ),
    // aead_limits: AES-256-GCM record limit shrinks to 10 frames.
    (
        "aead_limits",
        "AeadKind::Aes256Gcm => AES256_RECORD_LIMIT,",
        "AeadKind::Aes256Gcm => 10,",
    ),
    // aead_seal: wire header writes sequence + 1 instead of ticket_seq.
    (
        "aead_seal",
        "let _h2 = frame.try_write_u64_be(4, ticket_seq);",
        "let _h2 = frame.try_write_u64_be(4, wrapping_add(ticket_seq, 1));",
    ),
    // aead_open: opener stops enforcing matching key_epoch on incoming frames.
    (
        "aead_open",
        "if hdr.key_epoch != expected_epoch {",
        "if false {",
    ),
    // kem_client: client X25519 keygen reads the wrong seed bytes.
    (
        "kem_client",
        "x25519_base(in seeds[64..96], inout share[1184..1216]);",
        "x25519_base(in seeds[0..32], inout share[1184..1216]);",
    ),
    // kem_server: server encapsulation skips FIPS 203 public-key modulus check.
    (
        "kem_server",
        "if !mlkem768_validate(isa_code(isa), in client_share[0..1184]) {",
        "if false {",
    ),
];
