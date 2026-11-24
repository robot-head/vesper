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
    static LAST_GRAPH_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static FMT_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
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

    /// One file of `vesper fmt` (`fmt_commands::format_file`: the bounded
    /// read, the formatter and the checked atomic replace). Flag 1 is
    /// `--check`, 2 `--recovery`. The status is 0 unchanged, 1 formatted,
    /// 2 needs formatting or 3 failed (`hc_fmt_error` has the error);
    /// `stats` gets changed, input bytes, output bytes, whether there is an
    /// edit count, and the edit count.
    ///
    /// # Safety
    /// `path` is a lent byte slice and `stats` a lent writable slice of
    /// `slen` words.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_fmt_file(
        path: *const u8,
        len: usize,
        flags: u32,
        stats: *mut u64,
        slen: usize,
    ) -> u32 {
        // SAFETY: lent for the call; the bytes came from the command line
        // or from a directory listing.
        let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, len)) };
        let out: &mut [u64] = if slen == 0 || stats.is_null() {
            &mut []
        } else {
            // SAFETY: as documented.
            unsafe { std::slice::from_raw_parts_mut(stats, slen) }
        };
        out.iter_mut().for_each(|w| *w = 0);
        let report =
            match crate::fmt_commands::format_file(Path::new(path), flags & 1 != 0, flags & 2 != 0)
            {
                Ok(report) => report,
                Err(error) => {
                    super::FMT_ERROR.with(|e| *e.borrow_mut() = error);
                    return 3;
                }
            };
        let word = |key: &str| report[key].as_u64();
        let values = [
            u64::from(report["changed"].as_bool().unwrap_or(false)),
            word("input_bytes").unwrap_or(0),
            word("output_bytes").unwrap_or(0),
            u64::from(word("edits").is_some()),
            word("edits").unwrap_or(0),
        ];
        for (slot, v) in out.iter_mut().zip(values) {
            *slot = v;
        }
        match report["status"].as_str() {
            Some("formatted") => 1,
            Some("needs_format") => 2,
            _ => 0,
        }
    }

    /// The error of the last failed `hc_fmt_file`.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_fmt_error(buf: *mut u8, len: usize) -> i64 {
        super::FMT_ERROR.with(|e| {
            let e = e.borrow();
            // SAFETY: lent for the call.
            unsafe { copy_out(Some(e.as_bytes()), buf, len) }
        })
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

    /// Run `vesper graph` on `input`, returning `0` on success or `err.len()` (`> 0`)
    /// on error with `LAST_GRAPH_ERR` set.
    ///
    /// # Safety
    /// `input` and `focus` are lent byte slices.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_graph_run(
        input: *const u8,
        input_len: usize,
        has_focus: bool,
        focus: *const u8,
        focus_len: usize,
        invert: bool,
        json: bool,
        dot: bool,
    ) -> u64 {
        // SAFETY: lent for the call.
        let input = String::from_utf8_lossy(unsafe { bytes(input, input_len) }).into_owned();
        let focus = has_focus
            .then(|| String::from_utf8_lossy(unsafe { bytes(focus, focus_len) }).into_owned());
        match graph_run_impl(&input, focus.as_deref(), invert, json, dot) {
            Ok(()) => 0,
            Err(err) => {
                let bytes = err.into_bytes();
                let n = bytes.len() as u64;
                super::LAST_GRAPH_ERR.with(|cell| *cell.borrow_mut() = bytes);
                n
            }
        }
    }

    /// Copy `LAST_GRAPH_ERR` into `buf` (copy-out protocol).
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_graph_err(buf: *mut u8, len: usize) -> u64 {
        super::LAST_GRAPH_ERR.with(|cell| {
            let data = cell.borrow();
            if data.len() <= len && !buf.is_null() && !data.is_empty() {
                // SAFETY: `buf` holds `len >= data.len()` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
            }
            data.len() as u64
        })
    }

    fn graph_run_impl(
        input: &str,
        focus_raw: Option<&str>,
        invert: bool,
        json: bool,
        dot: bool,
    ) -> Result<(), String> {
        use serde::Serialize;
        use std::collections::{BTreeMap, BTreeSet, VecDeque};
        use std::fmt::Write as _;
        use std::fs;
        use vesper_loom_calculus::{
            Diagnostic, PackageGraph, PackageName, Solution, Validation, validate_solution,
        };

        let focus = match focus_raw {
            Some(name) => Some(PackageName::parse(name)?),
            None => None,
        };
        if json && dot {
            return Err("--json and --dot cannot be combined".into());
        }

        fn semantic_error(
            json: bool,
            kind: &str,
            message: String,
            diagnostics: Vec<Diagnostic>,
        ) -> Result<(), String> {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "status": "error",
                        "error": {"kind": kind, "message": &message, "diagnostics": diagnostics}
                    }))
                    .map_err(|e| e.to_string())?
                );
            }
            Err(message)
        }

        #[derive(Serialize)]
        struct PackageSummary {
            version: String,
            features: BTreeSet<String>,
            capability_ceiling: BTreeSet<String>,
            required_capabilities: BTreeSet<String>,
            forbid_unsafe_ffi: bool,
            is_root: bool,
        }
        #[derive(Serialize)]
        struct Dependency {
            dependent: PackageName,
            dependency: PackageName,
            requirement: String,
            allowed_capabilities: BTreeSet<String>,
            requested_features: BTreeSet<String>,
        }
        #[derive(Serialize)]
        struct SystemDependency {
            package: PackageName,
            system: String,
            requirement: String,
        }
        #[derive(Serialize)]
        struct GraphReport {
            schema_version: u32,
            status: &'static str,
            basis: &'static str,
            direction: &'static str,
            focus: Option<PackageName>,
            snapshot_roots: BTreeSet<PackageName>,
            packages: BTreeMap<PackageName, PackageSummary>,
            system_packages: BTreeMap<String, String>,
            dependencies: Vec<Dependency>,
            system_dependencies: Vec<SystemDependency>,
            unused_packages: BTreeSet<PackageName>,
            unused_system_packages: BTreeSet<String>,
        }
        fn list(values: &BTreeSet<String>) -> String {
            format!(
                "[{}]",
                values.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        }
        fn quote(text: &str) -> String {
            serde_json::to_string(text).expect("serializing a string is infallible")
        }
        fn project(
            graph: &PackageGraph,
            focus: Option<PackageName>,
            invert: bool,
            unused_packages: BTreeSet<PackageName>,
            unused_system_packages: BTreeSet<String>,
        ) -> Result<GraphReport, String> {
            let visible = match &focus {
                None => graph.packages.keys().cloned().collect(),
                Some(focus) => {
                    let mut adjacent = BTreeMap::<PackageName, BTreeSet<PackageName>>::new();
                    for (name, node) in &graph.packages {
                        for edge in &node.dependencies {
                            let (from, to) = if invert {
                                (&edge.package, name)
                            } else {
                                (name, &edge.package)
                            };
                            adjacent.entry(from.clone()).or_default().insert(to.clone());
                        }
                    }
                    let mut visible = BTreeSet::new();
                    let mut queue = VecDeque::from([focus.clone()]);
                    while let Some(name) = queue.pop_front() {
                        if visible.insert(name.clone())
                            && let Some(neighbors) = adjacent.get(&name)
                        {
                            queue.extend(neighbors.iter().cloned());
                        }
                    }
                    visible
                }
            };
            let mut report = GraphReport {
                schema_version: 1,
                status: "graph",
                basis: "declared_metadata",
                direction: if invert { "dependents" } else { "dependencies" },
                focus,
                snapshot_roots: graph.roots.clone(),
                packages: BTreeMap::new(),
                system_packages: BTreeMap::new(),
                dependencies: vec![],
                system_dependencies: vec![],
                unused_packages,
                unused_system_packages,
            };
            for (name, node) in &graph.packages {
                if !visible.contains(name) {
                    continue;
                }
                report.packages.insert(
                    name.clone(),
                    PackageSummary {
                        version: node.version.to_string(),
                        features: node.features.clone(),
                        capability_ceiling: node.capability_ceiling.clone(),
                        required_capabilities: node.required_capabilities.clone(),
                        forbid_unsafe_ffi: node.forbid_unsafe_ffi,
                        is_root: graph.roots.contains(name),
                    },
                );
                for edge in &node.dependencies {
                    if visible.contains(&edge.package) {
                        report.dependencies.push(Dependency {
                            dependent: name.clone(),
                            dependency: edge.package.clone(),
                            requirement: edge.requirement.to_string(),
                            allowed_capabilities: edge.allowed_capabilities.clone(),
                            requested_features: edge.requested_features.clone(),
                        });
                    }
                }
                for edge in &node.system_dependencies {
                    let version = graph.system_packages.get(&edge.name).ok_or_else(|| {
                        format!("validated graph is missing native package '{}'", edge.name)
                    })?;
                    report
                        .system_packages
                        .insert(edge.name.clone(), version.to_string());
                    report.system_dependencies.push(SystemDependency {
                        package: name.clone(),
                        system: edge.name.clone(),
                        requirement: edge.requirement.clone(),
                    });
                }
            }
            Ok(report)
        }
        fn render_text(report: &GraphReport) -> String {
            let mut out = format!("Package graph (declared metadata; {})\n", report.direction);
            if let Some(focus) = &report.focus {
                let _ = writeln!(out, "focus: {}", focus.as_str());
            }
            out.push_str(
                "Capability requirements describe the full validated snapshot, including when filtered.\n",
            );
            for (name, node) in &report.packages {
                let _ = writeln!(
                    out,
                    "{} {}{}",
                    name.as_str(),
                    node.version,
                    if node.is_root { " [root]" } else { "" }
                );
                let _ = writeln!(out, "  features: {}", list(&node.features));
                let _ = writeln!(
                    out,
                    "  declared ceiling: {}",
                    list(&node.capability_ceiling)
                );
                let _ = writeln!(
                    out,
                    "  required capabilities: {}",
                    list(&node.required_capabilities)
                );
                let _ = writeln!(out, "  forbid unsafe FFI: {}", node.forbid_unsafe_ffi);
            }
            out.push_str("Dependency constraints (dependent -> dependency):\n");
            for edge in &report.dependencies {
                let _ = writeln!(
                    out,
                    "  {} -> {}: {}; ceiling {}; requested features {}",
                    edge.dependent.as_str(),
                    edge.dependency.as_str(),
                    edge.requirement,
                    list(&edge.allowed_capabilities),
                    list(&edge.requested_features)
                );
            }
            for edge in &report.system_dependencies {
                let version = &report.system_packages[&edge.system];
                let _ = writeln!(
                    out,
                    "  {} -> native {} {}: {}",
                    edge.package.as_str(),
                    edge.system,
                    version,
                    edge.requirement
                );
            }
            if !report.unused_packages.is_empty() {
                let _ = writeln!(
                    out,
                    "Unreachable packages excluded: {}",
                    report
                        .unused_packages
                        .iter()
                        .map(PackageName::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !report.unused_system_packages.is_empty() {
                let _ = writeln!(
                    out,
                    "Unused native selections excluded: {}",
                    list(&report.unused_system_packages)
                );
            }
            out
        }
        fn render_dot(report: &GraphReport) -> String {
            let mut out = String::from("digraph vesper {\n");
            let _ = writeln!(
                out,
                "  graph [label={}];",
                quote(&format!("Declared metadata: {}", report.direction))
            );
            for (name, node) in &report.packages {
                let label = format!(
                    "{} {}{}\nfeatures: {}\ndeclared: {}\nrequired: {}",
                    name.as_str(),
                    node.version,
                    if node.is_root { " [root]" } else { "" },
                    list(&node.features),
                    list(&node.capability_ceiling),
                    list(&node.required_capabilities)
                );
                let _ = writeln!(
                    out,
                    "  {} [shape=box, label={}, peripheries={}];",
                    quote(&format!("pkg:{}", name.as_str())),
                    quote(&label),
                    if node.is_root { 2 } else { 1 }
                );
            }
            for (name, version) in &report.system_packages {
                let _ = writeln!(
                    out,
                    "  {} [shape=ellipse, label={}];",
                    quote(&format!("sys:{name}")),
                    quote(&format!("native {name} {version}"))
                );
            }
            let invert = report.direction == "dependents";
            for edge in &report.dependencies {
                let dependent = format!("pkg:{}", edge.dependent.as_str());
                let dependency = format!("pkg:{}", edge.dependency.as_str());
                let (from, to) = if invert {
                    (&dependency, &dependent)
                } else {
                    (&dependent, &dependency)
                };
                let label = format!(
                    "{} requires {}\nrequirement: {}\nceiling: {}\nrequested features: {}",
                    edge.dependent.as_str(),
                    edge.dependency.as_str(),
                    edge.requirement,
                    list(&edge.allowed_capabilities),
                    list(&edge.requested_features)
                );
                let _ = writeln!(
                    out,
                    "  {} -> {} [label={}];",
                    quote(from),
                    quote(to),
                    quote(&label)
                );
            }
            for edge in &report.system_dependencies {
                let package = format!("pkg:{}", edge.package.as_str());
                let system = format!("sys:{}", edge.system);
                let (from, to) = if invert {
                    (&system, &package)
                } else {
                    (&package, &system)
                };
                let label = format!(
                    "{} requires native {}\nrequirement: {}",
                    edge.package.as_str(),
                    edge.system,
                    edge.requirement
                );
                let _ = writeln!(
                    out,
                    "  {} -> {} [label={}];",
                    quote(from),
                    quote(to),
                    quote(&label)
                );
            }
            out.push_str("}\n");
            out
        }

        let bytes = fs::read(input).map_err(|e| format!("{input}: {e}"))?;
        let solution: Solution = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{input}: invalid lockfile: {e}"))?;
        let Validation {
            graph,
            diagnostics,
            unused_selections,
        } = validate_solution(&solution);
        if solution.roots.is_empty() || !diagnostics.is_empty() || graph.is_none() {
            let message = if solution.roots.is_empty() {
                "snapshot must declare at least one root".into()
            } else {
                format!(
                    "invalid snapshot: {} validation diagnostic(s)",
                    diagnostics.len()
                )
            };
            return semantic_error(json, "invalid_snapshot", message, diagnostics);
        }
        let Some(graph) = graph else {
            return Err("snapshot validation did not produce a graph".into());
        };
        if let Some(focus) = &focus
            && !graph.packages.contains_key(focus)
        {
            return semantic_error(
                json,
                "unknown_package",
                format!(
                    "package '{}' is not reachable from the snapshot roots",
                    focus.as_str()
                ),
                vec![],
            );
        }
        let unused_systems = solution
            .system_packages
            .iter()
            .filter(|selection| !graph.system_packages.contains_key(&selection.name))
            .map(|selection| selection.name.clone())
            .collect();
        let report = project(&graph, focus, invert, unused_selections, unused_systems)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
        } else if dot {
            print!("{}", render_dot(&report));
        } else {
            print!("{}", render_text(&report));
        }
        Ok(())
    }
}
