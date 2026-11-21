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
