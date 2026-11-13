//! Test-generated keys, certificate chains and hardware evidence. Nothing
//! here comes from a real vendor; vendor roots are untested.
#![allow(dead_code)]

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair as _};
use vesper_aegis::cca::{self, CcaPlatformClaims, CcaRealmClaims};
use vesper_aegis::claims::{GuestRuntimeClaims, RegisterHash, single_extend};
use vesper_aegis::snp::SnpReport;
use vesper_aegis::tdx::{QeReportBody, TdReportBody, TdxQuote, TdxQuoteHeader};
use vesper_aegis::{Curve, TrustRoot};

/// 2026-01-01T00:00:00Z in nanoseconds.
pub const NOW_NS: u64 = 1_767_225_600 * 1_000_000_000;

pub struct Signer {
    pub curve: Curve,
    key: EcdsaKeyPair,
    pub pkcs8: Vec<u8>,
}

impl Signer {
    pub fn generate(curve: Curve) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(Self::alg(curve), &rng)
            .unwrap()
            .as_ref()
            .to_vec();
        Self::from_pkcs8(curve, pkcs8)
    }

    fn alg(curve: Curve) -> &'static signature::EcdsaSigningAlgorithm {
        match curve {
            Curve::P256 => &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            Curve::P384 => &signature::ECDSA_P384_SHA384_FIXED_SIGNING,
        }
    }

    fn from_pkcs8(curve: Curve, pkcs8: Vec<u8>) -> Self {
        let key = EcdsaKeyPair::from_pkcs8(Self::alg(curve), &pkcs8, &SystemRandom::new()).unwrap();
        Signer { curve, key, pkcs8 }
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.key
            .sign(&SystemRandom::new(), msg)
            .unwrap()
            .as_ref()
            .to_vec()
    }

    pub fn sec1(&self) -> Vec<u8> {
        self.key.public_key().as_ref().to_vec()
    }

    pub fn raw_xy(&self) -> Vec<u8> {
        self.sec1()[1..].to_vec()
    }

    fn rcgen_key(&self) -> KeyPair {
        let alg = match self.curve {
            Curve::P256 => &rcgen::PKCS_ECDSA_P256_SHA256,
            Curve::P384 => &rcgen::PKCS_ECDSA_P384_SHA384,
        };
        KeyPair::from_pkcs8_der_and_sign_algo(&self.pkcs8.as_slice().into(), alg).unwrap()
    }
}

/// root -> intermediate -> leaf, all on `curve`; the leaf key is `leaf`.
pub struct TestChain {
    pub root: TrustRoot,
    pub root_der: Vec<u8>,
    pub intermediate_der: Vec<u8>,
    pub leaf_der: Vec<u8>,
    pub leaf: Signer,
    pem: String,
}

fn ca_params(cn: &str) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, cn);
    p
}

pub fn chain(curve: Curve, leaf_curve: Curve, leaf_expired: bool) -> TestChain {
    let root_key = Signer::generate(curve).rcgen_key();
    let root_params = ca_params("Aegis Test Root");
    let root_cert = root_params.self_signed(&root_key).unwrap();
    let root_issuer = Issuer::new(root_params, root_key);

    let inter_key = Signer::generate(curve).rcgen_key();
    let inter_params = ca_params("Aegis Test Intermediate");
    let inter_cert = inter_params.signed_by(&inter_key, &root_issuer).unwrap();
    let inter_issuer = Issuer::new(inter_params, inter_key);

    let leaf = Signer::generate(leaf_curve);
    let mut leaf_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "Aegis Test Attestation Key");
    if leaf_expired {
        leaf_params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        leaf_params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let leaf_cert = leaf_params
        .signed_by(&leaf.rcgen_key(), &inter_issuer)
        .unwrap();

    let root_der = root_cert.der().to_vec();
    let pem = format!("{}{}{}", leaf_cert.pem(), inter_cert.pem(), root_cert.pem());
    TestChain {
        pem,
        root: TrustRoot::from_der(root_der.clone()).unwrap(),
        root_der,
        intermediate_der: inter_cert.der().to_vec(),
        leaf_der: leaf_cert.der().to_vec(),
        leaf,
    }
}

