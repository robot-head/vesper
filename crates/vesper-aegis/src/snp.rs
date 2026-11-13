//! AMD SEV-SNP `ATTESTATION_REPORT` (versions 2 and 3, 1184 bytes), signed
//! with ECDSA P-384/SHA-384 by the VCEK.

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
    /// Big-endian `r || s` (converted from the report's little-endian,
    /// zero-extended 72-byte components).
    pub signature_r_s: [u8; 96],
}

impl SnpReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(bytes, Format::SnpReport);
        let version = r.u32("version")?;
        if !(2..=3).contains(&version) {
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
        r.reserved(168, "reserved4")?;
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
        o.extend_from_slice(&[0; 168]);
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
}

/// Verify an SEV-SNP report end to end and normalize it.
///
/// `vcek_chain` is `[VCEK, ASK]` (DER); the ARK is the configured `root`.
/// Checks: chain to `root` at `now_ns`; VCEK P-384 signature over bytes
/// `0..0x2A0`; report data equals the Vesper binding of `runtime`. SNP has
/// no runtime measurement registers, so the supply-chain digests are bound
/// by report data only. Not checked: VLEK-signed reports (rejected), AMD
/// KDS TCB collateral and CRLs.
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

fn unsupported(field: &'static str, value: u64) -> ParseError {
    ParseError::UnsupportedValue {
        format: Format::SnpReport,
        field,
        value,
    }
}
