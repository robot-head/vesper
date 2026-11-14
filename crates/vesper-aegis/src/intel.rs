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
use x509_parser::der_parser::ber::BerObject;
use x509_parser::der_parser::der::parse_der;

/// Intel SGX extension in PCK certificates.
pub const SGX_EXTENSION_OID: &str = "1.2.840.113741.1.13.1";

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
        #[derive(Deserialize)]
        struct Envelope<'a> {
            #[serde(borrow, rename = "tcbInfo")]
            tcb_info: Option<&'a serde_json::value::RawValue>,
            #[serde(borrow, rename = "enclaveIdentity")]
            enclave_identity: Option<&'a serde_json::value::RawValue>,
            signature: String,
        }
        let env: Envelope<'_> = serde_json::from_slice(response)
            .map_err(|e| collateral("PCS response", e.to_string()))?;
        let raw = match field {
            "tcbInfo" => env.tcb_info,
            "enclaveIdentity" => env.enclave_identity,
            _ => None,
        }
        .ok_or_else(|| collateral("PCS response", format!("missing {field}")))?;
        if env.tcb_info.is_some() && env.enclave_identity.is_some() {
            return Err(collateral(
                "PCS response",
                "both tcbInfo and enclaveIdentity".into(),
            ));
        }
        SignedCollateral::new(raw.get().as_bytes(), &env.signature)
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
struct ComponentJson {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QeIdentityJson {
    id: String,
    version: u32,
    issue_date: String,
    next_update: String,
    tcb_evaluation_data_number: u32,
    miscselect: String,
    miscselect_mask: String,
    attributes: String,
    attributes_mask: String,
    mrsigner: String,
    isvprodid: u16,
    tcb_levels: Vec<IsvLevelJson>,
}

fn collateral(what: &'static str, reason: String) -> AttestError {
    AttestError::Collateral { what, reason }
}

fn hex_array<const N: usize>(s: &str, what: &'static str) -> Result<[u8; N], AttestError> {
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

fn svn16(c: &[ComponentJson], what: &'static str) -> Result<[u8; 16], AttestError> {
    if c.len() != 16 {
        return Err(collateral(
            what,
            format!("{} components, expected 16", c.len()),
        ));
    }
    let mut out = [0u8; 16];
    for (o, c) in out.iter_mut().zip(c) {
        *o = c.svn;
    }
    Ok(out)
}

fn date(s: &str, what: &'static str) -> Result<u64, AttestError> {
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
        let j: QeIdentityJson = serde_json::from_slice(json)
            .map_err(|e| collateral("enclaveIdentity", e.to_string()))?;
        if j.id != "TD_QE" {
            return Err(collateral(
                "enclaveIdentity.id",
                format!("{:?}, expected TD_QE", j.id),
            ));
        }
        if j.version != 2 {
            return Err(collateral(
                "enclaveIdentity.version",
                format!("{}, expected 2", j.version),
            ));
        }
        Ok(QeIdentity {
            issue_date: date(&j.issue_date, "enclaveIdentity.issueDate")?,
            next_update: date(&j.next_update, "enclaveIdentity.nextUpdate")?,
            tcb_evaluation_data_number: j.tcb_evaluation_data_number,
            misc_select: u32::from_be_bytes(hex_array(&j.miscselect, "miscselect")?),
            misc_select_mask: u32::from_be_bytes(hex_array(&j.miscselect_mask, "miscselectMask")?),
            attributes: hex_array(&j.attributes, "attributes")?,
            attributes_mask: hex_array(&j.attributes_mask, "attributesMask")?,
            mr_signer: hex_array(&j.mrsigner, "mrsigner")?,
            isv_prod_id: j.isvprodid,
            levels: isv_levels(&j.tcb_levels, "enclaveIdentity.tcbLevels")?,
        })
    }
}

// ------------------------------------------------------ PCK extension --

fn ext_err(reason: &str) -> AttestError {
    collateral("PCK SGX extension", reason.into())
}

fn seq<'a, 'b>(o: &'b BerObject<'a>) -> Result<&'b Vec<BerObject<'a>>, AttestError> {
    o.as_sequence().map_err(|_| ext_err("expected SEQUENCE"))
}

fn entry<'a, 'b>(o: &'b BerObject<'a>) -> Result<(String, &'b BerObject<'a>), AttestError> {
    let s = seq(o)?;
    if s.len() != 2 {
        return Err(ext_err("entry is not (OID, value)"));
    }
    let oid = s[0].as_oid().map_err(|_| ext_err("entry OID"))?;
    Ok((oid.to_id_string(), &s[1]))
}

fn octets<const N: usize>(o: &BerObject<'_>, what: &str) -> Result<[u8; N], AttestError> {
    let b = o.as_slice().map_err(|_| ext_err(what))?;
    b.try_into().map_err(|_| ext_err(what))
}

fn small_int(o: &BerObject<'_>, max: u32, what: &str) -> Result<u32, AttestError> {
    let v = o.as_u32().map_err(|_| ext_err(what))?;
    if v > max {
        return Err(ext_err(what));
    }
    Ok(v)
}

/// Parse the SGX extension of a PCK certificate (DER).
pub fn parse_pck_extensions(cert_der: &[u8]) -> Result<PckExtensions, AttestError> {
    let (rest, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| collateral("PCK certificate", e.to_string()))?;
    if !rest.is_empty() {
        return Err(collateral("PCK certificate", "trailing bytes".into()));
    }
    let mut found = cert
        .extensions()
        .iter()
        .filter(|e| e.oid.to_id_string() == SGX_EXTENSION_OID);
    let ext = found.next().ok_or_else(|| ext_err("missing"))?;
    if found.next().is_some() {
        return Err(ext_err("duplicated"));
    }
    let (rest, top) = parse_der(ext.value).map_err(|_| ext_err("malformed DER"))?;
    if !rest.is_empty() {
        return Err(ext_err("trailing bytes"));
    }
    let issuer_cn = cert
        .issuer()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .ok_or_else(|| collateral("PCK certificate", "issuer has no common name".into()))?;
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
    let base = SGX_EXTENSION_OID;
    let mut ppid = None;
    let mut instance_id = None;
    let mut configuration = None;
    let mut tcb = None;
    let mut pce_id = None;
    let mut fmspc = None;
    let mut sgx_type = None;
    for item in seq(&top)? {
        let (oid, value) = entry(item)?;
        let Some(suffix) = oid.strip_prefix(base).and_then(|s| s.strip_prefix('.')) else {
            return Err(ext_err("entry outside the SGX arc"));
        };
        let slot_taken = match suffix {
            "1" => ppid.replace(octets::<16>(value, "PPID")?).is_some(),
            "2" => tcb.replace(parse_tcb(value)?).is_some(),
            "3" => pce_id.replace(octets::<2>(value, "PCE-ID")?).is_some(),
            "4" => fmspc.replace(octets::<6>(value, "FMSPC")?).is_some(),
            "5" => sgx_type.replace(small_int(value, 2, "SGX type")?).is_some(),
            "6" => instance_id
                .replace(octets::<16>(value, "PlatformInstanceID")?)
                .is_some(),
            "7" => configuration.replace(parse_configuration(value)?).is_some(),
            _ => return Err(ext_err("unknown entry")),
        };
        if slot_taken {
            return Err(ext_err("duplicated entry"));
        }
    }
    let (sgx_components, pce_svn, cpu_svn) = tcb.ok_or_else(|| ext_err("missing TCB"))?;
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
        ppid: ppid.ok_or_else(|| ext_err("missing PPID"))?,
        sgx_components,
        pce_svn,
        cpu_svn,
        pce_id: pce_id.ok_or_else(|| ext_err("missing PCE-ID"))?,
        fmspc: fmspc.ok_or_else(|| ext_err("missing FMSPC"))?,
        sgx_type: sgx_type.ok_or_else(|| ext_err("missing SGX type"))?,
    })
}

