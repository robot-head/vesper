//! Genuine three-model external replay plus independently authored refusal cases.
//! Synthetic protocol fixtures and transport markers are never replay evidence.
#[path = "../src/bin/lean4lean42/mod.rs"]
pub mod driver;

use driver::protocol::{ExpectedControl, Inventory, Machine};
use driver::{Config, Error, ErrorKind, Limits};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "00000000000000000000000000000000";
const CAPTURE: usize = 1024 * 1024;
static SERIAL: Mutex<()> = Mutex::new(());
const ROOTS: [&str; 33] = [
    "r01_add_zero",
    "r02_zero_add",
    "r03_sub_zero",
    "r04_mul_one",
    "r05_one_mul",
    "r06_mul_zero",
    "r07_zero_mul",
    "r08_self_sub",
    "r09_wadd_zero",
    "r10_wsub_zero",
    "r11_wmul_one",
    "r12_wadd_comm",
    "r13_wmul_comm",
    "r14_wadd_assoc",
    "r15_wmul_assoc",
    "r16_wadd_cancel",
    "r17_wmul_zero",
    "r18_wself_sub",
    "r19_checked_add_constants",
    "r20_checked_sub_constants",
    "r21_checked_mul_constants",
    "r22_wrapping_add_constants",
    "r23_wrapping_sub_constants",
    "r24_wrapping_mul_constants",
    "value_fits",
    "literal_exact",
    "value_toInt_injective",
    "literal_roundtrip",
    "total_const",
    "total_var",
    "total_of_eq",
    "congr_checked",
    "congr_wrapping",
];

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-lean4lean42-acceptance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("fresh private acceptance directory");
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binary() -> PathBuf {
    let given = PathBuf::from(env!("CARGO_BIN_EXE_lean4lean42"));
    let path = if given.components().count() > 1 || given.is_absolute() {
        given
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("Cargo binary PATH"))
            .flat_map(|directory| {
                let mut paths = vec![directory.join(&given)];
                if cfg!(windows) && given.extension().is_none() {
                    paths.push(directory.join(&given).with_extension("exe"));
                }
                paths
            })
            .find(|path| path.is_file())
            .expect("actual Cargo lean4lean42 binary")
    };
    path.canonicalize().expect("canonical actual binary")
}
fn hash_file(path: &Path) -> [u8; 32] {
    let mut input = File::open(path).unwrap();
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    *hash.finalize().as_bytes()
}
fn digest(value: &Value) -> [u8; 32] {
    let values = value.as_array().expect("digest array");
    assert_eq!(values.len(), 32);
    std::array::from_fn(|i| u8::try_from(values[i].as_u64().unwrap()).unwrap())
}
fn controls() -> Vec<ExpectedControl> {
    let manifest: Value =
        serde_json::from_slice(include_bytes!("../tools/lean4lean42/known-fixes.json")).unwrap();
    let controls: Vec<ExpectedControl> =
        serde_json::from_value(manifest["controls"].clone()).unwrap();
    assert_eq!(
        controls.len(),
        94,
        "published registry includes five shared metadata conditions"
    );
    controls
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Captured {
    fn text(&self) -> String {
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.stdout)
            .unwrap_or_else(|error| panic!("{error}\n{}", self.text()))
    }
}
fn drain(mut pipe: impl Read, overflow: &AtomicBool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = pipe.read(&mut buffer).expect("bounded capture read");
        if count == 0 {
            return bytes;
        }
        let room = CAPTURE.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(room)]);
        if count > room {
            overflow.store(true, Ordering::Relaxed);
        }
    }
}
fn execute(command: &mut Command, seconds: u64) -> Captured {
    let started = Instant::now();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("required subprocess {command:?}: {error}"));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let overflow = AtomicBool::new(false);
    let (status, stdout, stderr, refused) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, &overflow));
        let err = scope.spawn(|| drain(stderr, &overflow));
        let mut refused = false;
        let status = loop {
            if overflow.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(seconds)
            {
                refused = true;
                let _ = child.kill();
                break child.wait().unwrap();
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            thread::sleep(Duration::from_millis(20));
        };
        (status, out.join().unwrap(), err.join().unwrap(), refused)
    });
    let output = Captured {
        status,
        stdout,
        stderr,
    };
    assert!(
        !refused && !overflow.load(Ordering::Relaxed),
        "subprocess deadline/output cap: {command:?}\n{}",
        output.text()
    );
    eprintln!(
        "lean4lean42 acceptance subprocess {:.2}s: {command:?}",
        started.elapsed().as_secs_f64()
    );
    output
}

