//! `check`, `jit`, `run` and `build` of a package graph whose packages carry
//! `.vsys` libraries in their manifests (`[package] vsys`, manuscript §16.3;
//! self-hosting SH1 lane 7, #1150). Such a program checks only through the
//! Bridge lowering (`vesper::ffi_lowering`), which resolves its `Lib::f`
//! calls and `Ffi[Lib]` rows against the imports.
//!
//! * `check` reports the Bridge lowering's diagnostics at their files.
//! * `jit` and `run` bind each foreign symbol to a host function the CLI
//!   links in (`run` through the evaluator's host, `ffi_lowering::EvalHost`):
//!   the standard I/O libraries (`link name = "vesper_std_host"`, the shim in
//!   crates/vesper-std-host) and, on Unix, the C library itself
//!   (`link = "dynamic", name = "c"` or `"m"`, looked up in the process).
//!   Other libraries are refused before any code runs; link them into an
//!   executable with `build`.
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
            let symbols = host_symbols(&imports, "jit")?;
            let symbols: Vec<(&str, *const u8)> = symbols.iter().map(|(n, a)| (*n, *a)).collect();
            let value = checked
                .jit_with_options(c.entry, &symbols, c.optimize)
                .map_err(|e| e.to_string())?;
            println!("{value}");
            Ok(())
        }
        "build" if wasm => build_wasm(&c, &checked),
        "build" => build(&c, &checked),
        "run" => {
            // The evaluator reaches the same host functions as `jit`.
            let host = eval_host(&imports, "run")?;
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

/// Link names of the C libraries every Unix `vesper` process has loaded:
/// `link = "dynamic", name = "c"` (or `"m"`) imports libc itself, so
/// `jit`, `run` and `test` bind such a library's functions from the
/// process, the way the linker binds them in a `vesper build` executable.
pub const PROCESS_LIBRARIES: &[&str] = &["c", "m"];

/// Whether `library` imports the C library the process runs on.
fn is_process_library(library: &vesper_ffi::sys::LibraryDescriptor) -> bool {
    cfg!(unix)
        && library.link().mode() == vesper_ffi::sys::LinkMode::Dynamic
        && PROCESS_LIBRARIES.contains(&library.link().name())
}

/// The address of C library function `symbol` in this process.
#[cfg(unix)]
fn process_symbol(symbol: &str) -> Option<*const u8> {
    let name = std::ffi::CString::new(symbol).ok()?;
    // SAFETY: `name` is NUL-terminated and outlives the call; `dlsym` with
    // `RTLD_DEFAULT` only searches the loaded objects.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!address.is_null()).then_some(address as *const u8)
}

#[cfg(not(unix))]
fn process_symbol(_symbol: &str) -> Option<*const u8> {
    None
}

/// Bind the CLI's built-in foreign functions to evaluator calls.
pub(crate) fn eval_host(
    imports: &Imports,
    command: &str,
) -> Result<ffi_lowering::EvalHost, String> {
    let symbols = host_symbols(imports, command)?;
    ffi_lowering::EvalHost::new(imports, &symbols)
}

/// The host functions the CLI provides for `imports`: every function of a
/// library linked as [`STD_HOST`], and on Unix every function of the C
/// library itself ([`PROCESS_LIBRARIES`]), which the `vesper` process
/// already links.
fn host_symbols(
    imports: &Imports,
    command: &str,
) -> Result<Vec<(&'static str, *const u8)>, String> {
    let mut out = vec![];
    let std = ffi_lowering::std_host_symbols();
    for (library, _) in imports.libraries() {
        // `@vesper-std/cpu`'s instructions are native code the backend
        // defines; nothing hosts them.
        if library.link().name() == ffi_lowering::cpu::LINK_NAME {
            continue;
        }
        if is_process_library(library) {
            for f in library.functions().iter().filter(|f| !f.is_indirect()) {
                let symbol = f.link_symbol();
                let address = process_symbol(symbol).ok_or_else(|| {
                    format!(
                        "{command}: {}::{} links '{symbol}', which the C library this vesper runs on does not define",
                        library.name(),
                        f.symbol()
                    )
                })?;
                // The JIT keeps symbol names for the whole run; a CLI
                // invocation binds each one once.
                let name: &'static str = Box::leak(symbol.to_owned().into_boxed_str());
                out.push((name, address));
            }
            continue;
        }
        if library.link().name() != STD_HOST {
            return Err(format!(
                "{command}: library {} links '{}', which this vesper does not host; link it into an executable with `vesper build`",
                library.name(),
                library.link().name()
            ));
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
            out.push((*name, *address));
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

fn build(c: &Command<'_>, checked: &ffi_lowering::ForeignChecked) -> Result<(), String> {
    if c.entry != "main" {
        return Err("build currently requires entry 'main'".into());
    }
    if vesper::native::windows::is_windows_target(c.target) {
        return Err(
            "build: executables with .vsys imports link with cc only; Windows targets are not supported yet"
                .into(),
        );
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
    let mut link = process::Command::new(c.linker);
    link.arg(&object).arg("-o").arg(&path);
    for dir in c.link_search {
        link.arg("-L").arg(dir);
    }
    let names = checked.link_names();
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
