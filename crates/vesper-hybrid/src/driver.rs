//! The hybrid's `main` (self-hosting SH5 lane 12, #1335): the Vesper
//! driver of `modules/driver` runs `vesper check`, `run`, `jit`, `build`
//! and `emit`, and this module is the host it calls.
//!
//! `@vesper/driver` (packages/vesper-driver) parses and validates the
//! arguments; `@vesper/hybrid-driver` decides the pass sequence and runs
//! the linker through `@vesper-std/process`. Each compiler pass is one
//! [`Step`] here, reached through the `HybridDriver` import's `hd_step`,
//! over the state a [`Session`] keeps between passes. The passes are the
//! Rust CLI's own code (the included src/main.rs's helpers), so the hybrid
//! prints what the default compiler prints. Every other command goes to the
//! included Rust `main`.
use std::{
    collections::BTreeSet,
    ffi::c_int,
    io::{self, Write},
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
};

use vesper::{
    check, ir, lean, native, native_verifiers::NativeVerifier, optimize, packages, parser,
    runtime::Runtime,
};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/driver.rs"]
mod binding;
// `Session::step`, one file a group of steps (#1896).
#[path = "driver/backends.rs"]
mod backends;
#[path = "driver/front.rs"]
mod front;
#[path = "driver/native.rs"]
mod native_steps;

/// The process entry: the Vesper driver on the CLI thread (the same name
/// and stack as the Rust CLI's), or the Rust CLI when the driver hands the
/// command back.
pub fn main() -> c_int {
    // The Rust runtime's start-up ignores SIGPIPE; without `fn main` as the
    // entry nothing else does (writes to a closed pipe must fail, not kill).
    #[cfg(unix)]
    // SAFETY: setting a signal disposition before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let code = std::thread::Builder::new()
        .name("vesper".to_owned())
        .stack_size(crate::CLI_STACK_BYTES)
        .spawn(binding::main_code)
        .expect("spawn the vesper CLI thread")
        .join()
        // Passes catch their panics; the Vesper code does not unwind.
        .unwrap_or(PANIC_EXIT);
    if code == binding::delegate() {
        // Dispatch through `cli` directly and return `c_int` rather than
        // calling `crate::main()` / `std::process::exit`: on Windows MSVC,
        // `std::process::exit` calls `ExitProcess`, skipping C `atexit(report)`
        // (`VESPER_HYBRID_STATS=1`).
        const _: fn() = crate::main;
        if let Some(tool_code) =
            crate::tool_commands::dispatch(|c| crate::COMPILER_COMMANDS.contains(&c))
        {
            let _ = io::stdout().flush();
            return tool_code as c_int;
        }
        let result = std::thread::Builder::new()
            .name("vesper".to_owned())
            .stack_size(crate::CLI_STACK_BYTES)
            .spawn(crate::cli)
            .expect("spawn the vesper CLI thread")
            .join();
        let _ = io::stdout().flush();
        return match result {
            Ok(Ok(())) => 0,
            Ok(Err(e)) => {
                eprintln!("vesper: {e}");
                1
            }
            Err(_) => PANIC_EXIT as c_int,
        };
    }
    let _ = io::stdout().flush();
    c_int::try_from(code).unwrap_or(1)
}

/// Passes the Vesper driver ran (`VESPER_HYBRID_STATS=1` reports it).
pub static PASSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The exit status of a Rust panic.
const PANIC_EXIT: i64 = 101;

/// The CLI options `@vesper/driver`'s record carries (`record` in
/// packages/vesper-driver/src/lib.ves).
struct Options {
    command: &'static str,
    include_tests: bool,
    json: bool,
    optimize: bool,
    tee: bool,
    lib: bool,
    verifiers: BTreeSet<NativeVerifier>,
    fuel: u64,
    slots: [Option<String>; SLOTS],
    link_search: Vec<PathBuf>,
    instances: Vec<String>,
}

