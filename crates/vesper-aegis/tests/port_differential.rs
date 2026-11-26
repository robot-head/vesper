//! The Vesper build against the last Rust implementation (SH6 lane 8,
//! #1403; the differential of port wave P3a lane 8, #1043): every ported
//! parser and the policy check give the same result, success or the same
//! typed error, on the vendor samples and on 4,000 mutations of each.
//!
//! The Rust parsers were deleted when the port became the only build, so
//! each test folds the `Debug` text of every result, in order, into a
//! SHA-256 digest and compares it with the digest the Rust build gave on
//! the same inputs, recorded in `tests/data/port_differential.sha256` at
//! the last commit with the Rust implementation (see the lane report,
//! docs/parallel-results/rust-exit-aegis.md). The inputs are generated
//! from fixed seeds, so the digest covers exactly the cases the paired
//! differential compared. `AEGIS_DIFFERENTIAL_RECORD=1` prints the
//! digests instead of checking them.

mod support;

use std::path::PathBuf;
use vesper_aegis as port;

/// The results of one test, in order.
struct Results {
    name: &'static str,
    digest: ring::digest::Context,
    cases: u64,
}

impl Results {
    fn new(name: &'static str) -> Self {
        Results {
            name,
            digest: ring::digest::Context::new(&ring::digest::SHA256),
            cases: 0,
        }
    }

    /// Fold in one result; whether it is a success.
    fn add(&mut self, what: &str, result: impl std::fmt::Debug) -> bool {
        let text = format!("{what}: {result:?}\n");
        self.digest.update(text.as_bytes());
        self.cases += 1;
        !text.starts_with(&format!("{what}: Err"))
    }

    /// Compare with the digest the Rust build recorded.
    fn check(self) {
        let hex: String = self
            .digest
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let line = format!("{} {} {}", self.name, self.cases, hex);
        if std::env::var_os("AEGIS_DIFFERENTIAL_RECORD").is_some() {
            println!("{line}");
            return;
        }
        let recorded = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/port_differential.sha256"),
        )
        .expect("the recorded digests");
        let want = recorded
            .lines()
            .find(|l| l.split(' ').next() == Some(self.name))
            .unwrap_or_else(|| panic!("no recorded digest for {}", self.name));
        assert_eq!(
            line, want,
            "{}: the Vesper build differs from the Rust build's results",
            self.name
        );
    }
}

