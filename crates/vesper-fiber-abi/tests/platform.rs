//! Platform checks for the direct pinned-fiber port (Linux x86_64/AArch64 and
//! macOS AArch64), independent of the Linux-only Pulse scheduler. Windows x64
//! has its own version in `windows_platform.rs` (different calling convention
//! and fault reporting):
//!
//! * the guard page below a pinned stack really faults, both when touched
//!   directly and when a running fiber overflows into it;
//! * callee-saved general-purpose and vector registers survive interleaved
//!   switches, checked register by register in assembly on both sides of the
//!   switch (the host context and each fiber hold distinct patterns);
//! * floating-point control (rounding) and cumulative status flags are owned
//!   by each context, with fibers starting from the ABI default.
//!
//! The stack-frame ABI offsets and v2 layouts are covered by the unit tests in
//! `abi.rs`; this file exercises the machine behaviour of the current target.
#![cfg(all(vesper_direct_fibers, unix))]
use std::any::Any;
use std::process::Command;
use std::rc::Rc;
use vesper_fiber_abi::abi::{FiberContext, ParkRequest, ResumeSignal, exit};
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector, Installation};
use vesper_fiber_abi::fiber::vesper_fiber_park;
use vesper_fiber_abi::fpenv::{self, Rounding};
use vesper_fiber_abi::prototype::{EFFECT_ASYNC, OP_YIELD};
use vesper_fiber_abi::stack::FiberStack;
use vesper_fiber_abi::{Fiber, FiberConfig, Outcome, Parked, Resumption};

fn evidence() -> Rc<EvidenceVector> {
    let mut registry = EvidenceRegistry::new();
    Rc::new(EvidenceVector::install(&mut registry, &[Installation::parking(EFFECT_ASYNC)]).unwrap())
}

fn spawn(entry: vesper_fiber_abi::fiber::EntryFn, arg: u64, ev: &Rc<EvidenceVector>) -> Fiber {
    let code: Rc<dyn Any> = Rc::new(());
    // SAFETY: every entry in this file is static Rust code following the
    // entry contract.
    unsafe {
        Fiber::new(
            entry,
            arg,
            ev.clone(),
            code,
            FiberConfig {
                data_bytes: 128,
                ..Default::default()
            },
        )
    }
    .unwrap()
}

fn parked(o: Outcome) -> Parked {
    match o {
        Outcome::Parked(k) => k,
        Outcome::Exited(e) => panic!("expected a park, fiber exited: {e:?}"),
    }
}

/// Fill `ctx.park` with a yield on evidence slot 0.
///
/// # Safety
/// `ctx` is the running context and slot 0 of its evidence exists.
unsafe fn request_yield(ctx: *mut FiberContext) {
    unsafe {
        let identity = (*(*(*ctx).evidence).slots).identity;
        (*ctx).park = ParkRequest {
            evidence: identity,
            op: OP_YIELD,
            ..Default::default()
        };
    }
}

// ---- guard pages ---------------------------------------------------------

const PROBE_ENV: &str = "VESPER_FIBER_GUARD_PROBE";

#[inline(never)]
fn recurse(depth: u64) -> u64 {
    let mut frame = [0u8; 512];
    frame[0] = depth as u8;
    let frame = std::hint::black_box(frame);
    if std::hint::black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1).wrapping_add(u64::from(frame[0]))
}

unsafe extern "C" fn overflow_entry(_ctx: *mut FiberContext, _arg: u64) -> u64 {
    recurse(0)
}

/// Child side of the guard probe: never returns normally for the fault modes.
fn run_probe(mode: &str) {
    let stack = FiberStack::new(64 * 1024).unwrap();
    match mode {
        "in-bounds" => {
            // SAFETY: the lowest usable byte of our own stack mapping.
            unsafe { std::ptr::write_volatile(stack.base() as *mut u8, 1) };
            std::process::exit(0);
        }
        "touch-guard" => {
            // SAFETY: deliberately touches the PROT_NONE guard page; the
            // process is expected to die here.
            unsafe { std::ptr::write_volatile((stack.base() - 1) as *mut u8, 1) };
            std::process::exit(0);
        }
        "fiber-overflow" => {
            let fiber = spawn(overflow_entry, 0, &evidence());
            let _ = fiber.start();
            std::process::exit(0);
        }
        other => panic!("unknown probe {other}"),
    }
}

