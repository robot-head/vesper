//! Hand-built CLIF for the park/resume prototype.
//!
//! This is not source lowering: the two entries below are written directly
//! against the [`crate::clif`] helpers to exercise the ABI. The RFC describes
//! the adapter that would emit the same shapes from Place IR.
//!
//! Data-frame layout used by both entries (byte offsets):
//! * `0`: `counter: i64`, a mutable place updated before and after each park.
//! * `8`: `exit_note: i64`, written on the error/cancellation edges.
//! * `16..32`: `inbox: [u8; 16]`, filled by the resumer while parked (I/O).
//! * `32`, `40`: nested-evidence answers.
use crate::abi::{EvidenceId, exit};
use crate::clif::{self, branch_on_kind, emit_exit, emit_park, emit_violation, load_evidence};
use crate::evidence::{EvidenceError, EvidenceRegistry, EvidenceVector, Installation};
use cranelift_codegen::Context;
use cranelift_codegen::ir::{
    InstBuilder, MemFlags, StackSlotData, StackSlotKind, condcodes::IntCC, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::rc::Rc;

pub const EFFECT_CLOCK: u32 = 1;
pub const EFFECT_ASYNC: u32 = 2;
pub const EFFECT_NET: u32 = 3;
pub const EFFECT_ASK: u32 = 4;

pub const OP_NOW: u32 = 1;
pub const OP_YIELD: u32 = 2;
pub const OP_READ: u32 = 3;
pub const OP_ASK: u32 = 4;

pub const PULSE_READ_SYMBOL: &str = "vesper_proto_pulse_read";
pub const NESTED_SYMBOL: &str = "vesper_proto_nested";
/// Deliberately invalid fixture: keeps the entry data base live across a park.
pub const STALE_BASE_SYMBOL: &str = "vesper_proto_stale_base";

pub const COUNTER: usize = 0;
pub const EXIT_NOTE: usize = 8;
pub const INBOX: usize = 16;
pub const INBOX_LEN: u64 = 16;
pub const READ_LEN: u64 = 8;
pub const NESTED_INNER: usize = 32;
pub const NESTED_OUTER: usize = 40;
pub const DATA_BYTES: usize = 48;
/// Written to `exit_note` by the cancellation edge.
pub const CANCEL_NOTE: u64 = 0xCA9CE1;

/// Evidence depth requirements of each entry (outermost first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub effects: &'static [u32],
}
pub const PULSE_READ_EVIDENCE: Requirements = Requirements {
    effects: &[EFFECT_CLOCK, EFFECT_ASYNC, EFFECT_NET],
};
pub const NESTED_EVIDENCE: Requirements = Requirements {
    effects: &[EFFECT_ASK, EFFECT_ASK],
};

impl Requirements {
    /// Check a vector against the static depths generated code will load.
    pub fn check(&self, vector: &EvidenceVector) -> Result<(), String> {
        let slots = vector.slots();
        if slots.len() < self.effects.len() {
            return Err(format!(
                "entry needs {} evidence slots, vector has {}",
                self.effects.len(),
                slots.len()
            ));
        }
        for (depth, effect) in self.effects.iter().enumerate() {
            let slot = &slots[depth];
            if slot.effect != *effect || slot.depth as usize != depth {
                return Err(format!("evidence slot {depth} is not effect {effect}"));
            }
        }
        if slots[0].effect == EFFECT_CLOCK && slots[0].handler_fn.is_none() {
            return Err("Clock evidence must be tail-resumptive".into());
        }
        Ok(())
    }
}

/// Expected `pulse_read` result for `arg`, a tail-resumptive clock answer
/// `now`, and the bytes delivered into the inbox.
pub fn expected_pulse_read(arg: u64, now: u64, inbox: &[u8]) -> u64 {
    let scalar = arg.wrapping_mul(3).wrapping_add(1);
    let aggregate = arg
        .wrapping_add(100)
        .wrapping_add(0x5a5a)
        .wrapping_sub(7)
        .wrapping_add(5);
    let bytes: u64 = inbox.iter().map(|b| u64::from(*b)).sum();
    // The stack-resident mutable place starts at 5; the clock handler adds 1.
    scalar
        .wrapping_add(aggregate)
        .wrapping_add(bytes)
        .wrapping_add(now)
        .wrapping_add(6 * 1000)
}
/// Final value of the `counter` place after a successful `pulse_read`.
pub const PULSE_READ_COUNTER: u64 = 111;

