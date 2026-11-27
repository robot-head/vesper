//! Intel DCAP collateral for TDX quotes: PCK certificate SGX extensions,
//! signed TCB info (v3, `id = "TDX"`) and QE identity (v2, `id = "TD_QE"`),
//! PCK and root CA CRLs, and TCB status evaluation.
//!
//! The evaluation follows Intel's published algorithm for TDX TCB info
//! (PCS API v4, "TDX TCB Info" and "Enclave Identity" sections):
//!
//! 1. The first TCB level (levels are sorted, newest first) whose 16 SGX
//!    component SVNs are all `<=` the PCK certificate's, whose PCESVN is
//!    `<=` the PCK's, and whose TDX component SVNs are all `<=` the TD
//!    report's `TEE_TCB_SVN` gives the platform status. TDX components are
//!    compared from index 2 when the TCB info has `tdxModuleIdentities` and
//!    `TEE_TCB_SVN[1] > 0` (the module's own levels cover indexes 0 and 1),
//!    otherwise from index 0, and then the selected level's component 1
//!    must equal `TEE_TCB_SVN[1]`.
//! 2. The QE report must match the QE identity (MRSIGNER, ISVPRODID,
//!    masked MISCSELECT and ATTRIBUTES); the first QE level with
//!    `isvsvn <=` the QE's ISVSVN gives the QE status.
//! 3. The TDX module must match `tdxModule` (or, for `TEE_TCB_SVN[1] > 0`,
//!    the `TDX_<major>` entry of `tdxModuleIdentities`, whose levels give a
//!    module status).
//! 4. Statuses converge as in Intel's QVL: a `Revoked` QE or module makes
//!    the result `Revoked`; an `OutOfDate` QE or module turns `UpToDate`/
//!    `SWHardeningNeeded` into `OutOfDate` and `ConfigurationNeeded`/
//!    `ConfigurationAndSWHardeningNeeded` into `OutOfDateConfigurationNeeded`.
//!
//! Every input here is untrusted until its signature and chain are checked;
//! malformed, duplicated or unsupported fields are typed errors.

use crate::chain::{
    Curve, RevocationScope, TrustRoot, pem_certificates, verify_by_certificate,
    verify_chain_with_crls,
};
use crate::error::AttestError;
use crate::tdx::{QeReportBody, TdxQuote};
use crate::time::{iso8601_utc_to_unix, unix_seconds};
use serde::Deserialize;

/// Intel SGX extension in PCK certificates.
pub const SGX_EXTENSION_OID: &str = "1.2.840.113741.1.13.1";
/// SGX and TDX TCBs have sixteen one-byte SVN components.
pub(crate) const TCB_COMPONENTS: usize = 16;
/// Intel PCS collateral formats this crate reads: TCB info version 3 with
/// `tcbType` 0, and QE identity version 2.
pub(crate) const TCB_INFO_VERSION: u32 = 3;
pub(crate) const TCB_TYPE: u32 = 0;
pub(crate) const QE_IDENTITY_VERSION: u32 = 2;

/// A TCB status as Intel publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TcbStatus {
    UpToDate,
    SWHardeningNeeded,
    ConfigurationNeeded,
    ConfigurationAndSWHardeningNeeded,
    OutOfDate,
    OutOfDateConfigurationNeeded,
    Revoked,
}

impl TcbStatus {
    pub fn parse(s: &str) -> Result<Self, AttestError> {
        Ok(match s {
            "UpToDate" => TcbStatus::UpToDate,
            "SWHardeningNeeded" => TcbStatus::SWHardeningNeeded,
            "ConfigurationNeeded" => TcbStatus::ConfigurationNeeded,
            "ConfigurationAndSWHardeningNeeded" => TcbStatus::ConfigurationAndSWHardeningNeeded,
            "OutOfDate" => TcbStatus::OutOfDate,
            "OutOfDateConfigurationNeeded" => TcbStatus::OutOfDateConfigurationNeeded,
            "Revoked" => TcbStatus::Revoked,
            other => {
                return Err(collateral("tcbStatus", format!("unknown status {other:?}")));
            }
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TcbStatus::UpToDate => "UpToDate",
            TcbStatus::SWHardeningNeeded => "SWHardeningNeeded",
            TcbStatus::ConfigurationNeeded => "ConfigurationNeeded",
            TcbStatus::ConfigurationAndSWHardeningNeeded => "ConfigurationAndSWHardeningNeeded",
            TcbStatus::OutOfDate => "OutOfDate",
            TcbStatus::OutOfDateConfigurationNeeded => "OutOfDateConfigurationNeeded",
            TcbStatus::Revoked => "Revoked",
        }
    }

    /// Combine a platform status with an `OutOfDate`/`Revoked` QE or TDX
    /// module status (Intel QVL convergence).
    pub fn converge(self, other: TcbStatus) -> TcbStatus {
        match (self, other) {
            (TcbStatus::Revoked, _) | (_, TcbStatus::Revoked) => TcbStatus::Revoked,
            (TcbStatus::UpToDate | TcbStatus::SWHardeningNeeded, TcbStatus::OutOfDate) => {
                TcbStatus::OutOfDate
            }
            (
                TcbStatus::ConfigurationNeeded | TcbStatus::ConfigurationAndSWHardeningNeeded,
                TcbStatus::OutOfDate,
            ) => TcbStatus::OutOfDateConfigurationNeeded,
            (platform, _) => platform,
        }
    }
}

/// Which TCB statuses a relying party accepts. `Revoked` is never accepted,
/// whatever the list says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcbAcceptance {
    pub accept: Vec<TcbStatus>,
}

impl Default for TcbAcceptance {
    /// Only `UpToDate`.
    fn default() -> Self {
        TcbAcceptance {
            accept: vec![TcbStatus::UpToDate],
        }
    }
}

impl TcbAcceptance {
    pub fn check(&self, status: TcbStatus) -> Result<(), crate::error::AegisVerifyError> {
        if status != TcbStatus::Revoked && self.accept.contains(&status) {
            Ok(())
        } else {
            Err(crate::error::AegisVerifyError::TcbStatusNotAccepted(status))
        }
    }
}

/// A signed JSON collateral body (TCB info or enclave identity): the exact
/// bytes Intel signed, and the ECDSA P-256 `r || s` signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCollateral {
    pub body: Vec<u8>,
    pub signature: [u8; 64],
}

const PCS_FIELD_TCB_INFO: u64 = 0;
const PCS_FIELD_ENCLAVE_IDENTITY: u64 = 1;

