//! Self-hosting #1828 part 8: `@vesper/lsp-proof-run`
//! (packages/vesper-lsp-proof-run) against the proof-lens runner of
//! `src/lsp/proof_lens.rs` (`run`, `run_stages`, `ensure_olean`,
//! `bitvec_check`, `child_output`, `Cache`, `Stage`). Both run the same
//! prepared job (`proof_job_for_port`) against a fake Lean toolchain, a
//! native Vesper program (tests/fixtures/proof-lens-toolchain) that is a
//! launcher, a `lean` binary whose compiles succeed, fail, stall or find a
//! `bv_decide` counterexample per module, and a proof auditor that
//! generates or replays a JSON report. The outcome rows, the
//! normalized log of every Lean and auditor invocation, and the cache each
//! side leaves for the other must agree, in the JIT (optimize on and off)
//! and the evaluator.
#![cfg(unix)]

mod std_io_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use std_io_support::{MODES, Mode, Temp, jit_host, lit, run, std_dir, word};

const SOURCE: &str = "fn inc(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    if x < 100 {
        x + 1
    } else {
        0
    }
}

fn add(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}

fn half(x: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let q = x / 2;
    let r = x % 3;
    wrapping_add(q, r)
}

fn ratio(x: u64, y: u64) -> u64 eff [Fail[RuntimeError]] {
    x / y
}

fn both(a: bool, b: bool, x: u8) -> bool eff [Fail[ArithOverflow]] {
    a && (x < 200) && (wrapping_add(x, 100) > 3)
}

fn negate(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    -x
}

fn counted(x: u32) -> u32 eff [Fail[ArithOverflow]] {
    let mut total = x;
    total += 1;
    if total > 10 {
        total - 10
    } else {
        total
    }
}

fn main() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    inc(1) + add(2, 3) + half(4) + negate(5)
}
";

const DRIVER: &str = r#"use @vesper/lsp-proof-run::proof_run;

pub fn run_job() -> u64 eff [Ffi[StdFs], Ffi[StdProcess], Ffi[StdEnv]] {
    proof_run(in "@DIR@/job", in "@DIR@/out")
}
"#;

const VERSION_OK: &str =
    "Lean (version 4.35.0, x86_64-unknown-linux-gnu, commit 0123abcd, Release)\n";

#[derive(Clone, Copy, PartialEq)]
enum Launcher {
    Ok,
    Fails,
    Missing,
}

#[derive(Clone, Copy, PartialEq)]
enum Audit {
    Absent,
    Generate,
    Fixed(&'static [u8]),
    Fails,
    Slow,
}

#[derive(Clone, Copy, PartialEq)]
enum Cache {
    None,
    /// Each side runs twice on its own cache, then once on the other's.
    Shared,
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    extraction: &'static [&'static str],
    models: &'static [&'static str],
    launcher: Launcher,
    version: &'static str,
    audit: Audit,
    /// (function, `EXTRACTION` or `RUN`; behaviour tokens)
    plan: &'static [(&'static str, &'static str)],
    /// (function or obligation index, axioms JSON)
    axioms: &'static [(&'static str, &'static str)],
    cex: &'static str,
    timeout: u64,
    cache: Cache,
}

const BASE: Scenario = Scenario {
    name: "",
    extraction: &[],
    models: &[],
    launcher: Launcher::Ok,
    version: VERSION_OK,
    audit: Audit::Generate,
    plan: &[],
    axioms: &[],
    cex: "",
    timeout: 20,
    cache: Cache::None,
};

