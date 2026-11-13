//! Normalized attestation claims (design §33.2) and the evidence-independent
//! bindings every format shares: report-data derivation and runtime
//! measurement register replay.

use crate::error::{Format, ParseError};
use crate::reader::Reader;
use ring::digest;

pub const ENCLAVE_FLAG_DEBUG_DISABLED: u32 = 1 << 0;
pub const ENCLAVE_FLAG_SMT_MITIGATED: u32 = 1 << 1;
pub const ENCLAVE_FLAG_LEAN_VERIFIED: u32 = 1 << 2;
const KNOWN_FLAGS: u32 =
    ENCLAVE_FLAG_DEBUG_DISABLED | ENCLAVE_FLAG_SMT_MITIGATED | ENCLAVE_FLAG_LEAN_VERIFIED;

/// Size of [`AegisAttestationClaimsWire`]: 2+2+4+48+32+32+8 = 128, as the
/// design states; the fields are consistent with the stated size.
pub const CLAIMS_WIRE_LEN: usize = 128;

/// Domain separator for the report-data binding (`report_data[0..48]`).
pub const REPORT_DATA_DOMAIN: &[u8] = b"vesper-aegis/report-data/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum TeeArch {
    IntelTdx15 = 1,
    AmdSevSnp = 2,
    ArmCcaRealm = 3,
}

impl TeeArch {
    pub fn from_tag(tag: u16) -> Option<Self> {
        match tag {
            1 => Some(TeeArch::IntelTdx15),
            2 => Some(TeeArch::AmdSevSnp),
            3 => Some(TeeArch::ArmCcaRealm),
            _ => None,
        }
    }
}

/// How the Loom.lock and `.olean` digests reached the verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplyChainEvidence {
    /// Hardware runtime measurement registers (TDX RTMR[2]/RTMR[3], CCA
    /// REM[2]/REM[3]) replayed to the claimed digests, *and* report data.
    RegisterReplay,
    /// Only the signed report data commits to the digests (SEV-SNP has no
    /// runtime measurement registers). The commitment is made by the
    /// measured guest software, not by hardware extend operations.
    ReportDataOnly,
}

/// Digests a Vesper guest claims about its own build. They are untrusted
/// until a format verifier checks them against signed evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestRuntimeClaims {
    /// SHA-384 of the RA-TLS channel material (TLS key or handshake
    /// transcript) the relying party will compare against.
    pub channel_binding: [u8; 48],
    /// Loom.lock Merkle digest (BLAKE3).
    pub loom_lock_blake3: [u8; 32],
    /// Lean theorem-interface fingerprint of the shipped `.olean` files.
    pub olean_spec_mif: [u8; 32],
}

impl GuestRuntimeClaims {
    /// The 64-byte report data a Vesper guest requests from hardware:
    /// `SHA-384(REPORT_DATA_DOMAIN || channel_binding || loom || olean) || 0^16`.
    pub fn report_data(&self) -> [u8; 64] {
        let mut ctx = digest::Context::new(&digest::SHA384);
        ctx.update(REPORT_DATA_DOMAIN);
        ctx.update(&self.channel_binding);
        ctx.update(&self.loom_lock_blake3);
        ctx.update(&self.olean_spec_mif);
        let mut out = [0u8; 64];
        out[..48].copy_from_slice(ctx.finish().as_ref());
        out
    }
}

/// Loom.lock and theorem-interface digests computed with the Loom CAS
/// object identities (`vesper-loom-cas`), so the values match what Loom
/// stores for the same bytes.
pub fn supply_chain_digests(loom_lock: &[u8], theorem_interface: &[u8]) -> ([u8; 32], [u8; 32]) {
    use vesper_loom_cas::{Domain, blob_id};
    (
        blob_id(Domain::Artifact, loom_lock).digest.0,
        blob_id(Domain::Interface, theorem_interface).digest.0,
    )
}

/// Hash algorithm of a runtime measurement register bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterHash {
    Sha256,
    Sha384,
    Sha512,
}

impl RegisterHash {
    pub fn len(self) -> usize {
        match self {
            RegisterHash::Sha256 => 32,
            RegisterHash::Sha384 => 48,
            RegisterHash::Sha512 => 64,
        }
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub(crate) fn algorithm(self) -> &'static digest::Algorithm {
        match self {
            RegisterHash::Sha256 => &digest::SHA256,
            RegisterHash::Sha384 => &digest::SHA384,
            RegisterHash::Sha512 => &digest::SHA512,
        }
    }
}

/// One extend of a zero-initialised register with a 32-byte digest:
/// `H(0^n || digest || 0^(pad - 32))`, where the extend data is padded to
/// 48 bytes for TDX RTMRs and to 64 bytes for CCA REMs. The result has
/// length `hash.len()`; the caller compares it with the reported register.
pub fn single_extend(hash: RegisterHash, pad_to: usize, data: &[u8; 32]) -> Vec<u8> {
    let mut ctx = digest::Context::new(hash.algorithm());
    ctx.update(&vec![0u8; hash.len()]);
    ctx.update(data);
    ctx.update(&vec![0u8; pad_to.saturating_sub(32)]);
    ctx.finish().as_ref().to_vec()
}

/// Constant-time byte equality.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Raw AMD SEV-SNP TCB_VERSION components (Milan/Genoa layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SnpTcbVersion {
    pub bootloader: u8,
    pub tee: u8,
    pub snp: u8,
    pub microcode: u8,
}

impl SnpTcbVersion {
    pub fn from_u64(raw: u64) -> Self {
        let b = raw.to_le_bytes();
        SnpTcbVersion {
            bootloader: b[0],
            tee: b[1],
            snp: b[6],
            microcode: b[7],
        }
    }