/// The record's version (`record_format` in @vesper/driver).
const RECORD_FORMAT: u64 = 2;
const SLOTS: usize = 14;
// Slot numbers (`slot_*` in @vesper/driver).
const INPUT: usize = 0;
const OUTPUT: usize = 1;
const FORMAT: usize = 2;
const ENTRY: usize = 3;
const TARGET: usize = 4;
const LINKER: usize = 5;
const PACKAGE: usize = 6;
const POLICY: usize = 7;
const CODEGEN: usize = 8;
const LIB_NAME: usize = 9;
const INTERFACE: usize = 10;
const RUST_BINDING: usize = 11;
const RUST_TYPES: usize = 12;
const TARGET_FEATURES: usize = 13;
// List kinds (`list_*`).
const LINK_SEARCH: u64 = 0;
const INSTANCE: u64 = 1;
// Flag bits.
const INCLUDE_TESTS: u64 = 1;
const JSON: u64 = 2;
const OPTIMIZE: u64 = 4;
const TEE: u64 = 8;
const LIB: u64 = 16;

impl Options {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { bytes, at: 0 };
        let command = match r.word()? {
            1 => "check",
            2 => "run",
            3 => "jit",
            4 => "build",
            5 => "emit",
            _ => return None,
        };
        let flags = r.word()?;
        let bits = r.word()?;
        let verifiers = NativeVerifier::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| bits & (1 << i) != 0)
            .map(|(_, v)| v)
            .collect();
        let fuel = r.word()?;
        let mut slots: [Option<String>; SLOTS] = Default::default();
        for slot in &mut slots {
            let present = r.word()? != 0;
            let text = r.text()?;
            *slot = present.then_some(text);
        }
        let mut link_search = vec![];
        let mut instances = vec![];
        for _ in 0..r.word()? {
            let kind = r.word()?;
            let text = r.text()?;
            match kind {
                LINK_SEARCH => link_search.push(PathBuf::from(text)),
                INSTANCE => instances.push(text),
                _ => return None,
            }
        }
        (r.at == bytes.len()).then_some(Options {
            command,
            include_tests: flags & INCLUDE_TESTS != 0,
            json: flags & JSON != 0,
            optimize: flags & OPTIMIZE != 0,
            tee: flags & TEE != 0,
            lib: flags & LIB != 0,
            verifiers,
            fuel,
            slots,
            link_search,
            instances,
        })
    }

    fn slot(&self, k: usize) -> Option<&str> {
        self.slots[k].as_deref()
    }

    fn input(&self) -> &str {
        self.slot(INPUT).unwrap_or_default()
    }

    fn format(&self) -> &str {
        self.slot(FORMAT).unwrap_or_default()
    }

    fn entry(&self) -> &str {
        self.slot(ENTRY).unwrap_or_default()
    }

    fn target(&self) -> Option<&str> {
        self.slot(TARGET)
    }

    fn linker(&self) -> &str {
        self.slot(LINKER).unwrap_or_default()
    }

    fn output(&self) -> Option<PathBuf> {
        self.slot(OUTPUT).map(PathBuf::from)
    }

    fn policy(&self) -> Option<&Path> {
        self.slot(POLICY).map(Path::new)
    }

    /// `--verify`, `--verify-policy`, `--tee` or `--codegen` was given.
    fn exclusive(&self) -> bool {
        !self.verifiers.is_empty()
            || self.policy().is_some()
            || self.tee
            || self.slot(CODEGEN).is_some()
    }

    /// The CLI's `LibFlags`, rebuilt by feeding it the flags again.
    fn lib_flags(&self) -> Result<crate::lib_commands::LibFlags, String> {
        let mut flags = crate::lib_commands::LibFlags::default();
        let mut args: Vec<String> = vec![];
        if self.lib {
            args.push("--lib".into());
        }
        for (flag, k) in [
            ("--lib-name", LIB_NAME),
            ("--interface", INTERFACE),
            ("--rust-binding", RUST_BINDING),
            ("--rust-types", RUST_TYPES),
        ] {
            if let Some(v) = self.slot(k) {
                args.extend([flag.to_owned(), v.to_owned()]);
            }
        }
        for i in &self.instances {
            args.extend(["--instance".to_owned(), i.clone()]);
        }
        let mut args = args.into_iter();
        while let Some(a) = args.next() {
            flags.flag(&a, &mut args)?;
        }
        Ok(flags)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn word(&mut self) -> Option<u64> {
        let end = self.at.checked_add(8)?;
        let w = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(u64::from_le_bytes(w.try_into().ok()?))
    }

    fn text(&mut self) -> Option<String> {
        let len = usize::try_from(self.word()?).ok()?;
        let end = self.at.checked_add(len)?;
        let t = self.bytes.get(self.at..end)?;
        self.at = end;
        String::from_utf8(t.to_vec()).ok()
    }
}

