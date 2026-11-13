//! Real, publicly published vendor evidence verified offline against the
//! pinned vendor roots. Provenance of every file is in
//! `tests/data/vendor/README.md`.

use vesper_aegis::amd::{AmdTcb, SnpCollateral, SnpTcbBaseline, verify_snp_collateral};
use vesper_aegis::chain::{
    Curve, RevocationScope, pem_certificates, verify_chain, verify_chain_with_crls,
    verify_issued_by,
};
use vesper_aegis::error::{AttestError, ParseError};
use vesper_aegis::intel::{
    QeIdentity, SignedCollateral, TcbAcceptance, TcbStatus, TdxCollateral, TdxTcbInfo,
    evaluate_tdx_tcb, parse_pck_extensions, verify_tdx_collateral,
};
use vesper_aegis::roots::{
    AmdProduct, AmdSigningKey, INTEL_SGX_ROOT_CA, all_pinned, amd_ark, amd_intermediate,
};
use vesper_aegis::snp::{SnpReport, verify_snp_report_with_collateral};
use vesper_aegis::tdx::{TdxQuote, verify_tdx_quote_with_collateral};

mod support;

const NS: u64 = 1_000_000_000;

fn data(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/data/vendor/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2));
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

// ------------------------------------------------------------------ TDX --

/// `dcap-qvl` 0.3.4 `sample/tdx_quote` and its PCCS collateral
/// (`sample/tdx_quote_collateral.json`, Intel's `sgx_ql_qve_collateral_t`
/// fields).
fn dcap_qvl() -> (Vec<u8>, TdxCollateral) {
    let j: serde_json::Value =
        serde_json::from_slice(&data("tdx-dcap-qvl/tdx_quote_collateral.json")).unwrap();
    let s = |k: &str| j[k].as_str().unwrap().to_owned();
    let collateral = TdxCollateral {
        tcb_info: SignedCollateral::new(s("tcb_info"), &s("tcb_info_signature")).unwrap(),
        tcb_info_issuer_chain_pem: s("tcb_info_issuer_chain").into_bytes(),
        qe_identity: SignedCollateral::new(s("qe_identity"), &s("qe_identity_signature")).unwrap(),
        qe_identity_issuer_chain_pem: s("qe_identity_issuer_chain").into_bytes(),
        pck_crl_der: unhex(&s("pck_crl")),
        root_ca_crl_der: unhex(&s("root_ca_crl")),
    };
    (data("tdx-dcap-qvl/tdx_quote"), collateral)
}

/// Inside every validity window of the dcap-qvl collateral
/// (QE identity issued 2025-06-19T10:32:27Z, PCK CRL next update
/// 2025-07-19T10:00:35Z): 2025-06-20T00:00:00Z.
const DCAP_QVL_NOW: u64 = 1_750_377_600 * NS;

/// `go-tdx-guest` v0.3.1 `testing/testdata`: a production SPR E4 quote,
/// PCS responses and DER CRLs; issuer chains from `testing/test_cases.go`.
fn go_tdx_guest() -> (Vec<u8>, TdxCollateral) {
    let collateral = TdxCollateral {
        tcb_info: SignedCollateral::from_pcs_response(
            &data("tdx-go-tdx-guest/sample_tcbInfo_response"),
            "tcbInfo",
        )
        .unwrap(),
        tcb_info_issuer_chain_pem: data("tdx-go-tdx-guest/tcb_info_issuer_chain.pem"),
        qe_identity: SignedCollateral::from_pcs_response(
            &data("tdx-go-tdx-guest/sample_qeIdentity_response"),
            "enclaveIdentity",
        )
        .unwrap(),
        qe_identity_issuer_chain_pem: data("tdx-go-tdx-guest/qe_identity_issuer_chain.pem"),
        pck_crl_der: data("tdx-go-tdx-guest/pckcrl"),
        root_ca_crl_der: data("tdx-go-tdx-guest/rootcrl.der"),
    };
    (
        data("tdx-go-tdx-guest/tdx_prod_quote_SPR_E4.dat"),
        collateral,
    )
}

/// 2023-06-20T00:00:00Z: inside the go-tdx-guest TCB info, QE identity and
/// CRL windows.
const GO_TDX_NOW: u64 = 1_687_219_200 * NS;

