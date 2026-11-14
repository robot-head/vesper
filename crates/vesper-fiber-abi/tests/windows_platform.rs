//! Platform checks for the Windows x64 fiber port (`stack_windows.rs`,
//! `switch_windows.rs`, `fault.rs`), the counterpart of `platform.rs`:
//!
//! * the guard page below a pinned stack faults when touched from the host,
//!   and a running fiber that recurses past its stack gets a real
//!   `STATUS_STACK_OVERFLOW` (which Windows raises only when the TEB stack
//!   limits describe the fiber stack), reported by the fiber fault handler;
//! * an access violation inside a fiber is reported with the fiber's identity
//!   and terminates the process with the exception code; a host-side fault is
//!   not attributed to any fiber;
//! * the TEB stack fields are owned by each context;
//! * every Microsoft x64 callee-saved register (rbx, rbp, rdi, rsi, r12–r15
//!   and all 128 bits of xmm6–xmm15) survives interleaved switches, checked in
//!   assembly on both sides of the switch, with a negative control;
//! * floating-point control and status flags are owned by each context.
#![cfg(all(vesper_direct_fibers, windows))]
use std::any::Any;
use std::process::Command;
use std::rc::Rc;
use vesper_fiber_abi::abi::{FiberContext, FiberState, ParkRequest, ResumeSignal, exit};
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector, Installation};
use vesper_fiber_abi::fiber::vesper_fiber_park;
use vesper_fiber_abi::fpenv::{self, Rounding};
use vesper_fiber_abi::prototype::{EFFECT_ASYNC, OP_YIELD};
use vesper_fiber_abi::stack::{DEFAULT_STACK_SIZE, FiberStack, OVERFLOW_RESERVE, TebLimits};
use vesper_fiber_abi::switch::{current_fiber_context, current_teb_limits};
use vesper_fiber_abi::{Fiber, FiberConfig, Outcome, Parked, Resumption, fault};

const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_STACK_OVERFLOW: u32 = 0xC000_00FD;
const STATUS_GUARD_PAGE_VIOLATION: u32 = 0x8000_0001;

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

fn resume(k: Parked) -> Outcome {
    let evidence = k.request().evidence;
    k.resume(Resumption::value(evidence, 0)).unwrap()
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

// ---- guard pages and structured exceptions -------------------------------

const PROBE_ENV: &str = "VESPER_FIBER_GUARD_PROBE";
/// Exit status of the touch-guard probe if the write unexpectedly succeeded.
const WROTE_GUARD: i32 = 3;

/// Wine (used for local cross-checks) hands an unhandled exception to its
/// debugger, which resumes the faulting thread.
fn under_wine() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    // SAFETY: both strings are NUL-terminated; ntdll is always loaded.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
    }
}

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

unsafe extern "C" fn access_violation_entry(ctx: *mut FiberContext, _arg: u64) -> u64 {
    // Park once first, so the fault happens on a resumed (not just started)
    // context.
    // SAFETY: running context with evidence slot 0.
    unsafe {
        request_yield(ctx);
        if vesper_fiber_park(ctx).is_null() {
            return exit::ABI_VIOLATION;
        }
        std::ptr::write_volatile(std::hint::black_box(0x10usize) as *mut u8, 1);
    }
    exit::COMPLETE
}

/// Child side of the probes: never returns normally for the fault modes.
fn run_probe(mode: &str) {
    let stack = FiberStack::new(64 * 1024).unwrap();
    match mode {
        "in-bounds" => {
            // SAFETY: the lowest usable byte of our own stack allocation.
            unsafe { std::ptr::write_volatile(stack.base() as *mut u8, 1) };
            std::process::exit(0);
        }
        "touch-guard" => {
            // SAFETY: deliberately touches the PAGE_GUARD page from the host
            // thread; the process is expected to die here.
            unsafe { std::ptr::write_volatile((stack.base() - 1) as *mut u8, 1) };
            std::process::exit(WROTE_GUARD);
        }
        "fiber-overflow" => {
            let fiber = spawn(overflow_entry, 0, &evidence());
            let _ = fiber.start();
            std::process::exit(0);
        }
        "fiber-access-violation" => {
            let k = parked(spawn(access_violation_entry, 0, &evidence()).start());
            eprintln!("probe: fiber {} parked once", k.fiber_id());
            let _ = resume(k);
            std::process::exit(0);
        }
        other => panic!("unknown probe {other}"),
    }
}

