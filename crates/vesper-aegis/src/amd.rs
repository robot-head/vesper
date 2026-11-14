//! AMD SEV-SNP collateral: VCEK/VLEK certificate extensions, the ARK →
//! ASK/ASVK → VCEK/VLEK chain (AMD's RSA-PSS encoding), the ARK's CRL,
//! binding of the certificate to the report (chip ID and TCB), and TCB
//! status against a relying-party baseline.
//!
//! AMD publishes no signed TCB-status collateral comparable to Intel's TCB
//! info: a VCEK is issued per chip *and per TCB*, so the report's
//! `REPORTED_TCB` must equal the TCB in the VCEK's extensions, and whether
//! that TCB is current is the relying party's call. [`SnpTcbBaseline`]
//! carries that decision and maps it to [`TcbStatus`]: `UpToDate` at or
//! above the baseline, `OutOfDate` below it. `Revoked` comes from the CRL
//! (a revoked ASK/ASVK is an [`AttestError::Revoked`]).
//!
//! Report versions 2–5 are accepted (see [`crate::snp`]). From version 3
//! the report names its processor (CPUID family/model), which must be the
//! collateral's product: a Genoa report cannot be checked against a Milan
//! chain even if a certificate happened to verify.
//!
//! Anti-rollback: [`VerifiedSnpReport::crl_this_update`] records the ARK
//! CRL's `thisUpdate`; [`check_snp_crl_floor`] rejects a CRL older than
//! one already accepted ([`AttestError::CollateralDowngrade`]).

use crate::chain::{
    Curve, RevocationScope, TrustRoot, crl_this_update, pem_certificates, verify_chain,
    verify_chain_with_crls,
};
use crate::error::AttestError;
use crate::intel::TcbStatus;
use crate::roots::{AmdProduct, AmdSigningKey, amd_intermediate};
use crate::snp::SnpReport;
use x509_parser::der_parser::der::parse_der;

/// AMD's arc for VCEK/VLEK extensions.
pub const AMD_EXT_ARC: &str = "1.3.6.1.4.1.3704.1";

/// Report flag bit 1: `MASK_CHIP_ID` (chip ID reported as zero).
pub const SNP_FLAG_MASK_CHIP_ID: u32 = 1 << 1;

/// A decoded SEV-SNP `TCB_VERSION`. Milan and Genoa: bootloader, TEE,
/// 4 reserved bytes, SNP, microcode. Turin: FMC, bootloader, TEE, SNP,
/// 3 reserved bytes, microcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AmdTcb {
    /// Turin only.
    pub fmc: Option<u8>,
    pub bootloader: u8,
    pub tee: u8,
    pub snp: u8,
    pub microcode: u8,
}

impl AmdTcb {
    /// Decode a raw `TCB_VERSION`; reserved bytes must be zero.
    pub fn from_raw(product: AmdProduct, raw: u64) -> Result<Self, AttestError> {
        let b = raw.to_le_bytes();
        let (tcb, reserved) = match product {
            AmdProduct::Milan | AmdProduct::Genoa => (
                AmdTcb {
                    fmc: None,
                    bootloader: b[0],
                    tee: b[1],
                    snp: b[6],
                    microcode: b[7],
                },
                &b[2..6],
            ),
            AmdProduct::Turin => (
                AmdTcb {
                    fmc: Some(b[0]),
                    bootloader: b[1],
                    tee: b[2],
                    snp: b[3],
                    microcode: b[7],
                },
                &b[4..7],
            ),
        };
        if reserved.iter().any(|&x| x != 0) {
            return Err(AttestError::Collateral {
                what: "TCB_VERSION",
                reason: "reserved bytes are not zero".into(),
            });
        }
        Ok(tcb)
    }

    pub fn to_raw(self, product: AmdProduct) -> u64 {
        let b = match product {
            AmdProduct::Milan | AmdProduct::Genoa => [
                self.bootloader,
                self.tee,
                0,
                0,
                0,
                0,
                self.snp,
                self.microcode,
            ],
            AmdProduct::Turin => [
                self.fmc.unwrap_or(0),
                self.bootloader,
                self.tee,
                self.snp,
                0,
                0,
                0,
                self.microcode,
            ],
        };
        u64::from_le_bytes(b)
    }

    /// Every component at least `floor`'s.
    pub fn at_least(&self, floor: &AmdTcb) -> bool {
        self.fmc.unwrap_or(0) >= floor.fmc.unwrap_or(0)
            && self.bootloader >= floor.bootloader
            && self.tee >= floor.tee
            && self.snp >= floor.snp
            && self.microcode >= floor.microcode
    }
}