impl TestChain {
    pub fn der_chain(&self) -> Vec<Vec<u8>> {
        vec![self.leaf_der.clone(), self.intermediate_der.clone()]
    }

    pub fn pem_chain(&self) -> Vec<u8> {
        self.pem.clone().into_bytes()
    }
}

pub fn runtime() -> GuestRuntimeClaims {
    GuestRuntimeClaims {
        channel_binding: [0xC4; 48],
        loom_lock_blake3: [0x10; 32],
        olean_spec_mif: [0x0E; 32],
    }
}

pub const MRTD: [u8; 48] = [0x7D; 48];
pub const SNP_MEASUREMENT: [u8; 48] = [0x5A; 48];
pub const CCA_RIM: [u8; 32] = [0xCA; 32];

pub struct TdxParams {
    pub td_attributes: u64,
    pub tee_tcb_svn: [u8; 16],
    pub runtime: GuestRuntimeClaims,
}

impl Default for TdxParams {
    fn default() -> Self {
        let mut tee_tcb_svn = [0u8; 16];
        tee_tcb_svn[0] = 3;
        tee_tcb_svn[1] = 1;
        TdxParams {
            td_attributes: 0,
            tee_tcb_svn,
            runtime: runtime(),
        }
    }
}

/// A TDX quote structure signed by `att_key` with a QE report signed by
/// the PCK leaf of `pck`.
pub fn tdx_quote_struct(pck: &TestChain, att_key: &Signer, p: &TdxParams) -> TdxQuote {
    let header = TdxQuoteHeader {
        version: 4,
        att_key_type: 2,
        tee_type: 0x81,
        qe_svn: 7,
        pce_svn: 11,
        qe_vendor_id: [0x93; 16],
        user_data: [0; 20],
    };
    let body = TdReportBody {
        tee_tcb_svn: p.tee_tcb_svn,
        mr_seam: [1; 48],
        mr_signer_seam: [0; 48],
        seam_attributes: 0,
        td_attributes: p.td_attributes,
        xfam: 0xE7,
        mr_td: MRTD,
        mr_config_id: [0; 48],
        mr_owner: [0; 48],
        mr_owner_config: [0; 48],
        rtmr: [
            [0x11; 48],
            [0x12; 48],
            single_extend(RegisterHash::Sha384, 48, &p.runtime.loom_lock_blake3)
                .try_into()
                .unwrap(),
            single_extend(RegisterHash::Sha384, 48, &p.runtime.olean_spec_mif)
                .try_into()
                .unwrap(),
        ],
        report_data: p.runtime.report_data(),
    };
    let attestation_key: [u8; 64] = att_key.raw_xy().try_into().unwrap();
    let qe_auth_data = b"qe-auth".to_vec();
    let qe_report = QeReportBody {
        cpu_svn: [2; 16],
        misc_select: 0,
        attributes: [0; 16],
        mr_enclave: [0xE1; 32],
        mr_signer: [0xE2; 32],
        isv_prod_id: 1,
        isv_svn: 8,
        report_data: TdxQuote::expected_qe_report_data(&attestation_key, &qe_auth_data),
    };
    let qe_report_signature = pck.leaf.sign(&qe_report.encode()).try_into().unwrap();
    let mut quote = TdxQuote {
        header,
        body,
        quote_signature: [0; 64],
        attestation_key,
        qe_report,
        qe_report_signature,
        qe_auth_data,
        pck_chain_pem: pck.pem_chain(),
    };
    quote.quote_signature = att_key.sign(&quote.signed_bytes()).try_into().unwrap();
    quote
}

pub fn tdx_quote(pck: &TestChain, att_key: &Signer, p: &TdxParams) -> Vec<u8> {
    tdx_quote_struct(pck, att_key, p).encode()
}

