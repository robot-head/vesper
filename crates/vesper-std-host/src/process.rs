//! `sys library StdProcess` (packages/vesper-std/process/process.vsys): a
//! command builder and a child process, both linear handles.
use crate::*;
use std::process::{Command, Stdio};

/// A command being built (`linear extern type ProcCommand`).
pub struct ProcCommand {
    program: Result<std::ffi::OsString, std::io::Error>,
    args: Vec<Result<std::ffi::OsString, std::io::Error>>,
    env_clear: bool,
    env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    cwd: Option<Vec<u8>>,
    /// stdin, stdout, stderr: one of the `STDIO_*` modes.
    stdio: [u32; 3],
}

/// A spawned child (`linear extern type ProcChild`): running until waited,
/// then its status and captured output.
pub struct ProcChild {
    child: Option<std::process::Child>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// The `.vsys` wire struct `ProcStatus`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcStatus {
    /// The exit code when `exited` is 1.
    pub code: i64,
    /// The terminating signal (Unix) when `exited` is 0, else 0.
    pub signal: u32,
    pub exited: u32,
}

/// Stdio modes of [`vstd_proc_stdio`].
pub const STDIO_INHERIT: u32 = 0;
pub const STDIO_CAPTURE: u32 = 1;
pub const STDIO_NULL: u32 = 2;

/// Start building a command that runs `program` (looked up on `PATH` when
/// it has no separator). Never NULL; a bad name fails at spawn.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_command(program: *const u8, len: usize) -> *mut ProcCommand {
    // SAFETY: lent for the call.
    let raw = unsafe { bytes(program, len) };
    new_handle(
        HandleKind::Command,
        ProcCommand {
            program: os_string(raw),
            args: vec![],
            env_clear: false,
            env: vec![],
            cwd: None,
            stdio: [STDIO_INHERIT; 3],
        },
    )
}

/// Append one argument.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_arg(c: *mut ProcCommand, arg: *const u8, len: usize) {
    // SAFETY: a live handle lent `inout`; the argument is lent.
    let (c, raw) = unsafe { (&mut *c, bytes(arg, len)) };
    c.args.push(os_string(raw));
}

/// Set an environment variable for the child.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_env(
    c: *mut ProcCommand,
    key: *const u8,
    key_len: usize,
    value: *const u8,
    value_len: usize,
) {
    // SAFETY: as above.
    let c = unsafe { &mut *c };
    let (k, v) = unsafe { (bytes(key, key_len), bytes(value, value_len)) };
    c.env.push((k.to_vec(), Some(v.to_vec())));
}

/// Remove an environment variable from the child's environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_env_remove(c: *mut ProcCommand, key: *const u8, len: usize) {
    // SAFETY: as above.
    let (c, k) = unsafe { (&mut *c, bytes(key, len)) };
    c.env.push((k.to_vec(), None));
}

/// Start the child from an empty environment (variables set afterwards
/// still apply).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_env_clear(c: *mut ProcCommand) {
    // SAFETY: a live handle lent `inout`.
    let c = unsafe { &mut *c };
    c.env_clear = true;
    c.env.clear();
}

/// Run the child in directory `dir`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_cwd(c: *mut ProcCommand, dir: *const u8, len: usize) {
    // SAFETY: as above.
    let (c, d) = unsafe { (&mut *c, bytes(dir, len)) };
    c.cwd = Some(d.to_vec());
}

/// Set stream `which` (0 stdin, 1 stdout, 2 stderr) to `mode`. Capturing
/// stdin is not supported and means `null`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_stdio(c: *mut ProcCommand, which: u32, mode: u32) {
    // SAFETY: a live handle lent `inout`.
    let c = unsafe { &mut *c };
    if let Some(slot) = c.stdio.get_mut(which as usize) {
        *slot = mode;
    }
}

fn stdio(mode: u32) -> Stdio {
    match mode {
        STDIO_CAPTURE => Stdio::piped(),
        STDIO_NULL => Stdio::null(),
        _ => Stdio::inherit(),
    }
}

fn build(c: ProcCommand) -> std::io::Result<Command> {
    let mut cmd = Command::new(c.program?);
    for a in c.args {
        cmd.arg(a?);
    }
    if c.env_clear {
        cmd.env_clear();
    }
    for (k, v) in c.env {
        let k = os_string(&k)?;
        match v {
            Some(v) => cmd.env(k, os_string(&v)?),
            None => cmd.env_remove(k),
        };
    }
    if let Some(d) = c.cwd {
        cmd.current_dir(path_of(&d)?);
    }
    let stdin = if c.stdio[0] == STDIO_CAPTURE {
        STDIO_NULL
    } else {
        c.stdio[0]
    };
    cmd.stdin(stdio(stdin))
        .stdout(stdio(c.stdio[1]))
        .stderr(stdio(c.stdio[2]));
    Ok(cmd)
}

