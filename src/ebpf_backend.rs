//! Compiler eBPF target (design §27, §7): XDP and tc classifier programs
//! from checked Vesper source.
//!
//! `vesper build --target bpf` lowers a restricted, checked source subset
//! from the Place IR ([`crate::ir`]) to `vesper_ebpf` [`Asm`], runs the
//! result through the `vesper-ebpf` pre-flight verifier model, and writes a
//! relocatable BPF ELF object with BTF (`vesper_ebpf::elf`). The kernel
//! verifier is exercised by the acceptance tests where the host permits
//! `bpf()`.
//!
//! # Source surface
//!
//! The backend adds no syntax. A program is ordinary checked Vesper whose
//! entry function (default `main`) takes no parameters and returns one of
//! two declared action enums; its program type follows from the result:
//!
//! * `copy enum XdpAction { Aborted, Drop, Pass, Tx, Redirect }`: XDP;
//! * `copy enum TcAction { Ok, Reclassify, Shot, Pipe, Stolen, Queued,
//!   Repeat, Redirect }`: tc classifier (`BPF_PROG_TYPE_SCHED_CLS`).
//!
//! The variants must be declared in exactly this order, so a variant's tag
//! is its kernel return code.
//!
//! The kernel is reached through effects the entry leaves unhandled. The
//! backend recognises them by name and checks their signatures:
//!
//! * `effect Packet { fn load_u8(offset: u32) -> Option[u8];
//!   fn load_u16be(offset: u32) -> Option[u32];
//!   fn load_u32be(offset: u32) -> Option[u32]; }`: bounds-checked views
//!   of packet bytes. Each load is lowered to the verifier-canonical
//!   `cursor = data + offset; if cursor + size > data_end goto none`
//!   sequence (§27.1 item 2), so out-of-range reads are `None`, never
//!   undefined.
//! * `effect Bpf { fn ktime_get_ns() -> u64; fn smp_processor_id() -> u32;
//!   fn prandom_u32() -> u32; }`: helper calls.
//! * any other effect whose operations are drawn from
//!   `lookup(key: K) -> Option[V]`, `update(key: K, value: V)`,
//!   `delete(key: K)` and `add(key: K, delta: V)` is a BPF map. Its kind
//!   and size come from a nullary function named after the effect in
//!   snake case with a `_map` suffix (`Counters` -> `counters_map`)
//!   returning a constant `BpfMapSpec { kind: BpfMapKind::..., max_entries:
//!   N }`, where the program declares `copy enum BpfMapKind { Hash, Array,
//!   PerCpuHash, PerCpuArray, LruHash }` and `copy struct BpfMapSpec { kind:
//!   BpfMapKind, max_entries: u32 }`. `K` and `V` are `u8`, `u32` or `u64`.
//!
//! # Subset
//!
//! Values are `u8`, `u32`, `u64`, `i64`, `bool`, `Unit`, field-less enums
//! and `Option[T]` of those scalars. Functions reachable from the entry
//! become BPF-to-BPF subprograms (at most four scalar parameters, the
//! context travels in `r1`). Recursion is rejected except for tail
//! self-recursion, which is lowered to an open-coded loop; the loop is
//! accepted only when the verifier model proves it bounded (§27.1 item 3).
//! Every function's frame and the worst call chain must fit the 512-byte
//! eBPF stack. Operations the checker could not prove total (`Fail`), and
//! every other IR operation (closures, handlers, indirect calls, tuples,
//! structs at run time, indexing) are rejected with a typed
//! [`EbpfBackendError`] naming a stable rule id. Signed division and
//! `Packet::len` are not provided (see the lane report).
//!
//! Like `vesper-ebpf`'s `ref.leak` rule, this is a check of the emitted
//! program and of the source subset; it does not implement the Vesper
//! source typing rules for linear kernel references (Theorem 2, [T-Let]).
use crate::{
    ast::{Diagnostic, Span},
    check::World,
};
use std::fmt;
use vesper_ebpf::{Program, ProgramType, VerifierPolicy, VerifyError, VerifyReport};

