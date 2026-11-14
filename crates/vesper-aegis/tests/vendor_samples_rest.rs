//! Wave-seven vendor evidence: real Genoa and Turin SEV-SNP version-5
//! reports, a real VLEK-signed report, TDX quote v5 (TD 1.0, 1.5 and
//! 1.5-ex bodies) with Intel collateral, Platform CA (platform-instance)
//! PCK certificates, and Arm CCA tokens against their published reference
//! keys. Provenance of every file is in `tests/data/vendor/README.md`.

use vesper_aegis::amd::{
    AmdTcb, SnpCollateral, check_snp_crl_floor, parse_amd_cert_extensions, verify_snp_collateral,
};
use vesper_aegis::cca::{
    CcaCollection, CcaEndorsement, CcaPlatformProfile, CcaRealmProfile, CcaToken,
    cca_vesper_claims, verify_cca_endorsed,
};
use vesper_aegis::chain::{Curve, EcPublicKey, pem_certificates};
use vesper_aegis::error::{AttestError, Format, ParseError};
use vesper_aegis::intel::{
    PckIssuer, SignedCollateral, TcbStatus, TdxCollateral, TdxCollateralFloor,
    check_tdx_collateral_floor, parse_pck_extensions, verify_tdx_collateral,
};
use vesper_aegis::roots::{AmdProduct, AmdSigningKey, INTEL_SGX_ROOT_CA, amd_ark};
use vesper_aegis::snp::SnpReport;
use vesper_aegis::tdx::{
    TDX_BODY_TYPE_TD10, TDX_BODY_TYPE_TD15, TDX_BODY_TYPE_TD15_EX, TdxQuote, verify_tdx_quote,
};

mod support;

const NS: u64 = 1_000_000_000;
/// 2026-01-01T00:00:00Z: inside the Oak Genoa/Turin/Milan VCEK windows
/// (issued 2025-01..08, valid seven years).
const OAK_NOW: u64 = 1_767_225_600 * NS;
/// 2025-06-01T00:00:00Z: inside the AWS VLEK window (2024-12-10 to
/// 2025-12-10).
const VLEK_NOW: u64 = 1_748_736_000 * NS;
/// 2026-10-15T00:00:00Z: inside every window of the dcap-qvl 0.7.1
/// `tdx_quote_td15ex` collateral (PCK CRL 2026-10-08T00:28:26Z, QE identity
/// next update 2026-11-06T23:45:11Z).
const TD15EX_NOW: u64 = 1_792_022_400 * NS;
/// 2026-03-01T00:00:00Z: inside the `tdx_quote_outdated` collateral windows
/// (2026-02-18T10:58:51Z to 2026-03-20T10:42:15Z).
const OUTDATED_NOW: u64 = 1_772_323_200 * NS;
/// 2025-06-20T00:00:00Z: the dcap-qvl 0.3.4 collateral window.
const DCAP_QVL_034_NOW: u64 = 1_750_377_600 * NS;

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

