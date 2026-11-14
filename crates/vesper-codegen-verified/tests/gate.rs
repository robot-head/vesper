//! §42.5 gate acceptance: the honest lock passes; each tampering fails with its
//! own typed error.

use ed25519_dalek::{Signer, SigningKey};
use vesper_codegen_verified::*;
use vesper_loom_cas::{DerivationInputs, Domain, ObjectId, ObjectKind};
use vesper_loom_trust::{
    Attestation, AttestationBundle, BuilderPolicy, Statement, TrustedBuilder,
    canonical_signing_bytes,
};

const STAGE: &[u8] = b"\x7fELF stage-2 == stage-3 == stage-3' compiler image";
const SPEC: &[u8] = b"VesperMetatheory.olean interface bytes";
const SOURCES: [(&str, &[u8]); 2] = [
    ("src/driver.vsp", b"pub fn compile_unit_verified() {}"),
    ("Loom.toml", b"capability_ceiling = [\"Fail\"]"),
];
const TARGET: &str = "x86_64-unknown-linux-gnu";
const SANDBOX: &str = "vesper:sandbox/step@0.1.0";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn derivation() -> String {
    let source = ObjectId::new(
        Domain::Source,
        ObjectKind::Tree,
        vesper_loom_cas::Digest(*source_merkle(&SOURCES).unwrap().as_bytes()),
    );
    DerivationInputs::new(Domain::Artifact, "vesperc-vsp-2", "stage2")
        .with_target(TARGET)
        .with_input("source", source)
        .key()
        .unwrap()
        .to_string()
}

fn honest_lock(quorum: usize, tee: bool) -> BootstrapLock {
    let fixed = blake3::hash(STAGE);
    BootstrapLock::new(
        BootstrapSection {
            compiler_package: "@vesper-lang/compiler".into(),
            version: "1.0.0".into(),
            source_merkle: source_merkle(&SOURCES).unwrap(),
            lean_spec: blake3::hash(SPEC),
            allowed_lean_axioms: vec!["propext".into(), "Quot.sound".into()],
        },
        FixedPointQuorum {
            path_a_stage2: fixed,
            path_a_stage3: fixed,
            path_b_lean_ddc: fixed,
            lila_builder_signatures: quorum,
            aegis_tdx_snp_attested: tee,
        },
    )
    .unwrap()
}

struct Builders {
    keys: Vec<SigningKey>,
    policy: BuilderPolicy,
    derivation: String,
}

fn builders(n: u8) -> Builders {
    let keys: Vec<_> = (1..=n).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let policy = BuilderPolicy {
        schema_version: 1,
        minimum_operators: 2,
        max_statement_ttl: 500,
        builders: keys
            .iter()
            .enumerate()
            .map(|(i, k)| TrustedBuilder {
                key_id: format!("lila-{i}"),
                operator_id: format!("operator-{i}"),
                public_key: hex(&k.verifying_key().to_bytes()),
                valid_from: 10,
                valid_until: 1000,
                revoked: false,
            })
            .collect(),
    };
    Builders {
        keys,
        policy,
        derivation: derivation(),
    }
}

impl Builders {
    fn evidence<'a>(&'a self, bundle: &'a AttestationBundle) -> BuilderEvidence<'a> {
        BuilderEvidence {
            bundle,
            policy: &self.policy,
            derivation: &self.derivation,
            target: TARGET,
            sandbox_contract: SANDBOX,
            now: 200,
        }
    }

    /// Sign the identity the gate expects for `stage3` with keys `which`.
    fn sign(&self, lock: &BootstrapLock, stage3: &[u8], which: &[usize]) -> AttestationBundle {
        let empty = AttestationBundle {
            schema_version: 1,
            attestations: vec![],
        };
        let identity = expected_build_identity(
            lock,
            source_merkle(&SOURCES).unwrap(),
            blake3::hash(stage3),
            stage3,
            &self.evidence(&empty),
        )
        .unwrap();
        AttestationBundle {
            schema_version: 1,
            attestations: which
                .iter()
                .map(|&i| {
                    let statement = Statement {
                        schema_version: 1,
                        key_id: format!("lila-{i}"),
                        identity: identity.clone(),
                        issued_at: 100,
                        expires_at: 300,
                    };
                    let signature = hex(&self.keys[i]
                        .sign(&canonical_signing_bytes(&statement).unwrap())
                        .to_bytes());
                    Attestation {
                        statement,
                        signature,
                    }
                })
                .collect(),
        }
    }
}

