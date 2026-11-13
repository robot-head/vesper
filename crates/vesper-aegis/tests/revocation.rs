//! Revocation and collateral negative cases with generated chains and
//! CRLs (rcgen). Real-sample positives are in `vendor_samples.rs`; these
//! cover what no published vendor sample shows: a revoked PCK, a revoked
//! ASK, stale and forged CRLs, and VCEK/report TCB disagreement.

use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, CustomExtension, DnType,
    IsCa, Issuer, KeyIdMethod, KeyPair, RevokedCertParams, SerialNumber, date_time_ymd,
};
use vesper_aegis::amd::{AmdTcb, SnpCollateral, SnpTcbBaseline, verify_snp_collateral};
use vesper_aegis::chain::{Curve, RevocationScope, verify_chain_with_crls};
use vesper_aegis::error::AttestError;
use vesper_aegis::intel::{SignedCollateral, TcbStatus, TdxCollateral, verify_tdx_collateral};
use vesper_aegis::roots::{AmdProduct, AmdSigningKey};
use vesper_aegis::{TrustRoot, tdx::TdxQuote};

mod support;
use support::{NOW_NS, Signer, SnpParams, TdxParams};

struct Ca {
    issuer: Issuer<'static, KeyPair>,
    der: Vec<u8>,
    pem: String,
}

fn key(curve: Curve) -> KeyPair {
    let alg = match curve {
        Curve::P256 => &rcgen::PKCS_ECDSA_P256_SHA256,
        Curve::P384 => &rcgen::PKCS_ECDSA_P384_SHA384,
    };
    KeyPair::generate_for(alg).unwrap()
}

fn ca_params(cn: &str, serial: u64) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, cn);
    p.serial_number = Some(SerialNumber::from(serial));
    p
}

fn root(curve: Curve, cn: &str) -> Ca {
    let params = ca_params(cn, 1);
    let k = key(curve);
    let cert = params.self_signed(&k).unwrap();
    Ca {
        der: cert.der().to_vec(),
        pem: cert.pem(),
        issuer: Issuer::new(params, k),
    }
}

fn sub_ca(parent: &Ca, curve: Curve, cn: &str, serial: u64) -> Ca {
    let params = ca_params(cn, serial);
    let k = key(curve);
    let cert = params.signed_by(&k, &parent.issuer).unwrap();
    Ca {
        der: cert.der().to_vec(),
        pem: cert.pem(),
        issuer: Issuer::new(params, k),
    }
}

fn leaf(parent: &Ca, signer: &Signer, serial: u64, ext: Vec<CustomExtension>) -> (Vec<u8>, String) {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name
        .push(DnType::CommonName, "Aegis Test Leaf");
    p.serial_number = Some(SerialNumber::from(serial));
    p.custom_extensions = ext;
    let alg = match signer.curve {
        Curve::P256 => &rcgen::PKCS_ECDSA_P256_SHA256,
        Curve::P384 => &rcgen::PKCS_ECDSA_P384_SHA384,
    };
    let k = KeyPair::from_pkcs8_der_and_sign_algo(&signer.pkcs8.as_slice().into(), alg).unwrap();
    let cert = p.signed_by(&k, &parent.issuer).unwrap();
    (cert.der().to_vec(), cert.pem())
}

/// A CRL by `ca` listing `revoked` serials; valid 2025-01-01..`next`.
fn crl(ca: &Ca, revoked: &[u64], next: (i32, u8, u8)) -> Vec<u8> {
    CertificateRevocationListParams {
        this_update: date_time_ymd(2025, 1, 1),
        next_update: date_time_ymd(next.0, next.1, next.2),
        crl_number: SerialNumber::from(7u64),
        issuing_distribution_point: None,
        revoked_certs: revoked
            .iter()
            .map(|&s| RevokedCertParams {
                serial_number: SerialNumber::from(s),
                revocation_time: date_time_ymd(2025, 6, 1),
                reason_code: None,
                invalidity_date: None,
            })
            .collect(),
        key_identifier_method: KeyIdMethod::Sha256,
    }
    .signed_by(&ca.issuer)
    .unwrap()
    .der()
    .to_vec()
}

