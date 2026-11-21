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
    fs,
    io::{self, Write},
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
};

use vesper::{
    check, ir, lean, native, native_verifiers::NativeVerifier, optimize, packages, parser,
    runtime::Runtime,
};

/// The standard I/O host the driver's `@vesper-std/fs`, `process` and
/// `env` imports bind to (the binding names it).
pub mod std_host {
    pub use vesper_std_host::{env, fs, process};
}

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/driver.rs"]
mod binding;

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
        // A panic in the Rust CLI (it resumes its CLI thread's) must not
        // unwind across this C entry, which would abort: Rust's own start-up
        // exits with 101 after the hook printed it.
        let delegated = panic::catch_unwind(crate::main);
        let _ = io::stdout().flush();
        if delegated.is_err() {
            std::process::exit(PANIC_EXIT as i32);
        }
        return 0;
    }
    let _ = io::stdout().flush();
    std::process::exit(i32::try_from(code).unwrap_or(1))
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
const RECORD_FORMAT: u64 = 1;
const SLOTS: usize = 13;
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
    source: String,
    parsed: Option<parser::Parse>,
    checked: Option<check::Checked>,
    /// The last error message or pass result (`hd_text`).
    text: Vec<u8>,
}

/// The passes (`step_*` in modules/driver/src/lib.ves).
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
    EmitObject,
    EmitClif,
    IoError,
}

