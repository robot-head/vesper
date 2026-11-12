//! A bounded implementation of specification §14's delimiter-count/XOR example.
//!
//! This library builds real Baseline128 machine code; it does not integrate source
//! `eff [Isa[P]]` typing, vector saturation, scalable vectors, or formal proofs.
//! Full 16-byte chunks use SIMD and the remainder uses checked scalar loads.
//! Native code is local to the creating thread. No performance claim is made.

use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_codegen::isa::{self, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::fmt;
use std::rc::Rc;
use target_lexicon::Triple;

/// Profile names from the design. Only `Baseline128` is implemented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IsaProfileKind {
    Baseline128,
    Avx2,
    Avx512,
    Sve2,
    Rvv1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedProfile(IsaProfileKind),
    UnsupportedHost,
    MissingBaselineFeatures,
    Codegen(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProfile(p) => write!(f, "SIMD profile {p:?} is not implemented"),
            Self::UnsupportedHost => f.write_str("SIMD JIT requires an x86_64 or AArch64 host"),
            Self::MissingBaselineFeatures => f.write_str("host lacks the Baseline128 CPU features"),
            Self::Codegen(e) => write!(f, "SIMD code generation failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

fn codegen(e: impl fmt::Display) -> Error {
    Error::Codegen(e.to_string())
}

#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "foreign-host variants exercise discovery without minting tokens"
)]
enum Features {
    X86 {
        sse3: bool,
        ssse3: bool,
        sse41: bool,
        sse42: bool,
    },
    Aarch64 {
        neon: bool,
    },
    Other,
}

fn check_features(features: Features) -> Result<(), Error> {
    let supported = match features {
        Features::X86 {
            sse3,
            ssse3,
            sse41,
            sse42,
        } => sse3 && ssse3 && sse41 && sse42,
        Features::Aarch64 { neon } => neon,
        Features::Other => return Err(Error::UnsupportedHost),
    };
    if supported {
        Ok(())
    } else {
        Err(Error::MissingBaselineFeatures)
    }
}

// Keep all variants compiled so discovery decisions can be tested without
// constructing a capability for a simulated CPU.
fn host_features() -> Features {
    #[cfg(target_arch = "x86_64")]
    {
        Features::X86 {
            sse3: std::is_x86_feature_detected!("sse3"),
            ssse3: std::is_x86_feature_detected!("ssse3"),
            sse41: std::is_x86_feature_detected!("sse4.1"),
            sse42: std::is_x86_feature_detected!("sse4.2"),
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        Features::Aarch64 {
            neon: std::arch::is_aarch64_feature_detected!("neon"),
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Features::Other
    }
}

/// Non-executable discovery result. Unsupported profiles remain explicit even
/// when the machine has the corresponding hardware.
pub fn profile_support(profile: IsaProfileKind) -> Result<(), Error> {
    if profile != IsaProfileKind::Baseline128 {
        return Err(Error::UnsupportedProfile(profile));
    }
    check_features(host_features())
}

/// Typed evidence of runtime Baseline128 detection on this host.
///
/// Its private field prevents safe callers from manufacturing evidence. The
/// token is local (`!Send`, `!Sync`) and copyable; it asserts CPU availability,
/// not source-language ownership or scheduling portability.
///
/// ```compile_fail
/// let forged = vesper_simd::Baseline128Capability {
///     _private: std::marker::PhantomData,
/// };
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Baseline128Capability {
    _private: std::marker::PhantomData<Rc<()>>,
}

impl Baseline128Capability {
    pub fn detect() -> Result<Self, Error> {
        profile_support(IsaProfileKind::Baseline128)?;
        Ok(Self {
            _private: std::marker::PhantomData,
        })
    }
}

/// The object target is deliberately separate from an executable capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfTarget {
    X86_64,
    Aarch64,
}

impl ElfTarget {
    fn triple(self) -> Triple {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-gnu",
            Self::Aarch64 => "aarch64-unknown-linux-gnu",
        }
        .parse()
        .expect("constant target triple")
    }
}

/// Consumers must include all these fields (and implementation identity) in
/// their object cache key; a profile name alone is not a target ABI identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeMetadata {
    pub profile: IsaProfileKind,
    pub target: String,
    pub vector_bits: u16,
    pub tail_strategy: &'static str,
    pub abi: &'static str,
    pub codegen_version: &'static str,
    /// Exact shared codegen settings, sorted by name (including disabled flags).
    pub codegen_settings: Vec<String>,
    /// Exact enabled optional ISA flags, sorted by name. No host presets.
    pub enabled_isa_flags: Vec<String>,
}

