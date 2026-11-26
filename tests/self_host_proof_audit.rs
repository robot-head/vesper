//! `@vesper/proof-audit` (packages/vesper-proof-audit, SH5 lane 18, #1341)
//! against `vesper proof-audit` (src/proof_commands.rs).
//!
//! A small executable built from the package runs each case beside the Rust
//! command; stdout, stderr and the exit status must be byte-identical (the
//! private staging directory's name, which holds a process id, is masked).
//! The cases cover the option errors, a Lean that cannot start, and, with
//! Lean 4.35.0 (`VESPER_LEAN`), real kernel replays: accepted and rejected
//! theorems, axiom policies, missing artifacts, symlinks, forged artifacts
//! and the macro provenance gate on `vesper forge lean --provenance` output.
//!
//! Unix only for now: the driver executable links @vesper-std/hash-sys's C
//! BLAKE3, and the Windows runners have no Lean for the real cases.
#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const PACKAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/packages/vesper-proof-audit");
const AUDITOR_LEAN: &str = include_str!("../tools/proof-audit/Audit.lean");
const MIXED: &str = include_str!("fixtures/source_macros/provenance_mixed.ves");

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-self-host-proof-audit-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The generated `src/auditor.ves`: the auditor as one Vesper string
/// literal (Vesper string literals are JSON strings).
fn auditor_source() -> String {
    format!(
        "// GENERATED from tools/proof-audit/Audit.lean (the `AUDITOR` of\n\
         // src/proof_commands.rs); tests/self_host_proof_audit.rs checks it is\n\
         // current and rewrites it with VESPER_PROOF_AUDIT_BLESS=1.\n\
         pub const AUDITOR: Str = {};\n",
        serde_json::to_string(AUDITOR_LEAN).unwrap()
    )
}

#[test]
fn auditor_text_is_current() {
    let path = Path::new(PACKAGE).join("src/auditor.ves");
    let expected = auditor_source();
    if std::env::var_os("VESPER_PROOF_AUDIT_BLESS").is_some() {
        fs::write(&path, &expected).unwrap();
        return;
    }
    assert!(
        fs::read_to_string(&path).unwrap() == expected,
        "{} is stale; rerun with VESPER_PROOF_AUDIT_BLESS=1",
        path.display()
    );
}

const DRIVER: &str = r#"use @vesper/proof-audit::run;
use @vesper/proof-audit::strs::{
    push,
    put_str,
    strs_new
};
use @vesper-std/env::{
    arg_bytes,
    arg_count
};
use @vesper-std/fs::{
    IoError,
    write_stderr,
    write_stdout
};

pub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv], Ffi[StdProcess], Ffi[Blake3]] {
    let mut args = strs_new();
    for i in 1..arg_count() {
        match arg_bytes(i) {
            Option::Some(a) => push(inout args, in a.as_slice()),
            Option::None => {},
        }
    }
    let o = run(in args);
    let mut err: ChunkSeq[u8] = ChunkSeq::new[u8]();
    if o.failed {
        put_str(inout err, in "vesper: ");
        err.extend(in o.err.as_slice());
        err.push(10);
    }
    handle Fail[IoError] in {
        if o.out.len() > 0 {
            write_stdout(in o.out.as_slice());
        }
        if err.len() > 0 {
            write_stderr(in err.as_slice());
        }
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    if o.failed {
        1
    } else {
        0
    }
}
"#;

/// Build the package's driver executable into `dir`.
fn build_port(dir: &Path) -> PathBuf {
    let package = dir.join("driver");
    fs::create_dir_all(package.join("src")).unwrap();
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/vesper-std");
    fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/proof-audit-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\
             \"@vesper/proof-audit\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/env\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {} }}\n",
            serde_json::to_string(PACKAGE).unwrap(),
            serde_json::to_string(std.join("env").to_str().unwrap()).unwrap(),
            serde_json::to_string(std.join("fs").to_str().unwrap()).unwrap(),
        ),
    )
    .unwrap();
    fs::write(package.join("src/main.ves"), DRIVER).unwrap();
    let binary = dir.join("proof-audit-port");
    let output = vesper()
        .arg("build")
        .arg(&package)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

