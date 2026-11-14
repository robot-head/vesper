//! A direct, proof-carrying code generator for the scalar core (design §42.1
//! pillar 3b, §7): checked Place IR to x86-64 and AArch64 machine code with
//! no Cranelift in the path.
//!
//! The pipeline has three explicit stages:
//!
//! 1. **Lowering** (`lower.rs`): the functions reachable from the entry are
//!    planned, every value of the supported subset is flattened into 64-bit
//!    words, and each Place IR instruction becomes rows of the slot IR
//!    [`Lir`]. Everything outside the subset is a typed
//!    [`VerifiedError::Unsupported`], including a wildcard arm for IR
//!    operations this backend does not know.
//! 2. **Instruction selection** ([`select`]): a total function from one slot
//!    IR row to a fixed sequence of abstract target instructions [`M`]. Every
//!    row kind has a Lean counterpart in `lean/VerifiedCodegen.lean`, which
//!    proves that each data row preserves the modelled state relation
//!    (forward simulation). [`table_listing`] prints the table in the text
//!    format the Lean file prints, so the tests check the two agree.
//! 3. **Encoding** (`x86_64.rs`, `aarch64.rs`) of each abstract instruction
//!    to fixed bytes, and a container (`container.rs`): a static Linux ELF
//!    executable, or a Mach-O AArch64 object for the system linker.
//!
//! The supported subset is: `u8`, `u32`, `u64`, `i64`, `bool` and `Unit`;
//! structs (after generic substitution) and tuples of them, passed as words
//! on the stack; checked `+ - * / %`, negation and compound assignment with
//! the `vesper.arith.checked/1` aborts; `wrapping_*`; comparisons; `!`;
//! `if`; direct calls including recursion, where a self call in tail
//! position is a loop; `inout` arguments (copy-in/copy-out); `let` with
//! irrefutable tuple and struct patterns. Functions may declare
//! `Fail[ArithOverflow]` and `Fail[RuntimeError]`: an abort exits the
//! process with [`EXIT_OVERFLOW`] or [`EXIT_DIVISION_BY_ZERO`]. Handlers and
//! every other effect, floats, strings, enums and `match`, closures, generic
//! functions, vectors and regions are rejected.
//!
//! Values live in frame slots, never in registers across slot-IR rows: there
//! is no register allocator, so the code is slow but each row is local, which
//! is what the Lean proof relies on.
use crate::check::World;
use std::fmt;

mod aarch64;
mod asm;
mod cli;
mod container;
mod lower;
mod machine;
mod x86_64;

pub use cli::build_command;
pub use machine::{
    ArithOp, Cond, Kind, Label, Lir, Loc, M, Reg, Sign, Slot, TrapKind, WrapOp, select,
    table_listing, table_rows,
};

/// The value of `vesper build --codegen` that selects this backend.
pub const CODEGEN: &str = "verified";
/// Version of the frame layout, calling convention and result protocol.
pub const ABI: &str = "vesper.verified-codegen/1";
/// Process exit status of an unhandled `Fail[ArithOverflow]`.
pub const EXIT_OVERFLOW: u8 = 101;
/// Process exit status of an unhandled division by zero.
pub const EXIT_DIVISION_BY_ZERO: u8 = 102;
/// Process exit status when an unreachable block is reached.
pub const EXIT_UNREACHABLE: u8 = 103;
/// Frame words per function (slots plus the outgoing call area). Keeps every
/// frame offset inside one AArch64 scaled 12-bit immediate.
pub const MAX_FRAME_WORDS: u32 = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
}
/// A supported target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub os: Os,
}
impl Target {
    pub const X86_64_LINUX: Target = Target {
        arch: Arch::X86_64,
        os: Os::Linux,
    };
    pub const AARCH64_LINUX: Target = Target {
        arch: Arch::Aarch64,
        os: Os::Linux,
    };
    pub const AARCH64_MACOS: Target = Target {
        arch: Arch::Aarch64,
        os: Os::MacOs,
    };
    /// Parse a target triple.
    pub fn from_triple(triple: &str) -> Result<Self, VerifiedError> {
        let arch = if triple.starts_with("x86_64-") {
            Arch::X86_64
        } else if triple.starts_with("aarch64-") || triple.starts_with("arm64-") {
            Arch::Aarch64
        } else {
            return Err(VerifiedError::Target(format!(
                "unsupported target '{triple}' (x86_64 or aarch64 Linux, aarch64 macOS)"
            )));
        };
        let os = if triple.contains("-linux") {
            Os::Linux
        } else if triple.contains("-apple-darwin") || triple.contains("-macos") {
            Os::MacOs
        } else {
            return Err(VerifiedError::Target(format!(
                "unsupported operating system in '{triple}'"
            )));
        };
        if arch == Arch::X86_64 && os == Os::MacOs {
            return Err(VerifiedError::Target(
                "x86_64 macOS is not a verified-codegen target".into(),
            ));
        }
        Ok(Self { arch, os })
    }
    /// The target of the machine running this code, if supported.
    pub fn host() -> Option<Self> {
        match (std::env::consts::ARCH, std::env::consts::OS) {
            ("x86_64", "linux") => Some(Self::X86_64_LINUX),
            ("aarch64", "linux") => Some(Self::AARCH64_LINUX),
            ("aarch64", "macos") => Some(Self::AARCH64_MACOS),
            _ => None,
        }
    }
}

