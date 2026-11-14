//! Source Strata regions on every backend (design §15, wave eight lane 8):
//! the evaluator, the native JIT and AOT executables built with `vesper
//! build` agree on region programs with aggregate payloads, on the bytes a
//! region charges (`vesper.region.accounting/1`) and on run-time faults;
//! escape attempts are rejected with the existing diagnostics; and the Lean
//! model of the region rules (`lean/StrataRegions.lean`) checks.
//!
//! The Lean theorems are about the model of the rules, not about the Rust
//! checker or the backends; the backend agreement here is tested on these
//! programs only.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::{check, native, parser, runtime::Runtime};

/// Accepted programs and the value of `main` (below 256, so that an
/// executable's exit status carries it).
const PROGRAMS: [(&str, &str, &str); 8] = [
    (
        "aggregates",
        include_str!("fixtures/regions/aggregates.ves"),
        "39",
    ),
    (
        "accounting",
        include_str!("fixtures/regions/accounting.ves"),
        "56",
    ),
    (
        "linear_take",
        include_str!("fixtures/regions/linear_take.ves"),
        "8",
    ),
    (
        "commit_reclaim",
        include_str!("fixtures/regions/commit_reclaim.ves"),
        "17",
    ),
    (
        "recursion",
        include_str!("fixtures/regions/recursion.ves"),
        "165",
    ),
    (
        "nested_functions",
        include_str!("fixtures/regions/nested_functions.ves"),
        "32",
    ),
    (
        "scalars",
        "fn main() -> i64 {
    region outer {
        let x = outer.box(2.5);
        let flag = outer.box(true);
        let small: RegionBox[outer, u8] = outer.box(200);
        region inner(512) {
            let y = inner.box(10);
            if outer.get(flag) && outer.get(x) > 2.0 && outer.get(small) == 200 {
                inner.get(y)
            } else {
                0
            }
        }
    }
}
",
        "10",
    ),
    (
        "generic",
        "fn count[T: mod copy](x: T) -> u64 {
    region r {
        let _ = r.box(x);
        r.used()
    }
}

fn main() -> u64 {
    let a: Option[(i64, i64)] = Option::Some((1, 2));
    let b: Option[i64] = Option::None;
    wrapping_add(count(a), count(b))
}
",
        "40",
    ),
];

/// Programs the checker accepts that fault at run time, with the fault text
/// every backend reports.
const FAULTS: [(&str, &str, &str); 2] = [
    (
        "fault_bound",
        include_str!("fixtures/regions/fault_bound.ves"),
        "region bound exhausted",
    ),
    (
        "fault_stale",
        include_str!("fixtures/regions/fault_stale.ves"),
        "region handle is stale",
    ),
];

