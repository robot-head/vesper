//! End-to-end evidence verification over test-generated keys and chains:
//! accepted evidence, and tampering with every protected part.

mod support;

use support::*;
use vesper_aegis::cca::{self, CcaToken, cca_rim_measurement, verify_cca_token};
use vesper_aegis::claims::{RegisterHash, SupplyChainEvidence};
use vesper_aegis::policy::{Evidence, RaTlsError, TcbFloors, TrustRoots};
use vesper_aegis::snp::verify_snp_report;
use vesper_aegis::tdx::verify_tdx_quote;
use vesper_aegis::*;

fn policy() -> RelyingPartyPolicy {
    let rt = runtime();
    RelyingPartyPolicy {
        allowed_archs: vec![
            TeeArch::IntelTdx15,
            TeeArch::AmdSevSnp,
            TeeArch::ArmCcaRealm,
        ],
        tcb_floors: TcbFloors {
            tdx: 0x0103,
            snp: 20 << 8,
            cca: 0,
        },
        snp_component_floors: None,
        snp_max_vmpl: 0,
        allowed_measurements: vec![
            (TeeArch::IntelTdx15, MRTD),
            (TeeArch::AmdSevSnp, SNP_MEASUREMENT),
            (
                TeeArch::ArmCcaRealm,
                cca_rim_measurement(RegisterHash::Sha256, &CCA_RIM),
            ),
        ],
        expected_loom_blake3: rt.loom_lock_blake3,
        expected_olean_mif: rt.olean_spec_mif,
        require_register_replay: false,
    }
}

fn assert_bad_sig(r: Result<AttestationClaims, AttestError>) {
    assert!(
        matches!(r, Err(AttestError::BadSignature { .. })),
        "expected BadSignature, got {r:?}"
    );
}

#[test]
fn tdx_quote_verifies_through_ra_tls() {
    let pck = chain(Curve::P256, Curve::P256, false);
    let ak = Signer::generate(Curve::P256);
    let quote = tdx_quote(&pck, &ak, &TdxParams::default());
    let roots = TrustRoots {
        tdx: Some(pck.root.clone()),
        ..Default::default()
    };
    let rt = runtime();
    let claims = verify_ra_tls(
        Evidence::Tdx { quote: &quote },
        &roots,
        NOW_NS,
        &rt,
        &policy(),
        &rt.channel_binding,
    )
    .unwrap();
    assert_eq!(claims.arch, TeeArch::IntelTdx15);
    assert_eq!(claims.tcb_svn, 0x0103);
    assert_eq!(claims.mr_enclave_sha384, MRTD);
    assert_eq!(
        claims.supply_chain_evidence,
        SupplyChainEvidence::RegisterReplay
    );
    assert_ne!(claims.enclave_flags & ENCLAVE_FLAG_DEBUG_DISABLED, 0);
    assert_ne!(claims.enclave_flags & ENCLAVE_FLAG_LEAN_VERIFIED, 0);
    // 128-byte wire round trip.
    let wire = claims.to_wire().encode();
    assert_eq!(wire.len(), 128);
    assert_eq!(
        AegisAttestationClaimsWire::decode(&wire).unwrap(),
        claims.to_wire()
    );
}

