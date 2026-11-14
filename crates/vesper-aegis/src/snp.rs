//! AMD SEV-SNP `ATTESTATION_REPORT` (1184 bytes), signed with ECDSA
//! P-384/SHA-384 by the VCEK or VLEK.
//!
//! Supported versions (SEV-SNP firmware ABI):
//!
//! - 2: the original layout;
//! - 3 and 4: add CPUID family/model/stepping at `0x188..0x18B` (version 4
//!   has the version-3 layout, as in AMD's firmware and VirTEE `sev`);
//! - 5: adds `LAUNCH_MIT_VECTOR` (`0x1F8`) and `CURRENT_MIT_VECTOR`
//!   (`0x200`);
//! - 6: adds the extended TCBs `CURRENT_ETCB` (`0x220`), `LAUNCH_ETCB`
//!   (`0x240`) and `COMMITTED_ETCB` (`0x260`), 32 bytes each, which only
//!   "Venice" processors (family `1Ah`, models `50h..5Fh` and `C0h..CFh`)
//!   report; on every other processor they must be zero. Each ETCB holds
//!   fourteen one-byte SVNs (bits 111:0); bits 255:112 are reserved. The
//!   layout follows AMD's SEV-SNP firmware ABI and VirTEE `sev` 7.x
//!   (`EtcbVersion`, `ReportVariant::V6`).
//!
//! Versions above 6 and below 2 are rejected with
//! [`ParseError::UnsupportedValue`]. Every reserved byte of the version's
//! layout must be zero.

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
pub const SNP_REPORT_MAX_VERSION: u32 = 6;
/// First version with the mitigation vectors.
pub const SNP_REPORT_VERSION_MIT_VECTORS: u32 = 5;
/// First version with the extended TCBs.
pub const SNP_REPORT_VERSION_ETCB: u32 = 6;
/// Length of one `ETCB_VERSION` field.
pub const SNP_ETCB_LEN: usize = 32;
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
    /// Version 6 on Venice only (`None` otherwise, where the bytes are
    /// zero): extended TCB now, at launch, and committed.
    pub current_etcb: Option<SnpEtcb>,
    pub launch_etcb: Option<SnpEtcb>,
    pub committed_etcb: Option<SnpEtcb>,
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
        r.reserved(24, "reserved5")?;
        let venice = version >= 3 && is_venice_cpuid(cpuid);
        let (current_etcb, launch_etcb, committed_etcb) =
            if version >= SNP_REPORT_VERSION_ETCB && venice {
                (
                    Some(SnpEtcb::parse(r.array("current_etcb")?)?),
                    Some(SnpEtcb::parse(r.array("launch_etcb")?)?),
                    Some(SnpEtcb::parse(r.array("committed_etcb")?)?),
                )
            } else {
                r.reserved(3 * SNP_ETCB_LEN, "reserved6 (ETCBs: v6 Venice only)")?;
                (None, None, None)
            };
        r.reserved(32, "reserved7")?;
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
            current_etcb,
            launch_etcb,
            committed_etcb,
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
        o.extend_from_slice(&[0; 24]);
        for etcb in [&self.current_etcb, &self.launch_etcb, &self.committed_etcb] {
            o.extend_from_slice(&etcb.map(SnpEtcb::to_bytes).unwrap_or([0; SNP_ETCB_LEN]));
        }
        o.extend_from_slice(&[0; 32]);
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

    /// Whether the CPUID fields (version 3 and later) name a "Venice"
    /// processor (family `1Ah`, model `50h..5Fh` or `C0h..CFh`, as in VirTEE
    /// `sev`'s `Generation::identify_cpu`).
    pub fn is_venice(&self) -> bool {
        self.version >= 3 && is_venice_cpuid(self.cpuid)
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

fn is_venice_cpuid(cpuid: [u8; 3]) -> bool {
    cpuid[0] == 0x1A && matches!(cpuid[1], 0x50..=0x5F | 0xC0..=0xCF)
}

/// An `ETCB_VERSION` (version-6 reports on Venice): fourteen one-byte SVNs,
/// little-endian from bit 0, in AMD's order. Bytes 14..32 are reserved and
/// must be zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct SnpEtcb {
    /// Bits 7:0, IP Key Manager (ASP) firmware.
    pub ip_key_manager: u8,
    /// Bits 15:8, MP1 (power management) firmware.
    pub mp1: u8,
    /// Bits 23:16, AMD Root of Trust FMC.
    pub art_fmc: u8,
    /// Bits 31:24, AMD Root of Trust runtime.
    pub art_rt: u8,
    /// Bits 39:32, High Availability and Debuggability firmware.
    pub had_driver: u8,
    /// Bits 47:40, boot driver (ASP).
    pub boot_driver: u8,
    /// Bits 55:48, PreEsid driver (ASP).
    pub pre_esid: u8,
    /// Bits 63:56, TMPM firmware.
    pub tmpm: u8,
    /// Bits 71:64, lowest microcode patch level of all cores.
    pub microcode: u8,
    /// Bits 79:72, SoC driver (ASP).
    pub soc_driver: u8,
    /// Bits 87:80, ASP OS driver.
    pub asp_os_driver: u8,
    /// Bits 95:88, FHP driver (ASP).
    pub fhp_driver: u8,
    /// Bits 103:96, DPE driver (ASP).
    pub dpe_driver: u8,
    /// Bits 111:104, AMD Root Guest.
    pub arg: u8,
}

impl SnpEtcb {
    pub fn parse(b: [u8; SNP_ETCB_LEN]) -> Result<Self, ParseError> {
        if b[14..].iter().any(|&x| x != 0) {
            return Err(ParseError::NonZeroReserved {
                format: Format::SnpReport,
                field: "ETCB bits 255:112",
            });
        }
        Ok(SnpEtcb {
            ip_key_manager: b[0],
            mp1: b[1],
            art_fmc: b[2],
            art_rt: b[3],
            had_driver: b[4],
            boot_driver: b[5],
            pre_esid: b[6],
            tmpm: b[7],
            microcode: b[8],
            soc_driver: b[9],
            asp_os_driver: b[10],
            fhp_driver: b[11],
            dpe_driver: b[12],
            arg: b[13],
        })
    }

    pub fn to_bytes(self) -> [u8; SNP_ETCB_LEN] {
        let mut b = [0u8; SNP_ETCB_LEN];
        b[..14].copy_from_slice(&self.svns());
        b
    }

    /// The fourteen SVNs in wire order (byte 0 first).
    pub fn svns(&self) -> [u8; 14] {
        [
            self.ip_key_manager,
            self.mp1,
            self.art_fmc,
            self.art_rt,
            self.had_driver,
            self.boot_driver,
            self.pre_esid,
            self.tmpm,
            self.microcode,
            self.soc_driver,
            self.asp_os_driver,
            self.fhp_driver,
            self.dpe_driver,
            self.arg,
        ]
    }

    /// Every component at least `floor`'s (a component-wise, not
    /// lexicographic, comparison).
    pub fn at_least(&self, floor: &SnpEtcb) -> bool {
        self.svns().iter().zip(floor.svns()).all(|(h, f)| *h >= f)
    }
}

fn unsupported(field: &'static str, value: u64) -> ParseError {
    ParseError::UnsupportedValue {
        format: Format::SnpReport,
        field,
        value,
    }
}
