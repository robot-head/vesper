//! A finite scalar kernel the shipped `vesper` compiled, loaded into this
//! process (#2670, part of #1397).
//!
//! `vesper bench` measures native code in its own process, so it cannot ask
//! the compiler to run each call. Instead `vesper tool-facts bench` hands back
//! the kernel's native batch as machine code and relocations
//! (`native::batch::compile_code` in the root package), and this module
//! loads them into a Cranelift JIT module of its own: the declarations in the
//! compiler's order (so every function keeps its id and the relocations apply
//! unchanged), then each function's bytes. Calls then run as they did when
//! the tool JIT-compiled the kernel itself (`NativeBatch` in the compiler).
use cranelift_codegen::{
    binemit::Reloc,
    ir::LibCall,
    settings::{self, Configurable},
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use serde_json::Value as Json;
use std::{fmt, str::FromStr};

/// The most calls one kernel may run (`BatchLimits::max_runs` in the
/// compiler's default limits).
const MAX_RUNS: u64 = 1_000_000;

/// The relocation kinds the compiler's batch ISA emits (non-PIC, no
/// colocated libcalls) that this loader applies, matched by their Cranelift
/// names.
const RELOCS: [Reloc; 6] = [
    Reloc::Abs4,
    Reloc::Abs8,
    Reloc::X86PCRel4,
    Reloc::X86CallPCRel4,
    Reloc::X86CallPLTRel4,
    Reloc::Arm64Call,
];

/// Every declaration linkage, matched by its Cranelift name.
const LINKAGES: [Linkage; 5] = [
    Linkage::Import,
    Linkage::Local,
    Linkage::Preemptible,
    Linkage::Hidden,
    Linkage::Export,
];

/// The function namespace of a user relocation target (`u0:<index>`).
const FUNCTION_NAMESPACE: u32 = 0;

/// The imports a kernel may name: the float remainders the backend calls
/// out to (`register_jit_runtime_symbols` in the compiler).
extern "C" fn float_remainder(a: f64, b: f64) -> f64 {
    a % b
}
extern "C" fn float_remainder_f32(a: f32, b: f32) -> f32 {
    a % b
}
const FMOD: &str = "fmod";
const FMODF: &str = "fmodf";

/// What one entry returns, as the compiler encoded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultKind {
    Signed(u32),
    Unsigned(u32),
    F64,
    F32,
    Bool,
    Unit,
}

/// The result of one kernel call, displayed as the compiler's evaluator
/// displays the same value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scalar {
    Int(i64),
    UInt(u64),
    Float(f64),
    Float32(f32),
    Bool(bool),
    Unit,
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) => write!(f, "{v}"),
            Self::UInt(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Float32(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Unit => write!(f, "()"),
        }
    }
}

/// The batch statistics the bench report carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelStats {
    pub module_count: u64,
    pub compilation_count: u64,
    pub defined_functions: u64,
    pub entry_count: u64,
    pub runs: u64,
    pub max_entry_work: u64,
}

/// A loaded kernel. Code pointers never escape this owner; calls borrow it
/// exclusively, and dropping it frees its one module after every call has
/// returned.
pub struct NativeKernel {
    module: Option<JITModule>,
    code: *const u8,
    result: ResultKind,
    stats: KernelStats,
    _single_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

fn malformed(what: &str) -> String {
    format!("the compiler's native batch is malformed ({what}); use a matching vesper")
}

fn u64_field(value: &Json, name: &str) -> Result<u64, String> {
    value[name].as_u64().ok_or_else(|| malformed(name))
}

fn u32_field(value: &Json, name: &str) -> Result<u32, String> {
    u32::try_from(u64_field(value, name)?).map_err(|_| malformed(name))
}

fn str_field<'j>(value: &'j Json, name: &str) -> Result<&'j str, String> {
    value[name].as_str().ok_or_else(|| malformed(name))
}

