//! Build-script helper for Rust crates with a `vesper-port` feature
//! (port wave P0 lane 6, #878; recipe in `docs/maintenance/porting-a-crate.md`).
//!
//! Under the feature, a crate's `build.rs` calls [`Port::build`]. It compiles
//! the crate's Vesper reimplementation with the in-tree compiler (this crate
//! links the `vesper` library, so no installed binary is involved), archives
//! the object into a static library, tells Cargo to link it, and checks that
//! the committed Rust binding is exactly what the compiler generates for the
//! current source. The crate's public functions then forward to the binding
//! and its unchanged tests run against the Vesper build.
//!
//! In `build.rs`, under `#[cfg(feature = "vesper-port")]`:
//!
//! ```no_run
//! vesper_port_harness::Port::new("pilot", "vesper/pilot.ves")
//!     .binding("port/binding.rs")
//!     .rust_types("crate")
//!     .build();
//! ```
use std::path::{Path, PathBuf};
use vesper::native::export::{self, BindingOptions, LibraryOptions};

/// Set to `1` to rewrite a stale committed binding instead of failing.
pub const BLESS_ENV: &str = "VESPER_PORT_BLESS";

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
    /// is the `.ves` file, relative to the crate directory.
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

    /// Replace `from`, which must occur exactly once in the source, with
    /// `to` before compiling. Divergence checks use this to build a
    /// deliberately wrong port from the real source.
    pub fn edit(mut self, from: &str, to: &str) -> Self {
        self.edits.push((from.into(), to.into()));
        self
    }

    /// Compile, archive and link the library, and check the binding. Panics
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
        let source_path = dir.join(&self.source);
        println!("cargo:rerun-if-changed={}", source_path.display());
        println!("cargo:rerun-if-env-changed={BLESS_ENV}");
        let mut source = std::fs::read_to_string(&source_path)
            .map_err(|e| format!("{}: {e}", source_path.display()))?;
        for (from, to) in &self.edits {
            if source.matches(from.as_str()).count() != 1 {
                return Err(format!(
                    "edit '{from}' must occur exactly once in {}",
                    source_path.display()
                ));
            }
            source = source.replacen(from.as_str(), to, 1);
        }
        let library = export::compile_source(
            &source_path.display().to_string(),
            &source,
            &LibraryOptions {
                library: &self.library,
                target: Some(&target),
                optimize: true,
            },
        )
        .map_err(|e| e.to_string())?;
        if let Some(binding) = &self.binding {
            let path = dir.join(binding);
            println!("cargo:rerun-if-changed={}", path.display());
            let expected = export::rust_binding(
                &library.interface,
                &BindingOptions {
                    types_path: self.types_path.clone(),
                },
            );
            check_binding(&path, &expected)?;
        }
        let extension = if target.contains("windows") {
            "obj"
        } else {
            "o"
        };
        let object = out.join(format!("vesper_{}.{extension}", self.library));
        std::fs::write(&object, &library.object)
            .map_err(|e| format!("{}: {e}", object.display()))?;
        let interface = out.join(format!("vesper_{}.json", self.library));
        let json = serde_json_pretty(&library.interface)?;
        std::fs::write(&interface, json).map_err(|e| format!("{}: {e}", interface.display()))?;
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

fn serde_json_pretty(interface: &export::LibraryInterface) -> Result<String, String> {
    vesper::native::export::interface_json(interface)
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set; call Port::build from build.rs"))
}