/// The relying party's view of the current AMD TCB for a product (for
/// example from AMD security bulletins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnpTcbBaseline {
    pub latest: AmdTcb,
}

impl SnpTcbBaseline {
    pub fn status(&self, reported: &AmdTcb) -> TcbStatus {
        if reported.at_least(&self.latest) {
            TcbStatus::UpToDate
        } else {
            TcbStatus::OutOfDate
        }
    }
}

/// Values from a VCEK or VLEK certificate's AMD extensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmdCertExtensions {
    /// For example `Milan-B0`.
    pub product_name: String,
    pub tcb: AmdTcb,
    /// VCEK only: the chip ID (64 bytes on Milan/Genoa, 8 on Turin).
    pub hw_id: Option<Vec<u8>>,
    /// VLEK only: the cloud service provider ID.
    pub csp_id: Option<String>,
}

fn ext_err(reason: impl Into<String>) -> AttestError {
    AttestError::Collateral {
        what: "AMD certificate extension",
        reason: reason.into(),
    }
}

fn ext_u8(value: &[u8], what: &str) -> Result<u8, AttestError> {
    let (rest, obj) = parse_der(value).map_err(|_| ext_err(format!("{what}: malformed DER")))?;
    if !rest.is_empty() {
        return Err(ext_err(format!("{what}: trailing bytes")));
    }
    let v = obj
        .as_u32()
        .map_err(|_| ext_err(format!("{what}: not an INTEGER")))?;
    u8::try_from(v).map_err(|_| ext_err(format!("{what}: out of range")))
}

fn ext_string(value: &[u8], what: &str) -> Result<String, AttestError> {
    let (rest, obj) = parse_der(value).map_err(|_| ext_err(format!("{what}: malformed DER")))?;
    if !rest.is_empty() {
        return Err(ext_err(format!("{what}: trailing bytes")));
    }
    obj.as_str()
        .map(str::to_owned)
        .map_err(|_| ext_err(format!("{what}: not a string")))
}

fn hw_id_len(product: AmdProduct) -> usize {
    if product == AmdProduct::Turin { 8 } else { 64 }
}

/// The hwID extension value: raw chip ID bytes, or a DER `OCTET STRING`
/// holding them. Any other length is an error (reported by the caller).
fn hw_id_value(value: &[u8], product: AmdProduct) -> Result<Vec<u8>, AttestError> {
    let n = hw_id_len(product);
    if value.len() == n + 2 && value[0] == 0x04 && usize::from(value[1]) == n {
        return Ok(value[2..].to_vec());
    }
    Ok(value.to_vec())
}

/// Parse the AMD extensions of a VCEK/VLEK certificate (DER).
pub fn parse_amd_cert_extensions(
    cert_der: &[u8],
    product: AmdProduct,
) -> Result<AmdCertExtensions, AttestError> {
    let (rest, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| ext_err(format!("certificate: {e}")))?;
    if !rest.is_empty() {
        return Err(ext_err("certificate: trailing bytes"));
    }
    let mut product_name = None;
    let mut bl = None;
    let mut tee = None;
    let mut snp = None;
    let mut ucode = None;
    let mut fmc = None;
    let mut hw_id = None;
    let mut csp_id = None;
    for e in cert.extensions() {
        let oid = e.oid.to_id_string();
        let Some(suffix) = oid
            .strip_prefix(AMD_EXT_ARC)
            .and_then(|s| s.strip_prefix('.'))
        else {
            continue;
        };
        let dup = match suffix {
            "1" => false, // structVersion
            "2" => product_name
                .replace(ext_string(e.value, "productName")?)
                .is_some(),
            "3.1" => bl.replace(ext_u8(e.value, "blSPL")?).is_some(),
            "3.2" => tee.replace(ext_u8(e.value, "teeSPL")?).is_some(),
            "3.3" => snp.replace(ext_u8(e.value, "snpSPL")?).is_some(),
            "3.4" | "3.5" | "3.6" | "3.7" => {
                if ext_u8(e.value, "reserved SPL")? != 0 {
                    return Err(ext_err("reserved SPL is not zero"));
                }
                false
            }
            "3.8" => ucode.replace(ext_u8(e.value, "ucodeSPL")?).is_some(),
            "3.9" => fmc.replace(ext_u8(e.value, "fmcSPL")?).is_some(),
            // hwID: the chip ID, either as the raw extension value (AMD's
            // KDS certificates) or wrapped in a DER OCTET STRING (as some
            // later issuers encode it); the two are told apart by length.
            "4" => hw_id.replace(hw_id_value(e.value, product)?).is_some(),
            "5" => csp_id.replace(ext_string(e.value, "cspID")?).is_some(),
            other => return Err(ext_err(format!("unknown AMD extension .{other}"))),
        };
        if dup {
            return Err(ext_err(format!("duplicated extension .{suffix}")));
        }
    }
    let product_name = product_name.ok_or_else(|| ext_err("missing productName"))?;
    let family = product_name.split('-').next().unwrap_or("");
    if family != product.name() {
        return Err(AttestError::CollateralMismatch {
            what: "AMD certificate product name",
        });
    }
    let fmc = match (product, fmc) {
        (AmdProduct::Turin, Some(f)) => Some(f),
        (AmdProduct::Turin, None) => return Err(ext_err("missing fmcSPL")),
        (_, Some(_)) => return Err(ext_err("fmcSPL outside Turin")),
        (_, None) => None,
    };
    if let Some(id) = &hw_id {
        let want = hw_id_len(product);
        if id.len() != want {
            return Err(ext_err(format!(
                "hwID is {} bytes, expected {want}",
                id.len()
            )));
        }
    }
    Ok(AmdCertExtensions {
        product_name,
        tcb: AmdTcb {
            fmc,
            bootloader: bl.ok_or_else(|| ext_err("missing blSPL"))?,
            tee: tee.ok_or_else(|| ext_err("missing teeSPL"))?,
            snp: snp.ok_or_else(|| ext_err("missing snpSPL"))?,
            microcode: ucode.ok_or_else(|| ext_err("missing ucodeSPL"))?,
        },
        hw_id,
        csp_id,
    })
}

