//! Intel TDX DCAP quotes (ECDSA-256 attestation key, certification data
//! type 6 wrapping a type-5 PCK chain):
//!
//! - version 4: header, TD 1.0 report body (584 bytes);
//! - version 5: header, then a body descriptor (`type u16`, `size u32`)
//!   and one of the TD report bodies of Intel's `sgx_quote_5.h`: type 2
//!   TD 1.0 (584 bytes), type 3 TD 1.5 (648: adds `TEE_TCB_SVN_2` and
//!   `MRSERVICETD`), type 4 TD 1.5 "ex" (885: adds the VM index, TD ID,
//!   device-info hash, initial/current service-TD bindings and the initial
//!   platform TCB). The size must be the type's exact size. Type 1 (SGX
//!   enclave report) and unknown types are rejected.
//!
//! For v5 the attestation key signs header, descriptor and body. TCB
//! evaluation uses `TEE_TCB_SVN` for every body type, as Intel's QVL does
//! (`TEE_TCB_SVN_2` reflects TD-preserving module updates and is reported,
//! not appraised).

use crate::chain::{Curve, EcPublicKey, TrustRoot, pem_certificates, verify_chain};
use crate::claims::{
    AttestationClaims, ENCLAVE_FLAG_DEBUG_DISABLED, GuestRuntimeClaims, RegisterHash,
    SupplyChainEvidence, TeeArch, ct_eq, single_extend,
};
use crate::error::{AttestError, ParseError};
use crate::intel::{TdxCollateral, VerifiedTdxQuote, verify_tdx_collateral};
use ring::digest;

pub const TDX_QUOTE_VERSION: u16 = 4;
pub const TDX_QUOTE_VERSION_5: u16 = 5;
/// Quote v5 body types.
pub const TDX_BODY_TYPE_SGX: u16 = 1;
pub const TDX_BODY_TYPE_TD10: u16 = 2;
pub const TDX_BODY_TYPE_TD15: u16 = 3;
pub const TDX_BODY_TYPE_TD15_EX: u16 = 4;
pub const TDX_BODY_TD15_LEN: usize = 648;
pub const TDX_BODY_TD15_EX_LEN: usize = 885;
pub const TDX_ATT_KEY_ECDSA_P256: u16 = 2;
pub const TDX_TEE_TYPE: u32 = 0x81;
pub const TDX_HEADER_LEN: usize = 48;
pub const TDX_BODY_LEN: usize = 584;
pub const SGX_REPORT_BODY_LEN: usize = 384;
pub const CERT_DATA_QE_REPORT: u16 = 6;
pub const CERT_DATA_PCK_CHAIN_PEM: u16 = 5;
/// Upper bound on the signature-data section; real quotes are ~5 KiB.
pub const MAX_SIG_DATA_LEN: usize = 64 * 1024;
/// A v5 body descriptor: `body_type` (u16) and `body_size` (u32).
const TDX_BODY_DESCRIPTOR_LEN: usize = 6;
/// `TDATTRIBUTES.DEBUG` (bit 0).
pub const TD_ATTRIBUTE_DEBUG: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxQuoteHeader {
    pub version: u16,
    pub att_key_type: u16,
    pub tee_type: u32,
    pub qe_svn: u16,
    pub pce_svn: u16,
    pub qe_vendor_id: [u8; 16],
    pub user_data: [u8; 20],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdReportBody {
    pub tee_tcb_svn: [u8; 16],
    pub mr_seam: [u8; 48],
    pub mr_signer_seam: [u8; 48],
    pub seam_attributes: u64,
    pub td_attributes: u64,
    pub xfam: u64,
    pub mr_td: [u8; 48],
    pub mr_config_id: [u8; 48],
    pub mr_owner: [u8; 48],
    pub mr_owner_config: [u8; 48],
    pub rtmr: [[u8; 48]; 4],
    pub report_data: [u8; 64],
}

/// Fields a TD 1.5 report body (quote v5 body type 3) adds after the TD
/// 1.0 body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Td15Fields {
    pub tee_tcb_svn2: [u8; 16],
    pub mr_service_td: [u8; 48],
}