fn intel_root() -> vesper_aegis::TrustRoot {
    INTEL_SGX_ROOT_CA.trust_root().unwrap()
}

#[test]
fn pinned_roots_match_their_recorded_digests() {
    let all = all_pinned();
    assert_eq!(all.len(), 10);
    for cert in all {
        cert.der().unwrap();
        cert.trust_root().unwrap();
    }
}

#[test]
fn amd_intermediates_chain_to_their_ark() {
    // ASK and ASVK of every product verify to that product's ARK (AMD's
    // RSA-PSS encoding), and not to another product's.
    for p in AmdProduct::ALL {
        let ark = amd_ark(p).trust_root().unwrap();
        for key in [AmdSigningKey::Vcek, AmdSigningKey::Vlek] {
            let der = amd_intermediate(p, key).der().unwrap();
            verify_issued_by(der, ark.der()).unwrap_or_else(|e| panic!("{p:?} {key:?}: {e:?}"));
        }
        // The ARK is self-signed.
        verify_issued_by(ark.der(), ark.der()).unwrap();
        for other in AmdProduct::ALL.into_iter().filter(|&o| o != p) {
            let der = amd_intermediate(p, AmdSigningKey::Vcek).der().unwrap();
            let wrong = amd_ark(other).der().unwrap();
            assert!(verify_issued_by(der, wrong).is_err());
        }
        // A flipped signature bit.
        let mut der = amd_intermediate(p, AmdSigningKey::Vcek)
            .der()
            .unwrap()
            .to_vec();
        let n = der.len();
        der[n - 3] ^= 1;
        assert!(matches!(
            verify_issued_by(&der, ark.der()),
            Err(AttestError::BadSignature { .. })
        ));
    }
    // go-sev-guest's separate copy of the Milan ASVK + ARK is byte-identical.
    let copy = pem_certificates(&data("snp-go-sev-guest/milanvlek.testcer")).unwrap();
    assert_eq!(
        copy[0],
        amd_intermediate(AmdProduct::Milan, AmdSigningKey::Vlek)
            .der()
            .unwrap()
    );
    assert_eq!(copy[1], amd_ark(AmdProduct::Milan).der().unwrap());
}

#[test]
fn real_tdx_quote_dcap_qvl_verifies_up_to_date() {
    let (quote, collateral) = dcap_qvl();
    let v = verify_tdx_collateral(&quote, &collateral, &intel_root(), DCAP_QVL_NOW).unwrap();
    assert_eq!(v.evaluation.status, TcbStatus::UpToDate);
    assert_eq!(v.evaluation.platform_status, TcbStatus::UpToDate);
    assert_eq!(v.evaluation.qe_status, TcbStatus::UpToDate);
    assert_eq!(v.evaluation.module_status, Some(TcbStatus::UpToDate));
    assert!(v.evaluation.advisory_ids.is_empty());
    assert_eq!(v.evaluation.fmspc, [0xB0, 0xC0, 0x6F, 0, 0, 0]);
    assert_eq!(v.evaluation.tcb_evaluation_data_number, 17);
    assert_eq!(v.pck.fmspc, v.tcb_info.fmspc);
    TcbAcceptance::default().check(v.evaluation.status).unwrap();
}

