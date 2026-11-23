//! The host side of `@vesper/driver-commands` (self-hosting SH5 lane 13,
//! #1336): the `HybridCommands` import (packages/vesper-driver-commands/
//! commands.vsys). The routing in front of every command (help, version,
//! the unknown-command error, the hand-off to `vesper-<tool>`) is Vesper;
//! this prints for it as Rust's macros do and answers the few questions
//! @vesper-std cannot (the running executable's directory, the help text,
//! whether a file is executable).
use std::cell::RefCell;
use std::io;
use std::path::Path;

thread_local! {
    static LAST_EBPF_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_FORGE_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Prints of the Vesper command layers (`hc_print` and `hc_help`);
/// `VESPER_HYBRID_STATS=1` reports it. A command handed back to the Rust
/// CLI makes none.
pub static PRINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The `io::Error` with @vesper-std's error word (the kind in the low 8
/// bits, the raw OS error in the high 32), as the driver's host builds it.
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

/// Print as the Rust commands do (`print!`, `println!`, `eprintln!`): a
/// failed write panics with std's own message, and the CLI exits 101 as the
/// Rust CLI does when its thread panics. The panic must not unwind into
/// the Vesper caller, so it is caught here and the process exits.
fn rust_print(print: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(print)).is_err() {
        std::process::exit(101);
    }
}

/// The `HybridCommands` import's symbols (named by `// @export vsys
/// HybridCommands rust=crate::commands::host` in modules/driver).
pub mod host {
    use super::*;

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

    /// Copy `text` into `buf` when it fits; its length (the copy-out
    /// protocol), or -1 for none.
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    unsafe fn copy_out(text: Option<&[u8]>, buf: *mut u8, len: usize) -> i64 {
        let Some(text) = text else { return -1 };
        if text.len() <= len && !buf.is_null() {
            // SAFETY: `buf` holds `len >= text.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len()) };
        }
        i64::try_from(text.len()).unwrap_or(i64::MAX)
    }

    /// Write `text` to stdout (1) or stderr (2).
    ///
    /// # Safety
    /// `text` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_print(stream: u32, text: *const u8, len: usize) {
        PRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: lent for the call.
        let text = String::from_utf8_lossy(unsafe { bytes(text, len) });
        if stream == 2 {
            rust_print(|| eprint!("{text}"));
        } else {
            rust_print(|| print!("{text}"));
        }
    }

    /// The CLI's help (no command, `--help` or `-h`).
    #[unsafe(no_mangle)]
    pub extern "C" fn hc_help() {
        PRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        rust_print(|| print!("{}", crate::HELP));
    }

    /// Text `which`: see `text_*` in the package. `index` is unused.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_text(which: u32, _index: u64, buf: *mut u8, len: usize) -> i64 {
        let exe = std::env::current_exe().ok();
        let dir = |p: Option<std::path::PathBuf>| p.and_then(|p| p.parent().map(Path::to_path_buf));
        let owned = match which {
            4 => dir(exe.clone()),
            5 => dir(exe.and_then(|e| e.canonicalize().ok())),
            _ => None,
        };
        let text: Option<&[u8]> = match which {
            1 => Some(env!("CARGO_PKG_VERSION").as_bytes()),
            2 => Some(std::env::consts::ARCH.as_bytes()),
            3 => Some(std::env::consts::OS.as_bytes()),
            4 | 5 => owned.as_ref().map(|p| p.as_os_str().as_encoded_bytes()),
            _ => None,
        };
        // SAFETY: lent for the call.
        unsafe { copy_out(text, buf, len) }
    }

    /// Rust's `Display` of the `io::Error` with this error word.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_io_error(word: u64, buf: *mut u8, len: usize) -> i64 {
        let text = io_error(word).to_string();
        // SAFETY: lent for the call.
        unsafe { copy_out(Some(text.as_bytes()), buf, len) }
    }

    /// 1 when `path` is a regular file the host would run (on Unix, with an
    /// execute bit), as `tool_commands::executable` decides.
    ///
    /// # Safety
    /// `path` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_executable(path: *const u8, len: usize) -> u32 {
        // SAFETY: lent for the call; the bytes came from the OS (the
        // executable's directory or PATH) or from the command line.
        let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, len)) };
        let Ok(meta) = Path::new(path).metadata() else {
            return 0;
        };
        #[cfg(unix)]
        let ok = {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let ok = meta.is_file();
        u32::from(ok)
    }

    /// Run `vesper ebpf check` on `path`, printing verification/stack-bound
    /// output to stdout as `src/ebpf_commands.rs` does. Returns `0` on success
    /// or `err.len()` (`> 0`) on error, storing the error bytes in `LAST_EBPF_ERR`.
    ///
    /// # Safety
    /// `path` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_ebpf_check(
        path: *const u8,
        len: usize,
        load: bool,
        disassemble: bool,
        budget: u64,
    ) -> u64 {
        // SAFETY: lent for the call.
        let path =
            Path::new(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, len)) });
        let budget = u32::try_from(budget).unwrap_or(u32::MAX);
        match ebpf_check_impl(path, load, disassemble, budget) {
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

    fn ebpf_check_impl(
        input: &Path,
        load: bool,
        disassemble: bool,
        budget: u32,
    ) -> Result<(), String> {
        use vesper::stack_bound;
        use vesper_ebpf::{VerifierPolicy, insn};
        let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let prog = ebpf_program(input, &bytes)?;
        if disassemble {
            print!("{}", insn::disassemble(&prog.insns));
        }
        let report = match vesper_ebpf::verify(&prog, &VerifierPolicy::default()) {
            Ok(report) => report,
            Err(e) => {
                for f in vesper_ebpf::findings(&prog, &VerifierPolicy::default()) {
                    let at = f.inst.map_or_else(String::new, |i| format!(" at insn {i}"));
                    println!("error[{}]: {}{at}: {}", f.rule, f.function, f.message);
                }
                return Err(format!("ebpf check: '{}' rejected ({})", prog.name, e.rule));
            }
        };
        println!(
            "ebpf check: '{}' accepted ({} insn(s) processed, {} path(s), worst stack {} bytes)",
            prog.name, report.insns_processed, report.paths_completed, report.worst_stack
        );
        let graph = stack_bound::StackGraph::from(&report.call_graph);
        let root = graph
            .functions
            .first()
            .map(|f| f.name.clone())
            .ok_or("ebpf check: the verifier returned an empty call graph")?;
        let bound = stack_bound::stack_bound_report(
            &graph,
            &stack_bound::StackBoundPolicy {
                roots: vec![stack_bound::StackBudget {
                    function: root,
                    budget_bytes: budget,
                }],
                ..Default::default()
            },
        )?;
        print!("{}", stack_bound::render(&bound));
        for f in &bound.findings {
            println!("error[{}]: {}: {}", f.rule, f.function, f.message);
        }
        if !bound.accepted() {
            return Err("ebpf check: stack bound exceeded".into());
        }
        if load {
            ebpf_kernel_load(&prog)?;
        }
        Ok(())
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
}