/// Fields a TD 1.5 "ex" report body (quote v5 body type 4) adds after the
/// TD 1.5 fields (`sgx_report2_body_v1_5_ex_t`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Td15ExFields {
    pub vm_index: u8,
    pub td_id: [u8; 32],
    pub dev_info_hash: [u8; 48],
    pub init_service_td_hash: [u8; 48],
    pub init_service_td_attributes: [u8; 8],
    pub init_cpu_svn: [u8; 16],
    pub init_tee_tcb_svn: [u8; 16],
    pub init_tee_fmspc: [u8; 12],
    pub cur_service_td_hash: [u8; 48],
    pub cur_service_td_attributes: [u8; 8],
}

impl Td15Fields {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.tee_tcb_svn2);
        out.extend_from_slice(&self.mr_service_td);
    }
}

impl Td15ExFields {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.vm_index);
        out.extend_from_slice(&self.td_id);
        out.extend_from_slice(&self.dev_info_hash);
        out.extend_from_slice(&self.init_service_td_hash);
        out.extend_from_slice(&self.init_service_td_attributes);
        out.extend_from_slice(&self.init_cpu_svn);
        out.extend_from_slice(&self.init_tee_tcb_svn);
        out.extend_from_slice(&self.init_tee_fmspc);
        out.extend_from_slice(&self.cur_service_td_hash);
        out.extend_from_slice(&self.cur_service_td_attributes);
    }
}

/// The SGX report body of the Quoting Enclave (384 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QeReportBody {
    pub cpu_svn: [u8; 16],
    pub misc_select: u32,
    pub attributes: [u8; 16],
    pub mr_enclave: [u8; 32],
    pub mr_signer: [u8; 32],
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub report_data: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxQuote {
    pub header: TdxQuoteHeader,
    /// The TD 1.0 part of the report body (every body type starts with it).
    pub body: TdReportBody,
    /// Version 5, body types 3 and 4 only.
    pub td15: Option<Td15Fields>,
    /// Version 5, body type 4 only.
    pub td15_ex: Option<Td15ExFields>,
    /// ECDSA P-256 `r || s` over header and body.
    pub quote_signature: [u8; 64],
    /// Attestation public key `X || Y`.
    pub attestation_key: [u8; 64],
    pub qe_report: QeReportBody,
    /// PCK ECDSA P-256 `r || s` over the 384-byte QE report.
    pub qe_report_signature: [u8; 64],
    pub qe_auth_data: Vec<u8>,
    /// PEM PCK certificate chain, leaf first.
    pub pck_chain_pem: Vec<u8>,
}

impl TdxQuoteHeader {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.att_key_type.to_le_bytes());
        out.extend_from_slice(&self.tee_type.to_le_bytes());
        out.extend_from_slice(&self.qe_svn.to_le_bytes());
        out.extend_from_slice(&self.pce_svn.to_le_bytes());
        out.extend_from_slice(&self.qe_vendor_id);
        out.extend_from_slice(&self.user_data);
    }
}

impl TdReportBody {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.tee_tcb_svn);
        out.extend_from_slice(&self.mr_seam);
        out.extend_from_slice(&self.mr_signer_seam);
        out.extend_from_slice(&self.seam_attributes.to_le_bytes());
        out.extend_from_slice(&self.td_attributes.to_le_bytes());
        out.extend_from_slice(&self.xfam.to_le_bytes());
        out.extend_from_slice(&self.mr_td);
        out.extend_from_slice(&self.mr_config_id);
        out.extend_from_slice(&self.mr_owner);
        out.extend_from_slice(&self.mr_owner_config);
        for reg in &self.rtmr {
            out.extend_from_slice(reg);
        }
        out.extend_from_slice(&self.report_data);
    }
}

