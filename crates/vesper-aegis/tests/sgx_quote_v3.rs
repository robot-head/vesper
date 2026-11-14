//! Wave-eight: Intel SGX DCAP quote v3 (body type 1) on real published
//! samples. Provenance of every file is in `tests/data/vendor/README.md`.
//!
//! - `sgx-dcap-qvl/sgx_quote` with `sgx_quote_collateral.json`: a real SGX
//!   quote and the Intel PCS collateral fetched for it (Phala `dcap-qvl`).
//!   `dcap-qvl` reports `ConfigurationAndSWHardeningNeeded` with advisories
//!   `INTEL-SA-00289`, `INTEL-SA-00615`; this implementation must agree.
//! - `sgx-trustee/occlum_quote.dat`: a real SGX quote from an Occlum
//!   enclave (Confidential Containers Trustee test data), issued under the
//!   PCK **Platform** CA. No collateral was published with it, so only its
//!   signatures and PCK chain are checked.

use vesper_aegis::error::{AttestError, Format, ParseError};
use vesper_aegis::intel::{
    PckIssuer, SignedCollateral, TcbAcceptance, TcbStatus, TdxCollateral, verify_tdx_collateral,
};
use vesper_aegis::roots::INTEL_SGX_ROOT_CA;
use vesper_aegis::sgx::{
    INTEL_QE_VENDOR_ID, SGX_SIGNED_LEN, SgxEnclavePolicy, SgxQuote, SgxTcbInfo,
    verify_sgx_collateral, verify_sgx_quote_signatures_only,
};

mod support;

const NS: u64 = 1_000_000_000;
/// 2025-06-20T00:00:00Z: inside every window of the `sgx_quote` collateral
/// (TCB info 2025-06-19T10:56:11Z..2025-07-19, QE identity
/// 2025-06-19T10:01:18Z..2025-07-19, PCK CRL 2025-06-19T10:23:18Z..07-19,
/// root CA CRL 2025-03-20..2026-04-03).
const SGX_NOW: u64 = 1_750_377_600 * NS;
/// 2026-01-01T00:00:00Z: inside the Occlum PCK certificate's validity
/// (2023-04-26..2030-04-26).
const OCCLUM_NOW: u64 = 1_767_225_600 * NS;