/// What the passes keep between calls: the state `cli()` keeps in locals.
pub struct Session {
    o: Options,
    package: bool,
    loaded: Option<packages::Assembled>,
    /// The input as read (`original` in `cli()`).
    original: String,
    /// The compiled text: `original`, or its sugar expansion (`source`).
    source: String,
    /// Maps the expansion's spans back to `original` (`source_map`).
    source_map: Vec<vesper::forge_frontend::SourceMapSegment>,
    parsed: Option<parser::Parse>,
    checked: Option<check::Checked>,
    /// The last error message or pass result (`hd_text`).
    text: Vec<u8>,
    /// `WasmInput` handed the build to the Vesper driver (#1895).
    wasm_input: bool,
    /// `EbpfInput` did.
    ebpf_input: bool,
}

/// The passes (`step_*` in modules/driver/src/lib.ves).
///
/// New steps take a number from their group's range, so lanes adding steps
/// in parallel (#1896) don't collide: 28-39 the front-end passes (#1900),
/// 40-49 front end (driver/front.rs, #1893), 50-59 IR and native
/// (driver/native.rs, #1894), 60-69 the other backends
/// (driver/backends.rs, #1895), 70-79 session and I/O (this file, #1896).
#[derive(Clone, Copy)]
enum Step {
    IsPackage = 1,
    Load,
    EbpfBuild,
    Parse,
    EmitCst,
    ReportSyntax,
    PackageVsys,
    VsysRun,
    Check,
    LibBuild,
    EmitInterface,
    Optimize,
    Run,
    Jit,
    VerifiedBuild,
    WasmBuild,
    CheckMain,
    NativeObject,
    IsWindowsTarget,
    WindowsLink,
    Tee,
    EmitHir,
    EmitIr,
    EmitLean,
    EmitClif = 26,
    IoError,
    WasmInput = 60,
    EbpfInput,
}

const STEPS: [Step; 28] = [
    Step::IsPackage,
    Step::Load,
    Step::EbpfBuild,
    Step::Parse,
    Step::EmitCst,
    Step::ReportSyntax,
    Step::PackageVsys,
    Step::VsysRun,
    Step::Check,
    Step::LibBuild,
    Step::EmitInterface,
    Step::Optimize,
    Step::Run,
    Step::Jit,
    Step::VerifiedBuild,
    Step::WasmBuild,
    Step::CheckMain,
    Step::NativeObject,
    Step::IsWindowsTarget,
    Step::WindowsLink,
    Step::Tee,
    Step::EmitHir,
    Step::EmitIr,
    Step::EmitLean,
    Step::EmitClif,
    Step::IoError,
    Step::WasmInput,
    Step::EbpfInput,
];

/// A pass failed; its message is the session's text.
const FAIL: i64 = -1;
/// A pass panicked (the panic hook already printed it).
const PANIC: i64 = -2;

impl Session {
    fn parsed(&self) -> Result<&parser::Parse, String> {
        self.parsed
            .as_ref()
            .ok_or_else(|| "driver: no parse yet".into())
    }

    fn checked(&self) -> Result<&check::Checked, String> {
        self.checked
            .as_ref()
            .ok_or_else(|| "driver: no check yet".into())
    }

    fn package_vsys(&self) -> &[packages::PackageVsys] {
        self.loaded.as_ref().map_or(&[][..], |p| &p.vsys[..])
    }