impl QeReportBody {
    /// The QE report fields of a port parse (vesper/src/tdx.ves `qe_report`).
    pub(crate) fn from_port(f: &mut crate::port::Fields<'_>) -> Self {
        QeReportBody {
            cpu_svn: f.array(),
            misc_select: f.u32(),
            attributes: f.array(),
            mr_enclave: f.array(),
            mr_signer: f.array(),
            isv_prod_id: f.u16(),
            isv_svn: f.u16(),
            report_data: f.array(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SGX_REPORT_BODY_LEN);
        out.extend_from_slice(&self.cpu_svn);
        out.extend_from_slice(&self.misc_select.to_le_bytes());
        out.extend_from_slice(&[0; 28]);
        out.extend_from_slice(&self.attributes);
        out.extend_from_slice(&self.mr_enclave);
        out.extend_from_slice(&[0; 32]);
        out.extend_from_slice(&self.mr_signer);
        out.extend_from_slice(&[0; 96]);
        out.extend_from_slice(&self.isv_prod_id.to_le_bytes());
        out.extend_from_slice(&self.isv_svn.to_le_bytes());
        out.extend_from_slice(&[0; 60]);
        out.extend_from_slice(&self.report_data);
        out
    }
}

impl TdxQuote {
    /// Bounds-checked parse; the input must contain exactly one quote.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Ok(Self::parse_port(bytes, crate::port::TAIL_EXACT)?.0)
    }

    /// Parse a quote from a fixed-size quote buffer: bytes after the
    /// quote's declared length must all be zero.
    pub fn parse_padded(bytes: &[u8]) -> Result<Self, ParseError> {
        Ok(Self::parse_port(bytes, crate::port::TAIL_PADDED)?.0)
    }

    /// Parse one quote from the start of `bytes`; returns it and the number
    /// of bytes it occupies (header, body, length and signature data).
    pub fn parse_prefix(bytes: &[u8]) -> Result<(Self, usize), ParseError> {
        Self::parse_port(bytes, crate::port::TAIL_PREFIX)
    }

    /// `mode`: 0 exact, 1 zero padded, 2 prefix (vesper/src/reader.ves).
    fn parse_port(bytes: &[u8], mode: u64) -> Result<(Self, usize), ParseError> {
        let (used, p) =
            crate::port::parse(bytes, |w, _| crate::port::vesper::tdx_parse(bytes, mode, w))?;
        let mut f = p.fields();
        let header = TdxQuoteHeader {
            version: f.u16(),
            att_key_type: f.u16(),
            tee_type: f.u32(),
            qe_svn: f.u16(),
            pce_svn: f.u16(),
            qe_vendor_id: f.array(),
            user_data: f.array(),
        };
        let body_type = f.u16();
        let body = TdReportBody {
            tee_tcb_svn: f.array(),
            mr_seam: f.array(),
            mr_signer_seam: f.array(),
            seam_attributes: f.word(),
            td_attributes: f.word(),
            xfam: f.word(),
            mr_td: f.array(),
            mr_config_id: f.array(),
            mr_owner: f.array(),
            mr_owner_config: f.array(),
            rtmr: [f.array(), f.array(), f.array(), f.array()],
            report_data: f.array(),
        };
        let td15 =
            (body_type == TDX_BODY_TYPE_TD15 || body_type == TDX_BODY_TYPE_TD15_EX).then(|| {
                Td15Fields {
                    tee_tcb_svn2: f.array(),
                    mr_service_td: f.array(),
                }
            });
        let td15_ex = (body_type == TDX_BODY_TYPE_TD15_EX).then(|| Td15ExFields {
            vm_index: f.u8(),
            td_id: f.array(),
            dev_info_hash: f.array(),
            init_service_td_hash: f.array(),
            init_service_td_attributes: f.array(),
            init_cpu_svn: f.array(),
            init_tee_tcb_svn: f.array(),
            init_tee_fmspc: f.array(),
            cur_service_td_hash: f.array(),
            cur_service_td_attributes: f.array(),
        });
        let quote = TdxQuote {
            header,
            body,
            td15,
            td15_ex,
            quote_signature: f.array(),
            attestation_key: f.array(),
            qe_report: QeReportBody::from_port(&mut f),
            qe_report_signature: f.array(),
            qe_auth_data: f.vec(),
            pck_chain_pem: f.vec(),
        };
        Ok((quote, used as usize))
    }

