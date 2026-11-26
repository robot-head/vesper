//! ABBA benchmark of the Vesper port against the last Rust build (SH6
//! lane 8, #1403). The Rust parsers and policy check were deleted, so the
//! Rust arm is this example built at the last commit that had them
//! (the benchmark task builds it in a temporary worktree) and run as a child
//! process: `<rust-abba> window rust <workload> N` prints the nanoseconds
//! its N passes took, timed inside that process.
//!
//! Workloads, each over the vendor samples in tests/data/vendor:
//!
//! - `snp`: `SnpReport::parse` of the four AMD reports (VirTEE Milan, Oak
//!   Milan, Genoa and Turin);
//! - `tdx`: `TdxQuote::parse` of four Intel TDX quotes (v4 and v5,
//!   including the TD 1.5 "ex" body);
//! - `sgx`: `SgxQuote::parse` of the two SGX quotes;
//! - `policy`: `verify_attestation_claims` of the claims an SNP report
//!   gives, against a policy with four architectures and 16 measurements;
//! - `cca_claims`: `CcaPlatformClaims::from_cbor` and
//!   `CcaRealmClaims::from_cbor` of the vendor CCA tokens' payloads;
//! - `cca_token`: `CcaToken::parse` of the vendor CCA tokens (COSE_Sign1
//!   structure, still Rust, included);
//! - `corim`: `Corim::parse_unsigned` of the vendor unsigned CoRIMs;
//! - `measure`: the measurement hashes (`report_data`, `single_extend` for
//!   the three register banks, `cca_rim_measurement`, `rak_hash`,
//!   `tls_key_channel_binding`, `expected_qe_report_data`);
//! - `ecdsa`: `EcPublicKey::verify_fixed` of a P-256 and a P-384
//!   signature.
//!
//! The first four compare with the build at the last commit with the Rust
//! parsers and policy check; the CBOR workloads with the build at the last
//! commit with ciborium decoding and the Rust CCA claim parsers, where
//! the benchmark task builds this file (which uses only the crate's public
//! API) in place of that commit's. `measure` and `ecdsa` compare the same
//! way with the build at the last commit with the ring hashing and
//! signature calls (part 4a).
//!
//! ```sh
//! aspect crate aegis bench-abba                    # ABBA, wall clock
//! aspect crate aegis bench-abba -- flip-gate snp      # the flip gate's pairs line
//! cargo run --release -p vesper-aegis --example abba -- window port snp 2000
//! ```
//!
//! `window port <workload> N` runs N passes once (for callgrind instruction
//! counts). `compare <rust-abba> [workload...]` runs 9 epochs of windows per workload in
//! the order A B B A (A = Rust, B = port), pairs each epoch's totals and
//! reports the median regression of B over A with
//! `vesper_bench::analyze_pairs`. `flip-gate <workload> <rust-abba>` runs
//! 30 such epochs and prints them in the flip gate's pairs protocol
//! (aspect ci flip-gate --, crates/vesper-aegis/flip-gate.json).
use std::hint::black_box;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use vesper_aegis as port;

const EPOCHS: usize = 9;
/// Epochs of one `flip-gate` run (the gate's default).
const FLIP_GATE_EPOCHS: usize = 30;
/// Each workload and the passes in one window.
const WORKLOADS: [(&str, usize); 11] = [
    ("snp", 20_000),
    ("tdx", 10_000),
    ("sgx", 10_000),
    ("policy", 50_000),
    ("cca_claims", 5_000),
    ("cca_token", 2_000),
    ("corim", 2_000),
    ("corim_store", 2_000),
    ("appraise_cca", 5_000),
    ("measure", 50_000),
    ("ecdsa", 500),
];

fn data(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/vendor")
            .join(path),
    )
    .unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(u8::is_ascii_hexdigit).collect();
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

struct Samples {
    snp: Vec<Vec<u8>>,
    tdx: Vec<Vec<u8>>,
    sgx: Vec<Vec<u8>>,
    /// CCA token collections, and their (platform, realm) payloads.
    cca: Vec<Vec<u8>>,
    cca_payloads: Vec<(Vec<u8>, Vec<u8>)>,
    corim: Vec<Vec<u8>>,
    cca_good: Vec<u8>,
    cca_plat_corim: Vec<u8>,
    cca_realm_corim: Vec<u8>,
    /// The `ecdsa` workload's cases.
    ecdsa: Vec<EcdsaCase>,
}

