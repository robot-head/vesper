//! Wave-eight: AMD SEV-SNP attestation report version 6.
//!
//! No real version-6 report is published anywhere this lane could reach
//! (Project Oak, Trustee, go-sev-guest, VirTEE `sev`/`snpguest`, Contrast,
//! the Veraison and Intel/AMD repositories were searched; see the report).
//! So the version-6 layout is checked three ways:
//!
//! - real Genoa and Turin version-5 reports (Project Oak) relabelled as
//!   version 6 still parse, because their extended-TCB bytes are zero, and
//!   then fail their AMD signature: the parser accepts the v6 layout on
//!   real data and the signature covers the version field;
//! - generated reports (test keys) for Milan/Genoa/Turin v6 and Venice v6
//!   with extended TCBs verify end to end against a generated chain;
//! - every v6-specific rule (ETCB only on Venice, ETCB reserved bits,
//!   round trips, unknown versions) has a negative test.

use vesper_aegis::amd::{SnpCollateral, verify_snp_collateral};
use vesper_aegis::error::{AttestError, ParseError};
use vesper_aegis::roots::{AmdProduct, AmdSigningKey, amd_ark};
use vesper_aegis::snp::{SNP_ETCB_LEN, SnpEtcb, SnpReport, verify_snp_report};
use vesper_aegis::Curve;

mod support;
use support::{NOW_NS, SnpParams, chain, runtime, snp_report_struct};

const NS: u64 = 1_000_000_000;
const OAK_NOW: u64 = 1_767_225_600 * NS;