fn make_isa(triple: Triple) -> Result<OwnedTargetIsa, Error> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(codegen)?;
    flags.set("is_pic", "false").map_err(codegen)?;
    let mut builder = isa::lookup(triple.clone()).map_err(codegen)?;
    match triple.architecture {
        target_lexicon::Architecture::X86_64 => {
            // Deliberately do not use cranelift_native: it would inherit AVX,
            // POPCNT, etc. SSE4.2 and all prerequisite bits are runtime-probed.
            builder.enable("sse42").map_err(codegen)?;
        }
        target_lexicon::Architecture::Aarch64(_) => {}
        _ => return Err(Error::UnsupportedHost),
    }
    builder.finish(settings::Flags::new(flags)).map_err(codegen)
}

fn metadata(isa: &dyn isa::TargetIsa) -> CodeMetadata {
    let mut enabled_isa_flags: Vec<_> = isa
        .isa_flags()
        .iter()
        .filter(|flag| flag.as_bool() == Some(true))
        .map(|flag| flag.name.to_string())
        .collect();
    enabled_isa_flags.sort();
    let mut codegen_settings: Vec<_> = isa.flags().iter().map(|flag| flag.to_string()).collect();
    codegen_settings.sort();
    CodeMetadata {
        profile: IsaProfileKind::Baseline128,
        target: isa.triple().to_string(),
        vector_bits: 128,
        tail_strategy: "scalar-checked-remainder",
        abi: "v1:C:(ptr,u64,u8,ptr-to-u8)->u64",
        codegen_version: cranelift_codegen::VERSION,
        codegen_settings,
        enabled_isa_flags,
    }
}