/// Re-run this test binary in probe mode. Under user-mode emulation the
/// binary cannot be exec'd directly; `VESPER_PROBE_RUNNER` (for example
/// `qemu-aarch64 -L /usr/aarch64-linux-gnu`) names the wrapper to use.
fn probe(mode: &str) -> std::process::ExitStatus {
    let exe = std::env::current_exe().unwrap();
    let runner = std::env::var("VESPER_PROBE_RUNNER").unwrap_or_default();
    let mut words = runner.split_whitespace();
    let mut command = match words.next() {
        Some(program) => {
            let mut c = Command::new(program);
            c.args(words).arg(exe);
            c
        }
        None => Command::new(exe),
    };
    command
        .args([
            "--exact",
            "guard_probe_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(PROBE_ENV, mode)
        .output()
        .unwrap()
        .status
}

/// Re-executed by [`guard_page_faults_on_direct_touch_and_fiber_overflow`];
/// does nothing in a normal run.
#[test]
fn guard_probe_child() {
    if let Ok(mode) = std::env::var(PROBE_ENV) {
        run_probe(&mode);
    }
}

#[test]
fn guard_page_faults_on_direct_touch_and_fiber_overflow() {
    use std::os::unix::process::ExitStatusExt;
    let stack = FiberStack::new(64 * 1024).unwrap();
    assert_eq!(stack.guard_len(), FiberStack::page_size());
    assert_eq!(stack.base() % FiberStack::page_size(), 0);
    assert_eq!(stack.top() % 16, 0);
    drop(stack);

    let control = probe("in-bounds");
    assert!(control.success(), "in-bounds write failed: {control:?}");
    for mode in ["touch-guard", "fiber-overflow"] {
        let status = probe(mode);
        let signal = status.signal();
        // Linux reports SIGSEGV; Darwin reports SIGBUS or SIGSEGV for a
        // protection fault on an anonymous mapping.
        assert!(
            matches!(signal, Some(libc_sig::SIGSEGV) | Some(libc_sig::SIGBUS)),
            "{mode}: expected a guard fault, got {status:?}"
        );
    }
}

/// Signal numbers without a libc dev-dependency.
mod libc_sig {
    pub const SIGSEGV: i32 = 11;
    #[cfg(target_os = "linux")]
    pub const SIGBUS: i32 = 7;
    #[cfg(target_os = "macos")]
    pub const SIGBUS: i32 = 10;
}

// ---- callee-saved register isolation ---------------------------------------
//
// `probe_park(ctx, seed, mismatches)` loads a seed-derived pattern into every
// callee-saved register (AArch64: x19-x28 and d8-d15; x86_64: rbx, rbp,
// r12-r15), parks through `vesper_fiber_park`, and on return counts the
// registers that no longer hold their pattern. `probe_call(arg, f, seed,
// mismatches)` does the same around an ordinary call `f(arg)` on the host
// side. Each helper saves and restores the registers it uses, so it is a
// well-behaved C function itself.

type Callback = extern "C" fn(u64) -> u64;

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn probe_park(
    _ctx: *mut FiberContext,
    _seed: u64,
    _mismatches: *mut u64,
) -> *const ResumeSignal {
    std::arch::naked_asm!(
        "stp x29, x30, [sp, #-176]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]",
        "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]",
        "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "stp x1, x2, [sp, #160]",
        "mov x9, x1",
        "bl {load}",
        "bl {park}",
        "ldr x9, [sp, #160]",
        "bl {check}",
        "ldr x2, [sp, #168]",
        "str x10, [x2]",
        "ldp x19, x20, [sp, #16]",
        "ldp x21, x22, [sp, #32]",
        "ldp x23, x24, [sp, #48]",
        "ldp x25, x26, [sp, #64]",
        "ldp x27, x28, [sp, #80]",
        "ldp d8, d9, [sp, #96]",
        "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]",
        "ldp d14, d15, [sp, #144]",
        "ldp x29, x30, [sp], #176",
        "ret",
        load = sym load_pattern,
        park = sym vesper_fiber_park,
        check = sym check_pattern,
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn probe_call(_arg: u64, _f: Callback, _seed: u64, _mismatches: *mut u64) -> u64 {
    std::arch::naked_asm!(
        "stp x29, x30, [sp, #-176]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]",
        "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]",
        "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "stp x2, x3, [sp, #160]",
        "mov x9, x2",
        "mov x12, x1",
        "bl {load}",
        "blr x12",
        "ldr x9, [sp, #160]",
        "bl {check}",
        "ldr x3, [sp, #168]",
        "str x10, [x3]",
        "ldp x19, x20, [sp, #16]",
        "ldp x21, x22, [sp, #32]",
        "ldp x23, x24, [sp, #48]",
        "ldp x25, x26, [sp, #64]",
        "ldp x27, x28, [sp, #80]",
        "ldp d8, d9, [sp, #96]",
        "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]",
        "ldp d14, d15, [sp, #144]",
        "ldp x29, x30, [sp], #176",
        "ret",
        load = sym load_pattern,
        check = sym check_pattern,
    )
}

/// Internal helper (not a C function): x9 = seed; sets x19..x28 to
/// seed + 19..28 and d8..d15 to the bits seed + 108..115. Clobbers x11 only;
/// preserves x0..x8, x12 and the link register value it returns through.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn load_pattern() {
    std::arch::naked_asm!(
        "add x19, x9, #19",
        "add x20, x9, #20",
        "add x21, x9, #21",
        "add x22, x9, #22",
        "add x23, x9, #23",
        "add x24, x9, #24",
        "add x25, x9, #25",
        "add x26, x9, #26",
        "add x27, x9, #27",
        "add x28, x9, #28",
        "add x11, x9, #108",
        "fmov d8, x11",
        "add x11, x9, #109",
        "fmov d9, x11",
        "add x11, x9, #110",
        "fmov d10, x11",
        "add x11, x9, #111",
        "fmov d11, x11",
        "add x11, x9, #112",
        "fmov d12, x11",
        "add x11, x9, #113",
        "fmov d13, x11",
        "add x11, x9, #114",
        "fmov d14, x11",
        "add x11, x9, #115",
        "fmov d15, x11",
        "ret",
    )
}

/// Internal helper: x9 = seed; x10 = number of callee-saved registers that
/// do not hold the pattern written by `load_pattern`. Clobbers x11; preserves
/// x0.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn check_pattern() {
    std::arch::naked_asm!(
        "mov x10, #0",
        "sub x11, x19, x9",
        "cmp x11, #19",
        "cinc x10, x10, ne",
        "sub x11, x20, x9",
        "cmp x11, #20",
        "cinc x10, x10, ne",
        "sub x11, x21, x9",
        "cmp x11, #21",
        "cinc x10, x10, ne",
        "sub x11, x22, x9",
        "cmp x11, #22",
        "cinc x10, x10, ne",
        "sub x11, x23, x9",
        "cmp x11, #23",
        "cinc x10, x10, ne",
        "sub x11, x24, x9",
        "cmp x11, #24",
        "cinc x10, x10, ne",
        "sub x11, x25, x9",
        "cmp x11, #25",
        "cinc x10, x10, ne",
        "sub x11, x26, x9",
        "cmp x11, #26",
        "cinc x10, x10, ne",
        "sub x11, x27, x9",
        "cmp x11, #27",
        "cinc x10, x10, ne",
        "sub x11, x28, x9",
        "cmp x11, #28",
        "cinc x10, x10, ne",
        "fmov x11, d8",
        "sub x11, x11, x9",
        "cmp x11, #108",
        "cinc x10, x10, ne",
        "fmov x11, d9",
        "sub x11, x11, x9",
        "cmp x11, #109",
        "cinc x10, x10, ne",
        "fmov x11, d10",
        "sub x11, x11, x9",
        "cmp x11, #110",
        "cinc x10, x10, ne",
        "fmov x11, d11",
        "sub x11, x11, x9",
        "cmp x11, #111",
        "cinc x10, x10, ne",
        "fmov x11, d12",
        "sub x11, x11, x9",
        "cmp x11, #112",
        "cinc x10, x10, ne",
        "fmov x11, d13",
        "sub x11, x11, x9",
        "cmp x11, #113",
        "cinc x10, x10, ne",
        "fmov x11, d14",
        "sub x11, x11, x9",
        "cmp x11, #114",
        "cinc x10, x10, ne",
        "fmov x11, d15",
        "sub x11, x11, x9",
        "cmp x11, #115",
        "cinc x10, x10, ne",
        "ret",
    )
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn probe_park(
    _ctx: *mut FiberContext,
    _seed: u64,
    _mismatches: *mut u64,
) -> *const ResumeSignal {
    std::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 24",
        "mov [rsp], rsi",
        "mov [rsp + 8], rdx",
        "mov rcx, rsi",
        "call {load}",
        "call {park}",
        "mov rcx, [rsp]",
        "call {check}",
        "mov r8, [rsp + 8]",
        "mov [r8], rdx",
        "add rsp, 24",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        load = sym load_pattern,
        park = sym vesper_fiber_park,
        check = sym check_pattern,
    )
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn probe_call(_arg: u64, _f: Callback, _seed: u64, _mismatches: *mut u64) -> u64 {
    std::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 24",
        "mov [rsp], rdx",
        "mov [rsp + 8], rcx",
        "mov rcx, rdx",
        "mov rax, rsi",
        "call {load}",
        "call rax",
        "mov rcx, [rsp]",
        "call {check}",
        "mov r8, [rsp + 8]",
        "mov [r8], rdx",
        "add rsp, 24",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        load = sym load_pattern,
        check = sym check_pattern,
    )
}

/// Internal helper: rcx = seed; sets rbx, rbp, r12..r15 to seed + 1..6.
/// Preserves rdi, rsi, rdx and rax.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn load_pattern() {
    std::arch::naked_asm!(
        "lea rbx, [rcx + 1]",
        "lea rbp, [rcx + 2]",
        "lea r12, [rcx + 3]",
        "lea r13, [rcx + 4]",
        "lea r14, [rcx + 5]",
        "lea r15, [rcx + 6]",
        "ret",
    )
}

/// Internal helper: rcx = seed; rdx = number of callee-saved registers not
/// holding their pattern. Clobbers r8, r9; preserves rax.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn check_pattern() {
    std::arch::naked_asm!(
        "xor edx, edx",
        "lea r8, [rcx + 1]",
        "xor r9d, r9d",
        "cmp rbx, r8",
        "setne r9b",
        "add rdx, r9",
        "lea r8, [rcx + 2]",
        "xor r9d, r9d",
        "cmp rbp, r8",
        "setne r9b",
        "add rdx, r9",
        "lea r8, [rcx + 3]",
        "xor r9d, r9d",
        "cmp r12, r8",
        "setne r9b",
        "add rdx, r9",
        "lea r8, [rcx + 4]",
        "xor r9d, r9d",
        "cmp r13, r8",
        "setne r9b",
        "add rdx, r9",
        "lea r8, [rcx + 5]",
        "xor r9d, r9d",
        "cmp r14, r8",
        "setne r9b",
        "add rdx, r9",
        "lea r8, [rcx + 6]",
        "xor r9d, r9d",
        "cmp r15, r8",
        "setne r9b",
        "add rdx, r9",
        "ret",
    )
}

const REGISTER_PARKS: u64 = 3;

/// Parks `REGISTER_PARKS` times through `probe_park` with a per-fiber,
/// per-park pattern; data word 0 accumulates mismatches, word 1 counts parks.
unsafe extern "C" fn register_entry(ctx: *mut FiberContext, seed: u64) -> u64 {
    let mut total = 0u64;
    for i in 0..REGISTER_PARKS {
        let mut mismatches = 0u64;
        // SAFETY: running context with evidence slot 0.
        let signal = unsafe {
            request_yield(ctx);
            probe_park(ctx, seed + i * 0x1000, &mut mismatches)
        };
        if signal.is_null() {
            return exit::ABI_VIOLATION;
        }
        total += mismatches;
        // SAFETY: the resume signal names the current data frame.
        unsafe {
            let base = (*signal).data_base as *mut u64;
            *base = total;
            *base.add(1) = i + 1;
        }
    }
    exit::COMPLETE
}

struct ResumeSlot {
    parked: Option<Parked>,
    outcome: Option<Outcome>,
}

extern "C" fn resume_callback(slot: u64) -> u64 {
    // SAFETY: `slot` is the address of a live `ResumeSlot` on the test stack.
    let slot = unsafe { &mut *(slot as *mut ResumeSlot) };
    let k = slot.parked.take().unwrap();
    let evidence = k.request().evidence;
    slot.outcome = Some(k.resume(Resumption::value(evidence, 0)).unwrap());
    0
}

#[test]
fn callee_saved_registers_survive_interleaved_switches() {
    let ev = evidence();
    let seeds = [0x1111_0000_0000u64, 0x2222_0000_0000u64];
    let mut fibers: Vec<Option<Parked>> = seeds
        .iter()
        .map(|&s| Some(parked(spawn(register_entry, s, &ev).start())))
        .collect();
    let mut host_mismatches = 0;
    let mut finished = vec![];
    let mut round = 0u64;
    while fibers.iter().any(Option::is_some) {
        for (i, k) in fibers.iter_mut().enumerate() {
            let Some(parked_k) = k.take() else { continue };
            let mut slot = ResumeSlot {
                parked: Some(parked_k),
                outcome: None,
            };
            let mut mismatches = u64::MAX;
            // SAFETY: the callback only resumes the owned continuation.
            unsafe {
                probe_call(
                    &raw mut slot as u64,
                    resume_callback,
                    0x7777_0000_0000 + round * 0x100 + i as u64,
                    &mut mismatches,
                )
            };
            host_mismatches += mismatches;
            match slot.outcome.take().unwrap() {
                Outcome::Parked(next) => *k = Some(next),
                Outcome::Exited(e) => finished.push(e),
            }
        }
        round += 1;
    }
    assert_eq!(host_mismatches, 0, "host callee-saved state was clobbered");
    assert_eq!(finished.len(), 2);
    for e in &finished {
        assert_eq!(e.status, exit::COMPLETE);
        assert_eq!(e.word(0), 0, "fiber callee-saved state was clobbered");
        assert_eq!(e.word(1), REGISTER_PARKS);
    }
}

#[test]
fn register_probe_detects_clobbering() {
    // Negative control: a callback that clobbers a callee-saved register
    // (behind the C ABI's back) must be reported, so a passing isolation test
    // is not vacuous.
    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    extern "C" fn clobber(_: u64) -> u64 {
        std::arch::naked_asm!("mov x23, #0", "fmov d12, xzr", "mov x0, #0", "ret")
    }
    #[cfg(target_arch = "x86_64")]
    #[unsafe(naked)]
    extern "C" fn clobber(_: u64) -> u64 {
        std::arch::naked_asm!("xor r13d, r13d", "xor eax, eax", "ret")
    }
    let mut mismatches = 0u64;
    // SAFETY: probe_call restores every callee-saved register itself, so the
    // clobber stays inside the probe.
    unsafe { probe_call(0, clobber, 0x4242_0000, &mut mismatches) };
    #[cfg(target_arch = "aarch64")]
    assert_eq!(mismatches, 2);
    #[cfg(target_arch = "x86_64")]
    assert_eq!(mismatches, 1);
}

// ---- floating-point control and status ------------------------------------

fn code(r: Rounding) -> u64 {
    match r {
        Rounding::Nearest => 0,
        Rounding::Down => 1,
        Rounding::Up => 2,
        Rounding::TowardZero => 3,
    }
}

/// Data words (128-byte frame): [0] rounding at start, [1] flags at start, then per resume:
/// [2] rounding, [3] flags seen, [4] 1/3 bits, [5] -1/3 bits (resume 1),
/// [6..10) resume 2. `arg` 1: round up and raise inexact; otherwise round down
/// and raise divide-by-zero.
unsafe extern "C" fn fp_entry(ctx: *mut FiberContext, arg: u64) -> u64 {
    // SAFETY: running context, 64-byte data frame, evidence slot 0.
    unsafe {
        let mut base = (*ctx).data_base as *mut u64;
        *base = code(fpenv::current().rounding());
        *base.add(1) = u64::from(fpenv::exception_flags());
        if arg == 1 {
            fpenv::set_rounding(Rounding::Up);
            std::hint::black_box(fpenv::div_bits(1.0, 3.0));
        } else {
            fpenv::set_rounding(Rounding::Down);
            std::hint::black_box(fpenv::div_bits(1.0, 0.0));
        }
        for i in 0..2 {
            request_yield(ctx);
            let signal = vesper_fiber_park(ctx);
            if signal.is_null() {
                return exit::ABI_VIOLATION;
            }
            base = (*signal).data_base as *mut u64;
            let out = base.add(2 + 4 * i);
            *out = code(fpenv::current().rounding());
            *out.add(1) = u64::from(fpenv::exception_flags());
            let (third, neg_third) = fpenv::rounding_fingerprint();
            *out.add(2) = third;
            *out.add(3) = neg_third;
            // Leave only this fiber's original flag behind for the next park.
            fpenv::clear_exception_flags();
            if arg == 1 {
                std::hint::black_box(fpenv::div_bits(1.0, 3.0));
            } else {
                std::hint::black_box(fpenv::div_bits(1.0, 0.0));
            }
        }
        exit::COMPLETE
    }
}

#[test]
fn fp_rounding_and_status_flags_are_owned_by_each_context() {
    let ev = evidence();
    let saved = fpenv::current();
    assert_eq!(
        saved.control(),
        fpenv::FpEnv::DEFAULT.control(),
        "test thread env"
    );
    let nearest = fpenv::rounding_fingerprint();

    // SAFETY: until the restore below the host runs only integer code and
    // the asm arithmetic in `fpenv`.
    unsafe { fpenv::set_rounding(Rounding::TowardZero) };
    fpenv::clear_exception_flags();
    let mut ks: Vec<Option<Parked>> = [1u64, 2]
        .iter()
        .map(|&a| Some(parked(spawn(fp_entry, a, &ev).start())))
        .collect();
    let mut host_modes = vec![fpenv::current().rounding()];
    let mut host_flags = vec![fpenv::exception_flags()];
    let mut done = vec![];
    while ks.iter().any(Option::is_some) {
        for k in ks.iter_mut() {
            let Some(p) = k.take() else { continue };
            let evidence = p.request().evidence;
            match p.resume(Resumption::value(evidence, 0)).unwrap() {
                Outcome::Parked(next) => *k = Some(next),
                Outcome::Exited(e) => done.push(e),
            }
            host_modes.push(fpenv::current().rounding());
            host_flags.push(fpenv::exception_flags());
        }
    }
    // SAFETY: restoring the test thread's environment.
    unsafe { fpenv::install(saved) };
    fpenv::clear_exception_flags();

    assert!(
        host_modes.iter().all(|m| *m == Rounding::TowardZero),
        "{host_modes:?}"
    );
    assert!(
        host_flags.iter().all(|f| *f == 0),
        "fiber flags leaked: {host_flags:?}"
    );
    assert_eq!(done.len(), 2);
    let up = (nearest.0 + 1, nearest.1);
    let down = (nearest.0, nearest.1 + 1);
    for e in &done {
        assert_eq!(e.status, exit::COMPLETE);
        // Fibers start from the ABI default, not the creator's environment.
        assert_eq!(e.word(0), code(Rounding::Nearest));
        assert_eq!(e.word(1), 0, "a fresh fiber starts with clear flags");
        let (mode, flag, fingerprint) = if e.word(2) == code(Rounding::Up) {
            (Rounding::Up, fpenv::FLAG_INEXACT, up)
        } else {
            (Rounding::Down, fpenv::FLAG_DIVIDE_BY_ZERO, down)
        };
        for i in 0..2 {
            let w = 2 + 4 * i;
            assert_eq!(e.word(w), code(mode));
            // Exactly this fiber's own sticky flag survived the switches.
            assert_eq!(e.word(w + 1), u64::from(flag), "{mode:?} resume {i}");
            assert_eq!((e.word(w + 2), e.word(w + 3)), fingerprint, "{mode:?}");
        }
    }
    let mut modes: Vec<u64> = done.iter().map(|e| e.word(2)).collect();
    modes.sort();
    assert_eq!(modes, vec![code(Rounding::Down), code(Rounding::Up)]);
}