const CURRENT: (i32, u8, u8) = (2026, 6, 1);
const STALE: (i32, u8, u8) = (2025, 12, 1);

fn trust(ca: &Ca) -> TrustRoot {
    TrustRoot::from_der(ca.der.clone()).unwrap()
}

#[test]
fn intel_shaped_chain_revocation() {
    let r = root(Curve::P256, "Test SGX Root CA");
    let pck_ca = sub_ca(&r, Curve::P256, "Test SGX PCK Platform CA", 20);
    let pck = Signer::generate(Curve::P256);
    let (leaf_der, _) = leaf(&pck_ca, &pck, 300, vec![]);
    let inter = vec![pck_ca.der.clone()];
    let check = |crls: &[&[u8]]| {
        verify_chain_with_crls(
            &leaf_der,
            &inter,
            &trust(&r),
            NOW_NS,
            Curve::P256,
            crls,
            RevocationScope::EveryCertificate,
        )
    };
    let root_ok = crl(&r, &[], CURRENT);
    let pck_ok = crl(&pck_ca, &[301, 302], CURRENT);
    check(&[&pck_ok, &root_ok]).unwrap();
    // The PCK itself is revoked.
    let pck_revoked = crl(&pck_ca, &[299, 300], CURRENT);
    assert!(matches!(
        check(&[&pck_revoked, &root_ok]),
        Err(AttestError::Revoked { .. })
    ));
    // The PCK CA is revoked by the root.
    let root_revoked = crl(&r, &[20], CURRENT);
    assert!(matches!(
        check(&[&pck_ok, &root_revoked]),
        Err(AttestError::Revoked { .. })
    ));
    // A stale CRL.
    let pck_stale = crl(&pck_ca, &[], STALE);
    assert!(matches!(
        check(&[&pck_stale, &root_ok]),
        Err(AttestError::CollateralExpired { .. })
    ));
    // Missing coverage: no PCK CRL, or no CRLs at all.
    assert!(matches!(
        check(&[&root_ok]),
        Err(AttestError::MissingCollateral { .. })
    ));
    assert!(matches!(
        check(&[]),
        Err(AttestError::MissingCollateral { .. })
    ));
    // A CRL with the PCK CA's name but another key ("forged"): not accepted.
    let impostor = sub_ca(&r, Curve::P256, "Test SGX PCK Platform CA", 21);
    let forged = crl(&impostor, &[], CURRENT);
    assert!(check(&[&forged, &root_ok]).is_err());
    // Garbage CRL bytes.
    assert!(matches!(
        check(&[b"not a crl", &root_ok]),
        Err(AttestError::Crl(_))
    ));
}