fn evidence<'a>(
    b: &'a Builders,
    bundle: &'a AttestationBundle,
    stage2: &'a [u8],
    stage3: &'a [u8],
    path_b: &'a [u8],
) -> BootstrapEvidence<'a> {
    BootstrapEvidence {
        source_files: &SOURCES,
        lean_spec: SPEC,
        lean_axioms_used: &["propext"],
        stage2,
        stage3,
        path_b_ddc: path_b,
        builders: b.evidence(bundle),
        tee: TeeEvidence::None,
        rewrite: RewriteEvidence::None,
    }
}

#[test]
fn honest_lock_passes_and_round_trips() {
    let lock = honest_lock(3, false);
    assert_eq!(parse_lock(lock.to_toml().as_bytes()).unwrap(), lock);
    let b = builders(3);
    let bundle = b.sign(&lock, STAGE, &[0, 1, 2]);
    let report = audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE))
        .expect("honest lock passes");
    assert_eq!(report.fixed_point, blake3::hash(STAGE));
    assert_eq!(report.verified_operators.len(), 3);
}

#[test]
fn tampered_lock_digest_fails() {
    let mut lock = honest_lock(3, false);
    let other = blake3::hash(b"trojaned compiler");
    lock.quorum.path_a_stage2 = other;
    lock.quorum.path_a_stage3 = other;
    lock.quorum.path_b_lean_ddc = other;
    let lock = parse_lock(lock.to_toml().as_bytes()).unwrap();
    let b = builders(3);
    let bundle = b.sign(&lock, STAGE, &[0, 1, 2]);
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE)),
        Err(BootstrapVerifyError::TamperedDigest {
            field: "path_a_stage3_blake3",
            locked: other,
            actual: blake3::hash(STAGE),
        })
    );
}

#[test]
fn lock_with_one_tampered_field_is_not_a_fixed_point() {
    let text = honest_lock(3, false).to_toml();
    let fixed = blake3::hash(STAGE).to_hex().to_string();
    let other = blake3::hash(b"x").to_hex().to_string();
    let tampered = text.replacen(
        &format!("path_b_lean_compcert_ddc  = \"blake3:{fixed}\""),
        &format!("path_b_lean_compcert_ddc  = \"blake3:{other}\""),
        1,
    );
    assert_ne!(tampered, text);
    assert_eq!(
        parse_lock(tampered.as_bytes()),
        Err(BootstrapVerifyError::LockNotAtFixedPoint)
    );
}

#[test]
fn missing_signature_fails_quorum() {
    let lock = honest_lock(3, false);
    let b = builders(3);
    let bundle = b.sign(&lock, STAGE, &[0, 2]);
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE)),
        Err(BootstrapVerifyError::InsufficientLilaBuilderQuorum {
            actual: 2,
            min_required: 3,
        })
    );
    // Below the policy's own minimum too.
    let bundle = b.sign(&lock, STAGE, &[1]);
    assert!(matches!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE)),
        Err(BootstrapVerifyError::InsufficientLilaBuilderQuorum { actual: 1, .. })
    ));
}

#[test]
fn forged_or_mismatched_signature_is_rejected() {
    let lock = honest_lock(2, false);
    let b = builders(3);
    let mut bundle = b.sign(&lock, STAGE, &[0, 1, 2]);
    let mut sig = bundle.attestations[1].signature.clone().into_bytes();
    sig[0] = if sig[0] == b'0' { b'1' } else { b'0' };
    bundle.attestations[1].signature = String::from_utf8(sig).unwrap();
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE)),
        Err(BootstrapVerifyError::BuilderSignatureRejected {
            codes: vec!["invalid_signature".into()],
        })
    );
    // Signatures over a different Stage 3 do not count for this one.
    let bundle = b.sign(&lock, b"another stage 3", &[0, 1, 2]);
    assert!(matches!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, STAGE)),
        Err(BootstrapVerifyError::BuilderSignatureRejected { codes }) if codes.contains(&"identity_mismatch".to_string())
    ));
}