/// Collateral for one SEV-SNP report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnpCollateral {
    pub product: AmdProduct,
    pub signing_key: AmdSigningKey,
    /// The VCEK or VLEK (DER).
    pub leaf_der: Vec<u8>,
    /// The ASK/ASVK (DER); `None` uses the pinned one for
    /// `(product, signing_key)`.
    pub intermediate_der: Option<Vec<u8>>,
    /// The ARK's CRL for the product (DER). `None` skips revocation, which
    /// [`VerifiedSnpReport::revocation_checked`] records.
    pub crl_der: Option<Vec<u8>>,
}

impl SnpCollateral {
    /// Collateral from a KDS `cert_chain` response (`ASK` or `ASVK`, then
    /// `ARK`, PEM): the chain must hold exactly two certificates, and the
    /// ARK must be byte-identical to the pinned ARK for `product` (KDS
    /// serves a copy of the root that path building would otherwise
    /// ignore). The intermediate is used as served.
    pub fn from_kds_cert_chain(
        product: AmdProduct,
        signing_key: AmdSigningKey,
        leaf_der: Vec<u8>,
        cert_chain_pem: &[u8],
        crl_der: Option<Vec<u8>>,
    ) -> Result<Self, AttestError> {
        let certs = pem_certificates(cert_chain_pem)?;
        let [intermediate, ark] = <[Vec<u8>; 2]>::try_from(certs).map_err(|_| {
            AttestError::CertificateChain("KDS cert_chain must hold ASK/ASVK and ARK".into())
        })?;
        if ark.as_slice() != crate::roots::amd_ark(product).der()? {
            return Err(AttestError::CertificateChain(
                "KDS cert_chain ARK is not the pinned ARK".into(),
            ));
        }
        Ok(SnpCollateral {
            product,
            signing_key,
            leaf_der,
            intermediate_der: Some(intermediate),
            crl_der,
        })
    }
}

/// An SEV-SNP report whose chain, signature and certificate binding
/// verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSnpReport {
    pub report: SnpReport,
    pub product: AmdProduct,
    pub signing_key: AmdSigningKey,
    pub certificate: AmdCertExtensions,
    pub reported_tcb: AmdTcb,
    /// `None` when no baseline was supplied.
    pub status: Option<TcbStatus>,
    pub revocation_checked: bool,
    /// The ARK CRL's `thisUpdate` (Unix seconds), when a CRL was checked.
    pub crl_this_update: Option<u64>,
}

/// Reject an ARK CRL older than one the verifier already accepted: `floor`
/// is the latest `thisUpdate` (Unix seconds) seen for the product. A
/// verification without a CRL passes only when `floor` is `None`.
pub fn check_snp_crl_floor(
    verified: &VerifiedSnpReport,
    floor: Option<u64>,
) -> Result<(), AttestError> {
    match (floor, verified.crl_this_update) {
        (None, _) => Ok(()),
        (Some(_), None) => Err(AttestError::MissingCollateral {
            what: "ARK CRL (a newer one was accepted before)",
        }),
        (Some(floor), Some(have)) if have < floor => Err(AttestError::CollateralDowngrade {
            what: "ARK CRL thisUpdate",
            have,
            floor,
        }),
        _ => Ok(()),
    }
}