/// (curve, SEC1 public key, message, r || s).
type EcdsaCase = (port::Curve, Vec<u8>, Vec<u8>, Vec<u8>);

/// A fixed-width signature of a 200-byte message on each curve.
fn ecdsa_samples() -> Vec<EcdsaCase> {
    use ring::signature::{self, EcdsaKeyPair, KeyPair as _};
    let rng = ring::rand::SystemRandom::new();
    [
        (
            port::Curve::P256,
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        ),
        (
            port::Curve::P384,
            &signature::ECDSA_P384_SHA384_FIXED_SIGNING,
        ),
    ]
    .into_iter()
    .map(|(curve, alg)| {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
        let msg: Vec<u8> = (0..200u8).collect();
        let sig = key.sign(&rng, &msg).unwrap().as_ref().to_vec();
        (curve, key.public_key().as_ref().to_vec(), msg, sig)
    })
    .collect()
}

fn samples() -> Samples {
    let exact = |q: Vec<u8>, used: usize| q[..used].to_vec();
    let tdx = [
        "tdx-dcap-qvl/tdx_quote",
        "tdx-dcap-qvl-0.7/tdx_quote_td15ex",
        "tdx-go-tdx-guest/tdx_prod_quote_SPR_E4.dat",
        "tdx-trustee/tdx_quote_5.dat",
    ]
    .iter()
    .map(|p| {
        let q = data(p);
        let used = port::tdx::TdxQuote::parse_prefix(&q).unwrap().1;
        exact(q, used)
    })
    .collect();
    let sgx = ["sgx-dcap-qvl/sgx_quote", "sgx-trustee/occlum_quote.dat"]
        .iter()
        .map(|p| {
            let q = data(p);
            let used = port::sgx::SgxQuote::parse_prefix(&q).unwrap().1;
            exact(q, used)
        })
        .collect();
    let snp = vec![
        unhex(&String::from_utf8(data("snp-virtee-sev/report_milan.hex")).unwrap()),
        data("snp-oak/milan_report.bin"),
        data("snp-oak/genoa_report.bin"),
        data("snp-oak/turin_report.bin"),
    ];
    let cca: Vec<Vec<u8>> = [
        "cca-veraison/cca-token-01.cbor",
        "cca-veraison/cca-token-draft-ffm-03-all-claims.cbor",
        "cca-veraison-services/cca-good.cbor",
        "cca-veraison-services/e2e-cca-evidence.cbor",
    ]
    .iter()
    .map(|p| data(p))
    .filter(|b| port::cca::CcaToken::parse(b).is_ok())
    .collect();
    assert!(cca.len() >= 2, "the vendor CCA tokens parse");
    let cca_payloads = cca
        .iter()
        .map(|b| {
            let t = port::cca::CcaToken::parse(b).unwrap();
            (t.platform.payload.unwrap(), t.realm.payload.unwrap())
        })
        .collect();
    let corim: Vec<Vec<u8>> = [
        "cca-veraison-corim/unsigned-good-corim.cbor",
        "cca-veraison-services/corim-cca-realm-valid.cbor",
        "cca-veraison-services/e2e-cca-realm-endorsements.cbor",
    ]
    .iter()
    .map(|p| data(p))
    .filter(|b| port::corim::Corim::parse_unsigned(b).is_ok())
    .collect();
    assert!(!corim.is_empty(), "a vendor CoRIM parses");
    Samples {
        snp,
        tdx,
        sgx,
        cca,
        cca_payloads,
        corim,
        cca_good: data("cca-veraison-services/cca-good.cbor"),
        cca_plat_corim: data("cca-veraison-services/corim-cca-platform-valid.cbor"),
        cca_realm_corim: data("cca-veraison-services/corim-cca-realm-valid.cbor"),
        ecdsa: ecdsa_samples(),
    }
}