/// Escape and branding attempts and the diagnostic each one gets.
const REJECTED: [(&str, &str); 5] = [
    (
        include_str!("fixtures/regions/reject_escape_struct.ves"),
        "E0791",
    ),
    (
        include_str!("fixtures/regions/reject_escape_closure.ves"),
        "E0791",
    ),
    (
        include_str!("fixtures/regions/reject_cross_region.ves"),
        "E0792",
    ),
    (
        include_str!("fixtures/regions/reject_use_after_rewind.ves"),
        "E0796",
    ),
    (
        include_str!("fixtures/regions/reject_signature.ves"),
        "E0797",
    ),
];

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(name: &str, source: &str) -> check::World {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{name}: {:#?}", c.diagnostics);
    c.world
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-regions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether this host links executables the way `vesper build` does (a C
/// compiler driver named `cc` on a Unix host).
fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Builds `source` with the CLI and runs it: `(exit status, stderr)`.
fn executable(name: &str, source: &str) -> (Option<i32>, String) {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    std::fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    (
        run.status.code(),
        String::from_utf8_lossy(&run.stderr).into_owned(),
    )
}

#[test]
fn evaluator_jit_and_executables_agree() {
    let link = links_executables();
    if !link {
        eprintln!("no Unix C compiler driver: executables skipped");
    }
    for (name, source, expected) in PROGRAMS {
        let world = accepted(name, source);
        let mut runtime = Runtime::new(&world);
        let evaluated = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_string();
        let (opened, closed, open) = runtime.region_counts();
        assert_eq!(
            (opened, open),
            (closed, 0),
            "{name}: evaluator regions closed"
        );
        assert_eq!(evaluated, expected, "{name}: evaluator");
        let jit = native::jit(&world, "main")
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_string();
        assert_eq!(jit, expected, "{name}: JIT");
        let unoptimized = native::jit_with_options(&world, "main", false)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_string();
        assert_eq!(unoptimized, expected, "{name}: unoptimized JIT");
        if link {
            let (status, stderr) = executable(name, source);
            assert_eq!(
                status.map(|s| s.to_string()).as_deref(),
                Some(expected),
                "{name}: executable ({stderr})"
            );
        }
    }
}

#[test]
fn run_time_faults_agree() {
    let link = links_executables();
    for (name, source, fault) in FAULTS {
        let world = accepted(name, source);
        let e = Runtime::new(&world).run("main", vec![]).unwrap_err();
        assert!(e.contains(fault), "{name}: evaluator: {e}");
        let n = native::jit(&world, "main").unwrap_err();
        assert!(
            n.contains(&format!("native region fault: {fault}")),
            "{name}: JIT: {n}"
        );
        if link {
            let (status, stderr) = executable(name, source);
            assert_eq!(status, Some(70), "{name}: executable status");
            assert!(
                stderr.contains(&format!("vesper: native region fault: {fault}")),
                "{name}: executable stderr: {stderr}"
            );
        }
    }
}

#[test]
fn escape_attempts_keep_their_diagnostics() {
    for (source, code) in REJECTED {
        let c = checked(source);
        assert!(
            c.diagnostics.iter().any(|d| d.code == code),
            "expected {code} for\n{source}\ngot {:#?}",
            c.diagnostics
        );
    }
}

#[test]
fn object_output_defines_the_runtime_once_and_imports_only_libc() {
    let world = accepted("aggregates", PROGRAMS[0].1);
    let (bytes, clif) = native::object(&world, "main").unwrap();
    let text = String::from_utf8_lossy(&bytes);
    for name in [
        "vesper_region_open",
        "vesper_region_alloc",
        "vesper_region_rewind",
        "malloc",
        "memset",
        "exit",
    ] {
        assert!(text.contains(name), "object names {name}");
    }
    // The Rust JIT runtime is not part of the object.
    assert!(!text.contains("native_runtime"));
    assert!(clif.contains("call "), "lowered through runtime calls");
}

#[test]
fn freestanding_targets_reject_the_hosted_runtime() {
    let world = accepted("aggregates", PROGRAMS[0].1);
    if let Err(e) = native::object_for_target(&world, "main", Some("aarch64-unknown-none-elf")) {
        assert!(e.contains("freestanding"), "typed error: {e}");
    } else {
        panic!("a freestanding object with the hosted region runtime was produced");
    }
}

// ----- the Lean model --------------------------------------------------------

const NAMESPACE: &str = "Vesper.StrataRegions";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const KEY_THEOREMS: [&str; 9] = [
    "soundness",
    "lexical_lifetimes",
    "never_wrong",
    "escape_freedom",
    "dangling_reads_the_next_region",
    "escaping_box_rejected",
    "cross_region_rejected",
    "nested_typed",
    "nested_runs",
];

fn model() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("lean/StrataRegions.lean")
}

fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('-')) => {
                chars.next();
                depth += 1;
            }
            ('-', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('-', Some('-')) if depth == 0 => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[test]
fn lean_model_has_no_escape_hatches() {
    let source = std::fs::read_to_string(model()).expect("read lean/StrataRegions.lean");
    let code = strip_comments(&source);
    for word in [
        "sorry",
        "admit",
        "axiom",
        "native_decide",
        "implemented_by",
        "extern",
    ] {
        assert!(
            !code
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| w == word),
            "lean/StrataRegions.lean uses `{word}`"
        );
    }
    for theorem in KEY_THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "missing theorem {theorem}"
        );
    }
}

/// With `VESPER_LEAN` set (as in CI), compile the model with the pinned
/// Lean and check that every key theorem depends on the standard axioms at
/// most.
#[test]
fn lean_model_checks_with_standard_axioms_only() {
    let Some(lean) = std::env::var_os("VESPER_LEAN") else {
        eprintln!("Lean model check skipped: set VESPER_LEAN to the pinned Lean");
        return;
    };
    let temp = Temp::new();
    let mut source = std::fs::read_to_string(model()).unwrap();
    for theorem in KEY_THEOREMS {
        source.push_str(&format!("\n#print axioms {NAMESPACE}.{theorem}"));
    }
    let file = temp.0.join("StrataRegionsAudit.lean");
    std::fs::write(&file, source).unwrap();
    let out = Command::new(lean).arg(&file).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("warning"), "{stdout}");
    for theorem in KEY_THEOREMS {
        let line = stdout
            .lines()
            .find(|l| l.starts_with(&format!("'{NAMESPACE}.{theorem}'")))
            .unwrap_or_else(|| panic!("no axiom report for {theorem}:\n{stdout}"));
        if let Some((_, list)) = line.split_once("axioms: [") {
            for axiom in list.trim_end_matches(']').split(',').map(str::trim) {
                assert!(
                    STANDARD_AXIOMS.contains(&axiom),
                    "{theorem} depends on {axiom}"
                );
            }
        } else {
            assert!(line.ends_with("does not depend on any axioms"), "{line}");
        }
    }
}

#[test]
fn rewind_drops_the_evaluator_values_it_discards() {
    use std::rc::Rc;
    use vesper::regions::store::Regions;
    let payload = Rc::new(());
    let mut regions: Regions<Rc<()>> = Regions::default();
    regions.open(1, 1024).unwrap();
    let keep = regions.alloc(1, 8, Some(payload.clone())).unwrap();
    let mark = regions.checkpoint(1).unwrap();
    for _ in 0..4 {
        regions.alloc(1, 8, Some(payload.clone())).unwrap();
    }
    assert_eq!(Rc::strong_count(&payload), 6);
    regions.rewind(1, mark).unwrap();
    assert_eq!(Rc::strong_count(&payload), 2, "discarded values dropped");
    assert_eq!(regions.used(1).unwrap(), 8);
    assert!(regions.get(1, keep).is_ok());
    regions.close(1).unwrap();
    assert_eq!(Rc::strong_count(&payload), 1);
}