fn data(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/data/vendor/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn etcb(base: u8) -> SnpEtcb {
    let mut b = [0u8; SNP_ETCB_LEN];
    for (i, x) in b[..14].iter_mut().enumerate() {
        *x = base + i as u8;
    }
    SnpEtcb::parse(b).unwrap()
}

#[test]
fn real_v5_reports_relabelled_v6_parse_and_fail_their_signature() {
    for (product, p) in [("genoa", AmdProduct::Genoa), ("turin", AmdProduct::Turin)] {
        let report = data(&format!("snp-oak/{product}_report.bin"));
        let vcek = data(&format!("snp-oak/{product}_vcek.der"));
        let mut v6 = report.clone();
        v6[..4].copy_from_slice(&6u32.to_le_bytes());
        let parsed = SnpReport::parse(&v6).unwrap();
        assert_eq!(parsed.version, 6);
        assert!(!parsed.is_venice());
        assert_eq!(parsed.current_etcb, None);
        assert_eq!(parsed.product(), Some(p));
        // Mitigation vectors carry over unchanged.
        let v5 = SnpReport::parse(&report).unwrap();
        assert_eq!(parsed.launch_mit_vector, v5.launch_mit_vector);
        assert_eq!(parsed.current_mit_vector, v5.current_mit_vector);
        // Re-encoding reproduces the bytes exactly.
        assert_eq!(parsed.encode(), v6);
        let collateral = SnpCollateral {
            product: p,
            signing_key: AmdSigningKey::Vcek,
            leaf_der: vcek,
            intermediate_der: None,
            crl_der: None,
        };
        let root = amd_ark(p).trust_root().unwrap();
        verify_snp_collateral(&report, &collateral, &root, OAK_NOW, None).unwrap();
        assert!(matches!(
            verify_snp_collateral(&v6, &collateral, &root, OAK_NOW, None),
            Err(AttestError::BadSignature { .. })
        ));
        // A non-Venice v6 report with non-zero ETCB bytes is malformed.
        for off in [0x220, 0x240, 0x260, 0x27F] {
            let mut t = v6.clone();
            t[off] = 1;
            assert!(matches!(
                SnpReport::parse(&t),
                Err(ParseError::NonZeroReserved { .. })
            ));
        }
        // The 24 bytes before and the 32 after the ETCBs are reserved too.
        for off in [0x208, 0x21F, 0x280, 0x29F] {
            let mut t = v6.clone();
            t[off] = 1;
            assert!(SnpReport::parse(&t).is_err(), "{off:#x}");
        }
    }
}

#[test]
fn generated_v6_reports_verify_end_to_end() {
    let ark = chain(Curve::P384, Curve::P384, false);
    let rt = runtime();
    for cpuid in [[0x19, 0x01, 1], [0x19, 0x11, 1], [0x1A, 0x02, 0]] {
        let mut r = snp_report_struct(&ark.leaf, &SnpParams::default());
        r.version = 6;
        r.cpuid = cpuid;
        r.launch_mit_vector = 3;
        r.current_mit_vector = 7;
        r.signature_r_s = ark.leaf.sign(&r.signed_bytes()).try_into().unwrap();
        let bytes = r.encode();
        assert_eq!(SnpReport::parse(&bytes).unwrap(), r);
        verify_snp_report(&bytes, &ark.der_chain(), &ark.root, NOW_NS, &rt).unwrap();
    }

    // Venice (family 1Ah, model 50h..5Fh / C0h..CFh): the ETCBs are read.
    for model in [0x50, 0x5F, 0xC0, 0xCF] {
        let mut r = snp_report_struct(&ark.leaf, &SnpParams::default());
        r.version = 6;
        r.cpuid = [0x1A, model, 0];
        r.current_etcb = Some(etcb(10));
        r.launch_etcb = Some(etcb(5));
        r.committed_etcb = Some(etcb(10));
        r.signature_r_s = ark.leaf.sign(&r.signed_bytes()).try_into().unwrap();
        let bytes = r.encode();
        let parsed = SnpReport::parse(&bytes).unwrap();
        assert!(parsed.is_venice());
        assert_eq!(parsed.product(), None);
        assert_eq!(parsed.current_etcb, Some(etcb(10)));
        assert_eq!(parsed.launch_etcb, Some(etcb(5)));
        assert_eq!(bytes[0x220..0x22E], etcb(10).to_bytes()[..14]);
        assert!(parsed.current_etcb.unwrap().at_least(&parsed.launch_etcb.unwrap()));
        assert!(!parsed.launch_etcb.unwrap().at_least(&parsed.current_etcb.unwrap()));
        verify_snp_report(&bytes, &ark.der_chain(), &ark.root, NOW_NS, &rt).unwrap();
        // A flipped ETCB SVN breaks the signature.
        let mut t = bytes.clone();
        t[0x245] ^= 1;
        assert!(matches!(
            verify_snp_report(&t, &ark.der_chain(), &ark.root, NOW_NS, &rt),
            Err(AttestError::BadSignature { .. })
        ));
        // ETCB bits 255:112 are reserved.
        let mut t = bytes.clone();
        t[0x220 + 14] = 1;
        assert!(matches!(
            SnpReport::parse(&t),
            Err(ParseError::NonZeroReserved {
                field: "ETCB bits 255:112",
                ..
            })
        ));
        // Venice has no published AMD root or TCB layout to bind against:
        // collateral verification refuses it with a typed error.
        let collateral = SnpCollateral {
            product: AmdProduct::Turin,
            signing_key: AmdSigningKey::Vcek,
            leaf_der: ark.leaf_der.clone(),
            intermediate_der: None,
            crl_der: None,
        };
        assert!(matches!(
            verify_snp_collateral(&bytes, &collateral, &ark.root, NOW_NS, None),
            Err(AttestError::MissingCollateral { .. })
        ));
        // A Venice report below version 6 has no ETCBs: non-zero bytes there
        // are reserved.
        let mut v5 = bytes.clone();
        v5[..4].copy_from_slice(&5u32.to_le_bytes());
        assert!(matches!(
            SnpReport::parse(&v5),
            Err(ParseError::NonZeroReserved { .. })
        ));
    }
}

#[test]
fn v6_versions_and_mutations() {
    let ark = chain(Curve::P384, Curve::P384, false);
    let mut r = snp_report_struct(&ark.leaf, &SnpParams::default());
    r.version = 6;
    r.cpuid = [0x1A, 0x51, 0];
    r.current_etcb = Some(etcb(1));
    r.launch_etcb = Some(etcb(1));
    r.committed_etcb = Some(etcb(1));
    r.signature_r_s = ark.leaf.sign(&r.signed_bytes()).try_into().unwrap();
    let bytes = r.encode();
    for v in [7u32, 8, 0x100, u32::MAX] {
        let mut t = bytes.clone();
        t[..4].copy_from_slice(&v.to_le_bytes());
        assert!(matches!(
            SnpReport::parse(&t),
            Err(ParseError::UnsupportedValue {
                field: "version",
                ..
            })
        ));
    }
    let rt = runtime();
    let mut rng = support::SplitMix(0x6);
    for _ in 0..500 {
        let mut t = bytes.clone();
        let i = rng.below(t.len() as u64) as usize;
        let old = t[i];
        t[i] = rng.next() as u8;
        if rng.below(5) == 0 {
            let n = rng.below(t.len() as u64) as usize;
            t.truncate(n);
        }
        let res = verify_snp_report(&t, &ark.der_chain(), &ark.root, NOW_NS, &rt);
        if t.len() == bytes.len() && t[i] != old && i < 0x2A0 {
            assert!(res.is_err(), "mutated byte {i:#x} verified");
        }
    }
}
