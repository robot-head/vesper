//! RA-TLS relying-party policy: each rule rejects, in the design's order.

use vesper_aegis::claims::{SnpTcbVersion, SupplyChainEvidence};
use vesper_aegis::policy::TcbFloors;
use vesper_aegis::*;

fn claims(arch: TeeArch) -> AttestationClaims {
    AttestationClaims {
        arch,
        tcb_svn: 10,
        enclave_flags: ENCLAVE_FLAG_DEBUG_DISABLED,
        mr_enclave_sha384: [1; 48],
        loom_lock_blake3: [2; 32],
        olean_spec_mif: [3; 32],
        chan_binding_prefix: [4; 8],
        channel_binding: [4; 48],
        supply_chain_evidence: SupplyChainEvidence::RegisterReplay,
        snp_reported_tcb: (arch == TeeArch::AmdSevSnp).then_some(SnpTcbVersion {
            bootloader: 2,
            tee: 0,
            snp: 10,
            microcode: 100,
        }),
        snp_vmpl: (arch == TeeArch::AmdSevSnp).then_some(0),
    }
}

fn policy() -> RelyingPartyPolicy {
    RelyingPartyPolicy {
        allowed_archs: vec![TeeArch::IntelTdx15, TeeArch::AmdSevSnp],
        tcb_floors: TcbFloors {
            tdx: 10,
            snp: 10,
            cca: 0,
        },
        snp_component_floors: None,
        snp_max_vmpl: 0,
        allowed_measurements: vec![
            (TeeArch::IntelTdx15, [1; 48]),
            (TeeArch::AmdSevSnp, [1; 48]),
        ],
        expected_loom_blake3: [2; 32],
        expected_olean_mif: [3; 32],
        require_register_replay: false,
    }
}

const BINDING: [u8; 48] = [4; 48];

#[test]
fn every_rule_rejects() {
    use AegisVerifyError::*;
    let p = policy();
    let v =
        |c: &AttestationClaims, p: &RelyingPartyPolicy| verify_attestation_claims(c, p, &BINDING);
    assert_eq!(v(&claims(TeeArch::IntelTdx15), &p), Ok(()));
    assert_eq!(
        v(&claims(TeeArch::ArmCcaRealm), &p),
        Err(DisallowedTeeArchitecture(TeeArch::ArmCcaRealm))
    );
    let mut c = claims(TeeArch::IntelTdx15);
    c.enclave_flags = 0;
    assert_eq!(v(&c, &p), Err(EnclaveDebugModeEnabled));
    let mut c = claims(TeeArch::IntelTdx15);
    c.tcb_svn = 9;
    assert_eq!(
        v(&c, &p),
        Err(StaleTcbSvn {
            actual: 9,
            min_required: 10
        })
    );
    let mut c = claims(TeeArch::IntelTdx15);
    c.mr_enclave_sha384[47] ^= 1;
    assert_eq!(v(&c, &p), Err(BootMeasurementMismatch));
    // An allowlisted measurement for another architecture does not count.
    let mut q = p.clone();
    q.allowed_measurements = vec![(TeeArch::AmdSevSnp, [1; 48])];
    assert_eq!(
        v(&claims(TeeArch::IntelTdx15), &q),
        Err(BootMeasurementMismatch)
    );
    let mut c = claims(TeeArch::IntelTdx15);
    c.loom_lock_blake3[0] ^= 1;
    assert_eq!(v(&c, &p), Err(SupplyChainLoomLockMismatch));
    let mut c = claims(TeeArch::IntelTdx15);
    c.olean_spec_mif[0] ^= 1;
    assert_eq!(v(&c, &p), Err(FormalOleanProofDigestMismatch));
    // Same 8-byte prefix, different full binding: rejected (the design's
    // 8-byte prefix alone would accept this).
    let mut c = claims(TeeArch::IntelTdx15);
    c.channel_binding[40] ^= 1;
    assert_eq!(v(&c, &p), Err(ChannelTranscriptBindingMismatch));
    // Order: debug is reported before a stale TCB.
    let mut c = claims(TeeArch::IntelTdx15);
    c.enclave_flags = 0;
    c.tcb_svn = 0;
    assert_eq!(v(&c, &p), Err(EnclaveDebugModeEnabled));
}

#[test]
fn snp_specific_rules() {
    use AegisVerifyError::*;
    let mut p = policy();
    let mut c = claims(TeeArch::AmdSevSnp);
    assert_eq!(verify_attestation_claims(&c, &p, &BINDING), Ok(()));
    c.snp_vmpl = Some(2);
    assert_eq!(
        verify_attestation_claims(&c, &p, &BINDING),
        Err(DisallowedVmpl {
            actual: 2,
            max_allowed: 0
        })
    );
    let c = claims(TeeArch::AmdSevSnp);
    p.snp_component_floors = Some(SnpTcbVersion {
        bootloader: 2,
        tee: 0,
        snp: 10,
        microcode: 101,
    });
    assert_eq!(
        verify_attestation_claims(&c, &p, &BINDING),
        Err(StaleSnpTcbComponent {
            component: "microcode",
            actual: 100,
            min_required: 101
        })
    );
    p.snp_component_floors = None;
    p.require_register_replay = true;
    let mut c = claims(TeeArch::AmdSevSnp);
    c.supply_chain_evidence = SupplyChainEvidence::ReportDataOnly;
    assert_eq!(
        verify_attestation_claims(&c, &p, &BINDING),
        Err(SupplyChainNotRegisterMeasured(TeeArch::AmdSevSnp))
    );
}

/// Exhaustive check over small domains against an independent
/// re-statement of the policy (the same statement as Lean `checkPolicy`).
#[test]
fn policy_matches_reference_on_small_domain() {
    let p = policy();
    for arch in [
        TeeArch::IntelTdx15,
        TeeArch::AmdSevSnp,
        TeeArch::ArmCcaRealm,
    ] {
        for debug_off in [false, true] {
            for tcb in [9u16, 10, 11] {
                for m in [0u8, 1] {
                    for loom in [2u8, 5] {
                        for olean in [3u8, 6] {
                            for bind in [4u8, 7] {
                                let mut c = claims(arch);
                                c.enclave_flags = if debug_off {
                                    ENCLAVE_FLAG_DEBUG_DISABLED
                                } else {
                                    0
                                };
                                c.tcb_svn = tcb;
                                c.mr_enclave_sha384 = [m; 48];
                                c.loom_lock_blake3 = [loom; 32];
                                c.olean_spec_mif = [olean; 32];
                                c.channel_binding = [bind; 48];
                                c.chan_binding_prefix = [bind; 8];
                                let ok = verify_attestation_claims(&c, &p, &BINDING).is_ok();
                                let reference = arch != TeeArch::ArmCcaRealm
                                    && debug_off
                                    && tcb >= 10
                                    && m == 1
                                    && loom == 2
                                    && olean == 3
                                    && bind == 4;
                                assert_eq!(ok, reference);
                            }
                        }
                    }
                }
            }
        }
    }
}