fn control_hash(records: &Value) -> [u8; 32] {
    fn text(hash: &mut blake3::Hasher, value: &str) {
        hash.update(&(value.len() as u32).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let records = records.as_array().unwrap();
    let mut hash = blake3::Hasher::new();
    hash.update(b"vesper.lean4lean42.controls\0");
    hash.update(&1u16.to_le_bytes());
    hash.update(&(records.len() as u32).to_le_bytes());
    for (index, record) in records.iter().enumerate() {
        hash.update(&(index as u32).to_le_bytes());
        text(&mut hash, record["id"].as_str().unwrap());
        hash.update(&[match record["kind"].as_str().unwrap() {
            "raw_declaration" => 0,
            "checker_expr" => 1,
            "condition" => 2,
            _ => panic!("control kind"),
        }]);
        hash.update(&[
            u8::from(record["expected_accept"].as_bool().unwrap()),
            u8::from(record["actual_accept"].as_bool().unwrap()),
        ]);
        text(&mut hash, record["expected_reason"].as_str().unwrap());
        text(&mut hash, record["actual_reason"].as_str().unwrap());
        hash.update(&[u8::from(record["matched"].as_bool().unwrap())]);
    }
    *hash.finalize().as_bytes()
}
fn validate_controls(report: &Value, expected: &[ExpectedControl]) {
    let actual = report["controls"].as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    let mut ids = BTreeSet::new();
    let mut kinds = [0usize; 3];
    let mut refused = 0;
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(ids.insert(actual["id"].as_str().unwrap()));
        assert_eq!(actual["id"], expected.id);
        assert_eq!(actual["expected_accept"], expected.expected_accept);
        assert_eq!(actual["actual_accept"], expected.expected_accept);
        assert_eq!(actual["expected_reason"], expected.expected_reason);
        assert_eq!(actual["actual_reason"], expected.expected_reason);
        assert_eq!(actual["matched"], true);
        let kind = match actual["kind"].as_str().unwrap() {
            "raw_declaration" => 0,
            "checker_expr" => 1,
            "condition" => 2,
            _ => panic!("kind"),
        };
        kinds[kind] += 1;
        if !expected.expected_accept {
            refused += 1;
        }
        assert_eq!(serde_json::to_value(expected.kind).unwrap(), actual["kind"]);
    }
    assert_eq!(kinds, [52, 15, 27]);
    assert_eq!(refused, 37);
    for (reason, count, accepted) in [
        ("metadata_equal", 2, true),
        ("constructor_metadata_mismatch", 1, false),
        ("recursor_metadata_mismatch", 2, false),
    ] {
        let matching: Vec<_> = actual
            .iter()
            .filter(|record| record["expected_reason"] == reason)
            .collect();
        assert_eq!(matching.len(), count);
        for record in matching {
            assert_eq!(record["kind"], "condition");
            assert_eq!(record["actual_accept"], accepted);
        }
    }
    for kind in ["theorem", "definition", "opaque"] {
        for (free, reason) in [("fvar", "declaration_fvar"), ("mvar", "declaration_mvar")] {
            let id = format!("closure.{free}.{kind}");
            let record = actual.iter().find(|record| record["id"] == id).unwrap();
            assert_eq!(record["kind"], "raw_declaration");
            assert_eq!(record["actual_accept"], false);
            assert_eq!(record["actual_reason"], reason);
        }
        let id = format!("closure.closed.{kind}");
        assert_eq!(
            actual.iter().find(|record| record["id"] == id).unwrap()["actual_accept"],
            true
        );
    }
    assert_eq!(
        digest(&report["controls_blake3"]),
        control_hash(&report["controls"])
    );
}

fn genuine_positive(
    binary: &Path,
    root: &Path,
    receipt: &Value,
    expected: &[ExpectedControl],
) -> Value {
    let artifacts = Directory::new();
    fs::write(artifacts.0.join("unrelated.txt"), b"preserve").unwrap();
    let before = hash_file(&root.join("setup-receipt.json"));
    let output = execute(
        Command::new(binary)
            .arg("--tool-root")
            .arg(root)
            .arg("--artifacts")
            .arg(&artifacts.0),
        2400,
    );
    assert!(output.status.success(), "{}", output.text());
    let report = output.json();
    assert_eq!(report["schema"], 2);
    assert_eq!(report["status"], "complete");
    assert_eq!(report["scope"], "eligible_fresh_closure");
    assert_eq!(report["axiom_audit"], "not_run");
    assert_eq!(report["implementation_proof_audit"], "not_run");
    for field in ["timeouts", "crashes", "protocol_errors"] {
        assert_eq!(report[field], 0);
    }
    assert!(report["process_spawns"].as_u64().unwrap() >= 7);
    let tools = &report["tools"];
    assert_eq!(tools["profile"], "vesper.lean4lean42/2");
    assert_eq!(tools["lean_release"], receipt["lean_release"]);
    assert_eq!(receipt["lean_release"], "leanprover/lean4:v4.24.0");
    assert_eq!(
        tools["lean_commit"],
        "797c613eb9b6d4ec95db23e3e00af9ac6657f24b"
    );
    assert_eq!(tools["os"], std::env::consts::OS);
    assert_eq!(tools["arch"], std::env::consts::ARCH);
    assert_eq!(digest(&tools["receipt_blake3"]), before);
    for field in [
        "checker_blake3",
        "compiler_blake3",
        "compiler_runtime_manifest_blake3",
        "source_manifest_blake3",
        "build_output_manifest_blake3",
        "control_inventory_blake3",
        "control_import_manifest_blake3",
        "resource_bundle_blake3",
    ] {
        assert_eq!(
            tools[field], receipt[field],
            "actual setup/run association {field}"
        );
    }
    assert_eq!(
        digest(&tools["pins_blake3"]),
        *blake3::hash(include_bytes!("../tools/lean4lean42/pins.json")).as_bytes()
    );
    assert_eq!(
        digest(&tools["known_fixes_blake3"]),
        *blake3::hash(include_bytes!("../tools/lean4lean42/known-fixes.json")).as_bytes()
    );
    assert_eq!(
        digest(&tools["resource_bundle_blake3"]),
        *blake3::hash(include_bytes!(
            "../tools/lean4lean42/resource-source-manifest.json"
        ))
        .as_bytes()
    );
    validate_controls(&report, expected);
    let targets = report["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 3);
    for (index, target) in targets.iter().enumerate() {
        assert_eq!(
            target["target"],
            match index {
                0 => "rewrite_witness",
                1 => "arithmetic_oracle",
                2 => "resource_model42_kernel_snapshot",
                _ => unreachable!(),
            }
        );
        assert_eq!(target["status"], "complete");
        assert_eq!(target["native_compile_exit"], 0);
        assert_eq!(target["external_exit"], 0);
        let inventory = &target["inventory"];
        let coverage = &target["coverage"];
        let eligible = inventory["eligibility"][0].as_u64().unwrap();
        assert!(eligible > 0);
        assert_eq!(coverage["covered"], eligible);
        assert_eq!(
            coverage["target_covered"],
            inventory["target_eligibility"][0]
        );
        assert!(coverage["target_covered"].as_u64().unwrap() > 0);
        let forms = inventory["forms"].as_array().unwrap();
        let skips = inventory["eligibility"].as_array().unwrap();
        assert_eq!(
            forms.iter().map(|n| n.as_u64().unwrap()).sum::<u64>(),
            skips.iter().map(|n| n.as_u64().unwrap()).sum::<u64>()
        );
        assert_eq!(coverage["skipped"], Value::Array(skips[1..].to_vec()));
        for field in [
            "remaining",
            "pending",
            "constructor_pending",
            "recursor_pending",
        ] {
            assert_eq!(coverage[field], 0);
        }
        for (field, form) in [("constructor_compares", 5), ("recursor_compares", 6)] {
            let count = coverage[field].as_u64().unwrap();
            assert!(count > 0 && count <= forms[form].as_u64().unwrap());
        }
        match index {
            0 => assert_eq!(coverage["required_roots"], 33),
            1 => assert_eq!(coverage["required_roots"], 37),
            2 => {
                assert_eq!(
                    coverage["required_roots"],
                    inventory["target_eligibility"][0]
                );
                assert!(
                    skips
                        .iter()
                        .skip(1)
                        .any(|count| count.as_u64().unwrap() > 0)
                );
                assert_eq!(
                    inventory["target_eligibility"],
                    json!([inventory["target_eligibility"][0], 0, 0, 0])
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(
            coverage["required_roots_covered"],
            coverage["required_roots"]
        );
        assert!(
            coverage["additions"].as_u64().unwrap() > 0
                && coverage["additions"].as_u64().unwrap() < eligible,
            "group additions and exact constant coverage are distinct"
        );
        for field in [
            "inventory_blake3",
            "coverage_blake3",
            "import_manifest_blake3",
            "stdout_blake3",
            "stderr_blake3",
        ] {
            assert_ne!(digest(&target[field]), [0; 32]);
        }
        if index < 2 {
            assert!(target["resource"].is_null());
            let source = if index == 0 {
                include_bytes!("../lean/RewriteWitness.lean").as_slice()
            } else {
                include_bytes!("../lean/ArithmeticOracle.lean").as_slice()
            };
            assert_eq!(target["source"]["bytes"], source.len());
            assert_eq!(
                digest(&target["source"]["blake3"]),
                *blake3::hash(source).as_bytes()
            );
        } else {
            let resource = &target["resource"];
            assert_eq!(resource["schema"], 1);
            assert_eq!(resource["scope"], "kernel_logical_projection");
            assert_eq!(resource["module"], "ResourceModel42Kernel");
            assert_eq!(
                resource["resource_bundle_blake3"],
                tools["resource_bundle_blake3"]
            );
            assert_eq!(
                resource["output_constants"],
                inventory["target_eligibility"][0]
            );
            assert_eq!(
                resource["output_constants"].as_u64().unwrap(),
                resource["retained_constants"].as_u64().unwrap()
                    + resource["proof_aliases"].as_u64().unwrap()
            );
            assert!(resource["omitted_constants"].as_u64().unwrap() > 0);
            assert!(resource["proof_aliases"].as_u64().unwrap() > 0);
            for field in ["source_manifest_blake3", "original_manifest_blake3"] {
                assert_ne!(digest(&resource[field]), [0; 32]);
            }
        }
    }
    assert_eq!(hash_file(&root.join("setup-receipt.json")), before);
    assert_eq!(
        fs::read(artifacts.0.join("unrelated.txt")).unwrap(),
        b"preserve"
    );
    inspect_artifacts(&artifacts.0, &report);
    let encoded =
        serde_json::to_string(&report).expect("serialize the actual successful CLI report");
    assert!(encoded.len() <= Limits::default().report_bytes);
    println!("LEAN4LEAN42_REPORT {encoded}");
    report
}

fn inspect_artifacts(parent: &Path, report: &Value) {
    let roots: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(roots.len(), 1, "one unique artifact directory");
    let root = roots[0].canonicalize().unwrap();
    let files = report["artifacts"].as_array().unwrap();
    assert!(files.len() <= Limits::default().artifact_files);
    for file in files {
        let relative = Path::new(file["path"].as_str().unwrap());
        assert!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        );
        assert_ne!(
            relative.file_name().unwrap(),
            "report.json",
            "summary cannot hash itself"
        );
        let candidates = [root.join(relative), parent.join(relative)];
        let actual: Vec<_> = candidates
            .into_iter()
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(actual.len(), 1);
        let path = actual[0].canonicalize().unwrap();
        assert!(path.starts_with(&root));
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            file["bytes"].as_u64().unwrap()
        );
        assert_eq!(hash_file(&path), digest(&file["blake3"]));
    }
    let association = &report["targets"][2]["resource"];
    let bundle: Value = serde_json::from_slice(include_bytes!(
        "../tools/lean4lean42/resource-source-manifest.json"
    ))
    .unwrap();
    assert_eq!(bundle["schema"], 1);
    assert_eq!(
        bundle["domain"],
        "vesper.lean4lean42.resource-source-bundle/1"
    );
    for source in bundle["files"].as_array().unwrap() {
        let source_path = source["path"].as_str().unwrap();
        let retained = if let Some(relative) = source_path.strip_prefix("lean/") {
            format!("resource/sources/{relative}")
        } else if source_path == "tools/resource-model42/KernelSnapshot.lean" {
            "resource/projector/KernelSnapshot.lean".to_owned()
        } else {
            assert_eq!(
                source_path,
                "docs/rfcs/resource-model42-source-manifest.json"
            );
            "resource/historical-source-manifest.json".to_owned()
        };
        let observed = files
            .iter()
            .find(|file| file["path"] == retained)
            .expect("every frozen resource source remains retained and hashed");
        assert_eq!(observed["bytes"], source["bytes"]);
        assert_eq!(observed["blake3"], source["blake3"]);
    }
    for field in ["snapshot_tool", "request", "producer_report", "projection"] {
        let association_file = &association[field];
        let relative = association_file["path"].as_str().unwrap();
        let matches = files
            .iter()
            .filter(|file| {
                file["path"] == relative || file["path"] == format!("resource/{relative}")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "actual retained resource association {field}"
        );
        assert_eq!(matches[0]["bytes"], association_file["bytes"]);
        assert_eq!(matches[0]["blake3"], association_file["blake3"]);
    }
    assert_eq!(
        digest(&association["snapshot_tool"]["blake3"]),
        *blake3::hash(include_bytes!(
            "../tools/resource-model42/KernelSnapshot.lean"
        ))
        .as_bytes()
    );
    assert_eq!(
        digest(&association["source_manifest_blake3"]),
        *blake3::hash(include_bytes!(
            "../docs/rfcs/resource-model42-source-manifest.json"
        ))
        .as_bytes()
    );
}

fn transport_faults(binary: &Path) {
    use driver::process::testing::{ProbeKind as P, probe};
    let limits = Limits {
        frame_bytes: 512,
        stderr_bytes: 1024,
        ..Limits::default()
    };
    for (mode, kind, expected) in [
        ("malformed", P::Malformed, Some(ErrorKind::Protocol)),
        ("oversize", P::Oversize, Some(ErrorKind::OutputLimit)),
        (
            "truncated_header",
            P::TruncatedHeader,
            Some(ErrorKind::Protocol),
        ),
        (
            "truncated_payload",
            P::TruncatedPayload,
            Some(ErrorKind::Protocol),
        ),
        ("wrong_token", P::WrongToken, Some(ErrorKind::Protocol)),
        ("stall", P::Stall, Some(ErrorKind::Timeout)),
        ("stderr_flood", P::StderrFlood, Some(ErrorKind::OutputLimit)),
        ("crash", P::Crash, Some(ErrorKind::Crash)),
        ("short_writes", P::ShortWrites, None),
        ("queue_pressure", P::QueuePressure, None),
        ("queue_cancel", P::QueueCancel, Some(ErrorKind::Timeout)),
    ] {
        let mut command = Command::new(binary);
        command.args(["--fault-peer", mode]);
        let timeout = if mode == "stall" {
            100
        } else if mode == "queue_cancel" {
            1500
        } else {
            5000
        };
        let result =
            probe(command, kind, timeout, &limits).expect("actual production session probe");
        assert_eq!(result.kind, kind);
        assert_eq!(result.observed, expected, "{mode}: {result:?}");
        assert!(
            result.child_reaped
                && result.stdout_joined
                && result.stderr_joined
                && result.watcher_joined,
            "actual reap/joins, including cancellation of a full queue: {result:?}"
        );
        assert!(
            result.max_retained_frame <= limits.frame_bytes
                && result.stderr_retained <= limits.stderr_bytes
        );
        assert!(result.max_queued_frames <= 4);
        match mode {
            "oversize" => assert_eq!(
                result.max_retained_frame, 0,
                "header limit before payload allocation"
            ),
            "stderr_flood" => assert_eq!(result.stderr_retained, limits.stderr_bytes),
            "short_writes" => assert_eq!(result.frames, 1),
            "queue_pressure" => {
                assert_eq!(result.frames, 64);
                assert_eq!(
                    result.max_queued_frames, 4,
                    "actual backpressure was exercised"
                );
            }
            "queue_cancel" => assert_eq!(
                result.max_queued_frames, 4,
                "timeout while bounded queue was under pressure"
            ),
            _ => {}
        }
    }
    let invalid = Limits {
        frame_bytes: 0,
        ..limits
    };
    let error = probe(Command::new(binary), P::Stall, 1, &invalid)
        .expect_err("invalid probe cap before child spawn");
    assert_eq!(error.kind, ErrorKind::InvalidConfig);
}

fn name(parts: &[&str]) -> Value {
    json!(
        parts
            .iter()
            .map(|part| json!([0, part]))
            .collect::<Vec<_>>()
    )
}
fn name_bytes(value: &Value) -> u64 {
    2 + value
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            if part[0] == 0 {
                5 + part[1].as_str().unwrap().len() as u64
            } else {
                9
            }
        })
        .sum::<u64>()
}
fn refresh_header(frames: &mut [Value]) {
    let mut forms = [0u64; 8];
    let mut eligibility = [0u64; 4];
    let mut owned = [0u64; 4];
    let mut bytes = 0u64;
    for constant in frames.iter().filter(|frame| frame[1] == 3) {
        let form = constant[5].as_u64().unwrap() as usize;
        let skip = constant[6].as_u64().unwrap() as usize;
        forms[form] += 1;
        eligibility[skip] += 1;
        if constant[4] == 0 {
            owned[skip] += 1;
        }
        bytes += name_bytes(&constant[3]);
    }
    frames[0][3] = json!(forms);
    frames[0][4] = json!(eligibility);
    frames[0][5] = json!(owned);
    frames[0][6] = json!(bytes);
}
fn hello(mode: u8, target: u8) -> Value {
    json!([1, 0, TOKEN, mode, target, "4.24.0", "vesper.lean4lean42/2"])
}
fn terminal(mode: u8, target: u8, counters: [u64; 14]) -> Value {
    let mut values = vec![json!(1), json!(9), json!(TOKEN), json!(mode), json!(target)];
    values.extend(counters.into_iter().map(|value| json!(value)));
    Value::Array(values)
}
fn feed(machine: &mut Machine<'_>, value: &Value) -> Result<(), Error> {
    machine.feed(&serde_json::to_vec(value).unwrap())
}

/// Source-only parser data for rejection tests. No candidate checks these
/// declarations, and no successful replay or run report is produced from them.
struct NegativeInventory {
    target: u8,
    frames: Vec<Value>,
    inventory: Inventory,
    inductive: u32,
    constructor: u32,
    recursor: u32,
    eq: u32,
    quotients: [u32; 4],
    unsafe_id: u32,
}
impl NegativeInventory {
    fn build(target: u8, limits: &Limits) -> Self {
        let mut constants = Vec::new();
        let witness_module = if target == 0 { 0 } else { 1 };
        for root in ROOTS {
            constants.push(json!([
                1,
                3,
                constants.len(),
                name(&["Vesper", "RewriteWitness", root]),
                witness_module,
                2,
                0,
                []
            ]));
        }
        if target == 1 {
            for root in ["checkExpr", "checkProgram", "evalSteps", "processRequest"] {
                constants.push(json!([
                    1,
                    3,
                    constants.len(),
                    name(&["Vesper", "ArithmeticOracle", root]),
                    0,
                    1,
                    0,
                    []
                ]));
            }
        }
        if target == 3 {
            for root in [
                "validate",
                "checkProgram",
                "validateInvocation",
                "evalSteps",
            ] {
                constants.push(json!([
                    1,
                    3,
                    constants.len(),
                    name(&["Vesper", "ResourceModel42", root]),
                    0,
                    1,
                    0,
                    []
                ]));
            }
            constants.push(json!([
                1,
                3,
                constants.len(),
                name(&["ResourceKernelProofAliases", "realization_0"]),
                0,
                2,
                0,
                []
            ]));
        }
        let namespace = match target {
            0 => "RewriteWitness",
            1 => "ArithmeticOracle",
            3 => "ResourceModel42",
            _ => unreachable!(),
        };
        let inductive = constants.len() as u32;
        constants.push(json!([
            1,
            3,
            inductive,
            name(&["Vesper", namespace, "NegativeFixture"]),
            0,
            4,
            0,
            [inductive]
        ]));
        let constructor = constants.len() as u32;
        constants.push(json!([
            1,
            3,
            constructor,
            name(&["Vesper", namespace, "NegativeFixture", "mk"]),
            0,
            5,
            0,
            [inductive]
        ]));
        let recursor = constants.len() as u32;
        constants.push(json!([
            1,
            3,
            recursor,
            name(&["Vesper", namespace, "NegativeFixture", "rec"]),
            0,
            6,
            0,
            [inductive]
        ]));
        let init_module = if target == 0 { 1 } else { 2 };
        // These source-only data still have to model quotient prerequisites:
        // eligible inductive Eq, its self-owned group, and an earlier group event.
        let eq = constants.len() as u32;
        constants.push(json!([1, 3, eq, name(&["Eq"]), init_module, 4, 0, [eq]]));
        let first = constants.len() as u32;
        let quotients = [first, first + 1, first + 2, first + 3];
        for parts in [
            vec!["Quot"],
            vec!["Quot", "ind"],
            vec!["Quot", "lift"],
            vec!["Quot", "mk"],
        ] {
            constants.push(json!([
                1,
                3,
                constants.len(),
                name(&parts),
                init_module,
                7,
                0,
                quotients
            ]));
        }
        let unsafe_id = constants.len() as u32;
        constants.push(json!([
            1,
            3,
            unsafe_id,
            name(&["Init", "negativeUnsafe"]),
            init_module,
            1,
            1,
            []
        ]));
        let modules = match target {
            0 => vec!["RewriteWitness", "Init"],
            1 => vec!["ArithmeticOracle", "RewriteWitness", "Init"],
            3 => vec!["ResourceModel42Kernel", "RewriteWitness", "Init"],
            _ => unreachable!(),
        };
        let mut forms = [0u64; 8];
        let mut eligibility = [0u64; 4];
        let mut owned = [0u64; 4];
        let mut bytes = 0u64;
        for constant in &constants {
            let form = constant[5].as_u64().unwrap() as usize;
            let skip = constant[6].as_u64().unwrap() as usize;
            forms[form] += 1;
            eligibility[skip] += 1;
            if constant[4] == 0 {
                owned[skip] += 1;
            }
            bytes += name_bytes(&constant[3]);
        }
        let mut frames = vec![json!([
            1,
            1,
            modules.len(),
            forms,
            eligibility,
            owned,
            bytes
        ])];
        for (id, module) in modules.iter().enumerate() {
            frames.push(json!([
                1,
                2,
                id,
                name(&[module]),
                format!("/negative-only/{module}.olean")
            ]));
        }
        frames.extend(constants);
        frames.push(json!([1, 4]));
        let mut machine = Machine::new(0, target, TOKEN, limits, &[], None).unwrap();
        feed(&mut machine, &hello(0, target)).unwrap();
        for frame in &frames {
            feed(&mut machine, frame).expect("well-shaped source-only native inventory data");
        }
        let mut counts = [0u64; 14];
        counts[4] = 1;
        feed(&mut machine, &terminal(0, target, counts)).unwrap();
        let parsed = machine.finish().unwrap();
        assert!(parsed.coverage.is_none());
        Self {
            target,
            frames,
            inventory: parsed.inventory.unwrap(),
            inductive,
            constructor,
            recursor,
            eq,
            quotients,
            unsafe_id,
        }
    }
    fn replay<'a>(&'a self, limits: &'a Limits, with_skip: bool) -> Machine<'a> {
        let mut machine =
            Machine::new(1, self.target, TOKEN, limits, &[], Some(&self.inventory)).unwrap();
        feed(&mut machine, &hello(1, self.target)).unwrap();
        for frame in &self.frames {
            feed(&mut machine, frame).unwrap();
        }
        if with_skip {
            feed(&mut machine, &json!([1, 6, self.unsafe_id, 1])).unwrap();
        }
        machine
    }
    fn coverage_prefix(&self, machine: &mut Machine<'_>) -> u64 {
        let mut sequence = 0;
        for id in 0..self.inductive {
            feed(machine, &json!([1, 5, sequence, 0, [id]])).unwrap();
            sequence += 1;
        }
        for (action, ids) in [
            (1, vec![self.inductive]),
            (3, vec![self.constructor]),
            (4, vec![self.recursor]),
            (1, vec![self.eq]),
            (2, self.quotients.to_vec()),
        ] {
            feed(machine, &json!([1, 5, sequence, action, ids])).unwrap();
            sequence += 1;
        }
        sequence
    }
}
fn rejected(machine: Machine<'_>, error: Error) {
    assert!(
        matches!(
            error.kind,
            ErrorKind::Protocol | ErrorKind::Coverage | ErrorKind::Controls
        ),
        "actual parser/coverage/control cause: {error:?}"
    );
    assert!(!error.detail.is_empty());
    assert!(machine.finish().is_err(), "failed parser is poisoned");
}

fn coverage_refusals(limits: &Limits) {
    for target in [0, 1, 3] {
        let fixture = NegativeInventory::build(target, limits);
        for which in 0..12 {
            let mut machine = fixture.replay(limits, which != 8);
            let error = match which {
                0 => feed(&mut machine, &terminal(1, target, [0; 14])).unwrap_err(),
                1 => feed(&mut machine, &json!([1, 5, 0, 0, [999999]])).unwrap_err(),
                2 => {
                    feed(&mut machine, &json!([1, 5, 0, 0, [0]])).unwrap();
                    feed(&mut machine, &json!([1, 5, 1, 0, [0]])).unwrap_err()
                }
                3 => feed(&mut machine, &json!([1, 5, 0, 3, [fixture.constructor]])).unwrap_err(),
                4 => feed(&mut machine, &json!([1, 5, 0, 3, [fixture.recursor]])).unwrap_err(),
                5 => feed(&mut machine, &json!([1, 5, 0, 1, [fixture.inductive, 0]])).unwrap_err(),
                6 => {
                    feed(&mut machine, &json!([1, 5, 0, 1, [fixture.eq]])).unwrap();
                    feed(&mut machine, &json!([1, 5, 1, 2, &fixture.quotients[..3]])).unwrap_err()
                }
                7 => feed(&mut machine, &json!([1, 6, fixture.unsafe_id, 2])).unwrap_err(),
                8 => feed(&mut machine, &json!([1, 5, 0, 0, [0]])).unwrap_err(),
                9 => feed(&mut machine, &json!([1, 5, 1, 0, [0]])).unwrap_err(),
                10 => feed(&mut machine, &json!([1, 5, 0, 0, [fixture.unsafe_id]])).unwrap_err(),
                11 => {
                    fixture.coverage_prefix(&mut machine);
                    // All ID claims are source-only parser data. Deliberately
                    // unresolved work prevents any synthetic completion.
                    let mut counters = [0; 14];
                    counters[0] = u64::from(fixture.inductive) + 3;
                    counters[1] = fixture.inventory.totals.eligibility[0];
                    counters[2] = 1;
                    counters[3] = 1;
                    counters[4] = 1;
                    counters[7] = fixture.inventory.totals.target_eligibility[0];
                    counters[8] = 1;
                    feed(&mut machine, &terminal(1, target, counters)).unwrap_err()
                }
                _ => unreachable!(),
            };
            rejected(machine, error);
        }
        // Native/replay identity includes Name components, origin and every
        // ordinal, not a generic historical count or dotted pretty string.
        for which in 0..5 {
            let mut frames = fixture.frames.clone();
            let position = frames
                .iter()
                .position(|frame| frame[1] == 3 && frame[2] == 0)
                .unwrap();
            match which {
                0 => {
                    frames[position][3] =
                        name(&["Vesper", "RewriteWitness", "missing_required_root"])
                }
                1 => frames[position][3] = name(&["Vesper.RewriteWitness.r01_add_zero"]),
                2 => frames[position][4] = json!(if target == 0 { 1 } else { 2 }),
                3 => frames[position][6] = json!(1),
                4 => frames[position][2] = json!(1),
                _ => unreachable!(),
            }
            // Keep generic totals coherent so root/origin/eligibility failures
            // cannot be satisfied merely by a changed aggregate name length.
            refresh_header(&mut frames);
            for (mode, expected) in [
                (0, None),
                (0, Some(&fixture.inventory)),
                (1, Some(&fixture.inventory)),
            ] {
                if target == 3 && mode == 0 && expected.is_none() && which <= 3 {
                    continue;
                }
                let mut machine = Machine::new(mode, target, TOKEN, limits, &[], expected).unwrap();
                feed(&mut machine, &hello(mode, target)).unwrap();
                let error = frames
                    .iter()
                    .find_map(|frame| feed(&mut machine, frame).err())
                    .expect("changed inventory must fail before events");
                rejected(machine, error);
            }
        }
    }
}

fn resource_route_and_skip_refusals(limits: &Limits, controls: &[ExpectedControl]) {
    for mode in 0..=4 {
        for target in 0..=4 {
            let permitted = matches!((mode, target), (0, 0..=3) | (1, 0 | 1 | 3) | (2 | 3, 2));
            if !permitted {
                let error = Machine::new(mode, target, TOKEN, limits, controls, None)
                    .err()
                    .expect("unsupported route cannot fall back to control target");
                assert_eq!(error.kind, ErrorKind::Protocol);
            }
        }
    }
    for (mode, target) in [(0, 3), (1, 3), (3, 2)] {
        let fixture = NegativeInventory::build(3, limits);
        let expected = (mode == 1).then_some(&fixture.inventory);
        let mut machine = Machine::new(mode, target, TOKEN, limits, controls, expected).unwrap();
        let mut old = hello(mode, target);
        old[6] = json!("vesper.lean4lean42/1");
        let error = feed(&mut machine, &old).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        rejected(machine, error);
    }
    let fixture = NegativeInventory::build(3, limits);
    for skip in [1, 2, 3] {
        let mut frames = fixture.frames.clone();
        let owned = frames
            .iter()
            .position(|frame| frame[1] == 3 && frame[4] == 0 && frame[5] == 1)
            .unwrap();
        frames[owned][6] = json!(skip);
        refresh_header(&mut frames);
        let mut machine = Machine::new(0, 3, TOKEN, limits, controls, None).unwrap();
        feed(&mut machine, &hello(0, 3)).unwrap();
        let error = frames
            .iter()
            .find_map(|frame| feed(&mut machine, frame).err())
            .expect("projected package skip cannot pass native inventory");
        assert_eq!(error.kind, ErrorKind::Coverage);
        rejected(machine, error);
    }
}

fn control_and_shape_refusals(limits: &Limits, expected: &[ExpectedControl]) {
    for which in 0..5 {
        let mut machine = Machine::new(2, 2, TOKEN, limits, expected, None).unwrap();
        feed(&mut machine, &hello(2, 2)).unwrap();
        let error = if which == 0 {
            feed(&mut machine, &json!([1, 7, expected.len() - 1])).unwrap_err()
        } else {
            feed(&mut machine, &json!([1, 7, expected.len()])).unwrap();
            let first = &expected[0];
            let mut frame = json!([
                1,
                8,
                0,
                first.id,
                u8::from(first.expected_accept),
                u8::from(first.expected_accept),
                first.expected_reason,
                first.expected_reason,
                1
            ]);
            match which {
                1 => frame[5] = json!(u8::from(!first.expected_accept)),
                2 => frame[4] = json!(u8::from(!first.expected_accept)),
                3 => frame[7] = json!("unrelated_fixture_error"),
                4 => {
                    feed(&mut machine, &frame).unwrap();
                }
                _ => unreachable!(),
            }
            feed(&mut machine, &frame).unwrap_err()
        };
        rejected(machine, error);
    }
    for payload in [
        br#"[2,0,"00000000000000000000000000000000",3,2,"4.24.0","vesper.lean4lean42/2"]"#
            .as_slice(),
        br#"[1,0,"00000000000000000000000000000001",3,2,"4.24.0","vesper.lean4lean42/2"]"#
            .as_slice(),
        br#"[1,0,"00000000000000000000000000000000",3.0,2,"4.24.0","vesper.lean4lean42/2"]"#
            .as_slice(),
        br#"[1,0,"00000000000000000000000000000000",3e0,2,"4.24.0","vesper.lean4lean42/2"]"#
            .as_slice(),
        br#"[1,0,"00000000000000000000000000000000",3,true,"4.24.0","vesper.lean4lean42/2"]"#
            .as_slice(),
        br#"[1,0,"00000000000000000000000000000000",3,2,"4.24.0","vesper.lean4lean42/2",0]"#
            .as_slice(),
        b"{}".as_slice(),
        b"[".as_slice(),
    ] {
        let mut machine = Machine::new(3, 2, TOKEN, limits, &[], None).unwrap();
        let error = machine.feed(payload).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        rejected(machine, error);
    }
    let fixture = NegativeInventory::build(0, limits);
    for which in 0..5 {
        let mut frames = fixture.frames.clone();
        let id = frames.iter().position(|frame| frame[1] == 3).unwrap();
        match which {
            0 => frames[id][3] = json!([[1, "00"]]),
            1 => frames[id][3] = json!([[1, "18446744073709551616"]]),
            2 => frames[id][5] = json!(8),
            3 => frames[id][6] = json!(4),
            4 => frames[id][7] = json!([fixture.inductive]),
            _ => unreachable!(),
        }
        let mut machine = Machine::new(0, 0, TOKEN, limits, &[], None).unwrap();
        feed(&mut machine, &hello(0, 0)).unwrap();
        let error = frames
            .iter()
            .find_map(|frame| feed(&mut machine, frame).err())
            .expect("invalid exact-name/form/owner data");
        rejected(machine, error);
    }
}

fn configuration(root: &Path, receipt: &Value) -> Config {
    Config {
        tool_root: root.to_path_buf(),
        lean: PathBuf::from(receipt["compiler_path"].as_str().unwrap()),
        artifacts: None,
        limits: Limits::default(),
    }
}
fn no_replay(failure: &driver::Failure) {
    let report = serde_json::to_value(&failure.partial).unwrap();
    assert_ne!(report["status"], "complete");
    for target in report["targets"].as_array().unwrap() {
        assert_ne!(target["status"], "complete");
        assert!(target["external_exit"].is_null());
        assert!(
            target["coverage"].is_null(),
            "binding failures cannot fabricate external coverage"
        );
    }
}
fn preflight_refusals(binary: &Path, root: &Path, receipt: &Value) {
    for which in 0..8 {
        let mut config = configuration(root, receipt);
        match which {
            0 => config.limits.frame_bytes = 0,
            1 => config.limits.frame_bytes = 65537,
            2 => config.limits.declarations = 524289,
            3 => config.limits.name_bytes = 32 * 1024 * 1024 + 1,
            4 => config.limits.report_bytes = 1024 * 1024 + 1,
            5 => config.limits.compiler_runtime_files = 257,
            6 => config.limits.build_output_files = 16385,
            7 => config.limits.replay_timeout_ms = 1800001,
            _ => unreachable!(),
        }
        let failure = driver::run(&config).expect_err("invalid limit before external execution");
        assert_eq!(failure.error.kind, ErrorKind::InvalidConfig);
        no_replay(&failure);
        assert_eq!(failure.partial.process_spawns, 0);
    }
    let missing = Directory::new();
    let config = configuration(&missing.0, receipt);
    let failure = driver::run(&config).expect_err("absent candidate is a hard failure");
    no_replay(&failure);
    assert!(matches!(
        failure.error.kind,
        ErrorKind::Io | ErrorKind::ToolIdentity | ErrorKind::UnsupportedCandidate
    ));
    let mut wrong = configuration(root, receipt);
    wrong.lean = binary.to_path_buf();
    let failure =
        driver::run(&wrong).expect_err("ordinary run cannot probe an unmatched executable as Lean");
    assert_eq!(failure.error.kind, ErrorKind::ToolIdentity);
    no_replay(&failure);
    assert_eq!(failure.partial.process_spawns, 0);
    for arguments in [
        vec!["--unknown"],
        vec!["--tool-root"],
        vec!["--module", "RewriteWitness"],
        vec!["--skip-missing"],
    ] {
        let output = execute(Command::new(binary).args(arguments), 10);
        assert!(!output.status.success());
        let failure = output.json();
        assert_eq!(failure["error"]["kind"], "invalid_config");
        assert_ne!(failure["partial"]["status"], "complete");
    }
}

/// All ordinary source/build files remain regular. Hard links are used only
/// for unchanged bytes; every mutation replaces its directory entry atomically.
fn clone_candidate(source: &Path) -> Directory {
    let destination = Directory::new();
    let mut pending = vec![(source.to_path_buf(), destination.0.clone())];
    let mut files = 0usize;
    let mut bytes = 0u64;
    while let Some((from, to)) = pending.pop() {
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            let target = to.join(entry.file_name());
            let ty = entry.file_type().unwrap();
            assert!(
                !ty.is_symlink(),
                "genuine prepared source/build envelope contains only regular entries"
            );
            if ty.is_dir() {
                fs::create_dir(&target).unwrap();
                pending.push((entry.path(), target));
            } else {
                assert!(ty.is_file());
                files += 1;
                bytes = bytes.checked_add(entry.metadata().unwrap().len()).unwrap();
                assert!(
                    files <= 32768 && bytes <= 3 * 1024 * 1024 * 1024,
                    "bounded candidate copy"
                );
                if fs::hard_link(entry.path(), &target).is_err() {
                    fs::copy(entry.path(), &target).unwrap();
                }
            }
        }
    }
    destination
}
fn replace_from(source: &Path, destination: &Path, suffix: &[u8]) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = destination.parent().unwrap().join(format!(
        ".acceptance-replace-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let permissions = fs::metadata(source).unwrap().permissions();
    let mut input = File::open(source).unwrap();
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .unwrap();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).unwrap();
    }
    output.write_all(suffix).unwrap();
    output.flush().unwrap();
    drop(input);
    drop(output);
    fs::set_permissions(&temporary, permissions).unwrap();
    fs::rename(temporary, destination).unwrap();
}
fn replace_json(path: &Path, value: &Value) {
    let scratch = Directory::new();
    let input = scratch.0.join("receipt.json");
    fs::write(&input, serde_json::to_vec(value).unwrap()).unwrap();
    replace_from(&input, path, &[]);
}
fn binding_refusals(root: &Path, receipt: &Value) {
    let before = hash_file(&root.join("setup-receipt.json"));
    let copy = clone_candidate(root);
    let baseline = driver::artifact::verify_sources(&copy.0)
        .expect("copied genuine source set retains its exact pinned identity");
    assert_eq!(baseline, digest(&receipt["source_manifest_blake3"]));
    let source = receipt["source_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"].as_str().unwrap().ends_with(".lean"))
        .unwrap()["path"]
        .as_str()
        .unwrap();
    let output = receipt["build_output_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["path"]
                .as_str()
                .unwrap()
                .ends_with("VesperControls.olean")
        })
        .unwrap()["path"]
        .as_str()
        .unwrap();
    for relative in [source, output] {
        replace_from(&root.join(relative), &copy.0.join(relative), b"\n");
        let failure = driver::run(&configuration(&copy.0, receipt))
            .expect_err("changed exact source/control artifact cannot be bound");
        assert!(matches!(
            failure.error.kind,
            ErrorKind::ToolIdentity | ErrorKind::UnsupportedCandidate
        ));
        no_replay(&failure);
        replace_from(&root.join(relative), &copy.0.join(relative), &[]);
    }
    for relative in [
        "AcceptanceUnexpected.lean",
        ".lake/build/acceptance-unexpected.trace",
    ] {
        let path = copy.0.join(relative);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap()
            .write_all(b"unexpected fixed acceptance input\n")
            .unwrap();
        let failure = driver::run(&configuration(&copy.0, receipt))
            .expect_err("complete source/build file sets reject additions");
        assert!(matches!(
            failure.error.kind,
            ErrorKind::ToolIdentity | ErrorKind::UnsupportedCandidate
        ));
        no_replay(&failure);
        fs::remove_file(path).unwrap();
    }
    for which in 0..6 {
        let mut altered = receipt.clone();
        match which {
            0 => altered["unrecognized_acceptance_field"] = json!(true),
            1 => {
                altered["control_inventory_blake3"][0] =
                    json!(altered["control_inventory_blake3"][0].as_u64().unwrap() ^ 1)
            }
            2 => {
                altered["controls"][0]["actual_accept"] =
                    json!(!altered["controls"][0]["actual_accept"].as_bool().unwrap());
                altered["controls"][0]["matched"] = json!(true);
            }
            3 => {
                altered["build_output_manifest_blake3"][0] =
                    json!(altered["build_output_manifest_blake3"][0].as_u64().unwrap() ^ 1)
            }
            4 => {
                altered["resource_bundle_blake3"][0] =
                    json!(altered["resource_bundle_blake3"][0].as_u64().unwrap() ^ 1)
            }
            5 => altered["schema"] = json!(1),
            _ => unreachable!(),
        }
        replace_json(&copy.0.join("setup-receipt.json"), &altered);
        let failure = driver::run(&configuration(&copy.0, receipt))
            .expect_err("receipt claims are never runtime approval");
        assert!(matches!(
            failure.error.kind,
            ErrorKind::ToolIdentity
                | ErrorKind::UnsupportedCandidate
                | ErrorKind::Controls
                | ErrorKind::Protocol
        ));
        no_replay(&failure);
        replace_from(
            &root.join("setup-receipt.json"),
            &copy.0.join("setup-receipt.json"),
            &[],
        );
    }
    assert_eq!(driver::artifact::verify_sources(&copy.0).unwrap(), baseline);
    assert_eq!(
        hash_file(&root.join("setup-receipt.json")),
        before,
        "genuine prepared root was not modified"
    );
}