#[test]
fn stage2_stage3_mismatch_fails() {
    let lock = honest_lock(3, false);
    let b = builders(3);
    let bundle = b.sign(&lock, STAGE, &[0, 1, 2]);
    let stage3 = b"\x7fELF stage-3 with a self-propagating backdoor";
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, stage3, STAGE)),
        Err(BootstrapVerifyError::StageFixedPointDivergence {
            stage2: blake3::hash(STAGE),
            stage3: blake3::hash(stage3),
        })
    );
}

#[test]
fn path_b_mismatch_fails() {
    let lock = honest_lock(3, false);
    let b = builders(3);
    let bundle = b.sign(&lock, STAGE, &[0, 1, 2]);
    let path_b = b"\x7fELF stage-3' built by vesperc-lean-ddc";
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &evidence(&b, &bundle, STAGE, STAGE, path_b)),
        Err(BootstrapVerifyError::DdcCrossCompilerMismatch {
            stage3_path_a: blake3::hash(STAGE),
            stage3_path_b_ddc: blake3::hash(path_b),
        })
    );
}

#[test]
fn source_spec_and_axioms_are_bound() {
    let lock = honest_lock(2, false);
    let b = builders(2);
    let bundle = b.sign(&lock, STAGE, &[0, 1]);
    let base = evidence(&b, &bundle, STAGE, STAGE, STAGE);

    let changed: [(&str, &[u8]); 2] =
        [SOURCES[0], ("Loom.toml", b"capability_ceiling = [\"Net\"]")];
    let mut e = base.clone();
    e.source_files = &changed;
    assert!(matches!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::SourceMerkleMismatch { .. })
    ));

    let mut e = base.clone();
    e.lean_spec = b"different spec";
    assert!(matches!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::LeanSpecMismatch { .. })
    ));

    let mut e = base.clone();
    e.lean_axioms_used = &["propext", "Classical.choice"];
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::DisallowedLeanAxiom {
            axiom: "Classical.choice".into()
        })
    );
}

#[test]
fn tee_attestation_must_bind_the_fixed_point() {
    let lock = honest_lock(2, true);
    let b = builders(2);
    let bundle = b.sign(&lock, STAGE, &[0, 1]);
    let mut e = evidence(&b, &bundle, STAGE, STAGE, STAGE);
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::MissingTeeAttestation)
    );
    e.tee = TeeEvidence::VerifiedReportData([0; 64]);
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::TeeReportDataMismatch)
    );
    let mut data = [0; 64];
    data[..32].copy_from_slice(blake3::hash(STAGE).as_bytes());
    e.tee = TeeEvidence::VerifiedReportData(data);
    assert!(audit_ddc_bootstrap_quorum(&lock, &e).is_ok());
}

#[test]
fn rewrite_certificates_report_the_unwired_checker() {
    let lock = honest_lock(2, false);
    let b = builders(2);
    let bundle = b.sign(&lock, STAGE, &[0, 1]);
    let mut e = evidence(&b, &bundle, STAGE, STAGE, STAGE);
    e.rewrite = RewriteEvidence::Certificates(vec![1, 2, 3]);
    assert_eq!(
        audit_ddc_bootstrap_quorum(&lock, &e),
        Err(BootstrapVerifyError::RewriteCheckerNotWired)
    );
}

