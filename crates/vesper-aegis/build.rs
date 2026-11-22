//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("aegis", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One module at a time (`VESPER_AEGIS_DIVERGE=snp`), or all.
        println!("cargo:rerun-if-env-changed=VESPER_AEGIS_DIVERGE");
        let only = std::env::var("VESPER_AEGIS_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests port/check.sh names fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // reader.rs: reserved fields are never checked (they read as zero).
    (
        "reader",
        "all_zero(in b[start..end])",
        "all_zero(in b[start..start])",
    ),
    // cca::decode_value: one level less nesting than ciborium allows.
    (
        "cbor",
        "const MAX_DEPTH: u64 = 256;",
        "const MAX_DEPTH: u64 = 255;",
    ),
    // cca.rs: TBB RoTPK index 5 is refused.
    (
        "cca",
        "const ROTPK_MAX_INDEX: u64 = 5;",
        "const ROTPK_MAX_INDEX: u64 = 4;",
    ),
    // snp.rs: report version 7 is accepted.
    (
        "snp",
        "const SNP_REPORT_MAX_VERSION: u64 = 6;",
        "const SNP_REPORT_MAX_VERSION: u64 = 7;",
    ),
    // tdx.rs: a TD 1.5 "ex" body is one byte shorter (the vendor's v5
    // type-4 sample no longer parses).
    (
        "tdx",
        "const TDX_BODY_TD15_EX_LEN: u64 = 885;",
        "const TDX_BODY_TD15_EX_LEN: u64 = 884;",
    ),
    // sgx.rs: the PCK chain must be certification data type 6, not 5.
    (
        "sgx",
        "if cert_type != CERT_DATA_PCK_CHAIN_PEM {",
        "if cert_type != 6 {",
    ),
    // claims.rs: the CCA architecture tag is refused on the wire.
    (
        "wire",
        "if tag < ARCH_TDX || tag > ARCH_CCA {",
        "if tag < ARCH_TDX || tag > ARCH_SNP {",
    ),
    // policy.rs: a TCB SVN equal to the floor is stale.
    ("policy", "if tcb_svn < floor {", "if tcb_svn <= floor {"),
    // time.rs: a timestamp without a fraction must end in "Y".
    (
        "time",
        "        byte(in s, 19) == UPPER_Z\n",
        "        byte(in s, 19) == 89\n",
    ),
];
