//! Intel SGX DCAP quotes, version 3 (`sgx_quote_3.h`): a 48-byte header, the
//! 384-byte SGX enclave report body (body type 1 in quote-v5 terms) and
//! ECDSA-256 signature data whose certification data is the PEM PCK chain
//! (type 5) directly, followed by Intel SGX collateral: TCB info v3 with
//! `id = "SGX"` and the quoting-enclave identity v2 with `id = "QE"`.
//!
//! The TCB evaluation follows Intel's published algorithm for SGX TCB info
//! (PCS API v4, "TCB Info V3" and "Enclave Identity V2"), as in Intel's QVL
//! and Phala's `dcap-qvl`:
//!
//! 1. The first TCB level (levels are sorted, newest first) whose sixteen
//!    SGX component SVNs are all `<=` the PCK certificate's and whose
//!    PCESVN is `<=` the PCK's gives the platform status (a component-wise,
//!    not lexicographic, comparison).
//! 2. The QE report must match the QE identity (MRSIGNER, ISVPRODID,
//!    masked MISCSELECT and ATTRIBUTES); the first QE level with
//!    `isvsvn <=` the QE's ISVSVN gives the QE status.
//! 3. Statuses converge as for TDX ([`TcbStatus::converge`]).
//!
//! The application enclave itself (MRENCLAVE, MRSIGNER, product, SVN,
//! debug bit, report data) is judged by an explicit [`SgxEnclavePolicy`];
//! nothing about it is accepted by default.
//!
//! Every input here is untrusted until its signature and chain are checked;
//! malformed, duplicated or unsupported fields are typed errors, and no
//! input panics.

use crate::chain::{
    Curve, EcPublicKey, RevocationScope, TrustRoot, pem_certificates, verify_chain_with_crls,
};
use crate::claims::ct_eq;
use crate::error::{AttestError, Format, ParseError};
use crate::intel::{
    ComponentJson, PckExtensions, QeIdentity, TcbStatus, TdxCollateral, TdxTcbEvaluation, all_ge,
    check_embedded_root, check_window, collateral, date, evaluate_qe, hex_array,
    parse_pck_extensions, push_unique, svn16, verify_signed,
};
use crate::reader::Reader;
use crate::tdx::{CERT_DATA_PCK_CHAIN_PEM, MAX_SIG_DATA_LEN, QeReportBody, TdxQuote};
use serde::Deserialize;

pub const SGX_QUOTE_VERSION: u16 = 3;
/// `sgx_attestation_algorithm_id_t`: ECDSA-256 with P-256.
pub const SGX_ATT_KEY_ECDSA_P256: u16 = 2;
pub const SGX_QUOTE_HEADER_LEN: usize = 48;
pub const SGX_REPORT_BODY_LEN: usize = 384;
/// Header and report body: the bytes the attestation key signs.
pub const SGX_SIGNED_LEN: usize = SGX_QUOTE_HEADER_LEN + SGX_REPORT_BODY_LEN;
/// Intel's QE vendor ID (`939A7233-F79C-4CA9-940A-0DB3957F0607`).
pub const INTEL_QE_VENDOR_ID: [u8; 16] = [
    0x93, 0x9A, 0x72, 0x33, 0xF7, 0x9C, 0x4C, 0xA9, 0x94, 0x0A, 0x0D, 0xB3, 0x95, 0x7F, 0x06, 0x07,
];
/// `SGX_FLAGS_INITTED` (attributes byte 0, bit 0).
pub const SGX_FLAGS_INITTED: u8 = 0x01;
/// `SGX_FLAGS_DEBUG` (attributes byte 0, bit 1).
pub const SGX_FLAGS_DEBUG: u8 = 0x02;

/// The quote-v3 header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SgxQuoteHeader {
    pub version: u16,
    pub att_key_type: u16,
    pub qe_svn: u16,
    pub pce_svn: u16,
    pub qe_vendor_id: [u8; 16],
    pub user_data: [u8; 20],
}