// Both supported architectures have 64-bit pointers. The externally visible
// object ABI is C: uint64_t scan(const uint8_t*, uint64_t, uint8_t, uint8_t*).
// The raw ABI is private in the safe JIT API; external object users must provide
// readable [ptr, ptr+len) and a writable output byte, with no aliasing mutation.
fn define_scanner<M: Module>(module: &mut M) -> Result<(cranelift_module::FuncId, String), Error> {
    let mut ctx = module.make_context();
    ctx.func.signature.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I8),
        AbiParam::new(types::I64),
    ]);
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    let id = module
        .declare_function(
            "vesper_simd_scan_baseline128_v1",
            Linkage::Export,
            &ctx.func.signature,
        )
        .map_err(codegen)?;
    let mut frontend = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
        let entry = b.create_block();
        let vector_head = b.create_block();
        let vector_body = b.create_block();
        let reduce = b.create_block();
        let scalar_head = b.create_block();
        let scalar_body = b.create_block();
        let finish = b.create_block();
        b.append_block_params_for_function_params(entry);
        for block in [vector_head, vector_body, reduce] {
            b.append_block_param(block, types::I64); // offset
            b.append_block_param(block, types::I64); // count
            b.append_block_param(block, types::I8X16); // xor accumulator
        }
        for block in [scalar_head, scalar_body, finish] {
            b.append_block_param(block, types::I64);
            b.append_block_param(block, types::I64);
            b.append_block_param(block, types::I8);
        }
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let (input, len, delim, output) = (args[0], args[1], args[2], args[3]);
        let zero = b.ins().iconst(types::I64, 0);
        let zero_byte = b.ins().iconst(types::I8, 0);
        let zero_vec = b.ins().splat(types::I8X16, zero_byte);
        let needle = b.ins().splat(types::I8X16, delim);
        // floor(len/16)*16 cannot overflow; offsets only increase below it.
        let full_len = b.ins().band_imm(len, -16);
        b.ins()
            .jump(vector_head, &[zero.into(), zero.into(), zero_vec.into()]);

        b.switch_to_block(vector_head);
        let v = b.block_params(vector_head).to_vec();
        let more = b.ins().icmp(IntCC::UnsignedLessThan, v[0], full_len);
        b.ins().brif(
            more,
            vector_body,
            &[v[0].into(), v[1].into(), v[2].into()],
            reduce,
            &[v[0].into(), v[1].into(), v[2].into()],
        );

        b.switch_to_block(vector_body);
        let v = b.block_params(vector_body).to_vec();
        let address = b.ins().iadd(input, v[0]);
        // No aligned/notrap flags: slices and tails can start unaligned.
        let chunk = b.ins().load(types::I8X16, MemFlags::new(), address, 0);
        let eq = b.ins().icmp(IntCC::Equal, chunk, needle);
        let mut count = v[1];
        // Avoid assuming the independent x86 POPCNT feature. Each mask lane
        // is 0xff or 0, reduced to 1/0 and summed into a 64-bit count.
        for lane in 0..16 {
            let bit = b.ins().extractlane(eq, lane);
            let bit = b.ins().band_imm(bit, 1);
            let bit = b.ins().uextend(types::I64, bit);
            count = b.ins().iadd(count, bit);
        }
        let xor = b.ins().bxor(v[2], chunk);
        let next = b.ins().iadd_imm(v[0], 16);
        b.ins()
            .jump(vector_head, &[next.into(), count.into(), xor.into()]);

        b.switch_to_block(reduce);
        let v = b.block_params(reduce).to_vec();
        let mut xor = zero_byte;
        for lane in 0..16 {
            let byte = b.ins().extractlane(v[2], lane);
            xor = b.ins().bxor(xor, byte);
        }
        b.ins()
            .jump(scalar_head, &[v[0].into(), v[1].into(), xor.into()]);

        b.switch_to_block(scalar_head);
        let v = b.block_params(scalar_head).to_vec();
        let more = b.ins().icmp(IntCC::UnsignedLessThan, v[0], len);
        b.ins().brif(
            more,
            scalar_body,
            &[v[0].into(), v[1].into(), v[2].into()],
            finish,
            &[v[0].into(), v[1].into(), v[2].into()],
        );

        b.switch_to_block(scalar_body);
        let v = b.block_params(scalar_body).to_vec();
        let address = b.ins().iadd(input, v[0]);
        let byte = b.ins().load(types::I8, MemFlags::new(), address, 0);
        let equal = b.ins().icmp(IntCC::Equal, byte, delim);
        let one = b.ins().uextend(types::I64, equal);
        let count = b.ins().iadd(v[1], one);
        let xor = b.ins().bxor(v[2], byte);
        let next = b.ins().iadd_imm(v[0], 1);
        b.ins()
            .jump(scalar_head, &[next.into(), count.into(), xor.into()]);

        b.switch_to_block(finish);
        let v = b.block_params(finish).to_vec();
        b.ins().store(MemFlags::new(), v[2], output, 0);
        b.ins().return_(&[v[1]]);
        b.seal_all_blocks();
        b.finalize();
    }
    let clif = ctx.func.display().to_string();
    module.define_function(id, &mut ctx).map_err(codegen)?;
    Ok((id, clif))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanResult {
    pub matches: u64,
    pub xor: u8,
}

type Entry = unsafe extern "C" fn(*const u8, u64, u8, *mut u8) -> u64;

// Also frees allocations if code generation returns an error partway through.
struct ModuleMemory(Option<JITModule>);

impl Drop for ModuleMemory {
    fn drop(&mut self) {
        if let Some(module) = self.0.take() {
            // SAFETY: during building, code has never escaped. After building,
            // Rc is local and the last owner is being destroyed; entry is
            // private, no call can survive its scanner's borrow.
            unsafe { module.free_memory() };
        }
    }
}

struct NativeCode {
    _module: ModuleMemory,
    entry: Entry,
    metadata: CodeMetadata,
    clif: String,
}

