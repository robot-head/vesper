//! Native executables for Windows x64 (`x86_64-pc-windows-msvc`, §7, §12).
//!
//! Two halves:
//!
//! * **Objects.** [`object`] compiles the same checked program as
//!   [`super::object_instrumented`] into a COFF object. Cranelift picks the
//!   Microsoft x64 calling convention (`WindowsFastcall`) from the triple.
//!   `cranelift-object` writes no unwind tables, so every defined function's
//!   x64 `UNWIND_INFO` (from Cranelift's own prologue analysis) goes into
//!   `.xdata`, and one `RUNTIME_FUNCTION` per function into `.pdata`, with
//!   `IMAGE_REL_AMD64_ADDR32NB` relocations. With those tables the Windows
//!   exception dispatcher, `RtlVirtualUnwind`, debuggers and the wave-six
//!   fiber fault handler can walk through generated frames.
//! * **Linking.** [`link_executable`] links the object with `link.exe` (found
//!   through the Visual Studio installation, with its `LIB` paths) or with
//!   `lld-link`, against the static MSVC C runtime (`libcmt`, `libucrt`,
//!   `libvcruntime`) and `kernel32`. The CRT's `mainCRTStartup` calls the
//!   exported `main`; its integer result becomes the process exit code.
//!
//! The object half is target-independent and runs on any host; linking and
//! running need a Windows host or an `lld-link` with the MSVC libraries.

use super::{EmittedFunction, Instrument};
use crate::check::World;
use cranelift_codegen::{
    CodegenError, Context,
    control::ControlPlane,
    ir,
    isa::{TargetIsa, unwind::UnwindInfo},
};
use cranelift_module::{
    DataDescription, DataId, FuncId, Linkage, Module, ModuleDeclarations, ModuleError, ModuleReloc,
    ModuleResult,
};
use cranelift_object::{
    ObjectBuilder, ObjectModule, ObjectProduct,
    object::{
        self, SectionKind,
        write::{Relocation, SymbolSection},
    },
};
use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

/// The only Windows target this backend emits.
pub const MSVC_TARGET: &str = "x86_64-pc-windows-msvc";

/// The static MSVC runtime and system libraries every executable links.
pub const RUNTIME_LIBRARIES: [&str; 4] = [
    "libcmt.lib",
    "libucrt.lib",
    "libvcruntime.lib",
    "kernel32.lib",
];

/// A typed failure of Windows object emission or linking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsNativeError {
    /// A Windows triple this backend does not support (AArch64, GNU, ...).
    UnsupportedTarget(String),
    /// The target triple did not parse.
    InvalidTarget(String),
    /// Code generation failed before any unwind information was needed.
    Codegen(String),
    /// A function had machine code but no usable x64 unwind information.
    Unwind { function: String, reason: String },
    /// No MSVC-compatible linker was found.
    LinkerNotFound(String),
    /// The linker ran and failed.
    LinkerFailed {
        linker: String,
        status: Option<i32>,
        output: String,
    },
    /// Running the linker or touching a file failed.
    Io(String),
}