struct Probe {
    code: Option<u32>,
    stderr: String,
}

/// Re-run this test binary in probe mode.
fn probe(mode: &str) -> Probe {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new(exe)
        .args([
            "--exact",
            "guard_probe_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(PROBE_ENV, mode)
        .output()
        .unwrap();
    Probe {
        code: output.status.code().map(|c| c as u32),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Re-executed by the probe tests; does nothing in a normal run.
#[test]
fn guard_probe_child() {
    if let Ok(mode) = std::env::var(PROBE_ENV) {
        run_probe(&mode);
    }
}

#[test]
fn stack_layout_and_guard() {
    let stack = FiberStack::new(64 * 1024).unwrap();
    let page = FiberStack::page_size();
    assert_eq!(stack.guard_len(), OVERFLOW_RESERVE + page);
    assert_eq!(stack.base() % page, 0);
    assert_eq!(stack.top() % 16, 0);
    assert_eq!(stack.base() - stack.guard_len(), stack.allocation_base());
    drop(stack);
    let control = probe("in-bounds");
    assert_eq!(
        control.code,
        Some(0),
        "in-bounds write failed: {}",
        control.stderr
    );
}

#[test]
fn host_touch_of_the_guard_page_faults_and_is_not_attributed_to_a_fiber() {
    let p = probe("touch-guard");
    // Outside the current thread's TEB stack range a guard page raises a
    // plain guard-page violation (not a stack overflow).
    if under_wine() {
        // Wine's debugger resumes after an unhandled guard-page violation,
        // whose one-shot guard is then gone, so the write completes.
        eprintln!(
            "under Wine: guard-page exit status not checked ({:?})",
            p.code
        );
    } else {
        assert_ne!(
            p.code,
            Some(WROTE_GUARD as u32),
            "the guard page was writable"
        );
        assert_eq!(
            p.code,
            Some(STATUS_GUARD_PAGE_VIOLATION),
            "expected a guard-page fault: {}",
            p.stderr
        );
    }
    assert!(!p.stderr.contains("vesper fiber"), "{}", p.stderr);
}

#[test]
fn fiber_overflow_is_a_stack_overflow_reported_for_that_fiber() {
    let p = probe("fiber-overflow");
    // STATUS_STACK_OVERFLOW (rather than a guard-page violation) shows that
    // the kernel attributed the guard page to the running fiber's stack via
    // the TEB fields the switch installed.
    assert_eq!(
        p.code,
        Some(STATUS_STACK_OVERFLOW),
        "expected a stack overflow: {}",
        p.stderr
    );
    assert!(
        p.stderr.contains("faulted: exception 0xc00000fd"),
        "fault not reported: {}",
        p.stderr
    );
}

#[test]
fn access_violation_in_a_resumed_fiber_is_reported_with_its_identity() {
    let p = probe("fiber-access-violation");
    assert_eq!(
        p.code,
        Some(STATUS_ACCESS_VIOLATION),
        "expected an access violation: {}",
        p.stderr
    );
    let parked_line = p
        .stderr
        .lines()
        .find_map(|l| l.strip_prefix("probe: fiber "))
        .expect("probe did not park");
    let id = parked_line.split_whitespace().next().unwrap();
    assert!(
        p.stderr
            .contains(&format!("vesper fiber {id} faulted: exception 0xc0000005")),
        "fault not attributed to fiber {id}: {}",
        p.stderr
    );
}

#[test]
fn fault_classification() {
    assert!(fault::is_fault(STATUS_ACCESS_VIOLATION as i32));
    assert!(fault::is_fault(STATUS_STACK_OVERFLOW as i32));
    // A Rust panic or C++ exception is a software exception: not reported.
    assert!(!fault::is_fault(0xE06D_7363_u32 as i32));
    assert!(!fault::is_fault(0x8000_0003_u32 as i32)); // breakpoint
    assert_eq!(fault::last_fault(), None);
}

// ---- TEB ownership ---------------------------------------------------------

unsafe extern "C" fn teb_entry(ctx: *mut FiberContext, _arg: u64) -> u64 {
    // SAFETY: running context, 128-byte data frame, evidence slot 0.
    unsafe {
        for round in 0..2 {
            let limits = current_teb_limits();
            let base = (*ctx).data_base as *mut u64;
            let out = base.add(5 * round);
            *out = limits.stack_base;
            *out.add(1) = limits.stack_limit;
            *out.add(2) = limits.deallocation_stack;
            *out.add(3) = u64::from(limits.guaranteed_bytes);
            *out.add(4) = match current_fiber_context() {
                Some(seen) if seen == ctx => 1,
                Some(_) => 2,
                None => 0,
            };
            request_yield(ctx);
            if vesper_fiber_park(ctx).is_null() {
                return exit::ABI_VIOLATION;
            }
        }
    }
    exit::COMPLETE
}

#[test]
fn teb_stack_limits_are_owned_by_each_context() {
    let ev = evidence();
    let host = current_teb_limits();
    assert!(
        current_fiber_context().is_none(),
        "host stack is not a fiber stack"
    );
    let a = spawn(teb_entry, 0, &ev);
    let b = spawn(teb_entry, 0, &ev);
    let mut ka = parked(a.start());
    assert_eq!(current_teb_limits(), host);
    let mut kb = parked(b.start());
    assert_eq!(current_teb_limits(), host);
    let mut seen: Vec<TebLimits> = vec![];
    for _ in 0..2 {
        for k in [&mut ka, &mut kb] {
            let data = k.data();
            let w = |i: usize| u64::from_le_bytes(data[i * 8..i * 8 + 8].try_into().unwrap());
            seen.push(TebLimits {
                stack_base: w(0),
                stack_limit: w(1),
                deallocation_stack: w(2),
                guaranteed_bytes: w(3) as u32,
            });
            assert_eq!(w(4), 1, "the stack header names the running fiber");
        }
        let next_a = resume(ka);
        assert_eq!(
            current_teb_limits(),
            host,
            "host TEB restored after a switch"
        );
        let next_b = resume(kb);
        assert_eq!(current_teb_limits(), host);
        match (next_a, next_b) {
            (Outcome::Parked(x), Outcome::Parked(y)) => {
                // Second round: the data written before the second park.
                let wa = u64::from_le_bytes(x.data()[40..48].try_into().unwrap());
                let wb = u64::from_le_bytes(y.data()[40..48].try_into().unwrap());
                assert_eq!(
                    wa, seen[0].stack_base,
                    "fiber A's TEB is stable across parks"
                );
                assert_eq!(
                    wb, seen[1].stack_base,
                    "fiber B's TEB is stable across parks"
                );
                ka = x;
                kb = y;
            }
            (Outcome::Exited(x), Outcome::Exited(y)) => {
                assert_eq!(x.status, exit::COMPLETE);
                assert_eq!(y.status, exit::COMPLETE);
                break;
            }
            _ => panic!("fibers diverged"),
        }
    }
    let (fa, fb) = (seen[0], seen[1]);
    assert_ne!(fa.stack_base, host.stack_base);
    assert_ne!(fa.stack_base, fb.stack_base);
    for f in [fa, fb] {
        assert!(f.deallocation_stack < f.stack_limit && f.stack_limit < f.stack_base);
        assert_eq!(f.guaranteed_bytes as usize, OVERFLOW_RESERVE);
        assert_eq!((f.stack_base - f.stack_limit) as usize, DEFAULT_STACK_SIZE);
    }
    assert_eq!(fault::last_fault(), None);
}

// ---- callee-saved register isolation ---------------------------------------
//
// `probe_park(ctx, seed, mismatches)` loads a seed-derived pattern into every
// Microsoft x64 callee-saved register (rbx, rbp, rdi, rsi, r12-r15 and both
// halves of xmm6-xmm15), parks through `vesper_fiber_park`, and on return
// counts the register halves that no longer hold their pattern (28 checks).
// `probe_call(arg, f, seed, mismatches)` does the same around an ordinary
// call `f(arg)` on the host side. Each helper saves and restores what it uses
// and provides the callee's shadow space, so it is a well-behaved function.

type Callback = extern "C" fn(u64) -> u64;

#[unsafe(naked)]
unsafe extern "C" fn probe_park(
    _ctx: *mut FiberContext,
    _seed: u64,
    _mismatches: *mut u64,
) -> *const ResumeSignal {
    std::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push rdi",
        "push rsi",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 216",
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
        "mov [rsp + 32], rdx",
        "mov [rsp + 40], r8",
        "mov r10, rdx",
        "call {load}",
        "call {park}",
        "mov r10, [rsp + 32]",
        "call {check}",
        "mov r8, [rsp + 40]",
        "mov [r8], r11",
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
        "ret",
        load = sym load_pattern,
        park = sym vesper_fiber_park,
        check = sym check_pattern,
    )
}

#[unsafe(naked)]
unsafe extern "C" fn probe_call(_arg: u64, _f: Callback, _seed: u64, _mismatches: *mut u64) -> u64 {
    std::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push rdi",
        "push rsi",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 216",
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
        "mov [rsp + 32], r8",
        "mov [rsp + 40], r9",
        "mov [rsp + 208], rdx",
        "mov r10, r8",
        "call {load}",
        "mov rax, [rsp + 208]",
        "call rax",
        "mov r10, [rsp + 32]",
        "call {check}",
        "mov r8, [rsp + 40]",
        "mov [r8], r11",
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
        "ret",
        load = sym load_pattern,
        check = sym check_pattern,
    )
}

/// Internal helper: r10 = seed; sets the callee-saved registers to the
/// pattern. Clobbers r9, r11 and xmm0; preserves rax, rcx, rdx and r8.
#[unsafe(naked)]
unsafe extern "C" fn load_pattern() {
    std::arch::naked_asm!(
        "lea rbx, [r10 + 1]",
        "lea rbp, [r10 + 2]",
        "lea rdi, [r10 + 3]",
        "lea rsi, [r10 + 4]",
        "lea r12, [r10 + 5]",
        "lea r13, [r10 + 6]",
        "lea r14, [r10 + 7]",
        "lea r15, [r10 + 8]",
        "lea r9, [r10 + 9]",
        "mov r11, r9",
        "not r11",
        "movq xmm6, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm6, xmm0",
        "lea r9, [r10 + 10]",
        "mov r11, r9",
        "not r11",
        "movq xmm7, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm7, xmm0",
        "lea r9, [r10 + 11]",
        "mov r11, r9",
        "not r11",
        "movq xmm8, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm8, xmm0",
        "lea r9, [r10 + 12]",
        "mov r11, r9",
        "not r11",
        "movq xmm9, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm9, xmm0",
        "lea r9, [r10 + 13]",
        "mov r11, r9",
        "not r11",
        "movq xmm10, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm10, xmm0",
        "lea r9, [r10 + 14]",
        "mov r11, r9",
        "not r11",
        "movq xmm11, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm11, xmm0",
        "lea r9, [r10 + 15]",
        "mov r11, r9",
        "not r11",
        "movq xmm12, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm12, xmm0",
        "lea r9, [r10 + 16]",
        "mov r11, r9",
        "not r11",
        "movq xmm13, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm13, xmm0",
        "lea r9, [r10 + 17]",
        "mov r11, r9",
        "not r11",
        "movq xmm14, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm14, xmm0",
        "lea r9, [r10 + 18]",
        "mov r11, r9",
        "not r11",
        "movq xmm15, r9",
        "movq xmm0, r11",
        "punpcklqdq xmm15, xmm0",
        "ret",
    )
}

/// Internal helper: r10 = seed; r11 = number of mismatching register halves.
/// Clobbers rdx, r8, r9 and xmm0; preserves rax.
#[unsafe(naked)]
unsafe extern "C" fn check_pattern() {
    std::arch::naked_asm!(
        "xor r11d, r11d",
        "lea r8, [r10 + 1]",
        "xor edx, edx",
        "cmp rbx, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 2]",
        "xor edx, edx",
        "cmp rbp, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 3]",
        "xor edx, edx",
        "cmp rdi, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 4]",
        "xor edx, edx",
        "cmp rsi, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 5]",
        "xor edx, edx",
        "cmp r12, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 6]",
        "xor edx, edx",
        "cmp r13, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 7]",
        "xor edx, edx",
        "cmp r14, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 8]",
        "xor edx, edx",
        "cmp r15, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 9]",
        "movq r9, xmm6",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm6, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 10]",
        "movq r9, xmm7",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm7, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 11]",
        "movq r9, xmm8",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm8, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 12]",
        "movq r9, xmm9",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm9, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 13]",
        "movq r9, xmm10",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm10, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 14]",
        "movq r9, xmm11",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm11, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 15]",
        "movq r9, xmm12",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm12, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 16]",
        "movq r9, xmm13",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm13, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 17]",
        "movq r9, xmm14",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm14, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "lea r8, [r10 + 18]",
        "movq r9, xmm15",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "not r8",
        "pshufd xmm0, xmm15, 0x4E",
        "movq r9, xmm0",
        "xor edx, edx",
        "cmp r9, r8",
        "setne dl",
        "add r11, rdx",
        "ret",
    )
}

