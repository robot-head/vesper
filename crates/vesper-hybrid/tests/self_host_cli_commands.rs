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
    "highlight",
    "json",
    "migrate",
    "publish",
    "replay",
    "resolve",
    "sbom",
    "semver",
    "shot",
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
    // Bazel's TEST_TMPDIR can be relative, and every run changes its
    // working directory.
    let base = std::env::current_dir().unwrap().join(base);
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
    tool(&full, "refute");
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
    tool(&tools, "refute");
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
        // A bare drive (`C:`) names that drive's current directory, here the
        // run directory, where `run` stages `vesper-cwdtool`; it must not
        // become the drive's root (`C:\vesper-cwdtool.exe`). By contrast, a
        // verbatim drive (`\\?\C:`) takes a separator (`\\?\C:\vesper-cwdtool.exe`).
        let drive: String = root.to_string_lossy().chars().take(2).collect();
        assert!(drive.ends_with(':'), "{} has no drive", root.display());
        let _ = CWD_TOOL.set(default.clone());
        paths.push(("C:", Some(OsString::from(&drive))));
        let mut q = OsString::from(&drive);
        q.push(";");
        q.push(&path1);
        paths.push(("C:;path1", Some(q)));
        let verbatim_drive = format!(r"\\?\{drive}");
        paths.push((r"\\?\C:", Some(OsString::from(&verbatim_drive))));
        let mut q = OsString::from(&verbatim_drive);
        q.push(";");
        q.push(&path1);
        paths.push((r"\\?\C:;path1", Some(q)));
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
        s(&["cwdtool", "--version"]),
        s(&["verify", "--refute", "--version"]),
        s(&["verify", "--version", "--refute"]),
        s(&["verify", "--refute", "nope"]),
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

/// A tool `run` stages in each run directory (Windows: the bare-drive
/// `PATH` entry).
static CWD_TOOL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

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
                if rel.starts_with("vesper-cwdtool") {
                    continue;
                }
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
    if let Some(tool) = CWD_TOOL.get() {
        stage(tool, &dir.join(format!("vesper-cwdtool{EXE}")));
    }
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

/// The routing really runs in Vesper (the driver counts each command its
/// Vesper layer answers), and the compiler's commands in the Rust CLI.
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
            .find_map(|l| l.strip_prefix("vesper-hybrid: the command layers answered "))
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
    assert_eq!(prints(&bare, &["highlight", "--help"]), 1);
    assert_eq!(prints(&bare, &["shot", "--help"]), 1);
    // `vesper fmt` (part 2): its layer answers once, after every file.
    assert_eq!(prints(&bare, &["fmt", "--help"]), 1);
    assert_eq!(prints(&bare, &["fmt", "--bogus"]), 1);
    assert_eq!(prints(&bare, &["fmt", "input"]), 1);
    assert_eq!(prints(&bare, &["ebpf", "--help"]), 1);
    assert_eq!(prints(&bare, &["forge", "--help"]), 1);
    assert_eq!(prints(&bare, &["graph", "--help"]), 1);
    assert_eq!(prints(&bare, &["verify", "--refute"]), 1);
    assert_eq!(prints(&bare, &["lint", "--help"]), 1);
    assert_eq!(prints(&bare, &["verify", "--help"]), 1);
    // Handed back: the Rust CLI prints.
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

/// The `vesper fmt` fixture (part 2): canonical, noncanonical, damaged,
/// oversized and non-UTF-8 sources, skipped and symlinked directories, and
/// on Unix names JSON must escape and a name that is not UTF-8.
fn fmt_fixture(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let write = |rel: &str, bytes: &[u8]| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    let messy: &[u8] = b"fn main() -> i64 {   1 }\n";
    let canonical: &[u8] = b"fn main() -> i64 {\n    1\n}\n";
    write("a.ves", messy);
    write("b.ves", canonical);
    write("bad.ves", b"fn x( ");
    write("rec.ves", b"fn a() -> i64 {   1 }\nfn b( \n");
    write("big.ves", &vec![b' '; 256 * 1024 + 1]);
    write("edge.ves", &vec![b'\n'; 256 * 1024]);
    write("latin.ves", b"fn main() -> i64 { 1 } // \xe9\n");
    write("..ves", messy);
    write(".ves", messy);
    write("a.ves.bak", messy);
    write("notes.txt", messy);
    write("sub/c.ves", messy);
    write("sub/b.ves", canonical);
    write("sub/deep/d.ves", messy);
    write("sub/a/e.ves", messy);
    write("sub/x.txt", b"x\n");
    write(".hidden/h.ves", messy);
    write("target/t.ves", messy);
    write("bazel-out/z.ves", messy);
    write("targets/y.ves", messy);
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        std::os::unix::fs::symlink("a.ves", dir.join("link.ves")).unwrap();
        std::os::unix::fs::symlink("sub", dir.join("dirlink")).unwrap();
        std::os::unix::fs::symlink("missing", dir.join("dangling.ves")).unwrap();
        write("names/q\"\\\t\u{1}é.ves", messy);
        // APFS refuses names that are not UTF-8 (`EILSEQ`), so there both
        // compilers walk the tree without it.
        if let Err(e) = std::fs::write(
            dir.join("names")
                .join(std::ffi::OsStr::from_bytes(b"\xff.ves")),
            messy,
        ) {
            assert!(
                cfg!(target_vendor = "apple") && e.raw_os_error() == Some(92),
                "{e}"
            );
        }
        write("ro.ves", messy);
        std::fs::set_permissions(dir.join("ro.ves"), std::fs::Permissions::from_mode(0o444))
            .unwrap();
    }
}

