//! Floating-point control state is owned per context (ABI v2 switch frame).
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use std::any::Any;
use std::io;
use std::rc::Rc;
use vesper_fiber_abi::abi::{FLAG_FP_ENV_SAVED, FiberContext, ParkRequest, exit};
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector, Installation};
use vesper_fiber_abi::fiber::vesper_fiber_park;
use vesper_fiber_abi::fpenv::{self, Rounding};
use vesper_fiber_abi::prototype::{EFFECT_ASYNC, OP_YIELD};
use vesper_fiber_abi::pulse::{Completed, NativeTask};
use vesper_fiber_abi::{Fiber, FiberConfig};
use vesper_pulse::io_driver::Request;
use vesper_pulse::scheduler::{Driver, Finished, PollError, Scheduler};

fn code(r: Rounding) -> u64 {
    match r {
        Rounding::Nearest => 0,
        Rounding::Down => 1,
        Rounding::Up => 2,
        Rounding::TowardZero => 3,
    }
}

const PARKS: usize = 2;

/// Native fiber entry written in Rust against the ABI: records the
/// environment it starts with, installs its own rounding mode (`arg`: 1 = up,
/// otherwise down), parks twice, and records what it observes after each
/// resume. Data words: `[start, (rounding, 1/3 bits, -1/3 bits) per resume]`.
unsafe extern "C" fn fp_entry(ctx: *mut FiberContext, arg: u64) -> u64 {
    // SAFETY: `ctx` is the running context handed to this entry; the data
    // frame has 8 words; slot 0 of the evidence vector exists.
    unsafe {
        let mut base = (*ctx).data_base as *mut u64;
        *base = code(fpenv::current().rounding());
        fpenv::set_rounding(if arg == 1 {
            Rounding::Up
        } else {
            Rounding::Down
        });
        let identity = (*(*(*ctx).evidence).slots).identity;
        for i in 0..PARKS {
            (*ctx).park = ParkRequest {
                evidence: identity,
                op: OP_YIELD,
                ..Default::default()
            };
            let signal = vesper_fiber_park(ctx);
            if signal.is_null() {
                return exit::ABI_VIOLATION;
            }
            base = (*signal).data_base as *mut u64;
            let (third, neg_third) = fpenv::rounding_fingerprint();
            *base.add(1 + 3 * i) = code(fpenv::current().rounding());
            *base.add(2 + 3 * i) = third;
            *base.add(3 + 3 * i) = neg_third;
        }
        (*ctx).result = 0;
        exit::COMPLETE
    }
}

fn spawn(arg: u64, evidence: &Rc<EvidenceVector>) -> Fiber {
    let code: Rc<dyn Any> = Rc::new(());
    // SAFETY: `fp_entry` is static code following the entry contract.
    unsafe {
        Fiber::new(
            fp_entry,
            arg,
            evidence.clone(),
            code,
            FiberConfig {
                data_bytes: 64,
                ..Default::default()
            },
        )
    }
    .unwrap()
}

struct NoIo;
impl Driver<NativeTask> for NoIo {
    fn submit(&mut self, r: Request<NativeTask>) -> Result<(), (io::Error, Request<NativeTask>)> {
        Err((io::Error::other("no I/O"), r))
    }
    fn poll(&mut self, _: bool) -> Result<Vec<Finished<NativeTask>>, PollError<NativeTask>> {
        Ok(vec![])
    }
    fn pending(&self) -> usize {
        0
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn fp_control_state_is_isolated_between_two_fibers_and_the_scheduler() {
    let mut registry = EvidenceRegistry::new();
    let evidence = Rc::new(
        EvidenceVector::install(&mut registry, &[Installation::parking(EFFECT_ASYNC)]).unwrap(),
    );
    let fresh = spawn(1, &evidence);
    let flags = unsafe { (*fresh.context_ptr()).flags };
    assert_ne!(flags & FLAG_FP_ENV_SAVED, 0);
    drop(fresh);

    let saved = fpenv::current();
    let nearest = fpenv::rounding_fingerprint();
    let mut s = Scheduler::new(NoIo);
    s.spawn(NativeTask::new(spawn(1, &evidence), None));
    s.spawn(NativeTask::new(spawn(2, &evidence), None));
    // The scheduler context runs with a non-default mode of its own.
    // SAFETY: only integer scheduler code and asm arithmetic run until the
    // restore below.
    unsafe { fpenv::set_rounding(Rounding::TowardZero) };
    let mut scheduler_modes = vec![];
    let mut ticks = 0;
    while !s.is_idle() {
        s.tick(false).unwrap();
        scheduler_modes.push(fpenv::current().rounding());
        ticks += 1;
    }
    // SAFETY: restoring the test thread's original environment.
    unsafe { fpenv::install(saved) };
    assert_eq!(ticks, 2 * (PARKS + 1) - 1 + 1);
    assert!(
        scheduler_modes.iter().all(|m| *m == Rounding::TowardZero),
        "{scheduler_modes:?}"
    );
    assert_eq!(fpenv::current().control(), saved.control());

    let mut done = vec![];
    while let Some(bytes) = s.take_completed() {
        done.push(Completed::decode(&bytes).unwrap());
    }
    assert_eq!(s.take_failure(), None);
    assert_eq!(done.len(), 2);
    for c in &done {
        assert_eq!(c.status, exit::COMPLETE);
        // A fiber starts from the ABI default, not its creator's mode.
        assert_eq!(c.word(0), code(Rounding::Nearest));
    }
    let up = (nearest.0 + 1, nearest.1);
    let down = (nearest.0, nearest.1 + 1);
    let mut seen = vec![];
    for c in &done {
        let mode = c.word(1);
        for i in 0..PARKS {
            // Every resume restores this fiber's own mode, and the hardware
            // computes with it.
            assert_eq!(c.word(1 + 3 * i), mode);
            let fingerprint = (c.word(2 + 3 * i), c.word(3 + 3 * i));
            let expected = if mode == code(Rounding::Up) { up } else { down };
            assert_eq!(fingerprint, expected);
        }
        seen.push(mode);
    }
    seen.sort();
    assert_eq!(seen, vec![code(Rounding::Down), code(Rounding::Up)]);
}
