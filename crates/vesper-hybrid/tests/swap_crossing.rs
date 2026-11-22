//! One shared world and word answers across the hybrid's checker swaps
//! (self-hosting SH5 lane 10, #1333): building an object sends the world
//! once to `modules/checks`, both the layout and the IR check read it, and
//! every answer equals the Rust pass's (`VESPER_HYBRID_STATS=time` runs
//! both on each call and counts the differences).
use std::path::{Path, PathBuf};
use std::process::Command;

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
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or(path)
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// (calls, calls in a scope, differing calls, shared crossings) of one
/// swap's cost line.
fn cost(stderr: &str, module: &str) -> (u64, u64, u64, u64) {
    let prefix = format!("vesper-hybrid: cost {module}: ");
    let line = stderr
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no {module} cost line in {stderr:?}"));
    let num = |after: &str| -> u64 {
        let words: Vec<&str> = line
            .split([' ', '(', ')', ','])
            .filter(|w| !w.is_empty())
            .collect();
        let i = words.iter().position(|w| *w == after).unwrap();
        words[i - 1].parse().unwrap()
    };
    (num("calls"), num("in"), num("differ"), num("shared"))
}

fn run(example: &str, shared: bool) -> String {
    let root = repository();
    let out = std::env::temp_dir().join(format!(
        "vesper-hybrid-swap-crossing-{}-{shared}.o",
        std::process::id()
    ));
    let mut cmd = Command::new(hybrid());
    cmd.current_dir(&root)
        .env("VESPER_HYBRID_STATS", "time")
        .args(["emit", "--emit", "object", "-o"])
        .arg(&out)
        .arg(example);
    if !shared {
        cmd.env("VESPER_HYBRID_SHARED", "0");
    }
    let output = cmd.output().expect("run vesper-hybrid");
    let _ = std::fs::remove_file(&out);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Examples whose world the encoder takes whole, with ADTs to lay out.
const EXAMPLES: &[&str] = &[
    "examples/counter.ves",
    "examples/transaction.ves",
    "bootstrap/corpus/ackermann.ves",
];

#[test]
fn both_checker_swaps_share_one_world() {
    for example in EXAMPLES {
        let stderr = run(example, true);
        let (calls, scoped, differ, shared) = cost(&stderr, "check-layout");
        assert!(calls > 0 && scoped > 0, "{example}: {stderr}");
        assert!(shared > 0, "{example}: no shared layout crossing: {stderr}");
        assert_eq!(differ, 0, "{example}: {stderr}");
        let (calls, _, differ, shared) = cost(&stderr, "ir-check");
        assert!(calls > 0 && shared > 0, "{example}: {stderr}");
        assert_eq!(differ, 0, "{example}: {stderr}");
        // With the shared world off each swap keeps its own input (SH4
        // lane 2), with the same answers.
        let stderr = run(example, false);
        let (_, scoped, differ, shared) = cost(&stderr, "check-layout");
        assert!(scoped > 0, "{example}: {stderr}");
        assert_eq!((shared, differ), (0, 0), "{example}: {stderr}");
        let (_, _, differ, shared) = cost(&stderr, "ir-check");
        assert_eq!((shared, differ), (0, 0), "{example}: {stderr}");
    }
}
