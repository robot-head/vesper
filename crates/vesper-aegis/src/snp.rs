//! AMD SEV-SNP `ATTESTATION_REPORT` (1184 bytes), signed with ECDSA
//! P-384/SHA-384 by the VCEK or VLEK.
//!
//! Supported versions (SEV-SNP firmware ABI):
//!
//! - 2: the original layout;
//! - 3 and 4: add CPUID family/model/stepping at `0x188..0x18B` (version 4
//!   has the version-3 layout, as in AMD's firmware and VirTEE `sev`);
//! - 5: adds `LAUNCH_MIT_VECTOR` (`0x1F8`) and `CURRENT_MIT_VECTOR`
//!   (`0x200`).
//!
//! Version 6 (Venice extended TCBs) and later, and versions below 2, are
//! rejected with [`ParseError::UnsupportedValue`]. Every reserved byte of
//! the version's layout must be zero.

use crate::amd::{SnpCollateral, SnpTcbBaseline, VerifiedSnpReport, verify_snp_collateral};
use crate::chain::{Curve, TrustRoot, verify_chain};
use crate::claims::{
    AttestationClaims, ENCLAVE_FLAG_DEBUG_DISABLED, ENCLAVE_FLAG_SMT_MITIGATED, GuestRuntimeClaims,
    SnpTcbVersion, SupplyChainEvidence, TeeArch, ct_eq,
};
use crate::error::{AttestError, Format, ParseError};
use crate::reader::Reader;

pub const SNP_REPORT_LEN: usize = 0x4A0;
pub const SNP_SIGNED_LEN: usize = 0x2A0;
pub const SNP_SIG_ALGO_ECDSA_P384_SHA384: u32 = 1;
/// Lowest and highest supported report versions.
pub const SNP_REPORT_MIN_VERSION: u32 = 2;
pub const SNP_REPORT_MAX_VERSION: u32 = 5;
/// First version with the mitigation vectors.
pub const SNP_REPORT_VERSION_MIT_VECTORS: u32 = 5;
/// Guest policy bit 16: SMT allowed.
pub const SNP_POLICY_SMT: u64 = 1 << 16;
/// Guest policy bit 19: debugging allowed.
pub const SNP_POLICY_DEBUG: u64 = 1 << 19;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnpReport {
    pub version: u32,
    pub guest_svn: u32,
    pub policy: u64,
    pub family_id: [u8; 16],
    pub image_id: [u8; 16],
    pub vmpl: u32,
    pub signature_algo: u32,
    pub current_tcb: u64,
    pub platform_info: u64,
    /// Bit 0 AUTHOR_KEY_EN, bit 1 MASK_CHIP_KEY, bits 2..=4 SIGNING_KEY.
    pub flags: u32,
    pub report_data: [u8; 64],
    pub measurement: [u8; 48],
    pub host_data: [u8; 32],
    pub id_key_digest: [u8; 48],
    pub author_key_digest: [u8; 48],
    pub report_id: [u8; 32],
    pub report_id_ma: [u8; 32],
    pub reported_tcb: u64,
    /// Version 3 only (zero in version 2): CPUID family, model, stepping.
    pub cpuid: [u8; 3],
    pub chip_id: [u8; 64],
    pub committed_tcb: u64,
    pub current_version: [u8; 3],
    pub committed_version: [u8; 3],
    pub launch_tcb: u64,
    /// Version 5 and later (zero before): mitigation vector at launch and
    /// now.
    pub launch_mit_vector: u64,
    pub current_mit_vector: u64,
    /// Big-endian `r || s` (converted from the report's little-endian,
    /// zero-extended 72-byte components).
    pub signature_r_s: [u8; 96],
}

