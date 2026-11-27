//! Differential and ABBA flip-gate tests for `@vesper/loom-tuf`
//! (Rust-exit SH6 lane 16, #1411 / #2295).
use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::time::Instant;
use vesper_loom_tuf::{
    Limits, is_valid_target_name, percent_decode_role, percent_encode_role, sha256_hex,
    validate_delegation_graph, validate_json_bounds, verify_ed25519_quorum,
};

const ABBA_WARMUP_EPOCHS: usize = 2;
const ABBA_EPOCHS: usize = 30;
const ABBA_JSON_OPS: usize = 64;
const ABBA_GRAPH_OPS: usize = 128;
const ABBA_QUORUM_OPS: usize = 16;

const KEYID_LEN: usize = 32;
const PUBKEY_LEN: usize = 32;
const SIG_LEN: usize = 64;

mod reference {
    use super::*;

    fn safe_component(part: &str) -> bool {
        let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
            device.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        !part.is_empty()
            && part.len() <= 255
            && part != "."
            && part != ".."
            && !part.ends_with(['.', ' '])
            && !part.chars().any(char::is_control)
            && !part.contains(['\\', ':', '<', '>', '"', '|', '*', '?'])
            && !matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            && !numbered_device
    }

    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._~+@-".contains(&byte))
            && name.split('/').all(safe_component)
    }

    pub fn bound_json(bytes: &[u8], limits: &Limits) -> Result<(), &'static str> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| "invalid metadata JSON")?;
        fn walk(
            value: &serde_json::Value,
            limits: &Limits,
            depth: usize,
            nodes: &mut usize,
        ) -> Result<(), &'static str> {
            *nodes = nodes.saturating_add(1);
            if depth > limits.json_depth || *nodes > limits.files.saturating_mul(limits.roles) {
                return Err("metadata JSON traversal exceeds limit");
            }
            match value {
                serde_json::Value::Object(map) => {
                    if map.len() > limits.files {
                        return Err("metadata collection exceeds limit");
                    }
                    for child in map.values() {
                        walk(child, limits, depth + 1, nodes)?;
                    }
                }
                serde_json::Value::Array(array) => {
                    if array.len() > limits.files {
                        return Err("metadata collection exceeds limit");
                    }
                    for child in array {
                        walk(child, limits, depth + 1, nodes)?;
                    }
                }
                _ => (),
            }
            Ok(())
        }
        walk(&value, limits, 0, &mut 0)
    }

    pub fn check_graph_depth(
        graph: &BTreeMap<String, Vec<String>>,
        limits: &Limits,
    ) -> Result<(), &'static str> {
        fn dfs(
            name: &str,
            graph: &BTreeMap<String, Vec<String>>,
            path: &mut Vec<String>,
            budget: &mut usize,
            limits: &Limits,
        ) -> Result<(), &'static str> {
            if *budget == 0 {
                return Err("delegation graph work limit exceeded");
            }
            *budget -= 1;
            if path.len() > limits.directory_depth || path.iter().any(|seen| seen == name) {
                return Err("delegation graph depth or cycle exceeds policy");
            }
            path.push(name.to_owned());
            if let Some(children) = graph.get(name) {
                for child in children {
                    dfs(child, graph, path, budget, limits)?;
                }
            }
            path.pop();
            Ok(())
        }
        let mut budget = limits.roles.saturating_mul(limits.roles);
        for name in graph.keys() {
            dfs(name, graph, &mut Vec::new(), &mut budget, limits)?;
        }
        Ok(())
    }

    pub fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn verify_ed25519_quorum(
        payload: &[u8],
        role_keyids: &[u8],
        threshold: u64,
        pubkeys: &[u8],
        sigs: &[u8],
    ) -> Result<(), &'static str> {
        if threshold == 0 {
            return Err("invalid role threshold");
        }
        if !role_keyids.len().is_multiple_of(KEYID_LEN)
            || !pubkeys.len().is_multiple_of(KEYID_LEN + PUBKEY_LEN)
            || !sigs.len().is_multiple_of(KEYID_LEN + SIG_LEN)
        {
            return Err("malformed quorum input buffer");
        }
        let authorized: BTreeSet<&[u8]> = role_keyids
            .as_chunks::<KEYID_LEN>()
            .0
            .iter()
            .map(|c| c.as_slice())
            .collect();
        let keys: BTreeMap<&[u8], &[u8]> = pubkeys
            .as_chunks::<{ KEYID_LEN + PUBKEY_LEN }>()
            .0
            .iter()
            .map(|c| (&c[..KEYID_LEN], &c[KEYID_LEN..]))
            .collect();
        let mut seen: BTreeSet<&[u8]> = BTreeSet::new();
        for entry in sigs.as_chunks::<{ KEYID_LEN + SIG_LEN }>().0 {
            let keyid = &entry[..KEYID_LEN];
            let sig = &entry[KEYID_LEN..];
            if !authorized.contains(keyid) || seen.contains(keyid) {
                continue;
            }
            if let Some(&pk) = keys.get(keyid)
                && UnparsedPublicKey::new(&ED25519, pk)
                    .verify(payload, sig)
                    .is_ok()
            {
                seen.insert(keyid);
                if seen.len() as u64 >= threshold {
                    return Ok(());
                }
            }
        }
        Err("role signature threshold not met")
    }
}

