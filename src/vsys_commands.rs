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
    let object_target = match c.command {
        "build" => c.target,
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
            let symbols = host_symbols(&imports, "jit", c.link_search)?;
            let symbols: Vec<(&str, *const u8)> =
                symbols.iter().map(|(n, a)| (n.as_str(), *a)).collect();
            let value = checked
                .jit_with_options(c.entry, &symbols, c.optimize)
                .map_err(|e| e.to_string())?;
            println!("{value}");
            Ok(())
        }
        "build" => build(&c, &checked),
        "run" => {
            // The evaluator reaches the same host functions as `jit`.
            let host = eval_host_in(&imports, "run", c.link_search)?;
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

/// Bind the CLI's foreign functions to evaluator calls.
pub(crate) fn eval_host(
    imports: &Imports,
    command: &str,
) -> Result<ffi_lowering::EvalHost, String> {
    eval_host_in(imports, command, &[])
}

/// [`eval_host`], loading libraries from `search` first.
fn eval_host_in(
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
        if library.link().name() != STD_HOST {
            let lib = host_libraries::open(library.link().name(), search).map_err(|why| {
                format!(
                    "{command}: library {} links '{}', which could not be loaded ({why}); link it into an executable with `vesper build`",
                    library.name(),
                    library.link().name()
                )
            })?;
            for f in library.functions() {
                let address = lib.symbol(f.symbol()).ok_or_else(|| {
                    format!(
                        "{command}: {}::{} is not a symbol of {}",
                        library.name(),
                        f.symbol(),
                        lib.shown()
                    )
                })?;
                out.push((f.symbol().to_owned(), address));
            }
            continue;
        }
        for f in library.functions() {
            let (name, address) = std.iter().find(|(n, _)| *n == f.symbol()).ok_or_else(|| {
                format!(
                    "{command}: {}::{} is not a function of {STD_HOST}",
                    library.name(),
                    f.symbol()
                )
            })?;
            out.push(((*name).to_owned(), *address));
        }
    }
    Ok(out)
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
        use std::{ffi::CStr, ffi::c_void, path::Path};

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LoadLibraryA(name: *const i8) -> *mut c_void;
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
            let c = std::ffi::CString::new(path.to_string_lossy().as_bytes())
                .map_err(|_| "a path with a NUL byte".to_owned())?;
            // SAFETY: `c` is a terminated path; the module is never freed.
            let h = unsafe { LoadLibraryA(c.as_ptr().cast()) };
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

fn build(c: &Command<'_>, checked: &ffi_lowering::ForeignChecked) -> Result<(), String> {
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
    let names = checked.link_names();
    if vesper::native::windows::is_windows_target(c.target) {
        // Windows (runtime glue gap G11): link.exe or lld-link, as for any
        // Windows executable, with each library's import or static `.lib`.
        let result = link_windows(c, &object, &path, &names);
        let _ = fs::remove_file(&object);
        let path = result?;
        println!("built {}", path.display());
        return Ok(());
    }
    let mut link = process::Command::new(c.linker);
    link.arg(&object).arg("-o").arg(&path);
    for dir in c.link_search {
        link.arg("-L").arg(dir);
    }
    for (name, mode) in &names {
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
    let _ = fs::remove_file(&object);
    if !status.success() {
        return Err("native linker failed".into());
    }
    println!("built {}", path.display());
    Ok(())
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
    output: &std::path::Path,
    names: &[(String, vesper_ffi::sys::LinkMode)],
) -> Result<PathBuf, String> {
    let mut inputs = vec![object.to_path_buf()];
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
