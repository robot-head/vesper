//! Differential test of the Rust frame accounting against the executable Lean
//! count model in `lean/WarpUmem.lean`.
//!
//! A randomized software-NIC run records every transition (driver and NIC)
//! and snapshots the auditor's per-location counts, which it computes from the
//! real rings and owners. The transition log is replayed through Lean's
//! `apply` and through the Rust count model, and all three must agree at every
//! snapshot. This is evidence about the replayed runs; the general statement
//! is the Lean theorems, which are about the count model, not the Rust code.

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use common::{Echo, Rng, random_fault, random_packet};
use vesper_warp_umem::{
    ConservationError, LOCATIONS, Location, SoftNic, SoftNicMode, SoftXsk, TransitionKind,
    UmemAuditor, UmemConfig, XdpNic, XdpUmemAccounting,
};

const MODEL_ID: &str = "vesper.warp.umem.conservation-model/1";
const MODEL_CLAIM: &str =
    "count model of UMEM frame locations; not a proof about the Rust implementation";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const AUDITED_THEOREMS: usize = 11;

const ALL_KINDS: [TransitionKind; 21] = [
    TransitionKind::NicPullFill,
    TransitionKind::NicDeliver,
    TransitionKind::Harvest,
    TransitionKind::HarvestReject,
    TransitionKind::Recycle,
    TransitionKind::RejectToFree,
    TransitionKind::RecycleToFree,
    TransitionKind::TurnAround,
    TransitionKind::AllocTx,
    TransitionKind::AbandonTx,
    TransitionKind::Submit,
    TransitionKind::NicConsumeTx,
    TransitionKind::NicComplete,
    TransitionKind::CompleteLeased,
    TransitionKind::CompleteRetired,
    TransitionKind::CompleteLeaked,
    TransitionKind::Retire,
    TransitionKind::Replenish,
    TransitionKind::LeakRx,
    TransitionKind::LeakTx,
    TransitionKind::LeakInFlight,
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

fn lean_source() -> String {
    std::fs::read_to_string(root().join("WarpUmem.lean")).expect("lean/WarpUmem.lean")
}

/// The Lean `Trans`/`Loc` vocabulary must be exactly the Rust one.
#[test]
fn lean_and_rust_vocabularies_agree() {
    let src = lean_source();
    assert!(src.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(src.contains(MODEL_CLAIM));
    for kind in ALL_KINDS {
        let name = kind.lean_name();
        assert!(
            src.contains(&format!("| \"{name}\" => some .{name}")),
            "Lean Trans.ofName lacks {name}"
        );
        let (from, to) = kind.endpoints();
        assert!(
            src.contains(&format!("  | .{name} => .{}\n", from.lean_name())),
            "Lean Trans.src of {name}"
        );
        assert!(
            src.contains(&format!("  | .{name} => .{}\n", to.lean_name())),
            "Lean Trans.dst of {name}"
        );
    }
    let census: Vec<&str> = LOCATIONS.iter().map(|l| l.lean_name()).collect();
    let rendered = format!("[c.{}", census.join(", c."));
    let normalized: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(&rendered.split_whitespace().collect::<Vec<_>>().join(" ")),
        "Census.render order"
    );
    assert_eq!(src.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for banned in ["sorry", "admit"] {
        assert!(!src.contains(banned), "Lean model contains {banned}");
    }
    assert!(
        !src.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

/// Compiles the model (kernel-checking the proofs and the axiom report),
/// then runs the fixture on `input`. Returns the header and output lines.
fn run_lean(lean: &str, input: &str) -> (String, Vec<String>) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-warp-umem-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let compiled = std::process::Command::new(lean)
        .current_dir(root())
        .arg("-o")
        .arg(dir.join("WarpUmem.olean"))
        .arg("WarpUmem.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    let stderr = String::from_utf8_lossy(&compiled.stderr);
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{stderr}",
        compiled.status
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with("does not depend on any axioms") {
            audited += 1;
            continue;
        }
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS, "axiom report lines");

    let mut child = std::process::Command::new(lean)
        .env("LEAN_PATH", lean_path(lean, &dir))
        .arg("--run")
        .arg(root().join("WarpUmem/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let owned = input.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(owned.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines().map(str::to_string);
    let header = lines.next().expect("header");
    (header, lines.collect())
}

/// `dir` plus the toolchain's own library directory, so `import` resolves
/// both the freshly built model and Lean's prelude.
fn lean_path(lean: &str, dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Ok(out) = std::process::Command::new(lean)
        .arg("--print-libdir")
        .output()
        && out.status.success()
    {
        let lib = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !lib.is_empty() {
            paths.push(PathBuf::from(lib));
        }
    }
    std::env::join_paths(paths).expect("LEAN_PATH")
}

fn render(acc: &XdpUmemAccounting) -> String {
    LOCATIONS
        .iter()
        .map(|l| acc.get(*l).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

struct Recorded {
    frames: u32,
    trace: Vec<TransitionKind>,
    /// (number of transitions recorded, auditor counts at that moment).
    snapshots: Vec<(usize, XdpUmemAccounting)>,
}

/// A randomized run with faults and backpressure, ending with deliberately
/// dropped owners so that the leak transitions appear too.
fn record(seed: u64, steps: u32) -> Recorded {
    let mut rng = Rng(seed);
    let frames = 24 + rng.below(24) as u32;
    let cfg = UmemConfig {
        frame_count: frames,
        frame_size: 2048,
        headroom: 128,
        fill_size: frames.next_power_of_two(),
        rx_size: 8,
        tx_size: 4,
        completion_size: 4,
    };
    let mode = if seed.is_multiple_of(2) {
        SoftNicMode::Loopback
    } else {
        SoftNicMode::Scripted
    };
    let mut xsk = SoftNic::xsk(cfg, mode).unwrap();
    xsk.backend_mut().enable_trace();
    let mut auditor = UmemAuditor::new();
    let mut snapshots = Vec::new();
    let mut snap = |x: &mut SoftXsk, snapshots: &mut Vec<_>| {
        let acc = auditor.counts(x).expect("structurally consistent");
        snapshots.push((x.backend().trace().len(), acc));
    };
    snap(&mut xsk, &mut snapshots);
    xsk.replenish_fill(frames / 2);
    snap(&mut xsk, &mut snapshots);
    let mut echo = Echo::new(frames as usize);
    for _ in 0..steps {
        for _ in 0..rng.below(3) {
            let packet = random_packet(&mut rng, 900);
            xsk.backend_mut().queue_ingress(packet);
        }
        if rng.chance(15) {
            let fault = random_fault(&mut rng);
            xsk.backend_mut().inject(fault);
        }
        xsk.backend_mut().step();
        snap(&mut xsk, &mut snapshots);
        let mut check = |x: &mut SoftXsk| snap(x, &mut snapshots);
        echo.step(&mut xsk, &mut rng, &mut check);
    }
    // Deliberate leaks: a harvested frame, a staged frame and leases.
    // Drain completions first so fill buffers and frames are available.
    for _ in 0..8 {
        xsk.backend_mut().step();
        xsk.harvest_completions(u32::MAX);
        xsk.replenish_fill(u32::MAX);
    }
    loop {
        match xsk.harvest_rx() {
            Ok(Some(frame)) => xsk.recycle_to_fill_ring(frame).unwrap(),
            Ok(None) => break,
            Err(_) => {}
        }
    }
    for _ in 0..3 {
        assert!(matches!(
            xsk.backend_mut().deliver(&[1; 64]),
            vesper_warp_umem::DeliverOutcome::Delivered { .. }
        ));
    }
    let mut harvested = Vec::new();
    while harvested.len() < 2 {
        match xsk.harvest_rx() {
            Ok(Some(frame)) => harvested.push(frame),
            Ok(None) => panic!("deliveries pending"),
            Err(_) => {}
        }
    }
    let staged = xsk
        .turn_around_tx_in_place(harvested.pop().unwrap(), 8)
        .unwrap();
    drop(staged);
    drop(harvested);
    // A lease dropped before its transmit completes.
    let third = loop {
        match xsk.harvest_rx() {
            Ok(Some(frame)) => break frame,
            Ok(None) => panic!("third delivery pending"),
            Err(_) => {}
        }
    };
    let tx = xsk.turn_around_tx_in_place(third, 16).unwrap();
    drop(
        xsk.submit_paced_tx(tx, 1 << 40, 0)
            .expect("tx ring drained"),
    );
    snap(&mut xsk, &mut snapshots);
    echo.leases.truncate(echo.leases.len() / 2); // drops the rest
    for _ in 0..6 {
        xsk.backend_mut().step();
        xsk.harvest_completions(u32::MAX);
        snap(&mut xsk, &mut snapshots);
    }
    echo.drain(&mut xsk);
    snap(&mut xsk, &mut snapshots);
    let trace = xsk
        .backend_mut()
        .take_trace()
        .into_iter()
        .map(|t| t.kind)
        .collect();
    Recorded {
        frames,
        trace,
        snapshots,
    }
}

/// Rust count-model replay against the auditor, without Lean.
#[test]
fn rust_count_model_replays_the_audited_runs() {
    let mut kinds_seen = std::collections::HashSet::new();
    for seed in 1..=6u64 {
        let run = record(0xa5a5_0000 + seed, 300);
        let mut model = XdpUmemAccounting::new(run.frames);
        let mut applied = 0;
        for (at, acc) in &run.snapshots {
            while applied < *at {
                model
                    .apply(run.trace[applied])
                    .expect("no underflow in a real run");
                kinds_seen.insert(run.trace[applied]);
                applied += 1;
            }
            assert_eq!(
                &model, acc,
                "seed {seed}: replay vs auditor after {at} transitions"
            );
        }
        let last = run.snapshots.last().unwrap().1;
        assert!(last.get(Location::Leaked) > 0, "leaks were exercised");
        assert!(matches!(
            last.check_conservation(),
            Err(ConservationError::Leaked { .. })
        ));
    }
    // Every transition the software NIC can produce occurred (the two
    // full-fill-ring fallbacks are unreachable by configuration).
    for kind in ALL_KINDS {
        let unreachable = matches!(
            kind,
            TransitionKind::RejectToFree | TransitionKind::RecycleToFree
        );
        assert_eq!(kinds_seen.contains(&kind), !unreachable, "{kind:?}");
    }
    let mut empty = XdpUmemAccounting::new(1);
    assert_eq!(
        empty.apply(TransitionKind::Harvest),
        Err(ConservationError::Underflow {
            transition: TransitionKind::Harvest
        })
    );
}

/// The same replay through the executable Lean model. Requires `VESPER_LEAN`.
#[test]
fn lean_model_matches_rust_accounting() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean executable-model differential skipped");
        return;
    };
    let mut input = String::new();
    let mut expected: Vec<Option<String>> = Vec::new();
    let mut checkpoints = 0;
    for seed in 1..=4u64 {
        let run = record(0x5eed_0000 + seed, 250);
        input.push_str(&format!("init {}\n", run.frames));
        expected.push(Some(render(&XdpUmemAccounting::new(run.frames))));
        let mut snaps = run.snapshots.iter().peekable();
        let mut model = XdpUmemAccounting::new(run.frames);
        for (i, kind) in run.trace.iter().enumerate() {
            input.push_str(kind.lean_name());
            input.push('\n');
            model.apply(*kind).unwrap();
            // Only lines at snapshot points carry the auditor's counts; the
            // rest carry the Rust count model's.
            let mut line = render(&model);
            while let Some((at, acc)) = snaps.peek() {
                if *at == i + 1 {
                    line = render(acc);
                    checkpoints += 1;
                    snaps.next();
                } else if *at <= i {
                    snaps.next();
                } else {
                    break;
                }
            }
            expected.push(Some(line));
        }
    }
    // Rejections and the composite §35.3 step.
    input.push_str("init 3\nharvest\nretire\nreplenish\nnicPullFill\nnicDeliver\n");
    expected.extend([
        Some("3 0 0 0 0 0 0 0 0 0 0".into()),
        Some("reject".into()),
        Some("reject".into()),
        Some("2 1 0 0 0 0 0 0 0 0 0".into()),
        Some("2 0 1 0 0 0 0 0 0 0 0".into()),
        Some("2 0 0 1 0 0 0 0 0 0 0".into()),
    ]);
    input.push_str("step cidMiss\nstep reply\nreplenish\nnicPullFill\nnicDeliver\nstep reply\nstep malformedDescriptor\nnotATransition\n");
    expected.extend([
        Some("2 1 0 0 0 0 0 0 0 0 0".into()),
        Some("reject".into()),
        Some("1 2 0 0 0 0 0 0 0 0 0".into()),
        Some("1 1 1 0 0 0 0 0 0 0 0".into()),
        Some("1 1 0 1 0 0 0 0 0 0 0".into()),
        Some("1 1 0 0 0 0 1 0 0 0 0".into()),
        Some("reject".into()),
        Some("unknown".into()),
    ]);

    let (header, lines) = run_lean(&lean, &input);
    assert_eq!(header, format!("{MODEL_ID} | {MODEL_CLAIM}"));
    assert_eq!(lines.len(), expected.len(), "one Lean line per input line");
    for (i, (got, want)) in lines.iter().zip(&expected).enumerate() {
        if let Some(want) = want {
            assert_eq!(got, want, "line {i}");
        }
    }
    assert!(
        checkpoints > 1000,
        "auditor checkpoints compared: {checkpoints}"
    );
    eprintln!(
        "Lean count model agreed on {} lines, {checkpoints} auditor checkpoints",
        lines.len()
    );
}
