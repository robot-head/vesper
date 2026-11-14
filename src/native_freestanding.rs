//! Freestanding native images: bare-metal `riscv64imac-unknown-none-elf` and
//! `aarch64-unknown-none` (§26, §7; wave seven lane 27).
//!
//! [`build_image`] compiles a checked entry function with the ordinary
//! Cranelift backend ([`crate::native::object_for_isa`]) for a bare-metal ISA
//! and links it, with no allocator, no Pulse scheduler and no libc, into a
//! static ELF executable for a [`Board`]:
//!
//! * an entry stub ([`stub`]) written as machine code: it masks local
//!   interrupts (mstatus.MIE / DAIF), installs the trap vector, sets the stack
//!   pointer, zeroes `.bss` and calls the runtime's main;
//! * a runtime object ([`runtime`]) generated as Cranelift IR: it calls the
//!   entry, prints the result over the board's UART and exits through the
//!   board's exit device; its trap handler maps the faulting PC to the
//!   backend's abort sites (pattern-match failure, out-of-bounds index, ...)
//!   and prints which abort fired;
//! * a static linker ([`link`]) for exactly the relocations these objects
//!   use, and an equivalent GNU ld linker script ([`script`]) so an external
//!   `ld.lld` can link the same objects (checked by a gated test).
//!
//! Before an image is accepted, the frames of every emitted function are
//! measured and checked with Silicon's stack analysis
//! ([`vesper_silicon::stack::Program`]) against the stack the image reserves,
//! and the call graph is checked with Silicon's interrupt-mask rules
//! ([`vesper_silicon::atomic::check_atomic`]): the whole image runs with
//! interrupts masked, so it is atomic context and may not reach a suspending
//! or allocating symbol. Unsupported programs (parameters, aggregate results,
//! floating point on riscv64imac, imported symbols such as `fmod`, native
//! sessions that need Pulse) are typed [`FreestandingError`]s.
//!
//! What is not claimed: the images were booted under QEMU's `virt` machines,
//! not on hardware; the stack bound covers the code Cranelift emitted (the
//! stub's own use is added explicitly), and there is no hardware stack guard.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::TargetIsa;

use crate::check::World;

#[path = "freestanding/checks.rs"]
mod checks;
#[path = "freestanding/elf.rs"]
pub mod elf;
#[path = "freestanding/link.rs"]
pub mod link;
#[path = "freestanding/runtime.rs"]
pub mod runtime;
#[path = "freestanding/script.rs"]
pub mod script;
#[path = "freestanding/stub.rs"]
pub mod stub;

pub use vesper_silicon::Arch;

/// Schema of [`FreestandingImage`] and its object set.
pub const FREESTANDING_SCHEMA: &str = "vesper.native.freestanding/1";

/// Process exit status the runtime reports after the entry returned.
pub const EXIT_OK: u32 = 0;
/// Exit status after an abort (a backend trap site) was taken.
pub const EXIT_ABORT: u32 = 3;
/// Exit status after an exception that is not a known abort site.
pub const EXIT_EXCEPTION: u32 = 4;

/// A bare-metal target triple.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FreestandingTarget {
    /// `riscv64imac-unknown-none-elf`, machine mode.
    Riscv64Imac,
    /// `aarch64-unknown-none`, EL1.
    Aarch64,
}

impl FreestandingTarget {
    pub const ALL: [FreestandingTarget; 2] = [Self::Riscv64Imac, Self::Aarch64];