/// A policy check that passes every step: the SNP claims of a report, and
/// a policy whose matching measurement is the last of 16.
macro_rules! policy_case {
    ($k:ident, $report:expr) => {{
        use $k::claims::{SnpTcbVersion, SupplyChainEvidence, TeeArch};
        let report = $k::snp::SnpReport::parse($report).unwrap();
        let channel = [9u8; 48];
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&channel[..8]);
        let claims = $k::AttestationClaims {
            arch: TeeArch::AmdSevSnp,
            tcb_svn: 1,
            enclave_flags: $k::ENCLAVE_FLAG_DEBUG_DISABLED,
            mr_enclave_sha384: report.measurement,
            loom_lock_blake3: [1; 32],
            olean_spec_mif: [2; 32],
            chan_binding_prefix: prefix,
            channel_binding: channel,
            supply_chain_evidence: SupplyChainEvidence::ReportDataOnly,
            snp_reported_tcb: Some(SnpTcbVersion::from_u64(report.reported_tcb)),
            snp_vmpl: Some(report.vmpl),
        };
        let mut allowed_measurements: Vec<(TeeArch, [u8; 48])> =
            (0..15).map(|i| (TeeArch::AmdSevSnp, [i; 48])).collect();
        allowed_measurements.push((TeeArch::AmdSevSnp, report.measurement));
        let policy = $k::RelyingPartyPolicy {
            allowed_archs: vec![
                TeeArch::IntelTdx15,
                TeeArch::ArmCcaRealm,
                TeeArch::AmdSevSnp,
            ],
            tcb_floors: Default::default(),
            snp_component_floors: Some(SnpTcbVersion::default()),
            snp_max_vmpl: 3,
            allowed_measurements,
            expected_loom_blake3: [1; 32],
            expected_olean_mif: [2; 32],
            require_register_replay: false,
        };
        (claims, policy, channel)
    }};
}

/// One window of the port, in this process.
fn window(workload: &str, passes: usize, s: &Samples) -> u64 {
    macro_rules! go {
        ($k:ident) => {{
            let (claims, policy, channel) = policy_case!($k, &s.snp[1]);
            let now = 1_748_736_000 * 1_000_000_000u64;
            let plat_corim = $k::corim::Corim::parse_unsigned(&s.cca_plat_corim).unwrap();
            let realm_corim = $k::corim::Corim::parse_unsigned(&s.cca_realm_corim).unwrap();
            let mut cca_store = $k::corim::CcaCorimStore::new();
            cca_store.add(&plat_corim, now).unwrap();
            cca_store.add(&realm_corim, now).unwrap();
            let cca_verified =
                $k::cca::verify_cca_endorsed(&s.cca_good, &cca_store.endorsements).unwrap();
            let cca_opts = $k::corim::CcaAppraisalOptions {
                compare_signer_id: false,
            };
            let pass = || match workload {
                "snp" => {
                    for b in &s.snp {
                        black_box($k::snp::SnpReport::parse(black_box(b)).unwrap());
                    }
                }
                "tdx" => {
                    for b in &s.tdx {
                        black_box($k::tdx::TdxQuote::parse(black_box(b)).unwrap());
                    }
                }
                "sgx" => {
                    for b in &s.sgx {
                        black_box($k::sgx::SgxQuote::parse(black_box(b)).unwrap());
                    }
                }
                "policy" => {
                    for _ in 0..4 {
                        $k::verify_attestation_claims(
                            black_box(&claims),
                            black_box(&policy),
                            black_box(&channel),
                        )
                        .unwrap();
                    }
                }
                "cca_claims" => {
                    for (p, r) in &s.cca_payloads {
                        black_box($k::cca::CcaPlatformClaims::from_cbor(black_box(p)).unwrap());
                        black_box($k::cca::CcaRealmClaims::from_cbor(black_box(r)).unwrap());
                    }
                }
                "cca_token" => {
                    for b in &s.cca {
                        black_box($k::cca::CcaToken::parse(black_box(b)).unwrap());
                    }
                }
                "corim" => {
                    for b in &s.corim {
                        black_box($k::corim::Corim::parse_unsigned(black_box(b)).unwrap());
                    }
                }
                "corim_store" => {
                    let mut st = $k::corim::CcaCorimStore::new();
                    st.add(black_box(&plat_corim), now).unwrap();
                    st.add(black_box(&realm_corim), now).unwrap();
                    black_box(st);
                }
                "appraise_cca" => {
                    black_box(
                        $k::corim::appraise_cca_with_options(
                            black_box(&cca_verified),
                            black_box(&cca_store),
                            black_box(&cca_opts),
                        )
                        .unwrap(),
                    );
                }
                "measure" => {
                    use $k::claims::{GuestRuntimeClaims, RegisterHash, single_extend};
                    let rt = GuestRuntimeClaims {
                        channel_binding: [3; 48],
                        loom_lock_blake3: [4; 32],
                        olean_spec_mif: [5; 32],
                    };
                    black_box(black_box(&rt).report_data());
                    let d = [6u8; 32];
                    black_box(single_extend(RegisterHash::Sha384, 48, black_box(&d)));
                    black_box(single_extend(RegisterHash::Sha384, 64, black_box(&d)));
                    black_box(single_extend(RegisterHash::Sha512, 64, black_box(&d)));
                    let blob = [7u8; 97];
                    black_box($k::cca::cca_rim_measurement(
                        RegisterHash::Sha256,
                        black_box(&blob),
                    ));
                    black_box($k::cca::rak_hash(RegisterHash::Sha384, black_box(&blob)));
                    black_box($k::policy::tls_key_channel_binding(black_box(&blob)));
                    black_box($k::tdx::TdxQuote::expected_qe_report_data(
                        black_box(&[8; 64]),
                        black_box(&blob[..32]),
                    ));
                }
                "ecdsa" => {
                    for (curve, sec1, msg, sig) in &s.ecdsa {
                        let key = $k::EcPublicKey::from_sec1(*curve, sec1).unwrap();
                        key.verify_fixed(black_box(msg), black_box(sig), "bench")
                            .unwrap();
                    }
                }
                _ => panic!("unknown workload {workload}"),
            };
            pass();
            let start = Instant::now();
            for _ in 0..passes {
                pass();
            }
            start.elapsed().as_nanos() as u64
        }};
    }
    go!(port)
}

