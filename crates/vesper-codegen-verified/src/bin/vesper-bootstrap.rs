//! Bootstrap driver helper used by `bootstrap/run.sh`.
//!
//! - `seed-lock --base-commit C --rustc V --profile P --epoch N --seed-a F --seed-b F
//!   --objects-a DIR --objects-b DIR --results-a F --results-b F --lean V --cc V --out F`
//!   compares the two seed builds and the two paths, writes the seed lock, and
//!   exits 1 if a gate fails (the lock is still written for the record, except
//!   when a result transcript does not cover exactly the compiled programs).
//! - `check-seed-lock FILE` validates a seed lock.
//! - `check-lock FILE` validates a §42.2 `stage3_ddc.lock`.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use vesper_codegen_verified::parse_lock;
use vesper_codegen_verified::seed::{
    DdcSemantic, ObjectManifest, Reproducibility, ResultTranscript, SEED_SCHEMA, SeedBuild,
    SeedLock,
};

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn objects(dir: &Path) -> Result<ObjectManifest, String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|e| e == "o") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("non-UTF-8 object name")?
                .to_owned();
            files.push((name, read(&path)?));
        }
    }
    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    ObjectManifest::from_objects(&borrowed).map_err(|e| e.to_string())
}

fn seed_lock(args: &[String]) -> Result<bool, String> {
    let mut opts = BTreeMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let key = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument {flag}"))?;
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if opts.insert(key.to_owned(), value.clone()).is_some() {
            return Err(format!("duplicate {flag}"));
        }
    }
    let get = |k: &str| opts.get(k).cloned().ok_or_else(|| format!("missing --{k}"));
    let path = |k: &str| get(k).map(PathBuf::from);
    let seed_a = blake3::hash(&read(&path("seed-a")?)?);
    let seed_b = blake3::hash(&read(&path("seed-b")?)?);
    let objects_a = objects(&path("objects-a")?)?;
    let objects_b = objects(&path("objects-b")?)?;
    let results_a =
        ResultTranscript::parse(&read(&path("results-a")?)?).map_err(|e| e.to_string())?;
    let results_b =
        ResultTranscript::parse(&read(&path("results-b")?)?).map_err(|e| e.to_string())?;
    let mut ok = true;
    // Both transcripts must cover exactly the compiled programs, so empty or
    // partial semantic evidence cannot certify the corpus.
    let compiled: Vec<&str> = objects_a
        .entries
        .iter()
        .map(|e| e.program.as_str())
        .collect();
    for (label, transcript) in [("Path A", &results_a), ("Path B", &results_b)] {
        let names: Vec<&str> = transcript.lines.iter().map(|l| l.0.as_str()).collect();
        if names != compiled {
            // No lock is written: it would record an agreement nobody checked.
            eprintln!("{label} results do not cover exactly the compiled corpus programs");
            return Ok(false);
        }
    }
    if let Err(e) = objects_a.require_identical(&objects_b) {
        eprintln!("reproducibility gate: {e}");
        ok = false;
    }
    if let Err(e) = results_a.require_agreement(&results_b) {
        eprintln!("Path A / Path B gate: {e}");
        ok = false;
    }
    let lock = SeedLock {
        seed: SeedBuild {
            schema: SEED_SCHEMA.into(),
            base_commit: get("base-commit")?,
            rustc: get("rustc")?,
            cargo_profile: get("profile")?,
            source_date_epoch: get("epoch")?.parse().map_err(|_| "bad --epoch")?,
            corpus_programs: u32::try_from(objects_a.entries.len()).map_err(|_| "corpus")?,
        },
        reproducibility: Reproducibility {
            seed_binary_a: format!("blake3:{}", seed_a.to_hex()),
            seed_binary_b: format!("blake3:{}", seed_b.to_hex()),
            seed_binary_identical: seed_a == seed_b,
            corpus_objects_a: format!("blake3:{}", objects_a.digest().to_hex()),
            corpus_objects_b: format!("blake3:{}", objects_b.digest().to_hex()),
            corpus_objects_identical: objects_a.digest() == objects_b.digest(),
        },
        ddc_semantic: DdcSemantic {
            path_a_results: format!("blake3:{}", results_a.digest().to_hex()),
            path_b_results: format!("blake3:{}", results_b.digest().to_hex()),
            path_b_lean: get("lean")?,
            path_b_c_compiler: get("cc")?,
            results_agree: results_a.digest() == results_b.digest(),
        },
    };
    std::fs::write(path("out")?, lock.to_toml()).map_err(|e| e.to_string())?;
    println!(
        "seed binaries identical: {}; corpus objects identical: {}; Path A/B agree: {}",
        lock.reproducibility.seed_binary_identical,
        lock.reproducibility.corpus_objects_identical,
        lock.ddc_semantic.results_agree
    );
    if let Err(e) = lock.validate() {
        eprintln!("seed lock: {e}");
        ok = false;
    }
    Ok(ok)
}

fn run(args: &[String]) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("seed-lock") => seed_lock(&args[1..]),
        Some("check-seed-lock") if args.len() == 2 => {
            SeedLock::parse(&read(Path::new(&args[1]))?).map_err(|e| e.to_string())?;
            println!("seed lock ok");
            Ok(true)
        }
        Some("check-lock") if args.len() == 2 => {
            let lock = parse_lock(&read(Path::new(&args[1]))?).map_err(|e| e.to_string())?;
            println!("lock ok: fixed point {}", lock.quorum.path_a_stage3);
            Ok(true)
        }
        _ => Err(
            "usage: vesper-bootstrap seed-lock ... | check-seed-lock FILE | check-lock FILE".into(),
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("vesper-bootstrap: {e}");
            ExitCode::from(2)
        }
    }
}