impl SnpReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(bytes, Format::SnpReport);
        let version = r.u32("version")?;
        if !(SNP_REPORT_MIN_VERSION..=SNP_REPORT_MAX_VERSION).contains(&version) {
            return Err(unsupported("version", version.into()));
        }
        let guest_svn = r.u32("guest_svn")?;
        let policy = r.u64("policy")?;
        let family_id = r.array("family_id")?;
        let image_id = r.array("image_id")?;
        let vmpl = r.u32("vmpl")?;
        let signature_algo = r.u32("signature_algo")?;
        if signature_algo != SNP_SIG_ALGO_ECDSA_P384_SHA384 {
            return Err(unsupported("signature_algo", signature_algo.into()));
        }
        let current_tcb = r.u64("current_tcb")?;
        let platform_info = r.u64("platform_info")?;
        let flags = r.u32("flags")?;
        if flags & !0x1F != 0 {
            return Err(unsupported("flags", flags.into()));
        }
        r.reserved(4, "reserved0")?;
        let report_data = r.array("report_data")?;
        let measurement = r.array("measurement")?;
        let host_data = r.array("host_data")?;
        let id_key_digest = r.array("id_key_digest")?;
        let author_key_digest = r.array("author_key_digest")?;
        let report_id = r.array("report_id")?;
        let report_id_ma = r.array("report_id_ma")?;
        let reported_tcb = r.u64("reported_tcb")?;
        let cpuid: [u8; 3] = r.array("cpuid")?;
        if version == 2 && cpuid != [0; 3] {
            return Err(ParseError::NonZeroReserved {
                format: Format::SnpReport,
                field: "reserved1 (v2 cpuid)",
            });
        }
        r.reserved(21, "reserved1")?;
        let chip_id = r.array("chip_id")?;
        let committed_tcb = r.u64("committed_tcb")?;
        let current_version: [u8; 3] = r.array("current_version")?;
        r.reserved(1, "reserved2")?;
        let committed_version: [u8; 3] = r.array("committed_version")?;
        r.reserved(1, "reserved3")?;
        let launch_tcb = r.u64("launch_tcb")?;
        let (launch_mit_vector, current_mit_vector) = if version >= SNP_REPORT_VERSION_MIT_VECTORS {
            (r.u64("launch_mit_vector")?, r.u64("current_mit_vector")?)
        } else {
            r.reserved(16, "reserved4 (pre-v5 mitigation vectors)")?;
            (0, 0)
        };
        r.reserved(152, "reserved5")?;
        debug_assert_eq!(r.position(), SNP_SIGNED_LEN);
        let r_le: [u8; 72] = r.array("signature.r")?;
        let s_le: [u8; 72] = r.array("signature.s")?;
        r.reserved(368, "signature.reserved")?;
        r.finish()?;
        let mut signature_r_s = [0u8; 96];
        for (dst, src) in [(0usize, &r_le), (48, &s_le)] {
            if src[48..].iter().any(|&b| b != 0) {
                return Err(ParseError::NonZeroReserved {
                    format: Format::SnpReport,
                    field: "signature component high bytes",
                });
            }
            for i in 0..48 {
                signature_r_s[dst + i] = src[47 - i];
            }
        }
        Ok(SnpReport {
            version,
            guest_svn,
            policy,
            family_id,
            image_id,
            vmpl,
            signature_algo,
            current_tcb,
            platform_info,
            flags,
            report_data,
            measurement,
            host_data,
            id_key_digest,
            author_key_digest,
            report_id,
            report_id_ma,
            reported_tcb,
            cpuid,
            chip_id,
            committed_tcb,
            current_version,
            committed_version,
            launch_tcb,
            launch_mit_vector,
            current_mit_vector,
            signature_r_s,
        })
    }

    /// Bytes `0x000..0x2A0`, which the VCEK signs.
    pub fn signed_bytes(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(SNP_SIGNED_LEN);
        o.extend_from_slice(&self.version.to_le_bytes());
        o.extend_from_slice(&self.guest_svn.to_le_bytes());
        o.extend_from_slice(&self.policy.to_le_bytes());
        o.extend_from_slice(&self.family_id);
        o.extend_from_slice(&self.image_id);
        o.extend_from_slice(&self.vmpl.to_le_bytes());
        o.extend_from_slice(&self.signature_algo.to_le_bytes());
        o.extend_from_slice(&self.current_tcb.to_le_bytes());
        o.extend_from_slice(&self.platform_info.to_le_bytes());
        o.extend_from_slice(&self.flags.to_le_bytes());
        o.extend_from_slice(&[0; 4]);
        o.extend_from_slice(&self.report_data);
        o.extend_from_slice(&self.measurement);
        o.extend_from_slice(&self.host_data);
        o.extend_from_slice(&self.id_key_digest);
        o.extend_from_slice(&self.author_key_digest);
        o.extend_from_slice(&self.report_id);
        o.extend_from_slice(&self.report_id_ma);
        o.extend_from_slice(&self.reported_tcb.to_le_bytes());
        o.extend_from_slice(&self.cpuid);
        o.extend_from_slice(&[0; 21]);
        o.extend_from_slice(&self.chip_id);
        o.extend_from_slice(&self.committed_tcb.to_le_bytes());
        o.extend_from_slice(&self.current_version);
        o.push(0);
        o.extend_from_slice(&self.committed_version);
        o.push(0);
        o.extend_from_slice(&self.launch_tcb.to_le_bytes());
        o.extend_from_slice(&self.launch_mit_vector.to_le_bytes());
        o.extend_from_slice(&self.current_mit_vector.to_le_bytes());
        o.extend_from_slice(&[0; 152]);
        o
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = self.signed_bytes();
        for half in [&self.signature_r_s[..48], &self.signature_r_s[48..]] {
            o.extend(half.iter().rev());
            o.extend_from_slice(&[0; 24]);
        }
        o.extend_from_slice(&[0; 368]);
        o
    }

    /// `SIGNING_KEY` field: 0 = VCEK, 1 = VLEK, 7 = none.
    pub fn signing_key(&self) -> u32 {
        (self.flags >> 2) & 7
    }

    /// The processor family named by the report's CPUID fields (version 3
    /// and later), following AMD's KDS "Determining the Product Name":
    /// family `19h` model `01h` is Milan and model `11h` Genoa; family `1Ah`
    /// with extended model 0 or 1 is Turin. `None` for version 2 (no CPUID)
    /// and for any other processor.
    pub fn product(&self) -> Option<crate::roots::AmdProduct> {
        use crate::roots::AmdProduct;
        if self.version < 3 {
            return None;
        }
        let [family, model, _stepping] = self.cpuid;
        match (family, model) {
            (0x19, 0x01) => Some(AmdProduct::Milan),
            (0x19, 0x11) => Some(AmdProduct::Genoa),
            (0x1A, m) if m >> 4 <= 1 => Some(AmdProduct::Turin),
            _ => None,
        }
    }
}

