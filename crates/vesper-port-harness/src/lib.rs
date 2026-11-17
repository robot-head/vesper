//! Build-script helper for Rust crates with a Vesper port (port wave P0
//! lane 6, #878; build modes and the seed-copy rule from port wave P1
//! lane 2, #899; recipe in `docs/maintenance/porting-a-crate.md`).
//!
//! A crate's `build.rs` calls [`Port::build`]. It gets a C-ABI object for
//! the crate's Vesper reimplementation, archives it into a static library,
//! tells Cargo to link it, sets `cfg(vesper_port)` on the crate, and checks
//! that the committed Rust binding is exactly what the compiler generates
//! for the current source. The crate's public functions then forward to
//! the binding and its unchanged tests run against the Vesper build.
//!
//! The port is a Loom package directory (`Loom.toml` plus `src/**/*.ves`,
//! with path dependencies) or a single `.ves` file. This crate does not
//! link the `vesper` library, so a crate the compiler itself depends on can
//! use it without a Cargo dependency cycle. The object comes from, in
//! order:
//!
//! 1. [`OBJECT_ENV`]: a prebuilt object (Bazel builds it with
//!    `vesper_port_object` and passes it, with the generated binding in
//!    [`GENERATED_BINDING_ENV`], through `build_script_env`).
//! 2. [`COMPILER_ENV`]: a `vesper` executable, run as `vesper build --lib`.
//!    CI builds `vesper` once and passes it here.
//! 3. Otherwise the in-tree compiler, built by a nested
//!    `cargo build -p vesper --bin vesper` into
//!    `<target dir>/vesper-port-compiler`, so a plain
//!    `cargo test -p <crate>` works with nothing installed.
//!
//! In `build.rs` (under `#[cfg(feature = "vesper-port")]` for a crate whose
//! port is optional):
//!
//! ```no_run
//! vesper_port_harness::Port::new("pilot", "vesper/pilot.ves")
//!     .binding("port/binding.rs")
//!     .rust_types("crate")
//!     .build();
//! ```
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set to `1` to rewrite a stale committed binding instead of failing.
pub const BLESS_ENV: &str = "VESPER_PORT_BLESS";
/// Path of a `vesper` executable to compile ports with.
pub const COMPILER_ENV: &str = "VESPER_PORT_COMPILER";
/// Path of a prebuilt port object; no compiler runs.
pub const OBJECT_ENV: &str = "VESPER_PORT_OBJECT";
/// With [`OBJECT_ENV`]: the binding generated for that object, to check.
pub const GENERATED_BINDING_ENV: &str = "VESPER_PORT_GENERATED_BINDING";
/// Set on the nested compiler build. A port build script that sees it is
/// inside the compiler's own dependency closure, which the seed-copy rule
/// forbids, and fails instead of recursing.
pub const IN_COMPILER_BUILD_ENV: &str = "VESPER_PORT_IN_COMPILER_BUILD";
/// The cfg [`Port::build`] sets on the crate being built.
pub const CFG: &str = "vesper_port";

/// One Vesper library linked into the crate being built.
#[derive(Debug, Clone)]
pub struct Port {
    library: String,
    source: PathBuf,
    binding: Option<PathBuf>,
    types_path: Option<String>,
    edits: Vec<(String, String)>,
}

impl Port {
    /// `library` prefixes every symbol (`vesper_<library>_<fn>`); `source`
    /// is the Loom package directory or the `.ves` file, relative to the
    /// crate directory.
    pub fn new(library: &str, source: impl AsRef<Path>) -> Self {
        Self {
            library: library.into(),
            source: source.as_ref().into(),
            binding: None,
            types_path: None,
            edits: Vec::new(),
        }
    }

    /// The committed binding module to check (relative to the crate).
    pub fn binding(mut self, path: impl AsRef<Path>) -> Self {
        self.binding = Some(path.as_ref().into());
        self
    }

    /// Implement `Abi` for the crate's own types at `path` (for example
    /// `crate`) instead of declaring binding-local types.
    pub fn rust_types(mut self, path: &str) -> Self {
        self.types_path = Some(path.into());
        self
    }

