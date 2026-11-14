//! Lean model checks for `lean/CrucibleShrink.lean` and
//! `lean/CrucibleDpor.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI) the model
//! is compiled, which kernel-checks every proof, its axiom report is audited,
//! and the executable order `smallerB` is compared with
//! `ShrinkKey::smaller_than` on concrete tapes. The DPOR model's explored
//! final states are compared with Crucible's DPOR and exhaustive explorers
//! on random read/write programs. Agreement on inputs is a differential
//! test, not a proof that the Rust code refines the models.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use vesper_crucible::{
    ExploreConfig, Footprint, PropError, ShrinkKey, SimFiber, SimStep, SplitMix64, StepCx,
    explore_dpor, explore_exhaustive,
};

const MODEL_ID: &str = "vesper.crucible.shrink-model/1";
const MODEL_CLAIM: &str = "Lean model of tape shrinking; proofs cover the model, not Rust";
const AUDITED_THEOREMS: usize = 8;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

/// Remove `--` line comments and nested `/- -/` block comments.
fn strip_lean_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let (mut out, mut depth, mut i) = (Vec::new(), 0usize, 0);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && bytes[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(bytes[i]);
            }
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8 Lean source")
}

#[test]
fn lean_model_source_audit() {
    let source =
        std::fs::read_to_string(lean_dir().join("CrucibleShrink.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{MODEL_CLAIM}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for item in [
        "theorem smaller_wf",
        "theorem choice_tape_shrink_terminates",
        "theorem smallerB_correct",
        "theorem delete_span_smaller",
        "theorem lower_pointwise_smaller",
        "theorem set_lower_smaller",
        "def shrink",
        "termination_by",
        "theorem shrink_preserves",
        "theorem shrink_le",
    ] {
        assert!(source.contains(item), "missing {item}");
    }
    let code = strip_lean_comments(&source);
    assert!(!code.contains("partial ") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
        "decreasing_trivial",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code
            .lines()
            .any(|line| line.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_model(lean: &str, out: &Path) {
    compile(lean, out, "CrucibleShrink", AUDITED_THEOREMS);
}

fn compile(lean: &str, out: &Path, module: &str, audited_theorems: usize) {
    // Lean names the module after the path relative to its working
    // directory, so compile from `lean/`.
    let compiled = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out.join(format!("{module}.olean")))
        .arg(format!("{module}.lean"))
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with(" does not depend on any axioms") {
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
    assert_eq!(audited, audited_theorems, "axiom report lines");
}

fn run_fixture(lean: &str, out: &Path, input: &str) -> Vec<String> {
    fixture(
        lean,
        out,
        "CrucibleShrink/Fixture.lean",
        &format!("{MODEL_ID} | {MODEL_CLAIM}"),
        input,
    )
}

fn fixture(lean: &str, out: &Path, path: &str, header: &str, input: &str) -> Vec<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(lean)
        .env("LEAN_PATH", out)
        .arg("--run")
        .arg(lean_dir().join(path))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let input = input.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines().map(str::to_owned);
    assert_eq!(lines.next().expect("header"), header);
    lines.collect()
}

fn tape(rng: &mut SplitMix64) -> Vec<u64> {
    let len = rng.index(5);
    (0..len)
        .map(|_| match rng.index(4) {
            0 => 0,
            1 => u64::MAX - rng.below_inclusive(2),
            _ => rng.below_inclusive(6),
        })
        .collect()
}

fn render(values: &[u64]) -> String {
    values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn private_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let out = std::env::temp_dir().join(format!(
        "vesper-crucible-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    out
}

#[test]
fn lean_model_order_matches_rust_shrink_key() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compilation and differential skipped");
        return;
    };
    let out = private_dir("model");
    let _cleanup = Cleanup(out.clone());
    compile_model(&lean, &out);

    let mut rng = SplitMix64(0x0c4c_1b1e);
    let mut input = String::new();
    let mut expected = Vec::new();
    for _ in 0..2_000 {
        let (a, b) = (tape(&mut rng), tape(&mut rng));
        input.push_str(&format!("{};{}\n", render(&a), render(&b)));
        expected.push(
            ShrinkKey::of(&a)
                .smaller_than(ShrinkKey::of(&b))
                .to_string(),
        );
    }
    let got = run_fixture(&lean, &out, &input);
    assert_eq!(got, expected);
}

// ---------------------------------------------------------------------------
// lean/CrucibleDpor.lean: sleep-set exploration visits every trace class.

const DPOR_MODEL_ID: &str = "vesper.crucible.dpor-model/1";
const DPOR_MODEL_CLAIM: &str = "Lean model of sleep-set exploration; every trace class is visited; proofs cover the model, not Rust";
const DPOR_AUDITED_THEOREMS: usize = 7;

fn audit_source(source: &str) {
    let code = strip_lean_comments(source);
    assert!(!code.contains("partial def explore") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
        "decreasing_trivial",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code
            .lines()
            .any(|line| line.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

#[test]
fn dpor_model_source_audit() {
    let source = std::fs::read_to_string(lean_dir().join("CrucibleDpor.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{DPOR_MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{DPOR_MODEL_CLAIM}\"")));
    assert_eq!(
        source.matches("\n#print axioms ").count(),
        DPOR_AUDITED_THEOREMS
    );
    for item in [
        "inductive Equiv",
        "def Commutes",
        "def explore",
        "theorem equiv_symm",
        "theorem run_equiv",
        "theorem explore_sound",
        "theorem explore_complete",
        "theorem every_trace_class_visited",
    ] {
        assert!(source.contains(item), "missing {item}");
    }
    assert!(!strip_lean_comments(&source).contains("partial "));
    audit_source(&source);
    let fixture =
        std::fs::read_to_string(lean_dir().join("CrucibleDpor/Fixture.lean")).expect("fixture");
    audit_source(&fixture);
}

/// A read/write program fiber mirroring the Lean fixture's instance: `r c`
/// appends cell `c` to this fiber's log, `w c` writes
/// `100 * (id + 1) + k + 1` to cell `c`.
struct LogFiber {
    id: usize,
    ops: Vec<(bool, u64)>,
    pc: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogWorld {
    cells: Vec<u64>,
    logs: Vec<Vec<u64>>,
}

const LOG_BASE: u64 = 1000;

impl SimFiber<LogWorld> for LogFiber {
    fn footprint(&self) -> Footprint {
        match self.ops[self.pc] {
            (true, c) => Footprint::none().read(c).write(LOG_BASE + self.id as u64),
            (false, c) => Footprint::none().write(c),
        }
    }
    fn step(&mut self, w: &mut LogWorld, cx: &mut StepCx) -> Result<SimStep, PropError> {
        let (read, c) = self.ops[self.pc];
        if read {
            cx.read(c);
            cx.write(LOG_BASE + self.id as u64);
            w.logs[self.id].push(w.cells[c as usize]);
        } else {
            cx.write(c);
            w.cells[c as usize] = 100 * (self.id as u64 + 1) + self.pc as u64 + 1;
        }
        self.pc += 1;
        Ok(if self.pc == self.ops.len() {
            SimStep::Done
        } else {
            SimStep::Yield
        })
    }
}

fn show(w: &LogWorld) -> String {
    let nats = |xs: &[u64]| xs.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let logs: Vec<String> = w.logs.iter().map(|l| nats(l)).collect();
    format!("{}/{}", nats(&w.cells), logs.join(":"))
}

#[test]
fn lean_dpor_model_matches_rust_explorers() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean DPOR model compilation and differential skipped");
        return;
    };
    let out = private_dir("dpor-model");
    let _cleanup = Cleanup(out.clone());
    compile(&lean, &out, "CrucibleDpor", DPOR_AUDITED_THEOREMS);

    let mut rng = SplitMix64(0xd90e_5eed);
    let mut programs: Vec<Vec<Vec<(bool, u64)>>> = Vec::new();
    let mut input = String::new();
    for _ in 0..300 {
        let fibers = 2 + rng.index(2);
        let program: Vec<Vec<(bool, u64)>> = (0..fibers)
            .map(|_| {
                (0..1 + rng.index(3))
                    .map(|_| (rng.index(2) == 0, rng.below_inclusive(1)))
                    .collect()
            })
            .collect();
        let line: Vec<String> = program
            .iter()
            .map(|ops| {
                ops.iter()
                    .map(|&(r, c)| format!("{}{c}", if r { 'r' } else { 'w' }))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        input.push_str(&line.join(";"));
        input.push('\n');
        programs.push(program);
    }
    let got = fixture(
        &lean,
        &out,
        "CrucibleDpor/Fixture.lean",
        &format!("{DPOR_MODEL_ID} | {DPOR_MODEL_CLAIM}"),
        &input,
    );
    assert_eq!(got.len(), programs.len());
    let (mut lean_execs, mut dpor_execs, mut exhaustive_execs) = (0u64, 0u64, 0u64);
    for (program, line) in programs.iter().zip(&got) {
        let (count, states) = line.split_once('|').expect("count|states");
        let lean_states: BTreeSet<String> = states.split(' ').map(str::to_owned).collect();
        assert_eq!(count.parse::<usize>().unwrap(), states.split(' ').count());
        let setup = || {
            (
                LogWorld {
                    cells: vec![0; 3],
                    logs: vec![Vec::new(); program.len()],
                },
                program
                    .iter()
                    .enumerate()
                    .map(|(id, ops)| LogFiber {
                        id,
                        ops: ops.clone(),
                        pc: 0,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let c = ExploreConfig::default();
        let dp = explore_dpor(&c, setup, |w| Ok(show(w))).unwrap();
        let ex = explore_exhaustive(&c, setup, |w| Ok(show(w))).unwrap();
        let set = |e: &vesper_crucible::Exploration<String>| -> BTreeSet<String> {
            e.outcomes
                .keys()
                .map(|o| o.clone().expect("no failures"))
                .collect()
        };
        assert_eq!(lean_states, set(&dp), "{program:?}");
        assert_eq!(lean_states, set(&ex), "{program:?}");
        lean_execs += count.parse::<u64>().unwrap();
        dpor_execs += dp.executions;
        exhaustive_execs += ex.executions;
    }
    println!(
        "Lean sleep-set model: {lean_execs} executions; Rust DPOR {dpor_execs}; \
         exhaustive {exhaustive_execs}; same final states on all {} programs",
        programs.len()
    );
}
