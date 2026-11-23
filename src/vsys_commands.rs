//! `check`, `jit`, `run` and `build` of a package graph whose packages carry
//! `.vsys` libraries in their manifests (`[package] vsys`, manuscript §16.3;
//! self-hosting SH1 lane 7, #1150). Such a program checks only through the
//! Bridge lowering (`vesper::ffi_lowering`), which resolves its `Lib::f`
//! calls and `Ffi[Lib]` rows against the imports.
//!
//! * `check` reports the Bridge lowering's diagnostics at their files.
//! * `jit`, `run` and `test` bind each foreign symbol to a host function
//!   (`run` and `test` through the evaluator's host, `ffi_lowering::EvalHost`):
//!   the standard I/O libraries (`link name = "vesper_std_host"`, the shim in
//!   crates/vesper-std-host) from the CLI itself, and every other library
//!   from the shared library its link name names (runtime glue gap G5,
//!   [`host_libraries`]). A library that cannot be loaded, or that lacks a
//!   symbol, is refused before any code runs.
//! * `build` emits the executable's object with the foreign calls lowered
//!   and links it with each library the object calls (`-l<name>`, found on
//!   the `--link-search` directories).
use std::{fs, path::PathBuf, process};
use vesper::ffi_lowering::{self, Imports};
use vesper::packages::Assembled;

/// One CLI invocation.
pub struct Command<'a> {
    pub command: &'a str,
    pub format: &'a str,
    pub package: &'a Assembled,
    pub module: &'a vesper::ast::Module,
    pub entry: &'a str,
    pub output: Option<PathBuf>,
    pub target: Option<&'a str>,
    pub optimize: bool,
    pub linker: &'a str,
    pub link_search: &'a [PathBuf],
    pub json: bool,
    /// `--verify`, `--verify-policy`, `--tee` or `--codegen` was given.
    pub exclusive: bool,
}

/// The `.vsys` link name of the standard I/O host the CLI links in.
pub const STD_HOST: &str = "vesper_std_host";

pub fn run(c: Command<'_>) -> Result<(), String> {
    if c.exclusive {
        return Err(
            "--verify, --verify-policy, --tee and --codegen do not apply to packages with .vsys imports yet"
                .into(),
        );
    }
    // A wasm component reaches the standard I/O libraries through WASI
    // (the wasm backend's port, packages/vesper-wasm-backend/src/host.ves),
    // so its foreign calls are checked against the host's `.vsys` view.
    let wasm = c.command == "build" && c.target == Some(vesper::wasm_backend::TARGET);
    let object_target = match c.command {
        "build" if !wasm => c.target,
        _ => None,
    };
    let imports = c.package.imports(object_target)?;
    let checked = match ffi_lowering::check_module(c.module, &imports) {
        Ok(checked) => checked,
        Err(error) => {
            let mut ds = error.diagnostics.clone();
            if let Some(span) = &error.span {
                ds.push(vesper::ast::Diagnostic::new(
                    "bridge",
                    format!("{:?}: {}", error.kind, error.message),
                    span.clone(),
                ));
            }
            if ds.is_empty() {
                return Err(error.to_string());
            }
            let localized: Vec<_> = ds.iter().map(|d| c.package.sources.localize(d)).collect();
            crate::package_diagnostics(c.package, &localized, c.json);
            return Err(format!("{} semantic error(s)", localized.len()));
        }
    };
    match c.command {
        "check" => {
            if c.json {
                println!("[]");
            } else {
                println!("checked {}", c.package.package);
            }
            Ok(())
        }
        "jit" => {
            let symbols = host_symbols(
                &imports.for_host_calls(checked.calls()),
                "jit",
                c.link_search,
            )?;
            let symbols: Vec<(&str, *const u8)> =
                symbols.iter().map(|(n, a)| (n.as_str(), *a)).collect();
            let value = checked
                .jit_with_options(c.entry, &symbols, c.optimize)
                .map_err(|e| e.to_string())?;
            println!("{value}");
            Ok(())
        }
        "build" if wasm => build_wasm(&c, &checked),
        "build" => build(&c, &checked, &imports),
        "run" => {
            // The evaluator reaches the same host functions as `jit`.
            let host = eval_host_in(
                &imports.for_host_calls(checked.calls()),
                "run",
                c.link_search,
            )?;
            let value = checked
                .evaluate(c.entry, |call| host.call(call))
                .map_err(|e| e.to_string())?;
            println!("{value}");
            Ok(())
        }
        _ => Err(format!(
            "{} {}: not supported for packages with .vsys imports yet (check, jit, run and build are)",
            c.command, c.format
        )),
    }
}

