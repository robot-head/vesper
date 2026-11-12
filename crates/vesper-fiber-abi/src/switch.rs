//! Context-switch trampolines (System V x86_64 and AAPCS64 Linux).
//!
//! `vesper_fiber_switch(save_sp, load_sp, arg) -> arg` spills the callee-saved
//! registers of the current context onto its own stack, stores the resulting
//! stack pointer into `*save_sp`, installs `load_sp`, restores the target
//! context's callee-saved registers from that stack and returns `arg` to it.
//! Caller-saved registers are clobbered as for any call, so the Rust and
//! Cranelift callers need no special treatment.
//!
//! Saved register area (low to high, at the saved stack pointer):
//! * x86_64: r15 r14 r13 r12 rbx rbp, then the return address (48 + 8 bytes).
//! * AArch64: x19..x28, x29 (fp), x30 (lr), d8..d15 (176 bytes, 16-aligned).
//!
//! Not saved: x86 MXCSR/x87 control words, AArch64 FPCR. Generated code in
//! this prototype does not change them; a production runtime that allows
//! floating-point environment changes must add them to this area.
use crate::abi::FiberContext;

unsafe extern "C" {
    /// See module docs. `save_sp` must be writable; `load_sp` must be a stack
    /// pointer produced by this function or by [`initial_stack`].
    pub fn vesper_fiber_switch(save_sp: *mut u64, load_sp: u64, arg: u64) -> u64;
    fn vesper_fiber_start();
}

#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl vesper_fiber_switch",
    ".type vesper_fiber_switch,@function",
    "vesper_fiber_switch:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "mov rax, rdx",
    "ret",
    ".size vesper_fiber_switch, .-vesper_fiber_switch",
    "",
    ".p2align 4",
    ".type vesper_fiber_start,@function",
    "vesper_fiber_start:",
    // r12 = context, rbx = fiber_main, rax = first resume argument.
    // rsp is 16-byte aligned here, as required before a call.
    "mov rdi, r12",
    "mov rsi, rax",
    "call rbx",
    "ud2",
    ".size vesper_fiber_start, .-vesper_fiber_start",
);

#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl vesper_fiber_switch",
    ".type vesper_fiber_switch,%function",
    "vesper_fiber_switch:",
    "sub sp, sp, #176",
    "stp x19, x20, [sp, #0]",
    "stp x21, x22, [sp, #16]",
    "stp x23, x24, [sp, #32]",
    "stp x25, x26, [sp, #48]",
    "stp x27, x28, [sp, #64]",
    "stp x29, x30, [sp, #80]",
    "stp d8, d9, [sp, #96]",
    "stp d10, d11, [sp, #112]",
    "stp d12, d13, [sp, #128]",
    "stp d14, d15, [sp, #144]",
    "mov x9, sp",
    "str x9, [x0]",
    "mov sp, x1",
    "ldp x19, x20, [sp, #0]",
    "ldp x21, x22, [sp, #16]",
    "ldp x23, x24, [sp, #32]",
    "ldp x25, x26, [sp, #48]",
    "ldp x27, x28, [sp, #64]",
    "ldp x29, x30, [sp, #80]",
    "ldp d8, d9, [sp, #96]",
    "ldp d10, d11, [sp, #112]",
    "ldp d12, d13, [sp, #128]",
    "ldp d14, d15, [sp, #144]",
    "add sp, sp, #176",
    "mov x0, x2",
    "ret",
    ".size vesper_fiber_switch, .-vesper_fiber_switch",
    "",
    ".p2align 4",
    ".type vesper_fiber_start,%function",
    "vesper_fiber_start:",
    // x20 = context, x19 = fiber_main, x0 = first resume argument.
    "mov x1, x0",
    "mov x0, x20",
    "blr x19",
    "brk #0",
    ".size vesper_fiber_start, .-vesper_fiber_start",
);

/// Build the initial saved register area so that the first
/// `vesper_fiber_switch` into `top` enters `main(ctx, arg)` on the new stack.
///
/// # Safety
/// `top` must be the 16-byte aligned top of a writable stack of at least
/// 256 bytes that nothing else uses.
pub unsafe fn initial_stack(
    top: u64,
    ctx: *mut FiberContext,
    main: unsafe extern "C" fn(*mut FiberContext, u64) -> !,
) -> u64 {
    debug_assert_eq!(top % 16, 0);
    let start = vesper_fiber_start as *const () as u64;
    let main = main as *const () as u64;
    let ctx = ctx as u64;
    #[cfg(target_arch = "x86_64")]
    {
        // [sp]: r15 r14 r13 r12 rbx rbp ret ; after `ret`, rsp = sp + 56,
        // which must be 16-byte aligned for the `call` in vesper_fiber_start.
        let sp = top - 72;
        let words = [0, 0, 0, ctx, main, 0, start];
        let p = sp as *mut u64;
        for (i, w) in words.iter().enumerate() {
            // SAFETY: inside the caller-provided stack.
            unsafe { p.add(i).write(*w) };
        }
        // Zero the 16 bytes above the return address (terminates unwinders).
        unsafe {
            p.add(7).write(0);
            p.add(8).write(0);
        }
        sp
    }
    #[cfg(target_arch = "aarch64")]
    {
        let sp = top - 176;
        let p = sp as *mut u64;
        for i in 0..22 {
            // SAFETY: inside the caller-provided stack.
            unsafe { p.add(i).write(0) };
        }
        unsafe {
            p.add(0).write(main); // x19
            p.add(1).write(ctx); // x20
            p.add(10).write(0); // x29
            p.add(11).write(start); // x30
        }
        sp
    }
}