const ADD_CEX: &str = "a.lean:5:2: error: The prover found a counterexample, consider the following assignment:\nb = 0xffffffffffffffff#64\na = 0x8000000000000000#64\n\nmore text\n";

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "all proved",
            extraction: &["inc", "add"],
            models: &["inc", "half", "both"],
            axioms: &[("inc", "[\"propext\",\"Quot.sound\"]")],
            ..BASE
        },
        Scenario {
            name: "all proved, cached",
            extraction: &["inc"],
            models: &["inc", "counted"],
            cache: Cache::Shared,
            ..BASE
        },
        Scenario {
            name: "no auditor",
            models: &["inc"],
            audit: Audit::Absent,
            ..BASE
        },
        Scenario {
            name: "missing launcher",
            extraction: &["inc"],
            models: &["inc"],
            launcher: Launcher::Missing,
            ..BASE
        },
        Scenario {
            name: "launcher fails",
            models: &["inc"],
            launcher: Launcher::Fails,
            ..BASE
        },
        Scenario {
            name: "wrong version",
            models: &["inc", "add"],
            version: "Lean (version 4.30.0, x86_64-unknown-linux-gnu, Release)\n",
            ..BASE
        },
        Scenario {
            name: "extraction does not compile",
            extraction: &["inc", "add"],
            models: &["inc"],
            plan: &[("EXTRACTION", "compile_fail")],
            ..BASE
        },
        Scenario {
            name: "omega fails, bv_decide finds nothing, cached",
            models: &["ratio", "inc"],
            plan: &[("ratio", "omega_fail bv_ok")],
            cache: Cache::Shared,
            ..BASE
        },
        Scenario {
            name: "counterexample, cached",
            models: &["add", "ratio"],
            plan: &[("add", "omega_fail bv_cex"), ("ratio", "omega_fail bv_ok")],
            cex: ADD_CEX,
            cache: Cache::Shared,
            ..BASE
        },
        Scenario {
            name: "counterexample in decimal with noise",
            models: &["ratio"],
            plan: &[("ratio", "omega_fail bv_cex")],
            cex: "found a counterexample, consider:\r\nnot an assignment\r\nz = 7#64\r\n  y   =  0x0#64  \r\nx = +18446744073709551615#64\r\ny = 0x#64\r\nend\r\nx = 9\n",
            ..BASE
        },
        Scenario {
            name: "counterexample values that do not parse",
            models: &["negate", "both"],
            plan: &[
                ("negate", "omega_fail bv_cex"),
                ("both", "omega_fail bv_cex"),
            ],
            cex: "found a counterexample\nx = 0x1ffffffffffffffffffffffffffffffff#64\na = true\nx = -3\nb = false\nx = 0xFFFFFFFFFFFFFFFF#64\n",
            ..BASE
        },
        Scenario {
            name: "bv_decide fails",
            models: &["negate"],
            plan: &[("negate", "omega_fail")],
            ..BASE
        },
        Scenario {
            name: "bv_decide fails silently",
            models: &["negate"],
            plan: &[("negate", "omega_fail bv_quiet")],
            ..BASE
        },
        Scenario {
            name: "bv_decide times out",
            models: &["negate"],
            plan: &[("negate", "omega_fail bv_sleep")],
            timeout: 1,
            ..BASE
        },
        Scenario {
            name: "compile times out",
            models: &["inc", "half"],
            plan: &[("inc", "slow")],
            timeout: 1,
            ..BASE
        },
        Scenario {
            name: "run module does not compile",
            extraction: &["inc"],
            models: &["inc"],
            plan: &[("RUN", "compile_fail")],
            ..BASE
        },
        Scenario {
            name: "forbidden axioms",
            extraction: &["inc", "add"],
            models: &["inc", "half"],
            axioms: &[
                ("0", "[\"propext\"]"),
                ("1", "[3,null]"),
                ("inc", "[\"sorryAx\",\"propext\"]"),
                ("half", "{\"not\":\"an array\"}"),
            ],
            ..BASE
        },
        Scenario {
            name: "auditor fails",
            extraction: &["inc"],
            models: &["inc"],
            audit: Audit::Fails,
            ..BASE
        },
        Scenario {
            name: "auditor times out",
            models: &["inc"],
            audit: Audit::Slow,
            timeout: 1,
            ..BASE
        },
    ]
}