    fn step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        let o = &self.o;
        match step {
            Step::IsPackage => {
                self.package = packages::is_package(Path::new(o.input()));
                Ok(i64::from(self.package))
            }
            Step::Load => {
                let input = o.input();
                if self.package {
                    let path = Path::new(input);
                    let assembled = packages::load_for_target(path, o.target(), o.include_tests)?;
                    if !assembled.ok() {
                        crate::package_diagnostics(&assembled, &assembled.diagnostics, o.json);
                        return Ok(count(
                            assembled.diagnostics.len() + assembled.manifest_errors.len(),
                        ));
                    }
                    self.original = assembled.sources.combined();
                    self.loaded = Some(assembled);
                } else {
                    self.original = io::read_to_string(arg).map_err(|e| format!("{input}: {e}"))?;
                }
                Ok(0)
            }
            Step::PackageVsys => Ok(count(self.package_vsys().len())),
            Step::VsysRun => {
                // Programs over package-carried imports check and run only
                // through the Bridge lowering (#1150).
                let p = self.loaded.as_ref().ok_or("driver: no package")?;
                crate::vsys_commands::run(crate::vsys_commands::Command {
                    command: o.command,
                    format: o.format(),
                    package: p,
                    module: &self.parsed()?.module,
                    entry: o.entry(),
                    output: o.output(),
                    target: o.target(),
                    optimize: o.optimize,
                    linker: o.linker(),
                    link_search: &o.link_search,
                    json: o.json,
                    exclusive: o.exclusive(),
                })?;
                Ok(0)
            }
            Step::CheckMain => {
                let f = self
                    .checked()?
                    .world
                    .functions
                    .get("main")
                    .ok_or("missing main function")?;
                if !f.params.is_empty()
                    || !(vesper::numeric::is_integer_name(f.result.name())
                        || matches!(f.result.name(), "Unit" | ""))
                {
                    return Err(
                        "executable main must take no arguments and return an integer or Unit"
                            .into(),
                    );
                }
                Ok(0)
            }
            Step::IsWindowsTarget => Ok(i64::from(native::windows::is_windows_target(o.target()))),
            Step::Tee => {
                crate::measure_tee(Path::new(utf8(arg)?))?;
                Ok(0)
            }
            Step::IoError => {
                let word =
                    u64::from_le_bytes(arg.try_into().map_err(|_| "driver: bad error word")?);
                self.text = io_error(word).to_string().into_bytes();
                Ok(0)
            }
            Step::Parse
            | Step::EmitCst
            | Step::ReportSyntax
            | Step::Check
            | Step::EmitHir
            | Step::EmitInterface
            | Step::LibBuild => self.front_step(step),
            Step::Optimize
            | Step::EmitIr
            | Step::EmitClif
            | Step::EmitLean
            | Step::NativeObject
            | Step::WindowsLink
            | Step::Run
            | Step::Jit => self.native_step(step, arg),
            Step::WasmBuild
            | Step::EbpfBuild
            | Step::VerifiedBuild
            | Step::WasmInput
            | Step::EbpfInput => self.backends_step(step, arg),
        }
    }
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn utf8(arg: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(arg).map_err(|e| format!("driver: {e}"))
}

/// The `io::Error` of a `@vesper-std` error word: the OS error when there
/// is one, else its kind.
fn io_error(word: u64) -> io::Error {
    use io::ErrorKind as K;
    let os = (word >> 32) as u32;
    if os != 0 {
        return io::Error::from_raw_os_error(os as i32);
    }
    let kind = match word & 0xff {
        1 => K::NotFound,
        2 => K::PermissionDenied,
        3 => K::AlreadyExists,
        4 => K::NotADirectory,
        5 => K::IsADirectory,
        6 => K::DirectoryNotEmpty,
        7 => K::InvalidInput,
        8 => K::Interrupted,
        9 => K::UnexpectedEof,
        10 => K::WriteZero,
        11 => K::Unsupported,
        12 => K::OutOfMemory,
        _ => K::Other,
    };
    io::Error::from(kind)
}

/// The `HybridDriver` import's symbols (the `// @export vsys direct
/// rust=crate::driver::host` block of modules/driver/src/lib.ves).
pub mod host {
    use super::*;

