//! Context-switch trampolines (System V x86_64 Linux, AAPCS64 Linux and the
//! Apple arm64 ABI on macOS).
//!
//! `vesper_fiber_switch(save_sp, load_sp, arg) -> arg` spills the callee-saved
//! registers of the current context onto its own stack, stores the resulting
//! stack pointer into `*save_sp`, installs `load_sp`, restores the target
//! context's callee-saved registers from that stack and returns `arg` to it.
//! Caller-saved registers are clobbered as for any call, so the Rust and
//! Cranelift callers need no special treatment.
//!
//! Saved switch frame, ABI v2 (low to high, at the saved stack pointer):
//! * x86_64 (64 bytes): `mxcsr u32`, `x87 control word u16`, 2 bytes padding,
//!   then r15 r14 r13 r12 rbx rbp and the return address.
//! * AArch64 (192 bytes, 16-aligned): x19..x28, x29 (fp), x30 (lr), d8..d15,
//!   then `fpcr u64` at +160 and `fpsr u64` at +168, 16 bytes padding.
//!
//! These are exactly the registers and floating-point control state that the
//! target C ABI requires a callee to preserve (System V: rbx rbp r12–r15,
//! MXCSR control bits, x87 control word; AAPCS64: x19–x29, sp, the low 64 bits
//! of v8–v15, FPCR). Every context therefore owns its floating-point
//! environment: a fiber that changes its rounding mode does not change the
//! scheduler's, and vice versa. The x86 MXCSR status bits and the AArch64
//! FPSR (sticky exception flags, not callee-saved) are switched along with the
//! control bits, so exception flags are also context-local. Not supported:
//! x87 data registers or status word in use across a switch (the ABI requires
//! an empty x87 stack at calls), AVX upper halves (caller-saved), SVE/SME
//! state, and any use of x18 (caller-saved on Linux, reserved by Darwin; the
//! switch never touches it, so a Darwin context keeps whatever the kernel
//! maintains there). The trampolines carry no CFI: unwinding across a switch is not
//! supported, and panics abort at the `extern "C"` boundaries.
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
    "sub rsp, 8",
    "stmxcsr [rsp]",
    "fnstcw [rsp + 4]",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "ldmxcsr [rsp]",
    "fldcw [rsp + 4]",
    "add rsp, 8",
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
    // Global (hidden) so Rust code in any codegen unit can reference it.
    ".globl vesper_fiber_start",
    ".hidden vesper_fiber_start",
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

// The AArch64 switch body is shared by ELF (Linux) and Mach-O (Darwin) so the
// v2 frame layout cannot diverge between them; only symbol spelling and
// assembler directives are platform-specific. x18 is neither read nor written:
// Darwin reserves it as the platform register, Linux treats it as temporary.
#[cfg(target_arch = "aarch64")]
macro_rules! aarch64_switch_body {
    () => {
        concat!(
            "sub sp, sp, #192\n",
            "stp x19, x20, [sp, #0]\n",
            "stp x21, x22, [sp, #16]\n",
            "stp x23, x24, [sp, #32]\n",
            "stp x25, x26, [sp, #48]\n",
            "stp x27, x28, [sp, #64]\n",
            "stp x29, x30, [sp, #80]\n",
            "stp d8, d9, [sp, #96]\n",
            "stp d10, d11, [sp, #112]\n",
            "stp d12, d13, [sp, #128]\n",
            "stp d14, d15, [sp, #144]\n",
            "mrs x9, fpcr\n",
            "mrs x10, fpsr\n",
            "stp x9, x10, [sp, #160]\n",
            "mov x9, sp\n",
            "str x9, [x0]\n",
            "mov sp, x1\n",
            "ldp x19, x20, [sp, #0]\n",
            "ldp x21, x22, [sp, #16]\n",
            "ldp x23, x24, [sp, #32]\n",
            "ldp x25, x26, [sp, #48]\n",
            "ldp x27, x28, [sp, #64]\n",
            "ldp x29, x30, [sp, #80]\n",
            "ldp d8, d9, [sp, #96]\n",
            "ldp d10, d11, [sp, #112]\n",
            "ldp d12, d13, [sp, #128]\n",
            "ldp d14, d15, [sp, #144]\n",
            "ldp x9, x10, [sp, #160]\n",
            "msr fpcr, x9\n",
            "msr fpsr, x10\n",
            "add sp, sp, #192\n",
            "mov x0, x2\n",
            "ret\n",
        )
    };
}

