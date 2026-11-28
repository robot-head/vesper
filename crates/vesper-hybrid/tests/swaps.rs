//! The swap sets of the hybrid (SH6 lane 2, #1397): `VESPER_HYBRID_SWAPS`
//! picks every swap (`all`, the default), only the swaps whose flip gate
//! passed (`gated`, or `VESPER_HYBRID_GATED=1`), or none (`none`).
//!
//! `gated` is the set the default `vesper` switches to, so this checks that
//! each swap it turns on has its gate configuration under
//! `scripts/ci/flip-gate/` and a PASS row in the plan's scoreboard, that the
//! other swaps really stay on Rust, and that a misspelt set is refused. The
//! corpus differential of the gated set is in tests/differential.rs.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    // Bazel's launcher gives a bare name found on PATH.
    if path.is_absolute() {
        return path;
    }
    let suffixed = format!("{}{}", path.display(), std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .flat_map(|dir| [dir.join(&path), dir.join(&suffixed)])
                .find(|p| p.is_file())
        })
        .unwrap_or(path)
}

/// One successful `emit --emit object` of examples/counter.ves (a failing
/// command exits before the report on Windows, #1486), with `env` set and
/// the stats report on.
fn emit(env: &[(&str, &str)]) -> Output {
    emit_with(&hybrid(), env)
}

/// `emit` with the hybrid started from `compiler`.
fn emit_with(compiler: &Path, env: &[(&str, &str)]) -> Output {
    let out = std::env::temp_dir().join(format!(
        "vesper-hybrid-swaps-{}-{}.o",
        std::process::id(),
        env.iter()
            .map(|(k, v)| format!("{k}{v}"))
            .collect::<String>()
    ));
    let mut cmd = Command::new(compiler);
    cmd.current_dir(repository())
        .env("VESPER_HYBRID_STATS", "1")
        .env_remove("VESPER_HYBRID_SWAPS")
        .env_remove("VESPER_HYBRID_GATED")
        .envs(env.iter().copied())
        .args(["emit", "--emit", "object", "-o"])
        .arg(&out)
        .arg("examples/counter.ves");
    let output = cmd.output().expect("run vesper-hybrid");
    let _ = std::fs::remove_file(&out);
    output
}

/// One `vesper-hybrid: swap <name>: <on|off>, <verdict>` line.
struct Line {
    name: String,
    on: bool,
    /// The gate configuration of a PASS row.
    config: Option<String>,
}

/// The set's name and its swap lines, from the report.
fn swaps(output: &Output) -> (String, Vec<Line>) {
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let set = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: swaps ("))
        .and_then(|l| l.split(')').next())
        .unwrap_or_else(|| panic!("no swaps report in {stderr:?}"))
        .to_string();
    let lines = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("vesper-hybrid: swap "))
        .map(|l| {
            let (name, rest) = l.split_once(": ").expect("swap line");
            let on = match rest.split(',').next() {
                Some("on") => true,
                Some("off") => false,
                other => panic!("swap {name} is {other:?}"),
            };
            let config = rest
                .split_once("flip gate PASS (scripts/ci/flip-gate/")
                .map(|(_, report)| {
                    let (file, _) = report.split_once(", ").expect("gate file and PR");
                    file.strip_suffix(".json")
                        .expect("JSON gate configuration")
                        .to_owned()
                });
            Line {
                name: name.to_string(),
                on,
                config,
            }
        })
        .collect();
    (set, lines)
}

fn count(output: &Output, prefix: &str) -> u64 {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .find_map(|l| l.strip_prefix(prefix))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no `{prefix}` report in {stderr:?}"))
}

#[test]
fn gated_turns_on_exactly_the_flip_gate_passes() {
    let root = repository();
    let plan = std::fs::read_to_string(root.join("docs/maintenance/rust-exit-plan.md"))
        .expect("read the Rust-exit plan");
    let output = emit(&[("VESPER_HYBRID_SWAPS", "gated")]);
    let (set, lines) = swaps(&output);
    assert_eq!(set, "gated");
    assert!(lines.len() > 30, "only {} swap lines", lines.len());
    let mut passed = 0;
    for line in &lines {
        assert_eq!(
            line.on,
            line.config.is_some(),
            "gated: swap {} is {} but its verdict is {}",
            line.name,
            if line.on { "on" } else { "off" },
            if line.config.is_some() {
                "PASS"
            } else {
                "not gated"
            }
        );
        let Some(config) = &line.config else {
            continue;
        };
        passed += 1;
        let path = root.join(format!("scripts/ci/flip-gate/{config}.json"));
        assert!(
            path.is_file(),
            "swap {} names a missing gate configuration {}",
            line.name,
            path.display()
        );
        let row = format!("| `{config}` (PASS");
        assert!(
            plan.lines().any(|l| l.starts_with(&row)),
            "swap {} is PASS but the scoreboard has no `{row}` row",
            line.name
        );
    }
    assert!(passed > 0, "no swap is gated on");
    // Off: the whole layout runs in Rust.
    assert_eq!(
        count(&output, "vesper-hybrid: check-layout answered "),
        0,
        "gated: the Vesper check-layout module answered calls"
    );
    // On: the parser, AST optimizer and bounds hoisting (all PASS).
    assert!(count(&output, "vesper-hybrid: parse answered ") > 0);
    assert!(count(&output, "vesper-hybrid: optimize proposed for ") > 0);
    assert!(count(&output, "vesper-hybrid: bounds-hoist hoisted ") > 0);
}

