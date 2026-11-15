use vesper_codegen_verified::stage1::{
    FrontendOutput, MAX_BYTES, ProgramEvidence, ResultKind, Stage1Error, Stage1Evidence, Stage1Lock,
};

const PARENT: &[u8] = include_bytes!("../../../bootstrap/stage0_seed.lock");
const SOURCES: &[(&str, &[u8])] = &[("src/parser.ves", b"frontend")];

#[test]
fn committed_record_binds_parent_and_distinguishes_parser_from_supported_probe() {
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
    assert!(matches!(
        lock.require_cross_backend(),
        Err(Stage1Error::BackendUnavailable { .. })
    ));
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
