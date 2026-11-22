//! The stack-bound swap (SH2 lane 10, #1182) through the CLI: the default
//! `vesper` and `vesper-hybrid` run every command that reaches
//! `stack_bound::stack_bound_report` (`--verify-stack` on Cranelift objects
//! and `vesper ebpf check`) and must agree on exit status, stdout, stderr
//! and the object bytes; the hybrid must answer them from Vesper. The root
//! package's tests/self_host_check_loops_atomics.rs compares the decoded
//! word stream with Rust on 4,000 generated graphs.
//!
//! Under the `divergent-stack-bound` feature every graph is rejected as
//! malformed, and these commands must differ instead.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

/// As in tests/differential.rs: `VESPER_DEFAULT_COMPILER`, else
/// `VESPER_PORT_COMPILER`, else the in-tree one built into this target.
fn default_compiler() -> PathBuf {
    for var in ["VESPER_DEFAULT_COMPILER", "VESPER_PORT_COMPILER"] {
        if let Some(path) = std::env::var_os(var) {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                which(&path).unwrap_or(path)
            };
            assert!(path.is_file(), "{var}={} is not a file", path.display());
            return path;
        }
    }
    let target = hybrid()
        .parent()
        .and_then(Path::parent)
        .expect("target directory")
        .to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .arg("--target-dir")
        .arg(&target)
        .status()
        .expect("run cargo build --bin vesper");
    assert!(status.success(), "cargo build --bin vesper failed");
    target
        .join("debug")
        .join(format!("vesper{}", std::env::consts::EXE_SUFFIX))
}

struct Run {
    output: Output,
    object: Option<Vec<u8>>,
}

fn run(compiler: &Path, args: &[String], out: Option<&Path>, stats: bool) -> Run {
    if let Some(out) = out {
        let _ = std::fs::remove_file(out);
    }
    let mut cmd = Command::new(compiler);
    cmd.current_dir(repository()).args(args);
    if stats {
        cmd.env("VESPER_HYBRID_STATS", "1");
    } else {
        cmd.env_remove("VESPER_HYBRID_STATS");
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let object = out.and_then(|o| std::fs::read(o).ok());
    Run { output, object }
}

/// Every command that reaches the stack-bound report, with the output file
/// it writes (if any).
fn commands(scratch: &Path) -> Vec<(Vec<String>, Option<PathBuf>)> {
    let s = |x: &str| x.to_string();
    let mut out = vec![];
    let tight = scratch.join("tight.json");
    std::fs::write(
        &tight,
        r#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"ct_main":8}}}"#,
    )
    .unwrap();
    let roomy = scratch.join("roomy.json");
    std::fs::write(
        &roomy,
        r#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"ct_main":4096}}}"#,
    )
    .unwrap();
    let fixture = "tests/fixtures/integration_b/verifiers.ves";
    let policies = [
        vec![],
        vec![s("--verify-policy"), tight.display().to_string()],
        vec![s("--verify-policy"), roomy.display().to_string()],
    ];
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for entry in ["rec_main", "ct_main"] {
            for policy in &policies {
                for opt in [false, true] {
                    let object = scratch.join(format!("{entry}-{target}.o"));
                    let mut args = vec![
                        s("emit"),
                        s(fixture),
                        s("--emit"),
                        s("object"),
                        s("-o"),
                        object.display().to_string(),
                        s("--entry"),
                        s(entry),
                        s("--target"),
                        s(target),
                        s("--verify-stack"),
                    ];
                    args.extend(policy.iter().cloned());
                    if !opt {
                        args.push(s("--no-opt"));
                    }
                    out.push((args, Some(object)));
                }
            }
        }
    }
    // `vesper ebpf check` on the mitigator: accepted, then over a budget.
    let prog = vesper_ebpf::mitigator::build(&Default::default()).unwrap();
    let mitigator = scratch.join("mitigator.o");
    std::fs::write(&mitigator, vesper_ebpf::elf::write_object(&prog).unwrap()).unwrap();
    let m = mitigator.display().to_string();
    out.push((vec![s("ebpf"), s("check"), m.clone()], None));
    out.push((
        vec![s("ebpf"), s("check"), m, s("--stack-budget"), s("8")],
        None,
    ));
    out
}

#[test]
fn stack_bound_commands_match_the_default_compiler() {
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-stack-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let default = default_compiler();
    let hybrid = hybrid();
    let mut differ = vec![];
    let commands = commands(&scratch);
    for (args, out) in &commands {
        let d = run(&default, args, out.as_deref(), false);
        let h = run(&hybrid, args, out.as_deref(), false);
        let mut what = vec![];
        if d.output.status.code() != h.output.status.code() {
            what.push("exit status");
        }
        if d.output.stdout != h.output.stdout {
            what.push("stdout");
        }
        if d.output.stderr != h.output.stderr {
            what.push("stderr");
        }
        if d.object != h.object {
            what.push("object bytes");
        }
        if !what.is_empty() {
            differ.push(format!(
                "{args:?}: {what:?}\n  default: {}{}\n  hybrid:  {}{}",
                String::from_utf8_lossy(&d.output.stdout),
                String::from_utf8_lossy(&d.output.stderr),
                String::from_utf8_lossy(&h.output.stdout),
                String::from_utf8_lossy(&h.output.stderr),
            ));
        }
    }
    // The hybrid really answered from Vesper.
    let (args, out) = &commands[0];
    let stats = run(&hybrid, args, out.as_deref(), true);
    let stderr = String::from_utf8_lossy(&stats.output.stderr);
    let calls: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: stack-bound answered "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no stack-bound count in:\n{stderr}"));
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "stack bound: {} commands, {} differ; the first answered {calls} reports from Vesper",
        commands.len(),
        differ.len()
    );
    assert!(calls > 0, "{stderr}");
    if cfg!(feature = "divergent-stack-bound") {
        assert!(
            !differ.is_empty(),
            "the divergent stack-bound module agreed with the default compiler on every command"
        );
        eprintln!("divergent build caught, first: {}", differ[0]);
    } else {
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    }
}