/// Reports the auditor replays as is.
const REPORTS: &[&[u8]] = &[
    b"{\"kernel_replayed\":false,\"theorems\":[]}",
    b"{\"theorems\":[]}",
    b"{\"kernel_replayed\":true}",
    b"{\"kernel_replayed\":true,\"checked_constants\":-5,\"theorems\":{}}",
    b"{\"kernel_replayed\":true,\"checked_constants\":7.5,\"theorems\":[{\"name\":\"VesperLens.obligation_0\",\"axioms\":[]},{\"name\":\"VesperLens.obligation_0\",\"axioms\":[\"propext\"]}]}",
    b"{\"kernel_replayed\":true,\"checked_constants\":18446744073709551615,\"theorems\":[{\"name\":\"VesperLens.obligation_0\"}],\"kernel_replayed\":true}",
    b"{\"kernel_replayed\":true,\"kernel_replayed\":false}",
    b"[true]",
    b"",
    b"   \n  ",
    b"{\"kernel_replayed\": tru}",
    b"{\"a\":1,}",
    b"[1,2",
    b"[1,2,]",
    b"{\"a\" 1}",
    b"{1:2}",
    b"\"\\ud800\"",
    b"\"\\uDC00x\"",
    b"\"\\u12\"",
    b"\"\\q\"",
    b"\"a\tb\"",
    b"{\"kernel_replayed\":true,\"checked_constants\":1e400}",
    b"{\"kernel_replayed\":true,\"checked_constants\":-1e400}",
    b"{\"kernel_replayed\":true,\"checked_constants\":01}",
    b"{\"kernel_replayed\":true,\"checked_constants\":1.}",
    b"{\"kernel_replayed\":true,\"checked_constants\":-}",
    b"{\"kernel_replayed\":true,\"checked_constants\":1e}",
    b"{\"kernel_replayed\":true}\n{}",
    b"{\"kernel_replayed\":true}  x",
    b"nul",
    b"{\"kernel_replayed\":true,\n\"theorems\":[\n  {\"name\": \"\xff\xfe\", \"axioms\": []}\n], \"x\": [}",
    b"{\"kernel_replayed\":true,\"theorems\":[{\"name\":\"VesperLens.\\u006fbligation_0\",\"axioms\":[]}],\"s\":\"\\ud83d\\ude00\\n\\/\"}",
];

fn deep(n: usize) -> Vec<u8> {
    let mut v = vec![b'['; n];
    v.extend(std::iter::repeat_n(b']', n));
    v
}

/// The fake toolchain (tests/fixtures/proof-lens-toolchain), built once
/// as a native executable by `vesper build`. Copied in as the launcher,
/// `prefix/bin/lean` and the auditor, it acts by the name it runs under.
fn fake_toolchain() -> &'static Path {
    static BUILT: OnceLock<(Temp, PathBuf)> = OnceLock::new();
    &BUILT
        .get_or_init(|| {
            let dir = Temp::new("proof-lens-fake");
            let exe = dir.path().join("fake-toolchain");
            let status = Command::new(env!("CARGO_BIN_EXE_vesper"))
                .arg("build")
                .arg("tests/fixtures/proof-lens-toolchain")
                .arg("-o")
                .arg(&exe)
                .status()
                .expect("run vesper build");
            assert!(status.success(), "vesper build of the fake toolchain");
            (dir, exe)
        })
        .1
}