impl fmt::Display for WindowsNativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget(t) => write!(
                f,
                "unsupported Windows target '{t}': native Windows executables support only {MSVC_TARGET}"
            ),
            Self::InvalidTarget(e) => write!(f, "invalid target: {e}"),
            Self::Codegen(e) => write!(f, "{e}"),
            Self::Unwind { function, reason } => {
                write!(
                    f,
                    "no Windows x64 unwind information for {function}: {reason}"
                )
            }
            Self::LinkerNotFound(e) => write!(f, "no MSVC linker found: {e}"),
            Self::LinkerFailed {
                linker,
                status,
                output,
            } => {
                write!(f, "native linker '{linker}' failed")?;
                if let Some(code) = status {
                    write!(f, " with exit code {code}")?;
                }
                if !output.trim().is_empty() {
                    write!(f, ":\n{}", output.trim_end())?;
                }
                Ok(())
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WindowsNativeError {}

impl From<WindowsNativeError> for String {
    fn from(e: WindowsNativeError) -> Self {
        e.to_string()
    }
}

/// Whether `target` (or the host, for `None`) names a Windows operating
/// system. Every Windows target goes through this module, so the ones it
/// cannot build are rejected with a typed error instead of a wrong object.
pub fn is_windows_target(target: Option<&str>) -> bool {
    match target {
        Some(t) => t
            .parse::<target_lexicon::Triple>()
            .map(|t| is_windows(&t))
            .unwrap_or(false),
        None => cfg!(windows),
    }
}

/// Whether `triple` names a Windows operating system.
pub fn is_windows(triple: &target_lexicon::Triple) -> bool {
    triple.operating_system == target_lexicon::OperatingSystem::Windows
}

/// Accept exactly `x86_64-*-windows-msvc`.
pub fn check_target(triple: &target_lexicon::Triple) -> Result<(), WindowsNativeError> {
    use target_lexicon::{Architecture, BinaryFormat, Environment};
    if triple.architecture == Architecture::X86_64
        && triple.environment == Environment::Msvc
        && triple.binary_format == BinaryFormat::Coff
        && is_windows(triple)
    {
        Ok(())
    } else {
        Err(WindowsNativeError::UnsupportedTarget(triple.to_string()))
    }
}

/// Parse and check a Windows target string.
pub fn parse_target(target: &str) -> Result<target_lexicon::Triple, WindowsNativeError> {
    let triple: target_lexicon::Triple = target
        .parse()
        .map_err(|e| WindowsNativeError::InvalidTarget(format!("{e}")))?;
    check_target(&triple)?;
    Ok(triple)
}

/// One defined function's code range and unwind information.
struct Unwound {
    id: FuncId,
    name: String,
    info: Option<cranelift_codegen::isa::unwind::winx64::UnwindInfo>,
}

/// An [`ObjectModule`] that keeps the Windows x64 unwind information of every
/// function it compiles. Every other call is forwarded unchanged, so the
/// emitted code is exactly what [`super::object_instrumented`] produces for
/// the same ISA.
pub(super) struct UnwindModule {
    inner: ObjectModule,
    functions: Vec<Unwound>,
}

impl Module for UnwindModule {
    fn isa(&self) -> &dyn TargetIsa {
        self.inner.isa()
    }
    fn declarations(&self) -> &ModuleDeclarations {
        self.inner.declarations()
    }
    fn declare_function(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &ir::Signature,
    ) -> ModuleResult<FuncId> {
        self.inner.declare_function(name, linkage, signature)
    }
    fn declare_anonymous_function(&mut self, signature: &ir::Signature) -> ModuleResult<FuncId> {
        self.inner.declare_anonymous_function(signature)
    }
    fn declare_data(
        &mut self,
        name: &str,
        linkage: Linkage,
        writable: bool,
        tls: bool,
    ) -> ModuleResult<DataId> {
        self.inner.declare_data(name, linkage, writable, tls)
    }
    fn declare_anonymous_data(&mut self, writable: bool, tls: bool) -> ModuleResult<DataId> {
        self.inner.declare_anonymous_data(writable, tls)
    }
    fn define_function_with_control_plane(
        &mut self,
        func: FuncId,
        ctx: &mut Context,
        ctrl_plane: &mut ControlPlane,
    ) -> ModuleResult<()> {
        self.inner
            .define_function_with_control_plane(func, ctx, ctrl_plane)?;
        let name = self
            .inner
            .declarations()
            .get_function_decl(func)
            .linkage_name(func)
            .into_owned();
        let compiled = ctx.compiled_code().ok_or_else(|| {
            ModuleError::Compilation(CodegenError::Unsupported(format!(
                "{name}: no compiled code after definition"
            )))
        })?;
        let info = match compiled.create_unwind_info(self.inner.isa())? {
            Some(UnwindInfo::WindowsX64(info)) => Some(info),
            None => None,
            Some(_) => {
                return Err(ModuleError::Compilation(CodegenError::Unsupported(
                    format!("{name}: Cranelift produced non-Windows unwind information"),
                )));
            }
        };
        self.functions.push(Unwound {
            id: func,
            name,
            info,
        });
        Ok(())
    }
    fn define_function_bytes(
        &mut self,
        func_id: FuncId,
        _alignment: u64,
        _bytes: &[u8],
        _relocs: &[ModuleReloc],
    ) -> ModuleResult<()> {
        // Raw bytes carry no prologue description, so their frames could not
        // be unwound. The native backend never defines functions this way.
        Err(ModuleError::Compilation(CodegenError::Unsupported(
            format!("function {func_id} defined from raw bytes has no Windows unwind information"),
        )))
    }
    fn define_data(&mut self, data_id: DataId, data: &DataDescription) -> ModuleResult<()> {
        self.inner.define_data(data_id, data)
    }
}

/// Size in bytes of one `RUNTIME_FUNCTION` (`BeginAddress`, `EndAddress`,
/// `UnwindData`, each an image-relative `u32`).
pub const RUNTIME_FUNCTION_SIZE: usize = 12;

/// Write `.xdata` (one `UNWIND_INFO` per function) and `.pdata` (one
/// `RUNTIME_FUNCTION` per function, sorted by address) into `product`.
fn write_unwind_tables(
    product: &mut ObjectProduct,
    functions: &[Unwound],
) -> Result<usize, WindowsNativeError> {
    let mut rows = Vec::new();
    for f in functions {
        let Some((symbol, true)) = product.functions[f.id] else {
            return Err(WindowsNativeError::Unwind {
                function: f.name.clone(),
                reason: "function symbol is not defined in the object".into(),
            });
        };
        let sym = product.object.symbol(symbol);
        let SymbolSection::Section(section) = sym.section else {
            return Err(WindowsNativeError::Unwind {
                function: f.name.clone(),
                reason: "function symbol has no section".into(),
            });
        };
        let (start, size) = (sym.value, sym.size);
        if size == 0 || size > u64::from(u32::MAX) {
            return Err(WindowsNativeError::Unwind {
                function: f.name.clone(),
                reason: format!("invalid code size {size}"),
            });
        }
        match &f.info {
            Some(info) => rows.push((section, start, symbol, size, info, f.name.as_str())),
            // Cranelift only omits unwind information for frameless leaf
            // functions, which Windows unwinds without a table entry.
            None => continue,
        }
    }
    if rows.is_empty() {
        return Ok(0);
    }
    rows.sort_by_key(|r| (r.0, r.1));
    let obj = &mut product.object;
    let xdata = obj.add_section(Vec::new(), b".xdata".to_vec(), SectionKind::ReadOnlyData);
    let pdata = obj.add_section(Vec::new(), b".pdata".to_vec(), SectionKind::ReadOnlyData);
    let xdata_symbol = obj.section_symbol(xdata);
    let addr32nb = object::RelocationFlags::Coff {
        typ: object::pe::IMAGE_REL_AMD64_ADDR32NB,
    };
    for (_, _, symbol, size, info, name) in &rows {
        let mut bytes = vec![0; info.emit_size()];
        info.emit(&mut bytes);
        let unwind_offset = obj.append_section_data(xdata, &bytes, 4);
        let row = obj.append_section_data(pdata, &[0; RUNTIME_FUNCTION_SIZE], 4);
        let relocations = [
            (row, *symbol, 0),
            (row + 4, *symbol, *size as i64),
            (row + 8, xdata_symbol, unwind_offset as i64),
        ];
        for (offset, symbol, addend) in relocations {
            obj.add_relocation(
                pdata,
                Relocation {
                    offset,
                    symbol,
                    addend,
                    flags: addr32nb,
                },
            )
            .map_err(|e| WindowsNativeError::Unwind {
                function: (*name).to_string(),
                reason: e.to_string(),
            })?;
        }
    }
    Ok(rows.len())
}

/// Compile `entry` to a Windows x64 COFF object with unwind tables. `isa`
/// must be the object ISA for an `x86_64-*-windows-msvc` triple
/// ([`super::object_isa_for`]); the observer and instrument hooks behave
/// exactly as in [`super::object_instrumented`].
pub(super) fn object(
    world: &World,
    entry: &str,
    isa: Arc<dyn TargetIsa>,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
) -> Result<(Vec<u8>, String), String> {
    emit_with(isa, "vesper", |m| {
        Ok(super::compile(m, world, entry, optimize, observer, instrument)?.1)
    })
}

/// Run `compile` against a COFF [`ObjectModule`] for `isa` that records
/// each defined function's unwind information, then write the `.pdata` and
/// `.xdata` tables and emit the object. `compile` returns the CLIF text.
/// Native session objects ([`super::session::object`]) use this too.
pub(super) fn emit_with(
    isa: Arc<dyn TargetIsa>,
    name: &str,
    compile: impl FnOnce(&mut UnwindModule) -> Result<String, String>,
) -> Result<(Vec<u8>, String), String> {
    check_target(isa.triple())?;
    let builder = ObjectBuilder::new(isa, name, cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = UnwindModule {
        inner: ObjectModule::new(builder),
        functions: Vec::new(),
    };
    let clif = compile(&mut m)?;
    let UnwindModule { inner, functions } = m;
    let mut product = inner.finish();
    write_unwind_tables(&mut product, &functions)?;
    let bytes = product.emit().map_err(|e| e.to_string())?;
    Ok((bytes, clif))
}

/// Which command-line dialect a linker speaks. Both take MSVC-style
/// options; `rust-lld` additionally needs `-flavor link` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkerFlavor {
    /// Microsoft `link.exe`.
    Msvc,
    /// LLVM `lld-link`.
    LldLink,
    /// `rust-lld` (the LLVM linker shipped with Rust) in its `link` flavor.
    RustLld,
}

/// A resolved linker invocation: program, flavor and the environment
/// (`LIB`, `PATH`) its Visual Studio installation needs.
#[derive(Debug, Clone)]
pub struct Linker {
    pub program: PathBuf,
    pub flavor: LinkerFlavor,
    pub env: Vec<(OsString, OsString)>,
}

impl Linker {
    /// A linker named on the command line. The flavor comes from the file
    /// name; anything that is not `lld-link` or `rust-lld` is treated as
    /// `link.exe`-compatible.
    pub fn explicit(program: &Path) -> Self {
        let stem = program
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let flavor = match stem.as_str() {
            "lld-link" => LinkerFlavor::LldLink,
            "rust-lld" => LinkerFlavor::RustLld,
            _ => LinkerFlavor::Msvc,
        };
        Self {
            program: program.to_path_buf(),
            flavor,
            env: Vec::new(),
        }
    }

    /// Find a linker: the Visual Studio `link.exe` for x64 (with its library
    /// environment), else `lld-link` on `PATH`. A bare `link` on `PATH` is
    /// never used, because Git for Windows and MSYS put coreutils `link`
    /// there.
    pub fn find() -> Result<Self, WindowsNativeError> {
        if let Some(linker) = find_visual_studio_link() {
            return Ok(linker);
        }
        for name in ["lld-link", "lld-link.exe"] {
            if let Some(path) = find_on_path(name) {
                return Ok(Self {
                    program: path,
                    flavor: LinkerFlavor::LldLink,
                    env: Vec::new(),
                });
            }
        }
        Err(WindowsNativeError::LinkerNotFound(
            "install the Visual Studio C++ build tools (x64), put lld-link on PATH, or pass --linker <path>".into(),
        ))
    }

    /// The command that links `objects` into `output`.
    pub fn command(&self, objects: &[PathBuf], output: &Path) -> Command {
        let mut command = Command::new(&self.program);
        if self.flavor == LinkerFlavor::RustLld {
            command.args(["-flavor", "link"]);
        }
        command.args(link_args(objects, output));
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
}

#[cfg(windows)]
fn find_visual_studio_link() -> Option<Linker> {
    let tool = find_msvc_tools::find_tool(MSVC_TARGET, "link.exe")?;
    if tool.is_clang_cl() {
        return None;
    }
    let command = tool.to_command();
    Some(Linker {
        program: PathBuf::from(command.get_program()),
        flavor: LinkerFlavor::Msvc,
        env: command
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_os_string(), v?.to_os_string())))
            .collect(),
    })
}

