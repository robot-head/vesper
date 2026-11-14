//! Pinned public vendor roots and intermediates.
//!
//! Each certificate is compiled in from `roots/` and carries the SHA-256 of
//! its DER encoding and the place it was obtained. [`PinnedCert::der`]
//! re-checks the digest before handing the bytes out, so a corrupted build
//! input is a typed error, never a silently different root.
//!
//! Provenance (also in `docs/parallel-results/aegis-vendor-collateral.md`):
//! the vendor endpoints (`certificates.trustedservices.intel.com`,
//! `kdsintf.amd.com`) are not reachable from the build sandbox, so every
//! certificate here was taken from two independent published copies whose
//! bytes are identical: the Intel root from `dcap-qvl` 0.3.4
//! (`src/TrustedRootCA.der`) and `go-tdx-guest` v0.3.1
//! (`verify/trusted_root.pem`); the AMD ARK/ASK from `sev` 6.0.0
//! (`src/certs/snp/builtin/*/{ark,ask}.pem`) and `go-sev-guest` v0.15.0
//! (`verify/trust/ask_ark_*.pem`); the ASVKs from `go-sev-guest` v0.15.0
//! (`verify/trust/ask_ark_*_vlek.pem`, Milan also `verify/testdata/milanvlek.testcer`).
//! The canonical vendor URLs are recorded per certificate.
//!
//! Arm CCA has no single public platform root (each silicon vendor runs its
//! own endorsement service), so none is pinned.

use crate::chain::TrustRoot;
use crate::error::AttestError;
use ring::digest;

/// One pinned certificate.
#[derive(Debug)]
pub struct PinnedCert {
    pub name: &'static str,
    /// Canonical vendor URL for this certificate.
    pub source_url: &'static str,
    /// SHA-256 of the DER encoding.
    pub sha256: [u8; 32],
    der: &'static [u8],
}

impl PinnedCert {
    /// The DER bytes, after checking they still hash to [`Self::sha256`].
    pub fn der(&self) -> Result<&'static [u8], AttestError> {
        let got = digest::digest(&digest::SHA256, self.der);
        if got.as_ref() != self.sha256 {
            return Err(AttestError::PinnedRootDigestMismatch { name: self.name });
        }
        Ok(self.der)
    }

    /// This certificate as a trust anchor.
    pub fn trust_root(&self) -> Result<TrustRoot, AttestError> {
        TrustRoot::from_der(self.der()?)
    }
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    assert!(b.len() == 64, "SHA-256 hex must be 64 characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("lower-case hex expected"),
    }
}

/// Intel SGX Root CA (also the root of TDX PCK, TCB-signing and QE-identity
/// chains).
pub static INTEL_SGX_ROOT_CA: PinnedCert = PinnedCert {
    name: "Intel SGX Root CA",
    source_url: "https://certificates.trustedservices.intel.com/Intel_SGX_Provisioning_Certification_RootCA.cer",
    sha256: hex32("44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3"),
    der: include_bytes!("../roots/intel_sgx_root_ca.der"),
};

/// AMD EPYC product lines with SEV-SNP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AmdProduct {
    Milan,
    Genoa,
    Turin,
}

impl AmdProduct {
    pub const ALL: [AmdProduct; 3] = [AmdProduct::Milan, AmdProduct::Genoa, AmdProduct::Turin];

    /// KDS product name.
    pub fn name(self) -> &'static str {
        match self {
            AmdProduct::Milan => "Milan",
            AmdProduct::Genoa => "Genoa",
            AmdProduct::Turin => "Turin",
        }
    }
}

/// Which AMD signing hierarchy: VCEK (per chip, via the ASK) or VLEK (per
/// cloud provider, via the ASVK).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AmdSigningKey {
    Vcek,
    Vlek,
}

macro_rules! amd {
    ($ident:ident, $name:literal, $file:literal, $url:literal, $sha:literal) => {
        static $ident: PinnedCert = PinnedCert {
            name: $name,
            source_url: $url,
            sha256: hex32($sha),
            der: include_bytes!(concat!("../roots/amd/", $file)),
        };
    };
}