/// A fake toolchain for `s` in `tc`: the launcher, `prefix/bin/lean`, the
/// auditor and their control files. Returns (launcher, auditor).
fn toolchain(
    tc: &Path,
    s: &Scenario,
    modules: &[(String, String)],
    report: &[u8],
) -> (String, Option<PathBuf>) {
    let prefix = tc.join("prefix");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::create_dir_all(prefix.join("lib/lean")).unwrap();
    let fake = fake_toolchain();
    let install = |to: &Path| {
        fs::copy(fake, to).unwrap();
        fs::set_permissions(to, fs::Permissions::from_mode(0o755)).unwrap();
    };
    install(&prefix.join("bin/lean"));
    let launcher = tc.join("launcher");
    if s.launcher != Launcher::Missing {
        install(&launcher);
    }
    let launcher_mode = if s.launcher == Launcher::Fails {
        "fails"
    } else {
        "ok"
    };
    fs::write(tc.join("launcher_mode"), launcher_mode).unwrap();
    let auditor = tc.join("auditor");
    install(&auditor);
    fs::write(tc.join("version"), s.version).unwrap();
    let module_of = |f: &str| -> String {
        match f {
            "EXTRACTION" => "VesperLensObligation".into(),
            "RUN" => "VesperLensRun".into(),
            f => modules
                .iter()
                .find(|(name, _)| name == f)
                .map(|(_, m)| m.clone())
                .unwrap_or_else(|| panic!("no model of {f}")),
        }
    };
    let plan: String = s
        .plan
        .iter()
        .map(|(f, b)| format!("{} {b}\n", module_of(f)))
        .collect();
    fs::write(tc.join("plan"), plan).unwrap();
    let axioms: String = s
        .axioms
        .iter()
        .map(|(f, a)| {
            let theorem = match f.parse::<usize>() {
                Ok(i) => format!("VesperLens.obligation_{i}"),
                Err(_) => format!("{}.no_arith_fault", module_of(f)),
            };
            format!("{theorem} {a}\n")
        })
        .collect();
    fs::write(tc.join("axioms"), axioms).unwrap();
    fs::write(tc.join("cex"), s.cex).unwrap();
    let mode = match s.audit {
        Audit::Fixed(_) => "fixed",
        Audit::Fails => "fail",
        Audit::Slow => "slow",
        _ => "generate",
    };
    fs::write(tc.join("audit_mode"), mode).unwrap();
    fs::write(tc.join("report"), report).unwrap();
    let auditor = (s.audit != Audit::Absent).then_some(auditor);
    (launcher.display().to_string(), auditor)
}

struct Port {
    checked: vesper::ffi_lowering::ForeignChecked,
    /// `run_job` compiled once each with the optimizer on and off.
    jit: Option<[vesper::native::WordFunction; 2]>,
    dir: Temp,
    _pkg: Temp,
}

fn port() -> Port {
    let dir = Temp::new("proof-lens-io");
    let pkg = Temp::new("proof-lens-driver");
    let std = std_dir();
    let run_pkg = fs::canonicalize("packages/vesper-lsp-proof-run").unwrap();
    let mut deps = format!(
        "\"@vesper/lsp-proof-run\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        lit(&run_pkg)
    );
    for name in std_io_support::LIBRARIES {
        deps.push_str(&format!(
            "\"@vesper-std/{name}\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            lit(&std.join(name))
        ));
    }
    fs::write(
        pkg.path().join("Loom.toml"),
        format!(
            "[package]\nname = \"@local/proof-lens-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\", \"Ffi[StdProcess]\", \"Ffi[StdEnv]\"]\n\n\
             [dependencies]\n{deps}"
        ),
    )
    .unwrap();
    fs::create_dir_all(pkg.path().join("src")).unwrap();
    fs::write(
        pkg.path().join("src/lib.ves"),
        DRIVER.replace("@DIR@", &dir.lit()),
    )
    .unwrap();
    let checked = std_io_support::check(pkg.path());
    let jit = jit_host().then(|| {
        [true, false].map(|optimize| {
            checked
                .word_function("run_job", &std_io_support::symbols(), optimize)
                .unwrap()
        })
    });
    Port {
        checked,
        jit,
        dir,
        _pkg: pkg,
    }
}