fn b64url(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = A.iter().position(|&a| a == c).expect("base64url") as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

// --------------------------------------------------------------- SNP --

fn oak(product: &str) -> (Vec<u8>, Vec<u8>) {
    (
        data(&format!("snp-oak/{product}_report.bin")),
        data(&format!("snp-oak/{product}_vcek.der")),
    )
}

fn vcek_collateral(product: AmdProduct, vcek: Vec<u8>) -> SnpCollateral {
    SnpCollateral {
        product,
        signing_key: AmdSigningKey::Vcek,
        leaf_der: vcek,
        intermediate_der: None,
        crl_der: None,
    }
}

#[test]
fn real_genoa_v5_report_verifies_to_the_pinned_genoa_ark() {
    let (report, vcek) = oak("genoa");
    let parsed = SnpReport::parse(&report).unwrap();
    assert_eq!(parsed.version, 5);
    assert_eq!(parsed.product(), Some(AmdProduct::Genoa));
    assert_eq!(
        (parsed.launch_mit_vector, parsed.current_mit_vector),
        (1, 1)
    );
    assert_eq!(parsed.encode(), report);

    let root = amd_ark(AmdProduct::Genoa).trust_root().unwrap();
    let v = verify_snp_collateral(
        &report,
        &vcek_collateral(AmdProduct::Genoa, vcek.clone()),
        &root,
        OAK_NOW,
        None,
    )
    .unwrap();
    assert_eq!(v.certificate.product_name, "Genoa");
    assert_eq!(
        v.reported_tcb,
        AmdTcb {
            fmc: None,
            bootloader: 10,
            tee: 0,
            snp: 25,
            microcode: 84
        }
    );
    assert_eq!(v.certificate.hw_id.as_deref(), Some(&parsed.chip_id[..]));
    assert!(!v.revocation_checked);

    // The same evidence named as Milan collateral: the report's CPUID says
    // Genoa, so it is rejected before any certificate is looked at.
    let milan = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    assert!(matches!(
        verify_snp_collateral(
            &report,
            &vcek_collateral(AmdProduct::Milan, vcek.clone()),
            &milan,
            OAK_NOW,
            None
        ),
        Err(AttestError::CollateralMismatch {
            what: "report CPUID product vs collateral product"
        })
    ));
    // Against the Turin ARK (with the Genoa ASK named) the path fails.
    let turin = amd_ark(AmdProduct::Turin).trust_root().unwrap();
    assert!(
        verify_snp_collateral(
            &report,
            &vcek_collateral(AmdProduct::Genoa, vcek),
            &turin,
            OAK_NOW,
            None
        )
        .is_err()
    );
}

#[test]
fn real_turin_v5_report_verifies_with_fmc_and_eight_byte_hw_id() {
    let (report, vcek) = oak("turin");
    let parsed = SnpReport::parse(&report).unwrap();
    assert_eq!(parsed.version, 5);
    assert_eq!(parsed.product(), Some(AmdProduct::Turin));
    let root = amd_ark(AmdProduct::Turin).trust_root().unwrap();
    let v = verify_snp_collateral(
        &report,
        &vcek_collateral(AmdProduct::Turin, vcek),
        &root,
        OAK_NOW,
        None,
    )
    .unwrap();
    assert_eq!(v.certificate.product_name, "Turin");
    assert_eq!(
        v.reported_tcb,
        AmdTcb {
            fmc: Some(0),
            bootloader: 1,
            tee: 0,
            snp: 2,
            microcode: 71
        }
    );
    let hw_id = v.certificate.hw_id.unwrap();
    assert_eq!(hw_id.len(), 8);
    assert_eq!(hw_id, parsed.chip_id[..8]);
    assert!(parsed.chip_id[8..].iter().all(|&b| b == 0));
}

#[test]
fn real_milan_v3_report_from_oak_verifies() {
    let (report, vcek) = oak("milan");
    let parsed = SnpReport::parse(&report).unwrap();
    assert_eq!(parsed.version, 3);
    assert_eq!(parsed.product(), Some(AmdProduct::Milan));
    let root = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    let v = verify_snp_collateral(
        &report,
        &vcek_collateral(AmdProduct::Milan, vcek),
        &root,
        OAK_NOW,
        None,
    )
    .unwrap();
    assert_eq!(v.certificate.product_name, "Milan-B0");
}

#[test]
fn real_vlek_report_verifies_through_the_pinned_milan_asvk() {
    let report = data("snp-trustee/test-vlek-report.bin");
    let vlek = data("snp-trustee/test-vlek.der");
    let parsed = SnpReport::parse(&report).unwrap();
    assert_eq!(parsed.signing_key(), 1);
    let root = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    let collateral = SnpCollateral {
        product: AmdProduct::Milan,
        signing_key: AmdSigningKey::Vlek,
        leaf_der: vlek.clone(),
        intermediate_der: None,
        crl_der: None,
    };
    let v = verify_snp_collateral(&report, &collateral, &root, VLEK_NOW, None).unwrap();
    assert_eq!(v.signing_key, AmdSigningKey::Vlek);
    assert_eq!(
        v.certificate.csp_id.as_deref(),
        Some("CN=cc-us-east-2.amazonaws.com")
    );
    assert_eq!(v.certificate.hw_id, None);
    assert_eq!(
        v.reported_tcb,
        AmdTcb {
            fmc: None,
            bootloader: 4,
            tee: 0,
            snp: 24,
            microcode: 217
        }
    );
    // Named as VCEK collateral: the report's SIGNING_KEY says VLEK.
    let as_vcek = SnpCollateral {
        signing_key: AmdSigningKey::Vcek,
        ..collateral.clone()
    };
    assert!(matches!(
        verify_snp_collateral(&report, &as_vcek, &root, VLEK_NOW, None),
        Err(AttestError::CollateralMismatch { .. })
    ));
    // After the VLEK expired (2025-12-10).
    assert!(matches!(
        verify_snp_collateral(&report, &collateral, &root, OAK_NOW, None),
        Err(AttestError::CertificateExpired)
    ));
    // Through the VCEK intermediate (ASK) instead of the ASVK.
    let via_ask = SnpCollateral {
        intermediate_der: Some(
            vesper_aegis::roots::amd_intermediate(AmdProduct::Milan, AmdSigningKey::Vcek)
                .der()
                .unwrap()
                .to_vec(),
        ),
        ..collateral
    };
    assert!(verify_snp_collateral(&report, &via_ask, &root, VLEK_NOW, None).is_err());
}

#[test]
fn snp_report_versions_and_reserved_fields() {
    let (genoa, _) = oak("genoa");
    let (milan, milan_vcek) = oak("milan");
    let set_version = |r: &[u8], v: u32| {
        let mut r = r.to_vec();
        r[..4].copy_from_slice(&v.to_le_bytes());
        r
    };
    // Version 6 (Venice extended TCBs) and 1 are unsupported.
    for v in [0, 1, 6, 7, u32::MAX] {
        assert!(matches!(
            SnpReport::parse(&set_version(&genoa, v)),
            Err(ParseError::UnsupportedValue {
                format: Format::SnpReport,
                field: "version",
                ..
            })
        ));
    }
    // A v5 report relabelled v3: the mitigation vectors become reserved
    // bytes, which are not zero.
    assert!(matches!(
        SnpReport::parse(&set_version(&genoa, 3)),
        Err(ParseError::NonZeroReserved { .. })
    ));
    // Version 4 has the version-3 layout: it parses, and the relabelled
    // report then fails its signature.
    let v4 = set_version(&milan, 4);
    assert_eq!(SnpReport::parse(&v4).unwrap().version, 4);
    let root = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    assert!(matches!(
        verify_snp_collateral(
            &v4,
            &vcek_collateral(AmdProduct::Milan, milan_vcek),
            &root,
            OAK_NOW,
            None
        ),
        Err(AttestError::BadSignature { .. })
    ));
    // Non-zero bytes after the mitigation vectors.
    let mut r = genoa.clone();
    r[0x208] = 1;
    assert!(matches!(
        SnpReport::parse(&r),
        Err(ParseError::NonZeroReserved { .. })
    ));
    // An unknown processor in a v3+ report.
    let mut r = genoa.clone();
    r[0x188] = 0x17;
    assert!(SnpReport::parse(&r).unwrap().product().is_none());
    let groot = amd_ark(AmdProduct::Genoa).trust_root().unwrap();
    assert!(matches!(
        verify_snp_collateral(
            &r,
            &vcek_collateral(AmdProduct::Genoa, oak("genoa").1),
            &groot,
            OAK_NOW,
            None
        ),
        Err(AttestError::Collateral {
            what: "SNP report CPUID",
            ..
        })
    ));
}

#[test]
fn hw_id_extension_accepts_the_octet_string_encoding() {
    // Trustee's "new format" VCEK test certificate wraps hwID in a DER
    // OCTET STRING (its chip ID is the counting pattern 00..3f).
    let der = data("snp-trustee/test-vcek-invalid-new.der");
    let ext = parse_amd_cert_extensions(&der, AmdProduct::Milan).unwrap();
    let want: Vec<u8> = (0u8..64).collect();
    assert_eq!(ext.hw_id.as_deref(), Some(&want[..]));
    // The real Genoa VCEK uses the raw form.
    let ext = parse_amd_cert_extensions(&oak("genoa").1, AmdProduct::Genoa).unwrap();
    assert_eq!(ext.hw_id.unwrap().len(), 64);
    // A Genoa VCEK read as Turin: wrong product name.
    assert!(matches!(
        parse_amd_cert_extensions(&oak("genoa").1, AmdProduct::Turin),
        Err(AttestError::CollateralMismatch { .. })
    ));
}

#[test]
fn kds_cert_chain_collateral_checks_the_served_ark() {
    let report = unhex(&String::from_utf8(data("snp-virtee-sev/report_milan.hex")).unwrap());
    let vcek = data("snp-virtee-sev/vcek_milan.der");
    let chain = data("snp-virtee-sev/cert_chain_milan");
    let collateral = SnpCollateral::from_kds_cert_chain(
        AmdProduct::Milan,
        AmdSigningKey::Vcek,
        vcek.clone(),
        &chain,
        None,
    )
    .unwrap();
    let root = amd_ark(AmdProduct::Milan).trust_root().unwrap();
    // The sample's VCEK was issued 2023; any time in its window will do.
    let v = verify_snp_collateral(&report, &collateral, &root, OAK_NOW, None).unwrap();
    assert_eq!(v.product, AmdProduct::Milan);
    // No CRL checked, so a CRL floor cannot be met.
    check_snp_crl_floor(&v, None).unwrap();
    assert!(matches!(
        check_snp_crl_floor(&v, Some(1)),
        Err(AttestError::MissingCollateral { .. })
    ));
    // The Milan chain is not Genoa's.
    assert!(matches!(
        SnpCollateral::from_kds_cert_chain(
            AmdProduct::Genoa,
            AmdSigningKey::Vcek,
            vcek.clone(),
            &chain,
            None
        ),
        Err(AttestError::CertificateChain(_))
    ));
    // Only the ASK (one certificate) is not a KDS cert_chain.
    let first = pem_certificates(&chain).unwrap().remove(0);
    let pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64(&first)
    );
    assert!(
        SnpCollateral::from_kds_cert_chain(
            AmdProduct::Milan,
            AmdSigningKey::Vcek,
            vcek,
            pem.as_bytes(),
            None
        )
        .is_err()
    );
}