    pub fn triple(self) -> &'static str {
        match self {
            Self::Riscv64Imac => "riscv64imac-unknown-none-elf",
            Self::Aarch64 => "aarch64-unknown-none",
        }
    }

    /// Parse a triple; hosted triples and other ISAs are typed rejections.
    pub fn parse(triple: &str) -> Result<Self, FreestandingError> {
        Self::ALL
            .into_iter()
            .find(|t| t.triple() == triple)
            .ok_or_else(|| FreestandingError::UnsupportedTarget(triple.to_string()))
    }

    pub fn arch(self) -> Arch {
        match self {
            Self::Riscv64Imac => Arch::Riscv64,
            Self::Aarch64 => Arch::Aarch64,
        }
    }

    /// ISA-specific Cranelift settings. Cranelift's RISC-V backend refuses
    /// to run without the G extensions, so F and D are enabled for the code
    /// generator and the absence of floating point is enforced on the IR
    /// instead (see [`FreestandingError::UnsupportedOperation`]); compressed
    /// instructions (`C` = Zca on RV64) are on, compressed FP loads are off.
    pub fn isa_flags(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Riscv64Imac => &[("has_zca", "true"), ("has_zcd", "false")],
            Self::Aarch64 => &[],
        }
    }

    /// The Cranelift ISA: exactly the one Silicon's stack analysis measures
    /// with (`opt_level=speed`, frame pointers kept, probestack off,
    /// non-PIC).
    pub fn isa(self) -> Result<Arc<dyn TargetIsa>, FreestandingError> {
        vesper_silicon::stack::target_isa_with(self.arch(), self.isa_flags())
            .map_err(|e| FreestandingError::Codegen(e.to_string()))
    }

    /// The QEMU `virt` board for this target.
    pub fn qemu_virt(self) -> Board {
        match self {
            Self::Riscv64Imac => Board {
                name: "qemu-riscv64-virt".into(),
                ram_base: 0x8000_0000,
                ram_bytes: 128 << 20,
                image_base: 0x8000_0000,
                uart: Uart::Ns16550 { base: 0x1000_0000 },
                exit: ExitDevice::SifiveTest { base: 0x10_0000 },
            },
            Self::Aarch64 => Board {
                name: "qemu-aarch64-virt".into(),
                ram_base: 0x4000_0000,
                ram_bytes: 128 << 20,
                // QEMU places the device tree at the start of RAM.
                image_base: 0x4008_0000,
                uart: Uart::Pl011 { base: 0x0900_0000 },
                exit: ExitDevice::Semihosting,
            },
        }
    }
}

impl fmt::Display for FreestandingTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.triple())
    }
}

/// The serial device the runtime prints on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uart {
    /// NS16550A: THR at +0, LSR at +5 (bit 5 = transmit holding empty).
    Ns16550 { base: u64 },
    /// Arm PL011: DR at +0, FR at +0x18 (bit 5 = transmit FIFO full).
    Pl011 { base: u64 },
}

/// How the image ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitDevice {
    /// SiFive test finisher (QEMU riscv `virt`): `0x5555` passes,
    /// `code << 16 | 0x3333` fails with `code`.
    SifiveTest { base: u64 },
    /// Arm semihosting `SYS_EXIT_EXTENDED` through `hlt #0xf000` (QEMU needs
    /// `-semihosting`).
    Semihosting,
    /// Park the core (`wfi`/`wfe` loop); the status is only printed.
    Park,
}

/// Memory map and devices of the machine an image is linked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
    pub name: String,
    pub ram_base: u64,
    pub ram_bytes: u64,
    /// Load and entry address of `_start` (inside RAM).
    pub image_base: u64,
    pub uart: Uart,
    pub exit: ExitDevice,
}

/// Options for [`build_image`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageOptions {
    pub target: FreestandingTarget,
    /// Defaults to [`FreestandingTarget::qemu_virt`].
    pub board: Option<Board>,
    /// Bytes reserved for the single stack; also the budget of the stack
    /// analysis for both roots (main and the trap handler).
    pub stack_bytes: u32,
    /// Run the IR optimizer before lowering (as `vesper build` does).
    pub optimize: bool,
}

impl ImageOptions {
    pub fn new(target: FreestandingTarget) -> Self {
        Self {
            target,
            board: None,
            stack_bytes: 16 * 1024,
            optimize: true,
        }
    }
}

/// One abort site of the image: a trap instruction the backend emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrapSite {
    pub pc: u64,
    /// Object symbol of the containing function.
    pub function: String,
    /// Cranelift trap code (`user2`, `int_ovf`, ...).
    pub code: String,
    /// What the runtime prints for it.
    pub message: String,
}

/// Worst-case stack depth of one root, from Silicon's analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackRootSummary {
    pub root: String,
    pub worst_case_bytes: u64,
    /// Object symbols on a deepest path, root first.
    pub worst_path: Vec<String>,
    pub budget_bytes: u32,
}