/// Bind the CLI's foreign functions to evaluator calls, loading libraries
/// from `search` first.
pub(crate) fn eval_host_in(
    imports: &Imports,
    command: &str,
    search: &[PathBuf],
) -> Result<ffi_lowering::EvalHost, String> {
    let symbols = host_symbols(imports, command, search)?;
    let symbols: Vec<(&str, *const u8)> = symbols.iter().map(|(n, a)| (n.as_str(), *a)).collect();
    ffi_lowering::EvalHost::new(imports, &symbols)
}

/// The host functions for `imports`: every function of a library linked as
/// [`STD_HOST`] from the CLI itself, and every function of another library
/// from the shared library its link name names ([`host_libraries`]), looked
/// for in `search` first.
fn host_symbols(
    imports: &Imports,
    command: &str,
    search: &[PathBuf],
) -> Result<Vec<(String, *const u8)>, String> {
    let mut out = vec![];
    let std = ffi_lowering::std_host_symbols();
    for (library, _) in imports.libraries() {
        // `@vesper-std/cpu`'s instructions are native code the backend
        // defines; nothing hosts them.
        if library.link().name() == ffi_lowering::cpu::LINK_NAME {
            continue;
        }
        // Assembly objects are linked by `vesper build` only; the JIT and
        // the evaluator have no loader for them (manuscript §16.3).
        if library.link().mode() == vesper_ffi::sys::LinkMode::Static
            && let Some(package) = imports.objects_provider(library.link().name())
        {
            return Err(format!(
                "{command}: library {} is provided by the objects of {package} (`[package] objects`), which only `vesper build` links; build an executable instead",
                library.name()
            ));
        }
        if library.link().name() != STD_HOST {
            let lib = host_libraries::open(library.link().name(), search).map_err(|why| {
                format!(
                    "{command}: library {} links '{}', which could not be loaded ({why}); link it into an executable with `vesper build`",
                    library.name(),
                    library.link().name()
                )
            })?;
            // A function-pointer type has no symbol; `#[link_name]` gives
            // the C name a function binds to.
            for f in library.functions().iter().filter(|f| !f.is_indirect()) {
                let address = lib.symbol(f.link_symbol()).ok_or_else(|| {
                    format!(
                        "{command}: {}::{} is not a symbol of {}",
                        library.name(),
                        f.link_symbol(),
                        lib.shown()
                    )
                })?;
                bind(&mut out, f.link_symbol(), address, command)?;
            }
            continue;
        }
        for f in library.functions().iter().filter(|f| !f.is_indirect()) {
            let (name, address) =
                std.iter()
                    .find(|(n, _)| *n == f.link_symbol())
                    .ok_or_else(|| {
                        format!(
                            "{command}: {}::{} is not a function of {STD_HOST}",
                            library.name(),
                            f.symbol()
                        )
                    })?;
            bind(&mut out, name, *address, command)?;
        }
    }
    Ok(out)
}

/// `build --target wasm32-wasip2`: a component whose `StdEnv` and `StdFs`
/// calls import WASI 0.2. Only the wasm backend's Vesper port lowers them
/// (through `vesper-hybrid`); the Rust backend refuses the program.
fn build_wasm(c: &Command<'_>, checked: &ffi_lowering::ForeignChecked) -> Result<(), String> {
    let options = vesper::wasm_backend::WasmOptions {
        optimize: c.optimize,
        ..Default::default()
    };
    let artifact = vesper::wasm_backend::compile(checked.world(), c.entry, &options)
        .map_err(|e| e.to_string())?;
    let path = c.output.clone().unwrap_or_else(|| PathBuf::from("a.wasm"));
    vesper::atomic_file::atomic_write(&path, &artifact.component)?;
    println!(
        "built {} ({} component)",
        path.display(),
        vesper::wasm_backend::TARGET
    );
    Ok(())
}

/// Add `symbol` at `address` to the host table. The JIT and the evaluator
/// bind by symbol name, as a C linker does, so two libraries that resolve
/// one name to different addresses are refused rather than letting one
/// library's calls reach the other's function.
fn bind(
    out: &mut Vec<(String, *const u8)>,
    symbol: &str,
    address: *const u8,
    command: &str,
) -> Result<(), String> {
    match out.iter().find(|(n, _)| n == symbol) {
        Some((_, a)) if *a == address => Ok(()),
        Some(_) => Err(format!(
            "{command}: symbol '{symbol}' resolves to different functions in two imported libraries"
        )),
        None => {
            out.push((symbol.to_owned(), address));
            Ok(())
        }
    }
}