/// The SGX enclave report body (`sgx_report_body_t`, 384 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SgxReportBody {
    pub cpu_svn: [u8; 16],
    pub misc_select: u32,
    pub isv_ext_prod_id: [u8; 16],
    /// `flags` (bytes 0..8, little-endian) and `xfrm` (bytes 8..16).
    pub attributes: [u8; 16],
    pub mr_enclave: [u8; 32],
    pub mr_signer: [u8; 32],
    pub config_id: [u8; 64],
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub config_svn: u16,
    pub isv_family_id: [u8; 16],
    pub report_data: [u8; 64],
}

impl SgxReportBody {
    fn parse(r: &mut Reader<'_>) -> Result<Self, ParseError> {
        let cpu_svn = r.array("body.cpu_svn")?;
        let misc_select = r.u32("body.misc_select")?;
        r.reserved(12, "body.reserved1")?;
        let isv_ext_prod_id = r.array("body.isv_ext_prod_id")?;
        let attributes = r.array("body.attributes")?;
        let mr_enclave = r.array("body.mr_enclave")?;
        r.reserved(32, "body.reserved2")?;
        let mr_signer = r.array("body.mr_signer")?;
        r.reserved(32, "body.reserved3")?;
        let config_id = r.array("body.config_id")?;
        let isv_prod_id = r.u16("body.isv_prod_id")?;
        let isv_svn = r.u16("body.isv_svn")?;
        let config_svn = r.u16("body.config_svn")?;
        r.reserved(42, "body.reserved4")?;
        let isv_family_id = r.array("body.isv_family_id")?;
        let report_data = r.array("body.report_data")?;
        Ok(SgxReportBody {
            cpu_svn,
            misc_select,
            isv_ext_prod_id,
            attributes,
            mr_enclave,
            mr_signer,
            config_id,
            isv_prod_id,
            isv_svn,
            config_svn,
            isv_family_id,
            report_data,
        })
    }

    /// `SGX_FLAGS_DEBUG` is set.
    pub fn is_debug(&self) -> bool {
        self.attributes[0] & SGX_FLAGS_DEBUG != 0
    }
}

/// A parsed SGX quote v3; signatures not yet checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SgxQuote {
    pub header: SgxQuoteHeader,
    pub body: SgxReportBody,
    /// ECDSA P-256 `r || s` over header and body.
    pub quote_signature: [u8; 64],
    /// Attestation public key `X || Y`.
    pub attestation_key: [u8; 64],
    pub qe_report: QeReportBody,
    /// Raw 384-byte QE report, exactly as the PCK signed it.
    pub qe_report_raw: Vec<u8>,
    /// PCK ECDSA P-256 `r || s` over the QE report.
    pub qe_report_signature: [u8; 64],
    pub qe_auth_data: Vec<u8>,
    /// PEM PCK certificate chain, leaf first.
    pub pck_chain_pem: Vec<u8>,
    /// Header and body exactly as received (the attestation key signs them).
    pub signed_bytes: Vec<u8>,
}

