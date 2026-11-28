//! Integration B (#136): Stage-5 verifier hooks on the Cranelift functions
//! `src/native.rs` emits, on x86_64 and AArch64, and the guarantee that
//! programs compile to identical code without a verifier flag.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn fixture() -> PathBuf {
    support::repo_root().join("tests/fixtures/integration_b/verifiers.ves")
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-verifiers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn policy(&self, name: &str, json: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, json).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    support::vesper().args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Emit an object for `entry` on `target` with the given verifier flags.
fn emit(temp: &Temp, entry: &str, target: &str, flags: &[&str]) -> (Output, Vec<u8>) {
    let out = temp.0.join(format!("{entry}-{target}.o"));
    let _ = fs::remove_file(&out);
    let file = fixture();
    let mut args = vec![
        "emit",
        file.to_str().unwrap(),
        "--emit",
        "object",
        "-o",
        out.to_str().unwrap(),
        "--entry",
        entry,
        "--target",
        target,
    ];
    args.extend_from_slice(flags);
    let output = vesper(&args);
    let bytes = fs::read(&out).unwrap_or_default();
    (output, bytes)
}

fn rejects(temp: &Temp, entry: &str, flags: &[&str], rule: &str, function: &str) {
    for target in TARGETS {
        let (o, _) = emit(temp, entry, target, flags);
        let t = text(&o);
        assert!(
            !o.status.success(),
            "{target} {entry} {flags:?} accepted:\n{t}"
        );
        assert!(t.contains(rule), "{target}: missing {rule}:\n{t}");
        assert!(t.contains(function), "{target}: missing {function}:\n{t}");
        assert!(
            t.contains("verifiers.ves:"),
            "{target}: no source location:\n{t}"
        );
        assert!(t.contains("native verifier finding(s)"), "{t}");
    }
}

fn accepts(temp: &Temp, entry: &str, flags: &[&str], verifier: &str) {
    for target in TARGETS {
        let (o, bytes) = emit(temp, entry, target, flags);
        let t = text(&o);
        assert!(
            o.status.success(),
            "{target} {entry} {flags:?} rejected:\n{t}"
        );
        assert!(t.contains("verified"), "{t}");
        assert!(t.contains(verifier), "{t}");
        assert!(!bytes.is_empty());
    }
}

#[test]
fn const_time_rejects_a_secret_branch_and_accepts_a_secret_flow() {
    let temp = Temp::new();
    let planted = temp.policy(
        "ct.json",
        r#"{"schema":"vesper.native-verifiers/1","const_time":{"select":{"secret":["secret"]}}}"#,
    );
    rejects(
        &temp,
        "ct_main",
        &[
            "--verify-const-time",
            "--verify-policy",
            planted.to_str().unwrap(),
        ],
        "const-time/secret-branch",
        "select",
    );
    let clean = temp.policy(
        "ct-clean.json",
        r#"{"schema":"vesper.native-verifiers/1","const_time":{"forward":{"secret":["secret"]}}}"#,
    );
    accepts(
        &temp,
        "clean_main",
        &[
            "--verify=const-time",
            "--verify-policy",
            clean.to_str().unwrap(),
        ],
        "const-time",
    );
}

#[test]
fn stack_rejects_recursion_and_budgets_and_accepts_a_bounded_root() {
    let temp = Temp::new();
    rejects(
        &temp,
        "rec_main",
        &["--verify-stack"],
        "silicon.stack.recursion",
        "count",
    );
    let tight = temp.policy(
        "tight.json",
        r#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"ct_main":8}}}"#,
    );
    let policy = tight.to_str().unwrap();
    for target in TARGETS {
        // Unoptimized objects keep every call and every local in a stack
        // slot, so the path names the callee.
        let flags = &["--verify-stack", "--verify-policy", policy, "--no-opt"][..];
        let (o, _) = emit(&temp, "ct_main", target, flags);
        let t = text(&o);
        assert!(!o.status.success(), "{t}");
        assert!(t.contains("silicon.stack.budget_exceeded"), "{t}");
        assert!(
            t.contains("(exceeded) via ct_main -> select"),
            "{flags:?}: {t}"
        );
        // Optimized objects inline `select` into `ct_main` (#959) and keep
        // its locals in registers (#972): the verifier budgets the frame that
        // ships, which is at most the frame record (0 bytes on x86-64, 16 on
        // AArch64), so only the path is target-neutral.
        let flags = &["--verify-stack", "--verify-policy", policy][..];
        let (o, _) = emit(&temp, "ct_main", target, flags);
        let t = text(&o);
        assert!(t.contains(") via ct_main\n"), "{flags:?}: {t}");
        assert!(!t.contains("-> select"), "{flags:?}: {t}");
    }
    let roomy = temp.policy(
        "roomy.json",
        r#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"ct_main":4096}}}"#,
    );
    accepts(
        &temp,
        "ct_main",
        &["--verify-stack", "--verify-policy", roomy.to_str().unwrap()],
        "stack ct_main: worst case",
    );
}

