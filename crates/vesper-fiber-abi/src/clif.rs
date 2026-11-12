//! Cranelift emission helpers for ABI v1. These are the only places that
//! encode context/evidence offsets into generated code, so a source-lowering
//! adapter can target the ABI without duplicating layout knowledge.
use crate::abi::{ResumeKind, exit, offsets};
use cranelift_codegen::ir::{
    AbiParam, Block, FuncRef, InstBuilder, MemFlags, Signature, Type, Value, condcodes::IntCC,
    types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module, ModuleResult};

/// Imported runtime symbol (see [`crate::fiber::vesper_fiber_park`]).
pub const PARK_SYMBOL: &str = "vesper_fiber_park";

/// `extern "C" fn(ctx: *mut FiberContext, arg: u64) -> u64` (exit status).
pub fn entry_signature(call_conv: CallConv, ptr: Type) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// `extern "C" fn(ctx: *mut FiberContext) -> *const ResumeSignal`.
pub fn park_signature(call_conv: CallConv, ptr: Type) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(ptr));
    sig.returns.push(AbiParam::new(ptr));
    sig
}

/// `extern "C" fn(state: *mut u8, op: u32, arg: u64) -> u64`.
pub fn handler_signature(call_conv: CallConv, ptr: Type) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I32));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Cranelift's own `ModuleError` is large; keep its type for callers.
#[allow(clippy::result_large_err)]
pub fn declare_park<M: Module>(module: &mut M) -> ModuleResult<FuncId> {
    let call_conv = module.isa().default_call_conv();
    let ptr = module.isa().pointer_type();
    module.declare_function(
        PARK_SYMBOL,
        Linkage::Import,
        &park_signature(call_conv, ptr),
    )
}

fn flags() -> MemFlags {
    MemFlags::trusted()
}

/// Loaded fields of one evidence slot.
#[derive(Clone, Copy, Debug)]
pub struct EvidenceRef {
    pub identity: Value,
    pub handler_fn: Value,
    pub state_ptr: Value,
}

/// O(1) lookup of the slot at static lexical `depth`. The adapter must ensure
/// the vector passed to the fiber has more than `depth` slots
/// ([`crate::prototype::Requirements`]).
pub fn load_evidence(b: &mut FunctionBuilder, ptr: Type, ctx: Value, depth: u32) -> EvidenceRef {
    let vector = b.ins().load(ptr, flags(), ctx, offsets::CTX_EVIDENCE);
    let slots = b.ins().load(ptr, flags(), vector, offsets::EV_SLOTS);
    let slot = b
        .ins()
        .iadd_imm(slots, i64::from(depth) * offsets::SLOT_SIZE);
    EvidenceRef {
        identity: b
            .ins()
            .load(types::I64, flags(), slot, offsets::SLOT_IDENTITY),
        handler_fn: b.ins().load(ptr, flags(), slot, offsets::SLOT_HANDLER_FN),
        state_ptr: b.ins().load(ptr, flags(), slot, offsets::SLOT_STATE_PTR),
    }
}

/// Data-frame base at entry. Only valid until the first park.
pub fn entry_data_base(b: &mut FunctionBuilder, ptr: Type, ctx: Value) -> Value {
    b.ins().load(ptr, flags(), ctx, offsets::CTX_DATA_BASE)
}

/// Values available after a park returns.
#[derive(Clone, Copy, Debug)]
pub struct ParkResult {
    pub kind: Value,
    pub value: Value,
    /// The only valid data-frame base after this park.
    pub data_base: Value,
}

/// Fill `ctx.park`, call `vesper_fiber_park`, and validate the signal.
///
/// Control continues in a fresh block on success. A null signal or a signal
/// for another evidence identity jumps to `violation` (which takes no
/// parameters). Every SSA value defined before the call stays usable after
/// it: the switch preserves callee-saved registers and the pinned stack.
#[allow(clippy::too_many_arguments)]
pub fn emit_park(
    b: &mut FunctionBuilder,
    ptr: Type,
    park: FuncRef,
    ctx: Value,
    evidence: Value,
    op: u32,
    arg0: Value,
    arg1: Value,
    violation: Block,
) -> ParkResult {
    let park_at = offsets::CTX_PARK;
    b.ins()
        .store(flags(), evidence, ctx, park_at + offsets::PARK_EVIDENCE);
    let op = b.ins().iconst(types::I32, i64::from(op));
    b.ins().store(flags(), op, ctx, park_at + offsets::PARK_OP);
    let zero = b.ins().iconst(types::I32, 0);
    b.ins()
        .store(flags(), zero, ctx, park_at + offsets::PARK_FLAGS);
    b.ins()
        .store(flags(), arg0, ctx, park_at + offsets::PARK_ARG0);
    b.ins()
        .store(flags(), arg1, ctx, park_at + offsets::PARK_ARG1);
    let call = b.ins().call(park, &[ctx]);
    let signal = b.inst_results(call)[0];

    let check_identity = b.create_block();
    b.ins().brif(signal, check_identity, &[], violation, &[]);
    b.switch_to_block(check_identity);
    b.seal_block(check_identity);
    let echoed = b
        .ins()
        .load(types::I64, flags(), signal, offsets::RESUME_EVIDENCE);
    let same = b.ins().icmp(IntCC::Equal, echoed, evidence);
    let resumed = b.create_block();
    b.ins().brif(same, resumed, &[], violation, &[]);
    b.switch_to_block(resumed);
    b.seal_block(resumed);
    ParkResult {
        kind: b
            .ins()
            .load(types::I32, flags(), signal, offsets::RESUME_KIND),
        value: b
            .ins()
            .load(types::I64, flags(), signal, offsets::RESUME_VALUE),
        data_base: b
            .ins()
            .load(ptr, flags(), signal, offsets::RESUME_DATA_BASE),
    }
}

/// Branch on a resume kind. Unknown kinds go to `violation`.
pub fn branch_on_kind(
    b: &mut FunctionBuilder,
    kind: Value,
    on_value: Block,
    on_error: Block,
    on_cancel: Block,
    violation: Block,
) {
    let not_value = b.create_block();
    let not_error = b.create_block();
    let is_value = b
        .ins()
        .icmp_imm(IntCC::Equal, kind, ResumeKind::Value as i64);
    b.ins().brif(is_value, on_value, &[], not_value, &[]);
    b.switch_to_block(not_value);
    b.seal_block(not_value);
    let is_error = b
        .ins()
        .icmp_imm(IntCC::Equal, kind, ResumeKind::Error as i64);
    b.ins().brif(is_error, on_error, &[], not_error, &[]);
    b.switch_to_block(not_error);
    b.seal_block(not_error);
    let is_cancel = b
        .ins()
        .icmp_imm(IntCC::Equal, kind, ResumeKind::Cancelled as i64);
    b.ins().brif(is_cancel, on_cancel, &[], violation, &[]);
}

/// Store the result word and return `status` from the entry.
pub fn emit_exit(b: &mut FunctionBuilder, ctx: Value, status: u64, result: Value) {
    b.ins().store(flags(), result, ctx, offsets::CTX_RESULT);
    let status = b.ins().iconst(types::I64, status as i64);
    b.ins().return_(&[status]);
}

/// Fill a `violation` block: return [`exit::ABI_VIOLATION`].
pub fn emit_violation(b: &mut FunctionBuilder, violation: Block) {
    b.switch_to_block(violation);
    let status = b.ins().iconst(types::I64, exit::ABI_VIOLATION as i64);
    b.ins().return_(&[status]);
}
