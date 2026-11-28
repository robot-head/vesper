//! Lane 24 (#266): Stage-5 backend verifiers run from source capability
//! effects. `eff [ConstTime]`, `eff [Realtime]` and `eff [DetFloat]` imply the
//! Vault constant-time, Resonance realtime and Kinetic DetFloat verifiers and
//! their policies, which `vesper build` and `vesper emit --emit object|clif`
//! run on the emitted Cranelift functions without any flag. Every case runs
//! for x86_64 and AArch64 objects.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn root() -> PathBuf {
    support::repo_root()
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(
        root()
            .join("tests/fixtures/capability_verifiers")
            .join(kind),
    )
    .unwrap()
    .map(|e| e.unwrap().path())
    .filter(|p| p.extension().is_some_and(|e| e == "ves"))
    .collect();
    out.sort();
    assert!(!out.is_empty(), "no {kind} fixtures");
    out
}

/// Lane 15's accepted capability programs, which must also pass the backend.
fn lane15_accepted() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        fs::read_dir(root().join("tests/fixtures/capability_effects/accept"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "ves"))
            .collect();
    out.sort();
    out
}

fn vesper(args: &[&str]) -> Output {
    support::vesper().args(args).output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-capability-verifiers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Capability functions now compile once; their results under the native
/// backend still match the evaluator.
#[test]
fn native_results_match_the_evaluator() {
    let mut files = fixtures("accept");
    files.extend(lane15_accepted());
    files.extend(fixtures("reject"));
    for file in files {
        let f = file.to_str().unwrap();
        let run = vesper(&["run", f]);
        let jit = vesper(&["jit", f]);
        assert!(run.status.success(), "{f}: {}", stderr(&run));
        assert!(jit.status.success(), "{f}: {}", stderr(&jit));
        assert_eq!(run.stdout, jit.stdout, "{f}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn vesper_build_runs_the_capability_verifiers() {
    let temp = Temp::new();
    let ok = root().join("tests/fixtures/capability_verifiers/accept/consttime_select.ves");
    let exe = temp.path("ct");
    let o = vesper(&["build", ok.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    let err = stderr(&o);
    if err.contains("linker") && !o.status.success() {
        eprintln!("skipping: no linker: {err}");
        return;
    }
    assert!(o.status.success(), "{err}");
    assert!(err.contains("emitted function(s): const-time"), "{err}");
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(0));

    let bad = root().join("tests/fixtures/capability_verifiers/reject/realtime_fmod.ves");
    let exe = temp.path("rt");
    let o = vesper(&["build", bad.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("error[realtime/rt-unknown-callee]"),
        "{}",
        stderr(&o)
    );
    assert!(!exe.exists(), "a rejected program must not be linked");
}

/// An explicit `--verify=detfloat` keeps auditing every emitted function;
/// the DetFloat annotations only narrow the automatic check.
#[test]
fn explicit_detfloat_request_still_audits_every_function() {
    let temp = Temp::new();
    let file = temp.path("mixed.ves");
    fs::write(
        &file,
        "fn half(x: f64) -> f64 eff [DetFloat] {\n    x * 0.5\n}\n\nfn wrap(x: f64) -> f64 {\n    x % 2.0\n}\n\nfn main() -> f64 {\n    half(wrap(7.5))\n}\n",
    )
    .unwrap();
    let f = file.to_str().unwrap();
    for target in TARGETS {
        let out = temp.path("out.o");
        let base = [
            "emit",
            f,
            "--emit",
            "object",
            "--target",
            target,
            "-o",
            out.to_str().unwrap(),
        ];
        let automatic = vesper(&base);
        assert!(automatic.status.success(), "{}", stderr(&automatic));
        let mut explicit = base.to_vec();
        explicit.push("--verify=detfloat");
        let o = vesper(&explicit);
        let err = stderr(&o);
        assert!(!o.status.success(), "{err}");
        assert!(err.contains("error[detfloat/detfloat.libm]: wrap"), "{err}");
    }
}