/// Cloneable local handle retaining the JIT module until the last handle drops.
///
/// ```
/// use vesper_simd::{Baseline128Capability, JitScanner, ScanResult};
/// # fn run() -> Result<(), vesper_simd::Error> {
/// let scanner = JitScanner::compile(Baseline128Capability::detect()?)?;
/// assert_eq!(scanner.scan(b"a,a", b','), ScanResult { matches: 1, xor: b',' });
/// # Ok(())
/// # }
/// # match run() {
/// #     Err(vesper_simd::Error::MissingBaselineFeatures | vesper_simd::Error::UnsupportedHost) => {},
/// #     result => result.unwrap(),
/// # }
/// ```
///
/// ```compile_fail
/// fn requires_send<T: Send>() {}
/// requires_send::<vesper_simd::JitScanner>();
/// ```
#[derive(Clone)]
pub struct JitScanner(Rc<NativeCode>);

impl JitScanner {
    pub fn compile(_capability: Baseline128Capability) -> Result<Self, Error> {
        // Detection precedes ISA/code creation; the token cannot be fabricated
        // in safe Rust, and compile_profile also checks before reaching here.
        let isa = make_isa(Triple::host())?;
        let metadata = metadata(isa.as_ref());
        let mut memory = ModuleMemory(Some(JITModule::new(JITBuilder::with_isa(
            isa,
            default_libcall_names(),
        ))));
        let module = memory.0.as_mut().expect("module just created");
        let (id, clif) = define_scanner(module)?;
        module.finalize_definitions().map_err(codegen)?;
        let pointer = module.get_finalized_function(id);
        // SAFETY: define_scanner uses precisely this C signature and both
        // supported host targets have 64-bit pointers. Module owns the code.
        let entry = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
        Ok(Self(Rc::new(NativeCode {
            _module: memory,
            entry,
            metadata,
            clif,
        })))
    }

    pub fn compile_profile(profile: IsaProfileKind) -> Result<Self, Error> {
        profile_support(profile)?;
        Self::compile(Baseline128Capability::detect()?)
    }

    pub fn scan(&self, data: &[u8], delimiter: u8) -> ScanResult {
        let mut xor = 0;
        // SAFETY: the slice remains borrowed for the call, len fits u64 on
        // supported hosts, output is writable, and all loads are dominated by
        // the full-chunk or scalar bounds check. No input pointer escapes.
        let matches =
            unsafe { (self.0.entry)(data.as_ptr(), data.len() as u64, delimiter, &mut xor) };
        ScanResult { matches, xor }
    }

    pub fn metadata(&self) -> &CodeMetadata {
        &self.0.metadata
    }
    pub fn clif(&self) -> &str {
        &self.0.clif
    }
}

/// A real ELF relocatable object; it is data, never an executable capability.
///
/// Export: `uint64_t vesper_simd_scan_baseline128_v1(const uint8_t *data,
/// uint64_t len, uint8_t delimiter, uint8_t *xor_out)` using the target C ABI.
/// Foreign callers must establish the target's Baseline128 availability,
/// provide readable `data[0..len]` and a writable output byte, and keep the input
/// unchanged throughout the call. The Rust `JitScanner` API handles these
/// obligations for borrowed slices and never loads these cross-target objects.
pub struct ObjectArtifact {
    bytes: Vec<u8>,
    metadata: CodeMetadata,
    clif: String,
}

impl ObjectArtifact {
    pub fn emit(target: ElfTarget, profile: IsaProfileKind) -> Result<Self, Error> {
        if profile != IsaProfileKind::Baseline128 {
            return Err(Error::UnsupportedProfile(profile));
        }
        let isa = make_isa(target.triple())?;
        let metadata = metadata(isa.as_ref());
        let builder =
            ObjectBuilder::new(isa, "vesper-simd", default_libcall_names()).map_err(codegen)?;
        let mut module = ObjectModule::new(builder);
        let (_, clif) = define_scanner(&mut module)?;
        let bytes = module.finish().emit().map_err(codegen)?;
        Ok(Self {
            bytes,
            metadata,
            clif,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn metadata(&self) -> &CodeMetadata {
        &self.metadata
    }
    pub fn clif(&self) -> &str {
        &self.clif
    }
}

#[cfg(test)]
mod tests;
