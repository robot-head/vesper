//! `@vesper-std/process` (self-hosting SH0 lane 3, #1125): commands with
//! arguments, environment and directory, captured output, exit statuses,
//! spawn failures and `exit`, in the JIT (optimize on and off) and the
//! evaluator, each answer checked against `std::process` and every command
//! and child handle finalized (tests/std_io_support). Deadlines
//! (`wait_timeout`), process-tree kills (`process_group`, `kill_tree`) and
//! capture limits (#1988) are checked against what `src/lsp/proof_lens.rs`
//! and `vesper proof-audit` do with `std::process` today.
mod std_io_support;

use std_io_support::{MODES, Mode, Temp, agree, check_driver, lit};

const DRIVER: &str = r#"use @vesper-std/fs::IoError;
use @vesper-std/process::{
    ExitStatus, Output, command, arg, env, env_remove, env_clear, cwd, stdio, discard, spawn,
    wait, stdout, stderr, release, output, success, exit, id, stdout_stream, capture, null,
    stderr_stream, process_group, capture_limit, wait_timeout, kill_tree, truncated, child_id
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

// A changed `PATH` is searched in the child: a relative entry from the
// child's directory, and past a candidate that cannot run (a directory).
pub fn path_search() -> u64 eff [Ffi[StdProcess]] {
    handle Fail[IoError] in {
        let mut c = command(in "vesper-std-prog");
        env(inout c, in "PATH", in "bin");
        cwd(inout c, in "@DIR@");
        let a = output(c);
        let mut d = command(in "sh");
        arg(inout d, in "-c");
        arg(inout d, in "printf ok");
        env(inout d, in "PATH", in "@DIR@/first:/bin:/usr/bin");
        let b = output(d);
        mix(hash(in a.stdout.as_slice()), hash(in b.stdout.as_slice()))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

fn timed(r: Result[Option[ExitStatus], IoError]) -> u64 {
    match r {
        Result::Ok(Option::Some(s)) => status_word(s),
        Result::Ok(Option::None) => 1,
        Result::Err(e) => code(e),
    }
}

fn killed(r: Result[ExitStatus, IoError]) -> u64 {
    match r {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    }
}

// A child past its deadline: `wait_timeout` gives up (a 0 ms check too),
// `kill_tree` kills and reaps it, and the child then counts as waited for.
pub fn overdue() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "/bin/sh");
    arg(inout c, in "-c");
    arg(inout c, in "sleep 30");
    process_group(inout c);
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let early = timed(wait_timeout(inout ch, 0));
    let late = timed(wait_timeout(inout ch, 200));
    let k = killed(kill_tree(inout ch));
    let again = timed(wait_timeout(inout ch, 10));
    let twice = killed(kill_tree(inout ch));
    let gone: u64 = if child_id(in ch) == 0 { 1 } else { 0 };
    release(ch);
    mix(mix(mix(mix(mix(early, late), k), again), twice), gone)
}

// A process group: the background grandchild (its pid appended to
// `grandchildren`, checked dead by the test) goes with the child. The
// child's stdout is readable while it runs.
pub fn tree() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "/bin/sh");
    arg(inout c, in "-c");
    arg(inout c, in "sleep 30 & echo $! >> '@DIR@/grandchildren'; echo ready; wait");
    process_group(inout c);
    stdio(inout c, stdout_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let mut ready: u64 = 0;
    let mut tries: u64 = 0;
    loop {
        if ready != 0 || tries == 400 {
            break;
        }
        tries = wrapping_add(tries, 1);
        ready = match wait_timeout(inout ch, 25) {
            Result::Ok(Option::None) => {
                let seen = stdout(in ch);
                if seen.len() > 0 { 1 } else { 0 }
            },
            Result::Ok(Option::Some(s)) => wrapping_add(2, status_word(s)),
            Result::Err(e) => code(e),
        };
    }
    let k = killed(kill_tree(inout ch));
    let text = stdout(in ch);
    release(ch);
    mix(mix(ready, k), hash(in text.as_slice()))
}

