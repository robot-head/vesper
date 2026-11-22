//! `vesper <tool>` dispatch to `vesper-<tool>` binaries (self-hosting SH0
//! lane 2, #1124): same arguments, streams and exit status; a clear error when
//! the binary is missing; unknown commands keep the compiler's error.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// An executable that Cargo passes as a path and Bazel as a bare name on PATH.
fn executable(name: &str) -> PathBuf {
    let given = PathBuf::from(name);
    if given.components().count() > 1 || given.is_file() {
        return fs::canonicalize(&given).unwrap_or(given);
    }
    let file = format!("{name}{}", env::consts::EXE_SUFFIX);
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
        // Bazel's PATH entries are runfiles symlinks; link the real file.
        .map(|p| fs::canonicalize(&p).unwrap_or(p))
        .unwrap_or_else(|| panic!("{name} not found on PATH"))
}

fn vesper() -> PathBuf {
    executable(env!("CARGO_BIN_EXE_vesper"))
}

/// The tool binary `vesper` would run: beside it, else on PATH.
fn tool(name: &str) -> PathBuf {
    let file = format!("vesper-{name}{}", env::consts::EXE_SUFFIX);
    let beside = vesper().parent().unwrap().join(&file);
    if beside.is_file() {
        return beside;
    }
    executable(&format!("vesper-{name}"))
}