/// A linked image and everything that went into it.
#[derive(Clone, Debug)]
pub struct FreestandingImage {
    pub target: FreestandingTarget,
    pub board: Board,
    pub entry_function: String,
    /// The static ELF executable.
    pub elf: Vec<u8>,
    /// Inputs, in link order: stub, user code, runtime.
    pub objects: Vec<(String, Vec<u8>)>,
    /// GNU ld script that lays the objects out like [`link::link`].
    pub linker_script: String,
    /// `_start`.
    pub entry_address: u64,
    pub layout: link::Layout,
    pub symbols: Vec<link::LinkedSymbol>,
    pub traps: Vec<TrapSite>,
    pub stack: Vec<StackRootSummary>,
    pub stack_bytes: u32,
    /// Frame bytes per user function as Silicon measured them.
    pub frames: BTreeMap<String, u32>,
    /// Cranelift IR of the user functions.
    pub clif: String,
}

impl FreestandingImage {
    pub fn symbol(&self, name: &str) -> Option<&link::LinkedSymbol> {
        self.symbols.iter().find(|s| s.name == name)
    }
}

/// Typed rejection; nothing here is a successful build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreestandingError {
    UnsupportedTarget(String),
    /// The entry cannot be the root of a freestanding image.
    UnsupportedEntry(String),
    /// A construct this target cannot lower (floating point on riscv64imac,
    /// DetFloat NaN canonicalisation, ...).
    UnsupportedOperation {
        function: String,
        what: String,
    },
    /// Cranelift or the native backend failed.
    Codegen(String),
    /// A symbol no input defines: freestanding images have no libc, no
    /// allocator and no Pulse runtime.
    UnresolvedSymbol {
        symbol: String,
        object: String,
    },
    DuplicateSymbol(String),
    UnsupportedRelocation {
        object: String,
        r_type: u32,
    },
    UnsupportedSection {
        object: String,
        section: String,
    },
    RelocationOutOfRange {
        object: String,
        symbol: String,
        r_type: u32,
    },
    /// The image (with its stack) does not fit the board's RAM.
    ImageTooLarge {
        needed: u64,
        available: u64,
    },
    /// Malformed input object.
    Object(String),
    /// The object code differs from the code the checks measured.
    CodeMismatch {
        function: String,
    },
    /// Silicon's stack analysis rejected the image.
    Stack(Vec<vesper_silicon::VerifierFinding>),
    /// Silicon's interrupt-mask rules rejected the image.
    Interrupt(Vec<vesper_silicon::VerifierFinding>),
    /// An external linker failed ([`link_with_external`]).
    ExternalLinker(String),
}

impl fmt::Display for FreestandingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget(t) => write!(
                f,
                "unsupported freestanding target '{t}' (expected riscv64imac-unknown-none-elf or aarch64-unknown-none)"
            ),
            Self::UnsupportedEntry(m) => write!(f, "unsupported freestanding entry: {m}"),
            Self::UnsupportedOperation { function, what } => {
                write!(
                    f,
                    "'{function}' uses {what}, which this target does not support"
                )
            }
            Self::Codegen(m) => write!(f, "freestanding codegen: {m}"),
            Self::UnresolvedSymbol { symbol, object } => write!(
                f,
                "undefined symbol '{symbol}' referenced by {object}: freestanding images have no libc, allocator or Pulse runtime"
            ),
            Self::DuplicateSymbol(s) => write!(f, "symbol '{s}' is defined twice"),
            Self::UnsupportedRelocation { object, r_type } => {
                write!(f, "{object}: unsupported relocation type {r_type}")
            }
            Self::UnsupportedSection { object, section } => {
                write!(f, "{object}: unsupported allocated section '{section}'")
            }
            Self::RelocationOutOfRange {
                object,
                symbol,
                r_type,
            } => write!(
                f,
                "{object}: relocation {r_type} to '{symbol}' is out of range"
            ),
            Self::ImageTooLarge { needed, available } => write!(
                f,
                "image needs {needed} bytes of RAM but the board has {available}"
            ),
            Self::Object(m) => write!(f, "malformed object: {m}"),
            Self::CodeMismatch { function } => write!(
                f,
                "object code of '{function}' differs from the code that was measured"
            ),
            Self::Stack(findings) => {
                write!(f, "stack analysis rejected the image:")?;
                for x in findings {
                    write!(f, " [{}] {}: {};", x.rule, x.function, x.message)?;
                }
                Ok(())
            }
            Self::Interrupt(findings) => {
                write!(f, "interrupt-mask rules rejected the image:")?;
                for x in findings {
                    write!(f, " [{}] {}: {};", x.rule, x.function, x.message)?;
                }
                Ok(())
            }
            Self::ExternalLinker(m) => write!(f, "external linker: {m}"),
        }
    }
}

impl std::error::Error for FreestandingError {}