/// How the executable reports the entry's result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultProtocol {
    /// Exit with the low byte of the first result word (as `vesper build`
    /// executables do); `Unit` exits 0.
    #[default]
    ExitStatus,
    /// Write every result word to stdout as 8 little-endian bytes, then exit
    /// 0. Used by the differential tests.
    Words,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub target: Target,
    pub protocol: ResultProtocol,
    /// Run the Place IR optimizer first (as `vesper build` does by default).
    pub optimize: bool,
    pub max_functions: usize,
    pub max_rows: usize,
    /// AArch64 conditional branches reach ±1 MiB.
    pub max_code_bytes: usize,
}
impl Options {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            protocol: ResultProtocol::ExitStatus,
            optimize: true,
            max_functions: 4096,
            max_rows: 1_000_000,
            max_code_bytes: 1 << 20,
        }
    }
}

/// A construct outside the supported subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Float,
    String,
    Enum,
    Match,
    Index,
    FunctionValue,
    Generic,
    Effect,
    ExternalFunction,
    StructuralEquality,
    Type,
    /// An IR operation this backend does not know.
    Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifiedError {
    Unsupported {
        construct: Unsupported,
        function: String,
        detail: String,
    },
    Entry(String),
    Target(String),
    Limit(String),
    /// The checked program or IR violates an invariant the backend relies on.
    Invalid(String),
}
impl VerifiedError {
    pub(crate) fn unsupported(
        construct: Unsupported,
        function: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            construct,
            function: function.to_owned(),
            detail: detail.into(),
        }
    }
}
impl fmt::Display for VerifiedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                construct,
                function,
                detail,
            } => write!(
                f,
                "verified codegen: unsupported {construct:?} in '{function}': {detail}"
            ),
            Self::Entry(e) => write!(f, "verified codegen: entry: {e}"),
            Self::Target(e) => write!(f, "verified codegen: target: {e}"),
            Self::Limit(e) => write!(f, "verified codegen: limit: {e}"),
            Self::Invalid(e) => write!(f, "verified codegen: invalid input: {e}"),
        }
    }
}
impl std::error::Error for VerifiedError {}

/// What the output file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// A static Linux executable (run it directly).
    ElfExecutable,
    /// A Mach-O object exporting `_main` (link it with `cc`).
    MachOObject,
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub target: Target,
    pub container: Container,
    pub bytes: Vec<u8>,
    /// Machine code only.
    pub code_len: usize,
    /// `(path, kind)` of each result word, in output order.
    pub result_words: Vec<(Vec<String>, Kind)>,
    /// The slot IR of every function, for inspection.
    pub lir: Vec<(String, Vec<Lir>)>,
    /// Number of abstract machine instructions emitted.
    pub machine_instructions: usize,
}

/// Compile `entry` (no parameters) and everything it reaches.
pub fn compile(world: &World, entry: &str, options: &Options) -> Result<Artifact, VerifiedError> {
    let f = world
        .functions
        .get(entry)
        .ok_or_else(|| VerifiedError::Entry(format!("unknown entry function '{entry}'")))?;
    if !f.params.is_empty() {
        return Err(VerifiedError::Entry(format!(
            "entry '{entry}' must take no parameters"
        )));
    }
    let mut program = crate::ir::lower_world(world);
    if options.optimize {
        crate::ir_opt::optimize(world, &mut program).map_err(VerifiedError::Invalid)?;
    }
    let plan = lower::plan(world, &program, entry, options.max_functions)?;
    let mut next_label = plan
        .functions
        .last()
        .map(|f| f.label + 2)
        .unwrap_or(machine::FIRST_FREE_LABEL);
    let mut lir = vec![];
    let mut rows_total = 0usize;
    for index in 0..plan.functions.len() {
        let lowered = lower::function(
            world,
            &program,
            &plan,
            index,
            &mut next_label,
            options.max_rows.saturating_sub(rows_total),
        )?;
        rows_total += lowered.rows.len();
        let _ = (lowered.slots, lowered.out_words);
        lir.push((plan.functions[index].name.clone(), lowered.rows));
    }
    let entry_plan = &plan.functions[0];
    let result_words = entry_plan.result.0.clone();
    let mut a = asm::Asm::default();
    let start = match options.target.arch {
        Arch::X86_64 => x86_64::start,
        Arch::Aarch64 => aarch64::start,
    };
    start(
        &mut a,
        options.target.os,
        options.protocol,
        entry_plan.result.len(),
        entry_plan.label,
    )?;
    let mut machine_instructions = 0;
    for (_, rows) in &lir {
        for row in rows {
            for m in select(row) {
                machine_instructions += 1;
                match options.target.arch {
                    Arch::X86_64 => x86_64::emit(&mut a, &m)?,
                    Arch::Aarch64 => aarch64::emit(&mut a, &m)?,
                }
            }
            if a.here() > options.max_code_bytes {
                return Err(VerifiedError::Limit(format!(
                    "more than {} bytes of code",
                    options.max_code_bytes
                )));
            }
        }
    }
    let code = a.finish()?;
    let (container, bytes) = match options.target {
        Target {
            arch: Arch::X86_64,
            os: Os::Linux,
        } => (
            Container::ElfExecutable,
            container::elf_executable(container::EM_X86_64, &code),
        ),
        Target {
            arch: Arch::Aarch64,
            os: Os::Linux,
        } => (
            Container::ElfExecutable,
            container::elf_executable(container::EM_AARCH64, &code),
        ),
        Target {
            arch: Arch::Aarch64,
            os: Os::MacOs,
        } => (Container::MachOObject, container::macho_object(&code)),
        Target {
            arch: Arch::X86_64,
            os: Os::MacOs,
        } => {
            return Err(VerifiedError::Target(
                "x86_64 macOS is not a verified-codegen target".into(),
            ));
        }
    };
    Ok(Artifact {
        target: options.target,
        container,
        bytes,
        code_len: code.len(),
        result_words,
        lir,
        machine_instructions,
    })
}