mod lower;
mod surface;

pub use surface::{ActionKind, MapBinding};

/// Stable rule identifiers of [`EbpfBackendError::Unsupported`].
pub mod rules {
    /// Entry function shape (missing, parameters, result type).
    pub const ENTRY: &str = "ebpf.entry";
    /// The action enum does not have the canonical variant list.
    pub const ACTION: &str = "ebpf.action";
    /// A type outside the subset.
    pub const TYPE: &str = "ebpf.type";
    /// An effect the backend cannot lower, or a malformed recognised effect.
    pub const EFFECT: &str = "ebpf.effect";
    /// A malformed map declaration or map specification function.
    pub const MAP: &str = "ebpf.map";
    /// Recursion other than tail self-recursion.
    pub const RECURSION: &str = "ebpf.recursion";
    /// An IR operation or terminator outside the subset.
    pub const OPERATION: &str = "ebpf.operation";
    /// An operation that may raise `Fail` (checked overflow, division).
    pub const FALLIBLE: &str = "ebpf.fallible";
    /// A function's parameters or result do not fit the BPF calling
    /// convention.
    pub const CALL: &str = "ebpf.call";
    /// A frame or call chain over the 512-byte stack.
    pub const STACK: &str = "ebpf.stack";
    /// Generic functions are not monomorphised for eBPF.
    pub const GENERIC: &str = "ebpf.generic";
}

/// Why a source program did not become a verified eBPF object.
#[derive(Clone, Debug, PartialEq)]
pub enum EbpfBackendError {
    /// The source did not parse or check.
    Source(Vec<Diagnostic>),
    /// The checked program is outside the supported eBPF subset.
    Unsupported {
        rule: &'static str,
        function: String,
        span: Option<Span>,
        message: String,
    },
    /// Assembly failed (for example a jump out of 16-bit range).
    Asm(String),
    /// The `vesper-ebpf` verifier model rejected the emitted program.
    Verifier(VerifyError),
    /// ELF emission failed.
    Elf(String),
}

impl EbpfBackendError {
    /// The stable rule id: the subset rule, `source`, `asm`, `elf`, or the
    /// verifier model's own rule id.
    pub fn rule(&self) -> &str {
        match self {
            Self::Source(_) => "source",
            Self::Unsupported { rule, .. } => rule,
            Self::Asm(_) => "asm",
            Self::Verifier(e) => e.rule,
            Self::Elf(_) => "elf",
        }
    }
}

impl fmt::Display for EbpfBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(d) => {
                write!(f, "source has {} diagnostic(s)", d.len())?;
                for d in d {
                    write!(f, "\n  [{}] {}", d.code, d.message)?;
                }
                Ok(())
            }
            Self::Unsupported {
                rule,
                function,
                message,
                ..
            } => write!(f, "[{rule}] in `{function}`: {message}"),
            Self::Asm(m) => write!(f, "[asm] {m}"),
            Self::Verifier(e) => write!(f, "eBPF verifier model rejected the program: {e}"),
            Self::Elf(m) => write!(f, "[elf] {m}"),
        }
    }
}

impl std::error::Error for EbpfBackendError {}

pub(crate) fn unsupported(
    rule: &'static str,
    function: &str,
    span: Option<&Span>,
    message: impl Into<String>,
) -> EbpfBackendError {
    EbpfBackendError::Unsupported {
        rule,
        function: function.to_owned(),
        span: span.cloned(),
        message: message.into(),
    }
}

/// Compilation options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Entry function (default `main`).
    pub entry: String,
    /// File name recorded in BTF line info.
    pub file: String,
    /// Policy for the verifier model.
    pub policy: VerifierPolicy,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            entry: "main".into(),
            file: "main.ves".into(),
            policy: VerifierPolicy::default(),
        }
    }
}