/// The modes a scenario runs in: both JIT pipelines where the host has a
/// JIT, and the evaluator when `evaluator` (or without a JIT).
fn modes(evaluator: bool) -> Vec<Mode> {
    MODES
        .iter()
        .copied()
        .filter(|m| match m {
            Mode::Evaluator => evaluator || !jit_host(),
            _ => jit_host(),
        })
        .collect()
}

/// One run's rows and the toolchain log it left.
#[derive(Debug, PartialEq)]
struct Ran {
    rows: String,
    log: String,
}

fn take_log(tc: &Path) -> String {
    let log = fs::read_to_string(tc.join("log")).unwrap_or_default();
    let _ = fs::remove_file(tc.join("log"));
    log
}

fn rust_run(job: &vesper::lsp::ProofJobForPort, tc: &Path) -> Ran {
    let rows = job.run();
    Ran {
        rows,
        log: take_log(tc),
    }
}

fn vesper_run(port: &Port, words: &[u8], mode: Mode, tc: &Path) -> Ran {
    let out = port.dir.path().join("out");
    let _ = fs::remove_file(&out);
    fs::write(port.dir.path().join("job"), words).unwrap();
    let code = match (mode, &port.jit) {
        // SAFETY: `run_job` takes no arguments and returns one word.
        (Mode::Jit, Some(f)) => unsafe { f[0].call(&[]) }.unwrap(),
        (Mode::JitNoOpt, Some(f)) => unsafe { f[1].call(&[]) }.unwrap(),
        _ => word(&run(&port.checked, "run_job", mode)),
    };
    assert_eq!(code, 0, "run_job ({mode:?})");
    Ran {
        rows: fs::read_to_string(&out).unwrap(),
        log: take_log(tc),
    }
}

