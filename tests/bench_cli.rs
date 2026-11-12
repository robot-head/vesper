use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-bench-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text.as_bytes()).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("bench")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let start = Instant::now();
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            timed_out = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().unwrap();
    let output = Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    };
    assert!(
        !timed_out,
        "source benchmark did not finish within the test deadline"
    );
    output
}
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn real_source_arms_compile_once_then_execute_exact_counts() {
    let fixture = Fixture::new();
    let candidate_text = "fn helper()->u64{42} fn main()->u64{helper()}";
    let baseline_text = "fn reference()->u64{41}";
    let candidate = fixture.source("candidate.ves", candidate_text);
    let baseline = fixture.source("baseline.ves", baseline_text);
    let output = run(&[
        path(&candidate),
        "--baseline",
        path(&baseline),
        "--baseline-entry",
        "reference",
        "--samples",
        "6",
        "--warmup",
        "0",
        "--iterations",
        "8",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = report(&output);
    assert_eq!(data["schema"], "vesper.bench/1");
    assert_eq!(data["status"], "measured");
    assert_eq!(data["provider"], "std::time::Instant");
    assert_eq!(data["unit"], "nanoseconds");
    assert_eq!(data["body_calls"], 192);
    assert_eq!(data["pairs"].as_array().unwrap().len(), 6);
    for arm in ["candidate", "baseline"] {
        assert_eq!(data[arm]["jit"]["modules"], 1);
        assert_eq!(data[arm]["jit"]["compilations"], 1);
        assert_eq!(data[arm]["jit"]["runs"], 96);
    }
    assert_eq!(data["candidate"]["jit"]["defined_functions"], 2);
    assert_eq!(data["baseline"]["jit"]["defined_functions"], 1);
    assert_ne!(
        data["candidate"]["source_blake3"],
        data["baseline"]["source_blake3"]
    );
    assert_eq!(data["evidence"]["hardware_pmu"], "unavailable");
    assert!(data["analysis"]["gate_decision"].is_null());
    assert!(data["pairs"][0]["regression_basis_points"]["floor"].is_string());
    assert_eq!(fs::read(&candidate).unwrap(), candidate_text.as_bytes());
    assert_eq!(fs::read(&baseline).unwrap(), baseline_text.as_bytes());
}

#[test]
fn measurement_only_and_requested_insufficient_gate_have_different_exit_contracts() {
    let fixture = Fixture::new();
    let source = fixture.source("main.ves", "fn main()->u64{42}");
    let args = [
        path(&source),
        "--samples",
        "1",
        "--warmup",
        "0",
        "--iterations",
        "8",
        "--json",
    ];
    let measured = run(&args);
    assert!(
        measured.status.success(),
        "{}",
        String::from_utf8_lossy(&measured.stderr)
    );
    assert_eq!(report(&measured)["status"], "measured");
    assert!(report(&measured)["analysis"]["interval"].is_null());
    let mut gated_args = args.to_vec();
    gated_args.extend(["--max-regression-bp", "1000000"]);
    let gated = run(&gated_args);
    assert!(!gated.status.success());
    let data = report(&gated); // Exactly one JSON document, including on gate failure.
    assert_eq!(data["status"], "inconclusive");
    assert_eq!(data["analysis"]["gate_decision"], "Inconclusive");
    assert_eq!(
        data["analysis"]["inconclusive_reason"],
        "InsufficientEpochs"
    );
}

#[test]
fn unsupported_source_and_invalid_arguments_never_enter_measurement() {
    let fixture = Fixture::new();
    for (name, source) in [
        ("recursive", "fn main()->u64{main()}"),
        ("aggregate", "fn main(){let x=0;let x=(x,x);let x=(x,x);}"),
        ("invalid", "fn main()->u64{missing}"),
    ] {
        let file = fixture.source(&format!("{name}.ves"), source);
        let output = run(&[path(&file), "--json"]);
        assert!(!output.status.success(), "{name}");
        let data = report(&output);
        assert_eq!(data["status"], "error");
        assert!(data.get("pairs").is_none());
        assert_eq!(fs::read(file).unwrap(), source.as_bytes());
    }
    for args in [
        vec!["--json"],
        vec!["missing.ves", "--samples", "0", "--json"],
        vec!["missing.ves", "--iterations", "65537", "--json"],
        vec![
            "missing.ves",
            "--samples",
            "64",
            "--warmup",
            "64",
            "--iterations",
            "65536",
            "--json",
        ],
        vec!["missing.ves", "--max-regression-bp", "-10000", "--json"],
        vec!["missing.ves", "--unknown", "--json"],
    ] {
        let output = run(&args);
        assert!(!output.status.success());
        assert_eq!(report(&output)["status"], "error");
    }
    assert!(run(&["--help"]).status.success());
}

#[test]
fn metric_flags_validate_and_help_names_real_provider_scope() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--pmu"));
    assert!(text.contains("--metric time|cycles|instructions"));
    assert!(text.contains("no clock fallback"));
    for args in [
        vec!["missing.ves", "--metric", "bytes", "--json"],
        vec!["missing.ves", "--metric", "--json"],
        vec!["missing.ves", "--pmu=true", "--json"],
    ] {
        let output = run(&args);
        assert!(!output.status.success());
        assert_eq!(report(&output)["status"], "error");
    }
}

#[test]
fn requested_hardware_metrics_are_real_or_structured_unavailable_without_fallback() {
    let fixture = Fixture::new();
    let source_text = "fn main()->u64{42}";
    let source = fixture.source("pmu.ves", source_text);
    let availability = match vesper_bench_pmu::PerfGroup::open() {
        Ok(group) => {
            drop(group);
            None
        }
        Err(error) => Some(error.reason),
    };
    for metric in ["time", "cycles", "instructions"] {
        let output = run(&[
            path(&source),
            "--metric",
            metric,
            "--pmu",
            "--samples",
            "6",
            "--warmup",
            "1",
            "--iterations",
            "8",
            "--json",
        ]);
        let data = report(&output);
        assert_eq!(data["metric"], metric);
        if output.status.success() {
            assert!(
                availability.is_none(),
                "child produced counters despite parent provider unavailable"
            );
            assert_eq!(data["status"], "measured");
            assert_eq!(data["pmu"]["status"], "measured");
            assert_eq!(data["pmu"]["provider"], "linux.perf_event_open.group_read");
            assert_eq!(data["clock"]["provider"], "std::time::Instant");
            assert_eq!(data["clock"]["unit"], "nanoseconds");
            assert_eq!(data["body_calls"], 224);
            assert_eq!(data["baseline"]["jit"]["runs"], 112);
            assert_eq!(data["candidate"]["jit"]["runs"], 112);
            let windows = data["pmu"]["windows"].as_array().unwrap();
            assert_eq!(windows.len(), 28);
            for (index, window) in windows.iter().enumerate() {
                assert!(window["cycles"].as_str().unwrap().parse::<u64>().unwrap() > 0);
                assert!(
                    window["instructions"]
                        .as_str()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                        > 0
                );
                assert!(
                    window["enabled_ns"]
                        .as_str()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                        > 0
                );
                assert_eq!(window["enabled_ns"], window["running_ns"]);
                assert_eq!(window["invocation"]["warmup"], index < 4);
                assert_eq!(window["completion"]["attempted_operations"], 8);
            }
            if metric == "time" {
                assert_eq!(data["provider"], "std::time::Instant");
                assert_eq!(data["unit"], "nanoseconds");
                assert_eq!(data["pairs"], data["clock"]["pairs"]);
            } else {
                assert_eq!(data["provider"], "linux.perf_event_open.group_read");
                assert_eq!(data["unit"], metric);
                assert_eq!(data["pairs"], data["pmu"][format!("{metric}_pairs")]);
                assert!(data["pairs"][0].get("baseline_total_ns").is_none());
                assert!(data["pairs"][0][format!("baseline_total_{metric}")].is_string());
            }
            assert_eq!(data["pairs"].as_array().unwrap().len(), 6);
            eprintln!("LIVE_PMU_CLI_ACTUAL_COUNTERS: metric={metric},28 actual observed windows");
        } else {
            assert_eq!(data["status"], "inconclusive", "{data}");
            assert_eq!(data["pmu"]["status"], "unavailable");
            let unavailable = data["pmu"]
                .get("unavailable")
                .unwrap_or(&data["observer_error"]["unavailable"]);
            let reason = unavailable["reason"].as_str().unwrap();
            assert!(
                unavailable["operation"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            assert!(
                unavailable
                    .get("errno")
                    .is_some_and(|v| v.is_null() || v.is_i64())
            );
            assert!(
                [
                    "UnsupportedPlatform",
                    "PermissionDenied",
                    "OsError",
                    "InvalidRead",
                    "InvalidMetadata",
                    "WrongProcessOrThread",
                    "InvalidState",
                    "ForeignOrStaleToken",
                    "Exhausted",
                    "CounterRegression",
                    "NotFullyScheduled",
                    "NoEvents"
                ]
                .contains(&reason)
            );
            if let Some(vesper_bench_pmu::UnavailableReason::PermissionDenied) = availability {
                assert_eq!(reason, "PermissionDenied");
            }
            assert!(data.get("pairs").is_none());
            assert!(data.get("analysis").is_none());
            assert!(data.get("clock").is_none());
            assert!(data["pmu"].get("windows").is_none());
            assert!(data["pmu"].get("cycles_pairs").is_none());
            assert!(data["pmu"].get("instructions_pairs").is_none());
            eprintln!(
                "LIVE_PMU_CLI_UNAVAILABLE: metric={metric},reason={reason}; no positive hardware coverage"
            );
        }
        assert_eq!(fs::read(&source).unwrap(), source_text.as_bytes());
    }
}

#[test]
fn hardware_metric_implies_pmu_and_unavailable_or_insufficient_evidence_cannot_pass_gate() {
    let fixture = Fixture::new();
    let source_text = "fn main()->i64{7}";
    let source = fixture.source("gate.ves", source_text);
    for metric in ["cycles", "instructions"] {
        let output = run(&[
            path(&source),
            "--metric",
            metric,
            "--samples",
            "1",
            "--warmup",
            "0",
            "--iterations",
            "8",
            "--max-regression-bp",
            "1000000",
            "--json",
        ]);
        assert!(!output.status.success());
        let data = report(&output);
        assert_eq!(data["status"], "inconclusive");
        if data["pmu"]["status"] == "measured" {
            assert_eq!(data["analysis"]["gate_decision"], "Inconclusive");
            assert_eq!(
                data["analysis"]["inconclusive_reason"],
                "InsufficientEpochs"
            );
        } else {
            assert_eq!(
                data["provider_requested"],
                "linux.perf_event_open.group_read"
            );
            assert!(data.get("pairs").is_none());
            assert!(data.get("analysis").is_none());
        }
        assert_eq!(fs::read(&source).unwrap(), source_text.as_bytes());
    }
}