fn base64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in b.chunks(3) {
        let n = chunk.len();
        let v = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= n {
                s.push(A[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

// --------------------------------------------------------------- TDX --

fn dcap_collateral(path: &str) -> TdxCollateral {
    let j: serde_json::Value = serde_json::from_slice(&data(path)).unwrap();
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

fn intel_root() -> vesper_aegis::TrustRoot {
    INTEL_SGX_ROOT_CA.trust_root().unwrap()
}

#[test]
fn real_quote_v5_td15_ex_verifies_end_to_end() {
    let quote = data("tdx-dcap-qvl-0.7/tdx_quote_td15ex");
    let collateral = dcap_collateral("tdx-dcap-qvl-0.7/tdx_quote_td15ex_collateral.json");
    let parsed = TdxQuote::parse(&quote).unwrap();
    assert_eq!(parsed.header.version, 5);
    assert_eq!(parsed.body_type(), Some(TDX_BODY_TYPE_TD15_EX));
    assert!(parsed.td15.is_some());
    let ex = parsed.td15_ex.as_ref().unwrap();
    assert!(ex.vm_index <= 3);
    assert_eq!(parsed.encode(), quote);

    let v = verify_tdx_collateral(&quote, &collateral, &intel_root(), TD15EX_NOW).unwrap();
    assert_eq!(v.evaluation.status, TcbStatus::UpToDate);
    assert_eq!(v.evaluation.module_status, Some(TcbStatus::UpToDate));
    let current = v.current_evaluation.as_ref().unwrap();
    assert_eq!(current.status, TcbStatus::UpToDate);
    assert_eq!(v.evaluation.tcb_evaluation_data_number, 20);
    assert_eq!(v.evaluation.fmspc, [0xB0, 0xC0, 0x6F, 0, 0, 0]);
    // Platform-instance PCK (Platform CA).
    assert_eq!(v.pck.issuer, PckIssuer::Platform);
    let inst = v.pck.platform_instance.as_ref().unwrap();
    assert!(inst.configuration.is_some());

    // Outside the collateral window.
    assert!(matches!(
        verify_tdx_collateral(&quote, &collateral, &intel_root(), OUTDATED_NOW),
        Err(AttestError::CollateralExpired { .. }) | Err(AttestError::CertificateNotYetValid)
    ));
    // Every body-descriptor tamper is a typed error.
    let mut q = quote.clone();
    q[48] = 9; // body type
    assert!(matches!(
        TdxQuote::parse(&q),
        Err(ParseError::UnsupportedValue {
            field: "body.type",
            ..
        })
    ));
    let mut q = quote.clone();
    q[48] = 1; // SGX enclave report body
    assert!(matches!(
        TdxQuote::parse(&q),
        Err(ParseError::UnsupportedValue {
            field: "body.type",
            ..
        })
    ));
    let mut q = quote.clone();
    q[48] = TDX_BODY_TYPE_TD15 as u8; // type 3 with the 885-byte size
    assert!(matches!(
        TdxQuote::parse(&q),
        Err(ParseError::UnsupportedValue {
            field: "body.size",
            ..
        })
    ));
    let mut q = quote.clone();
    q[50] ^= 1; // size
    assert!(TdxQuote::parse(&q).is_err());
    // A flipped byte in the 1.5-ex tail breaks the quote signature.
    let mut q = quote.clone();
    q[54 + 648 + 10] ^= 1;
    assert!(matches!(
        verify_tdx_collateral(&q, &collateral, &intel_root(), TD15EX_NOW),
        Err(AttestError::BadSignature { .. })
    ));
    // VM index above 3.
    let mut q = quote.clone();
    q[54 + 648] = 4;
    assert!(matches!(
        TdxQuote::parse(&q),
        Err(ParseError::UnsupportedValue {
            field: "body.vm_index",
            ..
        })
    ));
}

#[test]
fn real_quote_v5_td15_below_every_published_level() {
    // dcap-qvl's `tdx_quote_outdated` (FMSPC 90C06F000000): every signature,
    // chain, CRL and collateral check passes, and the PCK's SGX component 8
    // is 3 while every published TCB level requires 5. Intel's
    // component-wise rule therefore finds no level ("TCB level not
    // supported"); a lexicographic comparison would have called it
    // OutOfDate.
    let quote = data("tdx-dcap-qvl-0.7/tdx_quote_outdated");
    let collateral = dcap_collateral("tdx-dcap-qvl-0.7/tdx_quote_outdated_collateral.json");
    let parsed = TdxQuote::parse(&quote).unwrap();
    assert_eq!(parsed.body_type(), Some(TDX_BODY_TYPE_TD15));
    assert!(parsed.td15_ex.is_none());
    let td15 = parsed.td15.as_ref().unwrap();
    assert_eq!(parsed.body.tee_tcb_svn[..3], [7, 1, 3]);
    assert_eq!(td15.tee_tcb_svn2[..3], [13, 1, 3]);
    assert!(matches!(
        verify_tdx_collateral(&quote, &collateral, &intel_root(), OUTDATED_NOW),
        Err(AttestError::TcbLevelNotFound {
            what: "TDX platform TCB level"
        })
    ));
    let certs = pem_certificates(&parsed.pck_chain_pem).unwrap();
    let pck = parse_pck_extensions(&certs[0]).unwrap();
    assert_eq!(pck.sgx_components[..8], [3, 3, 2, 2, 4, 1, 0, 3]);
    assert_eq!(pck.issuer, PckIssuer::Platform);
    let inst = pck.platform_instance.unwrap();
    let cfg = inst.configuration.unwrap();
    assert_eq!(
        (cfg.dynamic_platform, cfg.cached_keys, cfg.smt_enabled),
        (Some(true), Some(false), Some(true))
    );
}

#[test]
fn other_real_v5_quotes_verify_their_pck_chain_and_signatures() {
    // No collateral is published with these; the PCK chain (no CRL), QE
    // report, attestation-key and quote signatures verify, and the Vesper
    // binding is then (correctly) the first check to fail.
    let runtime = support::runtime();
    for (path, now) in [
        // go-tdx-guest: PCK valid 2026-01-27..2033-01-27.
        ("tdx-go-tdx-guest/quote_sample_v5.dat", OUTDATED_NOW),
        // Trustee: PCK valid to 2031-03.
        ("tdx-trustee/tdx_quote_5.dat", OUTDATED_NOW),
    ] {
        let quote = data(path);
        let parsed = TdxQuote::parse(&quote).unwrap();
        assert_eq!(parsed.header.version, 5, "{path}");
        assert_eq!(parsed.body_type(), Some(TDX_BODY_TYPE_TD15), "{path}");
        assert_eq!(parsed.encode(), quote, "{path}");
        let certs = pem_certificates(&parsed.pck_chain_pem).unwrap();
        let pck = parse_pck_extensions(&certs[0]).unwrap();
        assert_eq!(pck.issuer, PckIssuer::Platform, "{path}");
        assert!(pck.platform_instance.is_some(), "{path}");
        match verify_tdx_quote(&quote, &intel_root(), now, &runtime) {
            Err(AttestError::BindingMismatch { .. }) => {}
            other => panic!("{path}: {other:?}"),
        }
    }
    // The v4 body type of a v5 quote is TD 1.0; none of the samples has it,
    // so check the constant is distinct.
    assert_ne!(TDX_BODY_TYPE_TD10, TDX_BODY_TYPE_TD15);
}

#[test]
fn intel_collateral_rollback_is_a_typed_error() {
    // The dcap-qvl 0.3.4 quote and collateral (FMSPC B0C06F000000,
    // tcbEvaluationDataNumber 17) verify on their own. A verifier that has
    // already accepted the 0.7.1 collateral for the same FMSPC (number 20)
    // refuses them as a downgrade.
    let old_quote = data("tdx-dcap-qvl/tdx_quote");
    let old = dcap_collateral("tdx-dcap-qvl/tdx_quote_collateral.json");
    let old_v = verify_tdx_collateral(&old_quote, &old, &intel_root(), DCAP_QVL_034_NOW).unwrap();
    let new_quote = data("tdx-dcap-qvl-0.7/tdx_quote_td15ex");
    let new = dcap_collateral("tdx-dcap-qvl-0.7/tdx_quote_td15ex_collateral.json");
    let new_v = verify_tdx_collateral(&new_quote, &new, &intel_root(), TD15EX_NOW).unwrap();
    assert_eq!(old_v.evaluation.fmspc, new_v.evaluation.fmspc);

    let floor = TdxCollateralFloor::default().raised_by(&new_v);
    assert_eq!(floor.min_tcb_evaluation_data_number, 20);
    check_tdx_collateral_floor(&new_v, &floor).unwrap();
    assert!(matches!(
        check_tdx_collateral_floor(&old_v, &floor),
        Err(AttestError::CollateralDowngrade {
            what: "TCB info tcbEvaluationDataNumber",
            have: 17,
            floor: 20
        })
    ));
    // The floor never moves down.
    let again = floor.raised_by(&old_v);
    assert_eq!(again, floor);
    // The old collateral passes a floor it meets.
    let old_floor = TdxCollateralFloor::default().raised_by(&old_v);
    check_tdx_collateral_floor(&old_v, &old_floor).unwrap();
    // An issue-date-only floor.
    let date_floor = TdxCollateralFloor {
        min_tcb_info_issue_date: new_v.tcb_info.issue_date,
        ..TdxCollateralFloor::default()
    };
    assert!(matches!(
        check_tdx_collateral_floor(&old_v, &date_floor),
        Err(AttestError::CollateralDowngrade {
            what: "TCB info issueDate",
            ..
        })
    ));
}

#[test]
fn new_real_samples_survive_mutation_without_panics() {
    let mut rng = support::SplitMix(0x5eed_0007);
    let samples = [
        data("snp-oak/genoa_report.bin"),
        data("snp-oak/turin_report.bin"),
        data("snp-trustee/test-vlek-report.bin"),
        data("tdx-dcap-qvl-0.7/tdx_quote_td15ex"),
        data("tdx-go-tdx-guest/quote_sample_v5.dat"),
        data("cca-veraison/cca-token-01.cbor"),
        data("cca-veraison/cca-token-draft-ffm-03-all-claims.cbor"),
    ];
    let genoa_vcek = oak("genoa").1;
    let groot = amd_ark(AmdProduct::Genoa).trust_root().unwrap();
    for _ in 0..300 {
        for s in &samples {
            let mut m = s.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(m.len() as u64) as usize;
                m[i] ^= 1 << rng.below(8);
            }
            if rng.below(8) == 0 {
                m.truncate(rng.below(m.len() as u64) as usize);
            }
            let _ = SnpReport::parse(&m);
            let _ = TdxQuote::parse(&m);
            let _ = CcaToken::parse(&m);
            // A mutated Genoa report must never verify.
            if m.len() == 1184 && m != samples[0] {
                assert!(
                    verify_snp_collateral(
                        &m,
                        &vcek_collateral(AmdProduct::Genoa, genoa_vcek.clone()),
                        &groot,
                        OAK_NOW,
                        None
                    )
                    .is_err()
                );
            }
        }
    }
}

// --------------------------------------------------------------- CCA --

/// A Veraison trust-anchor file (`[{instance-id, implementation-id,
/// pkey: JWK}]`) as endorsements.
fn endorsements(path: &str) -> Vec<CcaEndorsement> {
    let j: serde_json::Value = serde_json::from_slice(&data(path)).unwrap();
    j.as_array()
        .unwrap()
        .iter()
        .map(|ta| {
            let k = &ta["pkey"];
            let curve = match k["crv"].as_str().unwrap() {
                "P-256" => Curve::P256,
                "P-384" => Curve::P384,
                other => panic!("{other}"),
            };
            let mut sec1 = vec![4];
            sec1.extend(b64url(k["x"].as_str().unwrap()));
            sec1.extend(b64url(k["y"].as_str().unwrap()));
            CcaEndorsement {
                instance_id: unhex(ta["instance-id"].as_str().unwrap()),
                implementation_id: Some(unhex(ta["implementation-id"].as_str().unwrap())),
                cpak: EcPublicKey::from_sec1(curve, &sec1).unwrap(),
            }
        })
        .collect()
}

#[test]
fn tf_rmm_cca_token_verifies_against_the_tf_a_platform_key() {
    // Veraison's `cca-token-01.cbor` is byte-identical to the token TF-RMM
    // produced in `github.com/veraison/ccatoken` (`realm/testvectors/
    // tf-rmm/cca_token.cbor`), signed with TF-A's published test platform
    // key; `ta-tfa.json` holds that key's public half.
    let token = data("cca-veraison/cca-token-01.cbor");
    let tas = endorsements("cca-veraison/ta-tfa.json");
    let v = verify_cca_endorsed(&token, &tas).unwrap();
    assert_eq!(v.token.collection, CcaCollection::Tag399);
    assert_eq!(v.token.platform_profile, CcaPlatformProfile::CcaSsd1);
    assert_eq!(v.token.realm_profile, CcaRealmProfile::Unspecified);
    assert_eq!(v.token.platform_claims.lifecycle, 0x3003);
    assert_eq!(v.token.realm_claims.public_key.len(), 97);
    // Not Vesper evidence.
    assert!(matches!(
        cca_vesper_claims(&v, &support::runtime()),
        Err(AttestError::BindingMismatch { .. })
    ));

    // Endorsement mismatches.
    let mut wrong_impl = tas.clone();
    wrong_impl[0].implementation_id = Some(vec![0; 32]);
    assert!(matches!(
        verify_cca_endorsed(&token, &wrong_impl),
        Err(AttestError::CollateralMismatch { .. })
    ));
    let mut any_impl = tas.clone();
    any_impl[0].implementation_id = None;
    verify_cca_endorsed(&token, &any_impl).unwrap();
    let mut other_instance = tas.clone();
    other_instance[0].instance_id[1] ^= 1;
    assert!(matches!(
        verify_cca_endorsed(&token, &other_instance),
        Err(AttestError::MissingCollateral { .. })
    ));
    let twice = [tas[0].clone(), tas[0].clone()];
    assert!(matches!(
        verify_cca_endorsed(&token, &twice),
        Err(AttestError::Collateral { .. })
    ));
    // The 2024 token's P-256 key in place of the TF-A key.
    let mut other_key = tas.clone();
    other_key[0].cpak = endorsements("cca-veraison/ta-ffm-03.json")[0].cpak.clone();
    assert!(matches!(
        verify_cca_endorsed(&token, &other_key),
        Err(AttestError::UnexpectedKeyType { .. })
    ));
    // A flipped bit in the platform signature.
    let mut t = token.clone();
    let n = t.len();
    t[n - 200] ^= 1;
    assert!(verify_cca_endorsed(&t, &tas).is_err());
}

#[test]
fn cca_2024_cmw_token_verifies_against_its_reference_key() {
    // `draft-ffm-rats-cca-token-03` (profiles `…2024…#2.0.0`), CMW
    // collection tag 907, platform ES256 by the CPAK, realm ES384 by a
    // COSE_Key RAK hashed with SHA-512, 64-byte measurements with SHA-256.
    let token = data("cca-veraison/cca-token-draft-ffm-03-all-claims.cbor");
    let tas = endorsements("cca-veraison/ta-ffm-03.json");
    let v = verify_cca_endorsed(&token, &tas).unwrap();
    assert_eq!(v.token.collection, CcaCollection::Cmw907);
    assert_eq!(v.token.platform_profile, CcaPlatformProfile::Platform2024);
    assert_eq!(v.token.realm_profile, CcaRealmProfile::Realm2024);
    assert_eq!(v.token.platform_claims.challenge.len(), 64);
    assert_eq!(v.token.realm_claims.initial_measurement.len(), 64);

    // Against the TF-A key: wrong curve for an ES256 token.
    let mut wrong = tas.clone();
    wrong[0].cpak = endorsements("cca-veraison/ta-tfa.json")[0].cpak.clone();
    assert!(verify_cca_endorsed(&token, &wrong).is_err());
}

#[test]
fn malformed_cca_collections_are_typed_errors() {
    let token = data("cca-veraison/cca-token-draft-ffm-03-all-claims.cbor");
    // Tag 907 → 908.
    let pos = token
        .windows(3)
        .position(|w| w == [0xd9, 0x03, 0x8b])
        .unwrap();
    let mut t = token.clone();
    t[pos + 2] = 0x8c;
    assert!(matches!(
        CcaToken::parse(&t),
        Err(ParseError::Cbor {
            reason: "wrong tag",
            ..
        })
    ));
    // CMW content format 263 → 264 (encoded 0x19 0x01 0x07).
    let pos = token
        .windows(3)
        .position(|w| w == [0x19, 0x01, 0x07])
        .unwrap();
    let mut t = token.clone();
    t[pos + 2] = 0x08;
    assert!(matches!(
        CcaToken::parse(&t),
        Err(ParseError::Cbor {
            reason: "CMW content format is not 263",
            ..
        })
    ));
    // An unknown platform profile ("…#2.0.0" → "…#2.0.1"), re-signed is
    // not possible, but parsing already refuses it.
    let needle = b"cca_platform#2.0.0";
    let pos = token
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    let mut t = token.clone();
    t[pos + needle.len() - 1] = b'1';
    assert!(matches!(
        CcaToken::parse(&t),
        Err(ParseError::Cbor {
            field: "platform profile",
            reason: "unknown profile"
        })
    ));
}
