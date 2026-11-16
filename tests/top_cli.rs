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

#[test]
fn top_preserves_alternate_wait_for_cycles_sharing_a_suffix() {
    let f = Fixture::new();
    f.write_json(
        "diamond_cycle.json",
        &json!({
            "schema_version": 1,
            "source": "diamond",
            "fibers": [
                {
                    "fiber_id": 1,
                    "status": "parked",
                    "run_ns": 100,
                    "idle_ns": 100,
                    "obligations": [
                        { "obligation_id": 11, "kind": "Lock", "parked": true, "waiting_on_fiber": 2 },
                        { "obligation_id": 12, "kind": "Lock", "parked": true, "waiting_on_fiber": 3 }
                    ]
                },
                {
                    "fiber_id": 2,
                    "status": "parked",
                    "run_ns": 100,
                    "idle_ns": 100,
                    "waiting_on_fiber": 4
                },
                {
                    "fiber_id": 3,
                    "status": "parked",
                    "run_ns": 100,
                    "idle_ns": 100,
                    "waiting_on_fiber": 4
                },
                {
                    "fiber_id": 4,
                    "status": "parked",
                    "run_ns": 100,
                    "idle_ns": 100,
                    "waiting_on_fiber": 1
                }
            ]
        }),
    );

    let out = f.run(&["--trace", "diamond_cycle.json", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = json_out(&out);
    assert_eq!(r["deadlock_cycles"], json!([[1, 2, 4, 1], [1, 3, 4, 1]]));
}

#[test]
fn top_source_module_observes_live_obligations_and_active_effects() {
    let f = Fixture::new();
    f.write_str(
        "resource.ves",
        "linear struct Ticket {}\neffect Unused { fn ping() -> i64; }\nfn main() -> Ticket {\n    Ticket {}\n}\n",
    );

    let out = f.run(&["resource.ves", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = json_out(&out);
    assert_eq!(r["total_live_obligations"], 1);
    assert_eq!(r["live_obligations"][0]["kind"], "Ticket");
    // Declared-but-inactive effects must not appear as active handler frames after exit.
    assert_eq!(r["fibers"][0]["effect_stack"], json!([]));
}

#[test]
fn top_chrono_closes_session_decodes_scheduler_timing_and_attributes_handoff() {
    use vesper_chrono::{
        Bundle, BundleKey, FlightRecorder, HopKind, RecorderConfig, RedactionPolicy,
        leaked_sessions,
        simpulse::{SimPulseTap, TraceEvent},
    };

    let f = Fixture::new();
    let key_bytes = [0x42u8; 32];
    let key = BundleKey::new(key_bytes);
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();

    let rec_sched = |rec: &mut FlightRecorder, time_ns: u64, v: Value| {
        let ev: TraceEvent = serde_json::from_value(v).unwrap();
        SimPulseTap::record_event(rec, time_ns, &ev).unwrap();
    };
    rec_sched(
        &mut rec,
        1_000,
        json!({ "Spawn": { "task": 1, "at_ns": 1_000 } }),
    );
    rec_sched(
        &mut rec,
        3_500,
        json!({ "Dispatch": { "task": 1, "at_ns": 3_500, "round": 0 } }),
    );
    let ob_id = rec.mint_obligation(1, 5_000, 10, None).unwrap();
    rec.record_obligation_hop(
        1,
        6_000,
        ob_id,
        HopKind::ChannelHandoff { to_fiber: 99 },
        11,
    )
    .unwrap();
    rec_sched(
        &mut rec,
        10_500,
        json!({ "SubmitIo": { "task": 1, "completion": 1, "scheduled": null } }),
    );
    rec_sched(
        &mut rec,
        15_500,
        json!({ "ClockAdvance": { "from_ns": 10_500, "to_ns": 15_500 } }),
    );
    rec_sched(
        &mut rec,
        15_500,
        json!({ "IoCompleted": { "task": 1, "completion": 1, "at_ns": 15_500, "result": { "Success": { "bytes": 4 } } } }),
    );
    rec_sched(
        &mut rec,
        16_000,
        json!({ "Dispatch": { "task": 1, "at_ns": 16_000, "round": 1 } }),
    );
    rec_sched(
        &mut rec,
        20_000,
        json!({ "Finish": { "task": 1, "at_ns": 20_000 } }),
    );

    let bundle_bytes = Bundle::capture(&rec, &RedactionPolicy::new(), b"top-cli")
        .seal(&key)
        .unwrap();
    fs::write(f.0.join("session.vchrono"), bundle_bytes).unwrap();
    let hex_key: String = key_bytes.iter().map(|b| format!("{b:02x}")).collect();

    let out = f.run(&["--chrono", "session.vchrono", "--key", &hex_key, "--json"]);
    assert!(
        out.status.success(),
        "top --chrono failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = json_out(&out);
    assert_eq!(leaked_sessions(), 0);
    // ClockAdvance (fiber 0) should not produce a bogus fiber; only fiber 1 and destination fiber 99.
    assert_eq!(r["total_fibers"], 2);
    assert_eq!(r["fibers"][0]["fiber_id"], 1);
    assert_eq!(r["fibers"][0]["status"], "completed");
    assert_eq!(r["fibers"][0]["run_ns"], 11_000);
    assert_eq!(r["fibers"][0]["idle_ns"], 8_000);
    assert_eq!(r["fibers"][0]["live_linear_obligations"], 0);
    assert_eq!(r["fibers"][1]["fiber_id"], 99);
    assert_eq!(r["fibers"][1]["status"], "parked");
    assert_eq!(r["fibers"][1]["live_linear_obligations"], 1);
    assert_eq!(r["wake_to_poll"]["samples"], 2);
    assert_eq!(r["wake_to_poll"]["min_ns"], 500);
    assert_eq!(r["wake_to_poll"]["max_ns"], 2_500);
}