/// One window of the Rust build: `rust_abba window rust <workload> N`.
fn rust_window(rust_abba: &str, workload: &str, passes: usize) -> u64 {
    let out = Command::new(rust_abba)
        .args(["window", "rust", workload, &passes.to_string()])
        .output()
        .unwrap_or_else(|e| panic!("{rust_abba}: {e}"));
    assert!(
        out.status.success(),
        "{rust_abba} window rust {workload} failed"
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("the Rust window's nanoseconds")
}

/// One A B B A epoch: (Rust total, port total).
fn epoch(rust_abba: &str, workload: &str, passes: usize, s: &Samples) -> (u64, u64) {
    let a1 = rust_window(rust_abba, workload, passes);
    let b1 = window(workload, passes, s);
    let b2 = window(workload, passes, s);
    let a2 = rust_window(rust_abba, workload, passes);
    (a1 + a2, b1 + b2)
}

fn main() {
    assert_eq!(port::IMPLEMENTATION, "vesper");
    let s = samples();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // `window port` here; `window rust` in a baseline the benchmark task
        // built (the same code over that commit's crate).
        Some("window") => println!("{}", window(&args[3], args[4].parse().unwrap(), &s)),
        Some("flip-gate") => {
            let workload = args[2].as_str();
            let passes = WORKLOADS
                .iter()
                .find(|(w, _)| *w == workload)
                .unwrap_or_else(|| panic!("unknown workload {workload}"))
                .1;
            let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
                .map(|_| {
                    let (a, b) = epoch(&args[3], workload, passes, &s);
                    format!("{a}:{b}")
                })
                .collect();
            println!("flip-gate pairs {workload} {}", pairs.join(" "));
        }
        Some("compare") => {
            let only = &args[3..];
            for (workload, passes) in WORKLOADS {
                if !only.is_empty() && !only.iter().any(|w| w == workload) {
                    continue;
                }
                let mut pairs = Vec::new();
                let mut ratios = Vec::new();
                for _ in 0..EPOCHS {
                    let (a, b) = epoch(&args[2], workload, passes, &s);
                    ratios.push(b as f64 / a as f64);
                    pairs.push(vesper_bench::PairedSample::new(a, b, 2 * passes as u64).unwrap());
                }
                ratios.sort_by(f64::total_cmp);
                let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
                println!(
                    "{workload}: port/Rust median {:.2}x (min {:.2}x, max {:.2}x); 2x gate {:?}",
                    ratios[EPOCHS / 2],
                    ratios[0],
                    ratios[EPOCHS - 1],
                    analysis.decision
                );
            }
        }
        _ => panic!(
            "usage: abba window port <workload> N | compare <rust-abba> [workload...] | flip-gate <workload> <rust-abba>"
        ),
    }
}