#[test]
fn gated_alias_is_the_same_set() {
    let a = swaps(&emit(&[("VESPER_HYBRID_SWAPS", "gated")]));
    let b = swaps(&emit(&[("VESPER_HYBRID_GATED", "1")]));
    assert_eq!(b.0, "gated");
    let on = |l: &[Line]| {
        l.iter()
            .filter(|l| l.on)
            .map(|l| l.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(on(&a.1), on(&b.1));
}

#[test]
fn all_is_the_default() {
    // A `gated-default` build defaults to `gated` instead (below).
    let default = if cfg!(feature = "gated-default") {
        "gated"
    } else {
        "all"
    };
    assert_eq!(swaps(&emit(&[])).0, default);
    assert_eq!(swaps(&emit(&[("VESPER_HYBRID_SWAPS", "")])).0, default);
    let output = emit(&[("VESPER_HYBRID_SWAPS", "all")]);
    let (set, lines) = swaps(&output);
    assert_eq!(set, "all");
    let off: Vec<_> = lines.iter().filter(|l| !l.on).map(|l| &l.name).collect();
    assert!(off.is_empty(), "all: off {off:?}");
    assert!(count(&output, "vesper-hybrid: parse answered ") > 0);
    assert!(count(&output, "vesper-hybrid: check-layout answered ") > 0);
}

/// The build the default `vesper` switches to runs the gated set with no
/// variable, and `all` still names every swap.
#[test]
fn gated_default_builds_run_the_gated_set() {
    if !cfg!(feature = "gated-default") {
        return;
    }
    let output = emit(&[]);
    let (set, lines) = swaps(&output);
    assert_eq!(set, "gated");
    for line in &lines {
        assert_eq!(line.on, line.config.is_some(), "swap {}", line.name);
    }
    assert!(count(&output, "vesper-hybrid: parse answered ") > 0);
    assert_eq!(count(&output, "vesper-hybrid: check-layout answered "), 0);
}

/// Started as `vesper` (how the default compiler ships it: Bazel's
/// `//:vesper_shipped`), the hybrid runs the gated set with no
/// variable; `VESPER_HYBRID_SWAPS=all` still turns every swap on.
#[test]
fn the_shipped_name_runs_the_gated_set() {
    let dir = std::env::temp_dir().join(format!("vesper-hybrid-shipped-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the shipped directory");
    let shipped = dir.join(format!("vesper{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(hybrid(), &shipped).expect("copy the hybrid as vesper");
    let output = emit_with(&shipped, &[]);
    let all = emit_with(&shipped, &[("VESPER_HYBRID_SWAPS", "all")]);
    let _ = std::fs::remove_dir_all(&dir);
    let (set, lines) = swaps(&output);
    assert_eq!(set, "gated");
    for line in &lines {
        assert_eq!(line.on, line.config.is_some(), "swap {}", line.name);
    }
    assert!(count(&output, "vesper-hybrid: parse answered ") > 0);
    assert_eq!(count(&output, "vesper-hybrid: check-layout answered "), 0);
    let (set, lines) = swaps(&all);
    assert_eq!(set, "all");
    assert!(lines.iter().all(|l| l.on), "all: a swap is off");
}

#[test]
fn none_installs_nothing() {
    let output = emit(&[("VESPER_HYBRID_SWAPS", "none")]);
    let (set, lines) = swaps(&output);
    assert_eq!(set, "none");
    let on: Vec<_> = lines.iter().filter(|l| l.on).map(|l| &l.name).collect();
    assert!(on.is_empty(), "none: on {on:?}");
    assert_eq!(count(&output, "vesper-hybrid: optimize proposed for "), 0);
    assert_eq!(count(&output, "vesper-hybrid: bounds-hoist hoisted "), 0);
}

#[test]
fn an_unknown_set_is_refused() {
    let output = emit(&[("VESPER_HYBRID_SWAPS", "gate")]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("VESPER_HYBRID_SWAPS=gate is not one of all, gated, none"),
        "{stderr:?}"
    );
}
