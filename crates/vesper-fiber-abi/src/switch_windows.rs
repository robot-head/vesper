//! Context-switch trampolines for Windows x64 (Microsoft x64 calling
//! convention, COFF).
//!
//! `vesper_fiber_switch(save_sp, load_sp, arg) -> arg` has the same contract
//! as on Linux and macOS: it spills the current context's callee-saved state
//! onto its own stack, stores the resulting stack pointer into `*save_sp`,
//! installs `load_sp`, restores the target context's state from that stack
//! and returns `arg` to it. Arguments arrive in rcx, rdx and r8.
//!
//! The Microsoft x64 convention preserves more than System V: rbx, rbp, rdi,
//! rsi, r12–r15, **xmm6–xmm15** (all 128 bits), the MXCSR control bits and
//! the x87 control word. A Windows context additionally owns four TEB fields
//! that the kernel and the exception dispatcher consult: `NT_TIB::StackBase`
//! (`gs:[0x08]`), `NT_TIB::StackLimit` (`gs:[0x10]`), `TEB::DeallocationStack`
//! (`gs:[0x1478]`) and `TEB::GuaranteedStackBytes` (`gs:[0x1748]`). They are
//! switched with the registers, as `SwitchToFiber` does, so that guard-page
//! overflow detection and exception dispatch see the stack that is actually
//! running (see `stack_windows.rs`).
//!
//! Saved switch frame (288 bytes, low to high at the saved stack pointer,
//! which is 16-byte aligned):
//!
//! | offset | contents |
//! | --- | --- |
//! | 0 | `mxcsr u32`, `x87 control word u16`, 2 bytes padding (as ABI v2 x86_64) |
//! | 8 | `GuaranteedStackBytes` (u32, zero-extended) |
//! | 16 | `DeallocationStack` |
//! | 24 | `StackLimit` |
//! | 32 | `StackBase` |
//! | 40 | padding |
//! | 48 | xmm6 … xmm15, 16 bytes each |
//! | 208 | padding |
//! | 216 | r15 r14 r13 r12 rsi rdi rbx rbp |
//! | 280 | return address |
//!
//! The `repr(C)` context, evidence and data layouts of ABI v2 are unchanged;
//! only this target-private frame (never read by generated code) is larger,
//! as the AArch64 one is.
//!
//! Unwinding. `vesper_fiber_start`, the outermost frame of every fiber, has
//! real x64 unwind information (`.seh_proc`, a 32-byte allocation for the
//! callee's shadow space) and its "return address" slot is zero, so the
//! exception dispatcher, `RtlVirtualUnwind` and backtraces stop cleanly at the
//! base of a fiber stack instead of walking into the scheduler's frames. The
//! switch itself carries no unwind information: it is never on the call chain
//! of running code (a resumed context has already returned from it), so no
//! exception can be dispatched through it. Rust panics still abort at the
//! `extern "C"` entry boundary. Not supported: AVX upper halves (volatile in
//! the Microsoft convention), x87 data registers in use across a switch,
//! fiber-local storage (`FlsAlloc`, `NT_TIB::FiberData`), and CET shadow
//! stacks (the switch returns to a different stack than it was called from;
//! binaries are not built `/CETCOMPAT`).
use crate::abi::FiberContext;
use crate::stack::TebLimits;

unsafe extern "C" {
    /// See module docs. `save_sp` must be writable; `load_sp` must be a stack
    /// pointer produced by this function or by [`initial_stack`].
    pub fn vesper_fiber_switch(save_sp: *mut u64, load_sp: u64, arg: u64) -> u64;
    fn vesper_fiber_start();
}