#[test]
fn realtime_rejects_recursion_and_undeclared_imports_and_accepts_a_confined_root() {
    let temp = Temp::new();
    let rec = temp.policy(
        "rt-rec.json",
        r#"{"schema":"vesper.native-verifiers/1","realtime":{"roots":{"rec_main":null}}}"#,
    );
    rejects(
        &temp,
        "rec_main",
        &[
            "--verify-realtime",
            "--verify-policy",
            rec.to_str().unwrap(),
        ],
        "realtime/rt-recursion",
        "rec_main",
    );
    let float = temp.policy(
        "rt-float.json",
        r#"{"schema":"vesper.native-verifiers/1","realtime":{"roots":{"float_main":null}}}"#,
    );
    rejects(
        &temp,
        "float_main",
        &[
            "--verify-realtime",
            "--verify-policy",
            float.to_str().unwrap(),
        ],
        "realtime/rt-unknown-callee",
        "fmod",
    );
    // Declaring `fmod` as a pure realtime-safe symbol confines the root.
    let declared = temp.policy(
        "rt-declared.json",
        r#"{"schema":"vesper.native-verifiers/1","realtime":{"roots":{"float_main":null},"symbols":{"fmod":{"effects":[],"frame_bytes":64}}}}"#,
    );
    accepts(
        &temp,
        "float_main",
        &[
            "--verify-realtime",
            "--verify-policy",
            declared.to_str().unwrap(),
        ],
        "realtime",
    );
    let clean = temp.policy(
        "rt-clean.json",
        r#"{"schema":"vesper.native-verifiers/1","realtime":{"roots":{"clean_main":null}}}"#,
    );
    accepts(
        &temp,
        "clean_main",
        &[
            "--verify-realtime",
            "--verify-policy",
            clean.to_str().unwrap(),
        ],
        "realtime",
    );
    // Effects declared for a helper still count once the optimized build
    // inlines it into the root (#959), and with `--no-opt` through the call.
    let alloc = temp.policy(
        "rt-alloc.json",
        r#"{"schema":"vesper.native-verifiers/1","realtime":{"roots":{"clean_main":null},"effects":{"forward":["Alloc"]}}}"#,
    );
    for no_opt in [false, true] {
        let mut flags = vec![
            "--verify-realtime",
            "--verify-policy",
            alloc.to_str().unwrap(),
        ];
        if no_opt {
            flags.push("--no-opt");
        }
        rejects(
            &temp,
            "clean_main",
            &flags,
            "realtime/rt-forbidden-effect",
            "clean_main",
        );
    }
}

#[test]
fn detfloat_rejects_platform_libm_and_accepts_integer_code() {
    let temp = Temp::new();
    rejects(
        &temp,
        "float_main",
        &["--verify-detfloat"],
        "detfloat/detfloat.libm",
        "wrap",
    );
    accepts(&temp, "clean_main", &["--verify-detfloat"], "detfloat");
}

#[test]
fn verifier_flags_validate_their_inputs() {
    let temp = Temp::new();
    let file = fixture();
    let file = file.to_str().unwrap();
    let o = vesper(&["emit", file, "--emit", "ir", "--verify-stack"]);
    assert!(text(&o).contains("--verify flags require build or emit --emit object/clif"));
    let o = vesper(&["emit", file, "--emit", "clif", "--verify=stack,nope"]);
    assert!(text(&o).contains("unknown verifier 'nope'"), "{}", text(&o));
    let policy = temp.policy("p.json", r#"{"schema":"vesper.native-verifiers/1"}"#);
    let o = vesper(&[
        "emit",
        file,
        "--emit",
        "clif",
        "--verify-policy",
        policy.to_str().unwrap(),
    ]);
    assert!(
        text(&o).contains("--verify-policy requires"),
        "{}",
        text(&o)
    );
    let wrong = temp.policy("w.json", r#"{"schema":"other/1"}"#);
    let o = vesper(&[
        "emit",
        file,
        "--emit",
        "clif",
        "--entry",
        "ct_main",
        "--verify=all",
        "--verify-policy",
        wrong.to_str().unwrap(),
    ]);
    assert!(text(&o).contains("schema must be"), "{}", text(&o));
    let unknown = temp.policy(
        "u.json",
        r#"{"schema":"vesper.native-verifiers/1","const_time":{"select":{"secret":["nope"]}}}"#,
    );
    let o = vesper(&[
        "emit",
        file,
        "--emit",
        "clif",
        "--entry",
        "ct_main",
        "--verify-const-time",
        "--verify-policy",
        unknown.to_str().unwrap(),
    ]);
    assert!(
        text(&o).contains("'select' has no parameter 'nope'"),
        "{}",
        text(&o)
    );
    assert!(!o.status.success());
}

#[test]
fn build_tee_writes_the_rtmr_single_extend_of_the_executable() {
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("skipped: no C linker");
        return;
    }
    let temp = Temp::new();
    let exe = temp.0.join("counter");
    let example = support::repo_root().join("examples/fibonacci.ves");
    let o = vesper(&[
        "build",
        example.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--tee",
    ]);
    if !o.status.success() {
        // Some hosts lack a working native linker for the default target.
        eprintln!("skipped: build failed: {}", text(&o));
        return;
    }
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.0.join("counter.tee.json")).unwrap()).unwrap();
    let bytes = fs::read(&exe).unwrap();
    let digest = *blake3::hash(&bytes).as_bytes();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    assert_eq!(report["schema"], "vesper.tee-measurement/1");
    assert_eq!(report["blake3"], hex(&digest));
    // One extend of a zeroed SHA-384 register with the digest padded to 48
    // bytes (vesper_aegis::claims::single_extend).
    let rtmr = {
        use sha2::Digest;
        let mut h = sha2::Sha384::new();
        h.update([0u8; 48]);
        h.update(digest);
        h.update([0u8; 16]);
        h.finalize()
    };
    assert_eq!(report["tdx_rtmr_single_extend_sha384"], hex(&rtmr));
    assert!(
        report["hardware_extend"]
            .as_str()
            .unwrap()
            .starts_with("not performed")
    );
    let o = vesper(&["emit", example.to_str().unwrap(), "--tee"]);
    assert!(text(&o).contains("--tee requires build"));
}