/// The shared libraries `jit`, `run` and `test` bind foreign symbols from
/// (runtime glue gap G5): the C library and the libraries the CLI already
/// links are searched in the process (`dlsym(RTLD_DEFAULT, ..)`, or the C
/// runtime DLLs on Windows); any other link name is loaded once, from the
/// `--link-search` directories first, then the system's search path. A
/// loaded library stays loaded for the life of the process.
mod host_libraries {
    use std::path::{Path, PathBuf};

    /// A loaded library, or the process's own symbols.
    pub struct Library {
        handles: Vec<*mut std::ffi::c_void>,
        shown: String,
    }

    impl Library {
        pub fn shown(&self) -> &str {
            &self.shown
        }

        /// The address of `symbol`, from the first handle that has it.
        pub fn symbol(&self, symbol: &str) -> Option<*const u8> {
            let name = std::ffi::CString::new(symbol).ok()?;
            self.handles
                .iter()
                .map(|h| sys::symbol(*h, &name))
                .find(|p| !p.is_null())
                .map(|p| p as *const u8)
        }
    }

    /// The file names a library of link name `name` has on this system.
    fn file_names(name: &str) -> Vec<String> {
        if cfg!(windows) {
            vec![format!("{name}.dll"), format!("lib{name}.dll")]
        } else if cfg!(target_os = "macos") {
            vec![format!("lib{name}.dylib")]
        } else {
            vec![format!("lib{name}.so")]
        }
    }

    /// Open the library of link name `name`.
    pub fn open(name: &str, search: &[PathBuf]) -> Result<Library, String> {
        if let Some(handles) = sys::process_library(name) {
            return Ok(Library {
                handles,
                shown: format!("the process (link name '{name}')"),
            });
        }
        let mut tried = vec![];
        let candidates: Vec<PathBuf> = search
            .iter()
            .flat_map(|d| file_names(name).into_iter().map(move |f| d.join(f)))
            .filter(|p| p.is_file())
            .chain(file_names(name).into_iter().map(PathBuf::from))
            .collect();
        for path in candidates {
            match sys::load(&path) {
                Ok(h) => {
                    return Ok(Library {
                        handles: vec![h],
                        shown: path.display().to_string(),
                    });
                }
                Err(e) => tried.push(format!("{}: {e}", Path::new(&path).display())),
            }
        }
        Err(tried.join("; "))
    }

    #[cfg(unix)]
    mod sys {
        use std::{ffi::CStr, ffi::c_void, os::unix::ffi::OsStrExt, path::Path};

        /// The process's own symbols for the libraries the CLI is linked
        /// with: the C library, and on Linux `m`, `pthread` and `dl`.
        pub fn process_library(name: &str) -> Option<Vec<*mut c_void>> {
            let linked: &[&str] = if cfg!(target_os = "macos") {
                &["c", "m", "System", "pthread", "dl"]
            } else {
                &["c", "m", "pthread", "dl"]
            };
            linked.contains(&name).then(|| vec![libc::RTLD_DEFAULT])
        }

        pub fn load(path: &Path) -> Result<*mut c_void, String> {
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| "a path with a NUL byte".to_owned())?;
            // SAFETY: `c` is a terminated path; the handle is never closed.
            let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if h.is_null() {
                // SAFETY: dlerror returns the thread's last error or null.
                let e = unsafe { libc::dlerror() };
                return Err(if e.is_null() {
                    "dlopen failed".into()
                } else {
                    // SAFETY: dlerror's text is terminated.
                    unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
                });
            }
            Ok(h)
        }

        pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
            // SAFETY: `handle` is RTLD_DEFAULT or a live dlopen handle.
            unsafe { libc::dlsym(handle, name.as_ptr()) }
        }
    }

    #[cfg(windows)]
    mod sys {
        use std::{ffi::CStr, ffi::c_void, os::windows::ffi::OsStrExt, path::Path};

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LoadLibraryW(name: *const u16) -> *mut c_void;
            fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
            fn GetLastError() -> u32;
        }

        /// The C library is the universal C runtime, with the older
        /// `msvcrt.dll` after it.
        pub fn process_library(name: &str) -> Option<Vec<*mut c_void>> {
            if name != "c" && name != "m" {
                return None;
            }
            let handles: Vec<_> = ["ucrtbase.dll", "msvcrt.dll"]
                .iter()
                .filter_map(|n| load(Path::new(n)).ok())
                .collect();
            (!handles.is_empty()).then_some(handles)
        }

        pub fn load(path: &Path) -> Result<*mut c_void, String> {
            // A wide path, so a directory outside the ANSI code page loads.
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            if wide[..wide.len() - 1].contains(&0) {
                return Err("a path with a NUL character".into());
            }
            // SAFETY: `wide` is a terminated UTF-16 path; the module is
            // never freed.
            let h = unsafe { LoadLibraryW(wide.as_ptr()) };
            if h.is_null() {
                // SAFETY: reads the thread's last error code.
                return Err(format!("LoadLibrary error {}", unsafe { GetLastError() }));
            }
            Ok(h)
        }

        pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
            // SAFETY: `handle` is a live module handle.
            unsafe { GetProcAddress(handle, name.as_ptr().cast()) }
        }
    }
}

fn build(
    c: &Command<'_>,
    checked: &ffi_lowering::ForeignChecked,
    imports: &Imports,
) -> Result<(), String> {
    if c.entry != "main" {
        return Err("build currently requires entry 'main'".into());
    }
    let f = checked
        .world()
        .functions
        .get("main")
        .ok_or("missing main function")?;
    if !f.params.is_empty()
        || !(vesper::numeric::is_integer_name(f.result.name())
            || matches!(f.result.name(), "Unit" | ""))
    {
        return Err("executable main must take no arguments and return an integer or Unit".into());
    }
    let path = c.output.clone().unwrap_or_else(|| PathBuf::from("a.out"));
    let object = path.with_extension("vesper.o");
    // `native_code` runs the verifiers that capability effects
    // (`ConstTime`, `Realtime`, `DetFloat`) imply, as for any executable.
    let (bytes, _) = checked
        .with_lowering(c.target, |world| {
            crate::native_code(
                world,
                "main",
                c.target,
                c.optimize,
                &Default::default(),
                None,
                (&c.package.package, ""),
            )
        })
        .map_err(|e| e.to_string())?;
    fs::write(&object, bytes).map_err(|e| e.to_string())?;
    // A package's objects provide its static libraries
    // (manuscript §16.3): they link in place of `lib<name>.a`.
    // A package's objects link only when the executable calls one of the
    // static libraries they provide.
    let mut names = checked.link_names();
    let mut used = std::collections::BTreeSet::new();
    names.retain(
        |(name, mode)| match (*mode, imports.objects_provider(name)) {
            (vesper_ffi::sys::LinkMode::Static, Some(package)) => {
                used.insert(package.to_owned());
                false
            }
            _ => true,
        },
    );
    let mut temporary = vec![object.clone()];
    let objects = package_objects(c, &path, &used, &mut temporary);
    let result = objects.and_then(|objects| link_executable(c, &object, &objects, &path, &names));
    for file in temporary {
        let _ = fs::remove_file(file);
    }
    println!("built {}", result?.display());
    Ok(())
}

/// The `[package] objects` of each package in `used` for the build target:
/// a prebuilt object as it is, and a C or assembly source compiled beside
/// `output` (each such file is added to `temporary`).
fn package_objects(
    c: &Command<'_>,
    output: &std::path::Path,
    used: &std::collections::BTreeSet<String>,
    temporary: &mut Vec<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let windows = vesper::native::windows::is_windows_target(c.target);
    let mut out = vec![];
    for (i, o) in c.package.objects.iter().enumerate() {
        if !used.contains(&o.package) {
            continue;
        }
        let Some(file) = o.file_for(c.target)? else {
            continue;
        };
        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if matches!(extension, "o" | "obj") {
            out.push(file.to_path_buf());
            continue;
        }
        let assembled = output.with_extension(format!(
            "vesper-asm{i}.{}",
            if windows { "obj" } else { "o" }
        ));
        temporary.push(assembled.clone());
        if windows {
            vesper::native::windows::assemble(file, &assembled)
                .map_err(|e| format!("{}: {e}", o.display))?;
        } else if extension == "asm" {
            return Err(format!(
                "{}: MASM source (.asm) assembles only for Windows targets; use a per-target path for the others",
                o.display
            ));
        } else {
            // The C compiler driver that links also compiles `.c` and
            // assembles (running the C preprocessor over `.S`).
            let mut command = process::Command::new(c.linker);
            if extension == "c" {
                command.arg("-O2");
            }
            let result = command
                .arg("-c")
                .arg(file)
                .arg("-o")
                .arg(&assembled)
                .output()
                .map_err(|e| format!("could not invoke assembler '{}': {e}", c.linker))?;
            if !result.status.success() {
                return Err(format!(
                    "{}: assembler '{}' failed:\n{}",
                    o.display,
                    c.linker,
                    String::from_utf8_lossy(&result.stderr).trim_end()
                ));
            }
        }
        out.push(assembled);
    }
    Ok(out)
}