fn make_keypair(seed: u8) -> (Ed25519KeyPair, [u8; KEYID_LEN], [u8; PUBKEY_LEN]) {
    let kp = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
    let pk: [u8; PUBKEY_LEN] = kp.public_key().as_ref().try_into().unwrap();
    let kid: [u8; KEYID_LEN] = Sha256::digest(pk).into();
    (kp, kid, pk)
}

#[test]
fn tuf_port_matches_rust_reference() {
    let mut cases = 0usize;

    // 1. Target name & component validation.
    let names = [
        "pkg/artifact.bin",
        "a/b/c_1.0~beta+2@foo-bar",
        "",
        "/",
        "pkg/",
        "/pkg/a",
        "pkg//a",
        ".",
        "..",
        "pkg/../a",
        "pkg/./a",
        "pkg/con",
        "pkg/CON.txt",
        "pkg/prn",
        "pkg/aux.json",
        "pkg/nul",
        "pkg/clock$",
        "pkg/com1",
        "pkg/LPT9.bin",
        "pkg/a.",
        "pkg/a ",
        "pkg/a\tb",
        "pkg/café",
        "pkg/a\\b",
        "pkg/a:b",
    ];
    for name in names {
        assert_eq!(
            is_valid_target_name(name),
            reference::valid_name(name),
            "name={name:?}"
        );
        cases += 1;
    }

    // 2. Streaming bounded JSON validation.
    let tight = Limits {
        json_depth: 4,
        files: 4,
        roles: 4,
        ..Limits::default()
    };
    let json_cases: &[&[u8]] = &[
        br#"{"signed":{"_type":"root","version":1,"keys":{}},"signatures":[]}"#,
        br#"{"a":[1,2,3,4]}"#,
        br#"{"a":[1,2,3,4,5]}"#,
        br#"{"a":{"b":{"c":{"d":{"e":1}}}}}"#,
        br#"{"a":true,"b":false,"c":null,"d":-12.34e+5,"e":"hi\n\u0041"}"#,
        br#"{"unterminated": "abc"#,
        br#"{"trailing": 1} extra"#,
        br#"[1, 2,]"#,
        br#"{}"#,
        br#"[]"#,
    ];
    for &doc in json_cases {
        assert_eq!(
            validate_json_bounds(doc, &tight).is_ok(),
            reference::bound_json(doc, &tight).is_ok(),
            "doc={}",
            String::from_utf8_lossy(doc)
        );
        cases += 1;
    }

    // 3. Delegation graph depth & cycle checks.
    let graph_limits = Limits {
        directory_depth: 3,
        roles: 8,
        ..Limits::default()
    };
    let mut g_ok = BTreeMap::new();
    g_ok.insert("targets".into(), vec!["a".into(), "b".into()]);
    g_ok.insert("a".into(), vec!["c".into()]);
    g_ok.insert("b".into(), vec!["c".into()]);
    g_ok.insert("c".into(), vec![]);

    let mut g_cycle = g_ok.clone();
    g_cycle.insert("c".into(), vec!["a".into()]);

    let mut g_deep = BTreeMap::new();
    g_deep.insert("n0".into(), vec!["n1".into()]);
    g_deep.insert("n1".into(), vec!["n2".into()]);
    g_deep.insert("n2".into(), vec!["n3".into()]);
    g_deep.insert("n3".into(), vec!["n4".into()]);
    g_deep.insert("n4".into(), vec![]);

    for g in [&g_ok, &g_cycle, &g_deep] {
        assert_eq!(
            validate_delegation_graph(g, &graph_limits).is_ok(),
            reference::check_graph_depth(g, &graph_limits).is_ok(),
        );
        cases += 1;
    }

    // 4. SHA-256 hex digest.
    for msg in [
        &b""[..],
        &b"vesper-loom-tuf"[..],
        &b"authenticated source archive fixture\n"[..],
    ] {
        assert_eq!(sha256_hex(msg), reference::sha256_hex(msg));
        cases += 1;
    }

    // 5. Ed25519 quorum verification.
    let (kp1, kid1, pk1) = make_keypair(1);
    let (kp2, kid2, pk2) = make_keypair(2);
    let (kp3, kid3, pk3) = make_keypair(3);
    let payload = br#"{"_type":"root","spec_version":"1.0.0","version":2}"#;
    let sig1 = kp1.sign(payload);
    let sig2 = kp2.sign(payload);
    let sig3 = kp3.sign(payload);
    let mut bad_sig = sig2.as_ref().to_vec();
    bad_sig[0] ^= 0xff;

    let mut role_keyids = Vec::new();
    role_keyids.extend_from_slice(&kid1);
    role_keyids.extend_from_slice(&kid2);

    let mut pubkeys = Vec::new();
    for (kid, pk) in [(&kid1, &pk1), (&kid2, &pk2), (&kid3, &pk3)] {
        pubkeys.extend_from_slice(kid);
        pubkeys.extend_from_slice(pk);
    }

    let make_sigs = |items: &[(&[u8], &[u8])]| -> Vec<u8> {
        let mut out = Vec::new();
        for (kid, s) in items {
            out.extend_from_slice(kid);
            out.extend_from_slice(s);
        }
        out
    };

    let quorum_cases: &[(u64, Vec<u8>)] = &[
        (1, make_sigs(&[(&kid1, sig1.as_ref())])),
        (
            2,
            make_sigs(&[(&kid1, sig1.as_ref()), (&kid2, sig2.as_ref())]),
        ),
        // Duplicate keyid must only count once.
        (
            2,
            make_sigs(&[(&kid1, sig1.as_ref()), (&kid1, sig1.as_ref())]),
        ),
        // Unauthorized keyid must not count.
        (
            2,
            make_sigs(&[(&kid1, sig1.as_ref()), (&kid3, sig3.as_ref())]),
        ),
        // Corrupted signature must not count.
        (2, make_sigs(&[(&kid1, sig1.as_ref()), (&kid2, &bad_sig)])),
        // Zero threshold is invalid.
        (0, make_sigs(&[(&kid1, sig1.as_ref())])),
    ];
    for (threshold, sigs) in quorum_cases {
        assert_eq!(
            verify_ed25519_quorum(payload, &role_keyids, *threshold, &pubkeys, sigs).is_ok(),
            reference::verify_ed25519_quorum(payload, &role_keyids, *threshold, &pubkeys, sigs)
                .is_ok(),
            "threshold={threshold}"
        );
        cases += 1;
    }

    // 6. Role-name percent-encoding and percent-decoding.
    const ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'_')
        .remove(b'.')
        .remove(b'-')
        .remove(b'~');
    for role in [
        "targets",
        "pkg/sub-role_1.0~a",
        "role with spaces/and:colons%25",
        "café/日本語",
        "bad%ZZescape%",
    ] {
        let enc_port = percent_encode_role(role);
        let enc_ref = percent_encoding::utf8_percent_encode(role, ESCAPE).to_string();
        assert_eq!(enc_port, enc_ref, "encode role={role:?}");
        let dec_port = percent_decode_role(&enc_port).unwrap();
        let dec_ref = percent_encoding::percent_decode_str(&enc_ref)
            .decode_utf8()
            .unwrap()
            .into_owned();
        assert_eq!(dec_port, dec_ref, "decode role={role:?}");
        cases += 2;
    }

    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_tuf() {
    let limits = Limits::default();
    let mut doc = String::from(
        "{\"signed\":{\"_type\":\"targets\",\"spec_version\":\"1.0.0\",\"version\":42,\"targets\":{",
    );
    for i in 0..64 {
        if i > 0 {
            doc.push(',');
        }
        doc.push_str(&format!(
            "\"pkg/item-{i}.tar.zst\":{{\"length\":{len},\"hashes\":{{\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}}}",
            len = 4096 + i * 17
        ));
    }
    doc.push_str("}},\"signatures\":[{\"keyid\":\"00\",\"sig\":\"11\"}]}");
    let doc_bytes = doc.as_bytes();

    let run_rust_json = || {
        for _ in 0..ABBA_JSON_OPS {
            assert!(black_box(
                reference::bound_json(black_box(doc_bytes), black_box(&limits)).is_ok()
            ));
        }
    };
    let run_vesper_json = || {
        for _ in 0..ABBA_JSON_OPS {
            assert!(black_box(
                validate_json_bounds(black_box(doc_bytes), black_box(&limits)).is_ok()
            ));
        }
    };
    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_json();
        run_vesper_json();
        run_vesper_json();
        run_rust_json();
    }
    let mut json_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_json();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper_json();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper_json();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust_json();
        let a2 = t3.elapsed().as_nanos().max(1);
        json_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs bound_json {}", json_pairs.join(" "));

    let mut graph = BTreeMap::new();
    for i in 0..24 {
        let mut kids = Vec::new();
        if i + 1 < 24 {
            kids.push(format!("role-{}", i + 1));
        }
        if i + 2 < 24 && i % 3 == 0 {
            kids.push(format!("role-{}", i + 2));
        }
        graph.insert(format!("role-{i}"), kids);
    }
    let run_rust_graph = || {
        for _ in 0..ABBA_GRAPH_OPS {
            assert!(black_box(
                reference::check_graph_depth(black_box(&graph), black_box(&limits)).is_ok()
            ));
        }
    };
    let run_vesper_graph = || {
        for _ in 0..ABBA_GRAPH_OPS {
            assert!(black_box(
                validate_delegation_graph(black_box(&graph), black_box(&limits)).is_ok()
            ));
        }
    };
    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_graph();
        run_vesper_graph();
        run_vesper_graph();
        run_rust_graph();
    }
    let mut graph_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_graph();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper_graph();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper_graph();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust_graph();
        let a2 = t3.elapsed().as_nanos().max(1);
        graph_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs delegation_graph {}", graph_pairs.join(" "));

    let (kp1, kid1, pk1) = make_keypair(1);
    let (kp2, kid2, pk2) = make_keypair(2);
    let sig1 = kp1.sign(doc_bytes);
    let sig2 = kp2.sign(doc_bytes);
    let mut role_keyids = Vec::new();
    role_keyids.extend_from_slice(&kid1);
    role_keyids.extend_from_slice(&kid2);
    let mut pubkeys = Vec::new();
    pubkeys.extend_from_slice(&kid1);
    pubkeys.extend_from_slice(&pk1);
    pubkeys.extend_from_slice(&kid2);
    pubkeys.extend_from_slice(&pk2);
    let mut sigs = Vec::new();
    sigs.extend_from_slice(&kid1);
    sigs.extend_from_slice(sig1.as_ref());
    sigs.extend_from_slice(&kid2);
    sigs.extend_from_slice(sig2.as_ref());

    let run_rust_quorum = || {
        for _ in 0..ABBA_QUORUM_OPS {
            assert!(black_box(
                reference::verify_ed25519_quorum(
                    black_box(doc_bytes),
                    black_box(&role_keyids),
                    2,
                    black_box(&pubkeys),
                    black_box(&sigs),
                )
                .is_ok()
            ));
        }
    };
    let run_vesper_quorum = || {
        for _ in 0..ABBA_QUORUM_OPS {
            assert!(black_box(
                verify_ed25519_quorum(
                    black_box(doc_bytes),
                    black_box(&role_keyids),
                    2,
                    black_box(&pubkeys),
                    black_box(&sigs),
                )
                .is_ok()
            ));
        }
    };
    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_quorum();
        run_vesper_quorum();
        run_vesper_quorum();
        run_rust_quorum();
    }
    let mut quorum_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_quorum();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper_quorum();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper_quorum();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust_quorum();
        let a2 = t3.elapsed().as_nanos().max(1);
        quorum_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs ed25519_quorum {}", quorum_pairs.join(" "));

    // 4. Checkpoint::from_json vs serde_json::from_slice.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CheckpointWireRef {
        state: StateRef,
        checksum: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StateRef {
        schema_version: u32,
        repository_id: String,
        bootstrap_sha256: String,
        roots: Vec<Vec<u8>>,
        timestamp_version: u64,
        snapshot_version: u64,
        role_versions: BTreeMap<String, u64>,
        verified_at: String,
    }
    let root_bytes: Vec<u8> = (0..512).map(|i| (i % 251) as u8).collect();
    let ck_json = serde_json::to_vec(&serde_json::json!({
        "state": {
            "schema_version": 1,
            "repository_id": "repo-main",
            "bootstrap_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "roots": [root_bytes, root_bytes],
            "timestamp_version": 42,
            "snapshot_version": 17,
            "role_versions": {"targets": 5, "packages": 3, "releases": 9},
            "verified_at": "2026-11-27T00:00:00Z"
        },
        "checksum": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    }))
    .unwrap();
    // Differential sanity check.
    let ck_port = vesper_loom_tuf::Checkpoint::from_json(&ck_json, &limits).unwrap();
    let ck_ref: CheckpointWireRef = serde_json::from_slice(&ck_json).unwrap();
    assert_eq!(ck_port.to_json().unwrap().len(), ck_json.len());
    let _ = (
        ck_ref.state.schema_version,
        &ck_ref.state.repository_id,
        &ck_ref.state.bootstrap_sha256,
        ck_ref.state.roots.len(),
        ck_ref.state.timestamp_version,
        ck_ref.state.snapshot_version,
        ck_ref.state.role_versions.len(),
        &ck_ref.state.verified_at,
        &ck_ref.checksum,
    );

    const ABBA_CHECKPOINT_OPS: usize = 2_000;
    let run_rust_ck = || {
        for _ in 0..ABBA_CHECKPOINT_OPS {
            let w: CheckpointWireRef = serde_json::from_slice(black_box(&ck_json)).unwrap();
            black_box(w);
        }
    };
    let run_vesper_ck = || {
        for _ in 0..ABBA_CHECKPOINT_OPS {
            let c = vesper_loom_tuf::Checkpoint::from_json(black_box(&ck_json), black_box(&limits))
                .unwrap();
            black_box(c);
        }
    };
    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_ck();
        run_vesper_ck();
        run_vesper_ck();
        run_rust_ck();
    }
    let mut ck_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_ck();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper_ck();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper_ck();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust_ck();
        let a2 = t3.elapsed().as_nanos().max(1);
        ck_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs checkpoint_json {}", ck_pairs.join(" "));
}