/// `vesper fmt` over a tree with a directory it cannot read: the walk
/// fails with that directory's error, in Vesper as in Rust. As root the
/// directory stays readable, and both compilers simply format it.
#[cfg(unix)]
#[test]
fn fmt_unreadable_directory_matches_the_default_compiler() {
    use std::os::unix::fs::PermissionsExt;
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-fmt-unreadable");
    let mut mismatches = vec![];
    let mut readable = true;
    for args in [
        &[][..],
        &["--json"],
        &["--check"],
        &["locked"],
        &[".", "--check", "--json"],
    ] {
        let mut runs = vec![];
        for (name, compiler) in [("default", &default), ("hybrid", &hybrid)] {
            let dir = root.join(name);
            if dir.join("locked").exists() {
                let _ = std::fs::set_permissions(
                    dir.join("locked"),
                    std::fs::Permissions::from_mode(0o755),
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("locked")).unwrap();
            std::fs::write(dir.join("a.ves"), b"fn main() -> i64 {   1 }\n").unwrap();
            std::fs::write(dir.join("locked/b.ves"), b"fn main() -> i64 {   1 }\n").unwrap();
            std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000))
                .unwrap();
            readable = std::fs::read_dir(dir.join("locked")).is_ok();
            let output = Command::new(compiler)
                .current_dir(&dir)
                .arg("fmt")
                .args(args)
                .env("RUST_BACKTRACE", "0")
                .env_remove("VESPER_HYBRID_STATS")
                .output()
                .unwrap();
            std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            runs.push((output, fmt_snapshot(&dir)));
        }
        let (d, h) = (&runs[0], &runs[1]);
        if d.0.status.code() != h.0.status.code()
            || d.0.stdout != h.0.stdout
            || d.0.stderr != h.0.stderr
            || d.1 != h.1
        {
            mismatches.push(format!(
                "fmt {args:?}: default {:?} {:?} {:?}, hybrid {:?} {:?} {:?}",
                d.0.status,
                String::from_utf8_lossy(&d.0.stdout),
                String::from_utf8_lossy(&d.0.stderr),
                h.0.status,
                String::from_utf8_lossy(&h.0.stdout),
                String::from_utf8_lossy(&h.0.stderr),
            ));
        }
        if !readable {
            assert!(
                !d.0.status.success(),
                "fmt {args:?} read a locked directory"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    if readable {
        eprintln!("fmt unreadable directory: running as root, the directory stays readable");
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn fmt_vectors() -> Vec<Vec<&'static str>> {
    let mut v: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--check"],
        vec!["--json"],
        vec!["--check", "--json"],
        vec!["--recovery"],
        vec!["--recovery", "--check", "--json"],
        vec!["."],
        vec!["./"],
        vec![".", "sub"],
        vec!["sub"],
        vec!["sub/"],
        vec!["sub//", "--json"],
        vec!["sub", "a.ves", "--check"],
        vec!["sub", "--check", "--json"],
        vec!["a.ves"],
        vec!["a.ves", "--check"],
        vec!["--json", "a.ves"],
        vec!["--json", "--check", "a.ves"],
        vec!["--recovery", "--json", "a.ves"],
        vec!["a.ves", "a.ves"],
        vec!["a.ves", "b.ves", "--json"],
        vec!["b.ves"],
        vec!["b.ves", "--json"],
        vec!["bad.ves"],
        vec!["bad.ves", "--json"],
        vec!["bad.ves", "a.ves"],
        vec!["rec.ves"],
        vec!["rec.ves", "--recovery"],
        vec!["rec.ves", "--recovery", "--json"],
        vec!["rec.ves", "--recovery", "--check"],
        vec!["big.ves"],
        vec!["big.ves", "--json"],
        vec!["edge.ves", "--json"],
        vec!["latin.ves", "--json"],
        vec!["notes.txt"],
        vec!["..ves", "--json"],
        vec!["missing.ves"],
        vec!["missing.ves", "--json"],
        vec!["missing.ves", "b.ves", "--json"],
        vec!["nodir/x.ves"],
        vec!["empty"],
        vec!["empty", "--json"],
        vec!["target"],
        vec![".hidden", "--check"],
        vec!["--bogus"],
        vec!["--bogus", "--json"],
        vec!["-"],
        vec!["-", "--json"],
        vec!["--", "--check"],
        vec!["--", "-h"],
        vec!["--check", "--", "a.ves"],
        vec!["--help"],
        vec!["-h"],
        vec!["--bogus", "-h"],
        vec!["a.ves", "--help"],
        vec!["--json", "--", "--help"],
        vec!["--", "--json", "a.ves"],
        vec![""],
    ];
    if cfg!(unix) {
        v.extend([
            vec!["link.ves"],
            vec!["link.ves", "--json"],
            vec!["dirlink"],
            vec!["dirlink", "--json"],
            vec!["dangling.ves"],
            vec!["names"],
            vec!["names", "--json"],
            vec!["names", "--check", "--json"],
            vec!["ro.ves"],
            vec!["ro.ves", "--json"],
        ]);
    }
    v
}

/// Every file under `dir` with its contents and, on Unix, its mode;
/// symbolic links as their targets.
fn fmt_snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, u32)> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(dir).unwrap().display().to_string();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = u32::from(meta.permissions().readonly());
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&p).unwrap();
                files.insert(rel, (target.display().to_string().into_bytes(), 0));
            } else if meta.is_dir() {
                files.insert(format!("{rel}/"), (vec![], mode));
                stack.push(p);
            } else {
                files.insert(rel, (std::fs::read(&p).unwrap(), mode));
            }
        }
    }
    files
}