const PCS_OUT_BODY_START: usize = 0;
const PCS_OUT_BODY_END: usize = 1;
const PCS_OUT_LEN: usize = 2;

const PCS_ERR_OK: u64 = 0;
const PCS_ERR_MISSING_FIELD: u64 = 2;
const PCS_ERR_BOTH_FIELDS: u64 = 3;
const PCS_ERR_BAD_SIG: u64 = 4;

impl SignedCollateral {
    /// From the signed body and the hex signature (as in Intel's
    /// `sgx_ql_qve_collateral_t` and PCCS caches).
    pub fn new(body: impl Into<Vec<u8>>, signature_hex: &str) -> Result<Self, AttestError> {
        let signature = hex_array::<64>(signature_hex, "collateral signature")?;
        Ok(SignedCollateral {
            body: body.into(),
            signature,
        })
    }

    /// From a PCS/PCCS response body `{"<field>": {...}, "signature": "<hex>"}`
    /// (`field` is `tcbInfo` or `enclaveIdentity`). The signed bytes are the
    /// field's JSON value exactly as it appears in the response.
    pub fn from_pcs_response(response: &[u8], field: &'static str) -> Result<Self, AttestError> {
        let field_kind = match field {
            "tcbInfo" => PCS_FIELD_TCB_INFO,
            "enclaveIdentity" => PCS_FIELD_ENCLAVE_IDENTITY,
            _ => return Err(collateral("PCS response", format!("missing {field}"))),
        };
        let mut out = [0u64; PCS_OUT_LEN];
        let mut signature = [0u8; 64];
        match crate::port::vesper::parse_pcs_envelope(
            response,
            field_kind,
            &mut out,
            &mut signature,
        ) {
            PCS_ERR_OK => Ok(SignedCollateral {
                body: response[out[PCS_OUT_BODY_START] as usize..out[PCS_OUT_BODY_END] as usize]
                    .to_vec(),
                signature,
            }),
            PCS_ERR_MISSING_FIELD => Err(collateral("PCS response", format!("missing {field}"))),
            PCS_ERR_BOTH_FIELDS => Err(collateral(
                "PCS response",
                "both tcbInfo and enclaveIdentity".into(),
            )),
            PCS_ERR_BAD_SIG => Err(collateral(
                "collateral signature",
                "invalid hex signature".into(),
            )),
            _ => Err(collateral("PCS response", "malformed JSON envelope".into())),
        }
    }
}

/// Everything needed to evaluate a TDX quote offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxCollateral {
    pub tcb_info: SignedCollateral,
    /// PEM chain of the TCB-signing certificate (leaf first).
    pub tcb_info_issuer_chain_pem: Vec<u8>,
    pub qe_identity: SignedCollateral,
    pub qe_identity_issuer_chain_pem: Vec<u8>,
    /// DER CRL issued by the PCK's CA (Platform or Processor CA).
    pub pck_crl_der: Vec<u8>,
    /// DER CRL issued by the Intel SGX Root CA.
    pub root_ca_crl_der: Vec<u8>,
}

/// Values from the PCK certificate's SGX extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PckExtensions {
    pub ppid: [u8; 16],
    /// SGX TCB component SVNs 1..=16.
    pub sgx_components: [u8; 16],
    pub pce_svn: u16,
    pub cpu_svn: [u8; 16],
    pub pce_id: [u8; 2],
    pub fmspc: [u8; 6],
    /// 0 = Standard, 1 = Scalable, 2 = Scalable with integrity.
    pub sgx_type: u32,
    /// Which Intel CA issued the PCK (from the issuer common name).
    pub issuer: PckIssuer,
    /// Platform-instance data: present exactly in certificates issued by
    /// the PCK Platform CA (multi-package platforms registered with
    /// Intel's registration service).
    pub platform_instance: Option<PckPlatformInstance>,
}

/// The Intel CA that issued a PCK certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PckIssuer {
    /// `Intel SGX PCK Processor CA`: single-package platforms.
    Processor,
    /// `Intel SGX PCK Platform CA`: multi-package (platform-instance)
    /// certificates.
    Platform,
}

/// `PlatformInstanceID` (SGX extension entry 6) and `Configuration`
/// (entry 7) of a Platform CA PCK certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PckPlatformInstance {
    pub platform_instance_id: [u8; 16],
    /// `None` when the certificate has no `Configuration` entry.
    pub configuration: Option<PckPlatformConfiguration>,
}

/// `Configuration` sub-entries (each optional in Intel's specification).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PckPlatformConfiguration {
    /// Platform can be extended with additional packages (via package add).
    pub dynamic_platform: Option<bool>,
    /// Platform root keys are cached by the registration service.
    pub cached_keys: Option<bool>,
    /// SMT (hyper-threading) is enabled on the platform.
    pub smt_enabled: Option<bool>,
}

/// One platform TCB level of a TDX TCB info.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxTcbLevel {
    pub sgx_components: [u8; 16],
    pub pce_svn: u16,
    pub tdx_components: [u8; 16],
    pub tcb_date: u64,
    pub status: TcbStatus,
    pub advisory_ids: Vec<String>,
}

/// A level keyed by ISVSVN (QE identity and TDX module identities).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsvTcbLevel {
    pub isv_svn: u16,
    pub tcb_date: u64,
    pub status: TcbStatus,
    pub advisory_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxModuleIdentity {
    /// `TDX_<major, two hex digits>`, or empty for the legacy `tdxModule`.
    pub id: String,
    pub mr_signer: [u8; 48],
    pub attributes: [u8; 8],
    pub attributes_mask: [u8; 8],
    pub levels: Vec<IsvTcbLevel>,
}

/// A parsed TDX TCB info (version 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxTcbInfo {
    pub fmspc: [u8; 6],
    pub pce_id: [u8; 2],
    /// Unix seconds.
    pub issue_date: u64,
    pub next_update: u64,
    pub tcb_evaluation_data_number: u32,
    pub tdx_module: Option<TdxModuleIdentity>,
    pub tdx_module_identities: Vec<TdxModuleIdentity>,
    pub levels: Vec<TdxTcbLevel>,
}

