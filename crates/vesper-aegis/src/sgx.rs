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
use crate::error::{AttestError, ParseError};
use crate::intel::{
    PckExtensions, QeIdentity, TCB_INFO_VERSION, TCB_TYPE, TcbStatus, TdxCollateral,
    TdxTcbEvaluation, all_ge, check_embedded_root, check_window, collateral, date, evaluate_qe,
    parse_pck_extensions, push_unique, verify_signed,
};
use crate::tdx::{QeReportBody, TdxQuote};

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
        Ok(Self::parse_port(bytes, crate::port::TAIL_EXACT)?.0)
    }

    /// Parse from a fixed-size quote buffer: bytes after the quote's
    /// declared length must all be zero.
    pub fn parse_padded(bytes: &[u8]) -> Result<Self, ParseError> {
        Ok(Self::parse_port(bytes, crate::port::TAIL_PADDED)?.0)
    }

    /// Parse one quote from the start of `bytes`; returns it and the number
    /// of bytes it occupies.
    pub fn parse_prefix(bytes: &[u8]) -> Result<(Self, usize), ParseError> {
        Self::parse_port(bytes, crate::port::TAIL_PREFIX)
    }

    /// `mode`: 0 exact, 1 zero padded, 2 prefix (vesper/src/reader.ves).
    fn parse_port(bytes: &[u8], mode: u64) -> Result<(Self, usize), ParseError> {
        let (used, p) =
            crate::port::parse(bytes, |w, _| crate::port::vesper::sgx_parse(bytes, mode, w))?;
        let mut f = p.fields();
        let header = SgxQuoteHeader {
            version: f.u16(),
            att_key_type: f.u16(),
            qe_svn: f.u16(),
            pce_svn: f.u16(),
            qe_vendor_id: f.array(),
            user_data: f.array(),
        };
        let body = SgxReportBody {
            cpu_svn: f.array(),
            misc_select: f.u32(),
            isv_ext_prod_id: f.array(),
            attributes: f.array(),
            mr_enclave: f.array(),
            mr_signer: f.array(),
            config_id: f.array(),
            isv_prod_id: f.u16(),
            isv_svn: f.u16(),
            config_svn: f.u16(),
            isv_family_id: f.array(),
            report_data: f.array(),
        };
        let signed_bytes = f.bytes(SGX_SIGNED_LEN).to_vec();
        let quote_signature = f.array();
        let attestation_key = f.array();
        let qe_report_raw = f.bytes(SGX_REPORT_BODY_LEN).to_vec();
        let quote = SgxQuote {
            header,
            body,
            quote_signature,
            attestation_key,
            qe_report: QeReportBody::from_port(&mut f),
            qe_report_raw,
            qe_report_signature: f.array(),
            qe_auth_data: f.vec(),
            pck_chain_pem: f.vec(),
            signed_bytes,
        };
        Ok((quote, used as usize))
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

const SGX_OUT_ID_START: usize = 0;
const SGX_OUT_ID_END: usize = 1;
const SGX_OUT_VERSION: usize = 2;
const SGX_OUT_ISSUE_DATE_START: usize = 3;
const SGX_OUT_ISSUE_DATE_END: usize = 4;
const SGX_OUT_NEXT_UPDATE_START: usize = 5;
const SGX_OUT_NEXT_UPDATE_END: usize = 6;
const SGX_OUT_TCB_TYPE: usize = 7;
const SGX_OUT_TCB_EVAL_NUM: usize = 8;
const SGX_OUT_LEVEL_COUNT: usize = 9;
const SGX_OUT_LEN: usize = 10;

const SGX_BYTES_FMSPC_OFF: usize = 0;
const SGX_BYTES_FMSPC_LEN: usize = 6;
const SGX_BYTES_PCEID_OFF: usize = 6;
const SGX_BYTES_PCEID_LEN: usize = 2;
const SGX_BYTES_TOTAL: usize = 8;
const SGX_COMPS_LEN: usize = 16;

const SGX_LVL_STRIDE: usize = 7;
const SGX_LVL_PCESVN: usize = 0;
const SGX_LVL_DATE_START: usize = 1;
const SGX_LVL_DATE_END: usize = 2;
const SGX_LVL_STATUS_START: usize = 3;
const SGX_LVL_STATUS_END: usize = 4;
const SGX_LVL_ADV_START: usize = 5;
const SGX_LVL_ADV_COUNT: usize = 6;

const SGX_ERR_OK: u64 = 0;
const SGX_ERR_HEX: u64 = 2;
const SGX_ERR_CAPACITY: u64 = 3;

const SGX_STACK_MAX_LEVELS: usize = 32;
const SGX_STACK_MAX_ADVS: usize = 128;

impl SgxTcbInfo {
    /// Parse an SGX TCB info JSON body (the signed bytes). Only version 3
    /// with `id = "SGX"` and `tcbType = 0` is accepted; a TDX TCB info
    /// (`id = "TDX"`, `tdxModule` fields) is rejected.
    pub fn parse(json: &[u8]) -> Result<Self, AttestError> {
        let mut out = [0u64; SGX_OUT_LEN];
        let mut bytes = [0u8; SGX_BYTES_TOTAL];
        let mut stack_levels = [0u64; SGX_STACK_MAX_LEVELS * SGX_LVL_STRIDE];
        let mut stack_comps = [0u8; SGX_STACK_MAX_LEVELS * SGX_COMPS_LEN];
        let mut stack_advs = [0u64; SGX_STACK_MAX_ADVS * 2];
        let mut heap_levels = Vec::new();
        let mut heap_comps = Vec::new();
        let mut heap_advs = Vec::new();

        let rc = crate::port::vesper::parse_sgx_tcb_info(
            json,
            &mut out,
            &mut bytes,
            &mut stack_levels,
            &mut stack_comps,
            &mut stack_advs,
        );
        let (levels_buf, comps_buf, advs_buf): (&[u64], &[u8], &[u64]) = if rc == SGX_ERR_CAPACITY {
            let mut lvl_cap = SGX_STACK_MAX_LEVELS * 2;
            let mut adv_cap = SGX_STACK_MAX_ADVS * 2;
            loop {
                heap_levels.resize(lvl_cap * SGX_LVL_STRIDE, 0);
                heap_comps.resize(lvl_cap * SGX_COMPS_LEN, 0);
                heap_advs.resize(adv_cap * 2, 0);
                let retry_rc = crate::port::vesper::parse_sgx_tcb_info(
                    json,
                    &mut out,
                    &mut bytes,
                    &mut heap_levels,
                    &mut heap_comps,
                    &mut heap_advs,
                );
                if retry_rc == SGX_ERR_OK {
                    break (&heap_levels, &heap_comps, &heap_advs);
                }
                if retry_rc != SGX_ERR_CAPACITY {
                    return Err(collateral(
                        "tcbInfo",
                        if retry_rc == SGX_ERR_HEX {
                            "invalid hex field".into()
                        } else {
                            "malformed JSON".into()
                        },
                    ));
                }
                lvl_cap *= 2;
                adv_cap *= 2;
            }
        } else if rc == SGX_ERR_OK {
            (&stack_levels, &stack_comps, &stack_advs)
        } else {
            return Err(collateral(
                "tcbInfo",
                if rc == SGX_ERR_HEX {
                    "invalid hex field".into()
                } else {
                    "malformed JSON".into()
                },
            ));
        };

        let span_str = |s: u64, e: u64, what: &'static str| -> Result<&str, AttestError> {
            std::str::from_utf8(&json[s as usize..e as usize])
                .map_err(|err| collateral(what, err.to_string()))
        };

        let id = span_str(out[SGX_OUT_ID_START], out[SGX_OUT_ID_END], "tcbInfo.id")?;
        if id != "SGX" {
            return Err(collateral("tcbInfo.id", format!("{id:?}, expected SGX")));
        }
        let version = out[SGX_OUT_VERSION] as u32;
        if version != TCB_INFO_VERSION {
            return Err(collateral(
                "tcbInfo.version",
                format!("{version}, expected {TCB_INFO_VERSION}"),
            ));
        }
        let tcb_type = out[SGX_OUT_TCB_TYPE] as u32;
        if tcb_type != TCB_TYPE {
            return Err(collateral("tcbInfo.tcbType", format!("{tcb_type}")));
        }
        let lvl_count = out[SGX_OUT_LEVEL_COUNT] as usize;
        if lvl_count == 0 {
            return Err(collateral("tcbInfo.tcbLevels", "empty".into()));
        }

        let mut levels = Vec::with_capacity(lvl_count);
        for k in 0..lvl_count {
            let base = k * SGX_LVL_STRIDE;
            let cbase = k * SGX_COMPS_LEN;
            let mut sgx_components = [0u8; SGX_COMPS_LEN];
            sgx_components.copy_from_slice(&comps_buf[cbase..cbase + SGX_COMPS_LEN]);
            let tcb_date = date(
                span_str(
                    levels_buf[base + SGX_LVL_DATE_START],
                    levels_buf[base + SGX_LVL_DATE_END],
                    "tcbLevels.tcbDate",
                )?,
                "tcbLevels.tcbDate",
            )?;
            let status = TcbStatus::parse(span_str(
                levels_buf[base + SGX_LVL_STATUS_START],
                levels_buf[base + SGX_LVL_STATUS_END],
                "tcbStatus",
            )?)?;
            let adv_start = levels_buf[base + SGX_LVL_ADV_START] as usize;
            let adv_count = levels_buf[base + SGX_LVL_ADV_COUNT] as usize;
            let mut advisory_ids = Vec::with_capacity(adv_count);
            for a in 0..adv_count {
                let aoff = (adv_start + a) * 2;
                advisory_ids
                    .push(span_str(advs_buf[aoff], advs_buf[aoff + 1], "tcbLevels")?.to_owned());
            }
            levels.push(SgxTcbLevel {
                sgx_components,
                pce_svn: levels_buf[base + SGX_LVL_PCESVN] as u16,
                tcb_date,
                status,
                advisory_ids,
            });
        }

        let mut fmspc = [0u8; SGX_BYTES_FMSPC_LEN];
        fmspc.copy_from_slice(
            &bytes[SGX_BYTES_FMSPC_OFF..SGX_BYTES_FMSPC_OFF + SGX_BYTES_FMSPC_LEN],
        );
        let mut pce_id = [0u8; SGX_BYTES_PCEID_LEN];
        pce_id.copy_from_slice(
            &bytes[SGX_BYTES_PCEID_OFF..SGX_BYTES_PCEID_OFF + SGX_BYTES_PCEID_LEN],
        );

        Ok(SgxTcbInfo {
            fmspc,
            pce_id,
            issue_date: date(
                span_str(
                    out[SGX_OUT_ISSUE_DATE_START],
                    out[SGX_OUT_ISSUE_DATE_END],
                    "tcbInfo.issueDate",
                )?,
                "tcbInfo.issueDate",
            )?,
            next_update: date(
                span_str(
                    out[SGX_OUT_NEXT_UPDATE_START],
                    out[SGX_OUT_NEXT_UPDATE_END],
                    "tcbInfo.nextUpdate",
                )?,
                "tcbInfo.nextUpdate",
            )?,
            tcb_evaluation_data_number: out[SGX_OUT_TCB_EVAL_NUM] as u32,
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