std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl vesper_fiber_switch",
    ".def vesper_fiber_switch",
    ".scl 2",
    ".type 32",
    ".endef",
    "vesper_fiber_switch:",
    "push rbp",
    "push rbx",
    "push rdi",
    "push rsi",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 216",
    "stmxcsr [rsp]",
    "fnstcw [rsp + 4]",
    "mov eax, dword ptr gs:[0x1748]",
    "mov [rsp + 8], rax",
    "mov rax, qword ptr gs:[0x1478]",
    "mov [rsp + 16], rax",
    "mov rax, qword ptr gs:[0x10]",
    "mov [rsp + 24], rax",
    "mov rax, qword ptr gs:[0x08]",
    "mov [rsp + 32], rax",
    "movaps [rsp + 48], xmm6",
    "movaps [rsp + 64], xmm7",
    "movaps [rsp + 80], xmm8",
    "movaps [rsp + 96], xmm9",
    "movaps [rsp + 112], xmm10",
    "movaps [rsp + 128], xmm11",
    "movaps [rsp + 144], xmm12",
    "movaps [rsp + 160], xmm13",
    "movaps [rsp + 176], xmm14",
    "movaps [rsp + 192], xmm15",
    "mov [rcx], rsp",
    "mov rsp, rdx",
    "ldmxcsr [rsp]",
    "fldcw [rsp + 4]",
    "mov rax, [rsp + 8]",
    "mov dword ptr gs:[0x1748], eax",
    "mov rax, [rsp + 16]",
    "mov qword ptr gs:[0x1478], rax",
    "mov rax, [rsp + 24]",
    "mov qword ptr gs:[0x10], rax",
    "mov rax, [rsp + 32]",
    "mov qword ptr gs:[0x08], rax",
    "movaps xmm6, [rsp + 48]",
    "movaps xmm7, [rsp + 64]",
    "movaps xmm8, [rsp + 80]",
    "movaps xmm9, [rsp + 96]",
    "movaps xmm10, [rsp + 112]",
    "movaps xmm11, [rsp + 128]",
    "movaps xmm12, [rsp + 144]",
    "movaps xmm13, [rsp + 160]",
    "movaps xmm14, [rsp + 176]",
    "movaps xmm15, [rsp + 192]",
    "add rsp, 216",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rsi",
    "pop rdi",
    "pop rbx",
    "pop rbp",
    "mov rax, r8",
    "ret",
    "",
    ".p2align 4",
    ".globl vesper_fiber_start",
    ".def vesper_fiber_start",
    ".scl 2",
    ".type 32",
    ".endef",
    // Entered by the first switch's `ret` with rsp 16-byte aligned and a zero
    // "return address" at [rsp]. r12 = context, rbx = fiber_main, rax = first
    // resume argument. The unwind info describes the shadow-space allocation,
    // so an unwinder computes caller rip = [rsp + 32] = 0 and stops.
    "vesper_fiber_start:",
    ".seh_proc vesper_fiber_start",
    "sub rsp, 32",
    ".seh_stackalloc 32",
    ".seh_endprologue",
    "mov rcx, r12",
    "mov rdx, rax",
    "call rbx",
    "ud2",
    ".seh_endproc",
);

/// Size of the saved switch frame in bytes (Windows x64; see module docs).
pub const SWITCH_FRAME_BYTES: usize = 288;

/// Marks the top of a fiber stack: `[StackBase - 16]` holds this value and
/// `[StackBase - 8]` the fiber's context, so a vectored exception handler can
/// tell from the TEB alone whether a fault happened on a fiber, and which.
pub const STACK_HEADER_MAGIC: u64 = u64::from_le_bytes(*b"VFIBSTK\0");

/// Bytes at the top of a fiber stack reserved for the unwind terminator and
/// the [`STACK_HEADER_MAGIC`] header.
const TOP_RESERVED: u64 = 32;

