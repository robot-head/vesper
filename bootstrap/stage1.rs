//! Root-package adapter for Stage 1. Kept outside the auditor library so that
//! the library never depends on the compiler facade.
#![forbid(unsafe_code)]
use std::path::Path;
use vesper::{check, lexer, native, parser, runtime::Value, verified_codegen as verified};
use vesper_codegen_verified::stage1::{
    FrontendOutput, ProgramEvidence, ResultKind, Stage1Evidence, Stage1Lock,
};

const FRONTEND: [&str; 6] = [
    "packages/vesper-compiler/Loom.toml",
    "packages/vesper-compiler/src/lexer.ves",
    "packages/vesper-compiler/src/cursor.ves",
    "packages/vesper-compiler/src/types_items.ves",
    "packages/vesper-compiler/src/expressions.ves",
    "packages/vesper-compiler/src/driver.ves",
];

fn mix(h: i64, x: i64) -> i64 {
    let m = h.wrapping_add(x).wrapping_mul(0x100000001b3);
    m.wrapping_add(m / 2147483648)
}
fn seed_cst(source: &str) -> Result<i64, String> {
    fn events(node: &parser::SyntaxNode, h: &mut i64) {
        let range = node.text_range();
        *h = mix(*h, node.kind().0 as i64 + 1000);
        *h = mix(*h, u32::from(range.start()) as i64);
        for child in node.children() {
            events(&child, h);
        }
        *h = mix(*h, 3000);
        *h = mix(*h, u32::from(range.end()) as i64);
    }
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("seed CST: {:?}", parsed.diagnostics));
    }
    let mut h = 0xcbf29ce484222325_u64 as i64;
    events(&parsed.syntax(), &mut h);
    Ok(if h < 0 { (-1i64).wrapping_sub(h) } else { h })
}

/// Same bounded byte adapter as tests/self_host_front_end.rs: 64-byte leaves
/// and eight-way internal nodes, without modifying the owned frontend source.
fn source_module(bytes: &[u8]) -> String {
    const EFFECTS: &str = "eff [Fail[RuntimeError], Fail[ArithOverflow]]";
    fn tree(group: &[(String, usize)]) -> String {
        if group.len() == 1 {
            return format!("{}(i)", group[0].0);
        }
        let mid = group.len() / 2;
        format!(
            "if i < {} {{ {} }} else {{ {} }}",
            group[mid].1,
            tree(&group[..mid]),
            tree(&group[mid..])
        )
    }
    let mut out = String::from("\n");
    let mut nodes = Vec::new();
    for (k, chunk) in bytes.chunks(64).enumerate() {
        let mut values: Vec<_> = chunk.iter().map(u8::to_string).collect();
        values.resize(64, "0".into());
        let name = format!("source_leaf_{k}");
        out.push_str(&format!(
            "fn {name}(i: i64) -> i64 {EFFECTS} {{ ({})[i - {}] }}\n",
            values.join(","),
            k * 64
        ));
        nodes.push((name, k * 64));
    }
    let mut level = 0;
    while nodes.len() > 1 {
        let mut next = Vec::new();
        for (i, group) in nodes.chunks(8).enumerate() {
            let name = format!("source_node_{level}_{i}");
            out.push_str(&format!(
                "fn {name}(i: i64) -> i64 {EFFECTS} {{ {} }}\n",
                tree(group)
            ));
            next.push((name, group[0].1));
        }
        nodes = next;
        level += 1;
    }
    let top = nodes
        .first()
        .map_or_else(|| "-1".into(), |n| format!("{}(i)", n.0));
    let n = bytes.len();
    out.push_str(&format!(
        "fn source_len() -> i64 {{ {n} }}\n\
         fn source_byte(i: i64) -> i64 {{ if i < 0 || i >= {n} {{ -1 }} else {{ \
         handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ {top} }} with {{ \
         return v => v, raise(e, r) => -1, }} }} with {{ return v => v, raise(e, r) => -1, }} }} }}\n"
    ));
    out
}