/// Result kinds the runtime can print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultKind {
    Signed,
    /// Unsigned with this bit width (8, 32 or 64).
    Unsigned(u32),
    Bool,
    Unit,
}

/// An entry whose only effects are `Fail` instances: an escaping failure is
/// an abort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailEntry {
    /// `Fail[T]` per instance, in instance order (record tag `i + 1`).
    pub instances: Vec<String>,
    /// Bytes of the failure record the runtime passes to the entry.
    pub record_bytes: u32,
}

/// First user trap code the runtime uses for unhandled-failure aborts
/// (the backend uses 1-4).
pub const ABORT_TRAP_BASE: u8 = 16;

fn result_kind(
    world: &World,
    entry: &str,
) -> Result<(ResultKind, Option<FailEntry>), FreestandingError> {
    use crate::ast::Type;
    let f = world
        .functions
        .get(entry)
        .ok_or_else(|| FreestandingError::UnsupportedEntry(format!("no function '{entry}'")))?;
    if !f.params.is_empty() {
        return Err(FreestandingError::UnsupportedEntry(format!(
            "'{entry}' takes parameters; a freestanding entry takes none"
        )));
    }
    if !f.generics.is_empty() {
        return Err(FreestandingError::UnsupportedEntry(format!(
            "'{entry}' is generic"
        )));
    }
    let fail_only = f.effects.tail.is_none()
        && f.effects
            .instances
            .iter()
            .all(|e| e.name == "Fail" && e.label.is_none() && e.args.len() == 1);
    if !fail_only {
        return Err(FreestandingError::UnsupportedEntry(format!(
            "'{entry}' has unhandled effects other than Fail; a freestanding entry handles them"
        )));
    }
    let fail = if f.effects.instances.is_empty() {
        None
    } else {
        if f.effects.instances.len() > usize::from(250 - ABORT_TRAP_BASE) {
            return Err(FreestandingError::UnsupportedEntry(
                "too many Fail instances".into(),
            ));
        }
        let mut payload = 8u32;
        let mut instances = Vec::new();
        for e in &f.effects.instances {
            let layout = crate::layout::Layout::of(world, &e.args[0])
                .map_err(FreestandingError::UnsupportedEntry)?;
            payload = payload.max(layout.size);
            instances.push(format!("Fail[{}]", e.args[0]));
        }
        Some(FailEntry {
            instances,
            record_bytes: 8 + payload.div_ceil(8) * 8,
        })
    };
    let kind = match &f.result {
        Type::Tuple(ts) if ts.is_empty() => ResultKind::Unit,
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "i64" => ResultKind::Signed,
            "u64" => ResultKind::Unsigned(64),
            "u32" => ResultKind::Unsigned(32),
            "u8" => ResultKind::Unsigned(8),
            "bool" => ResultKind::Bool,
            "Unit" => ResultKind::Unit,
            other => {
                return Err(FreestandingError::UnsupportedEntry(format!(
                    "result type '{other}' cannot be printed by the freestanding runtime (use i64, u64, u32, u8, bool or Unit)"
                )));
            }
        },
        other => {
            return Err(FreestandingError::UnsupportedEntry(format!(
                "result type '{other}' cannot be printed by the freestanding runtime (use i64, u64, u32, u8, bool or Unit)"
            )));
        }
    };
    Ok((kind, fail))
}

/// Message the trap handler prints for a backend trap code.
pub fn trap_message(code: cranelift_codegen::ir::TrapCode) -> String {
    use cranelift_codegen::ir::TrapCode;
    match code {
        c if c == TrapCode::unwrap_user(1) => "no match arm or handler selector matched".into(),
        c if c == TrapCode::unwrap_user(2) => "index out of bounds".into(),
        c if c == TrapCode::unwrap_user(3) => "no branch of a conditional chain was taken".into(),
        c if c == TrapCode::unwrap_user(4) => "invalid resumption selector".into(),
        TrapCode::INTEGER_OVERFLOW => "integer overflow".into(),
        TrapCode::INTEGER_DIVISION_BY_ZERO => "integer division by zero".into(),
        TrapCode::BAD_CONVERSION_TO_INTEGER => "bad conversion to integer".into(),
        // Cranelift records every load and store that may fault under this code.
        TrapCode::HEAP_OUT_OF_BOUNDS => "memory access fault".into(),
        TrapCode::STACK_OVERFLOW => "stack overflow".into(),
        other => format!("trap {other}"),
    }
}

