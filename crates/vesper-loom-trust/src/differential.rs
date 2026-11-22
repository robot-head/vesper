//! The Vesper port against the Rust it replaced (`src/reference/`): random
//! attestation bundles, policies, statements, catalogs and strings, built
//! to reach every rule, must give equal results, and each test asserts
//! that every diagnostic code or finding was reached.

use std::collections::BTreeMap;

use ed25519_dalek::{Signer, SigningKey};
use vesper_loom_calculus::PackageName;
use vesper_loom_cas::{DerivationInputs, Digest, Domain, ObjectId, ObjectKind};

use crate::reference;
use crate::slopsquat::{self, CatalogEntry, QuarantinePolicy};
use crate::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    /// A string of `0..max` characters of `alphabet`.
    fn text(&mut self, alphabet: &[char], max: usize) -> String {
        let n = self.below(max + 1);
        (0..n).map(|_| *self.pick(alphabet)).collect()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Counts of the reference's results, so a test can assert coverage.
#[derive(Default)]
struct Tally(BTreeMap<String, u64>);

impl Tally {
    fn see(&mut self, what: impl Into<String>) {
        *self.0.entry(what.into()).or_default() += 1;
    }
    fn require(&self, all: &[&str]) {
        let missing: Vec<_> = all.iter().filter(|w| !self.0.contains_key(**w)).collect();
        assert!(
            missing.is_empty(),
            "never reached: {missing:?} (seen {:?})",
            self.0
        );
    }
}

fn source_tree(d: u8) -> String {
    ObjectId::new(Domain::Source, ObjectKind::Tree, Digest([d; 32])).to_string()
}

fn base_identity() -> BuildIdentity {
    let source = ObjectId::new(Domain::Source, ObjectKind::Tree, Digest([4; 32]));
    let derivation = DerivationInputs::new(Domain::Artifact, "test-builder", "implementation-1")
        .with_target("x86_64-unknown-linux-gnu")
        .with_input("source", source)
        .key()
        .unwrap();
    BuildIdentity {
        package: PackageName::parse("@test/package").unwrap(),
        version: "1.2.3+source".into(),
        source_tree: source.to_string(),
        derivation: derivation.to_string(),
        target: "x86_64-unknown-linux-gnu".into(),
        builder_blake3: "01".repeat(32),
        sandbox_contract: "vesper:sandbox/step@0.1.0".into(),
        output: ObjectId::new(Domain::Artifact, ObjectKind::Blob, Digest([7; 32])).to_string(),
    }
}

const VERSIONS: &[&str] = &[
    "1.2.3",
    "1.2.3+source",
    "0.0.0",
    "01.2.3",
    "1.02.3",
    "1.2.03",
    "1.2",
    "1.2.3.4",
    "1.2.3-",
    "1.2.3+",
    "1.2.3-+b",
    "1.2.3-alpha.01",
    "1.2.3-alpha.0",
    "1.2.3-0a",
    "1.2.3-00",
    "1.2.3+01",
    "1.2.3-a..b",
    "1.2.3-.a",
    "1.2.3-a.",
    " 1.2.3",
    "1.2.3 ",
    "v1.2.3",
    "",
    "1.2.3-x-y.z+b.c-d",
    "1.2.3+b+c",
    "1.2.3-rc.1+build.5",
    "18446744073709551615.0.0",
    "18446744073709551616.0.0",
    "99999999999999999999.0.0",
    "1.2.3-\u{e9}",
    "1.2.3-α",
    "0.0.0-0",
    "1..3",
    ".1.2",
];

const SEMVER_CHARS: &[char] = &[
    '0', '1', '9', '.', '.', '-', '+', 'a', 'Z', '0', '.', '1', ' ', '\u{e9}',
];

fn version(rng: &mut Rng) -> String {
    if rng.chance(30) {
        rng.text(SEMVER_CHARS, 12)
    } else {
        rng.pick(VERSIONS).to_string()
    }
}

fn object_text(rng: &mut Rng) -> String {
    let d = Digest([rng.next() as u8; 32]);
    let ok = |dom, kind| ObjectId::new(dom, kind, d).to_string();
    match rng.below(14) {
        0 => ok(Domain::Source, ObjectKind::Tree),
        1 => ok(Domain::Source, ObjectKind::Blob),
        2 => ok(Domain::Artifact, ObjectKind::Tree),
        3 => ok(Domain::Artifact, ObjectKind::Blob),
        4 => ok(Domain::Interface, ObjectKind::Blob),
        5 => ok(Domain::Source, ObjectKind::Tree).replace("b3v1", "b3v2"),
        6 => ok(Domain::Source, ObjectKind::Tree).to_uppercase(),
        7 => format!("b3v1:source:tree:{}", "a".repeat(63)),
        8 => format!("b3v1:source:tree:{}:x", "a".repeat(64)),
        9 => format!("b3v1:source:{}", "a".repeat(64)),
        10 => format!("b3v1:source:tree:{}", "g".repeat(64)),
        11 => format!("b3v1:Source:tree:{}", "a".repeat(64)),
        12 => "x".repeat(129),
        _ => String::new(),
    }
}

fn derivation_text(rng: &mut Rng, good: &str) -> String {
    match rng.below(7) {
        0 | 1 => good.to_owned(),
        2 => good.to_uppercase(),
        3 => good.trim_start_matches("drv:").to_owned(),
        4 => format!("drv:b3v1:{}", "0".repeat(63)),
        5 => format!("drv:b3v1:{}", "f".repeat(64)),
        _ => format!("drv:b3v1:{}x", "0".repeat(64)),
    }
}

const LABEL_CHARS: &[char] = &[
    'a', 'Z', '0', '_', '-', '.', ':', '/', '@', ' ', '!', '\u{e9}', '"', '\\',
];

fn label_text(rng: &mut Rng, good: &str) -> String {
    match rng.below(6) {
        0..=2 => good.to_owned(),
        3 => String::new(),
        4 => "k".repeat(256 + rng.below(2)),
        _ => rng.text(LABEL_CHARS, 8),
    }
}

/// A build identity with each field, at random, changed to one of the
/// values that exercise the identity rules.
fn identity(rng: &mut Rng, base: &BuildIdentity) -> BuildIdentity {
    let mut i = base.clone();
    if rng.chance(70) {
        return i;
    }
    match rng.below(9) {
        0 => {
            i.package = PackageName::parse(match rng.below(3) {
                0 => "@other/package".to_owned(),
                1 => format!("@a/{}", "b".repeat(254 + rng.below(4))),
                _ => "std/core".to_owned(),
            })
            .unwrap()
        }
        1 => i.version = version(rng),
        2 => i.source_tree = object_text(rng),
        3 => i.derivation = derivation_text(rng, &base.derivation),
        4 => i.output = object_text(rng),
        5 => {
            i.builder_blake3 = match rng.below(4) {
                0 => "AB".repeat(32),
                1 => "a".repeat(63),
                2 => "a".repeat(66),
                _ => "09".repeat(32),
            }
        }
        6 => i.target = label_text(rng, &base.target),
        7 => i.sandbox_contract = label_text(rng, &base.sandbox_contract),
        _ => i.version = "9".repeat(257),
    }
    i
}

const NOWS: &[u64] = &[0, 9, 10, 99, 100, 150, 200, 299, 300, 999, 1000, u64::MAX];

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A random policy over builders with seeds 1..=5 and its signing keys.
fn policy(rng: &mut Rng) -> (BuilderPolicy, Vec<(String, u8)>) {
    let n = if rng.chance(3) { 0 } else { 1 + rng.below(5) };
    let mut builders = Vec::new();
    let mut keys = Vec::new();
    for k in 0..n {
        let seed = if rng.chance(8) { 1 } else { k as u8 + 1 };
        let id = if rng.chance(5) {
            "key-0".to_owned()
        } else {
            format!("key-{k}")
        };
        let mut b = TrustedBuilder {
            key_id: id.clone(),
            operator_id: format!("operator-{}", if rng.chance(25) { 0 } else { k }),
            public_key: hex(&signing_key(seed).verifying_key().to_bytes()),
            valid_from: 10,
            valid_until: 1000,
            revoked: rng.chance(5),
        };
        if rng.chance(15) {
            match rng.below(9) {
                0 => b.key_id = label_text(rng, &b.key_id),
                1 => b.operator_id = label_text(rng, &b.operator_id),
                2 => b.valid_from = b.valid_until,
                3 => b.public_key = b.public_key.to_uppercase(),
                4 => b.public_key = "zz".repeat(32),
                // The identity: a valid, small-order point.
                5 => b.public_key = format!("01{}", "00".repeat(31)),
                // y = 2 is not on the curve.
                6 => b.public_key = format!("02{}", "00".repeat(31)),
                7 => b.valid_until = 150,
                _ => b.valid_from = 120,
            }
        }
        keys.push((id, seed));
        builders.push(b);
    }
    let policy = BuilderPolicy {
        schema_version: if rng.chance(3) { 2 } else { SCHEMA_VERSION },
        minimum_operators: *rng.pick(&[2, 2, 2, 3, 0, 1, 64, 65]),
        max_statement_ttl: *rng.pick(&[500, 500, 200, 100, 0]),
        builders,
    };
    if rng.chance(2) {
        let mut p = policy;
        p.builders = (0..65)
            .map(|_| p.builders.first().cloned().unwrap_or(sample_builder()))
            .collect();
        return (p, keys);
    }
    (policy, keys)
}

fn sample_builder() -> TrustedBuilder {
    TrustedBuilder {
        key_id: "key-x".into(),
        operator_id: "operator-x".into(),
        public_key: hex(&signing_key(9).verifying_key().to_bytes()),
        valid_from: 10,
        valid_until: 1000,
        revoked: false,
    }
}

fn statement(rng: &mut Rng, key_id: &str, base: &BuildIdentity) -> Statement {
    let mut s = Statement {
        schema_version: SCHEMA_VERSION,
        key_id: key_id.to_owned(),
        identity: identity(rng, base),
        issued_at: 100,
        expires_at: 300,
    };
    if rng.chance(20) {
        match rng.below(7) {
            0 => s.schema_version = 2,
            1 => s.key_id = label_text(rng, key_id),
            2 => s.expires_at = s.issued_at,
            3 => s.issued_at = 5,
            4 => s.expires_at = 1200,
            5 => s.issued_at = 160,
            _ => s.expires_at = 140,
        }
    }
    s
}

/// A bundle of envelopes for the policy's keys, signed with the reference
/// message where it has one, then perturbed.
fn bundle(rng: &mut Rng, keys: &[(String, u8)], base: &BuildIdentity) -> AttestationBundle {
    let mut attestations = Vec::new();
    for (id, seed) in keys {
        if rng.chance(10) {
            continue;
        }
        let id = if rng.chance(5) {
            "key-unknown".to_owned()
        } else {
            id.clone()
        };
        let st = statement(rng, &id, base);
        let message = reference::verify::canonical_signing_bytes(&st).unwrap_or_default();
        let signer = if rng.chance(4) {
            signing_key(seed + 1)
        } else {
            signing_key(*seed)
        };
        let mut signature = hex(&signer.sign(&message).to_bytes());
        if rng.chance(6) {
            signature = match rng.below(3) {
                0 => signature.to_uppercase(),
                1 => signature[..126].to_owned(),
                _ => format!("{}{}", &signature[..2], "00".repeat(63)),
            };
        }
        let mut a = Attestation {
            statement: st,
            signature,
        };
        // A statement changed after signing.
        if rng.chance(4) {
            a.statement.issued_at = 101;
        }
        if rng.chance(5) {
            attestations.push(a.clone());
        }
        attestations.push(a);
    }
    if rng.chance(2) {
        let first = attestations.first().cloned();
        if let Some(a) = first {
            attestations.extend(std::iter::repeat_n(a, 129));
        }
    }
    AttestationBundle {
        schema_version: if rng.chance(3) { 2 } else { SCHEMA_VERSION },
        attestations,
    }
}

const CODES: &[&str] = &[
    "invalid_expected",
    "unsupported_schema",
    "invalid_policy",
    "limit_exceeded",
    "invalid_public_key",
    "duplicate_public_key",
    "duplicate_key_id",
    "invalid_statement",
    "duplicate_attestation",
    "unknown_key",
    "identity_mismatch",
    "revoked_key",
    "key_expired_or_not_yet_valid",
    "statement_expired_or_future",
    "statement_ttl_exceeded",
    "invalid_signature",
    "insufficient_quorum",
    "accepted",
];

#[test]
fn verification_matches_the_rust_verifier() {
    let mut rng = Rng(0x10_0a11);
    let base = base_identity();
    let mut tally = Tally::default();
    for case in 0..4000 {
        let expected = if rng.chance(85) {
            base.clone()
        } else {
            identity(&mut rng, &base)
        };
        let (policy, keys) = policy(&mut rng);
        let bundle = bundle(&mut rng, &keys, &base);
        let now = if rng.chance(80) { 150 } else { *rng.pick(NOWS) };
        let want = reference::verify::verify_attestations(&expected, &bundle, &policy, now);
        let got = verify_attestations(&expected, &bundle, &policy, now);
        assert_eq!(
            got, want,
            "case {case}: {expected:?} {policy:?} {bundle:?} at {now}"
        );
        for d in &want.diagnostics {
            tally.see(d.code.clone());
            tally.see(d.message.clone());
        }
        if want.accepted {
            tally.see("accepted");
        }
    }
    tally.require(CODES);
}

/// The identity and signing-message errors, each of which the statement
/// test must reach.
const STATEMENT_ERRORS: &[&str] = &[
    "package identity is too long",
    "build identity field exceeds its size limit",
    "invalid exact SemVer version",
    "invalid source object identity",
    "source_tree must be a source-domain tree object",
    "invalid derivation identity",
    "invalid output object identity",
    "output must be an artifact-domain object",
    "builder_blake3 must be a lowercase BLAKE3 digest",
    "invalid target or sandbox contract identity",
    "unsupported statement schema",
    "invalid statement key ID",
    "statement expiry must follow issuance",
    "ok",
];

#[test]
fn signing_messages_match_the_rust_encoder() {
    let mut rng = Rng(0x5167);
    let base = base_identity();
    let mut tally = Tally::default();
    for case in 0..20000 {
        let mut st = statement(&mut rng, "key-a", &base);
        if rng.chance(50) {
            st.identity = identity(&mut rng, &base);
        }
        if rng.chance(5) {
            st.issued_at = rng.next();
            st.expires_at = rng.next();
        }
        let want = reference::verify::canonical_signing_bytes(&st);
        let got = canonical_signing_bytes(&st);
        assert_eq!(got, want, "case {case}: {st:?}");
        match &want {
            Ok(_) => tally.see("ok"),
            Err(e) => tally.see(e.clone()),
        }
    }
    tally.require(STATEMENT_ERRORS);
}

#[test]
fn semver_versions_match_the_semver_crate() {
    // Through the identity rule: a version is accepted exactly when the
    // semver crate parses it (and it is then canonical).
    let mut rng = Rng(0x5e5e);
    let base = base_identity();
    let mut accepted = 0;
    for case in 0..20000 {
        let v = if rng.chance(50) {
            rng.text(SEMVER_CHARS, 14)
        } else {
            version(&mut rng)
        };
        let st = Statement {
            schema_version: SCHEMA_VERSION,
            key_id: "k".into(),
            identity: BuildIdentity {
                version: v.clone(),
                ..base.clone()
            },
            issued_at: 1,
            expires_at: 2,
        };
        let parses = semver::Version::parse(&v).is_ok_and(|p| p.to_string() == v) && v.len() <= 256;
        let got = canonical_signing_bytes(&st);
        assert_eq!(got.is_ok(), parses, "case {case}: {v:?} {got:?}");
        assert_eq!(
            got,
            reference::verify::canonical_signing_bytes(&st),
            "case {case}: {v:?}"
        );
        accepted += usize::from(parses);
    }
    assert!(accepted > 1000, "{accepted}");
}

const ORGS: &[&str] = &[
    "vesper-core",
    "vesper_core",
    "vesper-c0re",
    "vesperc0re",
    "vespercore",
    "s7d",
    "std-x",
    "stdlib",
    "std-official",
    "official-std",
    "vesper-std",
    "vesperstd",
    "acme",
    "acme1",
    "acne",
    "a",
];

const PKGS: &[&str] = &[
    "bytes",
    "byets",
    "bytes-fast",
    "byte5",
    "bytes_",
    "lz4-pure",
    "lz4pure",
    "x",
    "y",
    "risky",
    "r1sky",
];

fn package(rng: &mut Rng) -> PackageName {
    let text = match rng.below(20) {
        0 => format!("std/{}", rng.pick(PKGS)),
        1 => format!("@a/{}", "b".repeat(250 + rng.below(12))),
        _ => format!("@{}/{}", rng.pick(ORGS), rng.pick(PKGS)),
    };
    PackageName::parse(text).unwrap()
}

const REASONS: &[&str] = &[
    "MissingFromCatalog",
    "OversizedPackageName",
    "EditDistanceTyposquat",
    "IdentifierConflation",
    "StdNamespaceImpersonation",
    "ImmatureNamespace",
    "ExcessiveTrustCost",
    "MissingOidcProvenance",
    "Admit",
    "Quarantined",
    "Rejected",
];

#[test]
fn admission_screens_match_the_rust_screen() {
    let mut rng = Rng(0x51_0b5);
    let mut tally = Tally::default();
    for case in 0..6000 {
        let catalog: Vec<CatalogEntry> = (0..rng.below(12))
            .map(|_| CatalogEntry {
                package: package(&mut rng),
                org_created_at: *rng.pick(&[0, 1_000, 2_000_000, 10_000_000, u64::MAX]),
                trust_cost: *rng.pick(&[0, 5, 50, 51, 90, u32::MAX]),
                attested_oidc: rng.chance(80),
            })
            .collect();
        let candidate = if !catalog.is_empty() && rng.chance(40) {
            rng.pick(&catalog).package.clone()
        } else {
            package(&mut rng)
        };
        let policy = QuarantinePolicy {
            max_edit_distance: rng.below(5),
            min_namespace_age_secs: *rng.pick(&[0, 2_592_000, 5_000_000]),
            max_trust_cost: *rng.pick(&[0, 50, 100]),
            require_oidc_provenance: rng.chance(70),
            agent_mode: rng.chance(30),
            ..QuarantinePolicy::default()
        };
        let now = *rng.pick(&[0, 1_000, 3_000_000, 12_000_000, u64::MAX]);
        let want =
            reference::slopsquat::screen_package_admission(&candidate, &catalog, &policy, now);
        let got = slopsquat::screen_package_admission(&candidate, &catalog, &policy, now);
        assert_eq!(
            got, want,
            "case {case}: {candidate:?} {catalog:?} {policy:?} at {now}"
        );
        for f in &want.findings {
            tally.see(format!("{:?}", f.reason));
        }
        tally.see(format!("{:?}", want.decision));
    }
    tally.require(REASONS);
}

const TEXT_CHARS: &[char] = &[
    'a',
    'b',
    'B',
    '-',
    '_',
    '0',
    '1',
    '3',
    '4',
    '5',
    '7',
    '/',
    '@',
    '\u{e9}',
    '\u{1f600}',
];

#[test]
fn distances_keys_and_splits_match_the_rust_code() {
    let mut rng = Rng(0xd15);
    for case in 0..20000 {
        let a = rng.text(TEXT_CHARS, 10);
        let b = if rng.chance(30) {
            let mut b: Vec<char> = a.chars().collect();
            let at = rng.below(b.len() + 1);
            b.insert(at, *rng.pick(&['a', 'b']));
            b.into_iter().collect()
        } else {
            rng.text(TEXT_CHARS, 10)
        };
        assert_eq!(
            slopsquat::damerau_levenshtein(&a, &b),
            reference::slopsquat::damerau_levenshtein(&a, &b),
            "case {case}: {a:?} {b:?}"
        );
        assert_eq!(
            slopsquat::canonical_conflation_key(&a),
            reference::slopsquat::canonical_conflation_key(&a),
            "case {case}: {a:?}"
        );
        let p = package(&mut rng);
        assert_eq!(
            slopsquat::split_package(&p),
            reference::slopsquat::split_package(&p),
            "case {case}: {p:?}"
        );
    }
}

#[test]
fn the_source_tree_helper_is_a_source_tree() {
    assert!(source_tree(1).starts_with("b3v1:source:tree:"));
}