fn run(program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `command`'s output, retried while a script a test just wrote is busy
/// (`ETXTBSY`): another test thread's fork can still hold the script's write
/// descriptor until that child execs, and the kernel refuses to exec a file
/// open for writing. The dispatcher reports that as a spawn failure (exit 1,
/// "Text file busy"), and spawning `vesper` itself fails with the raw error.
#[cfg(unix)]
fn output_once_not_busy(command: &mut Command) -> Output {
    retry_busy(|| command.output())
}

/// `run`'s output, retried as [`output_once_not_busy`] does.
#[cfg(unix)]
fn retry_busy(mut run: impl FnMut() -> std::io::Result<Output>) -> Output {
    const BUSY: i32 = 26; // ETXTBSY on Linux and macOS
    for attempt in 1.. {
        let busy = match run() {
            Ok(out) => {
                let busy = out.status.code() == Some(1)
                    && String::from_utf8_lossy(&out.stderr).contains("os error 26");
                if !busy || attempt == 50 {
                    return out;
                }
                busy
            }
            Err(e) if e.raw_os_error() == Some(BUSY) && attempt < 50 => true,
            Err(e) => panic!("spawn: {e}"),
        };
        assert!(busy);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    unreachable!()
}

/// A private directory holding only a link to (or copy of) the compiler, so
/// no tool is beside it.
fn lone_vesper(tag: &str) -> (PathBuf, PathBuf) {
    let dir = env::temp_dir().join(format!("vesper-tool-dispatch-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(format!("vesper{}", env::consts::EXE_SUFFIX));
    if fs::hard_link(vesper(), &exe).is_err() {
        fs::copy(vesper(), &exe).unwrap();
    }
    (dir, exe)
}

#[test]
fn tool_output_and_status_match_the_tool_binary() {
    for args in [
        &["json", "--help"][..],
        &["grammar", "--help"][..],
        &["audit", "--help"][..],
        &["resolve", "--help"][..],
        &["sbom", "--help"][..],
        &["publish", "--help"][..],
        &["bench", "--help"][..],
    ] {
        let through = run(&vesper(), args);
        let direct = run(&tool(args[0]), &args[1..]);
        assert_eq!(through.status.code(), direct.status.code(), "{args:?}");
        assert_eq!(through.stdout, direct.stdout, "{args:?}");
        assert_eq!(through.stderr, direct.stderr, "{args:?}");
        assert!(through.status.success(), "{args:?}: {through:?}");
        assert!(!through.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn tool_failures_keep_the_compiler_error_format_and_exit_code() {
    let missing = env::temp_dir().join("vesper-tool-dispatch-no-such-file.json");
    let missing = missing.to_str().unwrap();
    let through = run(&vesper(), &["json", "check", missing]);
    let direct = run(&tool("json"), &["check", missing]);
    assert_eq!(through.status.code(), Some(1));
    assert_eq!(direct.status.code(), Some(1));
    assert_eq!(through.stderr, direct.stderr);
    let stderr = String::from_utf8_lossy(&through.stderr);
    assert!(stderr.starts_with("vesper: "), "{stderr}");
}

#[test]
fn help_still_lists_every_tool() {
    let out = run(&vesper(), &["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();
    for name in [
        "bench", "grammar", "json", "audit", "publish", "resolve", "sbom", "--tee",
    ] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(name)),
            "{name} missing from --help"
        );
    }
}

#[test]
fn unknown_commands_keep_the_compiler_error() {
    let out = run(&vesper(), &["no-such-tool", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "vesper: unknown command 'no-such-tool'; see --help\n"
    );
    let out = run(&vesper(), &["../bench"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown command '../bench'"));
}

#[test]
fn a_missing_tool_binary_is_named() {
    let (dir, exe) = lone_vesper("missing");
    let out = Command::new(&exe)
        .args(["bench", "--help"])
        .env("PATH", &dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("vesper: `vesper bench` is provided by the `vesper-bench` tool"),
        "{stderr}"
    );
    assert!(stderr.contains("cargo build -p vesper-tools"), "{stderr}");
    assert!(out.stdout.is_empty());
}

#[test]
fn build_tee_names_the_missing_measurement_tool() {
    let (dir, exe) = lone_vesper("tee");
    let source = dir.join("main.ves");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fibonacci.ves"),
        &source,
    )
    .unwrap();
    // Keep the linker on PATH, but no directory that holds vesper-tee.
    let tee = format!("vesper-tee{}", env::consts::EXE_SUFFIX);
    let path = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|d| !d.join(&tee).is_file())
        .collect::<Vec<_>>();
    let out = Command::new(&exe)
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(dir.join(format!("main{}", env::consts::EXE_SUFFIX)))
        .arg("--tee")
        .env("PATH", env::join_paths(path).unwrap())
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let linked = dir
        .join(format!("main{}", env::consts::EXE_SUFFIX))
        .is_file();
    let _ = fs::remove_dir_all(&dir);
    if !linked {
        eprintln!("skipped: no native linker here ({stderr})");
        return;
    }
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("`vesper tee` is provided by the `vesper-tee` tool"),
        "{stderr}"
    );
}

/// Any `vesper-<name>` executable is an external subcommand, as in Cargo:
/// arguments, stdin, stdout, stderr and the exit status pass through.
#[cfg(unix)]
#[test]
fn external_subcommands_pass_streams_and_status_through() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let (dir, exe) = lone_vesper("external");
    let script = dir.join("vesper-echo-dispatch");
    fs::write(
        &script,
        "#!/bin/sh\nprintf 'args:%s|' \"$@\"\necho\ncat\necho err >&2\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    // A non-executable file of the same name earlier on PATH is skipped.
    let shadow = dir.join("shadow");
    fs::create_dir_all(&shadow).unwrap();
    fs::write(shadow.join("vesper-echo-dispatch"), "not a program\n").unwrap();
    fs::rename(&script, dir.join("later-echo")).unwrap();
    let later = dir.join("later");
    fs::create_dir_all(&later).unwrap();
    fs::rename(dir.join("later-echo"), later.join("vesper-echo-dispatch")).unwrap();
    let path = env::join_paths([shadow, later, "/usr/bin".into(), "/bin".into()]).unwrap();
    let out = retry_busy(|| {
        let mut child = Command::new(&exe)
            .args(["echo-dispatch", "a b", "--json", ""])
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // A busy script never reads stdin; the pipe may already be closed.
        let _ = child.stdin.take().unwrap().write_all(b"from stdin\n");
        child.wait_with_output()
    });
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "args:a b|args:--json|args:|\nfrom stdin\n"
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

#[cfg(unix)]
#[test]
fn a_tool_killed_by_a_signal_exits_with_128_plus_the_signal() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, exe) = lone_vesper("signal");
    let script = dir.join("vesper-signal-dispatch");
    fs::write(&script, "#!/bin/sh\nkill -TERM $$\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let out = output_once_not_busy(
        Command::new(&exe)
            .arg("signal-dispatch")
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null()),
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(128 + 15));
}