#[test]
fn revoked_pck_fails_tdx_verification_end_to_end() {
    let r = root(Curve::P256, "Test SGX Root CA");
    let pck_ca = sub_ca(&r, Curve::P256, "Test SGX PCK Platform CA", 20);
    let pck = Signer::generate(Curve::P256);
    let (_, leaf_pem) = leaf(&pck_ca, &pck, 300, vec![]);
    let pem = format!("{leaf_pem}{}{}", pck_ca.pem, r.pem);
    let chain = support::chain(Curve::P256, Curve::P256, false);
    let ak = Signer::generate(Curve::P256);
    let mut q: TdxQuote = support::tdx_quote_struct(&chain, &ak, &TdxParams::default());
    q.pck_chain_pem = pem.into_bytes();
    q.qe_report_signature = pck.sign(&q.qe_report.encode()).try_into().unwrap();
    let quote = q.encode();
    let unused = SignedCollateral::new(b"{}".to_vec(), &"00".repeat(64)).unwrap();
    let collateral = |pck_crl: Vec<u8>| TdxCollateral {
        tcb_info: unused.clone(),
        tcb_info_issuer_chain_pem: r.pem.clone().into_bytes(),
        qe_identity: unused.clone(),
        qe_identity_issuer_chain_pem: r.pem.clone().into_bytes(),
        pck_crl_der: pck_crl,
        root_ca_crl_der: crl(&r, &[], CURRENT),
    };
    assert!(matches!(
        verify_tdx_collateral(
            &quote,
            &collateral(crl(&pck_ca, &[300], CURRENT)),
            &trust(&r),
            NOW_NS
        ),
        Err(AttestError::Revoked { .. })
    ));
    // Not revoked: the chain and quote signatures pass and verification
    // moves on to the (here unusable) TCB info, failing there instead.
    let err = verify_tdx_collateral(
        &quote,
        &collateral(crl(&pck_ca, &[], CURRENT)),
        &trust(&r),
        NOW_NS,
    )
    .unwrap_err();
    assert!(!matches!(err, AttestError::Revoked { .. }), "{err:?}");
    // Tampered quote signature under a valid chain.
    let mut bad = q.clone();
    bad.quote_signature[3] ^= 1;
    assert!(matches!(
        verify_tdx_collateral(
            &bad.encode(),
            &collateral(crl(&pck_ca, &[], CURRENT)),
            &trust(&r),
            NOW_NS
        ),
        Err(AttestError::BadSignature { .. })
    ));
}

const AMD: [u64; 6] = [1, 3, 6, 1, 4, 1];

fn amd_oid(rest: &[u64]) -> Vec<u64> {
    let mut v = AMD.to_vec();
    v.extend_from_slice(&[3704, 1]);
    v.extend_from_slice(rest);
    v
}

fn int(v: u8) -> Vec<u8> {
    if v >= 0x80 {
        vec![2, 2, 0, v]
    } else {
        vec![2, 1, v]
    }
}

fn vcek_extensions(tcb: AmdTcb, hw_id: &[u8]) -> Vec<CustomExtension> {
    let mut name = vec![0x16, 8];
    name.extend_from_slice(b"Milan-B0");
    vec![
        CustomExtension::from_oid_content(&amd_oid(&[1]), int(1)),
        CustomExtension::from_oid_content(&amd_oid(&[2]), name),
        CustomExtension::from_oid_content(&amd_oid(&[3, 1]), int(tcb.bootloader)),
        CustomExtension::from_oid_content(&amd_oid(&[3, 2]), int(tcb.tee)),
        CustomExtension::from_oid_content(&amd_oid(&[3, 3]), int(tcb.snp)),
        CustomExtension::from_oid_content(&amd_oid(&[3, 8]), int(tcb.microcode)),
        CustomExtension::from_oid_content(&amd_oid(&[4]), hw_id.to_vec()),
    ]
}