amd!(
    MILAN_ARK,
    "AMD ARK-Milan",
    "milan_ark.der",
    "https://kdsintf.amd.com/vcek/v1/Milan/cert_chain",
    "69d063b45344d26a2e94e1f4210de49ef555308287d4c174445c95639a540bcd"
);
amd!(
    MILAN_ASK,
    "AMD SEV-Milan (ASK)",
    "milan_ask.der",
    "https://kdsintf.amd.com/vcek/v1/Milan/cert_chain",
    "67d303bd3905fd38db8b20e0793699870e7fa612eaad5dec358293fd8c0bac1b"
);
amd!(
    MILAN_ASVK,
    "AMD SEV-VLEK-Milan (ASVK)",
    "milan_asvk.der",
    "https://kdsintf.amd.com/vlek/v1/Milan/cert_chain",
    "c5e081f59b7efab1fe2f8b505e159704e72f29cab7ef7cf628a05a42439082f5"
);
amd!(
    GENOA_ARK,
    "AMD ARK-Genoa",
    "genoa_ark.der",
    "https://kdsintf.amd.com/vcek/v1/Genoa/cert_chain",
    "4c6598d19c18719c5dfd4a7d335f674e5bfe1d8f800cea2cf270c10d103db2f1"
);
amd!(
    GENOA_ASK,
    "AMD SEV-Genoa (ASK)",
    "genoa_ask.der",
    "https://kdsintf.amd.com/vcek/v1/Genoa/cert_chain",
    "5464738c1546aed5f2cecf1dc98c5c960a92e8913238a61711bc90ec6e828521"
);
amd!(
    GENOA_ASVK,
    "AMD SEV-VLEK-Genoa (ASVK)",
    "genoa_asvk.der",
    "https://kdsintf.amd.com/vlek/v1/Genoa/cert_chain",
    "197e610743a917d6b9bb982a5a9226ccc0a15b611be0619e626aca9151457372"
);
amd!(
    TURIN_ARK,
    "AMD ARK-Turin",
    "turin_ark.der",
    "https://kdsintf.amd.com/vcek/v1/Turin/cert_chain",
    "1f084161a44bb6d93778a904877d4819cafa5d05ef4193b2ded9dd9c73dd3f6a"
);
amd!(
    TURIN_ASK,
    "AMD SEV-Turin (ASK)",
    "turin_ask.der",
    "https://kdsintf.amd.com/vcek/v1/Turin/cert_chain",
    "5b77ef5fe7a7a004fd9032668fba9d0fda22f88c4442069a479636a6ae3b3185"
);
amd!(
    TURIN_ASVK,
    "AMD SEV-VLEK-Turin (ASVK)",
    "turin_asvk.der",
    "https://kdsintf.amd.com/vlek/v1/Turin/cert_chain",
    "104e10a8bd060a3c20a434261a57d0588fd65a88915b4f65b08bdecaf8df1a3c"
);

/// The AMD Root Key for `product`.
pub fn amd_ark(product: AmdProduct) -> &'static PinnedCert {
    match product {
        AmdProduct::Milan => &MILAN_ARK,
        AmdProduct::Genoa => &GENOA_ARK,
        AmdProduct::Turin => &TURIN_ARK,
    }
}

/// The intermediate that signs `key` certificates for `product`: the ASK
/// for VCEKs, the ASVK for VLEKs.
pub fn amd_intermediate(product: AmdProduct, key: AmdSigningKey) -> &'static PinnedCert {
    match (product, key) {
        (AmdProduct::Milan, AmdSigningKey::Vcek) => &MILAN_ASK,
        (AmdProduct::Milan, AmdSigningKey::Vlek) => &MILAN_ASVK,
        (AmdProduct::Genoa, AmdSigningKey::Vcek) => &GENOA_ASK,
        (AmdProduct::Genoa, AmdSigningKey::Vlek) => &GENOA_ASVK,
        (AmdProduct::Turin, AmdSigningKey::Vcek) => &TURIN_ASK,
        (AmdProduct::Turin, AmdSigningKey::Vlek) => &TURIN_ASVK,
    }
}

/// Every pinned certificate (for audits and the report).
pub fn all_pinned() -> Vec<&'static PinnedCert> {
    let mut v = vec![&INTEL_SGX_ROOT_CA];
    for p in AmdProduct::ALL {
        v.push(amd_ark(p));
        v.push(amd_intermediate(p, AmdSigningKey::Vcek));
        v.push(amd_intermediate(p, AmdSigningKey::Vlek));
    }
    v
}