/// Compile `entry` of a checked `world` into a bootable image.
pub fn build_image(
    world: &World,
    entry: &str,
    options: &ImageOptions,
) -> Result<FreestandingImage, FreestandingError> {
    let target = options.target;
    let board = options.board.clone().unwrap_or_else(|| target.qemu_virt());
    let (kind, fail) = result_kind(world, entry)?;
    if target == FreestandingTarget::Aarch64 && crate::native::needs_nan_canonicalization(world) {
        return Err(FreestandingError::UnsupportedOperation {
            function: entry.into(),
            what: "DetFloat NaN canonicalisation (not part of the analysed ISA flags)".into(),
        });
    }
    let isa = target.isa()?;
    let mut emitted = Vec::new();
    let mut observe = |f: crate::native::EmittedFunction| emitted.push(f);
    // `cranelift-object` 0.125 panics (`unimplemented!`) on relocations it
    // cannot express for an ISA; that is a typed rejection here.
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::native::object_for_isa(
            world,
            entry,
            isa.clone(),
            options.optimize,
            Some(&mut observe),
        )
    }))
    .map_err(|_| {
        FreestandingError::Codegen(
            "the object writer cannot express a relocation for this target".into(),
        )
    })?;
    let (user_object, clif) = compiled.map_err(FreestandingError::Codegen)?;
    if target == FreestandingTarget::Riscv64Imac {
        checks::reject_floating_point(&emitted)?;
    }
    let entry_fn = emitted
        .iter()
        .find(|f| f.symbol == entry)
        .ok_or_else(|| FreestandingError::Codegen(format!("entry '{entry}' was not emitted")))?;
    let entry_func_id = entry_fn.func_id;

    // Compile every function once more on its own: the trap records and the
    // code bytes, which must equal the object's bytes.
    let mut traps = Vec::new();
    let mut code_sizes = BTreeMap::new();
    for f in &emitted {
        let mut ctx = cranelift_codegen::Context::for_function(f.function.clone());
        let compiled = ctx
            .compile(&*isa, &mut ControlPlane::default())
            .map_err(|e| FreestandingError::Codegen(format!("{}: {:?}", f.symbol, e.inner)))?;
        let code = compiled.code_buffer().to_vec();
        checks::compare_code(&user_object, &f.symbol, &code)?;
        code_sizes.insert(f.symbol.clone(), code.len());
        for t in compiled.buffer.traps() {
            traps.push((f.symbol.clone(), u64::from(t.offset), t.code));
        }
    }
    let mut messages: Vec<String> = Vec::new();
    let mut trap_entries = Vec::new();
    for (function, offset, code) in &traps {
        let message = trap_message(*code);
        let index = match messages.iter().position(|m| *m == message) {
            Some(i) => i,
            None => {
                messages.push(message.clone());
                messages.len() - 1
            }
        };
        trap_entries.push(TrapEntry {
            object: "vesper.o",
            function: function.clone(),
            offset: *offset,
            code: *code,
            message: index,
        });
    }
    // The runtime's unhandled-failure aborts: one trap per Fail instance and
    // one for an unknown tag. Their offsets are known once it is compiled.
    let mut abort_messages = Vec::new();
    if let Some(fail) = &fail {
        for (i, name) in fail
            .instances
            .iter()
            .chain(["Fail".to_string()].iter())
            .enumerate()
        {
            messages.push(format!("unhandled {name}"));
            abort_messages.push((messages.len() - 1, i));
        }
    }
    let user_traps = trap_entries.len();

    let runtime = runtime::build(&runtime::RuntimeSpec {
        target,
        isa: isa.clone(),
        board: board.clone(),
        entry: entry.to_string(),
        entry_func_id,
        result: kind,
        fail: fail.clone(),
        trap_messages: messages.clone(),
        trap_entries: trap_entries
            .iter()
            .map(|e| e.message)
            .chain(abort_messages.iter().map(|(m, _)| *m))
            .collect(),
    })?;
    for (message, i) in &abort_messages {
        let code = cranelift_codegen::ir::TrapCode::unwrap_user(ABORT_TRAP_BASE + *i as u8);
        let offset = runtime
            .traps
            .iter()
            .find(|(s, _, c)| s == runtime::MAIN_SYMBOL && *c == code)
            .map(|(_, o, _)| *o)
            .ok_or_else(|| {
                FreestandingError::Codegen("runtime abort trap was not emitted".into())
            })?;
        trap_entries.push(TrapEntry {
            object: "runtime.o",
            function: runtime::MAIN_SYMBOL.into(),
            offset,
            code,
            message: *message,
        });
    }
    debug_assert_eq!(trap_entries.len(), user_traps + abort_messages.len());
    let stub = stub::build(target, &board, object_flags(&user_object)?)?;

    // Silicon: frames, worst-case depth and interrupt-mask rules.
    let analysed = checks::analyse(
        target,
        &emitted,
        &code_sizes,
        &runtime,
        options.stack_bytes,
        stub::exit_stack_bytes(target, board.exit),
    )?;

    let objects = vec![
        ("stub.o".to_string(), stub.clone()),
        ("vesper.o".to_string(), user_object),
        ("runtime.o".to_string(), runtime.object.clone()),
    ];
    let link_options = link::LinkOptions {
        machine: target,
        image_base: board.image_base,
        ram_base: board.ram_base,
        ram_bytes: board.ram_bytes,
        stack_bytes: u64::from(options.stack_bytes),
    };
    let mut linked = link::link(&objects, &link_options)?;
    let sites = resolve_traps(&linked.symbols, &trap_entries, &messages)?;
    patch_trap_table(&mut linked.elf, &linked.symbols, &linked.layout, &sites)?;
    let linker_script = script::linker_script(target, &board, options.stack_bytes);
    Ok(FreestandingImage {
        target,
        board,
        entry_function: entry.into(),
        entry_address: linked.entry,
        elf: linked.elf,
        objects,
        linker_script,
        layout: linked.layout,
        symbols: linked.symbols,
        traps: sites,
        stack: analysed.roots,
        stack_bytes: options.stack_bytes,
        frames: analysed.frames,
        clif,
    })
}