/// Tail-resumptive Clock handler: `*state += 1`, `*(arg as *mut i64) += 1`,
/// returns `1000 + *state`.
///
/// # Safety
/// `state` must point to a `u64`; `arg` must be the address of an `i64`.
pub unsafe extern "C" fn clock_handler(state: *mut u8, op: u32, arg: u64) -> u64 {
    if op != OP_NOW {
        return u64::MAX;
    }
    // SAFETY: guaranteed by the caller contract above.
    unsafe {
        let calls = state.cast::<u64>();
        *calls += 1;
        let place = arg as *mut i64;
        *place += 1;
        1000 + *calls
    }
}

/// Evidence vector for `pulse_read`, owning its Clock state.
pub fn pulse_read_evidence(
    registry: &mut EvidenceRegistry,
) -> Result<(Rc<EvidenceVector>, *const u64), EvidenceError> {
    let mut calls = Box::new(0u64);
    let state: *mut u64 = &mut *calls;
    let mut vector = EvidenceVector::install(
        registry,
        &[
            // SAFETY: `state` points to the boxed u64 that `keep_alive`
            // below hands to the vector, so it lives as long as the vector;
            // `clock_handler` accepts any op and the generated code passes
            // the address of its stack-resident i64 place as `arg`.
            unsafe { Installation::direct(EFFECT_CLOCK, clock_handler, state.cast()) },
            Installation::parking(EFFECT_ASYNC),
            Installation::parking(EFFECT_NET),
        ],
    )?;
    vector.keep_alive(calls);
    Ok((Rc::new(vector), state))
}

pub fn nested_evidence(
    registry: &mut EvidenceRegistry,
) -> Result<Rc<EvidenceVector>, EvidenceError> {
    Ok(Rc::new(EvidenceVector::install(
        registry,
        &[
            Installation::parking(EFFECT_ASK),
            Installation::parking(EFFECT_ASK),
        ],
    )?))
}

/// Identities of the outer and inner `Ask` handlers.
pub fn nested_identities(vector: &EvidenceVector) -> (EvidenceId, EvidenceId) {
    (vector.identity(0).unwrap(), vector.identity(1).unwrap())
}

pub struct Ids {
    pub pulse_read: FuncId,
    pub nested: FuncId,
    pub stale_base: FuncId,
}