pub struct SnpParams {
    pub policy: u64,
    pub reported_tcb: u64,
    pub vmpl: u32,
    pub runtime: GuestRuntimeClaims,
}

impl Default for SnpParams {
    fn default() -> Self {
        SnpParams {
            // Bit 17 is reserved-must-be-one in real policies; SMT, debug off.
            policy: 1 << 17,
            reported_tcb: vesper_aegis::claims::SnpTcbVersion {
                bootloader: 3,
                tee: 0,
                snp: 20,
                microcode: 209,
            }
            .to_u64(),
            vmpl: 0,
            runtime: runtime(),
        }
    }
}

pub fn snp_report_struct(vcek: &Signer, p: &SnpParams) -> SnpReport {
    let mut r = SnpReport {
        version: 3,
        guest_svn: 1,
        policy: p.policy,
        family_id: [0; 16],
        image_id: [0; 16],
        vmpl: p.vmpl,
        signature_algo: 1,
        current_tcb: p.reported_tcb,
        platform_info: 1,
        flags: 0,
        report_data: p.runtime.report_data(),
        measurement: SNP_MEASUREMENT,
        host_data: [0; 32],
        id_key_digest: [0; 48],
        author_key_digest: [0; 48],
        report_id: [0x42; 32],
        report_id_ma: [0xFF; 32],
        reported_tcb: p.reported_tcb,
        cpuid: [0x19, 1, 1],
        chip_id: [0xC1; 64],
        committed_tcb: p.reported_tcb,
        current_version: [1, 55, 1],
        committed_version: [1, 55, 1],
        launch_tcb: p.reported_tcb,
        signature_r_s: [0; 96],
    };
    r.signature_r_s = vcek.sign(&r.signed_bytes()).try_into().unwrap();
    r
}

pub fn snp_report(vcek: &Signer, p: &SnpParams) -> Vec<u8> {
    snp_report_struct(vcek, p).encode()
}

pub struct CcaParams {
    pub lifecycle: u64,
    pub runtime: GuestRuntimeClaims,
    pub realm_hash: RegisterHash,
}

impl Default for CcaParams {
    fn default() -> Self {
        CcaParams {
            lifecycle: 0x3000,
            runtime: runtime(),
            realm_hash: RegisterHash::Sha256,
        }
    }
}

pub fn cca_realm_claims(rak: &Signer, p: &CcaParams) -> CcaRealmClaims {
    let n = p.realm_hash.len();
    CcaRealmClaims {
        challenge: p.runtime.report_data(),
        personalization: [0; 64],
        hash_algo: p.realm_hash,
        public_key: cca::encode_realm_cose_key(&rak.sec1()),
        public_key_hash_algo: RegisterHash::Sha256,
        initial_measurement: vec![0xCA; n],
        extensible_measurements: [
            vec![0; n],
            vec![0; n],
            single_extend(p.realm_hash, 64, &p.runtime.loom_lock_blake3),
            single_extend(p.realm_hash, 64, &p.runtime.olean_spec_mif),
        ],
    }
}

pub fn cca_token(iak: &Signer, rak: &Signer, p: &CcaParams) -> Vec<u8> {
    let realm = cca_realm_claims(rak, p);
    let platform = CcaPlatformClaims {
        challenge: cca::rak_hash(realm.public_key_hash_algo, &realm.public_key),
        instance_id: vec![1; 33],
        implementation_id: vec![2; 32],
        lifecycle: p.lifecycle,
        hash_algo: RegisterHash::Sha256,
    };
    let realm_tok = cca::build_sign1(Curve::P384, realm.to_cbor(), |d| rak.sign(d));
    let platform_tok = cca::build_sign1(iak.curve, platform.to_cbor(), |d| iak.sign(d));
    cca::encode_collection(&platform_tok, &realm_tok)
}

/// Deterministic test RNG.
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}