const REGISTER_PARKS: u64 = 3;
const REGISTER_CHECKS: u64 = 28;

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
    slot.outcome = Some(resume(k));
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
    // Negative controls: callbacks that clobber callee-saved state behind the
    // ABI's back must be reported, so a passing isolation test is not vacuous.
    #[unsafe(naked)]
    extern "C" fn clobber(_: u64) -> u64 {
        // r13, rdi (callee-saved only on Windows) and both halves of xmm9.
        std::arch::naked_asm!(
            "xor r13d, r13d",
            "xor edi, edi",
            "xorps xmm9, xmm9",
            "xor eax, eax",
            "ret"
        )
    }
    #[unsafe(naked)]
    extern "C" fn clobber_high(_: u64) -> u64 {
        // Only the upper 64 bits of xmm15.
        std::arch::naked_asm!("movlhps xmm15, xmm15", "xor eax, eax", "ret")
    }
    #[unsafe(naked)]
    extern "C" fn clobber_all(_: u64) -> u64 {
        // Every checked register: all 28 halves must be reported.
        std::arch::naked_asm!(
            "xor rbx, rbx",
            "xor rbp, rbp",
            "xor rdi, rdi",
            "xor rsi, rsi",
            "xor r12, r12",
            "xor r13, r13",
            "xor r14, r14",
            "xor r15, r15",
            "xorps xmm6, xmm6",
            "xorps xmm7, xmm7",
            "xorps xmm8, xmm8",
            "xorps xmm9, xmm9",
            "xorps xmm10, xmm10",
            "xorps xmm11, xmm11",
            "xorps xmm12, xmm12",
            "xorps xmm13, xmm13",
            "xorps xmm14, xmm14",
            "xorps xmm15, xmm15",
            "xor eax, eax",
            "ret",
        )
    }
    #[unsafe(naked)]
    extern "C" fn clean(_: u64) -> u64 {
        std::arch::naked_asm!("xor eax, eax", "ret")
    }
    let run = |f: Callback| {
        let mut mismatches = u64::MAX;
        // SAFETY: probe_call restores every callee-saved register itself, so
        // the clobber stays inside the probe.
        unsafe { probe_call(0, f, 0x4242_0000, &mut mismatches) };
        mismatches
    };
    assert_eq!(run(clean), 0);
    assert_eq!(run(clobber), 4);
    assert_eq!(run(clobber_high), 1);
    assert_eq!(run(clobber_all), REGISTER_CHECKS);
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

/// Data words: [0] rounding at start, [1] flags at start, then per resume:
/// [2] rounding, [3] flags seen, [4] 1/3 bits, [5] -1/3 bits (resume 1),
/// [6..10) resume 2. `arg` 1: round up and raise inexact; otherwise round down
/// and raise divide-by-zero.
unsafe extern "C" fn fp_entry(ctx: *mut FiberContext, arg: u64) -> u64 {
    // SAFETY: running context, 128-byte data frame, evidence slot 0.
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
            match resume(p) {
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
            assert_eq!(e.word(w + 1), u64::from(flag), "{mode:?} resume {i}");
            assert_eq!((e.word(w + 2), e.word(w + 3)), fingerprint, "{mode:?}");
        }
    }
}

#[test]
fn a_new_fiber_starts_poison_free_and_running() {
    // The fault handler only marks a context poisoned on a fault; ordinary
    // switches never do.
    let ev = evidence();
    let k = parked(spawn(teb_entry, 0, &ev).start());
    assert_eq!(k.fiber().state(), FiberState::Parked);
    drop(k);
}