const STEPS: [Step; 27] = [
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
    Step::EmitObject,
    Step::EmitClif,
    Step::IoError,
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

    fn native_code(&self) -> Result<(Vec<u8>, String), String> {
        crate::native_code(
            &self.checked()?.world,
            self.o.entry(),
            self.o.target(),
            self.o.optimize,
            &self.o.verifiers,
            self.o.policy(),
            (self.o.input(), &self.source),
        )
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
                    let assembled = if o.include_tests {
                        packages::load_for_tests(path)?
                    } else {
                        packages::load(path)?
                    };
                    if !assembled.ok() {
                        crate::package_diagnostics(&assembled, &assembled.diagnostics, o.json);
                        return Ok(count(
                            assembled.diagnostics.len() + assembled.manifest_errors.len(),
                        ));
                    }
                    self.source = assembled.sources.combined();
                    self.loaded = Some(assembled);
                } else {
                    self.source = fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
                }
                Ok(0)
            }
            Step::EbpfBuild => {
                // Compiler eBPF target (§27): verified BPF ELF object with BTF.
                let summary = vesper::ebpf_backend::build_command(
                    o.input(),
                    &self.source,
                    o.entry(),
                    o.output().as_deref(),
                )?;
                print!("{summary}");
                Ok(0)
            }
            Step::Parse => {
                let parsed = match &self.loaded {
                    Some(p) => parser::Parse {
                        module: p.module.clone(),
                        green: parser::parse("").green,
                        diagnostics: vec![],
                    },
                    None if o.include_tests => parser::parse_for_tests(&self.source),
                    None => parser::parse(&self.source),
                };
                let n = count(parsed.diagnostics.len());
                self.parsed = Some(parsed);
                Ok(n)
            }
            Step::EmitCst => {
                let parsed = self.parsed()?;
                if !parsed.diagnostics.is_empty() {
                    crate::diagnostics(o.input(), &self.source, &parsed.diagnostics, o.json);
                }
                crate::write(o.output(), format!("{:#?}\n", parsed.syntax()).as_bytes())?;
                Ok(0)
            }
            Step::ReportSyntax => {
                let parsed = self.parsed()?;
                crate::diagnostics(o.input(), &self.source, &parsed.diagnostics, o.json);
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
            Step::Check => {
                let checked = check::check(&self.parsed()?.module);
                let lib = o.lib_flags()?;
                // A library with `.vsys` imports checks only through the
                // Bridge lowering, which the export path runs.
                let n = if !checked.diagnostics.is_empty()
                    && !lib.needs_module(&self.source, !self.package_vsys().is_empty())
                {
                    match &self.loaded {
                        Some(p) => {
                            let localized: Vec<_> = checked
                                .diagnostics
                                .iter()
                                .map(|d| p.sources.localize(d))
                                .collect();
                            crate::package_diagnostics(p, &localized, o.json);
                        }
                        None => crate::diagnostics(
                            o.input(),
                            &self.source,
                            &checked.diagnostics,
                            o.json,
                        ),
                    }
                    count(checked.diagnostics.len())
                } else {
                    0
                };
                self.checked = Some(checked);
                Ok(n)
            }
            Step::LibBuild => {
                self.lib_build()?;
                Ok(0)
            }
            Step::EmitInterface => {
                // A package directory exports under its manifest name by
                // default.
                let package = o
                    .slot(PACKAGE)
                    .map(str::to_owned)
                    .or_else(|| self.loaded.as_ref().map(|p| p.package.clone()))
                    .ok_or("interface export requires --package @org/name or std/name")?;
                let interface = vesper::interface::export(&self.checked()?.world, &package)?;
                let bytes = serde_json::to_vec_pretty(&interface).map_err(|e| e.to_string())?;
                if let Some(path) = o.output() {
                    crate::package_commands::atomic_write(&path, &bytes)?;
                } else {
                    crate::write(None, &bytes)?;
                }
                Ok(0)
            }
            Step::Optimize => {
                let expression_types = &self
                    .checked
                    .as_ref()
                    .ok_or("driver: no check yet")?
                    .expression_types;
                let parsed = self.parsed.as_mut().ok_or("driver: no parse yet")?;
                optimize::optimize(&mut parsed.module, expression_types);
                let checked = check::check(&parsed.module);
                if !checked.diagnostics.is_empty() {
                    return Err(format!(
                        "optimizer validation failed: {:?}",
                        checked.diagnostics
                    ));
                }
                self.checked = Some(checked);
                Ok(0)
            }
            Step::Run => {
                let world = &self.checked()?.world;
                let entry = o.entry();
                let f = world.functions.get(entry).ok_or("missing entry function")?;
                if !f.params.is_empty() {
                    return Err("entry function must take no arguments".into());
                }
                if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
                    return Err("entry must handle all effects".into());
                }
                let mut runtime = Runtime::new(world);
                runtime.fuel = o.fuel;
                runtime.optimize = o.optimize;
                println!("{}", runtime.run(entry, vec![])?);
                Ok(0)
            }
            Step::Jit => {
                println!(
                    "{}",
                    native::jit_with_options(&self.checked()?.world, o.entry(), o.optimize)?
                );
                Ok(0)
            }
            Step::VerifiedBuild => {
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "verified code generator")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.out"));
                println!(
                    "{}",
                    vesper::verified_codegen::build_command(
                        world,
                        o.entry(),
                        o.target(),
                        o.optimize,
                        o.linker(),
                        &path,
                    )?
                );
                Ok(0)
            }
            Step::WasmBuild => {
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "WebAssembly backend")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let options = vesper::wasm_backend::WasmOptions {
                    optimize: o.optimize,
                    ..Default::default()
                };
                let artifact = vesper::wasm_backend::compile(world, o.entry(), &options)
                    .map_err(|e| e.to_string())?;
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.wasm"));
                crate::package_commands::atomic_write(&path, &artifact.component)?;
                println!(
                    "built {} ({} component)",
                    path.display(),
                    vesper::wasm_backend::TARGET
                );
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
            Step::NativeObject => {
                let object = PathBuf::from(utf8(arg)?);
                let (bytes, _) = self.native_code()?;
                fs::write(&object, bytes).map_err(|e| e.to_string())?;
                Ok(0)
            }
            Step::IsWindowsTarget => Ok(i64::from(native::windows::is_windows_target(o.target()))),
            Step::WindowsLink => {
                let object = PathBuf::from(utf8(arg)?);
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.out"));
                let linker = o.linker();
                let explicit = (linker != "cc").then(|| PathBuf::from(linker));
                let path = native::windows::link_executable(&object, &path, explicit.as_deref())?;
                self.text = path.display().to_string().into_bytes();
                Ok(0)
            }
            Step::Tee => {
                crate::measure_tee(Path::new(utf8(arg)?))?;
                Ok(0)
            }
            Step::EmitHir => {
                crate::write(
                    o.output(),
                    serde_json::to_string_pretty(&self.parsed()?.module)
                        .map_err(|e| e.to_string())?
                        .as_bytes(),
                )?;
                Ok(0)
            }
            Step::EmitIr => {
                let checked = self.checked()?;
                let mut program = ir::lower(checked);
                if o.optimize {
                    vesper::ir_opt::optimize(&checked.world, &mut program)?;
                }
                crate::write(
                    o.output(),
                    serde_json::to_string_pretty(&program)
                        .map_err(|e| e.to_string())?
                        .as_bytes(),
                )?;
                Ok(0)
            }
            Step::EmitLean => {
                crate::write(
                    o.output(),
                    lean::extract(&self.checked()?.world)?.as_bytes(),
                )?;
                Ok(0)
            }
            Step::EmitObject => {
                let path = o.output().ok_or("object output requires -o <path>")?;
                let (bytes, _) = self.native_code()?;
                fs::write(path, bytes).map_err(|e| e.to_string())?;
                Ok(0)
            }
            Step::EmitClif => {
                let (_, clif) = self.native_code()?;
                crate::write(o.output(), clif.as_bytes())?;
                Ok(0)
            }
            Step::IoError => {
                let word =
                    u64::from_le_bytes(arg.try_into().map_err(|_| "driver: bad error word")?);
                self.text = io_error(word).to_string().into_bytes();
                Ok(0)
            }
        }
    }

    /// `build --lib` (and any library option): the export path, as in
    /// `cli()`.
    fn lib_build(&mut self) -> Result<(), String> {
        let o = &self.o;
        let parsed = self.parsed.take().ok_or("driver: no parse yet")?;
        let loaded = &self.loaded;
        let package_vsys = loaded.as_ref().map_or(&[][..], |p| &p.vsys[..]);
        let lib = o.lib_flags()?;
        // Exports are the root module's `pub` functions. Items of package
        // submodules and dependencies are renamed and stay internal.
        let public = parsed
            .module
            .public_items
            .iter()
            .filter(|n| {
                loaded
                    .as_ref()
                    .is_none_or(|p| !p.sources.qualified.contains_key(*n))
            })
            .cloned()
            .collect();
        let directives_source = match loaded {
            Some(p) => crate::export_directives_source(p)?,
            None => self.source.clone(),
        };
        let foreign = loaded.is_some() && lib.needs_module(&self.source, !package_vsys.is_empty());
        let module = foreign.then(|| parsed.module.clone());
        let built = lib.build(
            o.command,
            parsed.module,
            &public,
            o.input(),
            &directives_source,
            package_vsys,
            o.output(),
            o.target(),
            o.optimize,
            o.json,
            o.exclusive(),
        );
        // A package that imports `.vsys` libraries is checked by the
        // export path; on failure, check it again through the Bridge
        // lowering to report each diagnostic at its own file and line.
        if built.is_err()
            && let (Some(p), Some(module)) = (loaded, module)
            && let Some(ds) =
                crate::foreign_package_diagnostics(p, &module, &directives_source, o.target())
        {
            crate::package_diagnostics(p, &ds, o.json);
            return Err(format!("{} semantic error(s)", ds.len()));
        }
        built
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
            Some(o) => Box::into_raw(Box::new(Session {
                o,
                package: false,
                loaded: None,
                source: String::new(),
                parsed: None,
                checked: None,
                text: vec![],
            })),
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
