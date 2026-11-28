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
#[path = "driver/load.rs"]
mod load;
#[path = "driver/native.rs"]
mod native_steps;
#[path = "driver/world.rs"]
mod world;

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
const RECORD_FORMAT: u64 = 3;
const SLOTS: usize = 14;
// Slot numbers (`slot_*` in @vesper/driver).
const INPUT: usize = 0;
const OUTPUT: usize = 1;
const FORMAT: usize = 2;
const ENTRY: usize = 3;
const TARGET: usize = 4;
const LINKER: usize = 5;
// 6 is `--package`, which only the Vesper driver reads (interface export).
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
    loaded: Option<packages::Assembled>,
    /// The package graph `LoadPackage` read for the Vesper driver to
    /// assemble (`hd_assembled`, #1986).
    loading: Option<packages::Loading<'static>>,
    /// The package graph the Vesper driver read itself (`Graph`), for
    /// `hd_assembled`.
    graph: Option<crate::vesper_hybrid::DriverGraph>,
    /// The Vesper driver is reading the package graph (`LoadBegin` said
    /// so); a `LoadPackage` now is its fallback to the Rust loader.
    reading: bool,
    /// The input as read (`original` in `cli()`).
    original: String,
    /// The compiled text: `original`, or its sugar expansion (`source`).
    source: String,
    /// Maps the expansion's spans back to `original` (`source_map`).
    source_map: Vec<vesper::forge_frontend::SourceMapSegment>,
    parsed: Option<parser::Parse>,
    checked: Option<check::Checked>,
    /// `check::check`'s state between its parts.
    checking: Checking,
    /// The last error message or pass result (`hd_text`).
    text: Vec<u8>,
    /// `WasmInput` handed the build to the Vesper driver (#1895).
    wasm_input: bool,
    /// `EbpfInput` did.
    ebpf_input: bool,
    /// `VerifiedInput` did.
    verified_input: bool,
}

/// What `check::check` keeps between the parts the Vesper driver
/// sequences (`check` in modules/driver/src/lib.ves, #1900).
#[derive(Default)]
struct Checking {
    /// The module the next part checks when the untyped value-enum members
    /// rewrote the parse's (the latest rewrite).
    rewritten: Option<vesper::ast::Module>,
    /// The diagnostics of each rewrite, outermost first: `check` appends
    /// them innermost first once the rewritten module is checked.
    pending: Vec<Vec<vesper::ast::Diagnostic>>,
}

/// The passes (`step_*` in modules/driver/src/lib.ves).
///
/// New steps take a number from their group's range, so lanes adding steps
/// in parallel (#1896) don't collide: 28-39 the front-end passes (#1900),
/// 40-49 front end (driver/front.rs, #1893), 50-59 IR and native
/// (driver/native.rs, #1894), 60-69 the other backends
/// (driver/backends.rs, #1895), 70-79 session and I/O (this file, #1896),
/// 80-89 the world the Vesper driver holds (driver/world.rs, #2024).
#[derive(Clone, Copy)]
enum Step {
    // 1 (`IsPackage`), 19 (`IsWindowsTarget`), 21 (`Tee`) and 27
    // (`IoError`) moved to the Vesper driver (#1896), 17 (`CheckMain`)
    // to the world it holds (#2024), and 11 (`EmitInterface`, the package
    // name of an interface export) to the Vesper driver's manifest read
    // (#1893); numbers are not reused.
    Load = 2,
    EbpfBuild = 3,
    Parse = 4,
    EmitCst = 5,
    ReportSyntax = 6,
    PackageVsys = 7,
    VsysRun = 8,
    Check = 9,
    LibBuild = 10,
    Optimize = 12,
    Run = 13,
    Jit = 14,
    VerifiedBuild = 15,
    WasmBuild = 16,
    NativeObject = 18,
    WindowsLink = 20,
    EmitHir = 22,
    EmitIr = 23,
    EmitLean = 24,
    EmitClif = 26,
    // `check::check`'s parts, sequenced by `check` in
    // modules/driver/src/lib.ves (#1900).
    CheckUntyped = 28,
    CheckTensor = 29,
    CheckFrontend = 30,
    CheckUses = 31,
    CheckFinish = 32,
    CheckReport = 33,
    WasmInput = 60,
    EbpfInput = 61,
    VerifiedInput = 62,
    /// Load the package the Vesper driver found at the input (#1896).
    LoadPackage = 70,
    // The Vesper driver's own read of the package graph (#1986 slice 2,
    // driver/load.rs); 74 (`VsysNames`) moved to @vesper/vsys (#2760).
    LoadBegin = 71,
    Graph = 75,
    /// The checked world's declarations, for the Vesper driver to hold.
    WorldDecls = 80,
    /// 1 when the Vesper driver collects the declarations itself (the
    /// world swap, #2024 slice 2c), else 0.
    WorldBegin = 81,
}