/// `vesper fmt` runs in Vesper (part 2): every option, the single-file and
/// workspace reports, the walk's order and skips, and every refusal agree
/// with the default CLI on exit status, stdout, stderr and the files left.
#[test]
fn fmt_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-fmt");
    let mut mismatches = vec![];
    let (mut d_time, mut h_time) = (Duration::ZERO, Duration::ZERO);
    let vectors = fmt_vectors();
    for args in &vectors {
        let mut runs = vec![];
        for (name, compiler) in [("default", &default), ("hybrid", &hybrid)] {
            let dir = root.join(name);
            fmt_fixture(&dir);
            let start = Instant::now();
            let output = Command::new(compiler)
                .current_dir(&dir)
                .arg("fmt")
                .args(args)
                .env("RUST_BACKTRACE", "0")
                .env_remove("VESPER_HYBRID_STATS")
                .output()
                .unwrap();
            let time = start.elapsed();
            runs.push((output, fmt_snapshot(&dir), time));
        }
        let (d, h) = (&runs[0], &runs[1]);
        d_time += d.2;
        h_time += h.2;
        let mut diff = vec![];
        if d.0.status.code() != h.0.status.code() {
            diff.push("exit status");
        }
        if d.0.stdout != h.0.stdout {
            diff.push("stdout");
        }
        if d.0.stderr != h.0.stderr {
            diff.push("stderr");
        }
        if d.1 != h.1 {
            diff.push("files");
        }
        if !diff.is_empty() {
            mismatches.push(format!(
                "fmt {args:?}: {} (default {:?} {:?} {:?}, hybrid {:?} {:?} {:?})",
                diff.join(", "),
                d.0.status,
                String::from_utf8_lossy(&d.0.stdout),
                String::from_utf8_lossy(&d.0.stderr),
                h.0.status,
                String::from_utf8_lossy(&h.0.stdout),
                String::from_utf8_lossy(&h.0.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    eprintln!(
        "fmt differential: {} argument vectors; default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        vectors.len(),
        d_time.as_secs_f64(),
        h_time.as_secs_f64(),
        h_time.as_secs_f64() / d_time.as_secs_f64(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper fmt layer differs from the default CLI on {} vectors:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The workspace walk's 200,000-entry bound holds while a directory is
/// read (Linux, where creating the entries is quick): exactly the limit
/// walks, one more is the bounded-walk error, on both compilers.
#[cfg(target_os = "linux")]
#[test]
fn fmt_walk_limit_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-fmt-limit");
    let big = root.join("big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..200_000 {
        std::fs::File::create(big.join(format!("{i:06}.txt"))).unwrap();
    }
    let run = |compiler: &Path| {
        Command::new(compiler)
            .current_dir(&root)
            .args(["fmt", "--json", "big"])
            .env_remove("VESPER_HYBRID_STATS")
            .output()
            .unwrap()
    };
    for extra in [None, Some("zz.ves")] {
        if let Some(name) = extra {
            std::fs::write(big.join(name), b"fn main() -> i64 {   1 }\n").unwrap();
        }
        let (d, h) = (run(&default), run(&hybrid));
        assert_eq!(d.status.code(), h.status.code(), "{extra:?}");
        assert_eq!(d.stdout, h.stdout, "{extra:?}");
        assert_eq!(d.stderr, h.stderr, "{extra:?}");
        let stderr = String::from_utf8_lossy(&h.stderr);
        assert_eq!(
            stderr.contains("fmt visited more than 200000 directory entries"),
            extra.is_some(),
            "{extra:?}: {stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn seed_ebpf_tree(dir: &Path) {
    let prog = vesper_ebpf::mitigator::build(&Default::default()).unwrap();
    std::fs::write(
        dir.join("mitigator.o"),
        vesper_ebpf::elf::write_object(&prog).unwrap(),
    )
    .unwrap();
    // Valid XDP program: `r0 = 2; exit` (16-hex-digit slots).
    std::fs::write(dir.join("ok.hex"), "b700000002000000\n9500000000000000\n").unwrap();
    // Same instructions in raw 8-byte little-endian binary.
    std::fs::write(
        dir.join("ok.bin"),
        [
            0xb7, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
    )
    .unwrap();
    // Invalid program: reads uninitialized r1 (`r0 = r1; exit`).
    std::fs::write(dir.join("bad.hex"), "bf01000000000000\n9500000000000000\n").unwrap();
    // Malformed hex line.
    std::fs::write(dir.join("malformed.hex"), "not-hex\n").unwrap();
}

#[test]
fn ebpf_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-ebpf");
    let cases: &[&[&str]] = &[
        &["ebpf"],
        &["ebpf", "--help"],
        &["ebpf", "-h"],
        &["ebpf", "check", "--help"],
        &["ebpf", "check", "-h"],
        &["ebpf", "verify"],
        &["ebpf", "check"],
        &["ebpf", "check", "--unknown"],
        &["ebpf", "check", "--stack-budget"],
        &["ebpf", "check", "--stack-budget", "abc", "ok.hex"],
        &["ebpf", "check", "ok.hex", "ok.bin"],
        &["ebpf", "check", "missing.hex"],
        &["ebpf", "check", "malformed.hex"],
        &["ebpf", "check", "ok.hex"],
        &["ebpf", "check", "ok.hex", "--disassemble"],
        &["ebpf", "check", "ok.bin"],
        &["ebpf", "check", "bad.hex"],
        &["ebpf", "check", "mitigator.o"],
        &["ebpf", "check", "mitigator.o", "--load"],
        &["ebpf", "check", "mitigator.o", "--stack-budget", "8"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_ebpf_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code() || d.stdout != h.stdout || d.stderr != h.stderr {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `ebpf` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn ebpf_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-ebpf-runs");
    seed_ebpf_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["ebpf", "check", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn seed_forge_tree(dir: &Path) {
    std::fs::write(
        dir.join("ok.ves"),
        "macro add1(x: Expr) -> Expr {\n    quote { $x + 1 }\n}\n\npub fn main() -> i64 {\n    add1!(41)\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("bad.ves"), "pub fn main() -> i64 {\n    1 +\n}\n").unwrap();
    std::fs::write(dir.join("bad_policy.json"), "{not-json").unwrap();
}

#[test]
fn forge_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-forge");
    let cases: &[&[&str]] = &[
        &["forge"],
        &["forge", "--help"],
        &["forge", "-h"],
        &["forge", "check", "--help"],
        &["forge", "nope"],
        &["forge", "check"],
        &["forge", "check", "--unknown"],
        &["forge", "check", "--package"],
        &["forge", "check", "--policy"],
        &["forge", "check", "ok.ves", "bad.ves"],
        &["forge", "check", "missing.ves"],
        &["forge", "check", "ok.ves", "--policy", "bad_policy.json"],
        &["forge", "check", "ok.ves"],
        &[
            "forge",
            "check",
            "ok.ves",
            "--json",
            "--package",
            "@local/test",
        ],
        &["forge", "check", "bad.ves"],
        &["forge", "expand", "ok.ves"],
        &["forge", "lean", "ok.ves"],
        &["forge", "lean", "ok.ves", "--provenance", "prov.json"],
        &["forge", "lean", "--provenance"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_forge_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        let d_prov = std::fs::read(root.join("d/prov.json")).ok();
        let h_prov = std::fs::read(root.join("h/prov.json")).ok();
        if d.status.code() != h.status.code()
            || d.stdout != h.stdout
            || d.stderr != h.stderr
            || d_prov != h_prov
        {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}, prov {}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
                d_prov == h_prov,
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `forge` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn forge_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-forge-runs");
    seed_forge_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["forge", "check", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn seed_graph_tree(dir: &Path) {
    let lock = serde_json::json!({
        "schema_version": 1,
        "roots": ["@acme/app"],
        "packages": [
            {
                "name": "@acme/app",
                "version": "1.0.0",
                "source": { "kind": "path", "path": "." },
                "digest": "0".repeat(64),
                "features": [],
                "capability_ceiling": ["Fail"],
                "required_capabilities": ["Fail"],
                "forbid_unsafe_ffi": true,
                "dependencies": [
                    {
                        "package": "@acme/lib",
                        "requirement": "^1.0",
                        "allowed_capabilities": ["Fail"],
                        "requested_features": ["std"]
                    }
                ],
                "system_dependencies": []
            },
            {
                "name": "@acme/lib",
                "version": "1.2.0",
                "source": { "kind": "path", "path": "lib" },
                "digest": "1".repeat(64),
                "features": ["std"],
                "capability_ceiling": ["Fail"],
                "required_capabilities": [],
                "forbid_unsafe_ffi": true,
                "dependencies": [],
                "system_dependencies": []
            }
        ],
        "system_packages": []
    });
    std::fs::write(
        dir.join("Loom.lock"),
        serde_json::to_vec_pretty(&lock).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("bad.lock"), "{not-json").unwrap();
    // A snapshot in the `Solution` schema that validates, with a native
    // dependency, an unused native selection and an unreachable package,
    // and one that does not (a root the selections do not meet).
    let valid = serde_json::json!({
        "schema_version": 1,
        "roots": [{ "name": "@acme/app", "version": "^1" }],
        "packages": [
            {
                "manifest": {
                    "package": {
                        "name": "@acme/app",
                        "version": "1.0.0",
                        "edition": "2026",
                        "capability_ceiling": ["Fail", "Net"]
                    },
                    "dependencies": {
                        "@acme/lib": { "version": "^1.2", "effects": ["Fail", "Net"], "features": ["std"] }
                    },
                    "features": {
                        "default": { "enables_effects": ["Fail"], "sys_deps": ["zlib ^1.2"] }
                    }
                },
                "features": ["default"]
            },
            {
                "manifest": {
                    "package": {
                        "name": "@acme/lib",
                        "version": "1.2.0",
                        "edition": "2026",
                        "capability_ceiling": ["Fail", "Net"]
                    },
                    "features": { "std": { "enables_effects": ["Net"] } }
                }
            },
            {
                "manifest": {
                    "package": { "name": "@acme/unused", "version": "0.1.0", "edition": "2026" }
                }
            }
        ],
        "system_packages": [
            { "name": "zlib", "version": "1.2.13" },
            { "name": "libc", "version": "0.2.0" }
        ]
    });
    std::fs::write(
        dir.join("valid.lock"),
        serde_json::to_vec_pretty(&valid).unwrap(),
    )
    .unwrap();
    let mut invalid = valid;
    invalid["roots"][0]["version"] = serde_json::json!("^9");
    std::fs::write(
        dir.join("invalid.lock"),
        serde_json::to_vec(&invalid).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("empty.lock"),
        r#"{"schema_version":1,"roots":[],"packages":[],"system_packages":[]}"#,
    )
    .unwrap();
}

#[test]
fn graph_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-graph");
    let cases: &[&[&str]] = &[
        &["graph", "--help"],
        &["graph", "-h"],
        &["graph", "--unknown"],
        &["graph", "--package"],
        &["graph", "--package", "@acme/app", "--package", "@acme/lib"],
        &["graph", "--package", "bad", "--json", "--dot"],
        &["graph", "--json", "--dot"],
        &["graph", "Loom.lock", "bad.lock"],
        &["graph", "missing.lock"],
        &["graph", "bad.lock"],
        &["graph", "empty.lock"],
        &["graph", "empty.lock", "--json"],
        &["graph"],
        &["graph", "Loom.lock", "--json"],
        &["graph", "Loom.lock", "--dot"],
        &["graph", "--package", "@acme/lib", "--invert"],
        &["graph", "--package", "@acme/missing", "--json"],
        &["graph", "valid.lock"],
        &["graph", "valid.lock", "--json"],
        &["graph", "valid.lock", "--dot"],
        &[
            "graph",
            "valid.lock",
            "--package",
            "@acme/lib",
            "--invert",
            "--dot",
        ],
        &["graph", "valid.lock", "--package", "@acme/app", "--json"],
        &["graph", "valid.lock", "--package", "@acme/lib"],
        &["graph", "valid.lock", "--package", "@acme/unused", "--json"],
        &["graph", "invalid.lock"],
        &["graph", "invalid.lock", "--json"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_graph_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code() || d.stdout != h.stdout || d.stderr != h.stderr {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `graph` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn graph_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-graph-runs");
    seed_graph_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["graph", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn seed_lint_tree(dir: &Path) {
    std::fs::write(
        dir.join("Loom.toml"),
        "[package]\nname = \"@acme/lint_test\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/clean.ves"),
        "pub fn add(a: i64, b: i64) -> i64 {\n    wrapping_add(a, b)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/unused_eff.ves"),
        "pub fn pure_but_eff() -> i64 eff [Fail[i64]] {\n    42\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("deny.toml"),
        "[package]\nname = \"@acme/lint_test\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[lints]\nunused_effect = \"deny\"\n",
    )
    .unwrap();
}

#[test]
fn lint_matches_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let root = scratch("vesper-cli-lint");
    let cases: &[&[&str]] = &[
        &["lint", "--help"],
        &["lint", "-h"],
        &["lint", "--unknown"],
        &["lint", "--unknown", "--json"],
        &["lint", "--config"],
        &["lint", "--config", "--json"],
        &["lint", "--config", "a", "--config", "b"],
        &["lint", "src/clean.ves", "src/unused_eff.ves"],
        &["lint", "missing.ves"],
        &["lint", "missing.ves", "--json"],
        &["lint", "src/clean.ves"],
        &["lint", "src/clean.ves", "--json"],
        &["lint", "src/unused_eff.ves"],
        &["lint", "src/unused_eff.ves", "--config", "deny.toml"],
        &[
            "lint",
            "src/unused_eff.ves",
            "--config",
            "deny.toml",
            "--json",
        ],
        &["lint", "src/unused_eff.ves", "--fix"],
        &["lint"],
        &["lint", "--json"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        let work = root.join("work");
        for compiler in [&default, &hybrid] {
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_lint_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push((out, std::fs::read(work.join("src/unused_eff.ves")).unwrap()));
        }
        let (d, d_file) = &results[0];
        let (h, h_file) = &results[1];
        if d.status.code() != h.status.code()
            || d.stdout != h.stdout
            || d.stderr != h.stderr
            || d_file != h_file
        {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}, file {}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
                d_file == h_file,
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `lint` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn lint_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-lint-runs");
    seed_lint_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["lint", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn seed_doc_tree(dir: &Path) {
    std::fs::write(
        dir.join("mod.ves"),
        "/// Adds two unsigned integers.\npub fn add(a: u64, b: u64) -> u64 {\n    a + b\n}\n",
    )
    .unwrap();
}

#[test]
fn doc_matches_the_default_compiler() {
    let hybrid = hybrid();
    let default = default_compiler();
    let root = scratch("vesper-cli-doc-diff");
    let cases: &[&[&str]] = &[
        &["doc", "--help"],
        &["doc", "-h"],
        &["doc", "store-olean", "--help"],
        &["doc"],
        &["doc", "--json"],
        &["doc", "--unknown"],
        &["doc", "--unknown", "--json"],
        &["doc", "mod.ves", "--target", "x86_64-unknown-linux-gnu"],
        &["doc", "mod.ves", "-o", "a.html", "--bundle", "b"],
        &["doc", "mod.ves", "--bundle", "b", "--json"],
        &["doc", "mod.ves", "--search", "add"],
        &["doc", "mod.ves", "--search", "-> u64", "--json"],
        &["doc", "mod.ves", "--olean-store", "store"],
        &["doc", "mod.ves", "--olean-root", "lean-build"],
        &["doc", "mod.ves", "--target", "a", "--target", "b"],
        &["doc", "mod.ves", "other.ves"],
        &["doc", "store-olean"],
        &[
            "doc",
            "store-olean",
            "--proof-audit",
            "a.json",
            "--proof-audit",
            "b.json",
        ],
        &["doc", "mod.ves"],
        &["doc", "mod.ves", "--json"],
        &["doc", "mod.ves", "-o", "out.html"],
        &["doc", "mod.ves", "--bundle", "bundle-out"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        let mut trees = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_doc_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
            trees.push(snapshot(&work));
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code()
            || d.stdout != h.stdout
            || d.stderr != h.stderr
            || trees[0] != trees[1]
        {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}, tree_eq {}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
                trees[0] == trees[1],
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `doc` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn doc_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-doc-runs");
    seed_doc_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["doc", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn seed_test_tree(dir: &Path) {
    std::fs::write(
        dir.join("unit.ves"),
        "/// ```vesper\n/// assert_eq(add(2, 3), 5);\n/// ```\npub fn add(a: u64, b: u64) -> u64 {\n    a + b\n}\n\nfn test_add() -> bool {\n    add(2, 3) == 5\n}\n",
    )
    .unwrap();
}

#[test]
fn test_matches_the_default_compiler() {
    let hybrid = hybrid();
    let default = default_compiler();
    let root = scratch("vesper-cli-test-diff");
    let cases: &[&[&str]] = &[
        &["test", "--help"],
        &["test", "-h"],
        &["test"],
        &["test", "--json"],
        &["test", "--unknown"],
        &["test", "--unknown", "--json"],
        &["test", "--doc", "--unknown"],
        &["test", "--doc", "--unknown", "--json"],
        &["test", "unit.ves", "--max-mutants", "5"],
        &["test", "unit.ves", "--mutant-offset", "1"],
        &["test", "unit.ves", "--mutate-dependency", "dep.ves"],
        &["test", "unit.ves", "--no-opt"],
        &["test", "unit.ves", "--engines", "all", "--mutate"],
        &["test", "unit.ves", "--engines", "bogus"],
        &["test", "unit.ves", "--filter", "a", "--filter", "b"],
        &["test", "unit.ves", "--mutate", "--max-mutants", "0"],
        &["test", "unit.ves", "--mutate", "--max-mutants", "2049"],
        &["test", "unit.ves", "--fuel", "0"],
        &["test", "unit.ves", "--fuel", "0x1_000"],
        &["test", "unit.ves", "--json"],
        &["test", "--doc", "unit.ves", "--json"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            seed_test_tree(&work);
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code() || d.stdout != h.stdout || d.stderr != h.stderr {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `test` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn test_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-test-runs");
    seed_test_tree(&dir);
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["test", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn proof_audit_matches_the_default_compiler() {
    let hybrid = hybrid();
    let default = default_compiler();
    let root = scratch("vesper-cli-proof-audit-diff");
    let cases: &[&[&str]] = &[
        &["proof-audit", "--help"],
        &["proof-audit", "-h"],
        &["proof-audit"],
        &["proof-audit", "--unknown"],
        &["proof-audit", "--root"],
        &["proof-audit", "--root", "a", "--root", "b"],
        &["proof-audit", "--theorem"],
        &["proof-audit", "--theorem", "bad..name"],
        &["proof-audit", "--allow-axiom", "sorryAx"],
        &["proof-audit", "--allow-axiom", "Lean.ofReduceBool"],
        &["proof-audit", "--allow-axiom", "Foo._native.bar"],
        &["proof-audit", "--lean", "a", "--lean", "b"],
        &["proof-audit", "--timeout", "0"],
        &["proof-audit", "--timeout", "301"],
        &["proof-audit", "--timeout", "abc"],
        &[
            "proof-audit",
            "--macro-provenance",
            "a",
            "--macro-provenance",
            "b",
        ],
        &["proof-audit", "ModA", "ModB"],
        &["proof-audit", "Lean.Init", "--root", "r", "--theorem", "t"],
        &["proof-audit", "ModA", "--root", "r"],
        &["proof-audit", "ModA", "--theorem", "t"],
        // Past the command layer, in the driver module (#1341): a missing
        // root, in text and JSON, and a Lean that is not there.
        &["proof-audit", "ModA", "--root", "missing", "--theorem", "t"],
        &[
            "proof-audit",
            "ModA",
            "--root",
            "missing",
            "--theorem",
            "t",
            "--json",
        ],
        &[
            "proof-audit",
            "ModA",
            "--root",
            ".",
            "--theorem",
            "t",
            "--lean",
            "no-such-lean",
        ],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code() || d.stdout != h.stdout || d.stderr != h.stderr {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `proof-audit` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn proof_audit_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-proof-audit-runs");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["proof-audit", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An audit past the command layer runs in the driver module (#1341).
#[test]
fn proof_audit_audits_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-proof-audit-audits");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["proof-audit", "ModA", "--root", "missing", "--theorem", "t"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vesper-hybrid: proof_audit answered 1 calls from Vesper"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `vesper verify SOURCE --format` past the command layer runs in
/// the driver module's `verify_run` (#1341).
#[test]
fn verify_format_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-verify-format");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["verify", "missing.ves", "--format", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vesper-hybrid: verify answered 1 calls from Vesper"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_matches_the_default_compiler() {
    let hybrid = hybrid();
    let default = default_compiler();
    let root = scratch("vesper-cli-verify-diff");
    let cases: &[&[&str]] = &[
        &["verify", "--help"],
        &["verify", "-h"],
        &["verify"],
        &["verify", "--json"],
        &["verify", "--extract", "--json"],
        &["verify", "--unknown"],
        &["verify", "--format", "--extract"],
        &["verify", "--format", "--proofs", "p.lean"],
        &["verify", "--format", "--theorem", "T"],
        &["verify", "--extract", "--theorem", "bad..name"],
        &["verify", "--extract", "--contract-proof", "no_eq"],
        &["verify", "--extract", "--contract-proof", "A.B=T"],
        &["verify", "--extract", "--allow-axiom", "sorryAx"],
        &[
            "verify",
            "--extract",
            "--contract-proof",
            "f=T",
            "--allow-axiom",
            "MyAxiom",
        ],
        &["verify", "--extract", "--json", "--emit-lean"],
        &["verify", "--format", "--timeout", "0"],
        &["verify", "--format", "--timeout", "301"],
        &["verify", "--format", "--lean", ""],
        &["verify", "--format", "a.ves", "b.ves"],
        &["verify", "--format", "--", "-dash.ves"],
        // Past the command layer, in the driver module (#1341): a missing
        // source and a directory, in text and JSON, and a Lean that is not
        // there.
        &["verify", "missing.ves", "--format"],
        &["verify", "missing.ves", "--format", "--json"],
        &["verify", ".", "--format", "--json"],
        &["verify", "--format", "--", "."],
        &["verify", ".", "--format", "--lean", "no-such-lean"],
        // `--format` as `--proofs`'s value is an `--extract` run.
        &["verify", "--extract", "--proofs", "--format", "missing.ves"],
    ];
    let mut diffs = vec![];
    for argv in cases {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let work = root.join(if compiler == &default { "d" } else { "h" });
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work).unwrap();
            let out = Command::new(compiler)
                .current_dir(&work)
                .env_remove("VESPER_HYBRID_STATS")
                .args(*argv)
                .output()
                .unwrap();
            results.push(out);
        }
        let d = &results[0];
        let h = &results[1];
        if d.status.code() != h.status.code() || d.stdout != h.stdout || d.stderr != h.stderr {
            diffs.push(format!(
                "{argv:?}: status {:?} vs {:?}, stdout {:?} vs {:?}, stderr {:?} vs {:?}",
                d.status.code(),
                h.status.code(),
                String::from_utf8_lossy(&d.stdout),
                String::from_utf8_lossy(&h.stdout),
                String::from_utf8_lossy(&d.stderr),
                String::from_utf8_lossy(&h.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        diffs.is_empty(),
        "Vesper `verify` differs from default CLI on {} cases:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn verify_runs_in_vesper() {
    let hybrid = hybrid();
    let dir = scratch("vesper-cli-verify-runs");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(&hybrid)
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["verify", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let prints: u64 = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("vesper-hybrid: the command layers answered ")
                .and_then(|s| s.strip_suffix(" commands from Vesper"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"));
    assert_eq!(prints, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn lsp_frame(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

#[test]
fn lsp_matches_the_default_compiler() {
    use std::io::Write;
    use std::process::Stdio;
    let hybrid = hybrid();
    let default = default_compiler();
    let mut session = Vec::new();
    session.extend_from_slice(&lsp_frame(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    ));
    session.extend_from_slice(&lsp_frame(
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
    ));
    session.extend_from_slice(&lsp_frame(r#"{"jsonrpc":"2.0","method":"exit"}"#));

    let inputs: &[(&str, &[u8])] = &[("eof", b""), ("initialize-shutdown-exit", &session)];
    for (name, input) in inputs {
        let mut results = vec![];
        for compiler in [&default, &hybrid] {
            let mut child = Command::new(compiler)
                .arg("lsp")
                .env_remove("VESPER_HYBRID_STATS")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            results.push(child.wait_with_output().unwrap());
        }
        let d = &results[0];
        let h = &results[1];
        assert_eq!(d.status.code(), h.status.code(), "{name}: status");
        assert_eq!(d.stdout, h.stdout, "{name}: stdout");
        assert_eq!(d.stderr, h.stderr, "{name}: stderr");
    }
}