/// Build the initial saved frame so that the first `vesper_fiber_switch` into
/// `top` enters `main(ctx, arg)` on the new stack with the fiber's own TEB
/// stack limits installed.
///
/// A new fiber starts with the target's default floating-point environment
/// ([`crate::fpenv::FpEnv::DEFAULT`]), independent of the creating thread's,
/// and with xmm6–xmm15 zeroed.
///
/// # Safety
/// `top` must be the 16-byte aligned top of a writable stack of at least
/// 512 bytes that nothing else uses, described by `limits`.
pub unsafe fn initial_stack(
    top: u64,
    ctx: *mut FiberContext,
    main: unsafe extern "C" fn(*mut FiberContext, u64) -> !,
    limits: TebLimits,
) -> u64 {
    debug_assert_eq!(top % 16, 0);
    debug_assert_eq!(limits.stack_base, top);
    let start = vesper_fiber_start as *const () as u64;
    let main = main as *const () as u64;
    // After the switch's `ret`, rsp = sp + SWITCH_FRAME_BYTES = top - 32,
    // which is 16-byte aligned and holds the zero unwind terminator.
    let sp = top - TOP_RESERVED - SWITCH_FRAME_BYTES as u64;
    debug_assert_eq!(sp % 16, 0);
    let p = sp as *mut u64;
    let words = (SWITCH_FRAME_BYTES as u64 + TOP_RESERVED) / 8;
    for i in 0..words as usize {
        // SAFETY: inside the caller-provided stack.
        unsafe { p.add(i).write(0) };
    }
    let env = crate::fpenv::FpEnv::DEFAULT;
    // SAFETY: every index below is inside [sp, top).
    unsafe {
        p.add(0)
            .write(u64::from(env.mxcsr) | (u64::from(env.x87_control) << 32));
        p.add(1).write(u64::from(limits.guaranteed_bytes));
        p.add(2).write(limits.deallocation_stack);
        p.add(3).write(limits.stack_limit);
        p.add(4).write(limits.stack_base);
        // 216: r15 r14 r13 r12 rsi rdi rbx rbp, 280: return address.
        p.add(30).write(ctx as u64); // r12
        p.add(33).write(main); // rbx
        p.add(35).write(start); // return address
        // [top - 32], [top - 24]: zero (unwind terminator, already written).
        p.add(38).write(STACK_HEADER_MAGIC);
        p.add(39).write(ctx as u64);
    }
    sp
}

/// The fiber context recorded at the top of the stack the current thread is
/// running on, if that stack is a fiber stack. Reads only the TEB and the
/// top 16 bytes of the current stack, so it is usable from a vectored
/// exception handler.
pub fn current_fiber_context() -> Option<*mut FiberContext> {
    let base: u64;
    // SAFETY: reads NT_TIB::StackBase of the current thread.
    unsafe {
        std::arch::asm!(
            "mov {}, qword ptr gs:[0x08]",
            out(reg) base,
            options(nostack, readonly, preserves_flags),
        );
    }
    if base < 16 {
        return None;
    }
    // SAFETY: the top 16 bytes below StackBase are part of the current
    // thread's committed stack (the outermost frame lives there).
    let (magic, ctx) = unsafe {
        (
            std::ptr::read_volatile((base - 16) as *const u64),
            std::ptr::read_volatile((base - 8) as *const u64),
        )
    };
    (magic == STACK_HEADER_MAGIC && ctx != 0).then_some(ctx as *mut FiberContext)
}

/// The current thread's TEB stack fields, as the switch saves them.
pub fn current_teb_limits() -> TebLimits {
    let (base, limit, dealloc): (u64, u64, u64);
    let guaranteed: u32;
    // SAFETY: reads four fields of the current thread's TEB.
    unsafe {
        std::arch::asm!(
            "mov {b}, qword ptr gs:[0x08]",
            "mov {l}, qword ptr gs:[0x10]",
            "mov {d}, qword ptr gs:[0x1478]",
            "mov {g:e}, dword ptr gs:[0x1748]",
            b = out(reg) base,
            l = out(reg) limit,
            d = out(reg) dealloc,
            g = out(reg) guaranteed,
            options(nostack, readonly, preserves_flags),
        );
    }
    TebLimits {
        stack_base: base,
        stack_limit: limit,
        deallocation_stack: dealloc,
        guaranteed_bytes: guaranteed,
    }
}