/// The cache directory's entries, relative and sorted, with their
/// permission bits and file contents (the root first).
fn cache_tree(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
    let bits = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let mut out = vec![(String::new(), bits(dir), Vec::new())];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().display().to_string();
            if p.is_dir() {
                out.push((rel + "/", bits(&p), Vec::new()));
                stack.push(p);
            } else {
                out.push((rel, bits(&p), fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn job_for(
    s: &Scenario,
    launcher: &str,
    auditor: Option<&Path>,
    cache: Option<&Path>,
) -> vesper::lsp::ProofJobForPort {
    let ext: Vec<String> = s.extraction.iter().map(|f| f.to_string()).collect();
    let models: Vec<String> = s.models.iter().map(|f| f.to_string()).collect();
    vesper::lsp::proof_job_for_port(SOURCE, &ext, &models, launcher, auditor, s.timeout, cache)
        .unwrap_or_else(|e| panic!("{}: {e}", s.name))
}

/// Check one scenario; returns the number of answers compared.
fn check_scenario(port: &Port, s: &Scenario, report: &[u8], evaluator: bool) -> usize {
    let tc = Temp::new("proof-lens-tc");
    // The module names come from the models: prepare once without a
    // toolchain to learn them.
    let probe = job_for(s, "launcher", None, None);
    let modules = probe.modules();
    let (launcher, auditor) = toolchain(tc.path(), s, &modules, report);
    let mut answers = 0;
    let started = Instant::now();
    match s.cache {
        Cache::None => {
            let job = job_for(s, &launcher, auditor.as_deref(), None);
            let want = rust_run(&job, tc.path());
            let words = job.words();
            for mode in modes(evaluator) {
                let got = vesper_run(port, &words, mode, tc.path());
                assert_eq!(got, want, "{} ({mode:?})", s.name);
                answers += want.rows.lines().count() + want.log.lines().count();
            }
        }
        Cache::Shared => {
            for mode in modes(evaluator) {
                let rust_cache = tc.path().join(format!("rust-cache-{mode:?}"));
                let port_cache = tc.path().join(format!("port-cache-{mode:?}"));
                let rust_job = job_for(s, &launcher, auditor.as_deref(), Some(&rust_cache));
                let port_job = job_for(s, &launcher, auditor.as_deref(), Some(&port_cache));
                let port_words = port_job.words();
                for pass in 0..2 {
                    let want = rust_run(&rust_job, tc.path());
                    let got = vesper_run(port, &port_words, mode, tc.path());
                    assert_eq!(got, want, "{} pass {pass} ({mode:?})", s.name);
                    answers += want.rows.lines().count() + want.log.lines().count();
                }
                assert_eq!(
                    cache_tree(&port_cache),
                    cache_tree(&rust_cache),
                    "{} cache ({mode:?})",
                    s.name
                );
                // Each side reads the other's cache.
                let rust_on_port = job_for(s, &launcher, auditor.as_deref(), Some(&port_cache));
                let port_on_rust = job_for(s, &launcher, auditor.as_deref(), Some(&rust_cache));
                let want = rust_run(&rust_on_port, tc.path());
                let got = vesper_run(port, &port_on_rust.words(), mode, tc.path());
                assert_eq!(got, want, "{} swapped caches ({mode:?})", s.name);
                answers += want.rows.lines().count() + want.log.lines().count();
            }
        }
    }
    eprintln!("{}: {answers} answers in {:?}", s.name, started.elapsed());
    answers
}

#[test]
fn proof_runs_match_rust() {
    let port = port();
    let mut answers = 0;
    let mut runs = 0;
    for s in scenarios() {
        answers += check_scenario(&port, &s, b"", true);
        runs += 1;
    }
    let mut reports: Vec<Vec<u8>> = REPORTS.iter().map(|r| r.to_vec()).collect();
    reports.push(deep(127));
    reports.push(deep(128));
    // The evaluator (slow on a whole run) replays the first few reports;
    // the JIT pipelines replay all of them.
    for (i, report) in reports.iter().enumerate() {
        let leaked: &'static [u8] = Box::leak(report.clone().into_boxed_slice());
        let s = Scenario {
            name: "fixed report",
            extraction: &["inc"],
            models: &["inc"],
            audit: Audit::Fixed(leaked),
            ..BASE
        };
        answers += check_scenario(&port, &s, leaked, i < 4);
        runs += 1;
    }
    eprintln!("proof lens runner: {runs} scenarios, {answers} rows and log lines, 0 diffs");
}

/// Wall time of the all-proved job, Rust and the port (JIT), each over
/// `CHECK_HARNESS_ROUNDS` runs (default 20) after one warm-up.
#[test]
#[ignore]
fn proof_run_speed() {
    if !jit_host() {
        return;
    }
    let rounds: u32 = std::env::var("CHECK_HARNESS_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(20);
    let port = port();
    let s = scenarios().remove(0);
    let tc = Temp::new("proof-lens-speed");
    let modules = job_for(&s, "launcher", None, None).modules();
    let (launcher, auditor) = toolchain(tc.path(), &s, &modules, b"");
    let job = job_for(&s, &launcher, auditor.as_deref(), None);
    let words = job.words();
    let want = rust_run(&job, tc.path());
    assert_eq!(vesper_run(&port, &words, Mode::Jit, tc.path()), want);
    let start = Instant::now();
    for _ in 0..rounds {
        let _ = rust_run(&job, tc.path());
    }
    let rust = start.elapsed() / rounds;
    let f = &port.jit.as_ref().unwrap()[0];
    fs::write(port.dir.path().join("job"), &words).unwrap();
    let start = Instant::now();
    for _ in 0..rounds {
        // SAFETY: `run_job` takes no arguments and returns one word.
        assert_eq!(unsafe { f.call(&[]) }.unwrap(), 0);
        let _ = take_log(tc.path());
    }
    let ported = start.elapsed() / rounds;
    eprintln!(
        "proof run (all proved): rust {rust:?}, vesper {ported:?}, {:.2}x",
        ported.as_secs_f64() / rust.as_secs_f64()
    );
}