/// Define both prototype entries in `module`. Returns their ids and the CLIF
/// text (for inspection and review).
pub fn define<M: Module>(module: &mut M, linkage: Linkage) -> Result<(Ids, String), String> {
    let park = clif::declare_park(module).map_err(|e| e.to_string())?;
    let call_conv = module.isa().default_call_conv();
    let ptr = module.isa().pointer_type();
    let sig = clif::entry_signature(call_conv, ptr);
    let pulse_read = module
        .declare_function(PULSE_READ_SYMBOL, linkage, &sig)
        .map_err(|e| e.to_string())?;
    let nested = module
        .declare_function(NESTED_SYMBOL, linkage, &sig)
        .map_err(|e| e.to_string())?;
    let stale_base = module
        .declare_function(STALE_BASE_SYMBOL, linkage, &sig)
        .map_err(|e| e.to_string())?;
    let mut text = String::new();
    let mut fctx = FunctionBuilderContext::new();

    // ---- pulse_read ---------------------------------------------------
    let mut ctx = Context::new();
    ctx.func.signature = sig.clone();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let park_ref = module.declare_func_in_func(park, b.func);
        let handler_sig = b.import_signature(clif::handler_signature(call_conv, ptr));
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let fiber = b.block_params(entry)[0];
        let arg = b.block_params(entry)[1];
        let mem = MemFlags::trusted();
        let violation = b.create_block();
        let failed = b.create_block();
        b.append_block_param(failed, types::I64); // error code
        b.append_block_param(failed, ptr); // current data base
        let cancelled = b.create_block();
        b.append_block_param(cancelled, ptr);

        // Live scalar in an SSA value across both parks.
        let three = b.ins().imul_imm(arg, 3);
        let scalar = b.ins().iadd_imm(three, 1);
        // Live aggregate in a stack slot on the pinned fiber stack.
        let agg = b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 24, 3));
        let a = b.ins().iadd_imm(arg, 100);
        b.ins().stack_store(a, agg, 0);
        let bfield = b.ins().iconst(types::I32, 0x5a5a);
        b.ins().stack_store(bfield, agg, 8);
        let cfield = b.ins().iconst(types::I32, -7);
        b.ins().stack_store(cfield, agg, 12);
        let dfield = b.ins().f64const(2.5);
        b.ins().stack_store(dfield, agg, 16);
        // Stack-resident mutable place passed to the inline handler.
        let local =
            b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        let five = b.ins().iconst(types::I64, 5);
        b.ins().stack_store(five, local, 0);
        let local_addr = b.ins().stack_addr(ptr, local, 0);

        // counter += 1 through the entry base.
        let base0 = clif::entry_data_base(&mut b, ptr, fiber);
        let c0 = b.ins().load(types::I64, mem, base0, COUNTER as i32);
        let c1 = b.ins().iadd_imm(c0, 1);
        b.ins().store(mem, c1, base0, COUNTER as i32);

        // Tail-resumptive Clock::now via direct evidence call (no switch).
        let clock = load_evidence(&mut b, ptr, fiber, 0);
        let op_now = b.ins().iconst(types::I32, i64::from(OP_NOW));
        let now_call = b.ins().call_indirect(
            handler_sig,
            clock.handler_fn,
            &[clock.state_ptr, op_now, local_addr],
        );
        let now = b.inst_results(now_call)[0];

        // Async::yield_now parks.
        let async_ev = load_evidence(&mut b, ptr, fiber, 1);
        let zero = b.ins().iconst(types::I64, 0);
        let r1 = emit_park(
            &mut b,
            ptr,
            park_ref,
            fiber,
            async_ev.identity,
            OP_YIELD,
            zero,
            zero,
            violation,
        );
        let after_yield = b.create_block();
        let yield_error = b.create_block();
        let yield_cancel = b.create_block();
        branch_on_kind(
            &mut b,
            r1.kind,
            after_yield,
            yield_error,
            yield_cancel,
            violation,
        );
        b.switch_to_block(yield_error);
        b.ins()
            .jump(failed, &[r1.value.into(), r1.data_base.into()]);
        b.switch_to_block(yield_cancel);
        b.ins().jump(cancelled, &[r1.data_base.into()]);

        b.switch_to_block(after_yield);
        let base1 = r1.data_base;
        let c = b.ins().load(types::I64, mem, base1, COUNTER as i32);
        let c = b.ins().iadd_imm(c, 10);
        b.ins().store(mem, c, base1, COUNTER as i32);

        // Net::read_some parks with (len, inbox offset).
        let net = load_evidence(&mut b, ptr, fiber, 2);
        let len = b.ins().iconst(types::I64, READ_LEN as i64);
        let dest = b.ins().iconst(types::I64, INBOX as i64);
        let r2 = emit_park(
            &mut b,
            ptr,
            park_ref,
            fiber,
            net.identity,
            OP_READ,
            len,
            dest,
            violation,
        );
        let after_read = b.create_block();
        let read_error = b.create_block();
        let read_cancel = b.create_block();
        branch_on_kind(
            &mut b,
            r2.kind,
            after_read,
            read_error,
            read_cancel,
            violation,
        );
        b.switch_to_block(read_error);
        b.ins()
            .jump(failed, &[r2.value.into(), r2.data_base.into()]);
        b.switch_to_block(read_cancel);
        b.ins().jump(cancelled, &[r2.data_base.into()]);

        // Sum the delivered bytes: for i in 0..min(n, 16).
        b.switch_to_block(after_read);
        let base2 = r2.data_base;
        let cap = b.ins().iconst(types::I64, INBOX_LEN as i64);
        let n = b.ins().umin(r2.value, cap);
        let head = b.create_block();
        b.append_block_param(head, types::I64);
        b.append_block_param(head, types::I64);
        let body = b.create_block();
        let done = b.create_block();
        b.append_block_param(done, types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().jump(head, &[zero.into(), zero.into()]);
        b.switch_to_block(head);
        let i = b.block_params(head)[0];
        let acc = b.block_params(head)[1];
        let more = b.ins().icmp(IntCC::UnsignedLessThan, i, n);
        b.ins().brif(more, body, &[], done, &[acc.into()]);
        b.switch_to_block(body);
        let at = b.ins().iadd(base2, i);
        let byte = b.ins().uload8(types::I64, mem, at, INBOX as i32);
        let acc2 = b.ins().iadd(acc, byte);
        let i2 = b.ins().iadd_imm(i, 1);
        b.ins().jump(head, &[i2.into(), acc2.into()]);
        b.switch_to_block(done);
        let bytes = b.block_params(done)[0];

        let c = b.ins().load(types::I64, mem, base2, COUNTER as i32);
        let c = b.ins().iadd_imm(c, 100);
        b.ins().store(mem, c, base2, COUNTER as i32);

        // Read the aggregate back from the fiber stack.
        let a = b.ins().stack_load(types::I64, agg, 0);
        let bf = b.ins().stack_load(types::I32, agg, 8);
        let bf = b.ins().sextend(types::I64, bf);
        let cf = b.ins().stack_load(types::I32, agg, 12);
        let cf = b.ins().sextend(types::I64, cf);
        let df = b.ins().stack_load(types::F64, agg, 16);
        let two = b.ins().f64const(2.0);
        let df = b.ins().fmul(df, two);
        let df = b.ins().fcvt_to_sint(types::I64, df);
        let sum = b.ins().iadd(a, bf);
        let sum = b.ins().iadd(sum, cf);
        let aggregate = b.ins().iadd(sum, df);
        let place = b.ins().stack_load(types::I64, local, 0);
        let place = b.ins().imul_imm(place, 1000);

        let r = b.ins().iadd(scalar, aggregate);
        let r = b.ins().iadd(r, bytes);
        let r = b.ins().iadd(r, now);
        let r = b.ins().iadd(r, place);
        emit_exit(&mut b, fiber, exit::COMPLETE, r);

        // Error edge: record the code in the data frame, exit FAILED.
        b.switch_to_block(failed);
        let code = b.block_params(failed)[0];
        let base = b.block_params(failed)[1];
        b.ins().store(mem, code, base, EXIT_NOTE as i32);
        emit_exit(&mut b, fiber, exit::FAILED, code);

        // Cancellation edge: run cleanup (mark the place), exit CANCELLED
        // with the counter as observed by the cleanup.
        b.switch_to_block(cancelled);
        let base = b.block_params(cancelled)[0];
        let note = b.ins().iconst(types::I64, CANCEL_NOTE as i64);
        b.ins().store(mem, note, base, EXIT_NOTE as i32);
        let c = b.ins().load(types::I64, mem, base, COUNTER as i32);
        emit_exit(&mut b, fiber, exit::CANCELLED, c);

        emit_violation(&mut b, violation);
        b.seal_all_blocks();
        b.finalize();
    }
    text.push_str(&ctx.func.display().to_string());
    module
        .define_function(pulse_read, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    module.clear_context(&mut ctx);

    // ---- nested ---------------------------------------------------------
    ctx.func.signature = sig;
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let park_ref = module.declare_func_in_func(park, b.func);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let fiber = b.block_params(entry)[0];
        let arg = b.block_params(entry)[1];
        let mem = MemFlags::trusted();
        let violation = b.create_block();
        let stop = b.create_block();

        // Innermost Ask (depth 1) first, then the outer one (depth 0), as a
        // forwarded/named operation would address it.
        let inner = load_evidence(&mut b, ptr, fiber, 1);
        let r1 = emit_park(
            &mut b,
            ptr,
            park_ref,
            fiber,
            inner.identity,
            OP_ASK,
            arg,
            arg,
            violation,
        );
        let got_inner = b.create_block();
        branch_on_kind(&mut b, r1.kind, got_inner, stop, stop, violation);
        b.switch_to_block(got_inner);
        b.ins()
            .store(mem, r1.value, r1.data_base, NESTED_INNER as i32);
        let outer = load_evidence(&mut b, ptr, fiber, 0);
        let r2 = emit_park(
            &mut b,
            ptr,
            park_ref,
            fiber,
            outer.identity,
            OP_ASK,
            arg,
            arg,
            violation,
        );
        let got_outer = b.create_block();
        branch_on_kind(&mut b, r2.kind, got_outer, stop, stop, violation);
        b.switch_to_block(got_outer);
        b.ins()
            .store(mem, r2.value, r2.data_base, NESTED_OUTER as i32);
        let hi = b.ins().imul_imm(r1.value, 1000);
        let r = b.ins().iadd(hi, r2.value);
        // The identities themselves are observable: XOR them into the result
        // word's high half would hide bugs, so return them via the frame.
        emit_exit(&mut b, fiber, exit::COMPLETE, r);

        b.switch_to_block(stop);
        let zero = b.ins().iconst(types::I64, 0);
        emit_exit(&mut b, fiber, exit::FAILED, zero);
        emit_violation(&mut b, violation);
        b.seal_all_blocks();
        b.finalize();
    }
    text.push_str(&ctx.func.display().to_string());
    module
        .define_function(nested, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    module.clear_context(&mut ctx);

    // ---- stale_base (negative fixture) ----------------------------------
    // Violates the base-relative data invariant on purpose: it loads the
    // counter through the entry base after a park. Used to show that data
    // relocation is observable and that the invariant is load-bearing.
    ctx.func.signature = clif::entry_signature(call_conv, ptr);
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let park_ref = module.declare_func_in_func(park, b.func);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let fiber = b.block_params(entry)[0];
        let mem = MemFlags::trusted();
        let violation = b.create_block();
        let base0 = clif::entry_data_base(&mut b, ptr, fiber);
        let ev = load_evidence(&mut b, ptr, fiber, 1);
        let zero = b.ins().iconst(types::I64, 0);
        let r = emit_park(
            &mut b,
            ptr,
            park_ref,
            fiber,
            ev.identity,
            OP_YIELD,
            zero,
            zero,
            violation,
        );
        let _ = r;
        let stale = b.ins().load(types::I64, mem, base0, COUNTER as i32);
        emit_exit(&mut b, fiber, exit::COMPLETE, stale);
        emit_violation(&mut b, violation);
        b.seal_all_blocks();
        b.finalize();
    }
    text.push_str(&ctx.func.display().to_string());
    module
        .define_function(stale_base, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    module.clear_context(&mut ctx);
    Ok((
        Ids {
            pulse_read,
            nested,
            stale_base,
        },
        text,
    ))
}