#[test]
fn tdx_tampering_is_rejected() {
    let pck = chain(Curve::P256, Curve::P256, false);
    let ak = Signer::generate(Curve::P256);
    let rt = runtime();
    let good = tdx_quote_struct(&pck, &ak, &TdxParams::default());
    assert!(verify_tdx_quote(&good.encode(), &pck.root, NOW_NS, &rt).is_ok());

    // Every byte of the signed header+body is covered by the quote signature.
    let bytes = good.encode();
    for at in (0..632).step_by(7) {
        let mut t = bytes.clone();
        t[at] ^= 0x01;
        let r = verify_tdx_quote(&t, &pck.root, NOW_NS, &rt);
        assert!(r.is_err(), "flip at {at} accepted");
        if !(0..8).contains(&at) {
            assert_bad_sig(r);
        }
    }
    // QE report tampering: PCK signature fails.
    let mut q = good.clone();
    q.qe_report.isv_svn ^= 1;
    assert_bad_sig(verify_tdx_quote(&q.encode(), &pck.root, NOW_NS, &rt));
    // Attestation key replaced (and quote re-signed) without a new QE report.
    let other = Signer::generate(Curve::P256);
    let mut q = good.clone();
    q.attestation_key = other.raw_xy().try_into().unwrap();
    q.quote_signature = other.sign(&q.signed_bytes()).try_into().unwrap();
    assert!(matches!(
        verify_tdx_quote(&q.encode(), &pck.root, NOW_NS, &rt),
        Err(AttestError::BindingMismatch { .. })
    ));
    // QE auth data changed: QE report data no longer binds.
    let mut q = good.clone();
    q.qe_auth_data = b"other".to_vec();
    assert!(matches!(
        verify_tdx_quote(&q.encode(), &pck.root, NOW_NS, &rt),
        Err(AttestError::BindingMismatch { .. })
    ));
    // Untrusted runtime claims that differ from the signed report data.
    let mut wrong = rt;
    wrong.channel_binding[0] ^= 1;
    assert!(matches!(
        verify_tdx_quote(&bytes, &pck.root, NOW_NS, &wrong),
        Err(AttestError::BindingMismatch { .. })
    ));
    // RTMR[2] that was not extended with the claimed Loom digest.
    let mut q = good.clone();
    q.body.rtmr[2] = [0; 48];
    q.quote_signature = ak.sign(&q.signed_bytes()).try_into().unwrap();
    assert_eq!(
        verify_tdx_quote(&q.encode(), &pck.root, NOW_NS, &rt),
        Err(AttestError::RegisterReplayMismatch {
            register: "RTMR[2]"
        })
    );
    let mut q = good;
    q.body.rtmr[3][5] ^= 1;
    q.quote_signature = ak.sign(&q.signed_bytes()).try_into().unwrap();
    assert_eq!(
        verify_tdx_quote(&q.encode(), &pck.root, NOW_NS, &rt),
        Err(AttestError::RegisterReplayMismatch {
            register: "RTMR[3]"
        })
    );
}

#[test]
fn tdx_chain_failures_are_rejected() {
    let ak = Signer::generate(Curve::P256);
    let rt = runtime();
    let pck = chain(Curve::P256, Curve::P256, false);
    let quote = tdx_quote(&pck, &ak, &TdxParams::default());
    // Wrong root.
    let stranger = chain(Curve::P256, Curve::P256, false);
    assert!(matches!(
        verify_tdx_quote(&quote, &stranger.root, NOW_NS, &rt),
        Err(AttestError::CertificateChain(_))
    ));
    // Expired PCK leaf.
    let expired = chain(Curve::P256, Curve::P256, true);
    let q = tdx_quote(&expired, &ak, &TdxParams::default());
    assert!(matches!(
        verify_tdx_quote(&q, &expired.root, NOW_NS, &rt),
        Err(AttestError::CertificateExpired)
    ));
    // PCK on the wrong curve.
    let p384 = chain(Curve::P256, Curve::P384, false);
    let mut qs = tdx_quote_struct(&pck, &ak, &TdxParams::default());
    qs.pck_chain_pem = p384.pem_chain();
    assert!(matches!(
        verify_tdx_quote(&qs.encode(), &p384.root, NOW_NS, &rt),
        Err(AttestError::UnexpectedKeyType { .. })
    ));
}

