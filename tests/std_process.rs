//! `@vesper-std/process` (self-hosting SH0 lane 3, #1125): commands with
//! arguments, environment and directory, captured output, exit statuses,
//! spawn failures and `exit`, in the JIT (optimize on and off) and the
//! evaluator, each answer checked against `std::process` and every command
//! and child handle finalized (tests/std_io_support).
mod std_io_support;

use std_io_support::{MODES, Mode, Temp, agree, check_driver, lit};

const DRIVER: &str = r#"use @vesper-std/fs::IoError;
use @vesper-std/process::{
    ExitStatus, Output, command, arg, env, env_remove, env_clear, cwd, stdio, discard, spawn,
    wait, stdout, stderr, release, output, success, exit, id, stdout_stream, capture, null
};

fn mix(h: u64, b: u64) -> u64 {
    wrapping_mul(h ^ b, 1099511628211)
}

fn hash(in s: Slice[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for i in 0..s.len() {
        let b = match s.try_get(i) {
            Option::Some(b) => b,
            Option::None => 0,
        };
        h = mix(h, widen_u64(b));
    }
    mix(h, s.len())
}

fn status_word(s: ExitStatus) -> u64 {
    let ok: u64 = if success(s) { 1 } else { 0 };
    let exited: u64 = if s.exited { 1 } else { 0 };
    mix(mix(mix(wrapping_u64(s.code), widen_u64(s.signal)), exited), ok)
}

fn code(e: IoError) -> u64 {
    wrapping_add(1000, widen_u64(e.kind))
}

// Arguments, a variable, the directory and both streams of a shell.
pub fn shell() -> u64 eff [Ffi[StdProcess]] {
    handle Fail[IoError] in {
        let mut c = command(in "/bin/sh");
        arg(inout c, in "-c");
        arg(inout c, in "printf '%s|%s|' \"$1\" \"$VESPER_STD_X\"; pwd; printf 'to stderr' >&2; exit 3");
        arg(inout c, in "sh");
        arg(inout c, in "first argument");
        env(inout c, in "VESPER_STD_X", in "x value");
        cwd(inout c, in "@DIR@");
        let out = output(c);
        mix(mix(status_word(out.status), hash(in out.stdout.as_slice())), hash(in out.stderr.as_slice()))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// An empty environment plus one variable; a removed variable.
pub fn environment() -> u64 eff [Ffi[StdProcess]] {
    handle Fail[IoError] in {
        let mut c = command(in "/bin/sh");
        arg(inout c, in "-c");
        arg(inout c, in "printf '%s%s' \"${HOME-unset}\" \"$Y\"");
        env_clear(inout c);
        env(inout c, in "Y", in "1");
        let a = output(c);
        let mut d = command(in "/bin/sh");
        arg(inout d, in "-c");
        arg(inout d, in "printf '%s' \"${VESPER_STD_KEEP-gone}\"");
        env(inout d, in "VESPER_STD_KEEP", in "kept");
        env_remove(inout d, in "VESPER_STD_KEEP");
        let b = output(d);
        mix(hash(in a.stdout.as_slice()), hash(in b.stdout.as_slice()))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// A program that does not exist: spawn reports NotFound, and `output`
// raises it.
pub fn missing() -> u64 eff [Ffi[StdProcess]] {
    let c = command(in "/nonexistent/vesper-std-no-such-program");
    let a = match spawn(c) {
        Result::Ok(ch) => {
            release(ch);
            0
        },
        Result::Err(e) => code(e),
    };
    let b = handle Fail[IoError] in {
        let d = command(in "vesper-std-no-such-program-on-path");
        let out = output(d);
        status_word(out.status)
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    mix(a, b)
}

// Spawn, wait, read the captured stream, wait again (an error), release;
// a discarded command; a child released while running is killed.
pub fn lifecycle() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "/bin/sh");
    arg(inout c, in "-c");
    arg(inout c, in "printf 'out'; exit 0");
    stdio(inout c, stdout_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let first = match wait(inout ch) {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    };
    let again = match wait(inout ch) {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    };
    let text = stdout(in ch);
    release(ch);
    let unused = command(in "/bin/sh");
    discard(unused);
    let mut s = command(in "/bin/sh");
    arg(inout s, in "-c");
    arg(inout s, in "sleep 30");
    let killed: u64 = match spawn(s) {
        Result::Ok(ch) => {
            release(ch);
            1
        },
        Result::Err(e) => code(e),
    };
    let me: u64 = if id() > 0 { 1 } else { 0 };
    mix(mix(mix(mix(first, again), hash(in text.as_slice())), killed), me)
}

pub fn quit() -> u64 eff [Ffi[StdProcess]] {
    exit(7);
    0
}
"#;

fn mix(h: u64, b: u64) -> u64 {
    (h ^ b).wrapping_mul(1099511628211)
}

fn hash(s: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in s {
        h = mix(h, u64::from(b));
    }
    mix(h, s.len() as u64)
}

fn status_word(s: std::process::ExitStatus) -> u64 {
    let code = s.code().map_or(0, |c| c as u64);
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&s).unwrap_or(0) as u64;
    #[cfg(not(unix))]
    let signal = 0;
    mix(
        mix(mix(code, signal), u64::from(s.code().is_some())),
        u64::from(s.success()),
    )
}

fn source(dir: &Temp) -> String {
    DRIVER.replace("@DIR@", &dir.lit())
}

#[cfg(unix)]
#[test]
fn commands_run_with_args_env_and_directory() {
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    let script = "printf '%s|%s|' \"$1\" \"$VESPER_STD_X\"; pwd; printf 'to stderr' >&2; exit 3";
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", script, "sh", "first argument"])
        .env("VESPER_STD_X", "x value")
        .current_dir(dir.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let want = mix(
        mix(status_word(out.status), hash(&out.stdout)),
        hash(&out.stderr),
    );
    assert_eq!(agree(&checked, "shell", &MODES), want);

    let want = mix(hash(b"unset1"), hash(b"gone"));
    assert_eq!(agree(&checked, "environment", &MODES), want);
}

#[cfg(unix)]
#[test]
fn spawn_failures_are_typed_and_children_are_released() {
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    assert_eq!(agree(&checked, "missing", &MODES), mix(1001, 1001));
    let start = std::time::Instant::now();
    let ok = mix(mix(mix(0, 0), 1), 1);
    let want = mix(mix(mix(mix(ok, 1007), hash(b"out")), 1), 1);
    assert_eq!(agree(&checked, "lifecycle", &MODES), want);
    // Three runs, each killing a 30-second sleep instead of waiting for it.
    assert!(start.elapsed().as_secs() < 25, "{:?}", start.elapsed());
}

/// Runs only as the child of `exit_ends_the_process`.
#[test]
#[ignore = "the child half of exit_ends_the_process"]
fn exit_child() {
    let Ok(mode) = std::env::var("VESPER_STD_EXIT_CHILD") else {
        return;
    };
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    let mode = match mode.as_str() {
        "jit" => Mode::Jit,
        "nojit" => Mode::JitNoOpt,
        _ => Mode::Evaluator,
    };
    std_io_support::run(&checked, "quit", mode);
    unreachable!("exit returned");
}

#[test]
fn exit_ends_the_process() {
    for mode in ["jit", "nojit", "eval"] {
        if mode != "eval" && !std_io_support::jit_host() {
            continue;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "exit_child", "--test-threads=1"])
            .env("VESPER_STD_EXIT_CHILD", mode)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(7), "{mode}");
    }
    let _ = lit;
}