    /// The v5 body type (`None` for a v4 quote).
    pub fn body_type(&self) -> Option<u16> {
        if self.header.version != TDX_QUOTE_VERSION_5 {
            return None;
        }
        Some(match (&self.td15, &self.td15_ex) {
            (_, Some(_)) => TDX_BODY_TYPE_TD15_EX,
            (Some(_), None) => TDX_BODY_TYPE_TD15,
            (None, None) => TDX_BODY_TYPE_TD10,
        })
    }

    /// Header and body (with the v5 body descriptor): the bytes the
    /// attestation key signs.
    pub fn signed_bytes(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(TDX_HEADER_LEN + TDX_BODY_DESCRIPTOR_LEN + TDX_BODY_TD15_EX_LEN);
        self.header.encode(&mut out);
        let mut body = Vec::with_capacity(TDX_BODY_TD15_EX_LEN);
        self.body.encode(&mut body);
        if let Some(t) = &self.td15 {
            t.encode(&mut body);
        }
        if let Some(t) = &self.td15_ex {
            t.encode(&mut body);
        }
        if let Some(body_type) = self.body_type() {
            out.extend_from_slice(&body_type.to_le_bytes());
            out.extend_from_slice(
                &u32::try_from(body.len())
                    .expect("body fits u32")
                    .to_le_bytes(),
            );
        }
        out.extend_from_slice(&body);
        out
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut cert = self.qe_report.encode();
        cert.extend_from_slice(&self.qe_report_signature);
        cert.extend_from_slice(
            &u16::try_from(self.qe_auth_data.len())
                .expect("qe_auth_data fits u16")
                .to_le_bytes(),
        );
        cert.extend_from_slice(&self.qe_auth_data);
        cert.extend_from_slice(&CERT_DATA_PCK_CHAIN_PEM.to_le_bytes());
        cert.extend_from_slice(
            &u32::try_from(self.pck_chain_pem.len())
                .expect("PCK chain fits u32")
                .to_le_bytes(),
        );
        cert.extend_from_slice(&self.pck_chain_pem);

        let mut sig = Vec::new();
        sig.extend_from_slice(&self.quote_signature);
        sig.extend_from_slice(&self.attestation_key);
        sig.extend_from_slice(&CERT_DATA_QE_REPORT.to_le_bytes());
        sig.extend_from_slice(
            &u32::try_from(cert.len())
                .expect("cert data fits u32")
                .to_le_bytes(),
        );
        sig.extend_from_slice(&cert);

        let mut out = self.signed_bytes();
        out.extend_from_slice(
            &u32::try_from(sig.len())
                .expect("signature data fits u32")
                .to_le_bytes(),
        );
        out.extend_from_slice(&sig);
        out
    }

    /// The value the QE must place in `qe_report.report_data[0..32]`.
    pub fn expected_qe_report_data(attestation_key: &[u8; 64], qe_auth_data: &[u8]) -> [u8; 64] {
        let mut ctx = digest::Context::new(&digest::SHA256);
        ctx.update(attestation_key);
        ctx.update(qe_auth_data);
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(ctx.finish().as_ref());
        out
    }
}

/// Check the signatures inside a quote, given the PCK key from a verified
/// chain: PCK signature over the QE report, QE report data binding the
/// attestation key and QE auth data, attestation-key signature over header
/// and body.
pub(crate) fn verify_quote_signatures(
    quote: &TdxQuote,
    pck: &EcPublicKey,
) -> Result<(), AttestError> {
    pck.verify_fixed(
        &quote.qe_report.encode(),
        &quote.qe_report_signature,
        "TDX QE report (PCK)",
    )?;
    let expected = TdxQuote::expected_qe_report_data(&quote.attestation_key, &quote.qe_auth_data);
    if !ct_eq(&quote.qe_report.report_data, &expected) {
        return Err(AttestError::BindingMismatch {
            what: "QE report data does not bind the attestation key",
        });
    }
    let ak = EcPublicKey::from_raw_xy(Curve::P256, &quote.attestation_key)?;
    ak.verify_fixed(
        &quote.signed_bytes(),
        &quote.quote_signature,
        "TDX quote (attestation key)",
    )
}