#[cfg(not(windows))]
fn find_visual_studio_link() -> Option<Linker> {
    None
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// MSVC-style arguments that link `objects` into the console executable
/// `output` against [`RUNTIME_LIBRARIES`].
pub fn link_args(objects: &[PathBuf], output: &Path) -> Vec<OsString> {
    let mut out = OsString::from("/OUT:");
    out.push(output.as_os_str());
    let mut args: Vec<OsString> = [
        "/NOLOGO",
        "/SUBSYSTEM:CONSOLE",
        "/MACHINE:X64",
        "/DEBUG:NONE",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(out);
    args.extend(objects.iter().map(|o| o.as_os_str().to_os_string()));
    args.extend(RUNTIME_LIBRARIES.iter().map(OsString::from));
    args
}

/// The executable path `vesper build -o <output>` writes on Windows: the
/// given path, with `.exe` added when it has no extension.
pub fn executable_path(output: &Path) -> PathBuf {
    if output.extension().is_none() {
        output.with_extension("exe")
    } else {
        output.to_path_buf()
    }
}

/// Link `object` into a Windows console executable at
/// [`executable_path`]`(output)`, using `linker` when given and
/// [`Linker::find`] otherwise. Returns the executable path.
pub fn link_executable(
    object: &Path,
    output: &Path,
    linker: Option<&Path>,
) -> Result<PathBuf, WindowsNativeError> {
    link(&[object.to_path_buf()], output, linker)
}

/// [`link_executable`] for several objects (for example a generated object
/// and a host harness or runtime object).
pub fn link(
    objects: &[PathBuf],
    output: &Path,
    linker: Option<&Path>,
) -> Result<PathBuf, WindowsNativeError> {
    let linker = match linker {
        Some(path) => Linker::explicit(path),
        None => Linker::find()?,
    };
    let output = executable_path(output);
    let result = linker.command(objects, &output).output().map_err(|e| {
        WindowsNativeError::Io(format!(
            "could not invoke native linker '{}': {e}",
            linker.program.display()
        ))
    })?;
    if !result.status.success() {
        let mut text = String::from_utf8_lossy(&result.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&result.stderr));
        return Err(WindowsNativeError::LinkerFailed {
            linker: linker.program.display().to_string(),
            status: result.status.code(),
            output: text,
        });
    }
    Ok(output)
}

/// What [`inspect`] found in a COFF object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoffSummary {
    /// `IMAGE_FILE_HEADER::Machine`.
    pub machine: u16,
    /// Section names in header order.
    pub sections: Vec<String>,
    /// `RUNTIME_FUNCTION` entries in `.pdata`.
    pub runtime_functions: usize,
    /// Relocations on `.pdata`, all `IMAGE_REL_AMD64_ADDR32NB`.
    pub pdata_relocations: usize,
    /// `UNWIND_INFO` records in `.xdata`, each version 1 without handlers.
    pub unwind_infos: usize,
    /// Defined external function symbols (`main`, ...).
    pub external_functions: Vec<String>,
}