/// A verified eBPF program compiled from source.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub program: Program,
    /// The verifier model's report (instructions processed, stack).
    pub report: VerifyReport,
    /// Which source effect became which map (index into `program.maps`).
    pub maps: Vec<MapBinding>,
    pub action: ActionKind,
    /// Source functions compiled, entry first, with their frame bytes.
    pub frames: Vec<(String, u32)>,
    /// Functions whose tail self-recursion became a loop.
    pub loops: Vec<String>,
}

impl Compiled {
    pub fn prog_type(&self) -> ProgramType {
        self.action.prog_type()
    }

    /// The relocatable BPF ELF object (`.BTF` and `.BTF.ext` included).
    pub fn object(&self) -> Result<Vec<u8>, EbpfBackendError> {
        vesper_ebpf::elf::write_object(&self.program)
            .map_err(|e| EbpfBackendError::Elf(e.to_string()))
    }

    /// Index of the map lowered from `effect`.
    pub fn map_index(&self, effect: &str) -> Option<usize> {
        self.maps
            .iter()
            .find(|m| m.effect == effect)
            .map(|m| m.index)
    }
}

/// Parse, check, lower, assemble and verify `source`.
pub fn compile_source(source: &str, options: &Options) -> Result<Compiled, EbpfBackendError> {
    let parsed = crate::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(EbpfBackendError::Source(parsed.diagnostics));
    }
    let checked = crate::check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Err(EbpfBackendError::Source(checked.diagnostics));
    }
    compile_world(&checked.world, Some(source), options)
}

/// Lower an already checked world. `source`, when given, provides BTF
/// line info.
pub fn compile_world(
    world: &World,
    source: Option<&str>,
    options: &Options,
) -> Result<Compiled, EbpfBackendError> {
    let ir = crate::ir::lower_world(world);
    let surface = surface::Surface::analyze(world, &ir, &options.entry)?;
    let lowered = lower::lower(world, &ir, &surface, source, options)?;
    let report = vesper_ebpf::verify(&lowered.program, &options.policy)
        .map_err(EbpfBackendError::Verifier)?;
    Ok(Compiled {
        program: lowered.program,
        report,
        maps: surface.maps.clone(),
        action: surface.action,
        frames: lowered.frames,
        loops: lowered.loops,
    })
}

/// `vesper build --target bpf <file.ves> [-o out.o] [--entry name]`:
/// compile, verify with the model and write the object. Returns the
/// summary printed by the CLI.
pub fn build_command(
    input: &str,
    source: &str,
    entry: &str,
    output: Option<&std::path::Path>,
) -> Result<String, String> {
    let options = Options {
        entry: entry.to_owned(),
        file: std::path::Path::new(input)
            .file_name()
            .map_or_else(|| input.to_owned(), |f| f.to_string_lossy().into_owned()),
        policy: VerifierPolicy::default(),
    };
    let compiled = compile_source(source, &options).map_err(|e| format!("{input}: {e}"))?;
    let object = compiled.object().map_err(|e| e.to_string())?;
    let path = output.map_or_else(
        || std::path::Path::new(input).with_extension("o"),
        std::path::Path::to_path_buf,
    );
    std::fs::write(&path, &object).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = format!(
        "wrote {} ({} bytes): {} program `{}`, {} instructions, {} map(s)\nverifier model: accepted, {} insns processed, worst-case stack {} bytes\n",
        path.display(),
        object.len(),
        vesper_ebpf::elf::program_section(&compiled.program),
        compiled.program.name,
        compiled.program.insns.len(),
        compiled.program.maps.len(),
        compiled.report.insns_processed,
        compiled.report.worst_stack,
    );
    for m in &compiled.maps {
        let d = &compiled.program.maps[m.index];
        out.push_str(&format!(
            "map {} <- effect {}: {:?}, key {} B, value {} B, {} entries\n",
            d.name, m.effect, d.kind, d.key_size, d.value_size, d.max_entries
        ));
    }
    Ok(out)
}