/// A parsed TD QE identity (version 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QeIdentity {
    pub issue_date: u64,
    pub next_update: u64,
    pub tcb_evaluation_data_number: u32,
    /// MISCSELECT value and mask (hex in the JSON, read as a big-endian
    /// `u32` value).
    pub misc_select: u32,
    pub misc_select_mask: u32,
    /// ATTRIBUTES bytes and mask, in report byte order.
    pub attributes: [u8; 16],
    pub attributes_mask: [u8; 16],
    pub mr_signer: [u8; 32],
    pub isv_prod_id: u16,
    pub levels: Vec<IsvTcbLevel>,
}

/// The outcome of TCB evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxTcbEvaluation {
    /// Converged status.
    pub status: TcbStatus,
    pub platform_status: TcbStatus,
    pub qe_status: TcbStatus,
    pub module_status: Option<TcbStatus>,
    /// Advisories of the matched platform, QE and module levels (deduplicated,
    /// in that order).
    pub advisory_ids: Vec<String>,
    /// `tcbDate` of the matched platform level (Unix seconds).
    pub tcb_date: u64,
    pub tcb_evaluation_data_number: u32,
    pub fmspc: [u8; 6],
}

/// A TDX quote whose signatures, PCK chain, revocation and collateral all
/// verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTdxQuote {
    pub quote: TdxQuote,
    pub pck: PckExtensions,
    pub tcb_info: TdxTcbInfo,
    pub qe_identity: QeIdentity,
    /// Evaluation of `TEE_TCB_SVN` (launch TCB).
    pub evaluation: TdxTcbEvaluation,
    /// TD 1.5 bodies: evaluation of `TEE_TCB_SVN_2` (current TCB), which
    /// must also find a level. `None` for TD 1.0 bodies.
    pub current_evaluation: Option<TdxTcbEvaluation>,
}