fn by_name<T: Copy + fmt::Debug>(table: &[T], name: &str, what: &str) -> Result<T, String> {
    table
        .iter()
        .copied()
        .find(|item| format!("{item:?}") == name)
        .ok_or_else(|| format!("native batch {what} {name} is unsupported by this tool"))
}

fn bytes(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(malformed("code"));
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).map_err(|_| malformed("code")))
        .collect()
}

fn result_kind(value: &Json) -> Result<ResultKind, String> {
    let bits = |field: &str| {
        value[field]
            .as_u64()
            .and_then(|b| u32::try_from(b).ok())
            .ok_or_else(|| malformed("result"))
    };
    if value.get("signed").is_some() {
        return Ok(ResultKind::Signed(bits("signed")?));
    }
    if value.get("unsigned").is_some() {
        return Ok(ResultKind::Unsigned(bits("unsigned")?));
    }
    match value.as_str() {
        Some("f64") => Ok(ResultKind::F64),
        Some("f32") => Ok(ResultKind::F32),
        Some("bool") => Ok(ResultKind::Bool),
        Some("unit") => Ok(ResultKind::Unit),
        _ => Err(malformed("result")),
    }
}

fn reloc(value: &Json) -> Result<ModuleReloc, String> {
    let target = &value["target"];
    let pair = |field: &str| -> Result<(u32, u32), String> {
        let items = target[field]
            .as_array()
            .ok_or_else(|| malformed("target"))?;
        let item = |i: usize| {
            items
                .get(i)
                .and_then(Json::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| malformed("target"))
        };
        Ok((item(0)?, item(1)?))
    };
    let name = if target.get("user").is_some() {
        let (namespace, index) = pair("user")?;
        if namespace != FUNCTION_NAMESPACE {
            return Err("native batch data relocations are unsupported by this tool".into());
        }
        ModuleRelocTarget::user(namespace, index)
    } else if target.get("function_offset").is_some() {
        let (func, offset) = pair("function_offset")?;
        ModuleRelocTarget::FunctionOffset(FuncId::from_u32(func), offset)
    } else if let Some(libcall) = target["libcall"].as_str() {
        ModuleRelocTarget::LibCall(LibCall::from_str(libcall).map_err(|()| malformed("libcall"))?)
    } else {
        return Err(malformed("target"));
    };
    Ok(ModuleReloc {
        offset: u32_field(value, "offset")?,
        kind: by_name(&RELOCS, str_field(value, "kind")?, "relocation")?,
        name,
        addend: value["addend"]
            .as_i64()
            .ok_or_else(|| malformed("addend"))?,
    })
}

impl NativeKernel {
    /// Load the compiler's `batch` answer and resolve its entry `entry`.
    pub fn load(batch: &Json, entry: &str) -> Result<Self, String> {
        let mut flags = settings::builder();
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(str::to_string)?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol(FMOD, float_remainder as *const u8);
        builder.symbol(FMODF, float_remainder_f32 as *const u8);
        let mut module = JITModule::new(builder);
        let result = Self::define(&mut module, batch, entry);
        let (code, result, stats) = match result {
            Ok(defined) => defined,
            Err(error) => {
                // SAFETY: nothing of this module was handed out.
                unsafe { module.free_memory() };
                return Err(error);
            }
        };
        Ok(Self {
            module: Some(module),
            code,
            result,
            stats,
            _single_thread: std::marker::PhantomData,
        })
    }