/// How an executable built with [`ResultProtocol::Words`] finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The result words.
    Words(Vec<u64>),
    Overflow,
    DivisionByZero,
    Unreachable,
    /// Any other exit (a bug or a signal).
    Other(String),
}
impl Outcome {
    /// Interpret an exit status and stdout.
    pub fn from_exit(code: Option<i32>, stdout: &[u8]) -> Self {
        match code {
            Some(0) if stdout.len().is_multiple_of(8) => Self::Words(
                stdout
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c))
                    .collect(),
            ),
            Some(c) if c == i32::from(EXIT_OVERFLOW) => Self::Overflow,
            Some(c) if c == i32::from(EXIT_DIVISION_BY_ZERO) => Self::DivisionByZero,
            Some(c) if c == i32::from(EXIT_UNREACHABLE) => Self::Unreachable,
            other => Self::Other(format!("exit {other:?}, {} stdout bytes", stdout.len())),
        }
    }
}

/// Rebuild the evaluator's value of type `ty` from result words, so it can
/// be compared with `runtime::Value`'s rendering.
pub fn decode(
    world: &World,
    ty: &crate::ast::Type,
    words: &[u64],
) -> Result<crate::runtime::Value, VerifiedError> {
    let mut at = 0;
    let v = decode_at(world, ty, words, &mut at, 0)?;
    if at != words.len() {
        return Err(VerifiedError::Invalid(format!(
            "{} result words, type has {at}",
            words.len()
        )));
    }
    Ok(v)
}
fn decode_at(
    world: &World,
    ty: &crate::ast::Type,
    words: &[u64],
    at: &mut usize,
    depth: usize,
) -> Result<crate::runtime::Value, VerifiedError> {
    use crate::ast::Type;
    use crate::runtime::Value;
    if depth > 32 {
        return Err(VerifiedError::Limit("type nesting".into()));
    }
    let ty = crate::numeric::default_type(ty);
    let mut word = || {
        let w = words
            .get(*at)
            .copied()
            .ok_or_else(|| VerifiedError::Invalid("too few result words".into()))?;
        *at += 1;
        Ok::<u64, VerifiedError>(w)
    };
    Ok(match ty.name() {
        "i64" => Value::Int(word()? as i64),
        "u64" => Value::UInt(word()?, 64),
        "u32" => Value::UInt(word()?, 32),
        "u8" => Value::UInt(word()?, 8),
        "bool" => Value::Bool(word()? != 0),
        "Unit" => Value::Tuple(vec![]),
        _ => match &ty {
            Type::Tuple(ts) => Value::Tuple(
                ts.iter()
                    .map(|t| decode_at(world, t, words, at, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Named(n, args) => {
                let adt = world
                    .adts
                    .get(n)
                    .filter(|a| !a.is_enum)
                    .ok_or_else(|| VerifiedError::Invalid(format!("cannot decode {ty}")))?;
                let vars = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let mut fields = std::collections::BTreeMap::new();
                for f in &adt.variants[0].fields {
                    let t = crate::ast::substitute(&f.ty, &vars);
                    fields.insert(f.name.clone(), decode_at(world, &t, words, at, depth + 1)?);
                }
                Value::Data {
                    ty: n.clone(),
                    constructor: adt.variants[0].name.clone(),
                    fields,
                    resource_identity: None,
                }
            }
            _ => return Err(VerifiedError::Invalid(format!("cannot decode {ty}"))),
        },
    })
}