const STEPS: [Step; 34] = [
    Step::Load,
    Step::EbpfBuild,
    Step::Parse,
    Step::EmitCst,
    Step::ReportSyntax,
    Step::PackageVsys,
    Step::VsysRun,
    Step::Check,
    Step::LibBuild,
    Step::Optimize,
    Step::Run,
    Step::Jit,
    Step::VerifiedBuild,
    Step::WasmBuild,
    Step::NativeObject,
    Step::WindowsLink,
    Step::EmitHir,
    Step::EmitIr,
    Step::EmitLean,
    Step::EmitClif,
    Step::CheckUntyped,
    Step::CheckTensor,
    Step::CheckFrontend,
    Step::CheckUses,
    Step::CheckFinish,
    Step::CheckReport,
    Step::WasmInput,
    Step::EbpfInput,
    Step::VerifiedInput,
    Step::LoadPackage,
    Step::LoadBegin,
    Step::Graph,
    Step::WorldDecls,
    Step::WorldBegin,
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

    /// The end of `LoadPackage`: reports an assembled package's errors
    /// (their count), or keeps it (0).
    fn package_loaded(&mut self, assembled: packages::Assembled) -> i64 {
        if !assembled.ok() {
            crate::package_diagnostics(&assembled, &assembled.diagnostics, self.o.json);
            return count(assembled.diagnostics.len() + assembled.manifest_errors.len());
        }
        self.original = assembled.sources.combined();
        self.loaded = Some(assembled);
        0
    }

    fn step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        let o = &self.o;
        match step {
            // Reads the package graph. When the Vesper driver assembles it
            // (#1986), the graph, read without its parses, waits in the
            // session for `hd_assembled` and its assembly input is the
            // session's text; else the Rust loader loads it here and the
            // text is empty.
            Step::LoadPackage => {
                if self.reading {
                    // The Vesper driver's read handed the graph back.
                    self.reading = false;
                    crate::vesper_hybrid::driver_read_fallback();
                }
                let timer = crate::vesper_hybrid::driver_load_timer();
                let path = Path::new(o.input());
                self.text.clear();
                let assembled = if crate::vesper_hybrid::driver_assembles(o.include_tests) {
                    let loading = packages::read_for_target(path, o.target(), false, true)?;
                    if let Some(input) = crate::vesper_hybrid::driver_assembly_input(&loading) {
                        self.text = input;
                        self.loading = Some(loading);
                        timer.stop();
                        return Ok(0);
                    }
                    // Read without its parses: load it again, parsed.
                    packages::load_for_target(path, o.target(), false)?
                } else {
                    packages::load_for_target(path, o.target(), o.include_tests)?
                };
                let n = self.package_loaded(assembled);
                timer.stop();
                Ok(n)
            }
            // The standalone source the Vesper driver read.
            Step::Load => {
                self.original =
                    io::read_to_string(arg).map_err(|e| format!("{}: {e}", o.input()))?;
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
            Step::Parse
            | Step::EmitCst
            | Step::ReportSyntax
            | Step::Check
            | Step::CheckUntyped
            | Step::CheckTensor
            | Step::CheckFrontend
            | Step::CheckUses
            | Step::CheckFinish
            | Step::CheckReport
            | Step::EmitHir
            | Step::LibBuild => self.front_step(step, arg),
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
            | Step::EbpfInput
            | Step::VerifiedInput => self.backends_step(step, arg),
            Step::LoadBegin | Step::Graph => self.load_step(step, arg),
            Step::WorldDecls | Step::WorldBegin => self.world_step(step, arg),
        }
    }
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn utf8(arg: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(arg).map_err(|e| format!("driver: {e}"))
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

    /// One command answered by the Vesper command layers
    /// (`crate::commands::ANSWERS`).
    #[unsafe(no_mangle)]
    pub extern "C" fn hd_commands_answered() {
        crate::commands::ANSWERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                    loaded: None,
                    loading: None,
                    graph: None,
                    reading: false,
                    original: String::new(),
                    source: String::new(),
                    source_map: Vec::new(),
                    parsed: None,
                    checked: None,
                    checking: Checking::default(),
                    text: vec![],
                    wasm_input: false,
                    ebpf_input: false,
                    verified_input: false,
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

    /// The package graph `LoadPackage` read, as the Vesper driver
    /// assembled it (`assembled_words` of modules/driver/src/assemble.ves,
    /// #1986): `LoadPackage`'s result for it.
    ///
    /// # Safety
    /// `s` is a live session lent `inout`; `words` a lent word slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hd_assembled(s: *mut Session, words: *const u64, len: usize) -> i64 {
        // SAFETY: lent for the call.
        let s = unsafe { &mut *s };
        let words = if len == 0 || words.is_null() {
            &[][..]
        } else {
            // SAFETY: `words` is valid for `len` words for the call.
            unsafe { std::slice::from_raw_parts(words, len) }
        };
        PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The Vesper assembly between the two timers is the assemble
        // cost's Vesper time; the stats' load time adds it.
        let timer = crate::vesper_hybrid::driver_load_timer();
        match panic::catch_unwind(AssertUnwindSafe(|| {
            let o = &s.o;
            let rust =
                || packages::load_for_target(Path::new(o.input()), o.target(), o.include_tests);
            let assembled = if let Some(graph) = s.graph.take() {
                crate::vesper_hybrid::driver_graph_assembled(graph, words, rust)
            } else if let Some(loading) = s.loading.take() {
                crate::vesper_hybrid::driver_assembled(loading, words, rust)
            } else {
                s.text = b"driver: no package graph".to_vec();
                return FAIL;
            };
            let n = match assembled {
                Ok(assembled) => s.package_loaded(assembled),
                Err(message) => {
                    s.text = message.into_bytes();
                    FAIL
                }
            };
            timer.stop();
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