#[test]
fn real_tdx_quote_go_tdx_guest_verifies() {
    let (quote, collateral) = go_tdx_guest();
    // The published file appends 39 bytes of test text after the quote;
    // that is rejected, and the quote proper verifies.
    assert!(matches!(
        TdxQuote::parse_padded(&quote),
        Err(ParseError::TrailingBytes { extra: 39, .. })
    ));
    let (q, used) = TdxQuote::parse_prefix(&quote).unwrap();
    // Every signature, chain, CRL and collateral check passes (evaluation is
    // the last step), but the quote's TEE_TCB_SVN [3, 0, 4, ..] is below
    // both published TDX levels ([3, 0, 5, ..]): Intel's algorithm gives
    // "TCB level not supported", a typed error here.
    assert_eq!(
        verify_tdx_collateral(&quote[..used], &collateral, &intel_root(), GO_TDX_NOW),
        Err(AttestError::TcbLevelNotFound {
            what: "TDX platform TCB level"
        })
    );
    // The PCK's SGX components [3, 3, 2, 2, 2, 1, 0, 2, ..] are below the
    // levels' [5, 5, 2, 2, 3, 1, 0, 3, ..] as well. Raised to the first
    // level, the same collateral evaluates to UpToDate; with a PCESVN
    // between the levels' 11 and 5, to OutOfDate.
    let certs = pem_certificates(&q.pck_chain_pem).unwrap();
    let mut pck = parse_pck_extensions(&certs[0]).unwrap();
    assert_eq!(pck.fmspc, [0x50, 0x80, 0x6F, 0, 0, 0]);
    assert_eq!(pck.sgx_components[..8], [3, 3, 2, 2, 2, 1, 0, 2]);
    assert_eq!(pck.pce_svn, 11);
    pck.sgx_components[..8].copy_from_slice(&[5, 5, 2, 2, 3, 1, 0, 3]);
    let info = TdxTcbInfo::parse(&collateral.tcb_info.body).unwrap();
    let qe = QeIdentity::parse(&collateral.qe_identity.body).unwrap();
    let mut patched = q.clone();
    patched.body.tee_tcb_svn[2] = 5;
    let e = evaluate_tdx_tcb(&patched, &pck, &info, &qe).unwrap();
    assert_eq!(e.status, TcbStatus::UpToDate);
    assert_eq!(e.module_status, None);
    pck.pce_svn = 7;
    let e = evaluate_tdx_tcb(&patched, &pck, &info, &qe).unwrap();
    assert_eq!(e.status, TcbStatus::OutOfDate);
    // TDX component 1 (module major) must equal the level's when the TCB
    // info has no module identities.
    patched.body.tee_tcb_svn[1] = 1;
    assert!(matches!(
        evaluate_tdx_tcb(&patched, &pck, &info, &qe),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // A wrong-root and an expired-collateral check on this sample too.
    assert!(
        verify_tdx_collateral(&quote[..used], &collateral, &intel_root(), DCAP_QVL_NOW).is_err()
    );
}

#[test]
fn real_tdx_quote_is_not_a_vesper_quote() {
    // Signature, chain and collateral are fine, but report data is not the
    // Vesper binding of these runtime claims.
    let (quote, collateral) = dcap_qvl();
    let err = verify_tdx_quote_with_collateral(
        &quote,
        &collateral,
        &intel_root(),
        DCAP_QVL_NOW,
        &support::runtime(),
    )
    .unwrap_err();
    assert!(
        matches!(err, AttestError::BindingMismatch { .. }),
        "{err:?}"
    );
}

#[test]
fn real_tdx_collateral_outside_its_window_is_rejected() {
    let (quote, collateral) = dcap_qvl();
    let root = intel_root();
    // After the PCK CRL's nextUpdate (2025-07-19T10:00:35Z).
    let late = 1_752_969_600 * NS; // 2025-07-20
    assert!(matches!(
        verify_tdx_collateral(&quote, &collateral, &root, late),
        Err(AttestError::CollateralExpired { .. })
    ));
    // Before the TCB info was issued (2025-06-19T10:16:03Z) but after the
    // CRLs: the collateral is not valid yet.
    let early = 1_750_327_200 * NS; // 2025-06-19T10:00:00Z
    let err = verify_tdx_collateral(&quote, &collateral, &root, early).unwrap_err();
    assert!(
        matches!(
            err,
            AttestError::CollateralExpired { .. }
                | AttestError::Crl(_)
                | AttestError::CertificateChain(_)
        ),
        "{err:?}"
    );
}

#[test]
fn tampered_real_tdx_inputs_are_rejected() {
    let (quote, collateral) = dcap_qvl();
    let root = intel_root();
    // A flipped bit in the TD report body.
    let mut q = quote.clone();
    q[48 + 100] ^= 1;
    assert!(matches!(
        verify_tdx_collateral(&q, &collateral, &root, DCAP_QVL_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // TCB info edited to claim a different status: its signature fails.
    let mut c = collateral.clone();
    let body = String::from_utf8(c.tcb_info.body.clone()).unwrap();
    c.tcb_info.body = body
        .replacen("\"OutOfDate\"", "\"UpToDate\"", 1)
        .into_bytes();
    assert!(matches!(
        verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // QE identity signature flipped.
    let mut c = collateral.clone();
    c.qe_identity.signature[10] ^= 0x80;
    assert!(matches!(
        verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // A CRL whose signature does not verify.
    let mut c = collateral.clone();
    let n = c.pck_crl_der.len();
    c.pck_crl_der[n - 5] ^= 1;
    assert!(verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW).is_err());
    // Only the root CA CRL: the PCK's revocation status is unknown.
    let mut c = collateral.clone();
    c.pck_crl_der = c.root_ca_crl_der.clone();
    assert!(matches!(
        verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW),
        Err(AttestError::MissingCollateral { .. })
    ));
    // The go-tdx-guest collateral is for another FMSPC (and expired then).
    let (_, other) = go_tdx_guest();
    assert!(verify_tdx_collateral(&quote, &other, &root, DCAP_QVL_NOW).is_err());
    // Wrong root: the AMD Milan ARK.
    let ark = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    assert!(matches!(
        verify_tdx_collateral(&quote, &collateral, &ark, DCAP_QVL_NOW),
        Err(AttestError::CertificateChain(_))
    ));
}

#[test]
fn tdx_tcb_status_from_real_tcb_info() {
    let (quote, collateral) = dcap_qvl();
    let v = verify_tdx_collateral(&quote, &collateral, &intel_root(), DCAP_QVL_NOW).unwrap();
    let info = TdxTcbInfo::parse(&collateral.tcb_info.body).unwrap();
    let qe = QeIdentity::parse(&collateral.qe_identity.body).unwrap();

    // A PCESVN between the two published levels lands on OutOfDate, with
    // Intel's advisories for that level.
    let mut pck = v.pck.clone();
    pck.pce_svn = 10;
    let e = evaluate_tdx_tcb(&v.quote, &pck, &info, &qe).unwrap();
    assert_eq!(e.status, TcbStatus::OutOfDate);
    assert!(e.advisory_ids.contains(&"INTEL-SA-00837".to_owned()));
    assert_eq!(
        TcbAcceptance::default().check(e.status),
        Err(vesper_aegis::AegisVerifyError::TcbStatusNotAccepted(
            TcbStatus::OutOfDate
        ))
    );
    // Below every level: no TCB level matches.
    pck.pce_svn = 1;
    assert!(matches!(
        evaluate_tdx_tcb(&v.quote, &pck, &info, &qe),
        Err(AttestError::TcbLevelNotFound { .. })
    ));

    // TDX module 1.x with minor SVN 2 hits TDX_01's OutOfDate level, which
    // converges an UpToDate platform to OutOfDate.
    let mut q = v.quote.clone();
    q.body.tee_tcb_svn[1] = 1;
    q.body.tee_tcb_svn[0] = 2;
    let e = evaluate_tdx_tcb(&q, &v.pck, &info, &qe).unwrap();
    assert_eq!(e.platform_status, TcbStatus::UpToDate);
    assert_eq!(e.module_status, Some(TcbStatus::OutOfDate));
    assert_eq!(e.status, TcbStatus::OutOfDate);
    // A module major version the TCB info does not list.
    q.body.tee_tcb_svn[1] = 9;
    assert!(matches!(
        evaluate_tdx_tcb(&q, &v.pck, &info, &qe),
        Err(AttestError::TcbLevelNotFound { .. })
    ));

    // A QE with a lower ISVSVN than any QE level, or another MRSIGNER.
    let mut q = v.quote.clone();
    q.qe_report.isv_svn = 0;
    assert!(matches!(
        evaluate_tdx_tcb(&q, &v.pck, &info, &qe),
        Err(AttestError::TcbLevelNotFound { .. })
    ));
    let mut q = v.quote.clone();
    q.qe_report.mr_signer[0] ^= 1;
    assert!(matches!(
        evaluate_tdx_tcb(&q, &v.pck, &info, &qe),
        Err(AttestError::CollateralMismatch { .. })
    ));
}

#[test]
fn tdx_tcb_status_variants_from_edited_levels() {
    // The published TCB info only has UpToDate and OutOfDate levels; the
    // other statuses are evaluated on copies whose first level's status is
    // edited (unsigned: evaluation only).
    let (quote, collateral) = dcap_qvl();
    let v = verify_tdx_collateral(&quote, &collateral, &intel_root(), DCAP_QVL_NOW).unwrap();
    let qe = QeIdentity::parse(&collateral.qe_identity.body).unwrap();
    let body = String::from_utf8(collateral.tcb_info.body.clone()).unwrap();
    let first_level = body
        .find("\"tcbLevels\":[{\"tcb\":{\"sgxtcbcomponents\"")
        .unwrap();
    for status in [
        TcbStatus::SWHardeningNeeded,
        TcbStatus::ConfigurationNeeded,
        TcbStatus::ConfigurationAndSWHardeningNeeded,
        TcbStatus::OutOfDateConfigurationNeeded,
        TcbStatus::Revoked,
    ] {
        let (head, tail) = body.split_at(first_level);
        let edited = format!(
            "{head}{}",
            tail.replacen(
                "\"tcbStatus\":\"UpToDate\"",
                &format!("\"tcbStatus\":\"{}\"", status.as_str()),
                1
            )
        );
        let info = TdxTcbInfo::parse(edited.as_bytes()).unwrap();
        let e = evaluate_tdx_tcb(&v.quote, &v.pck, &info, &qe).unwrap();
        assert_eq!(e.status, status);
        let lenient = TcbAcceptance {
            accept: vec![
                TcbStatus::UpToDate,
                TcbStatus::SWHardeningNeeded,
                TcbStatus::ConfigurationNeeded,
                TcbStatus::ConfigurationAndSWHardeningNeeded,
                TcbStatus::OutOfDate,
                TcbStatus::OutOfDateConfigurationNeeded,
                TcbStatus::Revoked,
            ],
        };
        assert_eq!(lenient.check(status).is_ok(), status != TcbStatus::Revoked);
    }
    // Unknown statuses, duplicate keys and wrong ids are typed errors.
    let unknown = body.replacen("\"UpToDate\"", "\"Fine\"", 1);
    assert!(matches!(
        TdxTcbInfo::parse(unknown.as_bytes()),
        Err(AttestError::Collateral { .. })
    ));
    let dup = body.replacen("{\"id\":\"TDX\",", "{\"id\":\"TDX\",\"id\":\"TDX\",", 1);
    assert!(matches!(
        TdxTcbInfo::parse(dup.as_bytes()),
        Err(AttestError::Collateral { .. })
    ));
    let sgx = body.replacen("{\"id\":\"TDX\",", "{\"id\":\"SGX\",", 1);
    assert!(matches!(
        TdxTcbInfo::parse(sgx.as_bytes()),
        Err(AttestError::Collateral { .. })
    ));
}

#[test]
fn real_pck_certificate_extension() {
    let (quote, _) = dcap_qvl();
    let q = TdxQuote::parse_padded(&quote).unwrap();
    let certs = pem_certificates(&q.pck_chain_pem).unwrap();
    let ext = parse_pck_extensions(&certs[0]).unwrap();
    assert_eq!(ext.fmspc, [0xB0, 0xC0, 0x6F, 0, 0, 0]);
    assert_eq!(ext.pce_id, [0, 0]);
    // The intermediate CA has no SGX extension.
    assert!(matches!(
        parse_pck_extensions(&certs[1]),
        Err(AttestError::Collateral { .. })
    ));
    // Revocation against the real CRLs alone (chain-level API).
    let (_, c) = dcap_qvl();
    verify_chain_with_crls(
        &certs[0],
        &certs[1..],
        &intel_root(),
        DCAP_QVL_NOW,
        Curve::P256,
        &[&c.pck_crl_der, &c.root_ca_crl_der],
        RevocationScope::EveryCertificate,
    )
    .unwrap();
}

// ------------------------------------------------------------- SEV-SNP --

/// `sev` 6.0.0 `tests/certs_data`: a Milan v2 report and its VCEK.
fn virtee_milan() -> (Vec<u8>, Vec<u8>) {
    let hex = String::from_utf8(data("snp-virtee-sev/report_milan.hex")).unwrap();
    (unhex(&hex), data("snp-virtee-sev/vcek_milan.der"))
}

/// `go-sev-guest` v0.15.0 `verify/testdata`: a Milan report and its VCEK.
fn go_sev_guest() -> (Vec<u8>, Vec<u8>) {
    (
        data("snp-go-sev-guest/attestation.bin"),
        data("snp-go-sev-guest/vcek.testcer"),
    )
}

/// 2025-06-20; both VCEKs are valid 2023..2030 / 2022..2029.
const SNP_NOW: u64 = DCAP_QVL_NOW;

fn vcek(leaf: Vec<u8>) -> SnpCollateral {
    SnpCollateral {
        product: AmdProduct::Milan,
        signing_key: AmdSigningKey::Vcek,
        leaf_der: leaf,
        intermediate_der: None,
        crl_der: None,
    }
}

#[test]
fn real_snp_reports_verify_to_the_milan_ark() {
    let ark = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    for (name, (report, leaf)) in [("virtee", virtee_milan()), ("go-sev-guest", go_sev_guest())] {
        let v = verify_snp_collateral(&report, &vcek(leaf.clone()), &ark, SNP_NOW, None)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(v.signing_key, AmdSigningKey::Vcek);
        assert!(v.certificate.product_name.starts_with("Milan"));
        assert_eq!(v.certificate.tcb, v.reported_tcb);
        assert_eq!(v.certificate.hw_id.as_deref(), Some(&v.report.chip_id[..]));
        assert!(!v.revocation_checked);
        // The chain supplied explicitly (sev's cert_chain_milan is ASK + ARK).
        if name == "virtee" {
            let chain = pem_certificates(&data("snp-virtee-sev/cert_chain_milan")).unwrap();
            let mut c = vcek(leaf.clone());
            c.intermediate_der = Some(chain[0].clone());
            verify_snp_collateral(&report, &c, &ark, SNP_NOW, None).unwrap();
            assert_eq!(chain[1], amd_ark(AmdProduct::Milan).der().unwrap());
        }
        // TCB status against a relying-party baseline.
        let at = SnpTcbBaseline {
            latest: v.reported_tcb,
        };
        let v2 =
            verify_snp_collateral(&report, &vcek(leaf.clone()), &ark, SNP_NOW, Some(&at)).unwrap();
        assert_eq!(v2.status, Some(TcbStatus::UpToDate));
        let newer = SnpTcbBaseline {
            latest: AmdTcb {
                snp: v.reported_tcb.snp + 1,
                ..v.reported_tcb
            },
        };
        let v3 = verify_snp_collateral(&report, &vcek(leaf.clone()), &ark, SNP_NOW, Some(&newer))
            .unwrap();
        assert_eq!(v3.status, Some(TcbStatus::OutOfDate));
        // Not a Vesper report.
        assert!(matches!(
            verify_snp_report_with_collateral(
                &report,
                &vcek(leaf),
                &ark,
                SNP_NOW,
                None,
                &support::runtime()
            ),
            Err(AttestError::BindingMismatch { .. })
        ));
        eprintln!("{name}: v{} tcb {:?}", v.report.version, v.reported_tcb);
    }
}

#[test]
fn tampered_real_snp_inputs_are_rejected() {
    let ark = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    let (report, leaf) = virtee_milan();
    let (other_report, other_leaf) = go_sev_guest();
    // Flipped measurement bit.
    let mut r = report.clone();
    r[0x90] ^= 1;
    assert!(matches!(
        verify_snp_collateral(&r, &vcek(leaf.clone()), &ark, SNP_NOW, None),
        Err(AttestError::BadSignature { .. })
    ));
    // Another chip's VCEK: hwID or TCB binding fails before the signature.
    assert!(matches!(
        verify_snp_collateral(&report, &vcek(other_leaf), &ark, SNP_NOW, None),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // Reported TCB rewritten (a downgrade claim) no longer matches the VCEK.
    let mut r = report.clone();
    r[0x180] = r[0x180].wrapping_sub(1);
    assert!(matches!(
        verify_snp_collateral(&r, &vcek(leaf.clone()), &ark, SNP_NOW, None),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // Wrong product's ARK and ASK.
    let genoa = amd_ark(AmdProduct::Genoa).trust_root().unwrap();
    assert!(verify_snp_collateral(&report, &vcek(leaf.clone()), &genoa, SNP_NOW, None).is_err());
    let mut c = vcek(leaf.clone());
    c.product = AmdProduct::Genoa;
    assert!(verify_snp_collateral(&report, &c, &genoa, SNP_NOW, None).is_err());
    // The Intel root.
    assert!(
        verify_snp_collateral(&report, &vcek(leaf.clone()), &intel_root(), SNP_NOW, None).is_err()
    );
    // VLEK collateral for a VCEK-signed report.
    let mut c = vcek(leaf.clone());
    c.signing_key = AmdSigningKey::Vlek;
    assert!(matches!(
        verify_snp_collateral(&report, &c, &ark, SNP_NOW, None),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // VCEK expired (go-sev-guest's VCEK ends 2029-09-24).
    let late = 1_900_000_000 * NS;
    assert!(matches!(
        verify_snp_collateral(&other_report, &vcek(go_sev_guest().1), &ark, late, None),
        Err(AttestError::CertificateExpired)
    ));
    // The VCEK is a leaf: it does not verify as an intermediate for itself.
    assert!(verify_chain(&leaf, &[], &ark, SNP_NOW, Curve::P384).is_err());
    let _ = SnpReport::parse(&other_report).unwrap();
}

// ------------------------------------------------------- fuzz on reals --

fn flip(rng: &mut support::SplitMix, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    match rng.below(4) {
        0 => v.truncate(rng.below(v.len() as u64) as usize),
        1 => {
            for _ in 0..=rng.below(4) {
                let i = rng.below(v.len() as u64) as usize;
                v[i] ^= 1 << rng.below(8);
            }
        }
        2 => {
            let i = rng.below(v.len() as u64) as usize;
            v[i] = rng.next() as u8;
        }
        _ => {
            let i = rng.below(v.len() as u64) as usize;
            let extra: Vec<u8> = (0..1 + rng.below(16)).map(|_| rng.next() as u8).collect();
            v.splice(i..i, extra);
        }
    }
    v
}

#[test]
fn mutated_real_evidence_never_panics_and_never_verifies() {
    let mut rng = support::SplitMix(0x005E_EDA1);
    let root = intel_root();
    let (quote, collateral) = dcap_qvl();
    let (_, used) = TdxQuote::parse_prefix(&quote).unwrap();
    for _ in 0..400 {
        let m = flip(&mut rng, &quote);
        let changed = m.len() < used || m[..used] != quote[..used];
        let r = verify_tdx_collateral(&m, &collateral, &root, DCAP_QVL_NOW);
        if changed {
            // The only bytes not covered by a signature are the PEM framing
            // of the PCK chain, which authenticates itself: a mutation may
            // only be accepted if it decodes to the same certificates.
            if let Ok(v) = r {
                assert_eq!(
                    pem_certificates(&v.quote.pck_chain_pem).unwrap(),
                    pem_certificates(&TdxQuote::parse_padded(&quote).unwrap().pck_chain_pem)
                        .unwrap(),
                    "mutated quote verified with different content"
                );
            }
        }
        let mut c = collateral.clone();
        c.tcb_info.body = flip(&mut rng, &collateral.tcb_info.body);
        let _ = TdxTcbInfo::parse(&c.tcb_info.body);
        if c.tcb_info.body != collateral.tcb_info.body {
            assert!(verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW).is_err());
        }
        let mut c = collateral.clone();
        c.qe_identity.body = flip(&mut rng, &collateral.qe_identity.body);
        let _ = QeIdentity::parse(&c.qe_identity.body);
        let mut c = collateral.clone();
        c.pck_crl_der = flip(&mut rng, &collateral.pck_crl_der);
        let _ = verify_tdx_collateral(&quote, &c, &root, DCAP_QVL_NOW);
    }
    let q = TdxQuote::parse_padded(&quote).unwrap();
    let pck = pem_certificates(&q.pck_chain_pem).unwrap().remove(0);
    let ark = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    let (report, vcek_der) = virtee_milan();
    for _ in 0..400 {
        let _ = parse_pck_extensions(&flip(&mut rng, &pck));
        let m = flip(&mut rng, &vcek_der);
        let _ = vesper_aegis::amd::parse_amd_cert_extensions(&m, AmdProduct::Milan);
        if m != vcek_der {
            assert!(verify_snp_collateral(&report, &vcek(m), &ark, SNP_NOW, None).is_err());
        }
        let r = flip(&mut rng, &report);
        if r != report {
            assert!(
                verify_snp_collateral(&r, &vcek(vcek_der.clone()), &ark, SNP_NOW, None).is_err()
            );
        }
        let _ = verify_issued_by(
            &flip(
                &mut rng,
                amd_intermediate(AmdProduct::Milan, AmdSigningKey::Vcek)
                    .der()
                    .unwrap(),
            ),
            ark.der(),
        );
    }
}
