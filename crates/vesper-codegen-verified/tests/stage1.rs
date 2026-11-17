use vesper_codegen_verified::stage1::{
    FrontendOutput, MAX_BYTES, ProgramEvidence, ResultKind, Stage1Error, Stage1Evidence, Stage1Lock,
};

const PARENT: &[u8] = include_bytes!("../../../bootstrap/stage0_seed.lock");
const SOURCES: &[(&str, &[u8])] = &[("src/parser.ves", b"frontend")];

#[test]
fn committed_record_binds_parent_and_is_strictly_cross_backend() {
    use vesper_codegen_verified::stage1::{VerifiedRecord, digest};
    let bytes = include_bytes!("../../../bootstrap/stage1.lock");
    let lock = Stage1Lock::parse(bytes).unwrap();
    assert_eq!(lock.parent_seed_lock, digest(PARENT));
    assert_eq!(
        lock.programs
            .iter()
            .filter(|p| p.kind == ResultKind::CstDigest)
            .count(),
        10
    );
    let probe = lock
        .programs
        .iter()
        .find(|p| p.kind == ResultKind::LexerByteClasses)
        .unwrap();
    assert_eq!(probe.result, 7856525685952484840);
    assert!(matches!(probe.verified, VerifiedRecord::Built { .. }));
    // Strict Stage 1: every frontend build, parser included, was built by the
    // verified backend too and returned the same full CST digest.
    assert!(
        lock.programs
            .iter()
            .all(|p| matches!(p.verified, VerifiedRecord::Built { .. }))
    );
    lock.require_cross_backend().unwrap();
    assert_eq!(lock.to_toml().unwrap().as_bytes(), bytes);
}