#[test]
fn specification_example_lock_parses() {
    // The §42.2 listing, verbatim.
    let text = r#"# bootstrap/stage3_ddc.lock — Pinned Hermetic Fixed-Point & Diverse Double-Compile Anchor
[bootstrap]
compiler_package     = "@vesper-lang/compiler"
version              = "1.0.0"
source_merkle_blake3 = "blake3:7a19e4c0f82b31d69a04e581c3b72f109e84d2a16c5b30f9e1d2a4c8b7f60123"
lean_spec_mif        = "mif:blake3:3c8e91a204f7b5d1e6c28094a7b3d5f18290e4c6b1a3f5d790e2c4a6b8d0f1e2"
allowed_lean_axioms  = ["propext", "Quot.sound"]
[fixed_point_quorum]
path_a_stage2_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
path_a_stage3_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
path_b_lean_compcert_ddc  = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
lila_builder_signatures   = 3
aegis_tdx_snp_attested    = true
"#;
    let lock = parse_lock(text.as_bytes()).unwrap();
    assert_eq!(lock.quorum.lila_builder_signatures, 3);
    assert!(lock.quorum.aegis_tdx_snp_attested);
}

#[test]
fn malformed_locks_are_typed_errors() {
    let good = honest_lock(3, false).to_toml();
    type Check = fn(&BootstrapVerifyError) -> bool;
    let cases: Vec<(String, Check)> = vec![
        (
            good.replace("aegis_tdx_snp_attested", "aegis_tdx_snp_attested_x"),
            |e| matches!(e, BootstrapVerifyError::LockSyntax { .. }),
        ),
        (format!("{good}extra = 1\n"), |e| {
            matches!(e, BootstrapVerifyError::LockSyntax { .. })
        }),
        (good.replacen("blake3:", "sha256:", 1), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "source_merkle_blake3",
                    ..
                }
            )
        }),
        (good.replace("= 3\n", "= 1\n"), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "lila_builder_signatures",
                    ..
                }
            )
        }),
        (good.replace("= 3\n", "= -3\n"), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "lila_builder_signatures",
                    ..
                }
            )
        }),
        (good.replace("\"Quot.sound\"", "\"sorryAx\""), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "allowed_lean_axioms",
                    ..
                }
            )
        }),
        (good.replace("\"Quot.sound\"", "\"propext\""), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "allowed_lean_axioms",
                    ..
                }
            )
        }),
        (good.replace("\"1.0.0\"", "\"1.0\""), |e| {
            matches!(
                e,
                BootstrapVerifyError::InvalidLockField {
                    field: "version",
                    ..
                }
            )
        }),
        (
            good.replace("@vesper-lang/compiler", "not a package"),
            |e| {
                matches!(
                    e,
                    BootstrapVerifyError::InvalidLockField {
                        field: "compiler_package",
                        ..
                    }
                )
            },
        ),
    ];
    for (text, expected) in cases {
        assert_ne!(text, good);
        let err = parse_lock(text.as_bytes()).unwrap_err();
        assert!(expected(&err), "{err:?} for\n{text}");
    }
    assert!(matches!(
        parse_lock(&[0xff, 0xfe]),
        Err(BootstrapVerifyError::LockSyntax { .. })
    ));
    assert!(matches!(
        parse_lock(&vec![b'#'; MAX_LOCK_BYTES + 1]),
        Err(BootstrapVerifyError::LockTooLarge { .. })
    ));
    // Upper-case hex is not canonical.
    let fixed = blake3::hash(STAGE).to_hex().to_string();
    let upper = good.replace(&fixed, &fixed.to_uppercase());
    assert!(matches!(
        parse_lock(upper.as_bytes()),
        Err(BootstrapVerifyError::InvalidLockField { .. })
    ));
}

#[test]
fn source_merkle_is_order_independent_and_rejects_duplicates() {
    let a = source_merkle(&SOURCES).unwrap();
    let b = source_merkle(&[SOURCES[1], SOURCES[0]]).unwrap();
    assert_eq!(a, b);
    assert!(source_merkle(&[SOURCES[0], SOURCES[0]]).is_err());
    // Moving bytes between the path and the content changes the root.
    assert_ne!(
        source_merkle(&[("ab", b"c")]).unwrap(),
        source_merkle(&[("a", b"bc")]).unwrap()
    );
}

#[test]
fn rewrite_hook_is_not_wired() {
    assert_eq!(
        verify_rewrite_certificate_chain(&[]),
        Err(BootstrapVerifyError::RewriteCheckerNotWired)
    );
}
