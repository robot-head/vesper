//! RA-TLS relying-party policy (design §33.5) and the end-to-end verifier
//! that combines format verification with policy.

use crate::cca::verify_cca_token;
use crate::chain::TrustRoot;
use crate::claims::{
    AttestationClaims, GuestRuntimeClaims, SnpTcbVersion, SupplyChainEvidence, TeeArch,
};
#[cfg(not(vesper_port))]
use crate::claims::{ENCLAVE_FLAG_DEBUG_DISABLED, ct_eq};
use crate::error::{AegisVerifyError, AttestError};
use crate::snp::verify_snp_report;
use crate::tdx::verify_tdx_quote;
use ring::digest;

/// Minimum normalized TCB SVN per architecture. The design has one floor;
/// the normalized values are not comparable across vendors, so the floor
/// is per architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TcbFloors {
    pub tdx: u16,
    pub snp: u16,
    pub cca: u16,
}

impl TcbFloors {
    pub fn for_arch(&self, arch: TeeArch) -> u16 {
        match arch {
            TeeArch::IntelTdx15 => self.tdx,
            TeeArch::AmdSevSnp => self.snp,
            TeeArch::ArmCcaRealm => self.cca,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelyingPartyPolicy {
    pub allowed_archs: Vec<TeeArch>,
    pub tcb_floors: TcbFloors,
    /// Optional per-component floors on the raw SEV-SNP reported TCB.
    pub snp_component_floors: Option<SnpTcbVersion>,
    /// Highest SEV-SNP VMPL accepted (0 = most privileged only).
    pub snp_max_vmpl: u32,
    /// Allowlist of `(architecture, normalized boot measurement)`.
    pub allowed_measurements: Vec<(TeeArch, [u8; 48])>,
    pub expected_loom_blake3: [u8; 32],
    pub expected_olean_mif: [u8; 32],
    /// Reject evidence whose supply-chain digests are not hardware
    /// register measured (SEV-SNP binds them through report data only).
    pub require_register_replay: bool,
}

/// Check normalized claims against `policy`. `expected_channel_binding` is
/// the SHA-384 binding of the TLS key or transcript the relying party
/// observed; it is compared in full and in constant time.
///
/// Check order follows the design: architecture, debug, TCB floor, boot
/// measurement, Loom.lock, `.olean`, channel binding; then the SNP-only
/// checks and register-replay requirement. `lean/AegisToctou.lean` models
/// The policy words before the allowed architectures, the policy bytes
/// before the allowed measurements, and a measurement's size
/// (vesper/src/claims.ves `PW_ARCHS`, `PB_MEASUREMENTS`).
#[cfg(vesper_port)]
const PW_HEADER_WORDS: usize = 8;
#[cfg(vesper_port)]
const PB_HEADER_BYTES: usize = 64;
#[cfg(vesper_port)]
const MEASUREMENT_LEN: usize = 48;

/// this function (`checkPolicy`).
#[cfg(vesper_port)]
pub fn verify_attestation_claims(
    claims: &AttestationClaims,
    policy: &RelyingPartyPolicy,
    expected_channel_binding: &[u8; 48],
) -> Result<(), AegisVerifyError> {
    // The word and byte layouts are documented at `check_policy` in
    // vesper/src/claims.ves.
    let cw = [
        claims.arch as u64,
        claims.tcb_svn.into(),
        claims.enclave_flags.into(),
        u64::from(claims.supply_chain_evidence == SupplyChainEvidence::RegisterReplay),
        u64::from(claims.snp_vmpl.is_some()),
        claims.snp_vmpl.unwrap_or(0).into(),
        u64::from(claims.snp_reported_tcb.is_some()),
        claims.snp_reported_tcb.unwrap_or_default().to_u64(),
    ];
    let mut cb = [0u8; 168];
    cb[..48].copy_from_slice(&claims.mr_enclave_sha384);
    cb[48..80].copy_from_slice(&claims.loom_lock_blake3);
    cb[80..112].copy_from_slice(&claims.olean_spec_mif);
    cb[112..120].copy_from_slice(&claims.chan_binding_prefix);
    cb[120..].copy_from_slice(&claims.channel_binding);
    let floors = &policy.tcb_floors;
    let mut pw = Vec::with_capacity(
        PW_HEADER_WORDS + policy.allowed_archs.len() + policy.allowed_measurements.len(),
    );
    pw.extend([
        floors.tdx.into(),
        floors.snp.into(),
        floors.cca.into(),
        u64::from(policy.snp_component_floors.is_some()),
        policy.snp_component_floors.unwrap_or_default().to_u64(),
        policy.snp_max_vmpl.into(),
        u64::from(policy.require_register_replay),
        policy.allowed_archs.len() as u64,
    ]);
    pw.extend(policy.allowed_archs.iter().map(|a| *a as u64));
    pw.extend(policy.allowed_measurements.iter().map(|(a, _)| *a as u64));
    let mut pb =
        Vec::with_capacity(PB_HEADER_BYTES + MEASUREMENT_LEN * policy.allowed_measurements.len());
    pb.extend_from_slice(&policy.expected_loom_blake3);
    pb.extend_from_slice(&policy.expected_olean_mif);
    for (_, m) in &policy.allowed_measurements {
        pb.extend_from_slice(m);
    }
    crate::port::vesper::verify_attestation_claims(&cw, &cb, &pw, &pb, expected_channel_binding)
        .map_err(crate::port::verify_error)
}

/// Check normalized claims against `policy`. `expected_channel_binding` is
/// the SHA-384 binding of the TLS key or transcript the relying party
/// observed; it is compared in full and in constant time.
#[cfg(not(vesper_port))]
pub fn verify_attestation_claims(
    claims: &AttestationClaims,
    policy: &RelyingPartyPolicy,
    expected_channel_binding: &[u8; 48],
) -> Result<(), AegisVerifyError> {
    if !policy.allowed_archs.contains(&claims.arch) {
        return Err(AegisVerifyError::DisallowedTeeArchitecture(claims.arch));
    }
    if claims.enclave_flags & ENCLAVE_FLAG_DEBUG_DISABLED == 0 {
        return Err(AegisVerifyError::EnclaveDebugModeEnabled);
    }
    let floor = policy.tcb_floors.for_arch(claims.arch);
    if claims.tcb_svn < floor {
        return Err(AegisVerifyError::StaleTcbSvn {
            actual: claims.tcb_svn,
            min_required: floor,
        });
    }
    if !policy
        .allowed_measurements
        .iter()
        .any(|(arch, m)| *arch == claims.arch && ct_eq(m, &claims.mr_enclave_sha384))
    {
        return Err(AegisVerifyError::BootMeasurementMismatch);
    }
    if !ct_eq(&claims.loom_lock_blake3, &policy.expected_loom_blake3) {
        return Err(AegisVerifyError::SupplyChainLoomLockMismatch);
    }
    if !ct_eq(&claims.olean_spec_mif, &policy.expected_olean_mif) {
        return Err(AegisVerifyError::FormalOleanProofDigestMismatch);
    }
    if !ct_eq(&claims.channel_binding, expected_channel_binding)
        || claims.chan_binding_prefix[..] != expected_channel_binding[..8]
    {
        return Err(AegisVerifyError::ChannelTranscriptBindingMismatch);
    }
    if claims.arch == TeeArch::AmdSevSnp {
        let vmpl = claims.snp_vmpl.unwrap_or(u32::MAX);
        if vmpl > policy.snp_max_vmpl {
            return Err(AegisVerifyError::DisallowedVmpl {
                actual: vmpl,
                max_allowed: policy.snp_max_vmpl,
            });
        }
        if let Some(min) = policy.snp_component_floors {
            let actual = claims.snp_reported_tcb.unwrap_or_default();
            for (component, a, m) in [
                ("bootloader", actual.bootloader, min.bootloader),
                ("tee", actual.tee, min.tee),
                ("snp", actual.snp, min.snp),
                ("microcode", actual.microcode, min.microcode),
            ] {
                if a < m {
                    return Err(AegisVerifyError::StaleSnpTcbComponent {
                        component,
                        actual: a,
                        min_required: m,
                    });
                }
            }
        }
    }
    if policy.require_register_replay
        && claims.supply_chain_evidence != SupplyChainEvidence::RegisterReplay
    {
        return Err(AegisVerifyError::SupplyChainNotRegisterMeasured(
            claims.arch,
        ));
    }
    Ok(())
}

/// Channel binding for an RA-TLS server key:
/// `SHA-384("vesper-aegis/ra-tls-key/v1\0" || SubjectPublicKeyInfo DER)`.
pub fn tls_key_channel_binding(spki_der: &[u8]) -> [u8; 48] {
    let mut ctx = digest::Context::new(&digest::SHA384);
    ctx.update(b"vesper-aegis/ra-tls-key/v1\0");
    ctx.update(spki_der);
    let mut out = [0u8; 48];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

/// Hardware evidence as received from the attester.
#[derive(Clone, Copy, Debug)]
pub enum Evidence<'a> {
    Tdx {
        quote: &'a [u8],
    },
    Snp {
        report: &'a [u8],
        /// `[VCEK, ASK]` DER.
        vcek_chain: &'a [Vec<u8>],
    },
    Cca {
        token: &'a [u8],
        /// IAK certificate chain, leaf first.
        iak_chain: &'a [Vec<u8>],
    },
}

/// Configured roots, one per vendor.
#[derive(Clone, Debug, Default)]
pub struct TrustRoots {
    pub tdx: Option<TrustRoot>,
    pub snp: Option<TrustRoot>,
    pub cca: Option<TrustRoot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaTlsError {
    /// No root is configured for this evidence's architecture.
    NoTrustRoot(TeeArch),
    Evidence(AttestError),
    Policy(AegisVerifyError),
}

impl std::fmt::Display for RaTlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for RaTlsError {}

/// Verify evidence, normalize it, and apply `policy` with the channel
/// binding observed on the TLS connection.
pub fn verify_ra_tls(
    evidence: Evidence<'_>,
    roots: &TrustRoots,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
    policy: &RelyingPartyPolicy,
    observed_channel_binding: &[u8; 48],
) -> Result<AttestationClaims, RaTlsError> {
    let need = |root: &Option<TrustRoot>, arch| root.clone().ok_or(RaTlsError::NoTrustRoot(arch));
    let claims = match evidence {
        Evidence::Tdx { quote } => {
            let root = need(&roots.tdx, TeeArch::IntelTdx15)?;
            verify_tdx_quote(quote, &root, now_ns, runtime)
        }
        Evidence::Snp { report, vcek_chain } => {
            let root = need(&roots.snp, TeeArch::AmdSevSnp)?;
            verify_snp_report(report, vcek_chain, &root, now_ns, runtime)
        }
        Evidence::Cca { token, iak_chain } => {
            let root = need(&roots.cca, TeeArch::ArmCcaRealm)?;
            verify_cca_token(token, iak_chain, &root, now_ns, runtime)
        }
    }
    .map_err(RaTlsError::Evidence)?;
    verify_attestation_claims(&claims, policy, observed_channel_binding)
        .map_err(RaTlsError::Policy)?;
    Ok(claims)
}
