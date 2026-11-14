//! Stage 0 seed reproducibility record: manifests, transcripts and the seed lock.

use vesper_codegen_verified::seed::*;

#[test]
fn identical_objects_pass_and_a_changed_object_is_named() {
    let a = ObjectManifest::from_objects(&[("fib", b"obj1"), ("gcd", b"obj2")]).unwrap();
    let b = ObjectManifest::from_objects(&[("gcd", b"obj2"), ("fib", b"obj1")]).unwrap();
    assert_eq!(a.digest(), b.digest());
    a.require_identical(&b).unwrap();
    let c = ObjectManifest::from_objects(&[("fib", b"obj1"), ("gcd", b"obj2-nondeterministic")])
        .unwrap();
    assert_eq!(
        a.require_identical(&c),
        Err(SeedError::ObjectNotReproducible {
            program: "gcd".into()
        })
    );
    let d = ObjectManifest::from_objects(&[("fib", b"obj1")]).unwrap();
    assert_eq!(a.require_identical(&d), Err(SeedError::CorpusShapeMismatch));
    assert!(ObjectManifest::from_objects(&[("fib", b"x"), ("fib", b"x")]).is_err());
    assert!(ObjectManifest::from_objects(&[("../escape", b"x")]).is_err());
}

#[test]
fn transcripts_must_agree_program_by_program() {
    let a = ResultTranscript::parse(b"fib 233\ngcd 21\n").unwrap();
    let b = ResultTranscript::parse(b"gcd 21\nfib 233\n").unwrap();
    a.require_agreement(&b).unwrap();
    assert_eq!(a.digest(), b.digest());
    let c = ResultTranscript::parse(b"fib 233\ngcd 20\n").unwrap();
    assert_eq!(
        a.require_agreement(&c),
        Err(SeedError::PathResultMismatch {
            program: "gcd".into(),
            path_a: "21".into(),
            path_b: "20".into(),
        })
    );
    for bad in [
        &b"fib\n"[..],
        b"fib 1\nfib 2\n",
        b"f\xffb 1\n",
        b"fib two words\n",
    ] {
        assert!(ResultTranscript::parse(bad).is_err(), "{bad:?}");
    }
}

fn lock() -> SeedLock {
    let d = |s: &[u8]| format!("blake3:{}", blake3::hash(s).to_hex());
    SeedLock {
        seed: SeedBuild {
            schema: SEED_SCHEMA.into(),
            base_commit: "1094e6400000000000000000000000000000abcd".into(),
            rustc: "rustc 1.100.0".into(),
            cargo_profile: "release".into(),
            source_date_epoch: 1,
            corpus_programs: 10,
        },
        reproducibility: Reproducibility {
            seed_binary_a: d(b"a"),
            seed_binary_b: d(b"a"),
            seed_binary_identical: true,
            corpus_objects_a: d(b"o"),
            corpus_objects_b: d(b"o"),
            corpus_objects_identical: true,
        },
        ddc_semantic: DdcSemantic {
            path_a_results: d(b"r"),
            path_b_results: d(b"r"),
            path_b_lean: "Lean 4.24.0".into(),
            path_b_c_compiler: "gcc 13.3.0".into(),
            results_agree: true,
        },
    }
}

#[test]
fn seed_lock_round_trips_and_rejects_contradictions() {
    let good = lock();
    assert_eq!(SeedLock::parse(good.to_toml().as_bytes()).unwrap(), good);

    let mut l = lock();
    l.reproducibility.corpus_objects_b = format!("blake3:{}", blake3::hash(b"other").to_hex());
    assert_eq!(
        l.validate(),
        Err(SeedError::InconsistentRecord {
            field: "corpus_objects_identical"
        })
    );
    l.reproducibility.corpus_objects_identical = false;
    assert!(matches!(
        l.validate(),
        Err(SeedError::ObjectNotReproducible { .. })
    ));

    let mut l = lock();
    l.ddc_semantic.path_b_results = format!("blake3:{}", blake3::hash(b"other").to_hex());
    l.ddc_semantic.results_agree = false;
    assert!(matches!(
        l.validate(),
        Err(SeedError::PathResultMismatch { .. })
    ));

    // A differing seed binary is recorded, not a gate failure, as long as the
    // flag matches the digests.
    let mut l = lock();
    l.reproducibility.seed_binary_b = format!("blake3:{}", blake3::hash(b"b").to_hex());
    l.reproducibility.seed_binary_identical = false;
    l.validate().unwrap();

    let text = good
        .to_toml()
        .replace("schema = ", "unknown = 1\nschema = ");
    assert!(matches!(
        SeedLock::parse(text.as_bytes()),
        Err(SeedError::Syntax { .. })
    ));
}