// A child that ends in time: its status and both streams, as `wait`
// gives them; waiting again is refused.
pub fn in_time() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "/bin/sh");
    arg(inout c, in "-c");
    arg(inout c, in "printf out; printf err >&2; exit 4");
    stdio(inout c, stdout_stream(), capture());
    stdio(inout c, stderr_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let s = timed(wait_timeout(inout ch, 20000));
    let again = match wait(inout ch) {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    };
    let o = stdout(in ch);
    let e = stderr(in ch);
    let cut: u64 = if truncated(in ch) { 1 } else { 0 };
    release(ch);
    mix(mix(mix(mix(s, again), hash(in o.as_slice())), hash(in e.as_slice())), cut)
}

// Repeated 0 ms checks alone see a child with captured output end, and
// collect what it wrote (more than a pipe holds).
pub fn zero_checks() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "/bin/sh");
    arg(inout c, in "-c");
    arg(inout c, in "head -c 100000 /dev/zero; exit 5");
    stdio(inout c, stdout_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let mut s: u64 = 1;
    let mut tries: u64 = 0;
    loop {
        if s != 1 || tries == 1000000 {
            break;
        }
        tries = wrapping_add(tries, 1);
        s = timed(wait_timeout(inout ch, 0));
    }
    let o = stdout(in ch);
    release(ch);
    mix(s, o.len())
}

// A capture limit keeps the first bytes of a stream far larger than a
// pipe holds, and the child still runs to its end: through
// `wait_timeout`, then through `wait`.
pub fn limited() -> u64 eff [Ffi[StdProcess]] {
    let mut total: u64 = 0;
    for round in 0..2 {
        let mut c = command(in "/bin/sh");
        arg(inout c, in "-c");
        arg(inout c, in "head -c 200000 /dev/zero; printf done >&2");
        stdio(inout c, stdout_stream(), capture());
        stdio(inout c, stderr_stream(), capture());
        capture_limit(inout c, 1000);
        process_group(inout c);
        let mut ch = match spawn(c) {
            Result::Ok(ch) => ch,
            Result::Err(e) => return code(e),
        };
        let s = if round == 0 {
            timed(wait_timeout(inout ch, 20000))
        } else {
            match wait(inout ch) {
                Result::Ok(s) => status_word(s),
                Result::Err(e) => code(e),
            }
        };
        let o = stdout(in ch);
        let e = stderr(in ch);
        let cut: u64 = if truncated(in ch) { 1 } else { 0 };
        release(ch);
        total = mix(total, mix(mix(mix(mix(s, o.len()), hash(in o.as_slice())), hash(in e.as_slice())), cut));
    }
    total
}

// The benchmark: `rounds` runs of a program with both streams captured,
// and one with an edited environment.
fn bench(rounds: u64) -> u64 eff [Ffi[StdProcess]] {
    let mut total: u64 = 0;
    for _ in 0..rounds {
        let r = handle Fail[IoError] in {
            let mut c = command(in "/bin/echo");
            arg(inout c, in "benchmark output");
            let a = output(c);
            let mut d = command(in "echo");
            arg(inout d, in "x");
            env(inout d, in "VESPER_STD_BENCH", in "1");
            let b = output(d);
            wrapping_add(a.stdout.len(), b.stdout.len())
        } with {
            return v => v,
            raise(e, _) => code(e),
        };
        total = wrapping_add(total, r);
    }
    total
}

pub fn bench_spawn() -> u64 eff [Ffi[StdProcess]] {
    bench(200)
}