impl SgxQuote {
    /// Bounds-checked parse; the input must contain exactly one quote.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (quote, used) = Self::parse_prefix(bytes)?;
        if used != bytes.len() {
            return Err(ParseError::TrailingBytes {
                format: Format::SgxQuoteV3,
                extra: bytes.len() - used,
            });
        }
        Ok(quote)
    }

    /// Parse from a fixed-size quote buffer: bytes after the quote's
    /// declared length must all be zero.
    pub fn parse_padded(bytes: &[u8]) -> Result<Self, ParseError> {
        let (quote, used) = Self::parse_prefix(bytes)?;
        if bytes[used..].iter().any(|&b| b != 0) {
            return Err(ParseError::TrailingBytes {
                format: Format::SgxQuoteV3,
                extra: bytes.len() - used,
            });
        }
        Ok(quote)
    }

    /// Parse one quote from the start of `bytes`; returns it and the number
    /// of bytes it occupies.
    pub fn parse_prefix(bytes: &[u8]) -> Result<(Self, usize), ParseError> {
        let mut r = Reader::new(bytes, Format::SgxQuoteV3);
        let version = r.u16("header.version")?;
        if version != SGX_QUOTE_VERSION {
            return Err(unsupported("header.version", version.into()));
        }
        let att_key_type = r.u16("header.att_key_type")?;
        if att_key_type != SGX_ATT_KEY_ECDSA_P256 {
            return Err(unsupported("header.att_key_type", att_key_type.into()));
        }
        // `att_key_data_0`: reserved for ECDSA-256 attestation keys (the
        // TEE type of quote v4 lives here; SGX is 0).
        r.reserved(4, "header.att_key_data_0")?;
        let header = SgxQuoteHeader {
            version,
            att_key_type,
            qe_svn: r.u16("header.qe_svn")?,
            pce_svn: r.u16("header.pce_svn")?,
            qe_vendor_id: r.array("header.qe_vendor_id")?,
            user_data: r.array("header.user_data")?,
        };
        let body = SgxReportBody::parse(&mut r)?;
        debug_assert_eq!(r.position(), SGX_SIGNED_LEN);
        let signed_bytes = bytes[..SGX_SIGNED_LEN].to_vec();

        let sig_len = r.u32("signature_data_len")? as usize;
        if sig_len > MAX_SIG_DATA_LEN {
            return Err(ParseError::OverBudget {
                format: Format::SgxQuoteV3,
                field: "signature_data_len",
                limit: MAX_SIG_DATA_LEN,
                value: sig_len as u64,
            });
        }
        let sig = r.take(sig_len, "signature_data")?;
        let used = r.position();

        let mut s = Reader::new(sig, Format::SgxQuoteV3);
        let quote_signature = s.array("quote_signature")?;
        let attestation_key = s.array("attestation_key")?;
        let qe_report_raw = s.take(SGX_REPORT_BODY_LEN, "qe_report")?.to_vec();
        let qe_report = {
            let mut q = Reader::new(&qe_report_raw, Format::SgxQuoteV3);
            QeReportBody::parse_from(&mut q)?
        };
        let qe_report_signature = s.array("qe_report_signature")?;
        let auth_len = s.u16("qe_auth_data.size")? as usize;
        let qe_auth_data = s.take(auth_len, "qe_auth_data")?.to_vec();
        let cert_type = s.u16("cert_data.type")?;
        if cert_type != CERT_DATA_PCK_CHAIN_PEM {
            return Err(unsupported("cert_data.type", cert_type.into()));
        }
        let cert_size = s.u32("cert_data.size")? as usize;
        let pck_chain_pem = s.take(cert_size, "cert_data")?.to_vec();
        s.finish()?;

        Ok((
            SgxQuote {
                header,
                body,
                quote_signature,
                attestation_key,
                qe_report,
                qe_report_raw,
                qe_report_signature,
                qe_auth_data,
                pck_chain_pem,
                signed_bytes,
            },
            used,
        ))
    }
}

/// PCK signature over the QE report, QE report data binding the
/// attestation key and QE auth data, and the attestation-key signature
/// over header and body.
fn verify_sgx_quote_signatures(quote: &SgxQuote, pck: &EcPublicKey) -> Result<(), AttestError> {
    pck.verify_fixed(
        &quote.qe_report_raw,
        &quote.qe_report_signature,
        "SGX QE report (PCK)",
    )?;
    let expected = TdxQuote::expected_qe_report_data(&quote.attestation_key, &quote.qe_auth_data);
    if !ct_eq(&quote.qe_report.report_data, &expected) {
        return Err(AttestError::BindingMismatch {
            what: "QE report data does not bind the attestation key",
        });
    }
    let ak = EcPublicKey::from_raw_xy(Curve::P256, &quote.attestation_key)?;
    ak.verify_fixed(
        &quote.signed_bytes,
        &quote.quote_signature,
        "SGX quote (attestation key)",
    )
}

/// Intel's SGX collateral has the same parts as TDX collateral (TCB info,
/// QE identity, their issuer chains, PCK and root CA CRLs); only the
/// documents' ids differ.
pub type SgxCollateral = TdxCollateral;