#[test]
fn amd_shaped_revocation_and_binding() {
    let ark = root(Curve::P384, "ARK-Milan");
    let ask = sub_ca(&ark, Curve::P384, "SEV-Milan", 2);
    let vcek = Signer::generate(Curve::P384);
    let params = SnpParams::default();
    let tcb = AmdTcb::from_raw(AmdProduct::Milan, params.reported_tcb).unwrap();
    let (vcek_der, _) = leaf(&ask, &vcek, 0x55, vcek_extensions(tcb, &[0xC1; 64]));
    let report = support::snp_report(&vcek, &params);
    let collateral = |crl_der: Option<Vec<u8>>, leaf: Vec<u8>| SnpCollateral {
        product: AmdProduct::Milan,
        signing_key: AmdSigningKey::Vcek,
        leaf_der: leaf,
        intermediate_der: Some(ask.der.clone()),
        crl_der,
    };
    let root = trust(&ark);
    let baseline = SnpTcbBaseline { latest: tcb };

    let v = verify_snp_collateral(
        &report,
        &collateral(Some(crl(&ark, &[], CURRENT)), vcek_der.clone()),
        &root,
        NOW_NS,
        Some(&baseline),
    )
    .unwrap();
    assert!(v.revocation_checked);
    assert_eq!(v.status, Some(TcbStatus::UpToDate));

    // The ARK's CRL revokes the ASK.
    assert!(matches!(
        verify_snp_collateral(
            &report,
            &collateral(Some(crl(&ark, &[2], CURRENT)), vcek_der.clone()),
            &root,
            NOW_NS,
            None
        ),
        Err(AttestError::Revoked { .. })
    ));
    // A stale ARK CRL.
    assert!(matches!(
        verify_snp_collateral(
            &report,
            &collateral(Some(crl(&ark, &[], STALE)), vcek_der.clone()),
            &root,
            NOW_NS,
            None
        ),
        Err(AttestError::CollateralExpired { .. })
    ));
    // A CRL from the ASK instead of the ARK does not count.
    assert!(matches!(
        verify_snp_collateral(
            &report,
            &collateral(Some(crl(&ask, &[], CURRENT)), vcek_der.clone()),
            &root,
            NOW_NS,
            None
        ),
        Err(AttestError::MissingCollateral { .. })
    ));

    // TCB downgrade: the report claims an older SNP firmware than the VCEK
    // was issued for (re-signed by the VCEK key, so only the binding fails).
    let older = SnpParams {
        reported_tcb: AmdTcb {
            snp: tcb.snp - 1,
            ..tcb
        }
        .to_raw(AmdProduct::Milan),
        ..SnpParams::default()
    };
    let downgraded = support::snp_report(&vcek, &older);
    assert!(matches!(
        verify_snp_collateral(
            &downgraded,
            &collateral(None, vcek_der.clone()),
            &root,
            NOW_NS,
            None
        ),
        Err(AttestError::CollateralMismatch {
            what: "certificate TCB vs REPORTED_TCB"
        })
    ));
    // A VCEK issued for the older TCB matches, and the baseline calls it
    // OutOfDate.
    let older_tcb = AmdTcb {
        snp: tcb.snp - 1,
        ..tcb
    };
    let (old_vcek, _) = leaf(&ask, &vcek, 0x56, vcek_extensions(older_tcb, &[0xC1; 64]));
    let v = verify_snp_collateral(
        &downgraded,
        &collateral(None, old_vcek),
        &root,
        NOW_NS,
        Some(&baseline),
    )
    .unwrap();
    assert_eq!(v.status, Some(TcbStatus::OutOfDate));

    // Another chip's VCEK.
    let (other_chip, _) = leaf(&ask, &vcek, 0x57, vcek_extensions(tcb, &[0xC2; 64]));
    assert!(matches!(
        verify_snp_collateral(&report, &collateral(None, other_chip), &root, NOW_NS, None),
        Err(AttestError::CollateralMismatch {
            what: "VCEK hwID vs CHIP_ID"
        })
    ));
    // Malformed extensions: duplicated SPL, missing product name, hwID length.
    let mut dup = vcek_extensions(tcb, &[0xC1; 64]);
    dup.push(CustomExtension::from_oid_content(&amd_oid(&[3, 9]), int(1)));
    let (bad, _) = leaf(&ask, &vcek, 0x58, dup);
    assert!(matches!(
        verify_snp_collateral(&report, &collateral(None, bad), &root, NOW_NS, None),
        Err(AttestError::Collateral { .. })
    ));
    let (bad, _) = leaf(&ask, &vcek, 0x59, vcek_extensions(tcb, &[0xC1; 63]));
    assert!(matches!(
        verify_snp_collateral(&report, &collateral(None, bad), &root, NOW_NS, None),
        Err(AttestError::Collateral { .. })
    ));
    // Tampered report signature.
    let mut r = report.clone();
    r[0x2A0 + 5] ^= 1;
    assert!(matches!(
        verify_snp_collateral(&r, &collateral(None, vcek_der), &root, NOW_NS, None),
        Err(AttestError::BadSignature { .. })
    ));
}