/// Link `object`, the package `objects` and the libraries `names` into
/// `path`.
fn link_executable(
    c: &Command<'_>,
    object: &std::path::Path,
    objects: &[PathBuf],
    path: &std::path::Path,
    names: &[(String, vesper_ffi::sys::LinkMode)],
) -> Result<PathBuf, String> {
    if vesper::native::windows::is_windows_target(c.target) {
        // Windows (runtime glue gap G11): link.exe or lld-link, as for any
        // Windows executable, with each library's import or static `.lib`.
        return link_windows(c, object, objects, path, names);
    }
    let mut link = process::Command::new(c.linker);
    link.arg(object).args(objects).arg("-o").arg(path);
    for dir in c.link_search {
        link.arg("-L").arg(dir);
    }
    for (name, mode) in names {
        match mode {
            vesper_ffi::sys::LinkMode::Dynamic => {
                link.arg(format!("-l{name}"));
            }
            // A static library links its archive even when a shared one sits
            // beside it: the archive's path where a search directory has it,
            // else `-Bstatic` (GNU and LLVM linkers).
            vesper_ffi::sys::LinkMode::Static => {
                match c
                    .link_search
                    .iter()
                    .map(|d| d.join(format!("lib{name}.a")))
                    .find(|p| p.is_file())
                {
                    Some(archive) => {
                        link.arg(archive);
                    }
                    None if cfg!(target_os = "macos") => {
                        link.arg(format!("-l{name}"));
                    }
                    None => {
                        link.args(["-Wl,-Bstatic", &format!("-l{name}"), "-Wl,-Bdynamic"]);
                    }
                }
            }
        }
    }
    // A Rust staticlib (such as the standard I/O host) needs the platform's
    // threading and dynamic-loading libraries.
    if !names.is_empty() && !cfg!(target_os = "macos") {
        link.args(["-lpthread", "-ldl"]);
    }
    link.arg("-lm");
    let status = link
        .status()
        .map_err(|e| format!("could not invoke native linker '{}': {e}", c.linker))?;
    if !status.success() {
        return Err("native linker failed".into());
    }
    Ok(path.to_path_buf())
}

/// The libraries a Rust static library (such as the standard I/O host)
/// needs on Windows, beside the C runtime the linker always adds.
const WINDOWS_RUST_SYSTEM_LIBRARIES: [&str; 5] = [
    "advapi32.lib",
    "bcrypt.lib",
    "ntdll.lib",
    "userenv.lib",
    "ws2_32.lib",
];

/// Link a Windows executable from `object` and the `.lib` of every link name
/// in `names` (runtime glue gap G11). The C library (`c`, `m`) is the static
/// MSVC runtime every Windows executable already links. Another library is
/// `<name>.lib` (an import library for a DLL, or a static archive) from the
/// first `--link-search` directory that has it, else from the linker's
/// `LIB` path.
fn link_windows(
    c: &Command<'_>,
    object: &std::path::Path,
    objects: &[PathBuf],
    output: &std::path::Path,
    names: &[(String, vesper_ffi::sys::LinkMode)],
) -> Result<PathBuf, String> {
    let mut inputs = vec![object.to_path_buf()];
    inputs.extend_from_slice(objects);
    let mut rust = false;
    for (name, _) in names {
        if matches!(name.as_str(), "c" | "m") {
            continue;
        }
        rust |= name == STD_HOST;
        let file = format!("{name}.lib");
        inputs.push(
            c.link_search
                .iter()
                .map(|d| d.join(&file))
                .find(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from(&file)),
        );
    }
    if rust {
        inputs.extend(WINDOWS_RUST_SYSTEM_LIBRARIES.iter().map(PathBuf::from));
    }
    // As for other Windows builds, the default `cc` means "find the MSVC
    // linker".
    let explicit = (c.linker != "cc").then(|| PathBuf::from(c.linker));
    vesper::native::windows::link(&inputs, output, explicit.as_deref()).map_err(|e| e.to_string())
}
