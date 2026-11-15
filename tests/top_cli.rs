use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-top-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn write_str(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).unwrap();
    }

    fn write_json(&self, name: &str, val: &Value) {
        fs::write(self.0.join(name), serde_json::to_vec_pretty(val).unwrap()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .arg("top")
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn json_out(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn top_trace_computes_latency_histograms_obligations_and_wait_cycles() {
    let f = Fixture::new();
    f.write_json(
        "trace.json",
        &json!({
            "schema_version": 1,
            "source": "cluster-shard-0",
            "fibers": [
                {
                    "fiber_id": 10,
                    "worker_core": 0,
                    "status": "parked",
                    "run_ns": 8000,
                    "idle_ns": 2000,
                    "wake_to_poll_ns": [400, 1500, 12000],
                    "effect_stack": ["Storage", "QuarryTxn"],
                    "obligations": [
                        {
                            "obligation_id": 101,
                            "kind": "WriteTxn",
                            "parked": true,
                            "waiting_on_obligation": 202
                        }
                    ]
                },
                {
                    "fiber_id": 20,
                    "worker_core": 1,
                    "status": "parked",
                    "run_ns": 3000,
                    "idle_ns": 7000,
                    "wake_to_poll_ns": [900, 250000, 1500000],
                    "effect_stack": ["Pulse::Channel", "Async"],
                    "obligations": [
                        {
                            "obligation_id": 202,
                            "kind": "LinearTx",
                            "parked": true,
                            "waiting_on_fiber": 10
                        }
                    ]
                },
                {
                    "fiber_id": 30,
                    "worker_core": 2,
                    "status": "running",
                    "run_ns": 9500,
                    "idle_ns": 500,
                    "wake_to_poll_ns": [300],
                    "effect_stack": ["Net"],
                    "obligations": [
                        {
                            "obligation_id": 303,
                            "kind": "InFlightRead",
                            "parked": false
                        }
                    ]
                }
            ]
        }),
    );

    let out = f.run(&["--trace", "trace.json", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = json_out(&out);
    assert_eq!(r["total_fibers"], 3);
    assert_eq!(r["parked_fibers"], 2);
    assert_eq!(r["total_live_obligations"], 3);
    assert_eq!(r["wake_to_poll"]["samples"], 7);
    assert_eq!(r["wake_to_poll"]["min_ns"], 300);
    assert_eq!(r["wake_to_poll"]["max_ns"], 1_500_000);
    assert_eq!(r["deadlock_cycles"], json!([[10, 20, 10]]));

    // Human mode renders Appendix B.4 banner + DEADLOCK CYCLE line.
    let human = f.run(&["--trace", "trace.json"]);
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("vesper "));
    assert!(lines[2].contains("modes: copy <= affine <= linear | 0 sorryAx"));
    assert!(text.contains("DEADLOCK CYCLE: fiber 10 -> fiber 20 -> fiber 10"));

    // --fail-on-cycle exits non-zero.
    let blocked = f.run(&["--trace", "trace.json", "--fail-on-cycle", "--json"]);
    assert!(!blocked.status.success());
}

#[test]
fn top_executes_ves_module_and_reads_env_trace_path() {
    let f = Fixture::new();
    f.write_str("prog.ves", "fn main() -> i64 {\n    42\n}\n");

    let run = f.run(&["prog.ves", "--write-trace", "captured.json", "--json"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let r = json_out(&run);
    assert_eq!(r["total_fibers"], 1);
    assert_eq!(r["deadlock_cycles"], json!([]));
    assert!(f.0.join("captured.json").is_file());

    // Re-read captured trace via VESPER_PULSE_TRACE_PATH without CLI flag.
    let via_env = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(&f.0)
        .env("VESPER_PULSE_TRACE_PATH", "captured.json")
        .args(["top", "--fail-on-cycle", "--json"])
        .output()
        .unwrap();
    assert!(
        via_env.status.success(),
        "{}",
        String::from_utf8_lossy(&via_env.stderr)
    );
    let r2 = json_out(&via_env);
    assert_eq!(r2["total_fibers"], 1);
}