/// serde_json's own error text (`EOF while parsing an object at line 1
/// column 1`), which the port does not reproduce: it names the field or says
/// the text is not JSON. These errors need a hand-corrupted manifest or
/// auditor record; the test still requires both sides to fail there.
const SERDE_DETAIL: [&[u8]; 2] = [
    b"invalid macro provenance manifest: ",
    b"invalid Lean audit result: ",
];

/// `text` with every staging directory name made `vesper-proof-audit-N-N`
/// and serde_json's error detail cut.
fn mask(text: &[u8]) -> Vec<u8> {
    let text = &cut_serde_detail(text);
    let needle = b"vesper-proof-audit-";
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(needle) {
            out.extend_from_slice(needle);
            i += needle.len();
            for _ in 0..2 {
                let digits = text[i..].iter().take_while(|b| b.is_ascii_digit()).count();
                if digits > 0 {
                    out.push(b'N');
                    i += digits;
                }
                if text.get(i) == Some(&b'-') {
                    out.push(b'-');
                    i += 1;
                }
            }
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

fn cut_serde_detail(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if let Some(lead) = SERDE_DETAIL.iter().find(|lead| text[i..].starts_with(lead)) {
            out.extend_from_slice(lead);
            out.extend_from_slice(b"<detail>");
            i += lead.len();
            while i < text.len() && text[i] != b'"' && text[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

struct Runner {
    port: PathBuf,
    cases: usize,
    rust_time: Duration,
    port_time: Duration,
}
impl Runner {
    fn run(command: &mut Command) -> (Output, Duration) {
        let started = Instant::now();
        let output = command.output().unwrap();
        (output, started.elapsed())
    }
    /// Run `proof-audit args` both ways; they must agree.
    fn case(&mut self, args: &[&str]) -> Output {
        let (rust, rust_time) = Self::run(vesper().arg("proof-audit").args(args));
        let (port, port_time) = Self::run(Command::new(&self.port).args(args));
        self.cases += 1;
        self.rust_time += rust_time;
        self.port_time += port_time;
        let show = |o: &Output| {
            format!(
                "status {:?}\nstdout:\n{}\nstderr:\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        };
        assert!(
            rust.status.code() == port.status.code()
                && mask(&rust.stdout) == mask(&port.stdout)
                && mask(&rust.stderr) == mask(&port.stderr),
            "proof-audit {args:?}\n--- Rust\n{}\n--- Vesper\n{}",
            show(&rust),
            show(&port)
        );
        rust
    }
}

fn resolve_lean(lean: String) -> String {
    let Ok(output) = Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
    else {
        return lean;
    };
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let binary = PathBuf::from(prefix)
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
    if output.status.success() && binary.is_file() {
        binary.to_string_lossy().into_owned()
    } else {
        lean
    }
}

fn lean() -> Option<String> {
    let lean = resolve_lean(std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into()));
    match Command::new(&lean).arg("--version").output() {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("version 4.35.0,") =>
        {
            Some(lean)
        }
        _ => None,
    }
}

fn compile(lean: &str, root: &Path, module: &str, source: &str) {
    fs::write(root.join(format!("{module}.lean")), source).unwrap();
    let output = Command::new(lean)
        .current_dir(root)
        .env("LEAN_PATH", root)
        .args(["-o", &format!("{module}.olean"), &format!("{module}.lean")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn option_cases(r: &mut Runner) {
    let mut many = vec!["Model", "--root", "."];
    let names = (0..129)
        .map(|i| format!("Example.theorem{i}"))
        .collect::<Vec<_>>();
    for name in &names {
        many.extend(["--theorem", name.as_str()]);
    }
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--help"],
        vec!["-h", "--bogus"],
        vec!["--root"],
        vec!["--root", ".", "--root", "."],
        vec!["--theorem"],
        vec!["--allow-axiom"],
        vec!["--theorem", "bad;run_cmd"],
        vec!["--theorem", "a..b"],
        vec!["--theorem", "9lives"],
        vec!["--theorem", "x'.y_1"],
        vec!["--allow-axiom", "sorryAx"],
        vec!["--allow-axiom", "Lean.ofReduceBool"],
        vec!["--allow-axiom", "Lean.trustCompiler"],
        vec![
            "--allow-axiom",
            "Example.native._native.native_decide.ax_1_1",
        ],
        vec!["--lean"],
        vec!["--lean", "a", "--lean", "b"],
        vec!["--timeout"],
        vec!["--timeout", "0"],
        vec!["--timeout", "301"],
        vec!["--timeout", "x"],
        vec!["--timeout", "+5", "--timeout", "6"],
        vec!["--timeout", "18446744073709551616"],
        vec!["--macro-provenance"],
        vec!["--macro-provenance", "a", "--macro-provenance", "b"],
        vec!["--frobnicate"],
        vec!["A", "B"],
        vec!["--json"],
        vec!["../Escape", "--root", ".", "--theorem", "Example.ok"],
        vec!["Lean", "--root", ".", "--theorem", "True.intro"],
        vec!["Std.Data", "--root", ".", "--theorem", "True.intro"],
        vec!["Lake", "--theorem", "X.y"],
        vec!["Leaner", "--theorem", "X.y"],
        vec!["Model", "--root", "."],
        vec!["Model", "--theorem", "Example.ok"],
        many,
        vec![
            "Model",
            "--root",
            ".",
            "--theorem",
            "Example.ok",
            "--lean",
            "/nonexistent/lean",
        ],
        vec![
            "Model",
            "--root",
            ".",
            "--theorem",
            "Example.ok",
            "--lean",
            "/nonexistent/lean",
            "--json",
        ],
    ];
    for args in &cases {
        r.case(args);
    }
}

fn lean_cases(r: &mut Runner, lean: &str) {
    let audit = |r: &mut Runner, root: &Path, module: &str, extra: &[&str]| {
        let mut args = vec![
            module.to_owned(),
            "--root".into(),
            root.to_str().unwrap().into(),
            "--lean".into(),
            lean.into(),
        ];
        if !extra.contains(&"--timeout") {
            args.extend(["--timeout".into(), "300".into()]);
        }
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        r.case(&args.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let f = Dir::new("lean");
    let root = &f.0;
    compile(
        lean,
        root,
        "Model",
        include_str!("../lean/Foundations.lean"),
    );
    let both = [
        "--theorem",
        "Vesper.Foundations.join_comm",
        "--theorem",
        "Vesper.Foundations.linear_exactly_once",
    ];
    assert!(
        audit(r, root, "Model", &[&both[..], &["--json"]].concat())
            .status
            .success()
    );
    audit(r, root, "Model", &both);
    // Missing module and theorems.
    audit(
        r,
        root,
        "Absent",
        &["--theorem", "Example.missing", "--json"],
    );
    audit(
        r,
        root,
        "Model",
        &["--theorem", "Example.missing", "--json"],
    );
    audit(r, root, "Model", &["--theorem", "Example.missing"]);
    audit(
        r,
        &root.join("nowhere"),
        "Model",
        &["--theorem", "Example.missing"],
    );

    // Axioms: rejected, then allowed, in both output forms.
    compile(
        lean,
        root,
        "Base",
        "namespace Example\naxiom assumption : True\ndef seed : Nat := 1\nend Example\n",
    );
    compile(
        lean,
        root,
        "Policy",
        "import Base\nnamespace Example\ntheorem exposed : True := assumption\ntheorem seed_refl : seed = seed := rfl\nend Example\n",
    );
    let rejected = audit(
        r,
        root,
        "Policy",
        &["--theorem", "Example.exposed", "--json"],
    );
    assert!(!rejected.status.success());
    audit(r, root, "Policy", &["--theorem", "Example.exposed"]);
    audit(
        r,
        root,
        "Policy",
        &[
            "--theorem",
            "Example.exposed",
            "--theorem",
            "Example.seed_refl",
            "--allow-axiom",
            "Example.assumption",
            "--allow-axiom",
            "Example.unused",
            "--json",
        ],
    );
    audit(
        r,
        root,
        "Policy",
        &[
            "--theorem",
            "Example.exposed",
            "--allow-axiom",
            "Example.assumption",
        ],
    );
    // sorry, a non-theorem and a partial definition.
    compile(
        lean,
        root,
        "Sorry",
        "theorem Example.incomplete : False := by sorry\n",
    );
    audit(
        r,
        root,
        "Sorry",
        &["--theorem", "Example.incomplete", "--json"],
    );
    audit(r, root, "Sorry", &["--theorem", "Example.incomplete"]);
    compile(
        lean,
        root,
        "Value",
        "def Example.value : Nat := 1\ntheorem Example.ok : True := True.intro\n",
    );
    audit(r, root, "Value", &["--theorem", "Example.value", "--json"]);
    compile(
        lean,
        root,
        "Loop",
        "partial def Example.loop (n : Nat) : Nat := Example.loop n\ntheorem Example.ok : True := True.intro\n",
    );
    audit(r, root, "Loop", &["--theorem", "Example.ok", "--json"]);
    // A timeout too short for any replay.
    audit(
        r,
        root,
        "Model",
        &[&both[..], &["--timeout", "1", "--json"]].concat(),
    );

    // A symlink in the artifact tree is refused.
    #[cfg(unix)]
    {
        let linked = Dir::new("link");
        compile(
            lean,
            &linked.0,
            "Model",
            include_str!("../lean/Foundations.lean"),
        );
        std::os::unix::fs::symlink(linked.0.join("Model.olean"), linked.0.join("Link.olean"))
            .unwrap();
        audit(r, &linked.0, "Model", &[&both[..], &["--json"]].concat());
    }
    // Nested artifact directories and non-artifact files.
    let nested = Dir::new("nested");
    fs::create_dir_all(nested.0.join("Deep/Er")).unwrap();
    compile(
        lean,
        &nested.0,
        "Leaf",
        "theorem Example.leaf : True := True.intro\n",
    );
    fs::rename(
        nested.0.join("Leaf.olean"),
        nested.0.join("Deep/Er/Leaf.olean"),
    )
    .unwrap();
    fs::write(nested.0.join("Deep/notes.txt"), "not an artifact").unwrap();
    fs::write(nested.0.join("Deep/Er/Leaf.ir"), "ir bytes").unwrap();
    audit(
        r,
        &nested.0,
        "Deep.Er.Leaf",
        &["--theorem", "Example.leaf", "--json"],
    );
}

fn provenance_cases(r: &mut Runner, lean: &str) {
    let work = Dir::new("forge");
    let source = work.0.join("mixed.ves");
    fs::write(&source, MIXED).unwrap();
    let manifest = work.0.join("provenance.json");
    let forged = vesper()
        .args(["forge", "lean"])
        .arg(&source)
        .arg("--provenance")
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(forged.status.success());
    let text = String::from_utf8(forged.stdout).unwrap();
    let proof = "import Expanded\n\
        theorem Proof.main_eq : Vesper.main = 42 := by decide\n\
        theorem Proof.plain_eq : Vesper.plain 7 = 7 := by decide\n\
        theorem Proof.block_eq : Vesper.«$block_main_0» = 42 := by decide\n";
    let build = |expanded: &str| {
        let root = Dir::new("forge-root");
        compile(lean, &root.0, "Expanded", expanded);
        compile(lean, &root.0, "Proof", proof);
        root
    };
    let audit = |r: &mut Runner, root: &Path, theorem: &str, manifest: Option<&Path>| {
        let mut args = vec![
            "Proof".to_owned(),
            "--root".into(),
            root.to_str().unwrap().into(),
            "--theorem".into(),
            theorem.into(),
            "--lean".into(),
            lean.into(),
            "--timeout".into(),
            "300".into(),
            "--json".into(),
        ];
        if let Some(m) = manifest {
            args.extend(["--macro-provenance".into(), m.to_str().unwrap().into()]);
        }
        r.case(&args.iter().map(String::as_str).collect::<Vec<_>>());
    };
    let root = build(&text);
    audit(r, &root.0, "Proof.main_eq", None);
    audit(r, &root.0, "Proof.main_eq", Some(&manifest));
    audit(r, &root.0, "Proof.block_eq", Some(&manifest));
    audit(r, &root.0, "Proof.plain_eq", None);
    audit(r, &root.0, "Proof.plain_eq", Some(&manifest));
    // Manifests that are missing, not JSON, of another schema, forged or
    // with an unknown field.
    audit(
        r,
        &root.0,
        "Proof.main_eq",
        Some(&work.0.join("absent.json")),
    );
    audit(r, &root.0, "Proof.main_eq", Some(&work.0));
    let table: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let variants: Vec<(&str, Vec<u8>)> = vec![
        ("not-json", b"{".to_vec()),
        ("not-utf8", vec![b'"', 0xff, b'"']),
        ("schema", {
            let mut t = table.clone();
            t["schema"] = "other/1".into();
            serde_json::to_vec(&t).unwrap()
        }),
        ("macro", {
            let mut t = table.clone();
            t["expansions"][0]["macro"] = "other".into();
            serde_json::to_vec(&t).unwrap()
        }),
        ("digest", {
            let mut t = table.clone();
            t["digest"] = "00".into();
            serde_json::to_vec_pretty(&t).unwrap()
        }),
        ("origin", {
            let mut t = table.clone();
            t["functions"][0]["origin"] = "elsewhere".into();
            serde_json::to_vec(&t).unwrap()
        }),
        ("unknown", {
            let mut t = table.clone();
            t["extra"] = 1.into();
            serde_json::to_vec(&t).unwrap()
        }),
        ("range-extra", {
            let mut t = table.clone();
            t["expansions"][0]["invocation"]["note"] = "ignored".into();
            serde_json::to_vec(&t).unwrap()
        }),
        ("duplicate-function", {
            let mut t = table.clone();
            let first = t["functions"][0].clone();
            t["functions"].as_array_mut().unwrap().push(first);
            serde_json::to_vec(&t).unwrap()
        }),
    ];
    for (name, bytes) in &variants {
        let path = work.0.join(format!("{name}.json"));
        fs::write(&path, bytes).unwrap();
        audit(r, &root.0, "Proof.main_eq", Some(&path));
    }
    // Stripped markers.
    let stripped = text
        .lines()
        .filter(|line| {
            !line.contains("«main$macro_provenance»")
                && !line.contains("«$block_main_0$macro_provenance»")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let stripped_root = build(&stripped);
    audit(r, &stripped_root.0, "Proof.main_eq", Some(&manifest));
    audit(r, &stripped_root.0, "Proof.block_eq", Some(&manifest));
    audit(r, &stripped_root.0, "Proof.plain_eq", Some(&manifest));
    // No module marker.
    let unmarked = text
        .lines()
        .filter(|line| !line.contains("«vesper$macro_provenance»"))
        .collect::<Vec<_>>()
        .join("\n");
    let unmarked_root = build(&unmarked);
    audit(r, &unmarked_root.0, "Proof.plain_eq", None);
    audit(r, &unmarked_root.0, "Proof.plain_eq", Some(&manifest));
}

#[test]
fn vesper_port_matches_rust_proof_audit() {
    let work = Dir::new("port");
    let mut r = Runner {
        port: build_port(&work.0),
        cases: 0,
        rust_time: Duration::ZERO,
        port_time: Duration::ZERO,
    };
    option_cases(&mut r);
    let options = r.cases;
    let Some(lean) = lean() else {
        eprintln!("{options} option cases agree; Lean 4.35.0 unavailable, real audits skipped");
        return;
    };
    r.rust_time = Duration::ZERO;
    r.port_time = Duration::ZERO;
    lean_cases(&mut r, &lean);
    provenance_cases(&mut r, &lean);
    eprintln!(
        "{} cases agree ({} with Lean); Lean audits: Rust {:.2?}, Vesper {:.2?} ({:.3}x)",
        r.cases,
        r.cases - options,
        r.rust_time,
        r.port_time,
        r.port_time.as_secs_f64() / r.rust_time.as_secs_f64()
    );
}