#[test]
fn snp_report_verifies_and_tampering_is_rejected() {
    let ark = chain(Curve::P384, Curve::P384, false);
    let rt = runtime();
    let report = snp_report(&ark.leaf, &SnpParams::default());
    let claims = verify_snp_report(&report, &ark.der_chain(), &ark.root, NOW_NS, &rt).unwrap();
    assert_eq!(claims.arch, TeeArch::AmdSevSnp);
    assert_eq!(claims.tcb_svn, 20 << 8);
    assert_eq!(claims.snp_vmpl, Some(0));
    assert_eq!(
        claims.supply_chain_evidence,
        SupplyChainEvidence::ReportDataOnly
    );
    assert_ne!(claims.enclave_flags & ENCLAVE_FLAG_SMT_MITIGATED, 0);
    verify_attestation_claims(&claims, &policy(), &rt.channel_binding).unwrap();

    for at in (0..0x2A0).step_by(5) {
        let mut t = report.clone();
        t[at] ^= 0x80;
        assert!(
            verify_snp_report(&t, &ark.der_chain(), &ark.root, NOW_NS, &rt).is_err(),
            "flip at {at:#x} accepted"
        );
    }
    // Signature bytes themselves.
    let mut t = report.clone();
    t[0x2A0] ^= 1;
    assert_bad_sig(verify_snp_report(
        &t,
        &ark.der_chain(),
        &ark.root,
        NOW_NS,
        &rt,
    ));
    // Signed by a key that is not the certified VCEK.
    let rogue = Signer::generate(Curve::P384);
    let forged = snp_report(&rogue, &SnpParams::default());
    assert_bad_sig(verify_snp_report(
        &forged,
        &ark.der_chain(),
        &ark.root,
        NOW_NS,
        &rt,
    ));
    // Missing ASK.
    assert!(matches!(
        verify_snp_report(
            &report,
            std::slice::from_ref(&ark.leaf_der),
            &ark.root,
            NOW_NS,
            &rt
        ),
        Err(AttestError::CertificateChain(_))
    ));
    // Debug policy is reported, then rejected by the relying-party policy.
    let dbg = snp_report(
        &ark.leaf,
        &SnpParams {
            policy: (1 << 17) | vesper_aegis::snp::SNP_POLICY_DEBUG,
            ..Default::default()
        },
    );
    let c = verify_snp_report(&dbg, &ark.der_chain(), &ark.root, NOW_NS, &rt).unwrap();
    assert_eq!(
        verify_attestation_claims(&c, &policy(), &rt.channel_binding),
        Err(AegisVerifyError::EnclaveDebugModeEnabled)
    );
}

#[test]
fn cca_token_verifies_and_tampering_is_rejected() {
    let iak_chain = chain(Curve::P384, Curve::P256, false);
    let rak = Signer::generate(Curve::P384);
    let rt = runtime();
    let token = cca_token(&iak_chain.leaf, &rak, &CcaParams::default());
    let claims =
        verify_cca_token(&token, &iak_chain.der_chain(), &iak_chain.root, NOW_NS, &rt).unwrap();
    assert_eq!(claims.arch, TeeArch::ArmCcaRealm);
    assert_eq!(
        claims.mr_enclave_sha384,
        cca_rim_measurement(RegisterHash::Sha256, &CCA_RIM)
    );
    assert_eq!(
        claims.supply_chain_evidence,
        SupplyChainEvidence::RegisterReplay
    );
    verify_attestation_claims(&claims, &policy(), &rt.channel_binding).unwrap();

    // SHA-512 realm hash also verifies.
    let t512 = cca_token(
        &iak_chain.leaf,
        &rak,
        &CcaParams {
            realm_hash: RegisterHash::Sha512,
            ..Default::default()
        },
    );
    verify_cca_token(&t512, &iak_chain.der_chain(), &iak_chain.root, NOW_NS, &rt).unwrap();

    // A legacy raw-SEC1 RAK claim also verifies.
    let legacy = {
        let mut realm = support::cca_realm_claims(&rak, &CcaParams::default());
        realm.public_key = rak.sec1();
        let platform = vesper_aegis::cca::CcaPlatformClaims {
            challenge: cca::rak_hash(realm.public_key_hash_algo, &realm.public_key),
            ..CcaToken::parse(&token).unwrap().platform_claims
        };
        cca::encode_collection(
            &cca::build_sign1(Curve::P256, platform.to_cbor(), |d| iak_chain.leaf.sign(d)),
            &cca::build_sign1(Curve::P384, realm.to_cbor(), |d| rak.sign(d)),
        )
    };
    verify_cca_token(
        &legacy,
        &iak_chain.der_chain(),
        &iak_chain.root,
        NOW_NS,
        &rt,
    )
    .unwrap();
    // COSE_Key on the wrong curve is rejected.
    assert!(matches!(
        cca::realm_public_key(&{
            use coset::CborSerializable;
            coset::CoseKeyBuilder::new_ec2_pub_key(
                coset::iana::EllipticCurve::P_256,
                vec![1; 32],
                vec![2; 32],
            )
            .build()
            .to_vec()
            .unwrap()
        }),
        Err(AttestError::UnexpectedKeyType { .. })
    ));

    // Byte flips anywhere in the collection never verify.
    for at in 0..token.len() {
        let mut t = token.clone();
        t[at] ^= 0x04;
        assert!(
            verify_cca_token(&t, &iak_chain.der_chain(), &iak_chain.root, NOW_NS, &rt).is_err(),
            "flip at {at} accepted"
        );
    }

    // A realm token signed by a different key than the RAK it names.
    let parsed = CcaToken::parse(&token).unwrap();
    let other = Signer::generate(Curve::P384);
    let realm = cca::build_sign1(Curve::P384, parsed.realm_claims.to_cbor(), |d| {
        other.sign(d)
    });
    let platform = cca::build_sign1(Curve::P256, parsed.platform_claims.to_cbor(), |d| {
        iak_chain.leaf.sign(d)
    });
    let t = cca::encode_collection(&platform, &realm);
    assert_bad_sig(verify_cca_token(
        &t,
        &iak_chain.der_chain(),
        &iak_chain.root,
        NOW_NS,
        &rt,
    ));

    // A genuine platform token paired with a realm token for another RAK.
    let rak2 = Signer::generate(Curve::P384);
    let realm2 = support::cca_realm_claims(&rak2, &CcaParams::default());
    let realm2 = cca::build_sign1(Curve::P384, realm2.to_cbor(), |d| rak2.sign(d));
    let t = cca::encode_collection(&platform, &realm2);
    assert!(matches!(
        verify_cca_token(&t, &iak_chain.der_chain(), &iak_chain.root, NOW_NS, &rt),
        Err(AttestError::BindingMismatch { .. })
    ));

    // Platform token signed by an uncertified key.
    let rogue = Signer::generate(Curve::P256);
    let t = cca_token(&rogue, &rak, &CcaParams::default());
    assert_bad_sig(verify_cca_token(
        &t,
        &iak_chain.der_chain(),
        &iak_chain.root,
        NOW_NS,
        &rt,
    ));

    // Debug lifecycle.
    let dbg = cca_token(
        &iak_chain.leaf,
        &rak,
        &CcaParams {
            lifecycle: 0x5000,
            ..Default::default()
        },
    );
    let c = verify_cca_token(&dbg, &iak_chain.der_chain(), &iak_chain.root, NOW_NS, &rt).unwrap();
    assert_eq!(c.enclave_flags & ENCLAVE_FLAG_DEBUG_DISABLED, 0);
}

