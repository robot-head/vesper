//! The Vesper CLI routing end to end (self-hosting SH5 lane 13, #1336): the
//! hybrid's driver hands every command off the compile path to
//! `@vesper/driver-commands` (packages/vesper-driver-commands), which runs
//! what is in front of the command layers (no arguments, `--help`/`-h`,
//! `--version`/`-V`, the unknown-command error and the hand-off to
//! `vesper-<tool>` programs of src/tool_commands.rs) and hands the
//! compiler's own commands back to the Rust CLI.
//!
//! The hybrid and the default compiler are staged side by side in scratch
//! layouts (tools beside them, none, the Bazel `crates/vesper-tools/bin`
//! tree) and run every argument vector under several `PATH`s, each in a
//! fresh directory. They must agree on exit status, stdout, stderr and
//! every file in the directory afterwards. A tool here is a hard link of
//! the default compiler (so it runs on every host), plus on Unix shell
//! scripts for exit codes, signals, shadows and spawn errors.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const EXE: &str = std::env::consts::EXE_SUFFIX;

/// `tool_commands::TOOLS`.
const TOOLS: &[&str] = &[
    "attest",
    "audit",
    "bench",
    "cas",
    "fuzz",
    "grammar",
    "json",
    "migrate",
    "publish",
    "replay",
    "resolve",
    "sbom",
    "semver",
    "simd",
    "top",
    "traceparent",
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{EXE}", name.display());
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

fn resolve(path: PathBuf) -> PathBuf {
    let path = if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    };
    path.canonicalize().unwrap_or(path)
}

fn hybrid() -> PathBuf {
    resolve(PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid")))
}

/// The default compiler, as in tests/driver.rs.
fn default_compiler() -> PathBuf {
    for var in ["VESPER_DEFAULT_COMPILER", "VESPER_PORT_COMPILER"] {
        if let Some(path) = std::env::var_os(var) {
            let path = resolve(PathBuf::from(path));
            assert!(path.is_file(), "{var}={} is not a file", path.display());
            return path;
        }
    }
    let hybrid = hybrid();
    let target = hybrid
        .parent()
        .and_then(Path::parent)
        .expect("target directory");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .arg("--target-dir")
        .arg(target)
        .status()
        .expect("run cargo build --bin vesper");
    assert!(status.success(), "cargo build --bin vesper failed");
    target.join("debug").join(format!("vesper{EXE}"))
}

/// A scratch directory on the target's file system where one exists, so
/// that the compilers can be hard linked rather than copied.
fn scratch(name: &str) -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TEST_TMPDIR").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `to` as `from`: a hard link, else a copy.
fn stage(from: &Path, to: &Path) {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to)
            .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), to.display()));
    }
}