/// Parse the parts of a COFF object that matter for Windows x64 unwinding
/// and check them: the machine is AMD64, `.pdata` is a whole number of
/// `RUNTIME_FUNCTION`s each covered by three `ADDR32NB` relocations, and
/// `.xdata` is a sequence of well-formed version-1 `UNWIND_INFO` records.
/// Malformed bytes are an error, never a panic.
pub fn inspect(bytes: &[u8]) -> Result<CoffSummary, String> {
    fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
        b.get(at..at + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .ok_or_else(|| format!("truncated COFF at {at}"))
    }
    fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| format!("truncated COFF at {at}"))
    }
    let machine = u16_at(bytes, 0)?;
    if machine != object::pe::IMAGE_FILE_MACHINE_AMD64 {
        return Err(format!("COFF machine {machine:#x} is not AMD64"));
    }
    let count = usize::from(u16_at(bytes, 2)?);
    let symtab = u32_at(bytes, 8)? as usize;
    let symbols = u32_at(bytes, 12)? as usize;
    let optional = usize::from(u16_at(bytes, 16)?);
    let strings = symtab
        .checked_add(symbols.checked_mul(18).ok_or("symbol count overflow")?)
        .ok_or("symbol table overflow")?;
    let name_of = |raw: &[u8]| -> Result<String, String> {
        if raw[..4] == [0, 0, 0, 0] {
            let offset = u32_at(raw, 4)? as usize;
            let start = strings
                .checked_add(offset)
                .ok_or("string offset overflow")?;
            let tail = bytes.get(start..).ok_or("string table truncated")?;
            let end = tail
                .iter()
                .position(|&c| c == 0)
                .ok_or("unterminated name")?;
            Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
        } else {
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
    };
    let section_name = |raw: &[u8]| -> Result<String, String> {
        if raw[0] == b'/' {
            let digits = std::str::from_utf8(&raw[1..])
                .map_err(|e| e.to_string())?
                .trim_end_matches('\0');
            let offset: usize = digits.parse().map_err(|_| "bad long section name")?;
            let start = strings
                .checked_add(offset)
                .ok_or("string offset overflow")?;
            let tail = bytes.get(start..).ok_or("string table truncated")?;
            let end = tail
                .iter()
                .position(|&c| c == 0)
                .ok_or("unterminated name")?;
            Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
        } else {
            let end = raw.iter().position(|&c| c == 0).unwrap_or(8);
            Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
    };
    let mut summary = CoffSummary {
        machine,
        sections: Vec::new(),
        runtime_functions: 0,
        pdata_relocations: 0,
        unwind_infos: 0,
        external_functions: Vec::new(),
    };
    for i in 0..count {
        let header = 20 + optional + i * 40;
        let raw = bytes
            .get(header..header + 40)
            .ok_or("section table truncated")?;
        let name = section_name(&raw[..8])?;
        let size = u32_at(raw, 16)? as usize;
        let data_at = u32_at(raw, 20)? as usize;
        let relocs_at = u32_at(raw, 24)? as usize;
        let relocs = usize::from(u16_at(raw, 32)?);
        let data = if size == 0 {
            &[][..]
        } else {
            bytes
                .get(data_at..data_at.checked_add(size).ok_or("section overflow")?)
                .ok_or_else(|| format!("section {name} data truncated"))?
        };
        match name.as_str() {
            ".pdata" => {
                if !size.is_multiple_of(RUNTIME_FUNCTION_SIZE) {
                    return Err(format!(".pdata size {size} is not a multiple of 12"));
                }
                summary.runtime_functions += size / RUNTIME_FUNCTION_SIZE;
                for r in 0..relocs {
                    let at = relocs_at + r * 10;
                    let offset = u32_at(bytes, at)? as usize;
                    let typ = u16_at(bytes, at + 8)?;
                    if typ != object::pe::IMAGE_REL_AMD64_ADDR32NB {
                        return Err(format!(".pdata relocation type {typ:#x} is not ADDR32NB"));
                    }
                    if !offset.is_multiple_of(4) || offset + 4 > size {
                        return Err(format!(".pdata relocation at {offset} is misplaced"));
                    }
                }
                summary.pdata_relocations += relocs;
            }
            ".xdata" => {
                let mut at = 0;
                while at < data.len() {
                    let header = data.get(at..at + 4).ok_or(".xdata record truncated")?;
                    if header[0] & 7 != 1 || header[0] >> 3 != 0 {
                        return Err(format!(
                            ".xdata record at {at}: version/flags {:#x}",
                            header[0]
                        ));
                    }
                    let nodes = usize::from(header[2]);
                    let len = 4 + 2 * (nodes + (nodes & 1));
                    if at + len > data.len() {
                        return Err(format!(".xdata record at {at} overruns the section"));
                    }
                    at += len;
                    summary.unwind_infos += 1;
                }
            }
            _ => {}
        }
        summary.sections.push(name);
    }
    if summary.pdata_relocations != summary.runtime_functions * 3 {
        return Err(format!(
            "{} RUNTIME_FUNCTIONs but {} relocations",
            summary.runtime_functions, summary.pdata_relocations
        ));
    }
    if summary.unwind_infos != summary.runtime_functions {
        return Err(format!(
            "{} RUNTIME_FUNCTIONs but {} UNWIND_INFOs",
            summary.runtime_functions, summary.unwind_infos
        ));
    }
    let mut index = 0;
    while index < symbols {
        let at = symtab + index * 18;
        let raw = bytes.get(at..at + 18).ok_or("symbol table truncated")?;
        let section = u16_at(raw, 12)?;
        let ty = u16_at(raw, 14)?;
        let class = raw[16];
        if class == object::pe::IMAGE_SYM_CLASS_EXTERNAL && section > 0 && ty & 0x30 == 0x20 {
            summary.external_functions.push(name_of(&raw[..8])?);
        }
        index += 1 + usize::from(raw[17]);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples `scripts/test-native-host.sh` builds on Linux and macOS,
    /// with the exit status each must return.
    const EXAMPLES: [(&str, &str, i64); 8] = [
        ("fibonacci", include_str!("../examples/fibonacci.ves"), 55),
        (
            "transaction",
            include_str!("../examples/transaction.ves"),
            42,
        ),
        (
            "projections",
            include_str!("../examples/projections.ves"),
            42,
        ),
        ("counter", include_str!("../examples/counter.ves"), 3),
        ("fail", include_str!("../examples/fail.ves"), 42),
        ("closures", include_str!("../examples/closures.ves"), 42),
        (
            "resumptions",
            include_str!("../examples/resumptions.ves"),
            42,
        ),
        (
            "checked_math",
            include_str!("../examples/checked_math.ves"),
            42,
        ),
    ];

    fn checked(source: &str) -> crate::check::Checked {
        let mut parsed = crate::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        crate::optimize::optimize(&mut parsed.module, &checked.expression_types);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked
    }

    fn windows_object(source: &str) -> Vec<u8> {
        let checked = checked(source);
        super::super::object_for_target(&checked.world, "main", Some(MSVC_TARGET))
            .expect("Windows object")
            .0
    }

    #[test]
    fn examples_emit_amd64_coff_with_unwind_tables() {
        for (name, source, _) in EXAMPLES {
            let bytes = windows_object(source);
            let summary = inspect(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(summary.machine, object::pe::IMAGE_FILE_MACHINE_AMD64);
            assert!(summary.sections.iter().any(|s| s == ".text"), "{name}");
            assert!(summary.sections.iter().any(|s| s == ".pdata"), "{name}");
            assert!(summary.sections.iter().any(|s| s == ".xdata"), "{name}");
            assert!(summary.runtime_functions >= 1, "{name}");
            assert_eq!(summary.external_functions, ["main"], "{name}");
        }
    }

    #[test]
    fn every_compiled_function_gets_a_runtime_function() {
        // `fib` and `main` both set up frames; both must be described.
        let checked = checked(EXAMPLES[0].1);
        let mut symbols = Vec::new();
        let mut observe = |f: EmittedFunction| symbols.push(f.symbol);
        let (bytes, _) = super::super::object_with_observer(
            &checked.world,
            "main",
            Some(MSVC_TARGET),
            true,
            Some(&mut observe),
        )
        .unwrap();
        let summary = inspect(&bytes).unwrap();
        assert!(symbols.len() >= 2, "{symbols:?}");
        assert_eq!(summary.runtime_functions, symbols.len(), "{symbols:?}");
    }

    #[test]
    fn host_objects_are_unchanged() {
        // The Linux/macOS object path keeps its format and gains no tables.
        let checked = checked(EXAMPLES[0].1);
        for target in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
            let (bytes, _) =
                super::super::object_for_target(&checked.world, "main", Some(target)).unwrap();
            assert!(inspect(&bytes).is_err(), "{target} is not COFF");
        }
    }

    #[test]
    fn unsupported_windows_targets_are_typed_errors() {
        let checked = checked(EXAMPLES[0].1);
        for target in [
            "aarch64-pc-windows-msvc",
            "x86_64-pc-windows-gnu",
            "i686-pc-windows-msvc",
        ] {
            assert!(is_windows_target(Some(target)));
            let error =
                super::super::object_for_target(&checked.world, "main", Some(target)).unwrap_err();
            assert!(
                error.contains("unsupported Windows target") || error.contains("must be x86_64"),
                "{target}: {error}"
            );
        }
        assert!(!is_windows_target(Some("x86_64-unknown-linux-gnu")));
        assert!(!is_windows_target(Some("not a triple")));
        assert!(matches!(
            parse_target("x86_64-pc-windows-gnu"),
            Err(WindowsNativeError::UnsupportedTarget(_))
        ));
        assert!(parse_target(MSVC_TARGET).is_ok());
    }

    #[test]
    fn malformed_objects_are_errors_not_panics() {
        let good = windows_object(EXAMPLES[0].1);
        for len in [0, 1, 19, 20, 60, good.len() / 2, good.len() - 1] {
            let _ = inspect(&good[..len]);
        }
        let mut bad = good.clone();
        bad[0] = 0x4c; // i386
        assert!(inspect(&bad).is_err());
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..2000 {
            let mut mutated = good.clone();
            for _ in 0..4 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let at = (rng as usize) % mutated.len();
                mutated[at] = (rng >> 32) as u8;
            }
            let _ = inspect(&mutated);
        }
    }

    #[test]
    fn link_arguments_name_the_static_runtime() {
        let args = link_args(&[PathBuf::from("a.vesper.o")], Path::new("a.exe"));
        let args: Vec<_> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"/OUT:a.exe".to_string()));
        assert!(args.contains(&"/SUBSYSTEM:CONSOLE".to_string()));
        assert!(args.contains(&"a.vesper.o".to_string()));
        for lib in RUNTIME_LIBRARIES {
            assert!(args.contains(&lib.to_string()), "{lib}");
        }
        assert_eq!(
            executable_path(Path::new("out/fib")),
            PathBuf::from("out/fib.exe")
        );
        assert_eq!(executable_path(Path::new("a.out")), PathBuf::from("a.out"));
        assert_eq!(
            Linker::explicit(Path::new("C:/llvm/bin/lld-link.exe")).flavor,
            LinkerFlavor::LldLink
        );
        assert_eq!(
            Linker::explicit(Path::new("/usr/bin/rust-lld")).flavor,
            LinkerFlavor::RustLld
        );
        let rust_lld = Linker::explicit(Path::new("rust-lld"));
        let command = rust_lld.command(&[PathBuf::from("x.o")], Path::new("x.exe"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[..2], ["-flavor", "link"]);
    }

    #[test]
    fn a_failing_linker_is_a_typed_error() {
        let dir = std::env::temp_dir().join(format!("vesper-win-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("x.vesper.o");
        std::fs::write(&object, windows_object(EXAMPLES[0].1)).unwrap();
        let missing = dir.join("no-such-linker");
        let error = link_executable(&object, &dir.join("x"), Some(&missing)).unwrap_err();
        assert!(matches!(error, WindowsNativeError::Io(_)), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const PULSE: &str = r#"
linear struct Ticket { id: u64 }
copy struct Block { lo: u64, hi: u64 }
copy enum Io { Got(u64), IoFailed(u64), IoCancelled }
copy enum Tick { Ticked, TickFailed(u32), TickCancelled }
linear effect Pulse {
    fn pause() -> Tick;
    fn read(inout buf: Block) -> Io;
}
fn main() -> u64 eff [Pulse] {
    let ticket = Ticket { id: 7 };
    let buf = Block { lo: 1, hi: 2 };
    let n: u64 = match Pulse::read(inout buf) { Got(n) => n, IoFailed(c) => c, IoCancelled => 0, };
    let t: u64 = match Pulse::pause() { Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, };
    let Ticket { id } = ticket;
    wrapping_add(wrapping_add(id, n), wrapping_add(t, buf.lo))
}
"#;

    #[test]
    fn pulse_session_objects_get_unwind_tables_and_park_through_the_fiber_abi() {
        use super::super::session::{
            Cancellation, EffectBinding, Operation, OperationBinding, entry_symbol,
        };
        let parsed = crate::parser::parse(PULSE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let bindings = [EffectBinding {
            effect: "Pulse".into(),
            ops: vec![
                OperationBinding::new(
                    "pause",
                    Operation::Yield,
                    "Ticked",
                    "TickFailed",
                    Cancellation::Constructor("TickCancelled".into()),
                ),
                OperationBinding::new(
                    "read",
                    Operation::Read,
                    "Got",
                    "IoFailed",
                    Cancellation::Constructor("IoCancelled".into()),
                ),
            ],
        }];
        let (bytes, _) =
            super::super::session::object(&checked.world, "main", &bindings, Some(MSVC_TARGET))
                .unwrap();
        let summary = inspect(&bytes).unwrap();
        assert!(summary.runtime_functions >= 1);
        assert_eq!(summary.external_functions, [entry_symbol("main")]);
        let park = b"vesper_fiber_park";
        assert!(bytes.windows(park.len()).any(|w| w == park));
        let error = super::super::session::object(
            &checked.world,
            "main",
            &bindings,
            Some("aarch64-pc-windows-msvc"),
        )
        .unwrap_err();
        assert!(error.contains("unsupported Windows target"), "{error}");
    }

    /// A C harness that runs a generated function on a worker thread,
    /// suspends it at random points and walks its stack with
    /// `RtlLookupFunctionEntry`/`RtlVirtualUnwind` from the suspended
    /// context. Every sample taken in generated code must unwind back to
    /// the harness frame that called it, which needs a correct `.pdata`
    /// entry and `UNWIND_INFO` for every generated frame on the stack.
    #[cfg_attr(not(windows), allow(dead_code))]
    const UNWIND_SAMPLER: &str = r#"
#include <windows.h>
#include <stdio.h>
extern long long run(void);
static DWORD64 caller_begin;
static volatile long long result = -7;
__declspec(noinline) long long call_generated(void) { long long r = run(); return r + 0; }
static DWORD WINAPI worker(LPVOID p) { (void)p; result = call_generated(); return 0; }
int main(void) {
    DWORD64 base;
    PRUNTIME_FUNCTION f = RtlLookupFunctionEntry((DWORD64)call_generated, &base, NULL);
    if (!f) { puts("no pdata for the harness"); return 4; }
    caller_begin = base + f->BeginAddress;
    HANDLE t = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    int samples = 0, generated_samples = 0, max_depth = 0;
    while (WaitForSingleObject(t, 1) == WAIT_TIMEOUT && samples < 500) {
        SuspendThread(t);
        CONTEXT c;
        c.ContextFlags = CONTEXT_FULL;
        GetThreadContext(t, &c);
        int ok = 0, depth = 0, generated = 0;
        for (; depth < 100000; depth++) {
            PRUNTIME_FUNCTION g = RtlLookupFunctionEntry(c.Rip, &base, NULL);
            if (!g) break;
            if (base + g->BeginAddress == caller_begin) { ok = 1; break; }
            if (depth == 0) generated = 1;
            PVOID data; DWORD64 frame;
            RtlVirtualUnwind(UNW_FLAG_NHANDLER, base, c.Rip, g, &c, &data, &frame, NULL);
        }
        ResumeThread(t);
        if (generated) {
            generated_samples++;
            if (!ok) { printf("sample %d: walk failed at depth %d\n", samples, depth); return 2; }
            if (depth > max_depth) max_depth = depth;
        }
        samples++;
    }
    WaitForSingleObject(t, INFINITE);
    printf("result %lld samples %d generated %d depth %d\n", result, samples, generated_samples, max_depth);
    return generated_samples >= 10 && result == 5702887 ? 0 : 3;
}
"#;

    /// `fib(34)` behind a handler: long enough to sample, deep enough to
    /// need every frame's unwind entry.
    const SAMPLED: &str = r#"fn fib(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    if n < 2 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn run() -> i64 {
    handle Fail[ArithOverflow] in {
        fib(34)
    } with {
        return value => value,
        raise(error, resume) => -1,
    }
}
"#;

    #[test]
    fn the_sampled_program_exports_its_entry() {
        let checked = checked(SAMPLED);
        let (bytes, _) =
            super::super::object_for_target(&checked.world, "run", Some(MSVC_TARGET)).unwrap();
        assert_eq!(inspect(&bytes).unwrap().external_functions, ["run"]);
    }

    /// Build [`UNWIND_SAMPLER`] with the MSVC compiler, link it with the
    /// generated object and run it. Runs on Windows hosts.
    #[cfg(windows)]
    #[test]
    fn generated_frames_unwind_with_rtl_virtual_unwind() {
        let dir = std::env::temp_dir().join(format!("vesper-win-unwind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let checked = checked(SAMPLED);
        let (bytes, _) =
            super::super::object_for_target(&checked.world, "run", Some(MSVC_TARGET)).unwrap();
        let generated = dir.join("generated.obj");
        std::fs::write(&generated, bytes).unwrap();
        let source = dir.join("sampler.c");
        std::fs::write(&source, UNWIND_SAMPLER).unwrap();
        let harness = dir.join("sampler.obj");
        let cl = find_msvc_tools::find_tool(MSVC_TARGET, "cl.exe").expect("MSVC cl.exe");
        let mut fo = OsString::from("/Fo");
        fo.push(harness.as_os_str());
        let status = cl
            .to_command()
            .args(["/nologo", "/c", "/O1", "/MT"])
            .arg(fo)
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success(), "cl.exe failed");
        let exe = link(&[harness, generated], &dir.join("sampler"), None).unwrap();
        let output = std::process::Command::new(&exe).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?}: {stdout}", output.status);
        assert!(stdout.contains("result 5702887"), "{stdout}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Link and run every example with the MSVC toolchain and compare its
    /// exit status with the evaluator's result. Runs on Windows hosts.
    #[cfg(windows)]
    #[test]
    fn examples_link_and_run_natively_with_the_evaluator_result() {
        let dir = std::env::temp_dir().join(format!("vesper-win-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, source, expected) in EXAMPLES {
            let checked = checked(source);
            let mut runtime = crate::runtime::Runtime::new(&checked.world);
            let evaluated = runtime.run("main", vec![]).unwrap().to_string();
            assert_eq!(evaluated, expected.to_string(), "{name}: evaluator");
            let object = dir.join(format!("{name}.vesper.o"));
            std::fs::write(&object, windows_object(source)).unwrap();
            let exe = link_executable(&object, &dir.join(name), None)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let status = std::process::Command::new(&exe).status().unwrap();
            assert_eq!(status.code(), Some(expected as i32), "{name}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