    /// The bytes of a lent `nocapture in CSlice[u8]`.
    ///
    /// # Safety
    /// `ptr` is valid for `len` bytes for the call (never NULL).
    unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
        if len == 0 || ptr.is_null() {
            return &[];
        }
        // SAFETY: as documented.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    /// Write `text` to stdout (1) or stderr (2).
    ///
    /// # Safety
    /// `text` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_print(stream: u32, text: *const u8, len: usize) {
        // SAFETY: lent for the call.
        let text = unsafe { bytes(text, len) };
        let _ = if stream == 2 {
            io::stderr().lock().write_all(text)
        } else {
            io::stdout().lock().write_all(text)
        };
    }

    /// The CLI's help (`--help` among a compile command's options).
    #[unsafe(no_mangle)]
    pub extern "C" fn hd_help() {
        print!("{}", crate::HELP);
    }

    /// A session over `@vesper/driver`'s record, or NULL when the record
    /// does not decode.
    ///
    /// # Safety
    /// `record` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_session(record: *const u8, len: usize) -> *mut Session {
        if binding::record_format() != RECORD_FORMAT {
            return std::ptr::null_mut();
        }
        // SAFETY: lent for the call.
        let record = unsafe { bytes(record, len) };
        match Options::decode(record) {
            Some(o) => {
                // `cli()` sets them once the argument checks reach them; the
                // driver's checks have all passed here.
                if let Some(features) = o.slot(TARGET_FEATURES) {
                    vesper::native::set_target_features(features);
                }
                Box::into_raw(Box::new(Session {
                    o,
                    package: false,
                    loaded: None,
                    original: String::new(),
                    source: String::new(),
                    source_map: Vec::new(),
                    parsed: None,
                    checked: None,
                    text: vec![],
                    wasm_input: false,
                    ebpf_input: false,
                }))
            }
            None => std::ptr::null_mut(),
        }
    }

    /// Free a session (`sink`).
    ///
    /// # Safety
    /// `s` came from `hd_session` and is freed once.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_free(s: *mut Session) {
        if !s.is_null() {
            // SAFETY: as documented.
            drop(unsafe { Box::from_raw(s) });
        }
    }

    /// Run pass `step` with its argument: its count or 0, `FAIL` (the
    /// message is the session's text) or `PANIC`.
    ///
    /// # Safety
    /// `s` is a live session lent `inout`; `arg` a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_step(
        s: *mut Session,
        step: u32,
        arg: *const u8,
        len: usize,
    ) -> i64 {
        // SAFETY: lent for the call.
        let (s, arg) = unsafe { (&mut *s, bytes(arg, len)) };
        PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(step) = STEPS.iter().copied().find(|k| *k as u32 == step) else {
            s.text = format!("driver: unknown pass {step}").into_bytes();
            return FAIL;
        };
        match panic::catch_unwind(AssertUnwindSafe(|| s.step(step, arg))) {
            Ok(Ok(n)) => n,
            Ok(Err(message)) => {
                s.text = message.into_bytes();
                FAIL
            }
            Err(_) => PANIC,
        }
    }

    /// The session's `Parse` from a parse the Vesper driver ran over the
    /// source `Load` read (`front.ves` `parse_source`): its diagnostic
    /// count. The text compiled is the source as read, with no sugar map.
    ///
    /// # Safety
    /// `s` is a live session lent `inout`; `words` a lent word slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_parsed(s: *mut Session, words: *const u64, len: usize) -> i64 {
        // SAFETY: lent for the call.
        let s = unsafe { &mut *s };
        let words = if len == 0 || words.is_null() {
            &[][..]
        } else {
            // SAFETY: `words` is valid for `len` words for the call.
            unsafe { std::slice::from_raw_parts(words, len) }
        };
        PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match panic::catch_unwind(AssertUnwindSafe(|| {
            let parsed = crate::vesper_hybrid::driver_parse(&s.original, words);
            let n = count(parsed.diagnostics.len());
            s.source = s.original.clone();
            s.source_map = Vec::new();
            s.parsed = Some(parsed);
            n
        })) {
            Ok(n) => n,
            Err(_) => PANIC,
        }
    }

    /// 1 when the Vesper driver parses a standalone source itself (the
    /// parse swap is installed), else 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn hd_front_begin() -> u32 {
        u32::from(crate::vesper_hybrid::driver_parse_begin())
    }

    /// `crate::write(None, ..)` of `cli()`: `text` to stdout, 0 or `FAIL`
    /// with the write error as the session's text.
    ///
    /// # Safety
    /// `s` is a live session lent `inout`; `text` a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_write(s: *mut Session, text: *const u8, len: usize) -> i64 {
        // SAFETY: lent for the call.
        let (s, text) = unsafe { (&mut *s, bytes(text, len)) };
        match crate::write(None, text) {
            Ok(()) => 0,
            Err(message) => {
                s.text = message.into_bytes();
                FAIL
            }
        }
    }

    /// Copy the session's text into `buf` when it fits; its length.
    ///
    /// # Safety
    /// `s` is a live session lent `in`; `buf` a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_text(s: *const Session, buf: *mut u8, len: usize) -> i64 {
        // SAFETY: lent for the call.
        let s = unsafe { &*s };
        if s.text.len() <= len && !buf.is_null() {
            // SAFETY: `buf` holds `len >= text.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(s.text.as_ptr(), buf, s.text.len()) };
        }
        count(s.text.len())
    }
}