    fn define(
        module: &mut JITModule,
        batch: &Json,
        entry: &str,
    ) -> Result<(*const u8, ResultKind, KernelStats), String> {
        let list = |name: &str| batch[name].as_array().ok_or_else(|| malformed(name));
        let signature = module.make_signature();
        for (index, declaration) in list("declarations")?.iter().enumerate() {
            let linkage = by_name(&LINKAGES, str_field(declaration, "linkage")?, "linkage")?;
            let symbol = str_field(declaration, "symbol")?;
            if linkage == Linkage::Import && symbol != FMOD && symbol != FMODF {
                return Err(format!(
                    "native batch import {symbol} is unavailable in this tool"
                ));
            }
            let id = module
                .declare_function(symbol, linkage, &signature)
                .map_err(|e| e.to_string())?;
            if id.as_u32() as usize != index {
                return Err(malformed("declarations"));
            }
        }
        for function in list("functions")? {
            let relocs = function["relocs"]
                .as_array()
                .ok_or_else(|| malformed("relocs"))?
                .iter()
                .map(reloc)
                .collect::<Result<Vec<_>, _>>()?;
            module
                .define_function_bytes(
                    FuncId::from_u32(u32_field(function, "func_id")?),
                    u64_field(function, "alignment")?,
                    &bytes(str_field(function, "code")?)?,
                    &relocs,
                )
                .map_err(|e| e.to_string())?;
        }
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let entry = list("entries")?
            .iter()
            .find(|e| e["name"].as_str() == Some(entry))
            .ok_or("unknown native batch entry")?;
        let code = module.get_finalized_function(FuncId::from_u32(u32_field(entry, "func_id")?));
        let result = result_kind(&entry["result"])?;
        let stats = &batch["stats"];
        let stats = KernelStats {
            module_count: u64_field(stats, "modules")?,
            compilation_count: u64_field(stats, "compilations")?,
            defined_functions: u64_field(stats, "defined_functions")?,
            entry_count: u64_field(stats, "entries")?,
            runs: 0,
            max_entry_work: u64_field(stats, "max_entry_work")?,
        };
        Ok((code, result, stats))
    }

    pub fn stats(&self) -> KernelStats {
        self.stats
    }

    /// Call the entry once.
    pub fn run(&mut self) -> Result<Scalar, String> {
        if self.stats.runs >= MAX_RUNS {
            return Err("native batch run limit".into());
        }
        self.stats.runs = self
            .stats
            .runs
            .checked_add(1)
            .ok_or("native batch run overflow")?;
        // SAFETY: the compiler checked the kernel's complete direct-call
        // scalar closure, the absence of pointers, hidden effect and sret
        // arguments, the host C ABI, no entry arguments and the exact return
        // kind (`NativeBatch` preflight). The exclusively borrowed owner
        // keeps the executable memory alive; pointers cannot escape this API.
        let value = unsafe {
            match self.result {
                ResultKind::F64 => {
                    let call: unsafe extern "C" fn() -> f64 = std::mem::transmute(self.code);
                    Scalar::Float(call())
                }
                ResultKind::F32 => {
                    let call: unsafe extern "C" fn() -> f32 = std::mem::transmute(self.code);
                    Scalar::Float32(call())
                }
                kind => {
                    let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(self.code);
                    integer(kind, call())?
                }
            }
        };
        Ok(value)
    }
}

/// The value of an integer-lane result, checked canonical for its type.
fn integer(kind: ResultKind, n: i64) -> Result<Scalar, String> {
    const WORD_BITS: u32 = 64;
    match kind {
        ResultKind::Signed(bits) => {
            if bits < WORD_BITS && (n << (WORD_BITS - bits)) >> (WORD_BITS - bits) != n {
                return Err("noncanonical native signed result".into());
            }
            Ok(Scalar::Int(n))
        }
        ResultKind::Unsigned(bits) => {
            let value = n as u64;
            if bits < WORD_BITS && value >> bits != 0 {
                return Err("noncanonical native unsigned result".into());
            }
            Ok(Scalar::UInt(value))
        }
        ResultKind::Bool => match n {
            0 => Ok(Scalar::Bool(false)),
            1 => Ok(Scalar::Bool(true)),
            _ => Err("noncanonical native bool result".into()),
        },
        ResultKind::Unit => {
            if n != 0 {
                return Err("noncanonical native Unit result".into());
            }
            Ok(Scalar::Unit)
        }
        ResultKind::F64 | ResultKind::F32 => unreachable!("float results are read as floats"),
    }
}

impl Drop for NativeKernel {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: the entry pointer is private, calls require &mut self
            // and return only owned scalar values, and this owner is
            // !Send/!Sync.
            unsafe {
                module.free_memory();
            }
        }
    }
}