fn resource_binding_and_inventory_refusals(root: &Path, receipt: &Value, positive: &Value) {
    use driver::protocol::Component;
    let config = configuration(root, receipt);
    let prepared = driver::artifact::prepare_resource(&config).expect(
        "genuine fixed source compilation and projection, never a caller-created descriptor",
    );
    prepared.revalidate(&config.limits).unwrap();
    let association = serde_json::to_value(prepared.association()).unwrap();
    assert_eq!(
        association["resource_bundle_blake3"],
        receipt["resource_bundle_blake3"]
    );
    assert_eq!(
        association["projection"]["blake3"],
        positive["targets"][2]["resource"]["projection"]["blake3"]
    );
    let inventory = driver::artifact::resource_inventory(&config, &prepared)
        .expect("actual production native-inventory route on the newly prepared projection");
    prepared.validate_inventory(&inventory).unwrap();
    let owned_module = inventory
        .modules
        .iter()
        .find(|module| module.name.is(&["ResourceModel42Kernel"]))
        .expect("actual projected module")
        .id;
    let aliases = prepared.aliases();
    let catalog = prepared.catalog();
    assert!(!aliases.is_empty());
    assert_eq!(
        catalog.len(),
        284,
        "fixed source theorem catalog includes all 78 semantic assertions"
    );
    let alias_index = inventory
        .constants
        .iter()
        .position(|constant| {
            constant.module_id == owned_module
                && constant.name.resource_display().ok().as_deref() == Some(aliases[0].as_str())
        })
        .expect("real separately preserved alternate proof");
    let api_index = inventory
        .constants
        .iter()
        .position(|constant| {
            constant.module_id == owned_module
                && constant
                    .name
                    .is(&["Vesper", "ResourceModel42", "evalSteps"])
        })
        .expect("actual evaluator API definition");
    let ordinary_index = inventory
        .constants
        .iter()
        .position(|constant| {
            constant.module_id == owned_module
                && constant.form == 1
                && !constant
                    .name
                    .is(&["Vesper", "ResourceModel42", "evalSteps"])
        })
        .expect("ordinary retained declaration, beyond requested theorem roots");
    let number_index = inventory
        .constants
        .iter()
        .position(|constant| {
            constant.module_id == owned_module
                && constant
                    .name
                    .components
                    .iter()
                    .any(|component| matches!(component, Component::Number(_)))
        })
        .expect("actual private/generated name with numeric component");
    let module_index = inventory
        .modules
        .iter()
        .position(|module| module.id == owned_module)
        .unwrap();
    for which in 0..9 {
        let mut changed = inventory.clone();
        match which {
            0 => changed.constants[alias_index]
                .name
                .components
                .push(Component::String("AcceptanceMissingAlias".into())),
            1 => changed.constants[ordinary_index]
                .name
                .components
                .push(Component::String("AcceptanceUnexpectedDeclaration".into())),
            2 => changed.constants[alias_index].form = 1,
            3 => changed.constants[api_index].form = 2,
            4 => changed.constants[alias_index].skip = 1,
            5 => {
                changed.constants[ordinary_index].name = changed.constants[alias_index].name.clone()
            }
            6 => changed.modules[module_index].path.push_str(".substituted"),
            7 => changed.target = 0,
            8 => {
                changed.constants[alias_index].module_id = inventory
                    .modules
                    .iter()
                    .find(|module| module.id != owned_module)
                    .unwrap()
                    .id
            }
            _ => unreachable!(),
        }
        let error = prepared
            .validate_inventory(&changed)
            .expect_err("actual descriptor rejects altered native association");
        assert_eq!(
            error.kind,
            ErrorKind::ToolIdentity,
            "mutation {which}: {error}"
        );
        assert!(!error.detail.is_empty());
    }
    for unsupported in ["0", "a.b", "_@bad", "λ"] {
        let mut changed = inventory.clone();
        changed.constants[ordinary_index]
            .name
            .components
            .push(Component::String(unsupported.into()));
        let error = prepared.validate_inventory(&changed).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Coverage);
        assert_eq!(error.detail, "unsupported resource-owned name component");
    }
    let mut number_as_string = inventory.clone();
    let component = number_as_string.constants[number_index]
        .name
        .components
        .iter_mut()
        .find(|component| matches!(component, Component::Number(_)))
        .unwrap();
    let Component::Number(number) = component else {
        unreachable!()
    };
    *component = Component::String(number.to_string());
    let error = prepared.validate_inventory(&number_as_string).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Coverage);
    assert_eq!(error.detail, "unsupported resource-owned name component");
    // All mutations above operate on actual observed carrier metadata. They
    // are refusal evidence; none produces a successful external replay report.
    prepared.validate_inventory(&inventory).unwrap();

    for path in [
        prepared.source_path(),
        prepared.projection_path(),
        prepared.request_path(),
        prepared.report_path(),
        prepared.root().join("originals/MetatheoryFull.olean"),
        prepared.root().join("projector/KernelSnapshot.lean"),
    ] {
        let backup = Directory::new();
        let original = backup.0.join("actual-bound-bytes");
        fs::copy(&path, &original).unwrap();
        replace_from(&original, &path, b"\n");
        let error = prepared
            .revalidate(&config.limits)
            .expect_err("changed source/object/request/report cannot remain bound");
        assert_eq!(error.kind, ErrorKind::ToolIdentity);
        replace_from(&original, &path, &[]);
        prepared.revalidate(&config.limits).unwrap();
    }
    let report_path = prepared.report_path();
    let bytes = fs::read(&report_path).unwrap();
    assert!(bytes.len() <= 16 * 1024 * 1024);
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    for which in 0..3 {
        let mut altered = original.clone();
        match which {
            0 => {
                altered["proof_aliases"][0]["name"] =
                    json!("ResourceKernelProofAliases.substituted")
            }
            1 => altered["omitted"][0]["parent"] = json!("AcceptanceUnrelatedParent"),
            2 => altered["occurrences"][0]["module"] = json!("AcceptanceWrongOrigin"),
            _ => unreachable!(),
        }
        replace_json(&report_path, &altered);
        let error = prepared
            .revalidate(&config.limits)
            .expect_err("well-shaped changed producer mappings cannot be rebound");
        assert_eq!(error.kind, ErrorKind::ToolIdentity);
        let backup = Directory::new();
        let file = backup.0.join("producer-report.json");
        fs::write(&file, &bytes).unwrap();
        replace_from(&file, &report_path, &[]);
    }
    for path in [
        prepared.projection_path(),
        prepared.root().join("originals/MetatheoryFull.olean"),
    ] {
        let backup = Directory::new();
        let file = backup.0.join("actual-object");
        fs::copy(&path, &file).unwrap();
        fs::remove_file(&path).unwrap();
        let error = prepared
            .revalidate(&config.limits)
            .expect_err("missing bound original/projection cannot remain accepted");
        assert_eq!(error.kind, ErrorKind::ToolIdentity);
        replace_from(&file, &path, &[]);
    }
    let sidecar = prepared.projection_path().with_extension("olean.private");
    fs::write(&sidecar, b"unbound projected sidecar").unwrap();
    let error = prepared
        .revalidate(&config.limits)
        .expect_err("unbound sidecar cannot hide behind base-object confinement");
    assert_eq!(error.kind, ErrorKind::ToolIdentity);
    fs::remove_file(sidecar).unwrap();
    prepared.revalidate(&config.limits).unwrap();
    prepared.validate_inventory(&inventory).unwrap();
}