/// Spawn the command (consuming it). NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_spawn(c: *mut ProcCommand, err: *mut u64) -> *mut ProcChild {
    // SAFETY: `sink` consumes the command exactly once.
    let c = unsafe { free_handle(HandleKind::Command, c) };
    match build(c).and_then(|mut cmd| cmd.spawn()) {
        Ok(child) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, 0) };
            new_handle(
                HandleKind::Child,
                ProcChild {
                    child: Some(child),
                    stdout: vec![],
                    stderr: vec![],
                },
            )
        }
        Err(e) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, error_word(&e)) };
            std::ptr::null_mut()
        }
    }
}

/// Discard a command without running it (its finalizer).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_command_free(c: *mut ProcCommand) {
    // SAFETY: `sink` consumes the command exactly once.
    drop(unsafe { free_handle(HandleKind::Command, c) });
}

/// Wait for the child, collecting captured output while it runs. Returns
/// 0 and the status, or 1 on error. A second wait is an `InvalidInput`
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_wait(
    ch: *mut ProcChild,
    status: *mut ProcStatus,
    err: *mut u64,
) -> u32 {
    // SAFETY: a live handle lent `inout`; the status and error slots.
    let ch = unsafe { &mut *ch };
    let Some(child) = ch.child.take() else {
        // SAFETY: lent for the call.
        unsafe { set_err(err, kind_word(IoKind::InvalidInput)) };
        return 1;
    };
    match child.wait_with_output() {
        Ok(out) => {
            ch.stdout = out.stdout;
            ch.stderr = out.stderr;
            let s = &out.status;
            #[cfg(unix)]
            let signal = {
                use std::os::unix::process::ExitStatusExt;
                s.signal().unwrap_or(0) as u32
            };
            #[cfg(not(unix))]
            let signal = 0;
            // SAFETY: lent for the call.
            unsafe {
                *status = ProcStatus {
                    code: s.code().map_or(0, i64::from),
                    signal,
                    exited: u32::from(s.code().is_some()),
                };
                set_err(err, 0);
            }
            0
        }
        Err(e) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, error_word(&e)) };
            1
        }
    }
}

/// Captured output of stream `which` (1 stdout, 2 stderr) into `buf`, by
/// the copy-out protocol: returns its length and copies only when it fits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_output(
    ch: *const ProcChild,
    which: u32,
    buf: *mut u8,
    len: usize,
) -> u64 {
    // SAFETY: a live handle lent `in`; the buffer.
    let (ch, buf) = unsafe { (&*ch, bytes_mut(buf, len)) };
    let data = if which == 2 { &ch.stderr } else { &ch.stdout };
    copy_out(data, buf)
}

/// The child's process id (0 once waited).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_child_id(ch: *const ProcChild) -> u64 {
    // SAFETY: a live handle lent `in`.
    let ch = unsafe { &*ch };
    ch.child.as_ref().map_or(0, |c| u64::from(c.id()))
}

/// Release a child (its finalizer). A child that was never waited is
/// killed and reaped first, so no zombie is left behind.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_proc_free(ch: *mut ProcChild) {
    // SAFETY: `sink` consumes the handle exactly once.
    let ch = unsafe { free_handle(HandleKind::Child, ch) };
    if let Some(mut c) = ch.child {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// End this process with `code` (flushing nothing Vesper holds).
#[unsafe(no_mangle)]
pub extern "C" fn vstd_proc_exit(code: i64) {
    std::process::exit(code as i32)
}

/// This process's id.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_proc_id() -> u64 {
    u64::from(std::process::id())
}

pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vstd_proc_command", vstd_proc_command as *const u8),
        ("vstd_proc_arg", vstd_proc_arg as *const u8),
        ("vstd_proc_env", vstd_proc_env as *const u8),
        ("vstd_proc_env_remove", vstd_proc_env_remove as *const u8),
        ("vstd_proc_env_clear", vstd_proc_env_clear as *const u8),
        ("vstd_proc_cwd", vstd_proc_cwd as *const u8),
        ("vstd_proc_stdio", vstd_proc_stdio as *const u8),
        ("vstd_proc_spawn", vstd_proc_spawn as *const u8),
        (
            "vstd_proc_command_free",
            vstd_proc_command_free as *const u8,
        ),
        ("vstd_proc_wait", vstd_proc_wait as *const u8),
        ("vstd_proc_output", vstd_proc_output as *const u8),
        ("vstd_proc_child_id", vstd_proc_child_id as *const u8),
        ("vstd_proc_free", vstd_proc_free as *const u8),
        ("vstd_proc_exit", vstd_proc_exit as *const u8),
        ("vstd_proc_id", vstd_proc_id as *const u8),
    ]
}