// x20 = context, x19 = fiber_main, x0 = first resume argument.
#[cfg(target_arch = "aarch64")]
macro_rules! aarch64_start_body {
    () => {
        concat!("mov x1, x0\n", "mov x0, x20\n", "blr x19\n", "brk #0\n",)
    };
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl vesper_fiber_switch",
    ".type vesper_fiber_switch,%function",
    "vesper_fiber_switch:",
    aarch64_switch_body!(),
    ".size vesper_fiber_switch, .-vesper_fiber_switch",
    "",
    ".p2align 4",
    ".globl vesper_fiber_start",
    ".hidden vesper_fiber_start",
    ".type vesper_fiber_start,%function",
    "vesper_fiber_start:",
    aarch64_start_body!(),
    ".size vesper_fiber_start, .-vesper_fiber_start",
);

// Mach-O: C symbols carry a leading underscore, `.private_extern` is the
// hidden-visibility spelling, and there are no `.type`/`.size` directives.
// Darwin arm64 keeps the AAPCS64 callee-saved set (x19-x28, x29 frame record,
// d8-d15 low halves) and 16-byte SP alignment; macOS arm64 user code is not
// arm64e, so the saved x30 carries no pointer-authentication signature.
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl _vesper_fiber_switch",
    "_vesper_fiber_switch:",
    aarch64_switch_body!(),
    "",
    ".p2align 4",
    ".globl _vesper_fiber_start",
    ".private_extern _vesper_fiber_start",
    "_vesper_fiber_start:",
    aarch64_start_body!(),
);

/// Size of the saved switch frame in bytes (ABI v2).
#[cfg(target_arch = "x86_64")]
pub const SWITCH_FRAME_BYTES: usize = 64;
/// Size of the saved switch frame in bytes (ABI v2).
#[cfg(target_arch = "aarch64")]
pub const SWITCH_FRAME_BYTES: usize = 192;

/// Build the initial saved register area so that the first
/// `vesper_fiber_switch` into `top` enters `main(ctx, arg)` on the new stack.
///
/// A new fiber starts with the target's default floating-point environment
/// ([`crate::fpenv::FpEnv::DEFAULT`]: round to nearest, all exceptions
/// masked, no flush-to-zero), independent of the creating thread's.
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
        // [sp]: fpenv r15 r14 r13 r12 rbx rbp ret ; after `ret`,
        // rsp = sp + 64, which must be 16-byte aligned for the `call` in
        // vesper_fiber_start.
        let sp = top - 80;
        let env = crate::fpenv::FpEnv::DEFAULT;
        let fp_word = u64::from(env.mxcsr) | (u64::from(env.x87_control) << 32);
        let words = [fp_word, 0, 0, 0, ctx, main, 0, start];
        let p = sp as *mut u64;
        for (i, w) in words.iter().enumerate() {
            // SAFETY: inside the caller-provided stack.
            unsafe { p.add(i).write(*w) };
        }
        // Zero the 16 bytes above the return address (terminates unwinders).
        unsafe {
            p.add(8).write(0);
            p.add(9).write(0);
        }
        debug_assert_eq!((sp + 64) % 16, 0);
        sp
    }
    #[cfg(target_arch = "aarch64")]
    {
        let sp = top - SWITCH_FRAME_BYTES as u64;
        let p = sp as *mut u64;
        for i in 0..SWITCH_FRAME_BYTES / 8 {
            // SAFETY: inside the caller-provided stack.
            unsafe { p.add(i).write(0) };
        }
        unsafe {
            p.add(0).write(main); // x19
            p.add(1).write(ctx); // x20
            p.add(10).write(0); // x29
            p.add(11).write(start); // x30
            p.add(20).write(crate::fpenv::FpEnv::DEFAULT.fpcr); // fpcr
            p.add(21).write(0); // fpsr
        }
        sp
    }
}
