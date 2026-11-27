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
//! - `corim`: `Corim::parse_unsigned` of the vendor unsigned CoRIMs.
//!
//! The first four compare with the build at the last commit with the Rust
//! parsers and policy check; the CBOR workloads with the build at the last
//! commit with ciborium decoding and the Rust CCA claim parsers, where
//! the benchmark task builds this file (which uses only the crate's public
//! API) in place of that commit's.
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
const WORKLOADS: [(&str, usize); 9] = [
    ("snp", 20_000),
    ("tdx", 10_000),
    ("sgx", 10_000),
    ("policy", 50_000),
    ("cca_claims", 5_000),
    ("cca_token", 2_000),
    ("corim", 2_000),
    ("corim_store", 2_000),
    ("appraise_cca", 5_000),
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

/// Rust reference baseline for the `der_cert` in-process ABBA flip-gate
/// (matches the pre-flip `RawCert::parse` / `tlv` in `src/chain.rs`).
const DER_CERT_PASSES: usize = 20_000;
const DER_BIT_STRING: u8 = 0x03;
const DER_CONTEXT_0: u8 = 0xa0;
const DER_LONG_FORM: u8 = 0x80;
const DER_LENGTH_OCTETS_MASK: u8 = 0x7f;
const DER_MAX_LENGTH_OCTETS: usize = 4;
const DER_SHORT_HEADER: usize = 2;
const DER_TBS_ISSUER: usize = 2;
const DER_TBS_SUBJECT: usize = 4;
const DER_TBS_SPKI: usize = 5;
const DER_TBS_INIT_CAP: usize = 8;

fn rust_der_tlv(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let first_len = *b.get(1)?;
    let (len, header) = if first_len < DER_LONG_FORM {
        (usize::from(first_len), DER_SHORT_HEADER)
    } else {
        let n = usize::from(first_len & DER_LENGTH_OCTETS_MASK);
        if n == 0 || n > DER_MAX_LENGTH_OCTETS {
            return None;
        }
        let mut len = 0usize;
        for &x in b.get(DER_SHORT_HEADER..DER_SHORT_HEADER + n)? {
            len = (len << 8) | usize::from(x);
        }
        (len, DER_SHORT_HEADER + n)
    };
    let end = header.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some(b.split_at(end))
}

fn rust_der_contents(t: &[u8]) -> Option<&[u8]> {
    let header = if t[1] < DER_LONG_FORM {
        DER_SHORT_HEADER
    } else {
        DER_SHORT_HEADER + usize::from(t[1] & DER_LENGTH_OCTETS_MASK)
    };
    t.get(header..)
}

fn rust_parse_raw_cert(der: &[u8]) -> Option<(usize, usize)> {
    let (outer, rest) = rust_der_tlv(der)?;
    if !rest.is_empty() {
        return None;
    }
    let body = rust_der_contents(outer)?;
    let (tbs, body) = rust_der_tlv(body)?;
    let (_signature_alg, body) = rust_der_tlv(body)?;
    let (sig, body) = rust_der_tlv(body)?;
    if !body.is_empty() || sig[0] != DER_BIT_STRING {
        return None;
    }
    let sig = rust_der_contents(sig)?;
    let (&unused, _signature) = sig.split_first()?;
    if unused != 0 {
        return None;
    }
    let mut t = rust_der_contents(tbs)?;
    let mut fields = Vec::with_capacity(DER_TBS_INIT_CAP);
    while !t.is_empty() {
        let (f, rest) = rust_der_tlv(t)?;
        fields.push(f);
        t = rest;
    }
    let o = usize::from(fields.first().is_some_and(|f| f[0] == DER_CONTEXT_0));
    let _issuer = fields.get(o + DER_TBS_ISSUER).copied()?;
    let _subject = fields.get(o + DER_TBS_SUBJECT).copied()?;
    let spki = fields.get(o + DER_TBS_SPKI).copied()?;
    let (spki_alg, _) = rust_der_tlv(rust_der_contents(spki)?)?;
    Some((tbs.len(), spki_alg.len()))
}

fn flip_gate_der_cert() {
    let cert = rcgen::generate_simple_self_signed(vec!["vesper.test".to_string()]).unwrap();
    let der = cert.cert.der().as_ref().to_vec();
    assert_eq!(
        rust_parse_raw_cert(&der).unwrap(),
        port::chain::parse_raw_cert_for_bench(&der).unwrap()
    );
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..DER_CERT_PASSES {
            black_box(rust_parse_raw_cert(black_box(&der)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..DER_CERT_PASSES {
            black_box(port::chain::parse_raw_cert_for_bench(black_box(&der)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs der_cert {}", pairs.join(" "));
}

const PEM_PASSES: usize = 2_000;

fn flip_gate_pem() {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    let c1 = rcgen::generate_simple_self_signed(vec!["a.vesper.test".to_string()]).unwrap();
    let c2 = rcgen::generate_simple_self_signed(vec!["b.vesper.test".to_string()]).unwrap();
    let pem = format!("{}\n{}\n", c1.cert.pem(), c2.cert.pem()).into_bytes();
    let rust_decode = |b: &[u8]| -> Vec<Vec<u8>> {
        CertificateDer::pem_slice_iter(b)
            .map(|item| item.unwrap().as_ref().to_vec())
            .collect()
    };
    assert_eq!(
        rust_decode(&pem),
        port::chain::pem_certificates(&pem).unwrap()
    );
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..PEM_PASSES {
            black_box(rust_decode(black_box(&pem)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..PEM_PASSES {
            black_box(port::chain::pem_certificates(black_box(&pem)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs pem {}", pairs.join(" "));
}

const PEM_PUBKEY_PASSES: usize = 20_000;

fn flip_gate_pem_pubkey() {
    use rustls_pki_types::SubjectPublicKeyInfoDer;
    use rustls_pki_types::pem::PemObject;
    let cert = rcgen::generate_simple_self_signed(vec!["pk.vesper.test".to_string()]).unwrap();
    let pem = cert.signing_key.public_key_pem();
    let rust_decode = |s: &str| -> Vec<u8> {
        SubjectPublicKeyInfoDer::from_pem_slice(s.as_bytes())
            .unwrap()
            .as_ref()
            .to_vec()
    };
    assert_eq!(
        rust_decode(&pem),
        port::chain::decode_pem_public_key_for_bench(&pem).unwrap()
    );
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..PEM_PUBKEY_PASSES {
            black_box(rust_decode(black_box(&pem)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..PEM_PUBKEY_PASSES {
            black_box(port::chain::decode_pem_public_key_for_bench(black_box(&pem)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs pem_pubkey {}", pairs.join(" "));
}

const CRL_PASSES: usize = 20_000;

fn flip_gate_crl_this_update() {
    let key = rcgen::KeyPair::generate().unwrap();
    let ca_params = rcgen::CertificateParams::new(vec!["ca.vesper.test".to_string()]).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, key);
    let crl_params = rcgen::CertificateRevocationListParams {
        this_update: rcgen::date_time_ymd(2026, 11, 27),
        next_update: rcgen::date_time_ymd(2027, 11, 27),
        crl_number: rcgen::SerialNumber::from(1u64),
        issuing_distribution_point: None,
        revoked_certs: Vec::new(),
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    };
    let crl = crl_params.signed_by(&issuer).unwrap();
    let der = crl.der().as_ref().to_vec();
    let rust_parse = |b: &[u8]| -> u64 {
        let (rest, parsed) = x509_parser::parse_x509_crl(b).unwrap();
        assert!(rest.is_empty());
        u64::try_from(parsed.last_update().timestamp()).unwrap()
    };
    assert_eq!(
        rust_parse(&der),
        port::chain::crl_this_update(&der).unwrap()
    );
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..CRL_PASSES {
            black_box(rust_parse(black_box(&der)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..CRL_PASSES {
            black_box(port::chain::crl_this_update(black_box(&der)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs crl_this_update {}", pairs.join(" "));
}

const AMD_EXT_PASSES: usize = 5_000;

fn flip_gate_amd_cert_ext() {
    let der = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/vendor/snp-virtee-sev/vcek_milan.der"),
    )
    .unwrap();
    let rust_parse = |b: &[u8]| -> usize {
        let (rest, cert) = x509_parser::parse_x509_certificate(b).unwrap();
        assert!(rest.is_empty());
        cert.extensions().len()
    };
    let parsed =
        port::amd::parse_amd_cert_extensions(&der, port::roots::AmdProduct::Milan).unwrap();
    assert_eq!(parsed.product_name.split('-').next(), Some("Milan"));
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..AMD_EXT_PASSES {
            black_box(rust_parse(black_box(&der)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..AMD_EXT_PASSES {
            black_box(
                port::amd::parse_amd_cert_extensions(
                    black_box(&der),
                    port::roots::AmdProduct::Milan,
                )
                .unwrap(),
            );
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs amd_cert_ext {}", pairs.join(" "));
}

const PCK_EXT_PASSES: usize = 5_000;

fn flip_gate_pck_ext() {
    let quote_bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/vendor/tdx-dcap-qvl/tdx_quote"),
    )
    .unwrap();
    let quote = port::tdx::TdxQuote::parse_prefix(&quote_bytes).unwrap().0;
    let certs = port::chain::pem_certificates(&quote.pck_chain_pem).unwrap();
    let der = certs[0].clone();
    let rust_parse = |b: &[u8]| -> usize {
        let (rest, cert) = x509_parser::parse_x509_certificate(b).unwrap();
        assert!(rest.is_empty());
        cert.extensions().len()
    };
    let parsed = port::intel::parse_pck_extensions(&der).unwrap();
    assert_eq!(parsed.pce_id.len(), 2);
    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..PCK_EXT_PASSES {
            black_box(rust_parse(black_box(&der)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..PCK_EXT_PASSES {
            black_box(port::intel::parse_pck_extensions(black_box(&der)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs pck_ext {}", pairs.join(" "));
}

const SPKI_PASSES: usize = 500_000;

fn flip_gate_spki() {
    use spki::ObjectIdentifier;
    const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
    const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
    const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");

    let cert_der = data("snp-oak/milan_vcek.der");
    let (_, cert) = x509_parser::parse_x509_certificate(&cert_der).unwrap();
    let spki_der = cert.public_key().raw.to_vec();

    let rust_parse = |der: &[u8]| -> port::chain::EcPublicKey {
        let info = spki::SubjectPublicKeyInfoRef::try_from(der).unwrap();
        assert_eq!(info.algorithm.oid, ID_EC_PUBLIC_KEY);
        let curve = match info.algorithm.parameters_oid().unwrap() {
            o if o == SECP256R1 => port::chain::Curve::P256,
            o if o == SECP384R1 => port::chain::Curve::P384,
            _ => panic!("unexpected curve"),
        };
        let point = info.subject_public_key.as_bytes().unwrap();
        port::chain::EcPublicKey::from_sec1(curve, point).unwrap()
    };

    // Differential check: Vesper port matches Rust `spki` crate.
    let expected = rust_parse(&spki_der);
    let actual = port::chain::parse_spki_ec_key(&spki_der).unwrap();
    assert_eq!(actual, expected);

    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..SPKI_PASSES {
            black_box(rust_parse(black_box(&spki_der)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..SPKI_PASSES {
            black_box(port::chain::parse_spki_ec_key(black_box(&spki_der)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs spki {}", pairs.join(" "));
}

const PCS_ENVELOPE_PASSES: usize = 5_000;

fn flip_gate_pcs_envelope() {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        #[serde(borrow, rename = "tcbInfo")]
        tcb_info: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow, rename = "enclaveIdentity")]
        enclave_identity: Option<&'a serde_json::value::RawValue>,
        signature: String,
    }
    let response = data("tdx-go-tdx-guest/sample_tcbInfo_response");
    let rust_parse = |resp: &[u8]| -> port::intel::SignedCollateral {
        let env: Envelope<'_> = serde_json::from_slice(resp).unwrap();
        assert!(env.enclave_identity.is_none());
        let raw = env.tcb_info.unwrap();
        port::intel::SignedCollateral::new(raw.get().as_bytes(), &env.signature).unwrap()
    };

    // Differential check: Vesper port matches serde_json::from_slice.
    let expected = rust_parse(&response);
    let actual = port::intel::SignedCollateral::from_pcs_response(&response, "tcbInfo").unwrap();
    assert_eq!(actual, expected);

    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..PCS_ENVELOPE_PASSES {
            black_box(rust_parse(black_box(&response)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..PCS_ENVELOPE_PASSES {
            black_box(
                port::intel::SignedCollateral::from_pcs_response(black_box(&response), "tcbInfo")
                    .unwrap(),
            );
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs pcs_envelope {}", pairs.join(" "));
}

const QE_IDENTITY_PASSES: usize = 20_000;

fn flip_gate_qe_identity() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct IsvLevelJson {
        tcb: IsvSvnJson,
        tcb_date: String,
        tcb_status: String,
        #[serde(rename = "advisoryIDs", default)]
        advisory_ids: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct IsvSvnJson {
        isvsvn: u16,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct QeIdentityJson {
        id: String,
        version: u32,
        issue_date: String,
        next_update: String,
        tcb_evaluation_data_number: u32,
        miscselect: String,
        miscselect_mask: String,
        attributes: String,
        attributes_mask: String,
        mrsigner: String,
        isvprodid: u16,
        tcb_levels: Vec<IsvLevelJson>,
    }
    let response = data("tdx-go-tdx-guest/sample_qeIdentity_response");
    let signed =
        port::intel::SignedCollateral::from_pcs_response(&response, "enclaveIdentity").unwrap();
    let body = signed.body;

    let rust_parse = |json: &[u8]| -> QeIdentityJson {
        let j: QeIdentityJson = serde_json::from_slice(json).unwrap();
        assert_eq!(j.id, "TD_QE");
        assert_eq!(j.version, 2);
        assert!(!j.tcb_levels.is_empty());
        let _ = (&j.issue_date, &j.next_update, j.tcb_evaluation_data_number);
        let _ = (
            &j.miscselect,
            &j.miscselect_mask,
            &j.attributes,
            &j.attributes_mask,
            &j.mrsigner,
            j.isvprodid,
        );
        for l in &j.tcb_levels {
            let _ = (l.tcb.isvsvn, &l.tcb_date, &l.tcb_status, &l.advisory_ids);
        }
        j
    };

    // Differential check: Vesper port matches serde_json::from_slice.
    let expected = rust_parse(&body);
    let actual = port::intel::QeIdentity::parse(&body).unwrap();
    assert_eq!(
        actual.tcb_evaluation_data_number,
        expected.tcb_evaluation_data_number
    );
    assert_eq!(actual.isv_prod_id, expected.isvprodid);
    assert_eq!(actual.levels.len(), expected.tcb_levels.len());

    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..QE_IDENTITY_PASSES {
            black_box(rust_parse(black_box(&body)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..QE_IDENTITY_PASSES {
            black_box(port::intel::QeIdentity::parse(black_box(&body)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs qe_identity {}", pairs.join(" "));
}

const SGX_TCB_INFO_PASSES: usize = 20_000;

fn flip_gate_sgx_tcb_info() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SgxTcbInfoJson {
        id: String,
        version: u32,
        issue_date: String,
        next_update: String,
        fmspc: String,
        pce_id: String,
        tcb_type: u32,
        tcb_evaluation_data_number: u32,
        tcb_levels: Vec<SgxTcbLevelJson>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SgxTcbLevelJson {
        tcb: SgxTcbJson,
        tcb_date: String,
        tcb_status: String,
        #[serde(rename = "advisoryIDs", default)]
        advisory_ids: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SgxTcbJson {
        sgxtcbcomponents: Vec<ComponentJson>,
        pcesvn: u16,
    }
    #[derive(serde::Deserialize)]
    struct ComponentJson {
        svn: u8,
    }

    let body = br#"{"id":"SGX","version":3,"issueDate":"2024-03-13T16:12:41Z","nextUpdate":"2024-04-12T16:12:41Z","fmspc":"00806F050000","pceId":"0000","tcbType":0,"tcbEvaluationDataNumber":17,"tcbLevels":[{"tcb":{"sgxtcbcomponents":[{"svn":4},{"svn":4},{"svn":2},{"svn":4},{"svn":2},{"svn":255},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0}],"pcesvn":13},"tcbDate":"2024-03-13T00:00:00Z","tcbStatus":"UpToDate"},{"tcb":{"sgxtcbcomponents":[{"svn":2},{"svn":2},{"svn":2},{"svn":2},{"svn":2},{"svn":255},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0},{"svn":0}],"pcesvn":11},"tcbDate":"2023-08-09T00:00:00Z","tcbStatus":"OutOfDate","advisoryIDs":["INTEL-SA-00837","INTEL-SA-00950"]}]}"#;

    let rust_parse = |json: &[u8]| -> SgxTcbInfoJson {
        let j: SgxTcbInfoJson = serde_json::from_slice(json).unwrap();
        assert_eq!(j.id, "SGX");
        assert_eq!(j.version, 3);
        assert_eq!(j.tcb_type, 0);
        let _ = (
            &j.issue_date,
            &j.next_update,
            &j.fmspc,
            &j.pce_id,
            j.tcb_evaluation_data_number,
        );
        for l in &j.tcb_levels {
            let _ = (
                l.tcb.sgxtcbcomponents.iter().map(|c| c.svn).sum::<u8>(),
                l.tcb.pcesvn,
                &l.tcb_date,
                &l.tcb_status,
                &l.advisory_ids,
            );
        }
        j
    };

    // Differential check: Vesper port matches serde_json::from_slice.
    let expected = rust_parse(body);
    let actual = port::sgx::SgxTcbInfo::parse(body).unwrap();
    assert_eq!(
        actual.tcb_evaluation_data_number,
        expected.tcb_evaluation_data_number
    );
    assert_eq!(actual.levels.len(), expected.tcb_levels.len());

    let run_rust = || -> u64 {
        let t = Instant::now();
        for _ in 0..SGX_TCB_INFO_PASSES {
            black_box(rust_parse(black_box(body)));
        }
        t.elapsed().as_nanos() as u64
    };
    let run_port = || -> u64 {
        let t = Instant::now();
        for _ in 0..SGX_TCB_INFO_PASSES {
            black_box(port::sgx::SgxTcbInfo::parse(black_box(body)).unwrap());
        }
        t.elapsed().as_nanos() as u64
    };
    black_box(run_rust());
    black_box(run_port());
    let pairs: Vec<String> = (0..FLIP_GATE_EPOCHS)
        .map(|_| {
            let a1 = run_rust();
            let b1 = run_port();
            let b2 = run_port();
            let a2 = run_rust();
            format!("{}:{}", a1 + a2, b1 + b2)
        })
        .collect();
    println!("flip-gate pairs sgx_tcb_info {}", pairs.join(" "));
}

fn main() {
    assert_eq!(port::IMPLEMENTATION, "vesper");
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("flip-gate-inproc") {
        if args.get(2).map(String::as_str) == Some("der_cert") {
            flip_gate_der_cert();
            return;
        }
        if args.get(2).map(String::as_str) == Some("pem") {
            flip_gate_pem();
            return;
        }
        if args.get(2).map(String::as_str) == Some("pem_pubkey") {
            flip_gate_pem_pubkey();
            return;
        }
        if args.get(2).map(String::as_str) == Some("crl_this_update") {
            flip_gate_crl_this_update();
            return;
        }
        if args.get(2).map(String::as_str) == Some("amd_cert_ext") {
            flip_gate_amd_cert_ext();
            return;
        }
        if args.get(2).map(String::as_str) == Some("pck_ext") {
            flip_gate_pck_ext();
            return;
        }
        if args.get(2).map(String::as_str) == Some("spki") {
            flip_gate_spki();
            return;
        }
        if args.get(2).map(String::as_str) == Some("pcs_envelope") {
            flip_gate_pcs_envelope();
            return;
        }
        if args.get(2).map(String::as_str) == Some("qe_identity") {
            flip_gate_qe_identity();
            return;
        }
        if args.get(2).map(String::as_str) == Some("sgx_tcb_info") {
            flip_gate_sgx_tcb_info();
            return;
        }
    }
    let s = samples();
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