fn parse_configuration(o: &BerObject<'_>) -> Result<PckPlatformConfiguration, AttestError> {
    let prefix = format!("{SGX_EXTENSION_OID}.7.");
    let mut c = PckPlatformConfiguration::default();
    for item in seq(o)? {
        let (oid, value) = entry(item)?;
        let flag = value
            .as_bool()
            .map_err(|_| ext_err("Configuration entry is not BOOLEAN"))?;
        let slot = match oid.strip_prefix(&prefix) {
            Some("1") => &mut c.dynamic_platform,
            Some("2") => &mut c.cached_keys,
            Some("3") => &mut c.smt_enabled,
            _ => return Err(ext_err("unknown Configuration entry")),
        };
        if slot.replace(flag).is_some() {
            return Err(ext_err("duplicated Configuration entry"));
        }
    }
    Ok(c)
}

fn parse_tcb(o: &BerObject<'_>) -> Result<([u8; 16], u16, [u8; 16]), AttestError> {
    let prefix = format!("{SGX_EXTENSION_OID}.2.");
    let mut comps = [None::<u8>; 16];
    let mut pce_svn = None;
    let mut cpu_svn = None;
    for item in seq(o)? {
        let (oid, value) = entry(item)?;
        let n: u32 = oid
            .strip_prefix(&prefix)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| ext_err("TCB entry OID"))?;
        let dup = match n {
            1..=16 => comps[(n - 1) as usize]
                .replace(small_int(value, 255, "TCB component")? as u8)
                .is_some(),
            17 => pce_svn
                .replace(small_int(value, 65535, "PCESVN")? as u16)
                .is_some(),
            18 => cpu_svn.replace(octets::<16>(value, "CPUSVN")?).is_some(),
            _ => return Err(ext_err("unknown TCB entry")),
        };
        if dup {
            return Err(ext_err("duplicated TCB entry"));
        }
    }
    let mut out = [0u8; 16];
    for (o, c) in out.iter_mut().zip(comps) {
        *o = c.ok_or_else(|| ext_err("missing TCB component"))?;
    }
    Ok((
        out,
        pce_svn.ok_or_else(|| ext_err("missing PCESVN"))?,
        cpu_svn.ok_or_else(|| ext_err("missing CPUSVN"))?,
    ))
}

// ---------------------------------------------------------- evaluation --

fn all_ge(have: &[u8], need: &[u8]) -> bool {
    have.iter().zip(need).all(|(h, n)| h >= n)
}

fn masked_eq(a: &[u8], b: &[u8], mask: &[u8]) -> bool {
    a.iter().zip(b).zip(mask).all(|((a, b), m)| a & m == b & m)
}

fn isv_status(levels: &[IsvTcbLevel], isv_svn: u16) -> Option<&IsvTcbLevel> {
    levels.iter().find(|l| l.isv_svn <= isv_svn)
}

fn push_unique(out: &mut Vec<String>, ids: &[String]) {
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

fn check_window(
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
fn check_embedded_root(certs: &[Vec<u8>], root: &TrustRoot) -> Result<(), AttestError> {
    match certs.last() {
        Some(last) if certs.len() >= 2 && last.as_slice() == root.der() => Ok(()),
        _ => Err(AttestError::CertificateChain(
            "chain does not end with the configured root".into(),
        )),
    }
}

/// Verify a signed collateral body: issuer chain to `root` (with the root
/// CA CRL) and the ECDSA P-256 signature over the exact body bytes.
fn verify_signed(
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
