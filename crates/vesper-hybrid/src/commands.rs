//! The host side of `@vesper/driver-commands` (self-hosting SH5 lane 13,
//! #1336): the `HybridCommands` import (packages/vesper-driver-engines/
//! commands-<os>.vsys). The routing in front of every command (help, version,
//! the unknown-command error, the hand-off to `vesper-<tool>`) is Vesper,
//! over @vesper-std (#1996); this is the command engines it calls.
use std::cell::RefCell;
use std::io;
use std::path::Path;

thread_local! {
    static LAST_EBPF_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_FORGE_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_LINT_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_DOC_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_TEST_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_VERIFY_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Commands the Vesper command layers answered (the driver's
/// `hd_commands_answered`); `VESPER_HYBRID_STATS=1` reports it. A command
/// handed back to the Rust CLI counts none.
pub static ANSWERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Flushes Rust's stdout when an engine call returns: the Vesper layers
/// write the streams directly, so text an engine printed with `print!`
/// must not wait in Rust's line buffer behind theirs. A failed flush is
/// left to the next write, as Rust's line buffer leaves it.
struct FlushOnReturn;

impl Drop for FlushOnReturn {
    fn drop(&mut self) {
        use std::io::Write as _;
        let _ = io::stdout().flush();
    }
}

/// The `HybridCommands` import's symbols (named by `// @export vsys
/// HybridCommands rust=crate::commands::host` in modules/driver).
pub mod host {
    use super::*;

    // `vesper lsp`'s engine seam (#1828 part 15c).
    pub use crate::lsp_engine::*;

    /// The bytes of a lent `nocapture in CSlice[u8]`.
    ///
    /// # Safety
    /// `ptr` is valid for `len` bytes for the call.
    unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
        if len == 0 || ptr.is_null() {
            return &[];
        }
        // SAFETY: as documented.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    /// Load the program at `path` into the running kernel for `vesper ebpf
    /// check --load`, once @vesper/ebpf-check accepted it, printing the
    /// `kernel: ...` line as `src/ebpf_commands.rs` does. Returns `0` on
    /// success or `err.len()` (`> 0`) on error, storing the error bytes in
    /// `LAST_EBPF_ERR`.
    ///
    /// # Safety
    /// `path` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_ebpf_load(path: *const u8, len: usize) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: lent for the call.
        let path =
            Path::new(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, len)) });
        match ebpf_load_impl(path) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_EBPF_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_EBPF_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_ebpf_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_EBPF_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    fn ebpf_program(path: &Path, bytes: &[u8]) -> Result<vesper_ebpf::Program, String> {
        use vesper_ebpf::{Program, ProgramType, RawInsn, elf, insn};
        let fail = |e: String| format!("{}: {e}", path.display());
        if bytes.starts_with(b"\x7fELF") {
            return elf::read_object(bytes).map_err(|e| fail(format!("{e:?}")));
        }
        let slots = if path.extension().is_some_and(|e| e == "hex" || e == "txt") {
            let mut raw = Vec::new();
            for (n, line) in String::from_utf8_lossy(bytes).lines().enumerate() {
                let line = line.split('#').next().unwrap_or_default().trim();
                if line.is_empty() {
                    continue;
                }
                let digits: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                if digits.len() != 16 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(fail(format!(
                        "line {}: expected one 8-byte slot as 16 hex digits",
                        n + 1
                    )));
                }
                for i in 0..8 {
                    raw.push(
                        u8::from_str_radix(&digits[2 * i..2 * i + 2], 16)
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
            raw
        } else {
            bytes.to_vec()
        };
        if slots.is_empty() {
            return Err(fail("program has no instructions".into()));
        }
        let insns: Vec<RawInsn> =
            insn::slots_from_bytes(&slots).map_err(|e| fail(format!("{e:?}")))?;
        Ok(Program {
            name: path
                .file_stem()
                .map_or_else(|| "prog".into(), |s| s.to_string_lossy().into_owned()),
            prog_type: ProgramType::Xdp,
            insns,
            maps: Vec::new(),
            map_relocs: Vec::new(),
            functions: Vec::new(),
            license: "Dual MIT/GPL".into(),
            attach: String::new(),
            kfuncs: Vec::new(),
            core_relos: Vec::new(),
            lines: Vec::new(),
        })
    }

    fn ebpf_load_impl(input: &Path) -> Result<(), String> {
        let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let prog = ebpf_program(input, &bytes)?;
        ebpf_kernel_load(&prog)
    }

    #[cfg(target_os = "linux")]
    fn ebpf_kernel_load(prog: &vesper_ebpf::Program) -> Result<(), String> {
        use vesper_ebpf::kernel::{LoadError, load};
        match load(prog) {
            Ok(_) => {
                println!("kernel: loaded and accepted by the running kernel's verifier");
                Ok(())
            }
            Err(LoadError::Unavailable { errno }) => {
                println!(
                    "kernel: unavailable (bpf() refused with errno {errno}); only the pre-flight verifier ran"
                );
                Ok(())
            }
            Err(LoadError::Rejected { errno, log }) => Err(format!(
                "kernel: rejected by the running kernel's verifier (errno {errno}):\n{log}"
            )),
            Err(e) => Err(format!("kernel: {e}")),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn ebpf_kernel_load(_prog: &vesper_ebpf::Program) -> Result<(), String> {
        println!(
            "kernel: unavailable (eBPF loading needs Linux); only the pre-flight verifier ran"
        );
        Ok(())
    }

    /// Execute `vesper forge <check|expand|lean>` after CLI argument parsing,
    /// returning `0` on success or `err.len()` (`> 0`) on error with `LAST_FORGE_ERR` set.
    ///
    /// # Safety
    /// `input`, `package`, `policy`, and `provenance` are lent byte slices.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_forge_run(
        action: u32,
        input: *const u8,
        input_len: usize,
        package: *const u8,
        package_len: usize,
        has_policy: bool,
        policy: *const u8,
        policy_len: usize,
        has_provenance: bool,
        provenance: *const u8,
        provenance_len: usize,
    ) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: all slices are lent for the duration of the call.
        let input = Path::new(unsafe {
            std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(input, input_len))
        });
        let package = String::from_utf8_lossy(unsafe { bytes(package, package_len) }).into_owned();
        let policy = has_policy.then(|| {
            Path::new(unsafe {
                std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(policy, policy_len))
            })
        });
        let provenance = has_provenance.then(|| {
            Path::new(unsafe {
                std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(provenance, provenance_len))
            })
        });
        match forge_run_impl(action, input, package, policy, provenance) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_FORGE_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_FORGE_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_forge_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_FORGE_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    fn forge_run_impl(
        action: u32,
        input: &Path,
        package: String,
        policy_path: Option<&Path>,
        provenance_path: Option<&Path>,
    ) -> Result<(), String> {
        use serde::Serialize;
        use std::fs::{self, OpenOptions};
        use std::io::{self, Read, Write};
        use vesper::{
            ast::Diagnostic,
            forge_frontend::{
                self as forge, CompileConfig, CompileLimits, ExpandedChecked, PolicyConfig, Stage,
            },
        };

        const MAX_POLICY: usize = 64 * 1024;
        const MAX_JSON: usize = 2 * 1024 * 1024;
        let check = action == 0;

        struct InputError {
            stage: Stage,
            message: String,
        }
        impl From<io::Error> for InputError {
            fn from(error: io::Error) -> Self {
                Self {
                    stage: Stage::Io,
                    message: error.to_string(),
                }
            }
        }
        fn read_regular(path: &Path, max: usize) -> Result<String, InputError> {
            if !fs::symlink_metadata(path)
                .map_err(InputError::from)?
                .is_file()
            {
                return Err(InputError {
                    stage: Stage::Io,
                    message: "source macros require a regular UTF-8 file without final symlinks"
                        .into(),
                });
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x0020_0000);
            }
            let file = options.open(path).map_err(InputError::from)?;
            let metadata = file.metadata().map_err(InputError::from)?;
            if !metadata.is_file() || metadata.len() > max as u64 {
                return Err(InputError {
                    stage: Stage::Resource,
                    message: "source macro file byte limit exceeded".into(),
                });
            }
            let mut bytes = Vec::new();
            file.take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(InputError::from)?;
            if bytes.len() > max {
                return Err(InputError {
                    stage: Stage::Resource,
                    message: "source macro file byte limit exceeded".into(),
                });
            }
            String::from_utf8(bytes).map_err(|_| InputError {
                stage: Stage::Io,
                message: "source macro files must be UTF-8".into(),
            })
        }

        struct BoundedOutput {
            bytes: Vec<u8>,
        }
        impl Write for BoundedOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|n| n > MAX_JSON)
                {
                    return Err(io::Error::other("source macro output byte limit exceeded"));
                }
                self.bytes
                    .try_reserve(bytes.len())
                    .map_err(|_| io::Error::other("source macro output allocation failed"))?;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        fn write_json(value: &impl Serialize) -> Result<(), String> {
            let mut output = BoundedOutput { bytes: Vec::new() };
            serde_json::to_writer(&mut output, value).map_err(|e| e.to_string())?;
            output.write_all(b"\n").map_err(|e| e.to_string())?;
            io::stdout()
                .lock()
                .write_all(&output.bytes)
                .map_err(|e| e.to_string())
        }

        #[derive(Serialize)]
        struct Success<'a> {
            schema: &'static str,
            status: &'static str,
            scope: &'static str,
            package: &'a str,
            source_blake3: String,
            expanded_blake3: String,
            source_bytes: usize,
            expanded_bytes: usize,
            macro_definitions: usize,
            invocations: usize,
            expanded_source: &'a str,
            source_map: &'a [forge::SourceMapSegment],
            audits: &'a [forge::InvocationAudit],
            provenance: &'a forge::provenance::ProvenanceTable,
        }
        impl<'a> From<&'a ExpandedChecked> for Success<'a> {
            fn from(value: &'a ExpandedChecked) -> Self {
                Self {
                    schema: forge::SCHEMA,
                    status: "expanded_checked",
                    scope: "tier1_source",
                    package: value.package(),
                    source_blake3: blake3::Hash::from_bytes(value.original_hash())
                        .to_hex()
                        .to_string(),
                    expanded_blake3: blake3::Hash::from_bytes(value.expanded_hash())
                        .to_hex()
                        .to_string(),
                    source_bytes: value.original_source().len(),
                    expanded_bytes: value.expanded_source().len(),
                    macro_definitions: value.macro_definitions(),
                    invocations: value.invocations(),
                    expanded_source: value.expanded_source(),
                    source_map: value.source_map(),
                    audits: value.audits(),
                    provenance: value.provenance(),
                }
            }
        }
        #[derive(Serialize)]
        struct Rejected<'a> {
            schema: &'static str,
            status: &'static str,
            stage: Stage,
            message: &'a str,
            source_blake3: Option<String>,
            expanded_source: Option<&'a str>,
            diagnostics: &'a [Diagnostic],
            #[serde(skip_serializing_if = "<[_]>::is_empty")]
            origins: &'a [Option<forge::provenance::DiagnosticOrigin>],
            source_map: [(); 0],
        }
        fn reject_with_origins(
            check: bool,
            stage: Stage,
            message: &str,
            source: Option<&str>,
            expanded: Option<&str>,
            diagnostics: &[Diagnostic],
            origins: &[Option<forge::provenance::DiagnosticOrigin>],
        ) -> Result<(), String> {
            if check {
                write_json(&Rejected {
                    schema: forge::SCHEMA,
                    status: "rejected",
                    stage,
                    message,
                    source_blake3: source.map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()),
                    expanded_source: expanded,
                    diagnostics,
                    origins,
                    source_map: [],
                })?;
            }
            Err(message.chars().take(4096).collect())
        }

        let limits = CompileLimits::default();
        let source = match read_regular(input, limits.max_source_bytes) {
            Ok(s) => s,
            Err(e) => return reject_with_origins(check, e.stage, &e.message, None, None, &[], &[]),
        };
        let policy = if let Some(path) = policy_path {
            let text = match read_regular(path, MAX_POLICY) {
                Ok(s) => s,
                Err(e) => {
                    return reject_with_origins(
                        check,
                        Stage::Config,
                        &e.message,
                        Some(&source),
                        None,
                        &[],
                        &[],
                    );
                }
            };
            match serde_json::from_str::<PolicyConfig>(&text) {
                Ok(policy) => policy,
                Err(e) => {
                    return reject_with_origins(
                        check,
                        Stage::Config,
                        &e.to_string(),
                        Some(&source),
                        None,
                        &[],
                        &[],
                    );
                }
            }
        } else {
            PolicyConfig::default()
        };
        match forge::expand_source(&source, &CompileConfig { package, policy }, limits) {
            Ok(artifact) => {
                if action == 2 {
                    let lean = forge::provenance::extract_lean(&artifact)?;
                    if let Some(path) = provenance_path {
                        let bytes = serde_json::to_vec_pretty(artifact.provenance())
                            .map_err(|e| e.to_string())?;
                        let mut file = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                            .map_err(|e| format!("{}: {e}", path.display()))?;
                        file.write_all(&bytes)
                            .and_then(|()| file.write_all(b"\n"))
                            .map_err(|e| e.to_string())?;
                    }
                    return io::stdout()
                        .lock()
                        .write_all(lean.text.as_bytes())
                        .map_err(|e| e.to_string());
                }
                if check {
                    if let Err(error) = write_json(&Success::from(&artifact)) {
                        return reject_with_origins(
                            true,
                            Stage::Resource,
                            &error,
                            Some(&source),
                            None,
                            &[],
                            &[],
                        );
                    }
                    Ok(())
                } else {
                    io::stdout()
                        .lock()
                        .write_all(artifact.expanded_source().as_bytes())
                        .map_err(|e| e.to_string())
                }
            }
            Err(error) => reject_with_origins(
                check,
                error.stage(),
                error.message(),
                Some(&source),
                error.expanded_source(),
                error.diagnostics(),
                error.origins(),
            ),
        }
    }

    /// Run `vesper lint` after CLI option parsing, returning `0` on success or
    /// `err.len()` (`> 0`) on error with `LAST_LINT_ERR` set.
    ///
    /// # Safety
    /// `path` and `config` are lent byte slices.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_lint_run(
        has_path: bool,
        path: *const u8,
        path_len: usize,
        fix: bool,
        semver_safe: bool,
        json: bool,
        has_config: bool,
        config: *const u8,
        config_len: usize,
    ) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: lent for the call.
        let path = has_path.then(|| {
            Path::new(unsafe {
                std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, path_len))
            })
            .to_path_buf()
        });
        let config = has_config.then(|| {
            Path::new(unsafe {
                std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(config, config_len))
            })
            .to_path_buf()
        });
        match lint_run_impl(path, fix, semver_safe, json, config) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_LINT_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_LINT_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_lint_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_LINT_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    fn lint_run_impl(
        path_opt: Option<std::path::PathBuf>,
        fix: bool,
        semver_safe: bool,
        json: bool,
        config_opt: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        use std::fs::{self, OpenOptions};
        use std::io::{Read, Write};
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};
        use vesper::tooling::doc::sources::{PackageSources, SourceLimits, discover, package_root};
        use vesper::tooling::lint::{
            ConfigProvenance, SourceReport, prepare_source, prepare_source_with_policy,
        };
        use vesper_lint::policy::{Policy, PolicyLimits};

        const MAX_BYTES: usize = 262_144;

        fn invalid(json: bool, error: String, config_label: Option<String>) -> Result<(), String> {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "status": "invalid",
                        "error": error,
                        "config": config_label
                    })
                );
            }
            Err(error)
        }
        fn read_source(path: &Path) -> Result<(String, fs::Permissions), String> {
            let metadata =
                fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if !metadata.is_file() {
                return Err("lint input must be a regular file without final symlinks".into());
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x0020_0000);
            }
            let file = options.open(path).map_err(|e| e.to_string())?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
                return Err("lint input is not a bounded regular file (256 KiB)".into());
            }
            let mut bytes = vec![];
            file.take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_BYTES {
                return Err("lint source exceeds 256 KiB".into());
            }
            Ok((
                String::from_utf8(bytes).map_err(|_| "lint input must be UTF-8")?,
                metadata.permissions(),
            ))
        }
        fn load_policy(path: &Path) -> Result<(Policy, ConfigProvenance), String> {
            const MAX_CONFIG_BYTES: u64 = 65_536;
            if path.as_os_str().len() > 4096 {
                return Err("lint config path exceeds 4096 bytes".into());
            }
            if !fs::symlink_metadata(path)
                .map_err(|e| e.to_string())?
                .is_file()
            {
                return Err("lint config must be a regular file without final symlinks".into());
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x0020_0000);
            }
            let file = options.open(path).map_err(|e| e.to_string())?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
                return Err("lint config must be a bounded regular file (64 KiB)".into());
            }
            let mut bytes = vec![];
            file.take(MAX_CONFIG_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_CONFIG_BYTES {
                return Err("lint config exceeds 64 KiB".into());
            }
            let source = std::str::from_utf8(&bytes).map_err(|_| "lint config must be UTF-8")?;
            let policy =
                Policy::parse(source, PolicyLimits::default()).map_err(|e| e.to_string())?;
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|e| e.to_string())?
                    .join(path)
            };
            let provenance = ConfigProvenance::Explicit {
                path: absolute.display().to_string(),
                bytes: bytes.len(),
                blake3: blake3::hash(&bytes).to_hex().to_string(),
            };
            Ok((policy, provenance))
        }
        struct Staging(Option<PathBuf>);
        impl Staging {
            fn publish(&mut self, destination: &Path) -> Result<(), String> {
                let path = self
                    .0
                    .as_deref()
                    .ok_or("staging path ownership already transferred")?;
                fs::rename(path, destination).map_err(|e| e.to_string())?;
                self.0 = None;
                Ok(())
            }
        }
        impl Drop for Staging {
            fn drop(&mut self) {
                if let Some(path) = &self.0 {
                    let _ = fs::remove_file(path);
                }
            }
        }
        fn replace_source(
            path: &Path,
            original: &str,
            candidate: &str,
            permissions: fs::Permissions,
        ) -> Result<(), String> {
            if permissions.readonly() {
                return Err("lint refuses to replace a readonly source".into());
            }
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let temporary_path = parent.join(format!(
                ".vesper-lint-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary_path).map_err(|e| e.to_string())?;
            let mut temporary = Staging(Some(temporary_path));
            file.write_all(candidate.as_bytes())
                .map_err(|e| e.to_string())?;
            file.set_permissions(permissions)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            let (current, _) = read_source(path)?;
            if current != original {
                return Err(
                    "source changed while lint was validating; original path left untouched".into(),
                );
            }
            temporary.publish(path)
        }
        fn print_report(prefix: &str, report: &SourceReport) {
            for finding in &report.diagnostics {
                println!(
                    "{prefix}{:?} {} {}:{}..{}: {}",
                    finding.severity,
                    finding.finding.code,
                    finding.finding.function,
                    finding.finding.span.start,
                    finding.finding.span.end,
                    finding.finding.message
                );
            }
            for rule in &report.policy.unavailable {
                println!(
                    "{prefix}Unavailable lint {} ({:?}); not analyzed",
                    rule.lint.as_str(),
                    rule.severity
                );
            }
            println!(
                "{prefix}lint: {} diagnostic(s), {} conservative skip(s), changed={}, remaining deny={}",
                report.diagnostics.len(),
                report.skipped.len(),
                report.changed,
                report.outcome.remaining_denied
            );
        }

        let mut config_label = config_opt.as_ref().map(|p| p.display().to_string());
        let mut explicit_config = config_opt.is_some();
        let (mut policy, mut provenance) = if let Some(config) = config_opt {
            match load_policy(&config) {
                Ok(v) => v,
                Err(error) => return invalid(json, error, config_label),
            }
        } else {
            (Policy::default(), ConfigProvenance::Default)
        };
        let package_start = match &path_opt {
            None => Some(PathBuf::from(".")),
            Some(path) if fs::metadata(path).is_ok_and(|m| m.is_dir()) => Some(path.clone()),
            Some(_) => None,
        };
        if !explicit_config {
            let start = package_start.as_deref().unwrap_or_else(|| {
                path_opt
                    .as_deref()
                    .and_then(Path::parent)
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
            });
            let manifest = match package_root(start, SourceLimits::default()) {
                Ok((_, manifest)) => manifest,
                Err(error) => return invalid(json, error, config_label),
            };
            if let Some(manifest) = manifest {
                config_label = Some(manifest.display().to_string());
                let (loaded_policy, loaded_prov) = match load_policy(&manifest) {
                    Ok(value) => value,
                    Err(error) => return invalid(json, error, config_label),
                };
                if let ConfigProvenance::Explicit {
                    path,
                    bytes,
                    blake3,
                } = loaded_prov
                {
                    provenance = ConfigProvenance::Discovered {
                        path,
                        bytes,
                        blake3,
                    };
                }
                policy = loaded_policy;
                explicit_config = true;
            }
        }

        let lint_one = |path: &Path| -> Result<(SourceReport, String), String> {
            let (source, permissions) = read_source(path)?;
            let prepared = if explicit_config {
                prepare_source_with_policy(&source, fix, semver_safe, &policy, provenance.clone())?
            } else {
                prepare_source(&source, fix, semver_safe)?
            };
            let output =
                serde_json::to_string_pretty(&prepared.report).map_err(|e| e.to_string())?;
            if let Some(candidate) = prepared.candidate {
                replace_source(path, &source, &candidate, permissions)?;
            }
            Ok((prepared.report, output))
        };

        if let Some(start) = package_start {
            let package: PackageSources = match discover(&start, SourceLimits::default()) {
                Ok(p) => p,
                Err(error) => return invalid(json, error, config_label),
            };
            let mut files = Vec::new();
            let mut denied = 0usize;
            let mut failed = 0usize;
            for file in &package.files {
                let name = package.display(file);
                match lint_one(file) {
                    Ok((report, _)) => {
                        if report.status == "denied" {
                            denied += 1;
                        }
                        if !json {
                            print_report(&format!("{name}: "), &report);
                        }
                        files.push(serde_json::json!({"path": name, "report": report}));
                    }
                    Err(error) => {
                        failed += 1;
                        if !json {
                            println!("{name}: error: {error}");
                        }
                        files.push(serde_json::json!({"path": name, "error": error}));
                    }
                }
            }
            let status = if failed > 0 {
                "failed"
            } else if denied > 0 {
                "denied"
            } else {
                "passed"
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "status": status,
                        "root": package.root.display().to_string(),
                        "manifest": package.manifest.as_ref().map(|m| m.display().to_string()),
                        "files": files,
                    }))
                    .map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "lint: {} file(s) under {}, {failed} failed, {denied} denied",
                    package.files.len(),
                    package.root.display()
                );
            }
            return match status {
                "passed" => Ok(()),
                "denied" => {
                    Err("lint policy denied remaining findings or unavailable analysis".into())
                }
                _ => Err(format!("lint failed on {failed} source file(s)")),
            };
        }

        let path = path_opt.expect("explicit file path");
        match lint_one(&path) {
            Ok((report, output)) => {
                if json {
                    println!("{output}");
                } else {
                    print_report("", &report);
                }
                if report.status == "denied" {
                    Err("lint policy denied remaining findings or unavailable analysis".into())
                } else {
                    Ok(())
                }
            }
            Err(error) => invalid(json, error, config_label),
        }
    }

    fn unpack_doc_args(raw: &[u8]) -> Result<Vec<String>, String> {
        let mut args = Vec::new();
        for slice in raw.split(|&b| b == 0) {
            if slice.is_empty() && args.is_empty() && raw.is_empty() {
                break;
            }
            args.push(String::from_utf8_lossy(slice).into_owned());
        }
        if raw.ends_with(&[0]) {
            args.pop();
        }
        Ok(args)
    }

    /// Run `vesper doc` from the Vesper `doc` command layer after CLI validation.
    ///
    /// Returns `0` on success, or the byte length of the error message stored in
    /// `LAST_DOC_ERR`.
    ///
    /// # Safety
    /// `packed` is a lent readable slice of `packed_len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_doc_run(packed: *const u8, packed_len: usize) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: lent for the call.
        let raw = unsafe { bytes(packed, packed_len) };
        let result = unpack_doc_args(raw).and_then(crate::doc_commands::run);
        match result {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_DOC_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_DOC_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_doc_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_DOC_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    fn unpack_test_args(raw: &[u8]) -> Vec<String> {
        let mut args = Vec::new();
        for slice in raw.split(|&b| b == 0) {
            if slice.is_empty() && args.is_empty() && raw.is_empty() {
                break;
            }
            args.push(String::from_utf8_lossy(slice).into_owned());
        }
        if raw.ends_with(&[0]) {
            args.pop();
        }
        args
    }

    /// Run `vesper test` from the Vesper `test` command layer after CLI validation.
    ///
    /// Returns `0` on success, or the byte length of the error message stored in
    /// `LAST_TEST_ERR`.
    ///
    /// # Safety
    /// `packed` is a lent readable slice of `packed_len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_test_run(packed: *const u8, packed_len: usize) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: lent for the call.
        let raw = unsafe { bytes(packed, packed_len) };
        let args = unpack_test_args(raw);
        match crate::doctest_commands::run(args) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_TEST_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_TEST_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_test_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_TEST_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    /// Run `vesper verify SOURCE --extract` from the Vesper `verify` command
    /// layer after CLI validation (`--format` runs in @vesper/verify).
    ///
    /// Returns `0` on success, or the byte length of the error message stored in
    /// `LAST_VERIFY_ERR`.
    ///
    /// # Safety
    /// `packed` is a lent readable slice of `packed_len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_verify_run(packed: *const u8, packed_len: usize) -> u64 {
        let _flush = FlushOnReturn;
        // SAFETY: lent for the call.
        let raw = unsafe { bytes(packed, packed_len) };
        match crate::verify_commands::run(unpack_test_args(raw)) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_VERIFY_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_VERIFY_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_verify_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_VERIFY_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }
}