/// Verify an SEV-SNP report against its VCEK/VLEK, the ASK/ASVK and the
/// product's ARK (`root`):
///
/// - the report's `SIGNING_KEY` matches the collateral (VCEK or VLEK);
/// - the chain verifies to `root` at `now_ns` (AMD's RSA-PSS encoding),
///   and, with a CRL, the ASK/ASVK is not revoked and the CRL is current
///   and signed by the ARK;
/// - the certificate's product name matches, its TCB equals the report's
///   `REPORTED_TCB`, and (VCEK) its hwID equals the report's `CHIP_ID`;
/// - the ECDSA P-384 signature over report bytes `0..0x2A0` verifies.
///
/// With a `baseline`, the reported TCB is mapped to a [`TcbStatus`].
/// Report data is not interpreted here.
pub fn verify_snp_collateral(
    bytes: &[u8],
    collateral: &SnpCollateral,
    root: &TrustRoot,
    now_ns: u64,
    baseline: Option<&SnpTcbBaseline>,
) -> Result<VerifiedSnpReport, AttestError> {
    let report = SnpReport::parse(bytes)?;
    if report.version >= 3 {
        match report.product() {
            Some(p) if p == collateral.product => {}
            Some(_) => {
                return Err(AttestError::CollateralMismatch {
                    what: "report CPUID product vs collateral product",
                });
            }
            None => {
                return Err(AttestError::Collateral {
                    what: "SNP report CPUID",
                    reason: format!(
                        "unknown processor family {:#04x} model {:#04x}",
                        report.cpuid[0], report.cpuid[1]
                    ),
                });
            }
        }
    }
    let key = match report.signing_key() {
        0 => AmdSigningKey::Vcek,
        1 => AmdSigningKey::Vlek,
        other => {
            return Err(AttestError::UnsupportedAlgorithm {
                what: "SNP signing key",
                value: other.into(),
            });
        }
    };
    if key != collateral.signing_key {
        return Err(AttestError::CollateralMismatch {
            what: "report SIGNING_KEY vs collateral (VCEK/VLEK)",
        });
    }
    let intermediate = match &collateral.intermediate_der {
        Some(d) => d.clone(),
        None => amd_intermediate(collateral.product, key).der()?.to_vec(),
    };
    let chain = [intermediate];
    let leaf_key = match &collateral.crl_der {
        Some(crl) => verify_chain_with_crls(
            &collateral.leaf_der,
            &chain,
            root,
            now_ns,
            Curve::P384,
            &[crl.as_slice()],
            RevocationScope::IssuedByRoot,
        )?,
        None => verify_chain(&collateral.leaf_der, &chain, root, now_ns, Curve::P384)?,
    };
    let crl_this_update = collateral
        .crl_der
        .as_deref()
        .map(crl_this_update)
        .transpose()?;
    let certificate = parse_amd_cert_extensions(&collateral.leaf_der, collateral.product)?;
    let reported_tcb = AmdTcb::from_raw(collateral.product, report.reported_tcb)?;
    if certificate.tcb != reported_tcb {
        return Err(AttestError::CollateralMismatch {
            what: "certificate TCB vs REPORTED_TCB",
        });
    }
    match key {
        AmdSigningKey::Vcek => {
            if report.flags & SNP_FLAG_MASK_CHIP_ID != 0 {
                return Err(AttestError::CollateralMismatch {
                    what: "MASK_CHIP_ID set: a VCEK cannot be bound to the report",
                });
            }
            let id = certificate
                .hw_id
                .as_deref()
                .ok_or_else(|| ext_err("VCEK without hwID"))?;
            if report.chip_id[..id.len()] != *id
                || report.chip_id[id.len()..].iter().any(|&b| b != 0)
            {
                return Err(AttestError::CollateralMismatch {
                    what: "VCEK hwID vs CHIP_ID",
                });
            }
        }
        AmdSigningKey::Vlek => {
            if certificate.hw_id.is_some() {
                return Err(ext_err("VLEK with hwID"));
            }
        }
    }
    leaf_key.verify_fixed(
        &report.signed_bytes(),
        &report.signature_r_s,
        "SNP report (VCEK/VLEK)",
    )?;
    Ok(VerifiedSnpReport {
        status: baseline.map(|b| b.status(&reported_tcb)),
        report,
        product: collateral.product,
        signing_key: key,
        certificate,
        reported_tcb,
        revocation_checked: collateral.crl_der.is_some(),
        crl_this_update,
    })
}