fn object_flags(object: &[u8]) -> Result<u32, FreestandingError> {
    elf::e_flags(object).map_err(FreestandingError::Object)
}

struct TrapEntry {
    object: &'static str,
    function: String,
    offset: u64,
    code: cranelift_codegen::ir::TrapCode,
    message: usize,
}

fn resolve_traps(
    symbols: &[link::LinkedSymbol],
    entries: &[TrapEntry],
    messages: &[String],
) -> Result<Vec<TrapSite>, FreestandingError> {
    entries
        .iter()
        .map(
            |TrapEntry {
                 object,
                 function,
                 offset,
                 code,
                 message,
             }| {
                let sym = symbols
                    .iter()
                    .find(|s| s.object == *object && s.name == *function)
                    .ok_or_else(|| FreestandingError::UnresolvedSymbol {
                        symbol: function.clone(),
                        object: "trap table".into(),
                    })?;
                Ok(TrapSite {
                    pc: sym.address + offset,
                    function: function.clone(),
                    code: code.to_string(),
                    message: messages[*message].clone(),
                })
            },
        )
        .collect()
}

/// Fill the program counters of the runtime's trap table in a linked image.
/// The table's message pointers were relocated by the linker; only the PCs
/// (zero in the object) are written here.
fn patch_trap_table(
    elf_bytes: &mut [u8],
    symbols: &[link::LinkedSymbol],
    layout: &link::Layout,
    sites: &[TrapSite],
) -> Result<(), FreestandingError> {
    let table = symbols
        .iter()
        .find(|s| s.name == runtime::TRAP_TABLE)
        .ok_or_else(|| FreestandingError::UnresolvedSymbol {
            symbol: runtime::TRAP_TABLE.into(),
            object: "trap table".into(),
        })?;
    let mut ranges = Vec::new();
    for seg in &layout.segments {
        ranges.push((seg.address, seg.file_offset, seg.file_bytes));
    }
    patch_table_at(elf_bytes, &ranges, table.address, sites)
}