/// Verify an SEV-SNP report end to end and normalize it.
///
/// `vcek_chain` is `[VCEK, ASK]` (DER); the ARK is the configured `root`.
/// Checks: chain to `root` at `now_ns`; VCEK P-384 signature over bytes
/// `0..0x2A0`; report data equals the Vesper binding of `runtime`. SNP has
/// no runtime measurement registers, so the supply-chain digests are bound
/// by report data only. Not checked: VLEK-signed reports (rejected), AMD
/// KDS TCB collateral and CRLs; use [`verify_snp_report_with_collateral`].
pub fn verify_snp_report(
    bytes: &[u8],
    vcek_chain: &[Vec<u8>],
    root: &TrustRoot,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    let report = SnpReport::parse(bytes)?;
    if report.signing_key() != 0 {
        return Err(AttestError::UnsupportedAlgorithm {
            what: "SNP signing key (only VCEK supported)",
            value: report.signing_key().into(),
        });
    }
    let (leaf, rest) = vcek_chain
        .split_first()
        .ok_or(AttestError::CertificateChain("empty VCEK chain".into()))?;
    let vcek = verify_chain(leaf, rest, root, now_ns, Curve::P384)?;
    vcek.verify_fixed(
        &report.signed_bytes(),
        &report.signature_r_s,
        "SNP report (VCEK)",
    )?;
    snp_vesper_claims(&report, runtime)
}

/// Check the Vesper report-data binding of a signature-verified report and
/// normalize it. SNP has no runtime measurement registers, so the
/// supply-chain digests are bound by report data only.
pub fn snp_vesper_claims(
    report: &SnpReport,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    if !ct_eq(&report.report_data, &runtime.report_data()) {
        return Err(AttestError::BindingMismatch {
            what: "SNP report data does not match runtime claims",
        });
    }
    let mut flags = 0;
    if report.policy & SNP_POLICY_DEBUG == 0 {
        flags |= ENCLAVE_FLAG_DEBUG_DISABLED;
    }
    if report.policy & SNP_POLICY_SMT == 0 {
        flags |= ENCLAVE_FLAG_SMT_MITIGATED;
    }
    let tcb = SnpTcbVersion::from_u64(report.reported_tcb);
    let mut claims = AttestationClaims::from_runtime(
        TeeArch::AmdSevSnp,
        u16::from(tcb.snp) << 8 | u16::from(tcb.tee),
        flags,
        report.measurement,
        runtime,
        SupplyChainEvidence::ReportDataOnly,
    );
    claims.snp_reported_tcb = Some(tcb);
    claims.snp_vmpl = Some(report.vmpl);
    Ok(claims)
}

/// [`crate::amd::verify_snp_collateral`] (VCEK or VLEK chain, optional
/// CRL, certificate-to-report binding, optional TCB baseline) followed by
/// the Vesper binding of [`snp_vesper_claims`].
pub fn verify_snp_report_with_collateral(
    bytes: &[u8],
    collateral: &SnpCollateral,
    root: &TrustRoot,
    now_ns: u64,
    baseline: Option<&SnpTcbBaseline>,
    runtime: &GuestRuntimeClaims,
) -> Result<(AttestationClaims, VerifiedSnpReport), AttestError> {
    let verified = verify_snp_collateral(bytes, collateral, root, now_ns, baseline)?;
    let claims = snp_vesper_claims(&verified.report, runtime)?;
    Ok((claims, verified))
}

fn unsupported(field: &'static str, value: u64) -> ParseError {
    ParseError::UnsupportedValue {
        format: Format::SnpReport,
        field,
        value,
    }
}