#[cfg(unix)]
fn script(path: &Path, body: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The compilers and tools, staged.
struct Layout {
    name: &'static str,
    default: PathBuf,
    hybrid: PathBuf,
}

struct Stage {
    root: PathBuf,
    layouts: Vec<Layout>,
    /// `PATH` values to run under (`None` unsets it), by name.
    paths: Vec<(&'static str, Option<OsString>)>,
}

fn staged() -> Stage {
    let (default, hybrid) = (default_compiler(), hybrid());
    let root = scratch("vesper-cli-routing");
    let compilers = |dir: &Path| {
        let (d, h) = (
            dir.join(format!("vesper{EXE}")),
            dir.join(format!("vesper-hybrid{EXE}")),
        );
        stage(&default, &d);
        stage(&hybrid, &h);
        (d, h)
    };
    let tool = |dir: &Path, name: &str| stage(&default, &dir.join(format!("vesper-{name}{EXE}")));
    let mut layouts = vec![];
    // Every tool beside the compilers, as Cargo builds them, plus an
    // external one.
    let full = root.join("full");
    let (d, h) = compilers(&full);
    for t in TOOLS {
        tool(&full, t);
    }
    tool(&full, "extra");
    layouts.push(Layout {
        name: "tools beside",
        default: d,
        hybrid: h,
    });
    // No tools anywhere but PATH.
    let (d, h) = compilers(&root.join("bare"));
    layouts.push(Layout {
        name: "no tools",
        default: d,
        hybrid: h,
    });
    // Bazel's tree: `<bin>/bin/vesper` with the tools at
    // `<bin>/crates/vesper-tools/bin/`.
    let (d, h) = compilers(&root.join("bazel/bin"));
    let tools = root.join("bazel/crates/vesper-tools/bin");
    tool(&tools, "semver");
    tool(&tools, "extra");
    layouts.push(Layout {
        name: "Bazel tree",
        default: d,
        hybrid: h,
    });

    let (path1, path2) = (root.join("path1"), root.join("path2"));
    std::fs::create_dir_all(&path1).unwrap();
    std::fs::create_dir_all(&path2).unwrap();
    tool(&path2, "pathtool");
    tool(&path2, "json");
    let join = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    let mut paths = vec![
        ("unset", None),
        ("empty", Some(OsString::new())),
        ("path2", Some(join(&[&path2]))),
    ];
    #[cfg(unix)]
    {
        let shell = |dir: &Path, name: &str, body: &str, mode: u32| {
            script(&dir.join(format!("vesper-{name}")), body, mode)
        };
        let echo =
            "#!/bin/sh\nprintf 'out:%s\\n' \"$0\" \"$@\"\nprintf 'err:%s\\n' \"$#\" >&2\nexit 3\n";
        shell(&path1, "mytool", echo, 0o755);
        shell(&path1, "shadow", "#!/bin/sh\necho path1\nexit 4\n", 0o644);
        shell(&path2, "shadow", "#!/bin/sh\necho path2\nexit 5\n", 0o755);
        shell(&path1, "noexec", "#!/bin/sh\necho never\n", 0o644);
        shell(
            &path1,
            "ownerexec",
            "#!/bin/sh\necho owner\nexit 6\n",
            0o700,
        );
        shell(&path1, "sig", "#!/bin/sh\nkill -TERM $$\nsleep 5\n", 0o755);
        shell(&path1, "abort", "#!/bin/sh\nkill -KILL $$\n", 0o755);
        shell(
            &path1,
            "writes",
            "#!/bin/sh\necho made > made.txt\nexit 0\n",
            0o755,
        );
        // An executable the kernel refuses (no `#!`, not an object).
        script(&path1.join("vesper-bad"), "\u{0}\u{1}\u{2}", 0o755);
        // An executable script whose interpreter does not exist.
        shell(&path1, "nointerp", "#!/nonexistent/interpreter\n", 0o755);
        std::fs::create_dir_all(path1.join("vesper-dir")).unwrap();
        // The shadowing semver on PATH: beside wins where there is one.
        shell(
            &path1,
            "semver",
            "#!/bin/sh\necho path-semver \"$@\"\n",
            0o755,
        );
        let mut trailing = path1.clone().into_os_string();
        trailing.push("/");
        paths.push(("path1:path2", Some(join(&[&path1, &path2]))));
        paths.push(("path1/:path2", Some(join(&[Path::new(&trailing), &path2]))));
        let mut lead = OsString::from(":");
        lead.push(join(&[&path1, &path2]));
        paths.push((":path1:path2", Some(lead)));
        let mut trail = join(&[&path1]);
        trail.push("::");
        trail.push(&path2);
        paths.push(("path1::path2", Some(trail)));
        paths.push(("relative", Some(OsString::from("../path1"))));
    }
    #[cfg(windows)]
    {
        tool(&path1, "mytool");
        // Quoted entries, and a `;` inside quotes.
        let quoted = |p: &Path| {
            let mut s = OsString::from("\"");
            s.push(p);
            s.push("\"");
            s
        };
        let mut q = quoted(&path2);
        q.push(";");
        q.push(&path1);
        paths.push(("\"path2\";path1", Some(q)));
        let mut q = OsString::from(";");
        q.push(&path1);
        q.push(";;");
        q.push(quoted(&path2));
        paths.push((";path1;;\"path2\"", Some(q)));
    }
    Stage {
        root,
        layouts,
        paths,
    }
}

/// Argument vectors (after the program name).
fn vectors() -> Vec<Vec<OsString>> {
    let s = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
    let mut v: Vec<Vec<OsString>> = vec![
        // Top level.
        s(&[]),
        s(&["--help"]),
        s(&["-h"]),
        s(&["-h", "junk"]),
        s(&["--version"]),
        s(&["-V"]),
        s(&["--version", "check"]),
        s(&["-x"]),
        s(&["-"]),
        s(&["--"]),
        s(&["--bogus", "semver"]),
        s(&["--HELP"]),
        // Unknown commands: invalid names never look for a tool.
        s(&[""]),
        s(&["nope"]),
        s(&["nope", "--help"]),
        s(&["bad$name"]),
        s(&["bad name"]),
        s(&["a/b"]),
        s(&["../path1/vesper-mytool"]),
        s(&["my-tool_9"]),
        s(&["Semver"]),
        s(&["é"]),
        s(&["\u{7f}"]),
        // Compiler commands go back to the Rust CLI.
        s(&["fmt", "--help"]),
        s(&["check"]),
        s(&["lint", "--help"]),
        s(&["check", "--version"]),
        // External tools: beside, on PATH, absent.
        s(&["extra", "--version"]),
        s(&["extra", "nope"]),
        s(&["extra", "-V", "x"]),
        s(&["pathtool", "--version"]),
        s(&["pathtool", "nope", "two words", ""]),
        s(&["mytool"]),
        s(&["mytool", "a", "b c", "", "-x", "--help"]),
        s(&["shadow"]),
        s(&["noexec"]),
        s(&["ownerexec"]),
        s(&["sig"]),
        s(&["abort"]),
        s(&["writes"]),
        s(&["bad"]),
        s(&["nointerp"]),
        s(&["dir"]),
        s(&["hybrid", "--version"]),
    ];
    for t in TOOLS {
        // The tools are the default compiler, so give them a command that
        // does not name a tool again.
        v.push(s(&[t, "--version"]));
        v.push(s(&[t, "nope", "x"]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        // Not UTF-8: the command is the Rust CLI's (it panics); a tool's
        // arguments pass through.
        v.push(vec![OsString::from_vec(vec![0xff, b'x'])]);
        v.push(vec![
            OsString::from("--help"),
            OsString::from_vec(vec![0xff]),
        ]);
        v.push(vec![
            OsString::from("mytool"),
            OsString::from_vec(vec![0xc3, 0x28, b'a']),
        ]);
        v.push(vec![
            OsString::from("extra"),
            OsString::from_vec(vec![0xff]),
        ]);
    }
    v
}

/// Every file under `dir`, by relative path.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                files.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    files
}

struct Run {
    output: Output,
    files: BTreeMap<String, Vec<u8>>,
    time: Duration,
}

fn run(
    compiler: &Path,
    dir: &Path,
    args: &[OsString],
    path: Option<&OsString>,
    stats: bool,
) -> Run {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("input"), b"fn main() -> i64 { 0 }\n").unwrap();
    let mut c = Command::new(compiler);
    c.current_dir(dir).args(args).env("RUST_BACKTRACE", "0");
    match path {
        Some(p) => c.env("PATH", p),
        None => c.env_remove("PATH"),
    };
    if stats {
        c.env("VESPER_HYBRID_STATS", "1");
    } else {
        c.env_remove("VESPER_HYBRID_STATS");
    }
    let start = Instant::now();
    let output = c
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let time = start.elapsed();
    let files = snapshot(dir);
    Run {
        output,
        files,
        time,
    }
}

#[test]
fn routing_matches_the_default_compiler() {
    let stage = staged();
    let vectors = vectors();
    let mut mismatches = vec![];
    let (mut d_time, mut h_time, mut runs) = (Duration::ZERO, Duration::ZERO, 0);
    // Each layout's run directory sits beside `path1`, so the relative
    // `../path1` entry names it.
    let (d_dir, h_dir) = (
        stage.root.join("run-default"),
        stage.root.join("run-hybrid"),
    );
    for layout in &stage.layouts {
        for (path_name, path) in &stage.paths {
            for args in &vectors {
                let d = run(&layout.default, &d_dir, args, path.as_ref(), false);
                let h = run(&layout.hybrid, &h_dir, args, path.as_ref(), false);
                d_time += d.time;
                h_time += h.time;
                runs += 1;
                let mut diff = vec![];
                if d.output.status.code() != h.output.status.code() {
                    diff.push("exit status");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if d.output.status.signal() != h.output.status.signal() {
                        diff.push("signal");
                    }
                }
                if d.output.stdout != h.output.stdout {
                    diff.push("stdout");
                }
                // A Rust panic names its thread's id; the rest must agree.
                if !d.output.status.code().is_some_and(|c| c == 101)
                    && d.output.stderr != h.output.stderr
                {
                    diff.push("stderr");
                }
                if d.files != h.files {
                    diff.push("files");
                }
                if !diff.is_empty() {
                    mismatches.push(format!(
                        "{} / PATH {path_name} / {args:?}: {} (default {:?} {:?} {:?}, hybrid {:?} {:?} {:?})",
                        layout.name,
                        diff.join(", "),
                        d.output.status,
                        String::from_utf8_lossy(&d.output.stdout),
                        String::from_utf8_lossy(&d.output.stderr),
                        h.output.status,
                        String::from_utf8_lossy(&h.output.stdout),
                        String::from_utf8_lossy(&h.output.stderr),
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&stage.root);
    eprintln!(
        "routing differential: {} layouts, {} PATHs, {} argument vectors, {runs} runs; default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        stage.layouts.len(),
        stage.paths.len(),
        vectors.len(),
        d_time.as_secs_f64(),
        h_time.as_secs_f64(),
        h_time.as_secs_f64() / d_time.as_secs_f64(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper routing differs from the default CLI on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The routing really runs in Vesper (it prints through the
/// `HybridCommands` import), and the compiler's commands in the Rust CLI.
#[test]
fn routing_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-routing-stats");
    let bare = dir.join(format!("bare/vesper-hybrid{EXE}"));
    stage(&hybrid, &bare);
    let prints = |compiler: &Path, args: &[&str]| -> u64 {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let r = run(compiler, &dir.join("run"), &args, None, true);
        let stderr = String::from_utf8_lossy(&r.output.stderr);
        stderr
            .lines()
            .find_map(|l| l.strip_prefix("vesper-hybrid: the command layers printed "))
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"))
    };
    assert_eq!(prints(&bare, &[]), 1);
    assert_eq!(prints(&bare, &["--help"]), 1);
    assert_eq!(prints(&bare, &["--version"]), 1);
    assert_eq!(prints(&bare, &["-x"]), 1);
    assert_eq!(prints(&bare, &["nope"]), 1);
    assert_eq!(prints(&bare, &["semver", "--help"]), 1);
    // Handed back: the Rust CLI prints.
    assert_eq!(prints(&bare, &["fmt", "--help"]), 0);
    assert_eq!(prints(&bare, &["check", "--fuel"]), 0);
    assert_eq!(prints(&bare, &["é"]), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed write of the output is the Rust CLI's panic and exit 101, not
/// a silent success (`print!` panics when stdout is full).
#[cfg(target_os = "linux")]
#[test]
fn full_stdout_fails_as_in_rust() {
    let default = default_compiler();
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-routing-full");
    for args in [&[][..], &["--help"], &["--version"], &["-V"]] {
        let mut both = vec![];
        for compiler in [&default, &hybrid] {
            let full = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap();
            let out = Command::new(compiler)
                .current_dir(&dir)
                .env_remove("VESPER_HYBRID_STATS")
                .env("RUST_BACKTRACE", "0")
                .args(args)
                .stdout(full)
                .output()
                .unwrap();
            both.push(out);
        }
        assert_eq!(both[0].status.code(), both[1].status.code(), "{args:?}");
        // The panic line names the thread's id, so compare the message.
        for out in &both {
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("failed printing to stdout: No space left on device"),
                "{args:?}: {stderr}"
            );
        }
        assert_eq!(both[1].status.code(), Some(101), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