fn program<'a>(verified: Result<&'a [u8], &'a str>) -> ProgramEvidence<'a> {
    ProgramEvidence {
        name: "fib",
        kind: ResultKind::CstDigest,
        source: b"fn main() -> i64 { 1 }",
        seed_result: 42,
        cranelift: FrontendOutput {
            artifact: b"cranelift",
            result: 42,
        },
        verified: verified.map(|artifact| FrontendOutput {
            artifact,
            result: 42,
        }),
    }
}
fn evidence<'a>(programs: &'a [ProgramEvidence<'a>]) -> Stage1Evidence<'a> {
    Stage1Evidence {
        parent_lock: PARENT,
        seed_binary: b"seed",
        frontend_sources: SOURCES,
        programs,
    }
}
fn record(programs: &[ProgramEvidence<'_>]) -> Stage1Lock {
    Stage1Lock::record("x86_64-unknown-linux-gnu", &evidence(programs)).unwrap()
}

#[test]
fn cross_backend_requires_built_artifacts_and_full_matching_cst_results() {
    let programs = [program(Ok(b"verified"))];
    let lock = record(&programs);
    lock.verify(&evidence(&programs)).unwrap();
    lock.require_cross_backend().unwrap();
    let text = lock.to_toml().unwrap();
    assert_eq!(Stage1Lock::parse(text.as_bytes()).unwrap(), lock);
    assert_eq!(
        Stage1Lock::parse(text.as_bytes())
            .unwrap()
            .to_toml()
            .unwrap(),
        text
    );

    let mut changed = [program(Ok(b"verified"))];
    changed[0].verified.as_mut().unwrap().result = 43;
    assert!(matches!(
        Stage1Lock::record("target", &evidence(&changed)),
        Err(Stage1Error::ResultMismatch { .. })
    ));
    changed[0].verified.as_mut().unwrap().result = 42;
    changed[0].cranelift.result = 43;
    assert!(matches!(
        Stage1Lock::record("target", &evidence(&changed)),
        Err(Stage1Error::ResultMismatch { .. })
    ));
}

#[test]
fn refusal_is_recorded_but_never_certifies_a_cross_backend_check() {
    let programs = [program(Err("unsupported Enum"))];
    let lock = record(&programs);
    lock.verify(&evidence(&programs)).unwrap();
    assert_eq!(
        lock.require_cross_backend(),
        Err(Stage1Error::BackendUnavailable {
            program: "fib".into(),
            reason: "unsupported Enum".into()
        })
    );
    let mut forged = [program(Ok(b"verified"))];
    assert!(matches!(
        lock.verify(&evidence(&forged)),
        Err(Stage1Error::DigestMismatch { .. })
    ));
    forged[0].verified = Err("different refusal");
    assert!(matches!(
        lock.verify(&evidence(&forged)),
        Err(Stage1Error::DigestMismatch { .. })
    ));
}

#[test]
fn changed_seed_parent_sources_corpus_and_artifacts_are_typed_failures() {
    let programs = [program(Ok(b"verified"))];
    let lock = record(&programs);
    let mut e = evidence(&programs);
    e.seed_binary = b"changed seed";
    assert_eq!(
        lock.verify(&e),
        Err(Stage1Error::DigestMismatch {
            field: "seed_binary".into()
        })
    );
    let parent = [PARENT, b"\n# a different parent\n"].concat();
    let mut e = evidence(&programs);
    e.parent_lock = &parent;
    assert_eq!(
        lock.verify(&e),
        Err(Stage1Error::DigestMismatch {
            field: "parent_seed_lock".into()
        })
    );
    let mut e = evidence(&programs);
    e.frontend_sources = &[("src/parser.ves", b"changed")];
    assert_eq!(
        lock.verify(&e),
        Err(Stage1Error::DigestMismatch {
            field: "frontend_sources".into()
        })
    );
    for change in 0..4 {
        let mut changed = [program(Ok(b"verified"))];
        match change {
            0 => changed[0].name = "gcd",
            1 => changed[0].source = b"different source",
            2 => changed[0].cranelift.artifact = b"tampered",
            _ => changed[0].verified.as_mut().unwrap().artifact = b"tampered",
        }
        assert!(matches!(
            lock.verify(&evidence(&changed)),
            Err(Stage1Error::CorpusMismatch | Stage1Error::DigestMismatch { .. })
        ));
    }
}

#[test]
fn malformed_empty_duplicate_and_over_budget_evidence_is_rejected() {
    assert!(matches!(
        Stage1Lock::parse(&vec![b' '; MAX_BYTES + 1]),
        Err(Stage1Error::TooLarge)
    ));
    assert!(matches!(
        Stage1Lock::parse(&[0xff]),
        Err(Stage1Error::Syntax(_))
    ));
    let programs = [program(Ok(b"verified"))];
    let lock = record(&programs);
    let unknown = format!("unknown = 1\n{}", lock.to_toml().unwrap());
    assert!(matches!(
        Stage1Lock::parse(unknown.as_bytes()),
        Err(Stage1Error::Syntax(_))
    ));
    assert!(Stage1Lock::record("target", &evidence(&[])).is_err());
    let duplicates = [program(Ok(b"one")), program(Ok(b"two"))];
    assert!(Stage1Lock::record("target", &evidence(&duplicates)).is_err());
    let mut e = evidence(&programs);
    e.parent_lock = b"not a seed lock";
    assert!(matches!(
        Stage1Lock::record("target", &e),
        Err(Stage1Error::ParentInvalid(_))
    ));
    let mut e = evidence(&programs);
    e.seed_binary = b"";
    assert!(Stage1Lock::record("target", &e).is_err());
    let mut p = [program(Ok(b""))];
    assert!(Stage1Lock::record("target", &evidence(&p)).is_err());
    p[0].verified = Err("");
    assert!(Stage1Lock::record("target", &evidence(&p)).is_err());
    let many: Vec<_> = (0..=vesper_codegen_verified::stage1::MAX_PROGRAMS)
        .map(|_| program(Ok(b"object")))
        .collect();
    assert!(matches!(
        Stage1Lock::record("target", &evidence(&many)),
        Err(Stage1Error::InvalidField("program count"))
    ));
}

#[test]
fn cli_never_certifies_an_unavailable_backend() {
    use std::process::Command;
    let path = std::env::temp_dir().join(format!("stage1-cli-{}.lock", std::process::id()));
    for (verified, accepted) in [
        (Ok(b"verified".as_slice()), true),
        (Err("unsupported Effect"), false),
    ] {
        let programs = [program(verified)];
        std::fs::write(&path, record(&programs).to_toml().unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_vesper-bootstrap"))
            .arg("check-stage1-lock")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), accepted);
        if !accepted {
            assert!(String::from_utf8_lossy(&output.stderr).contains("BackendUnavailable"));
        }
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn outcome_ratchet_reports_staleness_and_improvement_but_rejects_regression() {
    let refused = record(&[program(Err("unsupported Effect"))]);
    let built = record(&[program(Ok(b"verified"))]);
    // Same measurement: current.
    assert!(refused.compare_outcome(&refused).unwrap().is_current());
    // A new verified build is an improvement, never a failure.
    let report = refused.compare_outcome(&built).unwrap();
    assert_eq!(report.improved, ["fib"]);
    assert!(report.stale.is_empty());
    // Losing it is a typed regression.
    assert_eq!(
        built.compare_outcome(&refused),
        Err(Stage1Error::Regression {
            program: "fib".into(),
            reason: "unsupported Effect".into()
        })
    );
    // Another seed, other frontend bytes, other artifacts or results: stale only.
    let mut other = [program(Ok(b"verified again"))];
    other[0].cranelift.artifact = b"cranelift 2";
    other[0].seed_result = 7;
    other[0].cranelift.result = 7;
    other[0].verified.as_mut().unwrap().result = 7;
    let sources: &[(&str, &[u8])] = &[("src/parser.ves", b"frontend 2")];
    let fresh = Stage1Lock::record(
        "x86_64-unknown-linux-gnu",
        &Stage1Evidence {
            seed_binary: b"seed 2",
            frontend_sources: sources,
            ..evidence(&other)
        },
    )
    .unwrap();
    let report = built.compare_outcome(&fresh).unwrap();
    assert!(report.improved.is_empty());
    assert_eq!(
        report.stale,
        [
            "seed_binary",
            "frontend_sources",
            "fib.result",
            "fib.cranelift_artifact",
            "fib.verified"
        ]
    );
    // Another target or corpus is a hard failure.
    let elsewhere =
        Stage1Lock::record("aarch64-unknown-linux-gnu", &evidence(&[program(Ok(b"v"))])).unwrap();
    assert!(matches!(
        built.compare_outcome(&elsewhere),
        Err(Stage1Error::TargetMismatch { .. })
    ));
    let mut renamed = [program(Ok(b"verified"))];
    renamed[0].name = "gcd";
    assert_eq!(
        built.compare_outcome(&record(&renamed)),
        Err(Stage1Error::CorpusMismatch)
    );
    let mut edited = [program(Ok(b"verified"))];
    edited[0].source = b"fn main() -> i64 { 2 }";
    assert_eq!(
        built.compare_outcome(&record(&edited)),
        Err(Stage1Error::CorpusMismatch)
    );
}

#[test]
fn cli_outcome_ratchet_fails_closed_on_regression() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("stage1-outcome-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let refused = dir.join("refused.lock");
    let built = dir.join("built.lock");
    std::fs::write(
        &refused,
        record(&[program(Err("unsupported Effect"))])
            .to_toml()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &built,
        record(&[program(Ok(b"verified"))]).to_toml().unwrap(),
    )
    .unwrap();
    let run = |reference: &std::path::Path, fresh: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_vesper-bootstrap"))
            .arg("compare-stage1-outcome")
            .arg(reference)
            .arg(fresh)
            .output()
            .unwrap()
    };
    let improved = run(&refused, &built);
    assert!(improved.status.success());
    assert!(String::from_utf8_lossy(&improved.stdout).contains("improved: fib"));
    let regressed = run(&built, &refused);
    assert!(!regressed.status.success());
    assert!(String::from_utf8_lossy(&regressed.stderr).contains("Regression"));
    std::fs::remove_dir_all(dir).unwrap();
}