pub(crate) fn patch_table_at(
    elf_bytes: &mut [u8],
    segments: &[(u64, u64, u64)],
    table: u64,
    sites: &[TrapSite],
) -> Result<(), FreestandingError> {
    let file_offset = |addr: u64| -> Result<usize, FreestandingError> {
        for (base, off, len) in segments {
            if addr >= *base && addr + 8 <= base + len {
                return usize::try_from(off + (addr - base))
                    .map_err(|_| FreestandingError::Object("offset overflow".into()));
            }
        }
        Err(FreestandingError::Object(format!(
            "trap table address {addr:#x} is not in a loaded segment"
        )))
    };
    let count_at = file_offset(table)?;
    let count = u64::from_le_bytes(elf_bytes[count_at..count_at + 8].try_into().unwrap());
    if count != sites.len() as u64 {
        return Err(FreestandingError::Object(format!(
            "trap table holds {count} entries, image has {} trap sites",
            sites.len()
        )));
    }
    for (i, site) in sites.iter().enumerate() {
        let at =
            file_offset(table + runtime::TRAP_HEADER_BYTES + i as u64 * runtime::TRAP_ENTRY_BYTES)?;
        elf_bytes[at..at + 8].copy_from_slice(&site.pc.to_le_bytes());
    }
    Ok(())
}

/// Link the image's objects with an external `ld.lld` (or GNU ld for the
/// target) and its [`FreestandingImage::linker_script`], then fill the trap
/// table as [`build_image`] does. Used to check that the script describes the
/// same layout; `workdir` receives the inputs and `image.lld.elf`.
pub fn link_with_external(
    image: &FreestandingImage,
    linker: &std::path::Path,
    workdir: &std::path::Path,
) -> Result<Vec<u8>, FreestandingError> {
    let io = |e: std::io::Error| FreestandingError::ExternalLinker(e.to_string());
    std::fs::create_dir_all(workdir).map_err(io)?;
    let mut args = Vec::new();
    for (name, bytes) in &image.objects {
        let path = workdir.join(name);
        std::fs::write(&path, bytes).map_err(io)?;
        args.push(path);
    }
    let script_path = workdir.join("image.ld");
    std::fs::write(&script_path, &image.linker_script).map_err(io)?;
    let out = workdir.join("image.lld.elf");
    let output = std::process::Command::new(linker)
        .arg("-static")
        .arg("--no-relax")
        .arg("-T")
        .arg(&script_path)
        .arg("-o")
        .arg(&out)
        .args(&args)
        .output()
        .map_err(io)?;
    if !output.status.success() {
        return Err(FreestandingError::ExternalLinker(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let mut bytes = std::fs::read(&out).map_err(io)?;
    let info = elf::read_executable(&bytes).map_err(FreestandingError::Object)?;
    let table = info
        .symbols
        .iter()
        .find(|(n, _)| n == runtime::TRAP_TABLE)
        .map(|(_, a)| *a)
        .ok_or_else(|| FreestandingError::ExternalLinker("no trap table symbol".into()))?;
    // Recompute PCs from the external linker's symbol addresses.
    let mut sites = Vec::new();
    for site in &image.traps {
        let ours = image
            .symbol(&site.function)
            .ok_or_else(|| FreestandingError::ExternalLinker(site.function.clone()))?;
        let theirs = info
            .symbols
            .iter()
            .find(|(n, _)| *n == site.function)
            .map(|(_, a)| *a)
            .ok_or_else(|| FreestandingError::ExternalLinker(site.function.clone()))?;
        let mut s = site.clone();
        s.pc = theirs + (site.pc - ours.address);
        sites.push(s);
    }
    patch_table_at(&mut bytes, &info.segments, table, &sites)?;
    std::fs::write(&out, &bytes).map_err(io)?;
    Ok(bytes)
}

/// The QEMU command line that boots `elf_path` on the target's `virt`
/// board with the UART on stdio: `(program, arguments)`.
pub fn qemu_command(target: FreestandingTarget, elf_path: &str) -> (&'static str, Vec<String>) {
    let common = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match target {
        FreestandingTarget::Riscv64Imac => (
            "qemu-system-riscv64",
            common(&[
                "-machine",
                "virt",
                // imac: no F/D, so any floating-point instruction would trap.
                "-cpu",
                "rv64,f=false,d=false,zfa=false",
                "-bios",
                "none",
                "-m",
                "128M",
                "-nographic",
                "-monitor",
                "none",
                "-serial",
                "stdio",
                "-kernel",
                elf_path,
            ]),
        ),
        FreestandingTarget::Aarch64 => (
            "qemu-system-aarch64",
            common(&[
                "-machine",
                "virt",
                "-cpu",
                "cortex-a53",
                "-m",
                "128M",
                "-nographic",
                "-monitor",
                "none",
                "-serial",
                "stdio",
                "-semihosting-config",
                "enable=on,target=native",
                "-kernel",
                elf_path,
            ]),
        ),
    }
}