/// Cranelift optimization level for the prototype.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    None,
    Speed,
}

impl OptLevel {
    fn name(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
        }
    }
}

fn flags(pic: bool, opt: OptLevel) -> Result<settings::Flags, String> {
    let mut f = settings::builder();
    f.set("is_pic", if pic { "true" } else { "false" })
        .map_err(|e| e.to_string())?;
    f.set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    f.set("opt_level", opt.name()).map_err(|e| e.to_string())?;
    f.set("enable_verifier", "true")
        .map_err(|e| e.to_string())?;
    Ok(settings::Flags::new(f))
}

/// Emit a relocatable object containing both entries for `triple`
/// (`x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`). The object
/// imports `vesper_fiber_park`; the CLIF text is returned for review.
pub fn emit_object(triple: &str, opt: OptLevel) -> Result<(Vec<u8>, String), String> {
    let triple: target_lexicon::Triple = triple.parse().map_err(|e| format!("{e}"))?;
    if !matches!(
        triple.architecture,
        target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
    ) {
        return Err("ABI v1 targets x86_64 and AArch64 only".into());
    }
    let isa = cranelift_codegen::isa::lookup(triple)
        .map_err(|e| e.to_string())?
        .finish(flags(true, opt)?)
        .map_err(|e| e.to_string())?;
    let builder = cranelift_object::ObjectBuilder::new(
        isa,
        "vesper_native_pulse",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| e.to_string())?;
    let mut module = cranelift_object::ObjectModule::new(builder);
    let (_, text) = define(&mut module, Linkage::Export)?;
    let bytes = module.finish().emit().map_err(|e| e.to_string())?;
    Ok((bytes, text))
}

