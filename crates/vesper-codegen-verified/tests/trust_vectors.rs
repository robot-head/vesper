//! Frozen vectors for the auditor's trust copy (`src/trust.rs`).
//!
//! The digests below were recorded when this copy replaced the frozen seeds
//! of `vesper-loom-calculus`, `vesper-loom-cas` and `vesper-loom-trust`
//! (#1929): a differential over these same inputs found every signing
//! message, object id, package-name verdict and verification report
//! (accepted flag, keys, operators and every diagnostic) identical to the
//! seeds'. A change to the copy that alters any of them fails here; the
//! seeds are gone, so the vectors are the record of their behaviour.
use ed25519_dalek::{Signer, SigningKey};
use std::fmt::Write;
use vesper_codegen_verified::trust as c;

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

const PACKAGE_NAMES: [&str; 16] = [
    "@a/b",
    "@vesper-lang/compiler",
    "std/x",
    "x/y",
    "@A/b",
    "@a/b/c",
    "@a/",
    "@/b",
    "@1a/b",
    "@a/b_c-9",
    "@a-/b",
    "",
    "@a/b\u{e9}",
    "std/9",
    "@std/x",
    "a",
];
const VALID_PACKAGE_NAMES: [&str; 6] = [
    "@a/b",
    "@vesper-lang/compiler",
    "std/x",
    "@a/b_c-9",
    "@a-/b",
    "@std/x",
];

#[test]
fn package_names() {
    for n in PACKAGE_NAMES {
        assert_eq!(
            c::PackageName::parse(n).is_ok(),
            VALID_PACKAGE_NAMES.contains(&n),
            "{n}"
        );
    }
}

#[test]
fn object_ids() {
    let mut transcript = String::new();
    for d in [c::Domain::Source, c::Domain::Interface, c::Domain::Artifact] {
        for bytes in [&b""[..], b"abc", &[0u8; 5000]] {
            writeln!(transcript, "{}", c::blob_id(d, bytes)).unwrap();
        }
    }
    for s in [
        "b3v1:source:tree:".to_string() + &"0".repeat(64),
        "b3v1:artifact:blob:".to_string() + &"af".repeat(32),
        "b3v1:artifact:blob:".to_string() + &"AF".repeat(32),
        "b3v1:artifact:blob:".to_string() + &"a".repeat(63),
        "b3v1:artifact:blob:".to_string() + &"a".repeat(64) + ":",
        "b3v1:other:blob:".to_string() + &"a".repeat(64),
        "b3v1:source:dir:".to_string() + &"a".repeat(64),
    ] {
        writeln!(
            transcript,
            "{:?}",
            c::ObjectId::parse(&s).map(|i| i.to_string())
        )
        .unwrap();
    }
    assert_eq!(
        blake3::hash(transcript.as_bytes()).to_hex().as_str(),
        OBJECT_IDS
    );
}

const OBJECT_IDS: &str = "8f3e8addb0496c61fa1615f512e081ff37689fcb51cd9c410cb9a11bd82514c0";
const SIGNING_BYTES: &str = "35a0dec5b556a7c7e1aa9fd15966250dd74fbc7f68d1e02df32a5d6ea5d4cf8f";
const REPORTS: &str = "14754fefb98a93d7150d82b095dfb3137d06e96366ddbba2043ad1f01e233ab7";