fn data(path: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/vendor")
        .join(path);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn unhex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(u8::is_ascii_hexdigit).collect();
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// Truncations, bit flips, insertions, extreme length words, zeroed runs
/// and appended zero padding.
fn mutate(rng: &mut SplitMix, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    match rng.below(6) {
        0 => v.truncate(rng.below(v.len() as u64 + 1) as usize),
        1 => {
            for _ in 0..=rng.below(4) {
                let i = rng.below(v.len() as u64) as usize;
                if let Some(b) = v.get_mut(i) {
                    *b ^= 1 << rng.below(8);
                }
            }
        }
        2 => {
            let i = rng.below(v.len() as u64 + 1) as usize;
            let extra: Vec<u8> = (0..rng.below(16)).map(|_| rng.next() as u8).collect();
            v.splice(i..i, extra);
        }
        3 => {
            let i = rng.below(v.len().saturating_sub(4) as u64) as usize;
            let w = [0u32, 1, 0xFFFF, 0xFFFF_FFFF, 0x7FFF_FFFF, 0x1_0000][rng.below(6) as usize];
            if i + 4 <= v.len() {
                v[i..i + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        4 => {
            let i = rng.below(v.len() as u64) as usize;
            let n = (rng.below(64) as usize).min(v.len() - i.min(v.len()));
            v[i..i + n].fill(0);
        }
        _ => v.extend(std::iter::repeat_n(0, rng.below(64) as usize + 1)),
    }
    v
}

fn snp(r: &mut Results, b: &[u8]) -> bool {
    r.add("SnpReport::parse", port::snp::SnpReport::parse(b))
}

fn tdx(r: &mut Results, b: &[u8]) -> bool {
    r.add(
        "TdxQuote::parse_padded",
        port::tdx::TdxQuote::parse_padded(b),
    );
    r.add(
        "TdxQuote::parse_prefix",
        port::tdx::TdxQuote::parse_prefix(b),
    );
    r.add("TdxQuote::parse", port::tdx::TdxQuote::parse(b))
}

fn sgx(r: &mut Results, b: &[u8]) -> bool {
    r.add(
        "SgxQuote::parse_padded",
        port::sgx::SgxQuote::parse_padded(b),
    );
    r.add(
        "SgxQuote::parse_prefix",
        port::sgx::SgxQuote::parse_prefix(b),
    );
    r.add("SgxQuote::parse", port::sgx::SgxQuote::parse(b))
}

/// Every parser that accepted at least one mutation did so (the mutations
/// reach past the first field); every result goes into `r`.
fn sweep(r: &mut Results, name: &str, seeds: &[Vec<u8>], check: fn(&mut Results, &[u8]) -> bool) {
    let mut rng = SplitMix(0xAE61_5000 ^ name.len() as u64);
    let mut accepted = 0;
    for seed in seeds {
        assert!(check(r, seed), "{name}: the vendor sample parses");
        for _ in 0..4000 {
            accepted += usize::from(check(r, &mutate(&mut rng, seed)));
        }
    }
    assert!(
        accepted > 0,
        "{name}: some mutation is still a valid encoding"
    );
}

#[test]
fn snp_reports_agree() {
    let virtee = unhex(&String::from_utf8(data("snp-virtee-sev/report_milan.hex")).unwrap());
    let seeds = vec![
        virtee,
        data("snp-oak/milan_report.bin"),
        data("snp-oak/genoa_report.bin"),
        data("snp-oak/turin_report.bin"),
        data("snp-trustee/test-vlek-report.bin"),
    ];
    let mut r = Results::new("snp_reports_agree");
    sweep(&mut r, "snp", &seeds, snp);
    // Version 6 on Venice, with and without nonzero reserved ETCB bits.
    let mut v6 = seeds[0].clone();
    v6[0..4].copy_from_slice(&6u32.to_le_bytes());
    v6[0x188] = 0x1A;
    v6[0x189] = 0x50;
    for at in [0x220, 0x240, 0x260] {
        v6[at..at + 14].copy_from_slice(&[3; 14]);
    }
    assert!(snp(&mut r, &v6));
    v6[0x220 + 20] = 1;
    assert!(!snp(&mut r, &v6));
    for b in [[0u8; 32], [7; 32]] {
        r.add("SnpEtcb::parse", port::snp::SnpEtcb::parse(b));
    }
    r.check();
}

#[test]
fn tdx_quotes_agree() {
    let seeds = vec![
        data("tdx-dcap-qvl/tdx_quote"),
        data("tdx-dcap-qvl-0.7/tdx_quote_td15ex"),
        data("tdx-dcap-qvl-0.7/tdx_quote_outdated"),
        data("tdx-go-tdx-guest/tdx_prod_quote_SPR_E4.dat"),
        data("tdx-go-tdx-guest/quote_sample_v5.dat"),
        data("tdx-trustee/tdx_quote_5.dat"),
    ];
    let seeds: Vec<Vec<u8>> = seeds
        .into_iter()
        .map(|q| {
            let (_, used) = port::tdx::TdxQuote::parse_prefix(&q).unwrap();
            q[..used].to_vec()
        })
        .collect();
    let mut r = Results::new("tdx_quotes_agree");
    sweep(&mut r, "tdx", &seeds, tdx);
    r.check();
}

#[test]
fn sgx_quotes_agree() {
    let seeds: Vec<Vec<u8>> = [
        data("sgx-dcap-qvl/sgx_quote"),
        data("sgx-trustee/occlum_quote.dat"),
    ]
    .into_iter()
    .map(|q| {
        let (_, used) = port::sgx::SgxQuote::parse_prefix(&q).unwrap();
        q[..used].to_vec()
    })
    .collect();
    let mut r = Results::new("sgx_quotes_agree");
    sweep(&mut r, "sgx", &seeds, sgx);
    r.check();
}

#[test]
fn claims_wire_agrees() {
    let wire = |tag: u16, flags: u32| {
        let mut b = vec![0u8; 128];
        b[0..2].copy_from_slice(&tag.to_le_bytes());
        b[2..4].copy_from_slice(&0x0102u16.to_le_bytes());
        b[4..8].copy_from_slice(&flags.to_le_bytes());
        for (i, x) in b[8..].iter_mut().enumerate() {
            *x = i as u8;
        }
        b
    };
    let seeds: Vec<Vec<u8>> = (1..=3).map(|t| wire(t, 7)).collect();
    let mut r = Results::new("claims_wire_agrees");
    sweep(&mut r, "claims wire", &seeds, |r, b| {
        r.add(
            "AegisAttestationClaimsWire::decode",
            port::AegisAttestationClaimsWire::decode(b),
        )
    });
    for (tag, flags) in [(0, 0), (4, 0), (1, 8), (2, 1 << 31)] {
        let b = wire(tag, flags);
        assert!(!r.add(
            "AegisAttestationClaimsWire::decode",
            port::AegisAttestationClaimsWire::decode(&b),
        ));
    }
    r.check();
}

#[test]
fn timestamps_agree() {
    let seeds: Vec<Vec<u8>> = [
        "1970-01-01T00:00:00Z",
        "2000-02-29T12:30:59Z",
        "2024-02-29T23:59:59.123Z",
        "2100-03-01T00:00:00.5Z",
        "9999-12-31T23:59:59Z",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    let mut rng = SplitMix(0x7135);
    let mut r = Results::new("timestamps_agree");
    let mut accepted = 0;
    for seed in &seeds {
        for i in 0..20_000 {
            let mut m = seed.clone();
            match i % 4 {
                0 => {
                    let at = rng.below(m.len() as u64) as usize;
                    m[at] = b"0123456789-T:.Z9"[rng.below(16) as usize];
                }
                1 => {
                    let at = rng.below(m.len() as u64) as usize;
                    m[at] = rng.next() as u8;
                }
                2 => m.truncate(rng.below(m.len() as u64 + 1) as usize),
                _ => {
                    let at = rng.below(m.len() as u64 + 1) as usize;
                    m.insert(at, b"0Z.9"[rng.below(4) as usize]);
                }
            }
            let Ok(s) = std::str::from_utf8(&m) else {
                continue;
            };
            let a = port::time::iso8601_utc_to_unix(s);
            r.add(s, a);
            accepted += usize::from(a.is_some());
        }
    }
    assert!(accepted > 1000);
    r.check();
}

/// One claims/policy pair, built from the draws (a macro, as it was when
/// it built the pair in both crates).
macro_rules! policy_case {
    ($k:ident, $r:expr) => {{
        use $k::claims::{SnpTcbVersion, SupplyChainEvidence, TeeArch};
        let r: &[u64] = $r;
        let arch = |x: u64| {
            [
                TeeArch::IntelTdx15,
                TeeArch::AmdSevSnp,
                TeeArch::ArmCcaRealm,
            ][(x % 3) as usize]
        };
        let byte =
            |x: u64, n: usize| -> Vec<u8> { (0..n).map(|i| ((x >> (i % 8)) & 1) as u8).collect() };
        let arr48 = |x: u64| -> [u8; 48] { byte(x, 48).try_into().unwrap() };
        let arr32 = |x: u64| -> [u8; 32] { byte(x, 32).try_into().unwrap() };
        let tcb = |x: u64| SnpTcbVersion {
            bootloader: (x % 3) as u8,
            tee: ((x >> 2) % 3) as u8,
            snp: ((x >> 4) % 3) as u8,
            microcode: ((x >> 6) % 3) as u8,
        };
        let channel = arr48(r[8]);
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&channel[..8]);
        if r[9] % 5 == 0 {
            prefix[r[9] as usize % 8] ^= 1;
        }
        let claims = $k::AttestationClaims {
            arch: arch(r[0]),
            tcb_svn: (r[1] % 4) as u16,
            enclave_flags: (r[2] % 8) as u32,
            mr_enclave_sha384: arr48(r[3] % 3),
            loom_lock_blake3: arr32(r[4] % 2),
            olean_spec_mif: arr32(r[5] % 2),
            chan_binding_prefix: prefix,
            channel_binding: channel,
            supply_chain_evidence: if r[6] % 2 == 0 {
                SupplyChainEvidence::RegisterReplay
            } else {
                SupplyChainEvidence::ReportDataOnly
            },
            snp_reported_tcb: (r[7] % 3 != 0).then(|| tcb(r[7])),
            snp_vmpl: (r[10] % 3 != 0).then_some((r[10] % 5) as u32),
        };
        let policy = $k::RelyingPartyPolicy {
            allowed_archs: (0..r[11] % 4).map(|i| arch(r[12] >> (2 * i))).collect(),
            tcb_floors: $k::policy::TcbFloors {
                tdx: (r[13] % 4) as u16,
                snp: ((r[13] >> 2) % 4) as u16,
                cca: ((r[13] >> 4) % 4) as u16,
            },
            snp_component_floors: (r[14] % 2 == 0).then(|| tcb(r[14] >> 1)),
            snp_max_vmpl: (r[15] % 4) as u32,
            allowed_measurements: (0..r[16] % 4)
                .map(|i| (arch(r[17] >> (2 * i)), arr48((r[17] >> (8 + 2 * i)) % 3)))
                .collect(),
            expected_loom_blake3: arr32(r[18] % 2),
            expected_olean_mif: arr32(r[19] % 2),
            require_register_replay: r[20] % 2 == 0,
        };
        let expected = if r[21] % 4 == 0 {
            arr48(r[21] >> 2)
        } else {
            channel
        };
        format!(
            "{:?}",
            $k::verify_attestation_claims(&claims, &policy, &expected)
        )
    }};
}

#[test]
fn policy_decisions_agree() {
    let mut rng = SplitMix(0x9011C7);
    let mut outcomes = std::collections::BTreeSet::new();
    let mut results = Results::new("policy_decisions_agree");
    for _ in 0..200_000 {
        let r: Vec<u64> = (0..22).map(|_| rng.next()).collect();
        let a = policy_case!(port, &r);
        results.add("verify_attestation_claims", &a);
        outcomes.insert(
            a.split(['(', ' ', '{'])
                .next()
                .map(String::from)
                .unwrap_or_default(),
        );
        outcomes.insert(a);
    }
    // Every check of the policy is reached and decides at least once.
    for want in [
        "Ok",
        "Err(DisallowedTeeArchitecture",
        "Err(EnclaveDebugModeEnabled)",
        "Err(StaleTcbSvn",
        "Err(StaleSnpTcbComponent",
        "Err(BootMeasurementMismatch)",
        "Err(SupplyChainLoomLockMismatch)",
        "Err(FormalOleanProofDigestMismatch)",
        "Err(ChannelTranscriptBindingMismatch)",
        "Err(SupplyChainNotRegisterMeasured",
        "Err(DisallowedVmpl",
    ] {
        assert!(
            outcomes.iter().any(|o| o.starts_with(want)),
            "never reached {want}"
        );
    }
    results.check();
}

/// Every CBOR sample of the CCA and CoRIM vendor sets, sorted by path.
fn cbor_samples() -> Vec<Vec<u8>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/vendor");
    let mut paths = Vec::new();
    for dir in std::fs::read_dir(&root).unwrap() {
        let dir = dir.unwrap().path();
        if !dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("cca-")
        {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            if f.extension().is_some_and(|e| e == "cbor") {
                paths.push(f);
            }
        }
    }
    paths.sort();
    paths.iter().map(|p| std::fs::read(p).unwrap()).collect()
}

/// CBOR-aware mutations: the byte-level ones, and small values written
/// over a byte (claim keys, short lengths and headers live there).
fn mutate_cbor(rng: &mut SplitMix, seed: &[u8]) -> Vec<u8> {
    if rng.below(3) == 0 && !seed.is_empty() {
        let mut v = seed.to_vec();
        let i = rng.below(v.len() as u64) as usize;
        v[i] = rng.below(0x60) as u8;
        return v;
    }
    mutate(rng, seed)
}

type Sign1Proj = (
    Vec<u8>,
    u8,
    i64,
    Option<String>,
    Option<Vec<u8>>,
    Vec<u8>,
    Vec<u8>,
);

fn sign1_proj(s: &port::cca::CoseSign1) -> Sign1Proj {
    let (ak, av) = match s.alg {
        port::cca::CoseAlg::None => (0, 0),
        port::cca::CoseAlg::Assigned(a) => (1, a),
        port::cca::CoseAlg::Private(a) => (2, a),
        port::cca::CoseAlg::Text => (3, 0),
    };
    (
        s.protected_raw.clone(),
        ak,
        av,
        s.content_type.clone(),
        s.payload.clone(),
        s.signature.clone(),
        s.tbs_data(b"aad"),
    )
}

fn cca_token(r: &mut Results, b: &[u8]) -> bool {
    let res = port::cca::CcaToken::parse(b).map(|t| {
        (
            t.collection,
            t.platform_profile,
            t.realm_profile,
            sign1_proj(&t.platform),
            t.platform_claims,
            sign1_proj(&t.realm),
            t.realm_claims,
        )
    });
    r.add("CcaToken::parse", res)
}

fn corim(r: &mut Results, b: &[u8]) -> bool {
    r.add(
        "Corim::parse_unsigned",
        port::corim::Corim::parse_unsigned(b),
    )
}

fn cca_claims(r: &mut Results, b: &[u8]) -> bool {
    let p = r.add(
        "CcaPlatformClaims::from_cbor",
        port::cca::CcaPlatformClaims::from_cbor(b),
    );
    let q = r.add(
        "CcaRealmClaims::from_cbor",
        port::cca::CcaRealmClaims::from_cbor(b),
    );
    p || q
}

/// A sweep over CBOR seeds (any seed may fail to parse whole, as a signed
/// CoRIM does for `parse_unsigned`).
fn cbor_sweep(
    r: &mut Results,
    seeds: &[Vec<u8>],
    n: usize,
    check: fn(&mut Results, &[u8]) -> bool,
) -> usize {
    let mut rng = SplitMix(0xC0B0_0000 ^ r.name.len() as u64);
    let mut accepted = 0;
    for seed in seeds {
        accepted += usize::from(check(r, seed));
        for _ in 0..n {
            accepted += usize::from(check(r, &mutate_cbor(&mut rng, seed)));
        }
    }
    accepted
}

#[test]
fn cca_tokens_agree() {
    let mut r = Results::new("cca_tokens_agree");
    let accepted = cbor_sweep(&mut r, &cbor_samples(), 2_000, cca_token);
    assert!(accepted > 4, "{accepted} tokens parsed");
    r.check();
}

#[test]
fn corims_agree() {
    let mut r = Results::new("corims_agree");
    let accepted = cbor_sweep(&mut r, &cbor_samples(), 2_000, corim);
    assert!(accepted > 4, "{accepted} CoRIMs parsed");
    r.check();
}

/// Random claim sets on both sides of every limit, encoded by `to_cbor`
/// (which checks nothing), and the vendor tokens' payloads.
#[test]
fn cca_claims_agree() {
    use port::cca::{CcaPlatformClaims, CcaRealmClaims, CcaSwComponent, CcaTbbRotpk};
    use port::claims::RegisterHash;
    let mut r = Results::new("cca_claims_agree");
    let mut payloads = Vec::new();
    for s in cbor_samples() {
        if let Ok(t) = port::cca::CcaToken::parse(&s) {
            payloads.push(t.platform.payload.clone().unwrap());
            payloads.push(t.realm.payload.clone().unwrap());
        }
    }
    let accepted = cbor_sweep(&mut r, &payloads, 4_000, cca_claims);
    assert!(accepted > 100, "{accepted} payloads parsed");
    let mut rng = SplitMix(0xCCA0_C1A1);
    let algos = [
        RegisterHash::Sha256,
        RegisterHash::Sha384,
        RegisterHash::Sha512,
    ];
    let lens = [0u64, 31, 32, 33, 48, 63, 64, 65];
    for _ in 0..20_000 {
        let rng = &mut rng;
        let b = |rng: &mut SplitMix| vec![rng.next() as u8; lens[rng.below(8) as usize] as usize];
        let text = |rng: &mut SplitMix| {
            ["BL", "CM", "DM", "", "sha-256", "x"][rng.below(6) as usize].to_owned()
        };
        let opt = |rng: &mut SplitMix| (rng.below(2) == 0).then(|| text(rng));
        let comps = [0, 1, 2, 63, 64, 65][rng.below(6) as usize];
        let rotpks = [0, 1, 3, 12, 13][rng.below(5) as usize];
        let platform = CcaPlatformClaims {
            challenge: b(rng),
            instance_id: b(rng),
            implementation_id: b(rng),
            lifecycle: [0, 0x3000, u64::MAX][rng.below(3) as usize],
            hash_algo: algos[rng.below(3) as usize],
            sw_components: (0..comps)
                .map(|_| CcaSwComponent {
                    measurement_type: opt(rng),
                    measurement_value: b(rng),
                    version: opt(rng),
                    signer_id: b(rng),
                    measurement_description: opt(rng),
                })
                .collect(),
            config: (rng.below(2) == 0).then(|| b(rng)),
            manufacturing_config: (rng.below(2) == 0).then(|| b(rng)),
            tbb_rotpks: (0..rotpks)
                .map(|_| CcaTbbRotpk {
                    name: text(rng),
                    index: [0, 5, 6, u64::MAX][rng.below(4) as usize],
                    hash: b(rng),
                })
                .collect(),
        }
        .to_cbor();
        cca_claims(&mut r, &platform);
        cca_claims(&mut r, &mutate_cbor(rng, &platform));
        let realm = CcaRealmClaims {
            challenge: [7; 64],
            personalization: [8; 64],
            hash_algo: algos[rng.below(3) as usize],
            public_key: b(rng),
            public_key_hash_algo: algos[rng.below(3) as usize],
            initial_measurement: b(rng),
            extensible_measurements: std::array::from_fn(|_| b(rng)),
        }
        .to_cbor();
        cca_claims(&mut r, &realm);
        cca_claims(&mut r, &mutate_cbor(rng, &realm));
    }
    r.check();
}

fn b64url(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = A.iter().position(|&a| a == c).expect("base64url") as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn cbor_enc(v: &coset::cbor::value::Value) -> Vec<u8> {
    let mut out = Vec::new();
    coset::cbor::ser::into_writer(v, &mut out).unwrap();
    out
}

fn extract_sign1s_and_keys(seeds: &[Vec<u8>]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut sign1s = vec![data("cca-veraison-corim/signed-good-corim.cbor")];
    let mut keys = Vec::new();
    for b in seeds {
        if let Ok(t) = port::cca::CcaToken::parse(b) {
            keys.push(t.realm_claims.public_key.clone());
        }
        // Extract inner byte strings from Tag 399 / CMW 907 collections.
        let mut cur = b.as_slice();
        if let Ok(coset::cbor::value::Value::Tag(tag, inner)) =
            coset::cbor::de::from_reader(&mut cur)
            && let coset::cbor::value::Value::Map(entries) = *inner
        {
            for (_, v) in entries {
                match (tag, v) {
                    (399, coset::cbor::value::Value::Bytes(raw)) => sign1s.push(raw),
                    (907, coset::cbor::value::Value::Array(arr)) if arr.len() == 2 => {
                        if let Some(coset::cbor::value::Value::Bytes(raw)) = arr.get(1) {
                            sign1s.push(raw.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (sign1s, keys)
}

#[test]
fn cose_sign1_agrees() {
    use coset::cbor::value::Value;
    let mut r = Results::new("cose_sign1_agrees");
    let (mut seeds, _) = extract_sign1s_and_keys(&cbor_samples());
    let wrap = |prot: &[u8], unprot: Vec<(Value, Value)>, pay: Value, sig: &[u8]| {
        cbor_enc(&Value::Tag(
            18,
            Box::new(Value::Array(vec![
                Value::Bytes(prot.to_vec()),
                Value::Map(unprot),
                pay,
                Value::Bytes(sig.to_vec()),
            ])),
        ))
    };
    let mut prot_variants = vec![
        vec![],
        cbor_enc(&Value::Map(vec![])),
        cbor_enc(&Value::Map(vec![(1.into(), (-7).into())])),
        cbor_enc(&Value::Map(vec![(1.into(), (-35).into())])),
        cbor_enc(&Value::Map(vec![(1.into(), (-8).into())])),
        cbor_enc(&Value::Map(vec![(1.into(), 10.into())])),
        cbor_enc(&Value::Map(vec![(1.into(), (-70000).into())])),
        cbor_enc(&Value::Map(vec![(1.into(), Value::Text("ES256".into()))])),
        cbor_enc(&Value::Map(vec![(1.into(), 1000.into())])),
        cbor_enc(&Value::Map(vec![
            (1.into(), (-7).into()),
            (3.into(), Value::Text("application/rim+cbor".into())),
            (8.into(), Value::Bytes(b"meta".to_vec())),
        ])),
        cbor_enc(&Value::Map(vec![(3.into(), Value::Text(" a/b".into()))])),
        cbor_enc(&Value::Map(vec![(3.into(), Value::Text("a/b/c".into()))])),
        cbor_enc(&Value::Map(vec![(3.into(), 263.into())])),
        cbor_enc(&Value::Map(vec![(3.into(), Value::Text("".into()))])),
        cbor_enc(&Value::Map(vec![(
            2.into(),
            Value::Array(vec![1.into(), Value::Text("foo".into()), (-70000).into()]),
        )])),
        cbor_enc(&Value::Map(vec![(2.into(), Value::Array(vec![]))])),
        cbor_enc(&Value::Map(vec![(2.into(), Value::Array(vec![99.into()]))])),
        cbor_enc(&Value::Map(vec![
            (1.into(), (-7).into()),
            (1.into(), (-35).into()),
        ])),
        cbor_enc(&Value::Map(vec![
            (5.into(), Value::Bytes(b"iv".to_vec())),
            (6.into(), Value::Bytes(b"piv".to_vec())),
        ])),
        vec![0x81, 0x00],
    ];
    for p in prot_variants.drain(..) {
        seeds.push(wrap(&p, vec![], Value::Bytes(b"hi".to_vec()), b"sig"));
        seeds.push(wrap(&p, vec![], Value::Null, b"sig"));
    }
    seeds.push(wrap(
        &cbor_enc(&Value::Map(vec![(1.into(), (-7).into())])),
        vec![(
            7.into(),
            Value::Array(vec![
                Value::Bytes(vec![]),
                Value::Map(vec![]),
                Value::Bytes(vec![]),
                Value::Bytes(b"cs".to_vec()),
            ]),
        )],
        Value::Bytes(b"ok".to_vec()),
        b"sig",
    ));
    let one = |r: &mut Results, b: &[u8]| -> bool {
        let res = port::cca::CoseSign1::from_tagged_slice(b)
            .ok()
            .map(|s| sign1_proj(&s));
        r.add("CoseSign1::from_tagged_slice", res)
    };
    let accepted = cbor_sweep(&mut r, &seeds, 1_500, one);
    assert!(accepted > 10, "{accepted} COSE_Sign1 values parsed");
    r.check();
}

#[test]
fn cose_keys_agree() {
    use coset::cbor::value::Value;
    let mut r = Results::new("cose_keys_agree");
    let (_, mut seeds) = extract_sign1s_and_keys(&cbor_samples());
    let x = vec![0x11u8; 48];
    let y = vec![0x22u8; 48];
    let mut sec1_ok = vec![0x04];
    sec1_ok.extend_from_slice(&x);
    sec1_ok.extend_from_slice(&y);
    seeds.push(sec1_ok.clone());
    seeds.push(port::cca::encode_realm_cose_key(&sec1_ok));
    let mut sec1_bad = sec1_ok.clone();
    sec1_bad[0] = 0x02;
    seeds.push(sec1_bad);
    seeds.push(vec![0x04; 96]);
    seeds.push(vec![0x04; 98]);
    let mk = |entries: Vec<(Value, Value)>| cbor_enc(&Value::Map(entries));
    seeds.extend([
        mk(vec![
            (1.into(), 2.into()),
            ((-1).into(), 2.into()),
            ((-2).into(), Value::Bytes(x.clone())),
            ((-3).into(), Value::Bytes(y.clone())),
            (2.into(), Value::Bytes(b"k".to_vec())),
            (3.into(), (-35).into()),
            (
                4.into(),
                Value::Array(vec![2.into(), Value::Text("verify".into())]),
            ),
            (5.into(), Value::Bytes(b"iv".to_vec())),
        ]),
        mk(vec![
            (1.into(), 1.into()),
            ((-1).into(), 2.into()),
            ((-2).into(), Value::Bytes(x.clone())),
            ((-3).into(), Value::Bytes(y.clone())),
        ]),
        mk(vec![
            (1.into(), 2.into()),
            ((-1).into(), 1.into()),
            ((-2).into(), Value::Bytes(vec![1; 32])),
            ((-3).into(), Value::Bytes(vec![2; 32])),
        ]),
        mk(vec![
            (1.into(), 2.into()),
            ((-1).into(), 2.into()),
            ((-2).into(), Value::Bytes(vec![1; 47])),
            ((-3).into(), Value::Bytes(y.clone())),
        ]),
        mk(vec![
            (1.into(), 2.into()),
            ((-1).into(), 2.into()),
            (4.into(), Value::Array(vec![2.into(), 2.into()])),
            ((-2).into(), Value::Bytes(x.clone())),
            ((-3).into(), Value::Bytes(y.clone())),
        ]),
        mk(vec![
            (1.into(), 2.into()),
            ((-1).into(), 2.into()),
            ((-1).into(), 2.into()),
            ((-2).into(), Value::Bytes(x)),
            ((-3).into(), Value::Bytes(y)),
        ]),
    ]);
    let one = |r: &mut Results, b: &[u8]| -> bool {
        r.add("realm_public_key", port::cca::realm_public_key(b))
    };
    let accepted = cbor_sweep(&mut r, &seeds, 1_500, one);
    assert!(accepted > 5, "{accepted} keys accepted");
    r.check();
}

#[test]
fn signed_corims_agree() {
    use coset::cbor::value::Value;
    let mut r = Results::new("signed_corims_agree");
    let mut sec1 = vec![4];
    sec1.extend(b64url("MKBCTNIcKUSDii11ySs3526iDZ8AiTo7Tu6KPAqv7D4"));
    sec1.extend(b64url("4Etl6SRW2YiLUrN5vfvVHuhp7x8PxltmWWlbbM4IFyM"));
    let p256_key = port::chain::EcPublicKey::from_sec1(port::chain::Curve::P256, &sec1).unwrap();
    let signed = data("cca-veraison-corim/signed-good-corim.cbor");
    let unsigned = data("cca-veraison-corim/unsigned-good-corim.cbor");

    let wrap = |prot: Value, pay: Value, sig: &[u8]| {
        cbor_enc(&Value::Tag(
            18,
            Box::new(Value::Array(vec![
                Value::Bytes(cbor_enc(&prot)),
                Value::Map(vec![]),
                pay,
                Value::Bytes(sig.to_vec()),
            ])),
        ))
    };
    let mut seeds = vec![signed.clone(), unsigned];
    seeds.extend([
        wrap(
            Value::Map(vec![
                (1.into(), (-7).into()),
                (8.into(), Value::Bytes(b"".to_vec())),
            ]),
            Value::Bytes(b"p".to_vec()),
            &[0u8; 64],
        ),
        wrap(
            Value::Map(vec![
                (1.into(), (-7).into()),
                (3.into(), Value::Text("application/rim+cbor".into())),
            ]),
            Value::Bytes(b"p".to_vec()),
            &[0u8; 64],
        ),
        wrap(
            Value::Map(vec![
                (1.into(), (-7).into()),
                (3.into(), Value::Text("application/rim+cbor".into())),
                (8.into(), Value::Text("not bytes".into())),
            ]),
            Value::Bytes(b"p".to_vec()),
            &[0u8; 64],
        ),
        wrap(
            Value::Map(vec![
                (1.into(), (-7).into()),
                (3.into(), Value::Text("application/rim+cbor".into())),
                (8.into(), Value::Bytes(vec![0xa0])),
            ]),
            Value::Null,
            &[0u8; 64],
        ),
    ]);
    let now_ok = 1_748_736_000 * 1_000_000_000;
    let now_exp = 1_767_225_600 * 1_000_000_000;
    let mut rng = SplitMix(0x516D_ED00);
    let mut accepted = 0usize;
    for seed in &seeds {
        for &now in &[now_ok, now_exp] {
            accepted += usize::from(r.add(
                "Corim::verify_signed",
                port::corim::Corim::verify_signed(seed, &p256_key, now),
            ));
        }
        for _ in 0..1_500 {
            let m = mutate_cbor(&mut rng, seed);
            accepted += usize::from(r.add(
                "Corim::verify_signed",
                port::corim::Corim::verify_signed(&m, &p256_key, now_ok),
            ));
        }
    }
    assert!(accepted >= 1, "{accepted} signed CoRIMs verified");
    r.check();
}

#[test]
fn corim_store_agrees() {
    use port::corim::{
        CCA_PLATFORM_ENDORSEMENT_PROFILE, CCA_REALM_ENDORSEMENT_PROFILE, CcaCorimStore, ClassId,
        Corim, CorimProfile, CorimValidity, CryptoKey, Instance, Mkey, RawValue,
    };
    let mut r = Results::new("corim_store_agrees");
    let now = 1_748_736_000 * 1_000_000_000u64;
    let mut corims: Vec<Corim> = cbor_samples()
        .iter()
        .filter_map(|b| Corim::parse_unsigned(b).ok())
        .collect();
    let plat_ok =
        Corim::parse_unsigned(&data("cca-veraison-services/corim-cca-platform-valid.cbor"))
            .unwrap();
    let realm_ok =
        Corim::parse_unsigned(&data("cca-veraison-services/corim-cca-realm-valid.cbor")).unwrap();
    let mut push = |c: Corim| corims.push(c);

    // Systematic platform mutations.
    {
        let mut c = plat_ok.clone();
        c.validity = Some(CorimValidity {
            not_before: Some(2_000_000_000),
            not_after: 2_500_000_000,
        });
        push(c);
    }
    let mut c = plat_ok.clone();
    c.profile = None;
    push(c);
    let mut c = plat_ok.clone();
    c.profile = Some(CorimProfile::Uri("other".into()));
    push(c);
    let mut c = plat_ok.clone();
    c.profile = Some(CorimProfile::Oid(vec![1, 2]));
    push(c);

    for f in [
        (|c: &mut Corim| c.comids[0].reference_values[0].environment.has_group = true)
            as fn(&mut Corim),
        |c| c.comids[0].reference_values[0].environment.instance = Some(Instance::Bytes(vec![1])),
        |c| c.comids[0].reference_values[0].environment.has_class = false,
        |c| c.comids[0].reference_values[0].environment.class_id = None,
        |c| c.comids[0].reference_values[0].environment.class_id = Some(ClassId::Uuid([1; 16])),
        |c| {
            c.comids[0].reference_values[0].environment.class_id = Some(ClassId::Bytes(vec![0; 31]))
        },
        |c| c.comids[0].reference_values[0].measurements[0].has_authorized_by = true,
        |c| c.comids[0].reference_values[0].measurements[0].mkey = None,
        |c| c.comids[0].reference_values[0].measurements[0].mkey = Some(Mkey::Uint(1)),
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .other_keys = vec![99]
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .raw_value = Some(RawValue {
                value: vec![1],
                mask: Some(vec![1]),
            })
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests
                .clear();
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests[0]
                .alg = 99;
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests[0]
                .value
                .pop();
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .version_has_scheme = true;
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .cryptokeys
                .clear();
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .cryptokeys = vec![CryptoKey::Bytes(vec![1; 31])];
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .cryptokeys = vec![CryptoKey::PkixBase64Key("x".into())];
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .cryptokeys = vec![CryptoKey::Bytes(vec![1; 32]), CryptoKey::Bytes(vec![2; 32])];
        },
        |c| {
            let mut m = c.comids[0].reference_values[0].measurements[4].clone();
            m.values.raw_value = None;
            c.comids[0].reference_values[0].measurements[4] = m;
        },
        |c| {
            c.comids[0].reference_values[0].measurements[4]
                .values
                .raw_value = Some(RawValue {
                value: vec![1, 2],
                mask: None,
            });
        },
        |c| {
            c.comids[0].reference_values[0].measurements[4]
                .values
                .raw_value = Some(RawValue {
                value: vec![1, 2],
                mask: Some(vec![1]),
            });
        },
        |c| {
            let m = c.comids[0].reference_values[0].measurements[4].clone();
            c.comids[0].reference_values[0].measurements.push(m);
        },
        |c| {
            c.comids[0].reference_values[0].measurements[6].mkey =
                Some(Mkey::Text("cca.rotpk.CM.6".into()));
        },
        |c| {
            c.comids[0].reference_values[0].measurements[6]
                .values
                .cryptokeys
                .clear();
        },
        |c| {
            c.comids[0].reference_values[0].measurements[6]
                .values
                .cryptokeys = vec![CryptoKey::Bytes(vec![1; 5])];
        },
        |c| {
            c.comids[0].reference_values[0].measurements.retain(
                |m| !matches!(&m.mkey, Some(Mkey::Text(s)) if s == "cca.software-component"),
            );
        },
        |c| c.comids[1].attest_keys[0].has_conditions = true,
        |c| c.comids[1].attest_keys[0].environment.has_group = true,
        |c| c.comids[1].attest_keys[0].environment.instance = None,
        |c| c.comids[1].attest_keys[0].environment.instance = Some(Instance::Uuid([1; 16])),
        |c| c.comids[1].attest_keys[0].environment.instance = Some(Instance::Ueid(vec![2; 33])),
        |c| c.comids[1].attest_keys[0].environment.instance = Some(Instance::Ueid(vec![1; 32])),
        |c| c.comids[1].attest_keys[0].keys = vec![CryptoKey::Bytes(vec![1; 32])],
        |c| c.comids[1].attest_keys[0].keys = vec![],
        |c| {
            c.comids[1].attest_keys[0].keys = vec![CryptoKey::PkixBase64Key("bad pem".into())];
        },
    ] {
        let mut c = plat_ok.clone();
        f(&mut c);
        push(c);
    }

    // Systematic realm mutations.
    let mut realm_ak = realm_ok.clone();
    realm_ak.comids[0].attest_keys = plat_ok.comids[1].attest_keys.clone();
    push(realm_ak);
    for f in [
        (|c: &mut Corim| c.comids[0].reference_values[0].environment.has_group = true)
            as fn(&mut Corim),
        |c| c.comids[0].reference_values[0].environment.has_class = false,
        |c| c.comids[0].reference_values[0].environment.class_id = None,
        |c| c.comids[0].reference_values[0].environment.class_id = Some(ClassId::Uuid([1; 16])),
        |c| {
            c.comids[0].reference_values[0].environment.class_id = Some(ClassId::Bytes(vec![1; 31]))
        },
        |c| c.comids[0].reference_values[0].measurements[0].mkey = None,
        |c| c.comids[0].reference_values[0].measurements[0].mkey = Some(Mkey::Uint(0)),
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests
                .clear()
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests[0]
                .alg = 99
        },
        |c| {
            let d = c.comids[0].reference_values[0].measurements[0]
                .values
                .digests[0]
                .clone();
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests
                .push(d);
        },
        |c| {
            c.comids[0].reference_values[0].measurements[0]
                .values
                .digests[0]
                .value[0] ^= 1;
        },
        |c| {
            let m = c.comids[0].reference_values[0].measurements[0].clone();
            c.comids[0].reference_values[0].measurements.push(m);
        },
        |c| {
            let m = c.comids[0].reference_values[0].measurements[1].clone();
            c.comids[0].reference_values[0].measurements.push(m);
        },
        |c| {
            c.comids[0].reference_values[0]
                .measurements
                .retain(|m| !matches!(&m.mkey, Some(Mkey::Text(s)) if s == "cca.rim"));
        },
        |c| {
            if let Some(rpv) = c.comids[0].reference_values[0]
                .measurements
                .iter_mut()
                .find(|m| matches!(&m.mkey, Some(Mkey::Text(s)) if s == "cca.rpv"))
            {
                rpv.values.raw_value.as_mut().unwrap().mask = Some(vec![0xff; 64]);
            }
        },
        |c| {
            if let Some(rpv) = c.comids[0].reference_values[0]
                .measurements
                .iter_mut()
                .find(|m| matches!(&m.mkey, Some(Mkey::Text(s)) if s == "cca.rpv"))
            {
                rpv.values.raw_value.as_mut().unwrap().value = vec![1; 65];
            }
        },
    ] {
        let mut c = realm_ok.clone();
        assert_eq!(c.profile_uri(), Some(CCA_REALM_ENDORSEMENT_PROFILE));
        f(&mut c);
        push(c);
    }
    assert_eq!(
        plat_ok.profile_uri(),
        Some(CCA_PLATFORM_ENDORSEMENT_PROFILE)
    );

    for c in &corims {
        let mut s = CcaCorimStore::new();
        let res = s.add(c, now).map(|()| s);
        r.add("CcaCorimStore::add", res);
    }
    r.check();
}

#[test]
fn cca_appraisal_agrees() {
    use port::corim::{
        CcaAppraisalOptions, CcaCorimStore, Corim, Digest, NI_SHA256, appraise_cca_with_options,
    };
    let mut r = Results::new("cca_appraisal_agrees");
    let now = 1_748_736_000 * 1_000_000_000u64;
    let mut store = CcaCorimStore::new();
    for p in [
        "cca-veraison-services/corim-cca-platform-valid.cbor",
        "cca-veraison-services/corim-cca-realm-valid.cbor",
    ] {
        store
            .add(&Corim::parse_unsigned(&data(p)).unwrap(), now)
            .unwrap();
    }
    let verified = port::cca::verify_cca_endorsed(
        &data("cca-veraison-services/cca-good.cbor"),
        &store.endorsements,
    )
    .unwrap();

    type CaseFn = Box<dyn FnMut(&mut port::cca::VerifiedCcaToken, &mut CcaCorimStore)>;
    for cmp_signer in [true, false] {
        let opts = CcaAppraisalOptions {
            compare_signer_id: cmp_signer,
        };
        let mut cases: Vec<CaseFn> = vec![
            Box::new(|_, _| {}),
            Box::new(|_, s| s.platform.clear()),
            Box::new(|v, _| v.token.platform_claims.sw_components.clear()),
            Box::new(|v, _| {
                v.token.platform_claims.sw_components[0].measurement_description =
                    Some("md5".into());
            }),
            Box::new(|_, s| {
                s.platform[0].sw_components[0].digests[0].alg = NI_SHA256;
            }),
            Box::new(|v, _| v.token.platform_claims.sw_components[0].measurement_type = None),
            Box::new(|v, _| v.token.platform_claims.sw_components[0].version = None),
            Box::new(|_, s| {
                let mut alt = s.platform[0].clone();
                alt.sw_components[0].digests[0].value[0] ^= 1;
                s.platform.insert(0, alt);
            }),
            Box::new(|v, _| v.token.platform_claims.config = None),
            Box::new(|v, _| v.token.platform_claims.config.as_mut().unwrap()[0] ^= 1),
            Box::new(|v, _| v.token.platform_claims.manufacturing_config = None),
            Box::new(|v, _| {
                v.token
                    .platform_claims
                    .manufacturing_config
                    .as_mut()
                    .unwrap()[0] ^= 1;
            }),
            Box::new(|v, _| v.token.platform_claims.tbb_rotpks.clear()),
            Box::new(|v, _| v.token.platform_claims.tbb_rotpks[0].index = 0),
            Box::new(|v, _| v.token.platform_claims.tbb_rotpks[0].hash[0] ^= 1),
            Box::new(|_, s| s.realm.clear()),
            Box::new(|v, _| v.token.realm_claims.personalization[0] ^= 1),
            Box::new(|v, _| v.token.realm_claims.extensible_measurements[0][0] ^= 1),
            Box::new(|v, _| v.token.realm_claims.extensible_measurements[3][0] ^= 1),
            Box::new(|v, s| {
                let mut padded = s.realm[0].rim.clone();
                padded.resize(64, 0);
                v.token.realm_claims.initial_measurement = padded;
                let mut alt = s.realm[0].clone();
                alt.rems[2].as_mut().unwrap()[0] ^= 1;
                s.realm.insert(0, alt);
            }),
            Box::new(|v, s| {
                let mut padded = s.realm[0].rim.clone();
                padded.resize(64, 0);
                padded[63] = 1;
                v.token.realm_claims.initial_measurement = padded;
            }),
            Box::new(|_, s| {
                s.platform[0].sw_components[0].digests = vec![Digest {
                    alg: 99,
                    value: vec![0; 32],
                }];
            }),
        ];
        for tweak in &mut cases {
            let mut v = verified.clone();
            let mut s = store.clone();
            tweak(&mut v, &mut s);
            r.add(
                "appraise_cca_with_options",
                appraise_cca_with_options(&v, &s, &opts),
            );
        }
    }
    r.check();
}

/// The CCA crypto paths (SH6 lane 8 part 4a): the Vesper report data, the
/// register extends, the RIM measurement and the RAK hash on random inputs
/// of every algorithm; fixed-width ECDSA on test keys (valid, and with the
/// message, signature or key changed); and whole CCA tokens through
/// `verify_cca_endorsed`, `verify_cca_token` and `cca_vesper_claims`,
/// valid and with 2,000 byte mutations each. Keys and signatures are fresh
/// per run, so only results that do not show them are folded in.
#[test]
fn cca_crypto_agrees() {
    use port::cca::{
        CcaEndorsement, cca_rim_measurement, cca_vesper_claims, rak_hash, verify_cca_endorsed,
        verify_cca_token,
    };
    use port::claims::{GuestRuntimeClaims, RegisterHash, single_extend};
    use port::{Curve, EcPublicKey};
    let mut r = Results::new("cca_crypto_agrees");
    let mut rng = SplitMix(0xCC_A4A);
    let bytes =
        |rng: &mut SplitMix, n: usize| -> Vec<u8> { (0..n).map(|_| rng.next() as u8).collect() };
    let algs = [
        RegisterHash::Sha256,
        RegisterHash::Sha384,
        RegisterHash::Sha512,
    ];

    for _ in 0..2_000 {
        let rt = GuestRuntimeClaims {
            channel_binding: bytes(&mut rng, 48).try_into().unwrap(),
            loom_lock_blake3: bytes(&mut rng, 32).try_into().unwrap(),
            olean_spec_mif: bytes(&mut rng, 32).try_into().unwrap(),
        };
        r.add("report_data", rt.report_data());
    }
    for alg in algs {
        for pad in [0usize, 32, 48, 64, 100] {
            for _ in 0..200 {
                let d: [u8; 32] = bytes(&mut rng, 32).try_into().unwrap();
                r.add("single_extend", single_extend(alg, pad, &d));
            }
        }
        for _ in 0..500 {
            let n = rng.below(130) as usize;
            let rim = bytes(&mut rng, n);
            r.add("cca_rim_measurement", cca_rim_measurement(alg, &rim));
            r.add("rak_hash", rak_hash(alg, &rim));
        }
    }

    for curve in [Curve::P256, Curve::P384] {
        let signer = support::Signer::generate(curve);
        let other = support::Signer::generate(curve);
        let key = EcPublicKey::from_sec1(curve, &signer.sec1()).unwrap();
        let wrong = EcPublicKey::from_sec1(curve, &other.sec1()).unwrap();
        let mut bad_point = signer.sec1();
        bad_point[5] ^= 1;
        let bad_point = EcPublicKey {
            curve,
            sec1: bad_point,
        };
        for _ in 0..300 {
            let n = rng.below(300) as usize;
            let msg = bytes(&mut rng, n);
            let sig = signer.sign(&msg);
            r.add("verify_fixed ok", key.verify_fixed(&msg, &sig, "t"));
            r.add("verify_fixed key", wrong.verify_fixed(&msg, &sig, "t"));
            r.add(
                "verify_fixed point",
                bad_point.verify_fixed(&msg, &sig, "t"),
            );
            let mut m2 = msg.clone();
            m2.push(1);
            r.add("verify_fixed msg", key.verify_fixed(&m2, &sig, "t"));
            let mut s2 = sig.clone();
            let i = rng.below(s2.len() as u64) as usize;
            s2[i] ^= 1 << rng.below(8);
            r.add("verify_fixed sig", key.verify_fixed(&msg, &s2, "t"));
            r.add("verify_fixed short", key.verify_fixed(&msg, &sig[1..], "t"));
        }
    }

    // The tokens and chains are fixtures (tests/data/cca-crypto), written
    // once by `write_cca_crypto_fixtures`: fresh ECDSA signatures would make
    // a mutation that rewrites a signature byte with its own value succeed
    // in one run and fail in the next.
    for (name, realm_hash) in [
        ("p384-sha256", RegisterHash::Sha256),
        ("p256-sha384", RegisterHash::Sha384),
        ("p384-sha512", RegisterHash::Sha512),
    ] {
        let f = |ext: &str| data(&format!("../cca-crypto/{name}.{ext}"));
        let token = f("token");
        let iak_curve = if name.starts_with("p256") {
            Curve::P256
        } else {
            Curve::P384
        };
        let root = port::TrustRoot::from_der(f("root")).unwrap();
        let der_chain = vec![f("leaf"), f("intermediate")];
        let runtime = support::CcaParams {
            realm_hash,
            ..Default::default()
        }
        .runtime;
        let endorsement = CcaEndorsement {
            instance_id: vec![1; 33],
            implementation_id: Some(vec![2; 32]),
            cpak: EcPublicKey::from_sec1(iak_curve, &f("cpak")).unwrap(),
        };
        let mut other_rt = runtime;
        other_rt.olean_spec_mif[0] ^= 1;
        let mut cases = vec![token.clone()];
        for _ in 0..2_000 {
            cases.push(mutate_cbor(&mut rng, &token));
        }
        for t in &cases {
            let endorsed = verify_cca_endorsed(t, std::slice::from_ref(&endorsement));
            r.add("verify_cca_endorsed", endorsed.as_ref().map(|_| ()));
            if let Ok(v) = &endorsed {
                r.add("cca_vesper_claims", cca_vesper_claims(v, &runtime));
                r.add("cca_vesper_claims other", cca_vesper_claims(v, &other_rt));
            }
            r.add(
                "verify_cca_token",
                verify_cca_token(t, &der_chain, &root, support::NOW_NS, &runtime),
            );
        }
    }
    r.check();
}

/// Writes the CCA token and chain fixtures of `cca_crypto_agrees`.
#[test]
#[ignore = "writes tests/data/cca-crypto; run once by hand"]
fn write_cca_crypto_fixtures() {
    use port::Curve;
    use port::claims::RegisterHash;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/cca-crypto");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, iak_curve, realm_hash) in [
        ("p384-sha256", Curve::P384, RegisterHash::Sha256),
        ("p256-sha384", Curve::P256, RegisterHash::Sha384),
        ("p384-sha512", Curve::P384, RegisterHash::Sha512),
    ] {
        let chain = support::chain(Curve::P384, iak_curve, false);
        let rak = support::Signer::generate(Curve::P384);
        let p = support::CcaParams {
            realm_hash,
            ..Default::default()
        };
        let write =
            |ext: &str, b: &[u8]| std::fs::write(dir.join(format!("{name}.{ext}")), b).unwrap();
        write("token", &support::cca_token(&chain.leaf, &rak, &p));
        write("root", &chain.root_der);
        write("leaf", &chain.leaf_der);
        write("intermediate", &chain.intermediate_der);
        write("cpak", &chain.leaf.sec1());
    }
}