/// One platform TCB level of an SGX TCB info.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SgxTcbLevel {
    pub sgx_components: [u8; 16],
    pub pce_svn: u16,
    pub tcb_date: u64,
    pub status: TcbStatus,
    pub advisory_ids: Vec<String>,
}

/// A parsed SGX TCB info (version 3, `id = "SGX"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SgxTcbInfo {
    pub fmspc: [u8; 6],
    pub pce_id: [u8; 2],
    /// Unix seconds.
    pub issue_date: u64,
    pub next_update: u64,
    pub tcb_evaluation_data_number: u32,
    pub levels: Vec<SgxTcbLevel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SgxTcbInfoJson {
    id: String,
    version: u32,
    issue_date: String,
    next_update: String,
    fmspc: String,
    pce_id: String,
    tcb_type: u32,
    tcb_evaluation_data_number: u32,
    tcb_levels: Vec<SgxTcbLevelJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SgxTcbLevelJson {
    tcb: SgxTcbJson,
    tcb_date: String,
    tcb_status: String,
    #[serde(rename = "advisoryIDs", default)]
    advisory_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SgxTcbJson {
    sgxtcbcomponents: Vec<ComponentJson>,
    pcesvn: u16,
}

impl SgxTcbInfo {
    /// Parse an SGX TCB info JSON body (the signed bytes). Only version 3
    /// with `id = "SGX"` and `tcbType = 0` is accepted; a TDX TCB info
    /// (`id = "TDX"`, `tdxModule` fields) is rejected.
    pub fn parse(json: &[u8]) -> Result<Self, AttestError> {
        let j: SgxTcbInfoJson =
            serde_json::from_slice(json).map_err(|e| collateral("tcbInfo", e.to_string()))?;
        if j.id != "SGX" {
            return Err(collateral(
                "tcbInfo.id",
                format!("{:?}, expected SGX", j.id),
            ));
        }
        if j.version != 3 {
            return Err(collateral(
                "tcbInfo.version",
                format!("{}, expected 3", j.version),
            ));
        }
        if j.tcb_type != 0 {
            return Err(collateral("tcbInfo.tcbType", format!("{}", j.tcb_type)));
        }
        if j.tcb_levels.is_empty() {
            return Err(collateral("tcbInfo.tcbLevels", "empty".into()));
        }
        let mut levels = Vec::with_capacity(j.tcb_levels.len());
        for l in &j.tcb_levels {
            levels.push(SgxTcbLevel {
                sgx_components: svn16(&l.tcb.sgxtcbcomponents, "sgxtcbcomponents")?,
                pce_svn: l.tcb.pcesvn,
                tcb_date: date(&l.tcb_date, "tcbLevels.tcbDate")?,
                status: TcbStatus::parse(&l.tcb_status)?,
                advisory_ids: l.advisory_ids.clone(),
            });
        }
        Ok(SgxTcbInfo {
            fmspc: hex_array(&j.fmspc, "tcbInfo.fmspc")?,
            pce_id: hex_array(&j.pce_id, "tcbInfo.pceId")?,
            issue_date: date(&j.issue_date, "tcbInfo.issueDate")?,
            next_update: date(&j.next_update, "tcbInfo.nextUpdate")?,
            tcb_evaluation_data_number: j.tcb_evaluation_data_number,
            levels,
        })
    }
}

/// Evaluate the TCB status of a signature-checked SGX quote against parsed
/// collateral and the PCK certificate's extension.
pub fn evaluate_sgx_tcb(
    quote: &SgxQuote,
    pck: &PckExtensions,
    tcb_info: &SgxTcbInfo,
    qe_identity: &QeIdentity,
) -> Result<TdxTcbEvaluation, AttestError> {
    if pck.fmspc != tcb_info.fmspc {
        return Err(AttestError::CollateralMismatch {
            what: "FMSPC (PCK certificate vs TCB info)",
        });
    }
    if pck.pce_id != tcb_info.pce_id {
        return Err(AttestError::CollateralMismatch {
            what: "PCE-ID (PCK certificate vs TCB info)",
        });
    }
    let level = tcb_info
        .levels
        .iter()
        .find(|l| all_ge(&pck.sgx_components, &l.sgx_components) && pck.pce_svn >= l.pce_svn)
        .ok_or(AttestError::TcbLevelNotFound {
            what: "SGX platform TCB level",
        })?;
    let qe = evaluate_qe(&quote.qe_report, qe_identity)?;
    let mut advisory_ids = Vec::new();
    push_unique(&mut advisory_ids, &level.advisory_ids);
    push_unique(&mut advisory_ids, &qe.advisory_ids);
    Ok(TdxTcbEvaluation {
        status: level.status.converge(qe.status),
        platform_status: level.status,
        qe_status: qe.status,
        module_status: None,
        advisory_ids,
        tcb_date: level.tcb_date,
        tcb_evaluation_data_number: tcb_info.tcb_evaluation_data_number,
        fmspc: tcb_info.fmspc,
    })
}

/// An SGX quote whose signatures, PCK chain, revocation and collateral all
/// verified. The application enclave is not judged here; see
/// [`SgxEnclavePolicy::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSgxQuote {
    pub quote: SgxQuote,
    pub pck: PckExtensions,
    pub tcb_info: SgxTcbInfo,
    pub qe_identity: QeIdentity,
    /// The converged TCB status: pass it to
    /// [`crate::intel::TcbAcceptance::check`].
    pub evaluation: TdxTcbEvaluation,
}

/// Verify the quote's own signatures and its PCK chain to `root` (no CRLs,
/// no TCB collateral): enough to show that an Intel-certified platform's QE
/// signed the report, not that the platform is current. Use
/// [`verify_sgx_collateral`] for revocation and TCB status.
pub fn verify_sgx_quote_signatures_only(
    bytes: &[u8],
    root: &TrustRoot,
    now_ns: u64,
) -> Result<(SgxQuote, PckExtensions), AttestError> {
    let quote = SgxQuote::parse_padded(bytes)?;
    check_vendor(&quote)?;
    let certs = pem_certificates(&quote.pck_chain_pem)?;
    check_embedded_root(&certs, root)?;
    let (leaf, rest) = certs
        .split_first()
        .ok_or(ParseError::Pem("no certificates"))?;
    let pck_key = crate::chain::verify_chain(leaf, rest, root, now_ns, Curve::P256)?;
    verify_sgx_quote_signatures(&quote, &pck_key)?;
    let pck = parse_pck_extensions(leaf)?;
    Ok((quote, pck))
}

fn check_vendor(quote: &SgxQuote) -> Result<(), AttestError> {
    if quote.header.qe_vendor_id != INTEL_QE_VENDOR_ID {
        return Err(AttestError::CollateralMismatch {
            what: "QE vendor ID (not Intel's quoting enclave)",
        });
    }
    Ok(())
}

/// Verify an SGX DCAP quote v3 against Intel collateral, end to end:
///
/// - the QE vendor ID is Intel's;
/// - the PCK chain to `root`, with the PCK CRL and the root CA CRL (every
///   certificate below the root must be covered, CRLs must be current); the
///   chain must end with a byte-identical copy of `root`;
/// - the PCK signature over the QE report, the QE report data binding of
///   the attestation key, and the attestation-key signature over header
///   and enclave report body;
/// - TCB info (`SGX`, v3) and QE identity (`QE`, v2) signatures by the
///   TCB-signing certificate (chained to `root`, checked against the root
///   CA CRL) and their `issueDate ≤ now < nextUpdate` windows;
/// - FMSPC/PCE-ID and QE identity matches, and the TCB status
///   ([`evaluate_sgx_tcb`]).
///
/// Zero padding after the quote is accepted (DCAP quote buffers are fixed
/// size); any other trailing byte is an error.
pub fn verify_sgx_collateral(
    bytes: &[u8],
    collateral: &SgxCollateral,
    root: &TrustRoot,
    now_ns: u64,
) -> Result<VerifiedSgxQuote, AttestError> {
    let quote = SgxQuote::parse_padded(bytes)?;
    check_vendor(&quote)?;
    let certs = pem_certificates(&quote.pck_chain_pem)?;
    check_embedded_root(&certs, root)?;
    let (leaf, rest) = certs
        .split_first()
        .ok_or(ParseError::Pem("no certificates"))?;
    let crls: [&[u8]; 2] = [&collateral.pck_crl_der, &collateral.root_ca_crl_der];
    let pck_key = verify_chain_with_crls(
        leaf,
        rest,
        root,
        now_ns,
        Curve::P256,
        &crls,
        RevocationScope::EveryCertificate,
    )?;
    verify_sgx_quote_signatures(&quote, &pck_key)?;
    let pck = parse_pck_extensions(leaf)?;

    verify_signed(
        &collateral.tcb_info,
        &collateral.tcb_info_issuer_chain_pem,
        root,
        &collateral.root_ca_crl_der,
        now_ns,
        "TCB info (TCB signing key)",
    )?;
    let tcb_info = SgxTcbInfo::parse(&collateral.tcb_info.body)?;
    check_window(
        tcb_info.issue_date,
        tcb_info.next_update,
        now_ns,
        "TCB info",
    )?;

    verify_signed(
        &collateral.qe_identity,
        &collateral.qe_identity_issuer_chain_pem,
        root,
        &collateral.root_ca_crl_der,
        now_ns,
        "QE identity (TCB signing key)",
    )?;
    let qe_identity = QeIdentity::parse_with_id(&collateral.qe_identity.body, "QE")?;
    check_window(
        qe_identity.issue_date,
        qe_identity.next_update,
        now_ns,
        "QE identity",
    )?;

    let evaluation = evaluate_sgx_tcb(&quote, &pck, &tcb_info, &qe_identity)?;
    Ok(VerifiedSgxQuote {
        quote,
        pck,
        tcb_info,
        qe_identity,
        evaluation,
    })
}

/// What a relying party requires of the application enclave. `None`
/// fields are not checked; `min_isv_svn` and `allow_debug` always are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SgxEnclavePolicy {
    pub mr_enclave: Option<[u8; 32]>,
    pub mr_signer: Option<[u8; 32]>,
    pub isv_prod_id: Option<u16>,
    pub min_isv_svn: u16,
    /// Debug enclaves are refused unless this is set.
    pub allow_debug: bool,
    /// Exact report data (for example a hash binding a TLS key).
    pub report_data: Option<[u8; 64]>,
}