// ---------------------------------------------------------------- JSON --

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcbInfoJson {
    id: String,
    version: u32,
    issue_date: String,
    next_update: String,
    fmspc: String,
    pce_id: String,
    tcb_type: u32,
    tcb_evaluation_data_number: u32,
    tdx_module: Option<ModuleJson>,
    #[serde(default)]
    tdx_module_identities: Vec<ModuleIdentityJson>,
    tcb_levels: Vec<TcbLevelJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleJson {
    mrsigner: String,
    attributes: String,
    attributes_mask: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleIdentityJson {
    id: String,
    mrsigner: String,
    attributes: String,
    attributes_mask: String,
    tcb_levels: Vec<IsvLevelJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcbLevelJson {
    tcb: TcbJson,
    tcb_date: String,
    tcb_status: String,
    #[serde(rename = "advisoryIDs", default)]
    advisory_ids: Vec<String>,
}

#[derive(Deserialize)]
struct TcbJson {
    sgxtcbcomponents: Vec<ComponentJson>,
    pcesvn: u16,
    tdxtcbcomponents: Vec<ComponentJson>,
}

#[derive(Deserialize)]
pub(crate) struct ComponentJson {
    svn: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IsvLevelJson {
    tcb: IsvSvnJson,
    tcb_date: String,
    tcb_status: String,
    #[serde(rename = "advisoryIDs", default)]
    advisory_ids: Vec<String>,
}

#[derive(Deserialize)]
struct IsvSvnJson {
    isvsvn: u16,
}

const QE_OUT_ID_START: usize = 0;
const QE_OUT_ID_END: usize = 1;
const QE_OUT_VERSION: usize = 2;
const QE_OUT_ISSUE_DATE_START: usize = 3;
const QE_OUT_ISSUE_DATE_END: usize = 4;
const QE_OUT_NEXT_UPDATE_START: usize = 5;
const QE_OUT_NEXT_UPDATE_END: usize = 6;
const QE_OUT_TCB_EVAL_NUM: usize = 7;
const QE_OUT_ISV_PROD_ID: usize = 8;
const QE_OUT_LEVEL_COUNT: usize = 9;
const QE_OUT_LEN: usize = 10;

const QE_BYTES_MISC_OFF: usize = 0;
const QE_BYTES_MISC_MASK_OFF: usize = 4;
const QE_BYTES_ATTR_OFF: usize = 8;
const QE_BYTES_ATTR_MASK_OFF: usize = 24;
const QE_BYTES_MRSIGNER_OFF: usize = 40;
const QE_BYTES_TOTAL: usize = 72;

const QE_LVL_STRIDE: usize = 7;
const QE_LVL_ISVSVN: usize = 0;
const QE_LVL_DATE_START: usize = 1;
const QE_LVL_DATE_END: usize = 2;
const QE_LVL_STATUS_START: usize = 3;
const QE_LVL_STATUS_END: usize = 4;
const QE_LVL_ADV_START: usize = 5;
const QE_LVL_ADV_COUNT: usize = 6;

const QE_ERR_OK: u64 = 0;
const QE_ERR_HEX: u64 = 2;
const QE_ERR_CAPACITY: u64 = 3;

const QE_STACK_MAX_LEVELS: usize = 32;
const QE_STACK_MAX_ADVS: usize = 64;

pub(crate) fn collateral(what: &'static str, reason: String) -> AttestError {
    AttestError::Collateral { what, reason }
}

pub(crate) fn hex_array<const N: usize>(
    s: &str,
    what: &'static str,
) -> Result<[u8; N], AttestError> {
    let b = s.as_bytes();
    if b.len() != 2 * N {
        return Err(collateral(
            what,
            format!("expected {} hex digits, got {}", 2 * N, b.len()),
        ));
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = hex_digit(b[2 * i]);
        let lo = hex_digit(b[2 * i + 1]);
        match (hi, lo) {
            (Some(h), Some(l)) => *o = (h << 4) | l,
            _ => return Err(collateral(what, "non-hex digit".into())),
        }
    }
    Ok(out)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn svn16(
    c: &[ComponentJson],
    what: &'static str,
) -> Result<[u8; TCB_COMPONENTS], AttestError> {
    if c.len() != TCB_COMPONENTS {
        return Err(collateral(
            what,
            format!("{} components, expected {TCB_COMPONENTS}", c.len()),
        ));
    }
    let mut out = [0u8; 16];
    for (o, c) in out.iter_mut().zip(c) {
        *o = c.svn;
    }
    Ok(out)
}

pub(crate) fn date(s: &str, what: &'static str) -> Result<u64, AttestError> {
    iso8601_utc_to_unix(s).ok_or_else(|| collateral(what, format!("bad UTC timestamp {s:?}")))
}

fn isv_levels(
    levels: &[IsvLevelJson],
    what: &'static str,
) -> Result<Vec<IsvTcbLevel>, AttestError> {
    if levels.is_empty() {
        return Err(collateral(what, "no TCB levels".into()));
    }
    levels
        .iter()
        .map(|l| {
            let status = TcbStatus::parse(&l.tcb_status)?;
            // Intel's enclave and TDX module identity levels carry only
            // these three statuses; convergence (and the Lean model in
            // `lean/AegisCollateral.lean`) relies on it.
            if !matches!(
                status,
                TcbStatus::UpToDate | TcbStatus::OutOfDate | TcbStatus::Revoked
            ) {
                return Err(collateral(
                    what,
                    format!("status {} is not valid for an ISV level", status.as_str()),
                ));
            }
            Ok(IsvTcbLevel {
                isv_svn: l.tcb.isvsvn,
                tcb_date: date(&l.tcb_date, what)?,
                status,
                advisory_ids: l.advisory_ids.clone(),
            })
        })
        .collect()
}

fn module(
    id: String,
    mrsigner: &str,
    attributes: &str,
    mask: &str,
    levels: Vec<IsvTcbLevel>,
) -> Result<TdxModuleIdentity, AttestError> {
    Ok(TdxModuleIdentity {
        id,
        mr_signer: hex_array(mrsigner, "tdxModule.mrsigner")?,
        attributes: hex_array(attributes, "tdxModule.attributes")?,
        attributes_mask: hex_array(mask, "tdxModule.attributesMask")?,
        levels,
    })
}

impl TdxTcbInfo {
    /// Parse a TCB info JSON body (the signed bytes). Only TDX version 3 is
    /// accepted.
    pub fn parse(json: &[u8]) -> Result<Self, AttestError> {
        let j: TcbInfoJson =
            serde_json::from_slice(json).map_err(|e| collateral("tcbInfo", e.to_string()))?;
        if j.id != "TDX" {
            return Err(collateral(
                "tcbInfo.id",
                format!("{:?}, expected TDX", j.id),
            ));
        }
        if j.version != TCB_INFO_VERSION {
            return Err(collateral(
                "tcbInfo.version",
                format!("{}, expected {TCB_INFO_VERSION}", j.version),
            ));
        }
        if j.tcb_type != TCB_TYPE {
            return Err(collateral("tcbInfo.tcbType", format!("{}", j.tcb_type)));
        }
        if j.tcb_levels.is_empty() {
            return Err(collateral("tcbInfo.tcbLevels", "empty".into()));
        }
        let tdx_module = match &j.tdx_module {
            None => None,
            Some(m) => Some(module(
                String::new(),
                &m.mrsigner,
                &m.attributes,
                &m.attributes_mask,
                Vec::new(),
            )?),
        };
        let mut ids = Vec::with_capacity(j.tdx_module_identities.len());
        for m in &j.tdx_module_identities {
            if ids.iter().any(|x: &TdxModuleIdentity| x.id == m.id) {
                return Err(collateral(
                    "tcbInfo.tdxModuleIdentities",
                    format!("duplicate id {:?}", m.id),
                ));
            }
            ids.push(module(
                m.id.clone(),
                &m.mrsigner,
                &m.attributes,
                &m.attributes_mask,
                isv_levels(&m.tcb_levels, "tdxModuleIdentities.tcbLevels")?,
            )?);
        }
        let mut levels = Vec::with_capacity(j.tcb_levels.len());
        for l in &j.tcb_levels {
            levels.push(TdxTcbLevel {
                sgx_components: svn16(&l.tcb.sgxtcbcomponents, "sgxtcbcomponents")?,
                pce_svn: l.tcb.pcesvn,
                tdx_components: svn16(&l.tcb.tdxtcbcomponents, "tdxtcbcomponents")?,
                tcb_date: date(&l.tcb_date, "tcbLevels.tcbDate")?,
                status: TcbStatus::parse(&l.tcb_status)?,
                advisory_ids: l.advisory_ids.clone(),
            });
        }
        Ok(TdxTcbInfo {
            fmspc: hex_array(&j.fmspc, "tcbInfo.fmspc")?,
            pce_id: hex_array(&j.pce_id, "tcbInfo.pceId")?,
            issue_date: date(&j.issue_date, "tcbInfo.issueDate")?,
            next_update: date(&j.next_update, "tcbInfo.nextUpdate")?,
            tcb_evaluation_data_number: j.tcb_evaluation_data_number,
            tdx_module,
            tdx_module_identities: ids,
            levels,
        })
    }
}

impl QeIdentity {
    /// Parse a QE identity JSON body. Only `TD_QE` version 2 is accepted.
    pub fn parse(json: &[u8]) -> Result<Self, AttestError> {
        Self::parse_with_id(json, "TD_QE")
    }

    /// Parse an enclave identity JSON body (version 2) whose `id` must be
    /// `expected_id` (`TD_QE` for TDX, `QE` for the SGX quoting enclave).
    pub fn parse_with_id(json: &[u8], expected_id: &str) -> Result<Self, AttestError> {
        let mut out = [0u64; QE_OUT_LEN];
        let mut bytes = [0u8; QE_BYTES_TOTAL];
        let mut stack_levels = [0u64; QE_STACK_MAX_LEVELS * QE_LVL_STRIDE];
        let mut stack_advs = [0u64; QE_STACK_MAX_ADVS * 2];
        let mut heap_levels = Vec::new();
        let mut heap_advs = Vec::new();

        let rc = crate::port::vesper::parse_qe_identity(
            json,
            &mut out,
            &mut bytes,
            &mut stack_levels,
            &mut stack_advs,
        );
        let (levels_buf, advs_buf): (&[u64], &[u64]) = if rc == QE_ERR_CAPACITY {
            let mut lvl_cap = QE_STACK_MAX_LEVELS * 2;
            let mut adv_cap = QE_STACK_MAX_ADVS * 2;
            loop {
                heap_levels.resize(lvl_cap * QE_LVL_STRIDE, 0);
                heap_advs.resize(adv_cap * 2, 0);
                let retry_rc = crate::port::vesper::parse_qe_identity(
                    json,
                    &mut out,
                    &mut bytes,
                    &mut heap_levels,
                    &mut heap_advs,
                );
                if retry_rc == QE_ERR_OK {
                    break (&heap_levels, &heap_advs);
                }
                if retry_rc != QE_ERR_CAPACITY {
                    return Err(collateral(
                        "enclaveIdentity",
                        if retry_rc == QE_ERR_HEX {
                            "invalid hex field".into()
                        } else {
                            "malformed JSON".into()
                        },
                    ));
                }
                lvl_cap *= 2;
                adv_cap *= 2;
            }
        } else if rc == QE_ERR_OK {
            (&stack_levels, &stack_advs)
        } else {
            return Err(collateral(
                "enclaveIdentity",
                if rc == QE_ERR_HEX {
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

        let id = span_str(
            out[QE_OUT_ID_START],
            out[QE_OUT_ID_END],
            "enclaveIdentity.id",
        )?;
        if id != expected_id {
            return Err(collateral(
                "enclaveIdentity.id",
                format!("{id:?}, expected {expected_id}"),
            ));
        }
        let version = out[QE_OUT_VERSION] as u32;
        if version != QE_IDENTITY_VERSION {
            return Err(collateral(
                "enclaveIdentity.version",
                format!("{version}, expected {QE_IDENTITY_VERSION}"),
            ));
        }

        let issue_date = date(
            span_str(
                out[QE_OUT_ISSUE_DATE_START],
                out[QE_OUT_ISSUE_DATE_END],
                "enclaveIdentity.issueDate",
            )?,
            "enclaveIdentity.issueDate",
        )?;
        let next_update = date(
            span_str(
                out[QE_OUT_NEXT_UPDATE_START],
                out[QE_OUT_NEXT_UPDATE_END],
                "enclaveIdentity.nextUpdate",
            )?,
            "enclaveIdentity.nextUpdate",
        )?;

        let lvl_count = out[QE_OUT_LEVEL_COUNT] as usize;
        let mut levels = Vec::with_capacity(lvl_count);
        for k in 0..lvl_count {
            let base = k * QE_LVL_STRIDE;
            let tcb_date = date(
                span_str(
                    levels_buf[base + QE_LVL_DATE_START],
                    levels_buf[base + QE_LVL_DATE_END],
                    "enclaveIdentity.tcbLevels",
                )?,
                "enclaveIdentity.tcbLevels",
            )?;
            let status = TcbStatus::parse(span_str(
                levels_buf[base + QE_LVL_STATUS_START],
                levels_buf[base + QE_LVL_STATUS_END],
                "enclaveIdentity.tcbLevels",
            )?)?;
            let adv_start = levels_buf[base + QE_LVL_ADV_START] as usize;
            let adv_count = levels_buf[base + QE_LVL_ADV_COUNT] as usize;
            let mut advisory_ids = Vec::with_capacity(adv_count);
            for a in 0..adv_count {
                let aoff = (adv_start + a) * 2;
                advisory_ids.push(
                    span_str(
                        advs_buf[aoff],
                        advs_buf[aoff + 1],
                        "enclaveIdentity.tcbLevels",
                    )?
                    .to_owned(),
                );
            }
            levels.push(IsvTcbLevel {
                isv_svn: levels_buf[base + QE_LVL_ISVSVN] as u16,
                tcb_date,
                status,
                advisory_ids,
            });
        }

        let mut misc = [0u8; 4];
        misc.copy_from_slice(&bytes[QE_BYTES_MISC_OFF..QE_BYTES_MISC_OFF + 4]);
        let mut misc_mask = [0u8; 4];
        misc_mask.copy_from_slice(&bytes[QE_BYTES_MISC_MASK_OFF..QE_BYTES_MISC_MASK_OFF + 4]);
        let mut attributes = [0u8; 16];
        attributes.copy_from_slice(&bytes[QE_BYTES_ATTR_OFF..QE_BYTES_ATTR_OFF + 16]);
        let mut attributes_mask = [0u8; 16];
        attributes_mask
            .copy_from_slice(&bytes[QE_BYTES_ATTR_MASK_OFF..QE_BYTES_ATTR_MASK_OFF + 16]);
        let mut mr_signer = [0u8; 32];
        mr_signer.copy_from_slice(&bytes[QE_BYTES_MRSIGNER_OFF..QE_BYTES_MRSIGNER_OFF + 32]);

        Ok(QeIdentity {
            issue_date,
            next_update,
            tcb_evaluation_data_number: out[QE_OUT_TCB_EVAL_NUM] as u32,
            misc_select: u32::from_be_bytes(misc),
            misc_select_mask: u32::from_be_bytes(misc_mask),
            attributes,
            attributes_mask,
            mr_signer,
            isv_prod_id: out[QE_OUT_ISV_PROD_ID] as u16,
            levels,
        })
    }
}

// ------------------------------------------------------ PCK extension --

fn ext_err(reason: &str) -> AttestError {
    collateral("PCK SGX extension", reason.into())
}

const PCK_OUT_FLAGS: usize = 0;
const PCK_OUT_PCE_SVN: usize = 1;
const PCK_OUT_SGX_TYPE: usize = 2;
const PCK_OUT_CN_START: usize = 3;
const PCK_OUT_CN_END: usize = 4;
const PCK_OUT_PPID_START: usize = 5;
const PCK_OUT_PPID_END: usize = 6;
const PCK_OUT_PCEID_START: usize = 7;
const PCK_OUT_PCEID_END: usize = 8;
const PCK_OUT_FMSPC_START: usize = 9;
const PCK_OUT_FMSPC_END: usize = 10;
const PCK_OUT_CPUSVN_START: usize = 11;
const PCK_OUT_CPUSVN_END: usize = 12;
const PCK_OUT_INST_START: usize = 13;
const PCK_OUT_INST_END: usize = 14;
const PCK_OUT_LEN: usize = 15;

const PCK_FLAG_PPID: u64 = 1;
const PCK_FLAG_TCB: u64 = 2;
const PCK_FLAG_PCE_ID: u64 = 4;
const PCK_FLAG_FMSPC: u64 = 8;
const PCK_FLAG_SGX_TYPE: u64 = 16;
const PCK_FLAG_INSTANCE_ID: u64 = 32;
const PCK_FLAG_CONFIG: u64 = 64;
const PCK_FLAG_DYN_SET: u64 = 128;
const PCK_FLAG_DYN_VAL: u64 = 256;
const PCK_FLAG_CACHED_SET: u64 = 512;
const PCK_FLAG_CACHED_VAL: u64 = 1024;
const PCK_FLAG_SMT_SET: u64 = 2048;
const PCK_FLAG_SMT_VAL: u64 = 4096;

const PCK_ERR_OK: u64 = 0;
const PCK_ERR_TRAILING: u64 = 2;
const PCK_ERR_MISSING_EXT: u64 = 3;
const PCK_ERR_DUP_EXT: u64 = 4;
const PCK_ERR_BAD_EXT: u64 = 5;
const PCK_ERR_NO_CN: u64 = 6;

fn copy_span<const N: usize>(der: &[u8], start: u64, end: u64) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&der[start as usize..end as usize]);
    out
}

/// Parse the SGX extension of a PCK certificate (DER).
pub fn parse_pck_extensions(cert_der: &[u8]) -> Result<PckExtensions, AttestError> {
    let mut out = [0u64; PCK_OUT_LEN];
    let mut sgx_components = [0u8; TCB_COMPONENTS];
    match crate::port::vesper::parse_pck_ext(cert_der, &mut out, &mut sgx_components) {
        PCK_ERR_OK => {}
        PCK_ERR_TRAILING => return Err(collateral("PCK certificate", "trailing bytes".into())),
        PCK_ERR_MISSING_EXT => return Err(ext_err("missing")),
        PCK_ERR_DUP_EXT => return Err(ext_err("duplicated")),
        PCK_ERR_BAD_EXT => return Err(ext_err("malformed DER")),
        PCK_ERR_NO_CN => {
            return Err(collateral(
                "PCK certificate",
                "issuer has no common name".into(),
            ));
        }
        _ => return Err(collateral("PCK certificate", "malformed DER".into())),
    }
    let issuer_cn = std::str::from_utf8(
        &cert_der[out[PCK_OUT_CN_START] as usize..out[PCK_OUT_CN_END] as usize],
    )
    .map_err(|_| collateral("PCK certificate", "issuer has no common name".into()))?;
    let issuer = match issuer_cn {
        "Intel SGX PCK Processor CA" => PckIssuer::Processor,
        "Intel SGX PCK Platform CA" => PckIssuer::Platform,
        other => {
            return Err(collateral(
                "PCK certificate",
                format!("unknown issuer {other:?}"),
            ));
        }
    };
    let flags = out[PCK_OUT_FLAGS];
    if (flags & PCK_FLAG_TCB) == 0 {
        return Err(ext_err("missing TCB"));
    }
    if (flags & PCK_FLAG_PPID) == 0 {
        return Err(ext_err("missing PPID"));
    }
    if (flags & PCK_FLAG_PCE_ID) == 0 {
        return Err(ext_err("missing PCE-ID"));
    }
    if (flags & PCK_FLAG_FMSPC) == 0 {
        return Err(ext_err("missing FMSPC"));
    }
    if (flags & PCK_FLAG_SGX_TYPE) == 0 {
        return Err(ext_err("missing SGX type"));
    }
    let instance_id = if (flags & PCK_FLAG_INSTANCE_ID) != 0 {
        Some(copy_span::<16>(
            cert_der,
            out[PCK_OUT_INST_START],
            out[PCK_OUT_INST_END],
        ))
    } else {
        None
    };
    let configuration = if (flags & PCK_FLAG_CONFIG) != 0 {
        Some(PckPlatformConfiguration {
            dynamic_platform: ((flags & PCK_FLAG_DYN_SET) != 0)
                .then_some((flags & PCK_FLAG_DYN_VAL) != 0),
            cached_keys: ((flags & PCK_FLAG_CACHED_SET) != 0)
                .then_some((flags & PCK_FLAG_CACHED_VAL) != 0),
            smt_enabled: ((flags & PCK_FLAG_SMT_SET) != 0)
                .then_some((flags & PCK_FLAG_SMT_VAL) != 0),
        })
    } else {
        None
    };
    let platform_instance = match (issuer, instance_id, configuration) {
        (PckIssuer::Platform, Some(platform_instance_id), configuration) => {
            Some(PckPlatformInstance {
                platform_instance_id,
                configuration,
            })
        }
        (PckIssuer::Platform, None, _) => {
            return Err(ext_err(
                "Platform CA certificate without PlatformInstanceID",
            ));
        }
        (PckIssuer::Processor, None, None) => None,
        (PckIssuer::Processor, _, _) => {
            return Err(ext_err(
                "Processor CA certificate with platform-instance entries",
            ));
        }
    };
    Ok(PckExtensions {
        issuer,
        platform_instance,
        ppid: copy_span::<16>(cert_der, out[PCK_OUT_PPID_START], out[PCK_OUT_PPID_END]),
        sgx_components,
        pce_svn: out[PCK_OUT_PCE_SVN] as u16,
        cpu_svn: copy_span::<16>(cert_der, out[PCK_OUT_CPUSVN_START], out[PCK_OUT_CPUSVN_END]),
        pce_id: copy_span::<2>(cert_der, out[PCK_OUT_PCEID_START], out[PCK_OUT_PCEID_END]),
        fmspc: copy_span::<6>(cert_der, out[PCK_OUT_FMSPC_START], out[PCK_OUT_FMSPC_END]),
        sgx_type: out[PCK_OUT_SGX_TYPE] as u32,
    })
}

// ---------------------------------------------------------- evaluation --

pub(crate) fn all_ge(have: &[u8], need: &[u8]) -> bool {
    have.iter().zip(need).all(|(h, n)| h >= n)
}

fn masked_eq(a: &[u8], b: &[u8], mask: &[u8]) -> bool {
    a.iter().zip(b).zip(mask).all(|((a, b), m)| a & m == b & m)
}

fn isv_status(levels: &[IsvTcbLevel], isv_svn: u16) -> Option<&IsvTcbLevel> {
    levels.iter().find(|l| l.isv_svn <= isv_svn)
}

pub(crate) fn push_unique(out: &mut Vec<String>, ids: &[String]) {
    for id in ids {
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
}

/// Check the QE report against the QE identity and return its level.
pub fn evaluate_qe(qe: &QeReportBody, identity: &QeIdentity) -> Result<IsvTcbLevel, AttestError> {
    if qe.mr_signer != identity.mr_signer {
        return Err(AttestError::CollateralMismatch {
            what: "QE MRSIGNER",
        });
    }
    if qe.isv_prod_id != identity.isv_prod_id {
        return Err(AttestError::CollateralMismatch {
            what: "QE ISVPRODID",
        });
    }
    if qe.misc_select & identity.misc_select_mask
        != identity.misc_select & identity.misc_select_mask
    {
        return Err(AttestError::CollateralMismatch {
            what: "QE MISCSELECT",
        });
    }
    if !masked_eq(
        &qe.attributes,
        &identity.attributes,
        &identity.attributes_mask,
    ) {
        return Err(AttestError::CollateralMismatch {
            what: "QE ATTRIBUTES",
        });
    }
    isv_status(&identity.levels, qe.isv_svn)
        .cloned()
        .ok_or(AttestError::TcbLevelNotFound {
            what: "QE identity level for the QE ISVSVN",
        })
}

/// Evaluate the TCB status of `quote` (already signature-checked) against
/// parsed collateral and the PCK certificate's extension, using the TD
/// report's `TEE_TCB_SVN` (the TCB the TD was launched on).
pub fn evaluate_tdx_tcb(
    quote: &TdxQuote,
    pck: &PckExtensions,
    tcb_info: &TdxTcbInfo,
    qe_identity: &QeIdentity,
) -> Result<TdxTcbEvaluation, AttestError> {
    evaluate_tdx_tcb_svn(quote, &quote.body.tee_tcb_svn, pck, tcb_info, qe_identity)
}

/// For a TD 1.5 report (quote v5 body type 3 or 4): the same evaluation
/// with `TEE_TCB_SVN_2`, the TCB currently running after TD-preserving
/// module updates. Intel's QVL requires it to be appraisable too. `None`
/// for TD 1.0 bodies.
pub fn evaluate_tdx_current_tcb(
    quote: &TdxQuote,
    pck: &PckExtensions,
    tcb_info: &TdxTcbInfo,
    qe_identity: &QeIdentity,
) -> Result<Option<TdxTcbEvaluation>, AttestError> {
    quote
        .td15
        .as_ref()
        .map(|t| evaluate_tdx_tcb_svn(quote, &t.tee_tcb_svn2, pck, tcb_info, qe_identity))
        .transpose()
}

fn evaluate_tdx_tcb_svn(
    quote: &TdxQuote,
    tee: &[u8; 16],
    pck: &PckExtensions,
    tcb_info: &TdxTcbInfo,
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
    let major = tee[1];
    let use_identities = major > 0 && !tcb_info.tdx_module_identities.is_empty();

    // TDX module identity.
    let seam_attributes = quote.body.seam_attributes.to_le_bytes();
    let module_status;
    let mut module_advisories: &[String] = &[];
    if use_identities {
        let want = format!("TDX_{major:02X}");
        let m = tcb_info
            .tdx_module_identities
            .iter()
            .find(|m| m.id.eq_ignore_ascii_case(&want))
            .ok_or(AttestError::TcbLevelNotFound {
                what: "TDX module identity for TEE_TCB_SVN[1]",
            })?;
        if m.mr_signer != quote.body.mr_signer_seam {
            return Err(AttestError::CollateralMismatch {
                what: "TDX module MRSIGNER",
            });
        }
        if !masked_eq(&seam_attributes, &m.attributes, &m.attributes_mask) {
            return Err(AttestError::CollateralMismatch {
                what: "TDX module attributes",
            });
        }
        let level =
            isv_status(&m.levels, u16::from(tee[0])).ok_or(AttestError::TcbLevelNotFound {
                what: "TDX module level for TEE_TCB_SVN[0]",
            })?;
        module_status = Some(level.status);
        module_advisories = &level.advisory_ids;
    } else {
        let m = tcb_info
            .tdx_module
            .as_ref()
            .ok_or_else(|| collateral("tcbInfo.tdxModule", "missing".into()))?;
        if m.mr_signer != quote.body.mr_signer_seam {
            return Err(AttestError::CollateralMismatch {
                what: "TDX module MRSIGNER",
            });
        }
        if !masked_eq(&seam_attributes, &m.attributes, &m.attributes_mask) {
            return Err(AttestError::CollateralMismatch {
                what: "TDX module attributes",
            });
        }
        module_status = None;
    }

    // Platform level.
    let start = if use_identities { 2 } else { 0 };
    let level = tcb_info
        .levels
        .iter()
        .find(|l| {
            all_ge(&pck.sgx_components, &l.sgx_components)
                && pck.pce_svn >= l.pce_svn
                && all_ge(&tee[start..], &l.tdx_components[start..])
        })
        .ok_or(AttestError::TcbLevelNotFound {
            what: "TDX platform TCB level",
        })?;
    if !use_identities && level.tdx_components[1] != tee[1] {
        return Err(AttestError::CollateralMismatch {
            what: "TDX module major version (tdxtcbcomponents[1] vs TEE_TCB_SVN[1])",
        });
    }

    let qe = evaluate_qe(&quote.qe_report, qe_identity)?;

    let mut status = level.status.converge(qe.status);
    if let Some(m) = module_status {
        status = status.converge(m);
    }
    let mut advisory_ids = Vec::new();
    push_unique(&mut advisory_ids, &level.advisory_ids);
    push_unique(&mut advisory_ids, &qe.advisory_ids);
    push_unique(&mut advisory_ids, module_advisories);
    Ok(TdxTcbEvaluation {
        status,
        platform_status: level.status,
        qe_status: qe.status,
        module_status,
        advisory_ids,
        tcb_date: level.tcb_date,
        tcb_evaluation_data_number: tcb_info.tcb_evaluation_data_number,
        fmspc: tcb_info.fmspc,
    })
}

/// Anti-rollback floor for Intel collateral: the newest values a verifier
/// has already accepted for this FMSPC. Collateral below the floor is an
/// [`AttestError::CollateralDowngrade`], even if it is signed and inside
/// its own validity window (old, validly signed collateral stays valid
/// until its `nextUpdate`, which is exactly what a rollback replays).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TdxCollateralFloor {
    /// Lowest acceptable TCB info `tcbEvaluationDataNumber`.
    pub min_tcb_info_evaluation_data_number: u32,
    /// Lowest acceptable QE identity `tcbEvaluationDataNumber`. Kept apart
    /// from the TCB info number: the two documents are fetched separately
    /// and may carry different numbers without either moving backwards.
    pub min_qe_identity_evaluation_data_number: u32,
    /// Lowest acceptable TCB info `issueDate` (Unix seconds).
    pub min_tcb_info_issue_date: u64,
    /// Lowest acceptable QE identity `issueDate` (Unix seconds).
    pub min_qe_identity_issue_date: u64,
}

impl TdxCollateralFloor {
    /// The floor after accepting `verified`: the componentwise maximum of
    /// `self` and its collateral, so the floor never moves down.
    pub fn raised_by(&self, verified: &VerifiedTdxQuote) -> TdxCollateralFloor {
        TdxCollateralFloor {
            min_tcb_info_evaluation_data_number: self
                .min_tcb_info_evaluation_data_number
                .max(verified.tcb_info.tcb_evaluation_data_number),
            min_qe_identity_evaluation_data_number: self
                .min_qe_identity_evaluation_data_number
                .max(verified.qe_identity.tcb_evaluation_data_number),
            min_tcb_info_issue_date: self
                .min_tcb_info_issue_date
                .max(verified.tcb_info.issue_date),
            min_qe_identity_issue_date: self
                .min_qe_identity_issue_date
                .max(verified.qe_identity.issue_date),
        }
    }
}

/// Reject collateral of `verified` below `floor` (see
/// [`TdxCollateralFloor`]).
pub fn check_tdx_collateral_floor(
    verified: &VerifiedTdxQuote,
    floor: &TdxCollateralFloor,
) -> Result<(), AttestError> {
    let checks = [
        (
            "TCB info tcbEvaluationDataNumber",
            u64::from(verified.tcb_info.tcb_evaluation_data_number),
            u64::from(floor.min_tcb_info_evaluation_data_number),
        ),
        (
            "QE identity tcbEvaluationDataNumber",
            u64::from(verified.qe_identity.tcb_evaluation_data_number),
            u64::from(floor.min_qe_identity_evaluation_data_number),
        ),
        (
            "TCB info issueDate",
            verified.tcb_info.issue_date,
            floor.min_tcb_info_issue_date,
        ),
        (
            "QE identity issueDate",
            verified.qe_identity.issue_date,
            floor.min_qe_identity_issue_date,
        ),
    ];
    for (what, have, floor) in checks {
        if have < floor {
            return Err(AttestError::CollateralDowngrade { what, have, floor });
        }
    }
    Ok(())
}

pub(crate) fn check_window(
    issue: u64,
    next_update: u64,
    now_ns: u64,
    what: &'static str,
) -> Result<(), AttestError> {
    let now = unix_seconds(now_ns);
    if now < issue || now >= next_update {
        return Err(AttestError::CollateralExpired { what });
    }
    Ok(())
}

/// Intel's chains (PCK certification data, collateral issuer chains) end
/// with a copy of the root. webpki ignores that copy (the pinned anchor is
/// used), so a corrupted copy would otherwise pass unnoticed; require it to
/// be byte-identical to the anchor.
pub(crate) fn check_embedded_root(certs: &[Vec<u8>], root: &TrustRoot) -> Result<(), AttestError> {
    match certs.last() {
        Some(last) if certs.len() >= 2 && last.as_slice() == root.der() => Ok(()),
        _ => Err(AttestError::CertificateChain(
            "chain does not end with the configured root".into(),
        )),
    }
}

/// Verify a signed collateral body: issuer chain to `root` (with the root
/// CA CRL) and the ECDSA P-256 signature over the exact body bytes.
pub(crate) fn verify_signed(
    signed: &SignedCollateral,
    issuer_chain_pem: &[u8],
    root: &TrustRoot,
    root_ca_crl: &[u8],
    now_ns: u64,
    what: &'static str,
) -> Result<(), AttestError> {
    let certs = pem_certificates(issuer_chain_pem)?;
    check_embedded_root(&certs, root)?;
    let (leaf, rest) = certs
        .split_first()
        .ok_or(crate::error::ParseError::Pem("no certificates"))?;
    verify_chain_with_crls(
        leaf,
        rest,
        root,
        now_ns,
        Curve::P256,
        &[root_ca_crl],
        RevocationScope::EveryCertificate,
    )?;
    verify_by_certificate(leaf, Curve::P256, &signed.body, &signed.signature, what)
}

/// Verify a TDX quote (v4, or v5 with a TD 1.0, 1.5 or 1.5-ex body)
/// against Intel collateral, end to end:
///
/// - the PCK chain to `root`, with the PCK CRL and the root CA CRL (every
///   certificate below the root must be covered, CRLs must be current);
/// - the PCK signature over the QE report, the QE report data binding of
///   the attestation key, and the attestation-key signature over the quote;
/// - TCB info and QE identity signatures by the TCB-signing certificate
///   (chained to `root`, checked against the root CA CRL) and their
///   `issueDate ≤ now < nextUpdate` windows;
/// - FMSPC/PCE-ID, QE identity and TDX module identity matches, and the TCB
///   status ([`evaluate_tdx_tcb`]), and for TD 1.5 bodies also the
///   current TCB ([`evaluate_tdx_current_tcb`]), which must find a level.
///
/// The PCK and issuer chains must end with a byte-identical copy of `root`.
/// Zero padding after the quote's declared length is accepted (DCAP quote
/// buffers are fixed size); any other trailing byte is an error. Report
/// data and RTMRs are not interpreted here (see
/// [`crate::tdx::verify_tdx_quote_with_collateral`] for the Vesper binding).
pub fn verify_tdx_collateral(
    bytes: &[u8],
    collateral: &TdxCollateral,
    root: &TrustRoot,
    now_ns: u64,
) -> Result<VerifiedTdxQuote, AttestError> {
    let quote = TdxQuote::parse_padded(bytes)?;
    let certs = pem_certificates(&quote.pck_chain_pem)?;
    check_embedded_root(&certs, root)?;
    let (leaf, rest) = certs
        .split_first()
        .ok_or(crate::error::ParseError::Pem("no certificates"))?;
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
    crate::tdx::verify_quote_signatures(&quote, &pck_key)?;
    let pck = parse_pck_extensions(leaf)?;

    verify_signed(
        &collateral.tcb_info,
        &collateral.tcb_info_issuer_chain_pem,
        root,
        &collateral.root_ca_crl_der,
        now_ns,
        "TCB info (TCB signing key)",
    )?;
    let tcb_info = TdxTcbInfo::parse(&collateral.tcb_info.body)?;
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
    let qe_identity = QeIdentity::parse(&collateral.qe_identity.body)?;
    check_window(
        qe_identity.issue_date,
        qe_identity.next_update,
        now_ns,
        "QE identity",
    )?;

    let evaluation = evaluate_tdx_tcb(&quote, &pck, &tcb_info, &qe_identity)?;
    let current_evaluation = evaluate_tdx_current_tcb(&quote, &pck, &tcb_info, &qe_identity)?;
    Ok(VerifiedTdxQuote {
        quote,
        pck,
        tcb_info,
        qe_identity,
        evaluation,
        current_evaluation,
    })
}