pub fn bench_none() -> u64 eff [Ffi[StdProcess]] {
    bench(0)
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
fn a_changed_path_is_searched_in_the_child() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Temp::new("std-process");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(dir.path().join("first/sh")).unwrap();
    let prog = bin.join("vesper-std-prog");
    std::fs::write(&prog, "#!/bin/sh\nprintf found\n").unwrap();
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    let checked = check_driver(&source(&dir));
    let want = mix(hash(b"found"), hash(b"ok"));
    assert_eq!(agree(&checked, "path_search", &MODES), want);
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

fn exited(code: u64) -> u64 {
    mix(mix(mix(code, 0), 1), u64::from(code == 0))
}

/// Whether process `pid` has ended (gone, or a zombie nobody reaps).
#[cfg(unix)]
fn ended(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    stat.trim().is_empty() || stat.trim_start().starts_with('Z')
}

#[cfg(unix)]
#[test]
fn deadlines_kill_the_whole_tree() {
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    // Killed by SIGKILL (no exit code, signal 9), not ended by the 30 s
    // sleep. The tests run one at a time, so no time bound is checked.
    let sigkill = mix(mix(mix(0, 9), 0), 0);
    let want = mix(mix(mix(mix(mix(1, 1), sigkill), 1007), 1007), 1);
    assert_eq!(agree(&checked, "overdue", &MODES), want);
    assert_eq!(
        agree(&checked, "tree", &MODES),
        mix(mix(1, sigkill), hash(b"ready\n"))
    );
    let pids = std::fs::read_to_string(dir.path().join("grandchildren")).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert!(!pids.is_empty());
    for pid in pids {
        let mut tries = 0;
        while !ended(pid) {
            tries += 1;
            assert!(tries < 50, "grandchild {pid} outlived kill_tree");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(unix)]
#[test]
fn children_in_time_and_capture_limits() {
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    let want = mix(
        mix(mix(mix(exited(4), 1007), hash(b"out")), hash(b"err")),
        0,
    );
    assert_eq!(agree(&checked, "in_time", &MODES), want);
    assert_eq!(
        agree(&checked, "zero_checks", &MODES),
        mix(exited(5), 100000)
    );
    let round = mix(
        mix(mix(mix(exited(0), 1000), hash(&[0; 1000])), hash(b"done")),
        1,
    );
    assert_eq!(
        agree(&checked, "limited", &MODES),
        mix(mix(0, round), round)
    );
}

/// Spawn throughput, Vesper (JIT, optimized) against `std::process`, in
/// ABBA order. Run by hand:
/// `cargo test --release --test std_process -- --ignored --nocapture bench`.
#[cfg(unix)]
#[test]
#[ignore = "benchmark"]
fn bench_spawning_children() {
    let dir = Temp::new("std-process");
    let checked = check_driver(&source(&dir));
    let rust = || {
        let mut t = 0u64;
        for _ in 0..200 {
            let a = std::process::Command::new("/bin/echo")
                .arg("benchmark output")
                .output()
                .unwrap();
            let b = std::process::Command::new("echo")
                .arg("x")
                .env("VESPER_STD_BENCH", "1")
                .output()
                .unwrap();
            t += (a.stdout.len() + b.stdout.len()) as u64;
        }
        t
    };
    let symbols = std_io_support::symbols();
    let jit = |entry: &str| {
        let start = std::time::Instant::now();
        let v = std_io_support::word(&checked.jit(entry, &symbols).unwrap());
        (v, start.elapsed())
    };
    let want = rust();
    let mut vesper = vec![];
    let mut native = vec![];
    for round in 0..4 {
        let order: &[bool] = if round % 2 == 0 {
            &[true, false]
        } else {
            &[false, true]
        };
        for &v in order {
            if v {
                let (got, t) = jit("bench_spawn");
                let (_, base) = jit("bench_none");
                assert_eq!(got, want);
                vesper.push(t.saturating_sub(base));
            } else {
                let start = std::time::Instant::now();
                assert_eq!(rust(), want);
                native.push(start.elapsed());
            }
        }
    }
    vesper.sort();
    native.sort();
    eprintln!(
        "median of 400 spawns: Vesper {:?}, std::process {:?}, ratio {:.2}",
        vesper[2],
        native[2],
        vesper[2].as_secs_f64() / native[2].as_secs_f64()
    );
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

#[cfg(windows)]
#[test]
fn windows_commands_run_with_args_env_cwd_and_missing() {
    let dir = Temp::new("std-process-win");
    let src = r#"use @vesper-std/fs::IoError;
use @vesper-std/process::{
    ExitStatus, Output, command, arg, env, env_remove, env_clear, cwd, stdio, discard, spawn,
    wait, stdout, stderr, release, output, success, id, stdout_stream, capture
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

pub fn win_shell() -> u64 eff [Ffi[StdProcess]] {
    handle Fail[IoError] in {
        let mut c = command(in "cmd.exe");
        arg(inout c, in "/C");
        arg(inout c, in "echo %VESPER_STD_X% & cd & exit /b 3");
        env(inout c, in "VESPER_STD_X", in "xvalue");
        cwd(inout c, in "@DIR@");
        let out = output(c);
        mix(status_word(out.status), hash(in out.stdout.as_slice()))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn win_missing_and_lifecycle() -> u64 eff [Ffi[StdProcess]] {
    let c = command(in "C:\\nonexistent\\vesper-std-no-such-program.exe");
    let a = match spawn(c) {
        Result::Ok(ch) => {
            release(ch);
            0
        },
        Result::Err(e) => code(e),
    };
    let mut d = command(in "cmd.exe");
    arg(inout d, in "/C");
    arg(inout d, in "echo ok");
    stdio(inout d, stdout_stream(), capture());
    let mut ch = match spawn(d) {
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
    let unused = command(in "cmd.exe");
    discard(unused);
    let me: u64 = if id() > 0 { 1 } else { 0 };
    mix(mix(mix(mix(a, first), again), hash(in text.as_slice())), me)
}
"#
    .replace("@DIR@", &dir.lit());
    let checked = check_driver(&src);
    let out = std::process::Command::new("cmd.exe")
        .args(["/C", "echo %VESPER_STD_X% & cd & exit /b 3"])
        .env("VESPER_STD_X", "xvalue")
        .current_dir(dir.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let want_shell = mix(status_word(out.status), hash(&out.stdout));
    assert_eq!(agree(&checked, "win_shell", &MODES), want_shell);

    let ok = mix(mix(mix(0, 0), 1), 1);
    let want_life = mix(mix(mix(mix(1001, ok), 1007), hash(b"ok\r\n")), 1);
    assert_eq!(
        agree(&checked, "win_missing_and_lifecycle", &MODES),
        want_life
    );
}

/// `wait_timeout`, `kill_tree` (a job object) and capture limits on
/// Windows, as the Unix tests check them.
#[cfg(windows)]
#[test]
fn windows_deadlines_kill_the_whole_tree() {
    let dir = Temp::new("std-process-win");
    let src = r#"use @vesper-std/fs::IoError;
use @vesper-std/process::{
    ExitStatus, command, arg, stdio, spawn, wait, stdout, stderr, release, success,
    stdout_stream, stderr_stream, capture, process_group, capture_limit, wait_timeout,
    kill_tree, truncated, child_id
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

fn timed(r: Result[Option[ExitStatus], IoError]) -> u64 {
    match r {
        Result::Ok(Option::Some(s)) => status_word(s),
        Result::Ok(Option::None) => 1,
        Result::Err(e) => code(e),
    }
}

fn killed(r: Result[ExitStatus, IoError]) -> u64 {
    match r {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    }
}

pub fn win_overdue() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "cmd.exe");
    arg(inout c, in "/C");
    arg(inout c, in "ping -n 30 127.0.0.1 >NUL");
    process_group(inout c);
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let early = timed(wait_timeout(inout ch, 0));
    let late = timed(wait_timeout(inout ch, 300));
    let k = killed(kill_tree(inout ch));
    let again = timed(wait_timeout(inout ch, 10));
    let twice = killed(kill_tree(inout ch));
    let gone: u64 = if child_id(in ch) == 0 { 1 } else { 0 };
    release(ch);
    mix(mix(mix(mix(mix(early, late), k), again), twice), gone)
}

// The grandchild (a `ping` started by PowerShell, its pid appended to
// `grandchildren`) is in the child's job and goes with it.
// Whether PowerShell's `ready` line is in `s` (the grandchild `ping`
// writes to the same stream, maybe first).
fn has_ready(in s: Slice[u8]) -> bool {
    let word = "ready".as_bytes();
    let n = word.len();
    if s.len() < n {
        return false;
    }
    for i in 0..wrapping_add(wrapping_sub(s.len(), n), 1) {
        let mut all = true;
        for j in 0..n {
            let a = match s.try_get(wrapping_add(i, j)) {
                Option::Some(b) => b,
                Option::None => 0,
            };
            let b = match word.try_get(j) {
                Option::Some(b) => b,
                Option::None => 1,
            };
            if a != b {
                all = false;
            }
        }
        if all {
            return true;
        }
    }
    false
}

pub fn win_tree() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "powershell.exe");
    arg(inout c, in "-NoProfile");
    arg(inout c, in "-NonInteractive");
    arg(inout c, in "-Command");
    arg(inout c, in "$p = Start-Process -FilePath ping.exe -ArgumentList '-n','30','127.0.0.1' -NoNewWindow -PassThru; Add-Content -Path '@DIR@/grandchildren' -Value $p.Id; [Console]::Out.WriteLine('ready'); [Console]::Out.Flush(); Start-Sleep -Seconds 30");
    process_group(inout c);
    stdio(inout c, stdout_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let mut ready: u64 = 0;
    let mut tries: u64 = 0;
    loop {
        if ready != 0 || tries == 1200 {
            break;
        }
        tries = wrapping_add(tries, 1);
        ready = match wait_timeout(inout ch, 50) {
            Result::Ok(Option::None) => {
                let seen = stdout(in ch);
                if has_ready(in seen.as_slice()) { 1 } else { 0 }
            },
            Result::Ok(Option::Some(s)) => wrapping_add(2, status_word(s)),
            Result::Err(e) => code(e),
        };
    }
    let k = killed(kill_tree(inout ch));
    release(ch);
    mix(ready, k)
}

pub fn win_in_time() -> u64 eff [Ffi[StdProcess]] {
    let mut c = command(in "cmd.exe");
    arg(inout c, in "/C");
    arg(inout c, in "echo out& echo err 1>&2& exit /b 4");
    stdio(inout c, stdout_stream(), capture());
    stdio(inout c, stderr_stream(), capture());
    let mut ch = match spawn(c) {
        Result::Ok(ch) => ch,
        Result::Err(e) => return code(e),
    };
    let s = timed(wait_timeout(inout ch, 20000));
    let again = match wait(inout ch) {
        Result::Ok(s) => status_word(s),
        Result::Err(e) => code(e),
    };
    let o = stdout(in ch);
    let e = stderr(in ch);
    let cut: u64 = if truncated(in ch) { 1 } else { 0 };
    release(ch);
    mix(mix(mix(mix(s, again), hash(in o.as_slice())), hash(in e.as_slice())), cut)
}

pub fn win_limited() -> u64 eff [Ffi[StdProcess]] {
    let mut total: u64 = 0;
    for round in 0..2 {
        let mut c = command(in "cmd.exe");
        arg(inout c, in "/C");
        arg(inout c, in "for /L %i in (1,1,8000) do @echo 0123456789");
        stdio(inout c, stdout_stream(), capture());
        capture_limit(inout c, 1000);
        process_group(inout c);
        let mut ch = match spawn(c) {
            Result::Ok(ch) => ch,
            Result::Err(e) => return code(e),
        };
        let s = if round == 0 {
            timed(wait_timeout(inout ch, 60000))
        } else {
            match wait(inout ch) {
                Result::Ok(s) => status_word(s),
                Result::Err(e) => code(e),
            }
        };
        let o = stdout(in ch);
        let cut: u64 = if truncated(in ch) { 1 } else { 0 };
        release(ch);
        total = mix(total, mix(mix(mix(s, o.len()), hash(in o.as_slice())), cut));
    }
    total
}
"#
    .replace("@DIR@", &dir.lit());
    let checked = check_driver(&src);
    // TerminateJobObject / TerminateProcess exit code 1.
    let killed = exited(1);
    let want = mix(mix(mix(mix(mix(1, 1), killed), 1007), 1007), 1);
    assert_eq!(agree(&checked, "win_overdue", &MODES), want);
    assert_eq!(agree(&checked, "win_tree", &MODES), mix(1, killed));
    let pids = std::fs::read_to_string(dir.path().join("grandchildren")).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert!(!pids.is_empty());
    for pid in pids {
        let mut tries = 0;
        loop {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            if !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")) {
                break;
            }
            tries += 1;
            assert!(tries < 50, "grandchild {pid} outlived kill_tree");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    let out = std::process::Command::new("cmd.exe")
        .args(["/C", "echo out& echo err 1>&2& exit /b 4"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let want = mix(
        mix(
            mix(mix(exited(4), 1007), hash(&out.stdout)),
            hash(&out.stderr),
        ),
        0,
    );
    assert_eq!(agree(&checked, "win_in_time", &MODES), want);
    let out = std::process::Command::new("cmd.exe")
        .args(["/C", "for /L %i in (1,1,8000) do @echo 0123456789"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.stdout.len() > 1000);
    let round = mix(mix(mix(exited(0), 1000), hash(&out.stdout[..1000])), 1);
    assert_eq!(
        agree(&checked, "win_limited", &MODES),
        mix(mix(0, round), round)
    );
}