impl SgxEnclavePolicy {
    /// Judge the enclave report body of a verified quote.
    pub fn check(&self, body: &SgxReportBody) -> Result<(), AttestError> {
        if body.attributes[0] & SGX_FLAGS_INITTED == 0 {
            return Err(AttestError::BindingMismatch {
                what: "SGX enclave is not initialized (ATTRIBUTES.INIT clear)",
            });
        }
        if body.is_debug() && !self.allow_debug {
            return Err(AttestError::BindingMismatch {
                what: "SGX enclave is a debug enclave",
            });
        }
        if let Some(m) = &self.mr_enclave
            && !ct_eq(m, &body.mr_enclave)
        {
            return Err(AttestError::BindingMismatch {
                what: "SGX MRENCLAVE",
            });
        }
        if let Some(m) = &self.mr_signer
            && !ct_eq(m, &body.mr_signer)
        {
            return Err(AttestError::BindingMismatch {
                what: "SGX MRSIGNER",
            });
        }
        if let Some(p) = self.isv_prod_id
            && p != body.isv_prod_id
        {
            return Err(AttestError::BindingMismatch {
                what: "SGX ISVPRODID",
            });
        }
        if body.isv_svn < self.min_isv_svn {
            return Err(AttestError::BindingMismatch {
                what: "SGX ISVSVN below the policy minimum",
            });
        }
        if let Some(d) = &self.report_data
            && !ct_eq(d, &body.report_data)
        {
            return Err(AttestError::BindingMismatch {
                what: "SGX report data",
            });
        }
        Ok(())
    }
}

fn unsupported(field: &'static str, value: u64) -> ParseError {
    ParseError::UnsupportedValue {
        format: Format::SgxQuoteV3,
        field,
        value,
    }
}