#[test]
fn ra_tls_requires_a_configured_root_and_matching_channel() {
    let ark = chain(Curve::P384, Curve::P384, false);
    let rt = runtime();
    let report = snp_report(&ark.leaf, &SnpParams::default());
    let chain_der = ark.der_chain();
    let ev = Evidence::Snp {
        report: &report,
        vcek_chain: &chain_der,
    };
    assert_eq!(
        verify_ra_tls(
            ev,
            &TrustRoots::default(),
            NOW_NS,
            &rt,
            &policy(),
            &rt.channel_binding
        ),
        Err(RaTlsError::NoTrustRoot(TeeArch::AmdSevSnp))
    );
    let roots = TrustRoots {
        snp: Some(ark.root.clone()),
        ..Default::default()
    };
    let mut observed = rt.channel_binding;
    observed[47] ^= 1;
    assert_eq!(
        verify_ra_tls(ev, &roots, NOW_NS, &rt, &policy(), &observed),
        Err(RaTlsError::Policy(
            AegisVerifyError::ChannelTranscriptBindingMismatch
        ))
    );
    let mut strict = policy();
    strict.require_register_replay = true;
    assert_eq!(
        verify_ra_tls(ev, &roots, NOW_NS, &rt, &strict, &rt.channel_binding),
        Err(RaTlsError::Policy(
            AegisVerifyError::SupplyChainNotRegisterMeasured(TeeArch::AmdSevSnp)
        ))
    );
    assert!(verify_ra_tls(ev, &roots, NOW_NS, &rt, &policy(), &rt.channel_binding).is_ok());
}

#[test]
fn supply_chain_digests_use_loom_cas_identities() {
    let (loom, olean) =
        vesper_aegis::claims::supply_chain_digests(b"[lock]\n", b"theorem t : True");
    assert_eq!(
        loom,
        vesper_loom_cas_digest(vesper_loom_cas::Domain::Artifact, b"[lock]\n")
    );
    assert_ne!(loom, olean);
}

fn vesper_loom_cas_digest(d: vesper_loom_cas::Domain, b: &[u8]) -> [u8; 32] {
    vesper_loom_cas::blob_id(d, b).digest.0
}