/// JIT-compiled prototype for the host. Dropping this handle does not free the
/// machine code while any fiber created from it is alive.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub struct JitPrototype {
    code: Rc<JitCode>,
    pulse_read: crate::fiber::EntryFn,
    nested: crate::fiber::EntryFn,
    stale_base: crate::fiber::EntryFn,
    clif: String,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
struct JitCode {
    module: Option<cranelift_jit::JITModule>,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
impl Drop for JitCode {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: the last Rc is gone, so no fiber stack can return into
            // this code and no entry pointer derived from it is reachable.
            unsafe { module.free_memory() };
        }
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
impl JitPrototype {
    pub fn new(opt: OptLevel) -> Result<Self, String> {
        let isa = cranelift_native::builder()
            .map_err(str::to_string)?
            .finish(flags(false, opt)?)
            .map_err(|e| e.to_string())?;
        let mut builder =
            cranelift_jit::JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol(
            clif::PARK_SYMBOL,
            crate::fiber::vesper_fiber_park as *const u8,
        );
        let mut module = cranelift_jit::JITModule::new(builder);
        let (ids, clif) = define(&mut module, Linkage::Local)?;
        module.finalize_definitions().map_err(|e| e.to_string())?;
        // SAFETY: both functions were defined with `entry_signature`.
        let entry = |id| unsafe {
            std::mem::transmute::<*const u8, crate::fiber::EntryFn>(
                module.get_finalized_function(id),
            )
        };
        let (pulse_read, nested, stale_base) = (
            entry(ids.pulse_read),
            entry(ids.nested),
            entry(ids.stale_base),
        );
        Ok(Self {
            code: Rc::new(JitCode {
                module: Some(module),
            }),
            pulse_read,
            nested,
            stale_base,
            clif,
        })
    }
    pub fn clif(&self) -> &str {
        &self.clif
    }
    /// Observe whether the machine code is still mapped.
    pub fn code_liveness(&self) -> std::rc::Weak<dyn std::any::Any> {
        let strong: Rc<dyn std::any::Any> = self.code.clone();
        Rc::downgrade(&strong)
    }
    pub fn pulse_read_entry(&self) -> crate::fiber::EntryFn {
        self.pulse_read
    }
    pub fn nested_entry(&self) -> crate::fiber::EntryFn {
        self.nested
    }
    fn spawn(
        &self,
        entry: crate::fiber::EntryFn,
        requirements: Requirements,
        arg: u64,
        evidence: Rc<EvidenceVector>,
        config: crate::fiber::FiberConfig,
    ) -> std::io::Result<crate::fiber::Fiber> {
        requirements
            .check(&evidence)
            .map_err(std::io::Error::other)?;
        if config.data_bytes < DATA_BYTES {
            return Err(std::io::Error::other("prototype data frame is 48 bytes"));
        }
        // SAFETY: entry is code owned by `self.code`, defined with the ABI v1
        // entry signature; evidence depths were checked above.
        unsafe { crate::fiber::Fiber::new(entry, arg, evidence, self.code.clone(), config) }
    }
    pub fn spawn_pulse_read(
        &self,
        arg: u64,
        evidence: Rc<EvidenceVector>,
    ) -> std::io::Result<crate::fiber::Fiber> {
        self.spawn(
            self.pulse_read,
            PULSE_READ_EVIDENCE,
            arg,
            evidence,
            crate::fiber::FiberConfig {
                data_bytes: DATA_BYTES,
                ..Default::default()
            },
        )
    }
    /// Spawn the deliberately invalid stale-base fixture (Async at depth 1).
    pub fn spawn_stale_base(
        &self,
        evidence: Rc<EvidenceVector>,
    ) -> std::io::Result<crate::fiber::Fiber> {
        self.spawn(
            self.stale_base,
            PULSE_READ_EVIDENCE,
            0,
            evidence,
            crate::fiber::FiberConfig {
                data_bytes: DATA_BYTES,
                ..Default::default()
            },
        )
    }
    pub fn spawn_nested(
        &self,
        arg: u64,
        evidence: Rc<EvidenceVector>,
    ) -> std::io::Result<crate::fiber::Fiber> {
        self.spawn(
            self.nested,
            NESTED_EVIDENCE,
            arg,
            evidence,
            crate::fiber::FiberConfig {
                data_bytes: DATA_BYTES,
                ..Default::default()
            },
        )
    }
}