fn identity_json(i: usize) -> serde_json::Value {
    let src = c::ObjectId::new(c::Domain::Source, c::ObjectKind::Tree, [i as u8; 32]).to_string();
    let out = c::blob_id(c::Domain::Artifact, format!("stage3-{i}").as_bytes()).to_string();
    let mut v = serde_json::json!({
        "package": "@vesper-lang/compiler",
        "version": "1.0.0",
        "source_tree": src,
        "derivation": c::derivation_id(&[7; 32]),
        "target": "x86_64-unknown-linux-gnu",
        "builder_blake3": hex(&[9; 32]),
        "sandbox_contract": "vesper:sandbox/step@0.1.0",
        "output": out,
    });
    // Mutations: each breaks one rule (or none for i % 13 == 0).
    let o = v.as_object_mut().unwrap();
    match i % 13 {
        1 => {
            o.insert("version".into(), "01.0.0".into());
        }
        2 => {
            o.insert("version".into(), "1.0.0+build".into());
        }
        3 => {
            o.insert(
                "source_tree".into(),
                c::blob_id(c::Domain::Source, b"x").to_string().into(),
            );
        }
        4 => {
            o.insert(
                "derivation".into(),
                format!("drv:b3v1:{}", "A".repeat(64)).into(),
            );
        }
        5 => {
            o.insert(
                "output".into(),
                c::blob_id(c::Domain::Interface, b"x").to_string().into(),
            );
        }
        6 => {
            o.insert("builder_blake3".into(), "ab".into());
        }
        7 => {
            o.insert("target".into(), "bad target".into());
        }
        8 => {
            o.insert("source_tree".into(), "b3v2:source:tree:00".into());
        }
        9 => {
            o.insert("version".into(), "1.0.0-rc.1".into());
        }
        10 => {
            o.insert("package".into(), "std/core".into());
        }
        11 => {
            o.insert("sandbox_contract".into(), "x".repeat(257).into());
        }
        12 => {
            o.insert("version".into(), "1.0".into());
        }
        _ => {}
    }
    v
}

#[test]
fn signing_messages_and_reports() {
    let keys: Vec<_> = (1..=4u8)
        .map(|i| SigningKey::from_bytes(&[i; 32]))
        .collect();
    let mut signing = blake3::Hasher::new();
    let mut reports = String::new();
    for i in 0..13 * 6 {
        let ident = identity_json(i);
        let expected_json = identity_json(i - i % 13);
        if serde_json::from_value::<c::BuildIdentity>(ident.clone()).is_err() {
            writeln!(reports, "case {i}: identity does not parse").unwrap();
            continue;
        }
        let variant = i / 13;
        let policy = serde_json::json!({
            "schema_version": 1,
            "minimum_operators": if variant == 5 { 1 } else { 2 },
            "max_statement_ttl": if variant == 4 { 50 } else { 500 },
            "builders": keys.iter().enumerate().map(|(k, key)| serde_json::json!({
                "key_id": format!("lila-{k}"),
                "operator_id": format!("op-{}", if variant == 3 { 0 } else { k }),
                "public_key": hex(&key.verifying_key().to_bytes()),
                "valid_from": 10,
                "valid_until": 1000,
                "revoked": variant == 2 && k == 0,
            })).collect::<Vec<_>>(),
        });
        let mut attestations = vec![];
        for (k, key) in keys.iter().enumerate().take(3) {
            let statement = serde_json::json!({
                "schema_version": 1,
                "key_id": format!("lila-{k}"),
                "identity": ident,
                "issued_at": if variant == 1 && k == 1 { 5 } else { 100 },
                "expires_at": 300,
            });
            let cs: c::Statement = serde_json::from_value(statement.clone()).unwrap();
            let cb = c::canonical_signing_bytes(&cs);
            if let Ok(b) = &cb {
                signing.update(b);
            }
            let msg = cb.unwrap_or_else(|_| b"x".to_vec());
            let mut sig = key.sign(&msg).to_bytes();
            if variant == 1 && k == 2 {
                sig[0] ^= 1;
            }
            attestations.push(serde_json::json!({"statement": statement, "signature": hex(&sig)}));
        }
        let bundle = serde_json::json!({"schema_version": 1, "attestations": attestations});
        let r = c::verify_attestations(
            &serde_json::from_value(expected_json).unwrap(),
            &serde_json::from_value(bundle).unwrap(),
            &serde_json::from_value(policy).unwrap(),
            200,
        );
        writeln!(
            reports,
            "case {i}: {} {:?} {:?} {:?}",
            r.accepted, r.verified_keys, r.verified_operators, r.diagnostics
        )
        .unwrap();
    }
    assert_eq!(signing.finalize().to_hex().as_str(), SIGNING_BYTES);
    assert_eq!(
        blake3::hash(reports.as_bytes()).to_hex().as_str(),
        REPORTS,
        "{reports}"
    );
}