    /// Replace `from`, which must occur exactly once in the source (for a
    /// package: once across the root package's `.ves` files), with `to`
    /// before compiling. Divergence checks use this to build a deliberately
    /// wrong port from the real source.
    pub fn edit(mut self, from: &str, to: &str) -> Self {
        self.edits.push((from.into(), to.into()));
        self
    }

    /// Build, archive and link the library, and check the binding. Panics
    /// with the compiler's message on any failure, as build scripts do.
    pub fn build(self) {
        if let Err(e) = self.try_build() {
            panic!("vesper-port: {e}");
        }
    }

    /// [`Port::build`] returning the error instead of panicking.
    pub fn try_build(&self) -> Result<(), String> {
        let dir = PathBuf::from(env("CARGO_MANIFEST_DIR")?);
        let out = PathBuf::from(env("OUT_DIR")?);
        let target = env("TARGET")?;
        for name in [
            BLESS_ENV,
            COMPILER_ENV,
            OBJECT_ENV,
            GENERATED_BINDING_ENV,
            IN_COMPILER_BUILD_ENV,
        ] {
            println!("cargo:rerun-if-env-changed={name}");
        }
        if std::env::var_os(IN_COMPILER_BUILD_ENV).is_some() {
            return Err(format!(
                "{} is port-backed but is being built inside the vesper compiler's own \
                 dependency closure. Crates inside `cargo tree -p vesper -e normal,build` must \
                 depend on the frozen seed package instead (see \"Crates the compiler links\" \
                 in docs/maintenance/porting-a-crate.md).",
                std::env::var("CARGO_PKG_NAME").unwrap_or_default()
            ));
        }
        println!("cargo:rustc-check-cfg=cfg({CFG})");
        println!("cargo:rustc-cfg={CFG}");
        let source = dir.join(&self.source);
        for input in inputs(&source)? {
            println!("cargo:rerun-if-changed={}", input.display());
        }
        let extension = if target.contains("windows") {
            "obj"
        } else {
            "o"
        };
        let object = out.join(format!("vesper_{}.{extension}", self.library));
        let generated = match std::env::var_os(OBJECT_ENV) {
            Some(prebuilt) => {
                if !self.edits.is_empty() {
                    return Err(format!(
                        "{OBJECT_ENV} names a prebuilt object, so source edits cannot apply; \
                         build the divergent object with `vesper_port_object(edits = ...)`"
                    ));
                }
                let prebuilt = PathBuf::from(prebuilt);
                println!("cargo:rerun-if-changed={}", prebuilt.display());
                std::fs::copy(&prebuilt, &object)
                    .map_err(|e| format!("{}: {e}", prebuilt.display()))?;
                let generated = std::env::var_os(GENERATED_BINDING_ENV).map(PathBuf::from);
                if let Some(path) = &generated {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
                generated
            }
            None => {
                let compiler = compiler()?;
                let input = prepare_source(&source, &out.join("edited"), &self.edits)?;
                let binding = out.join(format!("vesper_{}_binding.rs", self.library));
                let mut command = Command::new(&compiler);
                command
                    .args(["build", "--lib", "--lib-name", &self.library, "--target"])
                    .arg(&target)
                    .arg(&input)
                    .arg("-o")
                    .arg(&object)
                    .arg("--interface")
                    .arg(out.join(format!("vesper_{}.json", self.library)))
                    .arg("--rust-binding")
                    .arg(&binding);
                if let Some(types) = &self.types_path {
                    command.args(["--rust-types", types]);
                }
                run(&mut command, &compiler)?;
                Some(binding)
            }
        };
        if let Some(binding) = &self.binding {
            let path = dir.join(binding);
            println!("cargo:rerun-if-changed={}", path.display());
            match generated {
                Some(generated) => {
                    let expected = std::fs::read_to_string(&generated)
                        .map_err(|e| format!("{}: {e}", generated.display()))?;
                    check_binding(&path, &expected)?;
                }
                None => println!(
                    "cargo:warning={} was not checked: {OBJECT_ENV} is set without {GENERATED_BINDING_ENV}",
                    path.display()
                ),
            }
        }
        cc::Build::new()
            .object(&object)
            .cargo_metadata(true)
            .try_compile(&format!("vesper_{}", self.library))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Compare a committed binding with the generated text (line endings
/// normalized, so a CRLF checkout still matches). With [`BLESS_ENV`] set
/// to `1`, a stale file is rewritten instead.
pub fn check_binding(path: &Path, expected: &str) -> Result<(), String> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    let expected = expected.replace("\r\n", "\n");
    if current.replace("\r\n", "\n") == expected {
        return Ok(());
    }
    if std::env::var(BLESS_ENV).as_deref() == Ok("1") {
        return std::fs::write(path, expected).map_err(|e| format!("{}: {e}", path.display()));
    }
    Err(format!(
        "{} is stale: it differs from the binding the compiler generates for the current Vesper source. \
         Rerun the build with {BLESS_ENV}=1 to rewrite it, then review the diff.",
        path.display()
    ))
}

/// True when `path` is a Loom package directory.
pub fn is_package(path: &Path) -> bool {
    path.is_dir() && path.join("Loom.toml").is_file()
}

/// The package at `dir` and its path-dependency closure, as canonical
/// directories, root first. Dependencies without a `path` and path
/// dependencies that do not exist (optional ones) are skipped; the compiler
/// reports them if they matter.
pub fn package_closure(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let canonical =
            std::fs::canonicalize(&next).map_err(|e| format!("{}: {e}", next.display()))?;
        if out.contains(&canonical) {
            continue;
        }
        let manifest = canonical.join("Loom.toml");
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("{}: {e}", manifest.display()))?;
        let value: toml::Table =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let mut deps = Vec::new();
        if let Some(table) = value.get("dependencies").and_then(|d| d.as_table()) {
            for spec in table.values() {
                if let Some(path) = spec.get("path").and_then(|p| p.as_str()) {
                    let dep = canonical.join(path);
                    if dep.join("Loom.toml").is_file() {
                        deps.push(dep);
                    }
                }
            }
        }
        out.push(canonical);
        // Reverse, so dependencies are visited in manifest order.
        todo.extend(deps.into_iter().rev());
    }
    Ok(out)
}

/// The paths a port build reads, for `cargo:rerun-if-changed`: the file
/// itself, or every package directory in the closure (Cargo rescans a
/// directory, so new modules are noticed too).
pub fn inputs(source: &Path) -> Result<Vec<PathBuf>, String> {
    if is_package(source) {
        package_closure(source)
    } else if source.is_file() {
        Ok(vec![source.to_path_buf()])
    } else {
        Err(format!(
            "{}: not a .ves file or a directory with a Loom.toml",
            source.display()
        ))
    }
}

/// The compiler input with `edits` applied. Without edits it is `source`
/// unchanged. With edits, `source` (a file, or a package and its path
/// dependencies with their relative layout) is copied under `scratch`,
/// each edit is applied where it occurs exactly once (in the file, or
/// across the root package's `.ves` files), and the copy is returned.
pub fn prepare_source(
    source: &Path,
    scratch: &Path,
    edits: &[(String, String)],
) -> Result<PathBuf, String> {
    if edits.is_empty() {
        return Ok(source.to_path_buf());
    }
    if scratch.exists() {
        std::fs::remove_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    }
    if !is_package(source) {
        let mut text =
            std::fs::read_to_string(source).map_err(|e| format!("{}: {e}", source.display()))?;
        for (from, to) in edits {
            if text.matches(from.as_str()).count() != 1 {
                return Err(format!(
                    "edit '{from}' must occur exactly once in {}",
                    source.display()
                ));
            }
            text = text.replacen(from.as_str(), to, 1);
        }
        let name = source.file_name().ok_or("source has no file name")?;
        let copy = scratch.join(name);
        write(&copy, &text)?;
        return Ok(copy);
    }
    let closure = package_closure(source)?;
    let base = common_ancestor(&closure);
    let mut root_files = Vec::new();
    for (i, package) in closure.iter().enumerate() {
        let rel = package.strip_prefix(&base).unwrap_or(package);
        let copy = scratch.join(rel);
        copy_file(&package.join("Loom.toml"), &copy.join("Loom.toml"))?;
        let mut files = Vec::new();
        if package.join("src").is_dir() {
            walk(&package.join("src"), &mut files)?;
        }
        for file in files {
            let target = copy.join(file.strip_prefix(package).unwrap_or(&file));
            copy_file(&file, &target)?;
            if i == 0 && file.extension().is_some_and(|e| e == "ves") {
                root_files.push(target);
            }
        }
    }
    for (from, to) in edits {
        let mut hits = Vec::new();
        for file in &root_files {
            let text =
                std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
            let n = text.matches(from.as_str()).count();
            if n > 0 {
                hits.push((file.clone(), text, n));
            }
        }
        match hits.as_slice() {
            [(file, text, 1)] => write(file, &text.replacen(from.as_str(), to, 1))?,
            _ => {
                return Err(format!(
                    "edit '{from}' must occur exactly once in the .ves files of {}",
                    source.display()
                ));
            }
        }
    }
    let root = closure[0].strip_prefix(&base).unwrap_or(&closure[0]);
    Ok(scratch.join(root))
}

fn common_ancestor(paths: &[PathBuf]) -> PathBuf {
    let mut base = paths[0].clone();
    while !paths.iter().all(|p| p.starts_with(&base)) {
        if !base.pop() {
            break;
        }
    }
    base
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        // `metadata` follows symlinks (Bazel runfiles are symlink trees).
        let kind = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            walk(&path, out)?;
        } else if kind.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", from.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The workspace root: the nearest ancestor of the crate being built whose
/// `Cargo.toml` declares `[workspace]`. Found at run time, because compiled
/// code must not embed build paths (`env!("CARGO_MANIFEST_DIR")`).
fn repository() -> Result<PathBuf, String> {
    let crate_dir = std::fs::canonicalize(env("CARGO_MANIFEST_DIR")?)
        .map_err(|e| format!("CARGO_MANIFEST_DIR: {e}"))?;
    crate_dir
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|text| text.lines().any(|l| l.trim() == "[workspace]"))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("no [workspace] Cargo.toml above {}", crate_dir.display()))
}