/// Check the Vesper binding of a signature-verified quote and normalize it:
/// report data equals the binding of `runtime`, RTMR[2]/RTMR[3] equal one
/// extend with the claimed Loom.lock and `.olean` digests.
pub fn tdx_vesper_claims(
    quote: &TdxQuote,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    if !ct_eq(&quote.body.report_data, &runtime.report_data()) {
        return Err(AttestError::BindingMismatch {
            what: "TD report data does not match runtime claims",
        });
    }
    if !ct_eq(
        &quote.body.rtmr[2],
        &single_extend(RegisterHash::Sha384, 48, &runtime.loom_lock_blake3),
    ) {
        return Err(AttestError::RegisterReplayMismatch {
            register: "RTMR[2]",
        });
    }
    if !ct_eq(
        &quote.body.rtmr[3],
        &single_extend(RegisterHash::Sha384, 48, &runtime.olean_spec_mif),
    ) {
        return Err(AttestError::RegisterReplayMismatch {
            register: "RTMR[3]",
        });
    }
    let mut flags = 0;
    if quote.body.td_attributes & TD_ATTRIBUTE_DEBUG == 0 {
        flags |= ENCLAVE_FLAG_DEBUG_DISABLED;
    }
    let tcb = u16::from(quote.body.tee_tcb_svn[1]) << 8 | u16::from(quote.body.tee_tcb_svn[0]);
    Ok(AttestationClaims::from_runtime(
        TeeArch::IntelTdx15,
        tcb,
        flags,
        quote.body.mr_td,
        runtime,
        SupplyChainEvidence::RegisterReplay,
    ))
}

/// Verify a TDX quote end to end and normalize it.
///
/// Checks: PCK chain to `root` at `now_ns`; PCK signature over the QE
/// report; QE report data binds the attestation key and QE auth data;
/// attestation-key signature over header and body; report data equals the
/// Vesper binding of `runtime`; RTMR[2]/RTMR[3] equal one extend with the
/// claimed Loom.lock and `.olean` digests. Not checked here: Intel TCB-info
/// and QE-identity collateral and CRLs; use
/// [`verify_tdx_quote_with_collateral`] for those.
pub fn verify_tdx_quote(
    bytes: &[u8],
    root: &TrustRoot,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
) -> Result<AttestationClaims, AttestError> {
    let quote = TdxQuote::parse(bytes)?;
    let certs = pem_certificates(&quote.pck_chain_pem)?;
    let (leaf, rest) = certs
        .split_first()
        .ok_or(ParseError::Pem("no certificates"))?;
    let pck = verify_chain(leaf, rest, root, now_ns, Curve::P256)?;
    verify_quote_signatures(&quote, &pck)?;
    tdx_vesper_claims(&quote, runtime)
}

/// [`crate::intel::verify_tdx_collateral`] (chains, CRLs, TCB info, QE
/// identity, TCB status) followed by the Vesper binding of
/// [`tdx_vesper_claims`]. The TCB status is returned, not judged: pass it
/// to [`crate::intel::TcbAcceptance::check`].
pub fn verify_tdx_quote_with_collateral(
    bytes: &[u8],
    collateral: &TdxCollateral,
    root: &TrustRoot,
    now_ns: u64,
    runtime: &GuestRuntimeClaims,
) -> Result<(AttestationClaims, VerifiedTdxQuote), AttestError> {
    let verified = verify_tdx_collateral(bytes, collateral, root, now_ns)?;
    let claims = tdx_vesper_claims(&verified.quote, runtime)?;
    Ok((claims, verified))
}