struct Measured {
    name: String,
    kind: ResultKind,
    source: Vec<u8>,
    seed_cst: i64,
    object: Vec<u8>,
    cst: i64,
    verified: Result<(Vec<u8>, i64), String>,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn measure(root: &Path, work: &Path, target: &str, seed: &[u8]) -> Result<Stage1Lock, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let sources: Vec<_> = FRONTEND
        .iter()
        .map(|p| read(&root.join(p)))
        .collect::<Result<_, _>>()?;
    let frontend = sources[1..]
        .iter()
        .map(|b| std::str::from_utf8(b).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    let mut corpus: Vec<_> = std::fs::read_dir(root.join("bootstrap/corpus"))
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    corpus.retain(|p| p.extension().is_some_and(|e| e == "ves"));
    corpus.sort();
    let mut cases = Vec::new();
    for path in corpus {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("corpus name")?
            .to_owned();
        let source = read(&path)?;
        let expected = seed_cst(std::str::from_utf8(&source).map_err(|e| e.to_string())?)?;
        cases.push((name, source, expected, "parse_main", ResultKind::CstDigest));
    }
    let probe = read(&root.join("bootstrap/byte-class-probe.ves"))?;
    let mut checksum = 0i64;
    for i in -1i64..=256 {
        let digit = (48..=57).contains(&i);
        let upper = (65..=90).contains(&i);
        let lower = (97..=122).contains(&i) || i == 95;
        let hex = digit || (65..=70).contains(&i) || (97..=102).contains(&i) || i == 95;
        let space = [32, 9, 10, 13, 12].contains(&i);
        let bits = i64::from(digit)
            + 2 * i64::from(upper)
            + 4 * i64::from(lower)
            + 8 * i64::from(digit || upper || lower)
            + 16 * i64::from(hex)
            + 32 * i64::from(space);
        checksum = checksum.wrapping_mul(131).wrapping_add(bits);
    }
    cases.push((
        "lexer-byte-classes".into(),
        probe,
        checksum,
        "byte_class_probe",
        ResultKind::LexerByteClasses,
    ));
    let mut measured = Vec::new();
    for (name, source, expected, entry, kind) in cases {
        let text = if kind == ResultKind::CstDigest {
            format!("{frontend}{}", source_module(&source))
        } else {
            format!(
                "{frontend}{}{}",
                source_module(&[]),
                std::str::from_utf8(&source).map_err(|e| e.to_string())?
            )
        };
        let limits = parser::ParseLimits {
            max_source_bytes: lexer::HARD_SOURCE_BYTES,
            max_tokens: lexer::HARD_TOKENS,
            ..Default::default()
        };
        let mut parsed = parser::parse_with_limits(&text, limits).map_err(|e| format!("{e:?}"))?;
        if !parsed.diagnostics.is_empty() {
            return Err(format!("{name}: {:?}", parsed.diagnostics));
        }
        let checked = check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(format!("{name}: {:?}", checked.diagnostics));
        }
        // The public optimizer installs candidates only after the root's actual
        // bounded rewrite-certificate checker accepts them. No duplicate checker.
        vesper::optimize::optimize(&mut parsed.module, &checked.expression_types);
        let checked = check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(format!("optimized {name}: {:?}", checked.diagnostics));
        }
        let (object, _) = native::object_with_options(&checked.world, entry, Some(target), false)?;
        std::fs::write(work.join(format!("{name}.cranelift.o")), &object)
            .map_err(|e| e.to_string())?;
        let Value::Int(cst) = native::jit_with_options(&checked.world, entry, false)? else {
            return Err(format!("{name}: frontend did not return i64"));
        };
        let options = verified::Options {
            protocol: verified::ResultProtocol::Words,
            optimize: false,
            ..verified::Options::new(
                verified::Target::from_triple(target).map_err(|e| e.to_string())?,
            )
        };
        let verification = match verified::compile(&checked.world, entry, &options) {
            Err(e @ verified::VerifiedError::Unsupported { .. }) => Err(e.to_string()),
            Err(e) => return Err(e.to_string()),
            Ok(artifact) => {
                let executable = work.join(format!("{name}.verified"));
                match artifact.container {
                    verified::Container::ElfExecutable => {
                        std::fs::write(&executable, &artifact.bytes).map_err(|e| e.to_string())?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &executable,
                                std::fs::Permissions::from_mode(0o755),
                            )
                            .map_err(|e| e.to_string())?;
                        }
                    }
                    verified::Container::MachOObject => {
                        return Err("verified Words protocol needs an executable container".into());
                    }
                }
                let output = std::process::Command::new(&executable)
                    .output()
                    .map_err(|e| e.to_string())?;
                if !output.status.success() || output.stdout.len() != 8 {
                    return Err(format!("{name}: invalid verified output"));
                }
                let mut word = [0; 8];
                word.copy_from_slice(&output.stdout);
                Ok((artifact.bytes, i64::from_le_bytes(word)))
            }
        };
        println!(
            "{name}: {kind:?}, seed {expected}, Cranelift {cst}, verified {}",
            if verification.is_ok() {
                "built"
            } else {
                "unavailable"
            }
        );
        measured.push(Measured {
            name,
            kind,
            source,
            seed_cst: expected,
            object,
            cst,
            verified: verification,
        });
    }
    let programs: Vec<_> = measured
        .iter()
        .map(|m| ProgramEvidence {
            name: &m.name,
            kind: m.kind,
            source: &m.source,
            seed_result: m.seed_cst,
            cranelift: FrontendOutput {
                artifact: &m.object,
                result: m.cst,
            },
            verified: match &m.verified {
                Ok((b, cst)) => Ok(FrontendOutput {
                    artifact: b,
                    result: *cst,
                }),
                Err(e) => Err(e.as_str()),
            },
        })
        .collect();
    let parent = read(&root.join("bootstrap/stage0_seed.lock"))?;
    let adapter = read(&root.join("bootstrap/stage1.rs"))?;
    let mut inputs: Vec<_> = FRONTEND
        .iter()
        .zip(&sources)
        .map(|(p, b)| (*p, b.as_slice()))
        .collect();
    inputs.push(("bootstrap/stage1.rs", adapter.as_slice()));
    Stage1Lock::record(
        target,
        &Stage1Evidence {
            parent_lock: &parent,
            seed_binary: seed,
            frontend_sources: &inputs,
            programs: &programs,
        },
    )
    .map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        return Err(
            "usage: stage1 ROOT WORK TARGET OUT [--record] [--require-cross-backend]".into(),
        );
    }
    if args[4..]
        .iter()
        .any(|s| s != "--record" && s != "--require-cross-backend")
    {
        return Err("unknown argument".into());
    }
    let root = Path::new(&args[0]);
    let work = Path::new(&args[1]);
    let out = Path::new(&args[3]);
    // This executable contains the root Rust compiler facade and is the seed
    // that actually performs this build. Do not hash an unrelated CLI binary.
    let seed = read(&std::env::current_exe().map_err(|e| e.to_string())?)?;
    // Check retained artifacts before regeneration can overwrite them. A clean
    // work directory is allowed; every rebuilt artifact is compared below.
    let prior = if args.iter().any(|s| s == "--record") {
        None
    } else {
        let prior = Stage1Lock::parse(&read(out)?).map_err(|e| e.to_string())?;
        if prior.seed_binary != vesper_codegen_verified::stage1::digest(&seed) {
            return Err("DigestMismatch { field: seed_binary }".into());
        }
        for p in &prior.programs {
            let artifact = work.join(format!("{}.cranelift.o", p.name));
            if artifact.exists()
                && vesper_codegen_verified::stage1::digest(&read(&artifact)?)
                    != p.cranelift_artifact
            {
                return Err(format!(
                    "DigestMismatch {{ field: {}.cranelift_artifact }}",
                    p.name
                ));
            }
            if let vesper_codegen_verified::stage1::VerifiedRecord::Built { artifact: digest } =
                &p.verified
            {
                let artifact = work.join(format!("{}.verified", p.name));
                if artifact.exists()
                    && vesper_codegen_verified::stage1::digest(&read(&artifact)?) != *digest
                {
                    return Err(format!(
                        "DigestMismatch {{ field: {}.verified_artifact }}",
                        p.name
                    ));
                }
            }
        }
        Some(prior)
    };
    let lock = measure(root, work, &args[2], &seed)?;
    let text = lock.to_toml().map_err(|e| e.to_string())?;
    if prior.is_some_and(|prior| prior != lock) {
        return Err("DigestMismatch { field: stage1 record }".into());
    }
    if args.iter().any(|s| s == "--require-cross-backend") {
        lock.require_cross_backend().map_err(|e| e.to_string())?;
    }
    // Publish only after every available comparison and any requested strict gate.
    std::fs::write(out, text).map_err(|e| e.to_string())?;
    if let Err(e) = lock.require_cross_backend() {
        println!("recorded Cranelift frontend evidence; cross-backend gate: {e}");
    }
    Ok(())
}
fn main() {
    let result = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(run)
        .map_err(|e| e.to_string())
        .and_then(|t| t.join().map_err(|_| "stage1 worker panicked".into()))
        .and_then(|r| r);
    if let Err(e) = result {
        eprintln!("stage1: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[test]
    fn cst_digest_matches_real_frontend_over_bootstrap_corpus() {
        let root =
            std::env::var_os("VESPER_ROOT").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let work = std::env::temp_dir().join(format!("stage1-corpus-{}", std::process::id()));
        let target = if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        };
        let root2 = root.clone();
        let work2 = work.clone();
        let lock = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || measure(&root2, &work2, target, b"CI seed identity"))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(lock.programs.len(), 11);
        let probe = lock
            .programs
            .iter()
            .find(|p| p.kind == ResultKind::LexerByteClasses)
            .unwrap();
        assert!(matches!(
            probe.verified,
            vesper_codegen_verified::stage1::VerifiedRecord::Built { .. }
        ));
        assert!(matches!(
            lock.require_cross_backend(),
            Err(vesper_codegen_verified::stage1::Stage1Error::BackendUnavailable { .. })
        ));
        std::fs::remove_dir_all(work).unwrap();
    }
}