/// The `vesper` executable: [`COMPILER_ENV`], or the in-tree compiler built
/// by a nested Cargo build (see the crate documentation).
fn compiler() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(COMPILER_ENV) {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!(
                "{COMPILER_ENV}={} is not a file; build it with `cargo build --bin vesper`",
                path.display()
            ));
        }
        println!("cargo:rerun-if-changed={}", path.display());
        return Ok(path);
    }
    let root = repository()?;
    let target_dir = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => root.join(dir),
        None => root.join("target"),
    }
    .join("vesper-port-compiler");
    // Rebuild (a no-op when fresh) whenever the compiler's sources change.
    for input in ["src", "crates", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    // An explicit host triple: the compiler runs here, whatever target the
    // outer build (or CARGO_BUILD_TARGET) selects, and its path is then
    // always `<target dir>/<host>/debug`.
    let host = env("HOST")?;
    let mut command = Command::new(&cargo);
    command
        .current_dir(&root)
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .args(["--target", &host])
        .arg("--target-dir")
        .arg(&target_dir)
        .env(IN_COMPILER_BUILD_ENV, "1")
        // Flags the outer build chose for its own target, not this host tool.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR");
    run(&mut command, Path::new(&cargo))?;
    let exe = format!("vesper{}", std::env::consts::EXE_SUFFIX);
    Ok(target_dir.join(host).join("debug").join(exe))
}

fn run(command: &mut Command, program: &Path) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{:?} failed ({}):\n{}{}",
        command,
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set; call Port::build from build.rs"))
}