fn data(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/data/vendor/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn collateral() -> TdxCollateral {
    let j: serde_json::Value =
        serde_json::from_slice(&data("sgx-dcap-qvl/sgx_quote_collateral.json")).unwrap();
    let s = |k: &str| j[k].as_str().unwrap().to_owned();
    TdxCollateral {
        tcb_info: SignedCollateral::new(s("tcb_info"), &s("tcb_info_signature")).unwrap(),
        tcb_info_issuer_chain_pem: s("tcb_info_issuer_chain").into_bytes(),
        qe_identity: SignedCollateral::new(s("qe_identity"), &s("qe_identity_signature")).unwrap(),
        qe_identity_issuer_chain_pem: s("qe_identity_issuer_chain").into_bytes(),
        pck_crl_der: unhex(&s("pck_crl")),
        root_ca_crl_der: unhex(&s("root_ca_crl")),
    }
}

fn root() -> vesper_aegis::TrustRoot {
    INTEL_SGX_ROOT_CA.trust_root().unwrap()
}

fn quote() -> Vec<u8> {
    data("sgx-dcap-qvl/sgx_quote")
}

#[test]
fn real_sgx_quote_v3_verifies_end_to_end() {
    let q = SgxQuote::parse(&quote()).unwrap();
    assert_eq!(q.header.version, 3);
    assert_eq!(q.header.att_key_type, 2);
    assert_eq!((q.header.qe_svn, q.header.pce_svn), (10, 15));
    assert_eq!(q.header.qe_vendor_id, INTEL_QE_VENDOR_ID);
    assert_eq!(&q.body.mr_enclave[..4], &[0x33, 0xD8, 0x73, 0x6D]);
    assert_eq!(&q.body.mr_signer[..4], &[0x81, 0x5F, 0x42, 0xF1]);
    assert_eq!((q.body.isv_prod_id, q.body.isv_svn), (0, 0));
    assert_eq!(&q.body.report_data[..13], b"Hello, world!");
    assert!(!q.body.is_debug());

    let v = verify_sgx_collateral(&quote(), &collateral(), &root(), SGX_NOW).unwrap();
    // The same verdict as dcap-qvl's `could_parse_sgx_quote` test.
    assert_eq!(
        v.evaluation.status,
        TcbStatus::ConfigurationAndSWHardeningNeeded
    );
    assert_eq!(
        v.evaluation.platform_status,
        TcbStatus::ConfigurationAndSWHardeningNeeded
    );
    assert_eq!(v.evaluation.qe_status, TcbStatus::UpToDate);
    assert_eq!(
        v.evaluation.advisory_ids,
        ["INTEL-SA-00289", "INTEL-SA-00615"]
    );
    assert_eq!(v.evaluation.tcb_evaluation_data_number, 17);
    assert_eq!(v.pck.fmspc, [0x00, 0xA0, 0x67, 0x11, 0x00, 0x00]);
    assert_eq!(v.pck.issuer, PckIssuer::Processor);
    assert_eq!(v.tcb_info.fmspc, v.pck.fmspc);

    // A relying party that only accepts UpToDate refuses it; one that
    // accepts the status takes it.
    assert!(TcbAcceptance::default().check(v.evaluation.status).is_err());
    TcbAcceptance {
        accept: vec![TcbStatus::ConfigurationAndSWHardeningNeeded],
    }
    .check(v.evaluation.status)
    .unwrap();

    // The enclave policy.
    let mut policy = SgxEnclavePolicy {
        mr_enclave: Some(v.quote.body.mr_enclave),
        mr_signer: Some(v.quote.body.mr_signer),
        isv_prod_id: Some(0),
        min_isv_svn: 0,
        allow_debug: false,
        report_data: Some(v.quote.body.report_data),
    };
    policy.check(&v.quote.body).unwrap();
    policy.min_isv_svn = 1;
    assert!(matches!(
        policy.check(&v.quote.body),
        Err(AttestError::BindingMismatch { .. })
    ));
    policy.min_isv_svn = 0;
    let mut other = v.quote.body.mr_enclave;
    other[0] ^= 1;
    policy.mr_enclave = Some(other);
    assert!(policy.check(&v.quote.body).is_err());
    let mut debug_body = v.quote.body.clone();
    debug_body.attributes[0] |= vesper_aegis::sgx::SGX_FLAGS_DEBUG;
    let policy = SgxEnclavePolicy::default();
    assert!(policy.check(&debug_body).is_err());
    SgxEnclavePolicy {
        allow_debug: true,
        ..Default::default()
    }
    .check(&debug_body)
    .unwrap();
}

#[test]
fn real_sgx_quote_tampering_fails_typed() {
    let q = quote();
    let c = collateral();
    let r = root();
    // Every byte of the signed header and body is covered by the
    // attestation-key signature (or rejected earlier by the parser).
    for i in (0..SGX_SIGNED_LEN).step_by(7) {
        let mut t = q.clone();
        t[i] ^= 0x01;
        let e = verify_sgx_collateral(&t, &c, &r, SGX_NOW).unwrap_err();
        assert!(
            matches!(
                e,
                AttestError::BadSignature { .. }
                    | AttestError::Parse(_)
                    | AttestError::CollateralMismatch { .. }
            ),
            "byte {i}: {e:?}"
        );
    }
    // A flipped attestation-key signature byte.
    let mut t = q.clone();
    t[SGX_SIGNED_LEN + 4] ^= 1;
    assert!(matches!(
        verify_sgx_collateral(&t, &c, &r, SGX_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // A flipped QE report byte (MRENCLAVE of the QE): the PCK signature.
    let qe = SGX_SIGNED_LEN + 4 + 128 + 64;
    let mut t = q.clone();
    t[qe] ^= 1;
    assert!(matches!(
        verify_sgx_collateral(&t, &c, &r, SGX_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // The QE vendor ID must be Intel's.
    let mut t = q.clone();
    t[12] ^= 1;
    assert!(matches!(
        verify_sgx_collateral(&t, &c, &r, SGX_NOW),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // Header checks.
    for (off, field) in [(0, "header.version"), (2, "header.att_key_type")] {
        let mut t = q.clone();
        t[off] = 4;
        assert!(
            matches!(
                SgxQuote::parse(&t),
                Err(ParseError::UnsupportedValue {
                    format: Format::SgxQuoteV3,
                    field: f,
                    ..
                }) if f == field
            ),
            "{field}"
        );
    }
    let mut t = q.clone();
    t[4] = 0x81; // a TDX TEE type in att_key_data_0
    assert!(matches!(
        SgxQuote::parse(&t),
        Err(ParseError::NonZeroReserved { .. })
    ));
    // Truncation anywhere is a typed error.
    for n in [0, 47, 48, 431, 432, 435, 600, q.len() - 1] {
        assert!(SgxQuote::parse(&q[..n]).is_err(), "len {n}");
    }
    // Zero padding is accepted, other trailing bytes are not.
    let mut padded = q.clone();
    padded.extend_from_slice(&[0; 64]);
    verify_sgx_collateral(&padded, &c, &r, SGX_NOW).unwrap();
    padded.push(1);
    assert!(matches!(
        verify_sgx_collateral(&padded, &c, &r, SGX_NOW),
        Err(AttestError::Parse(ParseError::TrailingBytes { .. }))
    ));
    // A TDX quote is not an SGX quote.
    let tdx = data("tdx-dcap-qvl/tdx_quote");
    assert!(SgxQuote::parse(&tdx).is_err());
}

#[test]
fn real_sgx_collateral_checks() {
    let q = quote();
    let c = collateral();
    let r = root();
    // Outside the collateral windows.
    assert!(matches!(
        verify_sgx_collateral(&q, &c, &r, SGX_NOW + 40 * 86_400 * NS),
        Err(AttestError::CollateralExpired { .. }) | Err(AttestError::CertificateChain(_))
    ));
    // A tampered TCB info body fails its signature.
    let mut t = c.clone();
    let body = String::from_utf8(t.tcb_info.body.clone()).unwrap();
    t.tcb_info.body = body
        .replacen("ConfigurationAndSWHardeningNeeded", "UpToDate", 1)
        .into_bytes();
    assert!(matches!(
        verify_sgx_collateral(&q, &t, &r, SGX_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // A tampered QE identity body fails its signature.
    let mut t = c.clone();
    t.qe_identity.signature[0] ^= 1;
    assert!(matches!(
        verify_sgx_collateral(&q, &t, &r, SGX_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // TDX collateral (correctly signed by Intel) is the wrong document.
    let tdx: serde_json::Value =
        serde_json::from_slice(&data("tdx-dcap-qvl/tdx_quote_collateral.json")).unwrap();
    let mut t = c.clone();
    t.tcb_info = SignedCollateral::new(
        tdx["tcb_info"].as_str().unwrap(),
        tdx["tcb_info_signature"].as_str().unwrap(),
    )
    .unwrap();
    // (Its window is 2025-06; the signature verifies, the id does not.)
    assert!(matches!(
        verify_sgx_collateral(&q, &t, &r, SGX_NOW),
        Err(AttestError::Collateral { .. }) | Err(AttestError::CollateralExpired { .. })
    ));
    let mut t = c.clone();
    t.qe_identity = SignedCollateral::new(
        tdx["qe_identity"].as_str().unwrap(),
        tdx["qe_identity_signature"].as_str().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        verify_sgx_collateral(&q, &t, &r, SGX_NOW),
        Err(AttestError::Collateral { .. }) | Err(AttestError::CollateralExpired { .. })
    ));
    // The SGX quote through the TDX verifier, and SGX collateral through
    // the SGX TCB-info parser only.
    assert!(verify_tdx_collateral(&q, &c, &r, SGX_NOW).is_err());
    assert!(SgxTcbInfo::parse(&c.tcb_info.body).is_ok());
    assert!(SgxTcbInfo::parse(b"{}").is_err());
    // A level list nobody matches.
    let body = String::from_utf8(c.tcb_info.body.clone()).unwrap();
    let mut bumped = body.clone();
    for n in 0..=20 {
        bumped = bumped.replace(&format!("\"pcesvn\":{n},"), "\"pcesvn\":99,");
        bumped = bumped.replace(&format!("\"pcesvn\":{n}}}"), "\"pcesvn\":99}");
    }
    let info = SgxTcbInfo::parse(bumped.as_bytes()).unwrap();
    let v = verify_sgx_collateral(&q, &c, &r, SGX_NOW).unwrap();
    assert!(matches!(
        vesper_aegis::sgx::evaluate_sgx_tcb(&v.quote, &v.pck, &info, &v.qe_identity),
        Err(AttestError::TcbLevelNotFound { .. })
    ));
}

#[test]
fn real_occlum_sgx_quote_signatures_and_platform_ca() {
    let q = data("sgx-trustee/occlum_quote.dat");
    let (quote, pck) = verify_sgx_quote_signatures_only(&q, &root(), OCCLUM_NOW).unwrap();
    assert_eq!(quote.header.version, 3);
    assert_eq!(pck.issuer, PckIssuer::Platform);
    assert!(pck.platform_instance.is_some());
    let mut t = q.clone();
    t[100] ^= 1;
    assert!(verify_sgx_quote_signatures_only(&t, &root(), OCCLUM_NOW).is_err());
    // Its PCK chain is not covered by the other sample's Processor CA CRL.
    assert!(verify_sgx_collateral(&q, &collateral(), &root(), SGX_NOW).is_err());
}

#[test]
fn sgx_quote_mutations_never_panic() {
    let mut rng = support::SplitMix(0x5A5);
    for sample in [quote(), data("sgx-trustee/occlum_quote.dat")] {
        for _ in 0..300 {
            let mut t = sample.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(t.len() as u64) as usize;
                t[i] = rng.next() as u8;
            }
            if rng.below(4) == 0 {
                let n = rng.below(t.len() as u64) as usize;
                t.truncate(n);
            }
            let _ = SgxQuote::parse(&t);
            let _ = verify_sgx_quote_signatures_only(&t, &root(), OCCLUM_NOW);
        }
    }
}