#[test]
// Platform-neutral Lean replay; CI runs it on Linux only (docs/ci-platforms.md),
// and macOS skips it in BUILD.bazel. `cargo test -- --ignored` still runs it.
#[cfg_attr(
    windows,
    ignore = "long platform-neutral Lean replay; runs on Linux CI (docs/ci-platforms.md)"
)]
fn genuine_three_model_replay_and_independent_refusal_matrix() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let started = Instant::now();
    let root=std::env::var_os("VESPER_LEAN4LEAN42_ROOT").map(PathBuf::from)
        .expect("VESPER_LEAN4LEAN42_ROOT must name the genuinely prepared candidate; missing is HARDFAIL, never skip");
    let root = root
        .canonicalize()
        .expect("actual prepared candidate directory");
    assert!(root.is_dir());
    let receipt_bytes =
        fs::read(root.join("setup-receipt.json")).expect("genuine sealed setup receipt");
    assert!(receipt_bytes.len() <= 4 * 1024 * 1024);
    let receipt: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let expected = controls();
    let executable = binary();
    let limits = Limits::default();
    transport_faults(&executable);
    coverage_refusals(&limits);
    resource_route_and_skip_refusals(&limits, &expected);
    control_and_shape_refusals(&limits, &expected);
    preflight_refusals(&executable, &root, &receipt);
    let positive = genuine_positive(&executable, &root, &receipt, &expected);
    resource_binding_and_inventory_refusals(&root, &receipt, &positive);
    binding_refusals(&root, &receipt);
    eprintln!(
        "lean4lean42 complete serial acceptance wall time {:.2}s",
        started.elapsed().as_secs_f64()
    );
}