    pub fn to_u64(self) -> u64 {
        u64::from_le_bytes([
            self.bootloader,
            self.tee,
            0,
            0,
            0,
            0,
            self.snp,
            self.microcode,
        ])
    }
}

/// Normalized claims from any of the three formats (design §33.5), after
/// signature, chain, report-data and register checks have passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationClaims {
    pub arch: TeeArch,
    /// TDX: `TEE_TCB_SVN[1] << 8 | TEE_TCB_SVN[0]` (TDX module major/minor).
    /// SNP: `snp_fw << 8 | tee` of the reported TCB. CCA: 0 (the token has no
    /// single security version; see the report).
    pub tcb_svn: u16,
    pub enclave_flags: u32,
    /// TDX MRTD, SNP launch measurement, or for CCA
    /// [`cca_rim_measurement`](crate::cca::cca_rim_measurement) of the RIM.
    pub mr_enclave_sha384: [u8; 48],
    pub loom_lock_blake3: [u8; 32],
    pub olean_spec_mif: [u8; 32],
    pub chan_binding_prefix: [u8; 8],
    /// Full channel binding (the wire struct carries only its prefix).
    pub channel_binding: [u8; 48],
    pub supply_chain_evidence: SupplyChainEvidence,
    /// SEV-SNP only: raw reported TCB and VMPL.
    pub snp_reported_tcb: Option<SnpTcbVersion>,
    pub snp_vmpl: Option<u32>,
}

impl AttestationClaims {
    pub(crate) fn from_runtime(
        arch: TeeArch,
        tcb_svn: u16,
        mut enclave_flags: u32,
        mr_enclave_sha384: [u8; 48],
        runtime: &GuestRuntimeClaims,
        supply_chain_evidence: SupplyChainEvidence,
    ) -> Self {
        if runtime.olean_spec_mif != [0u8; 32] {
            enclave_flags |= ENCLAVE_FLAG_LEAN_VERIFIED;
        }
        let mut chan_binding_prefix = [0u8; 8];
        chan_binding_prefix.copy_from_slice(&runtime.channel_binding[..8]);
        AttestationClaims {
            arch,
            tcb_svn,
            enclave_flags,
            mr_enclave_sha384,
            loom_lock_blake3: runtime.loom_lock_blake3,
            olean_spec_mif: runtime.olean_spec_mif,
            chan_binding_prefix,
            channel_binding: runtime.channel_binding,
            supply_chain_evidence,
            snp_reported_tcb: None,
            snp_vmpl: None,
        }
    }

    pub fn to_wire(&self) -> AegisAttestationClaimsWire {
        AegisAttestationClaimsWire {
            tee_arch_tag: self.arch as u16,
            tcb_svn: self.tcb_svn,
            enclave_flags: self.enclave_flags,
            mr_enclave_sha384: self.mr_enclave_sha384,
            loom_lock_blake3: self.loom_lock_blake3,
            olean_spec_mif: self.olean_spec_mif,
            chan_binding_prefix: self.chan_binding_prefix,
        }
    }
}

/// The padding-free 128-byte claim header of design §33.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AegisAttestationClaimsWire {
    pub tee_arch_tag: u16,
    pub tcb_svn: u16,
    pub enclave_flags: u32,
    pub mr_enclave_sha384: [u8; 48],
    pub loom_lock_blake3: [u8; 32],
    pub olean_spec_mif: [u8; 32],
    pub chan_binding_prefix: [u8; 8],
}

impl AegisAttestationClaimsWire {
    pub fn encode(&self) -> [u8; CLAIMS_WIRE_LEN] {
        let mut out = [0u8; CLAIMS_WIRE_LEN];
        out[0..2].copy_from_slice(&self.tee_arch_tag.to_le_bytes());
        out[2..4].copy_from_slice(&self.tcb_svn.to_le_bytes());
        out[4..8].copy_from_slice(&self.enclave_flags.to_le_bytes());
        out[8..56].copy_from_slice(&self.mr_enclave_sha384);
        out[56..88].copy_from_slice(&self.loom_lock_blake3);
        out[88..120].copy_from_slice(&self.olean_spec_mif);
        out[120..128].copy_from_slice(&self.chan_binding_prefix);
        out
    }

    /// Decode exactly 128 bytes. Unknown architecture tags and flag bits
    /// are rejected rather than ignored.
    pub fn decode(bytes: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(bytes, Format::ClaimsWire);
        let tee_arch_tag = r.u16("tee_arch_tag")?;
        if TeeArch::from_tag(tee_arch_tag).is_none() {
            return Err(ParseError::UnsupportedValue {
                format: Format::ClaimsWire,
                field: "tee_arch_tag",
                value: tee_arch_tag.into(),
            });
        }
        let tcb_svn = r.u16("tcb_svn")?;
        let enclave_flags = r.u32("enclave_flags")?;
        if enclave_flags & !KNOWN_FLAGS != 0 {
            return Err(ParseError::UnsupportedValue {
                format: Format::ClaimsWire,
                field: "enclave_flags",
                value: enclave_flags.into(),
            });
        }
        let wire = AegisAttestationClaimsWire {
            tee_arch_tag,
            tcb_svn,
            enclave_flags,
            mr_enclave_sha384: r.array("mr_enclave_sha384")?,
            loom_lock_blake3: r.array("loom_lock_blake3")?,
            olean_spec_mif: r.array("olean_spec_mif")?,
            chan_binding_prefix: r.array("chan_binding_prefix")?,
        };
        r.finish()?;
        Ok(wire)
    }
}
