//! Differential verification of `@vesper/pulse` (`crates/vesper-pulse/vesper/`):
//! relocatable segmented `FrameStack`, `FrameId`, `Address`, `FrameError`,
//! word/byte/address read/write, LIFO `pop`, relocation, and error invariants
//! against `vesper_pulse::frames::FrameStack`.
mod std_io_support;

use std_io_support::{MODES, Temp, agree};
use vesper::{ffi_lowering, packages};
use vesper_pulse::frames::{Address, FrameError, FrameStack};

fn pulse_imports() -> ffi_lowering::Imports {
    let (triple, abi) = packages::vsys_target(None).expect("host target");
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let path = format!("crates/vesper-pulse/vesper/pulse-{os}-{arch}.vsys");
    let text = std::fs::read_to_string(&path).expect("read pulse .vsys");
    let text = text.replace("@TARGET@", triple).replace("@ABI@", abi);
    let mut imports = ffi_lowering::Imports::new();
    imports
        .add(&text, ffi_lowering::Handler::Direct)
        .expect("import pulse .vsys");
    imports
}

fn check_pulse_driver(source: &str) -> ffi_lowering::ForeignChecked {
    let dir = Temp::new("pulse-port");
    let pulse_pkg = std::fs::canonicalize("crates/vesper-pulse/vesper").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/pulse-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper/pulse\" = {{ version = \"^0.1\", path = {:?} }}\n",
            pulse_pkg
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.ves"), source).unwrap();
    let assembled = packages::load(dir.path()).expect("load @test/pulse-driver");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &pulse_imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics))
}

#[test]
fn segmented_relocation_and_word_ops_match_rust() {
    let mut stack = FrameStack::new(16).unwrap();
    let a = stack.push(16).unwrap();
    let b = stack.push(24).unwrap();
    let x = Address {
        frame: a,
        offset: 8,
    };
    let y = Address {
        frame: b,
        offset: 16,
    };
    stack.write_word(x, 20).unwrap();
    stack.write_word(y, 22).unwrap();
    stack.relocate().unwrap();
    let rust_sum = stack.read_word(x).unwrap() + stack.read_word(y).unwrap();
    stack.write_word(x, 100).unwrap();
    let rust_after = stack.read_word(x).unwrap() + stack.read_word(y).unwrap();
    let rust_depth = stack.depth() as u64;
    let rust_segs = stack.segments() as u64;
    let rust_bytes = stack.bytes() as u64;
    let popped_b = stack.pop(b).unwrap();
    let popped_a = stack.pop(a).unwrap();
    let rust_pop_lens = (popped_b.len() as u64) * 100 + (popped_a.len() as u64);
    let rust_final_depth = stack.depth() as u64;
    let rust_final_segs = stack.segments() as u64;

    let src = r#"
use @vesper/pulse::{
    Address, FrameError, frame_stack_bytes, frame_stack_depth, frame_stack_new, frame_stack_pop,
    frame_stack_push, frame_stack_read_word, frame_stack_relocate, frame_stack_segments,
    frame_stack_write_word,
};

pub fn check() -> u64 {
    handle Fail[FrameError] in {
        let mut stack = frame_stack_new(16);
        let a = frame_stack_push(inout stack, 16);
        let b = frame_stack_push(inout stack, 24);
        let x = Address { frame: a, offset: 8 };
        let y = Address { frame: b, offset: 16 };
        frame_stack_write_word(inout stack, x, 20);
        frame_stack_write_word(inout stack, y, 22);
        frame_stack_relocate(inout stack);
        let sum = wrapping_add(frame_stack_read_word(in stack, x), frame_stack_read_word(in stack, y));
        frame_stack_write_word(inout stack, x, 100);
        let after = wrapping_add(frame_stack_read_word(in stack, x), frame_stack_read_word(in stack, y));
        let depth = frame_stack_depth(in stack);
        let segs = frame_stack_segments(in stack);
        let bytes = frame_stack_bytes(in stack);
        let pb = frame_stack_pop(inout stack, b);
        let pa = frame_stack_pop(inout stack, a);
        let pop_lens = wrapping_add(wrapping_mul(pb.len(), 100), pa.len());
        let fdepth = frame_stack_depth(in stack);
        let fsegs = frame_stack_segments(in stack);
        let mut acc = sum;
        acc = wrapping_add(acc, wrapping_mul(after, 1000));
        acc = wrapping_add(acc, wrapping_mul(depth, 1000000));
        acc = wrapping_add(acc, wrapping_mul(segs, 10000000));
        acc = wrapping_add(acc, wrapping_mul(bytes, 100000000));
        acc = wrapping_add(acc, wrapping_mul(pop_lens, 10000000000));
        acc = wrapping_add(acc, wrapping_add(fdepth, fsegs));
        acc
    } with {
        return x => x,
        raise(_e, _k) => 0,
    }
}
"#;
    let checked = check_pulse_driver(src);
    let expected = rust_sum
        + rust_after * 1000
        + rust_depth * 1_000_000
        + rust_segs * 10_000_000
        + rust_bytes * 100_000_000
        + rust_pop_lens * 10_000_000_000
        + rust_final_depth
        + rust_final_segs;
    assert_eq!(agree(&checked, "check", &MODES), expected);
}

#[test]
fn stored_addresses_and_stale_frame_errors_match_rust() {
    let mut stack = FrameStack::new(32).unwrap();
    let a = stack.push(16).unwrap();
    let b = stack.push(16).unwrap();
    let target = Address {
        frame: b,
        offset: 8,
    };
    let slot = Address {
        frame: a,
        offset: 0,
    };
    stack.write_address(slot, target).unwrap();
    let roundtrip = stack.read_address(slot).unwrap();
    assert_eq!(roundtrip, target);
    assert_eq!(stack.pop(a), Err(FrameError::NotTopFrame));
    stack.pop(b).unwrap();
    assert_eq!(stack.read_address(slot), Err(FrameError::StaleFrame));
    assert_eq!(stack.live_id(b.bits()), Err(FrameError::StaleFrame));
    assert_eq!(
        stack.read(
            Address {
                frame: a,
                offset: 12
            },
            8
        ),
        Err(FrameError::OutOfBounds)
    );
    assert!(matches!(FrameStack::new(0), Err(FrameError::InvalidSize)));

    let src = r#"
use @vesper/pulse::{
    Address, FrameError, frame_stack_live_id, frame_stack_new, frame_stack_pop,
    frame_stack_push, frame_stack_read, frame_stack_read_address, frame_stack_write_address,
};

fn err_code(e: FrameError) -> u64 {
    match e {
        FrameError::Allocation => 1,
        FrameError::ExhaustedIds => 2,
        FrameError::InvalidSize => 3,
        FrameError::StaleFrame => 4,
        FrameError::OutOfBounds => 5,
        FrameError::NotTopFrame => 6,
    }
}

pub fn check_errors() -> u64 {
    let bad = handle Fail[FrameError] in {
        let _s = frame_stack_new(0);
        0
    } with {
        return x => x,
        raise(e, _k) => err_code(e),
    };
    handle Fail[FrameError] in {
        let mut stack = frame_stack_new(32);
        let a = frame_stack_push(inout stack, 16);
        let b = frame_stack_push(inout stack, 16);
        let target = Address { frame: b, offset: 8 };
        let slot = Address { frame: a, offset: 0 };
        frame_stack_write_address(inout stack, slot, target);
        let rt = frame_stack_read_address(in stack, slot);
        let rt_ok: u64 = if rt.frame.bits == b.bits && rt.offset == 8 { 1 } else { 0 };
        let not_top = handle Fail[FrameError] in {
            let _p = frame_stack_pop(inout stack, a);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let _pb = frame_stack_pop(inout stack, b);
        let stale_addr = handle Fail[FrameError] in {
            let _r = frame_stack_read_address(in stack, slot);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let stale_id = handle Fail[FrameError] in {
            let _id = frame_stack_live_id(in stack, b.bits);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let oob = handle Fail[FrameError] in {
            let _bytes = frame_stack_read(in stack, Address { frame: a, offset: 12 }, 8);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let mut acc = bad;
        acc = wrapping_add(acc, wrapping_mul(rt_ok, 10));
        acc = wrapping_add(acc, wrapping_mul(not_top, 100));
        acc = wrapping_add(acc, wrapping_mul(stale_addr, 1000));
        acc = wrapping_add(acc, wrapping_mul(stale_id, 10000));
        acc = wrapping_add(acc, wrapping_mul(oob, 100000));
        acc
    } with {
        return x => x,
        raise(_e, _k) => 0,
    }
}
"#;
    let checked = check_pulse_driver(src);
    let expected = 3 + 10 + 6 * 100 + 4 * 1000 + 4 * 10000 + 5 * 100000;
    assert_eq!(agree(&checked, "check_errors", &MODES), expected);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_epoll_pipe_roundtrip() {
    let src = r#"
use @vesper/pulse::epoll_pipe_roundtrip_check;

pub fn check_epoll() -> u64 eff [Ffi[PulseSys]] {
    epoll_pipe_roundtrip_check()
}
"#;
    let checked = check_pulse_driver(src);
    assert_eq!(
        agree(
            &checked,
            "check_epoll",
            &[std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt],
        ),
        100
    );
}

#[cfg(windows)]
#[test]
fn windows_iocp_roundtrip() {
    let src = r#"
use @vesper/pulse::iocp_roundtrip_check;

pub fn check_iocp() -> u64 eff [Ffi[PulseSys]] {
    iocp_roundtrip_check()
}
"#;
    let checked = check_pulse_driver(src);
    assert_eq!(
        agree(
            &checked,
            "check_iocp",
            &[std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt],
        ),
        100
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_kqueue_pipe_roundtrip() {
    let src = r#"
use @vesper/pulse::kqueue_pipe_roundtrip_check;

pub fn check_kqueue() -> u64 eff [Ffi[PulseSys]] {
    kqueue_pipe_roundtrip_check()
}
"#;
    let checked = check_pulse_driver(src);
    assert_eq!(
        agree(
            &checked,
            "check_kqueue",
            &[std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt],
        ),
        100
    );
}

#[test]
fn driver_kind_from_name_matches_rust() {
    use vesper_pulse::driver::DriverKind as RustDriverKind;

    let cases = [
        ("io_uring", Some(RustDriverKind::IoUring)),
        ("  IO-URING \n", Some(RustDriverKind::IoUring)),
        ("uring", Some(RustDriverKind::IoUring)),
        ("EPOLL", Some(RustDriverKind::Epoll)),
        ("kqueue", Some(RustDriverKind::Kqueue)),
        ("iocp", Some(RustDriverKind::Iocp)),
        ("unknown", None),
        ("", None),
    ];
    for (i, (input, expected)) in cases.iter().enumerate() {
        assert_eq!(RustDriverKind::from_name(input), *expected, "case {i}");
    }

    let src = r#"
use @vesper/pulse::{DriverKind, driver_kind_from_name, driver_kind_id};

fn encode(opt: Option[DriverKind]) -> u64 {
    match opt {
        Option::Some(k) => wrapping_add(widen_u64(driver_kind_id(k)), 1),
        Option::None => 0,
    }
}

pub fn check_names() -> u64 {
    let mut acc: u64 = 0;
    acc = wrapping_add(acc, encode(driver_kind_from_name(in "io_uring".as_bytes())));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "  IO-URING \n".as_bytes())), 10));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "uring".as_bytes())), 100));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "EPOLL".as_bytes())), 1000));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "kqueue".as_bytes())), 10000));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "iocp".as_bytes())), 100000));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "unknown".as_bytes())), 1000000));
    acc = wrapping_add(acc, wrapping_mul(encode(driver_kind_from_name(in "".as_bytes())), 10000000));
    acc
}
"#;
    let checked = check_pulse_driver(src);
    let expected = 1 + 10 + 100 + 2 * 1000 + 3 * 10000 + 4 * 100000;
    assert_eq!(agree(&checked, "check_names", &MODES), expected);
}

#[test]
fn readiness_queue_protocol_invariants() {
    let src = r#"
use @vesper/pulse::{
    DriverError, DriverKind, Operation, driver_kind_id, readiness_queue_cancel_all,
    readiness_queue_complete, readiness_queue_has_ready, readiness_queue_in_flight,
    readiness_queue_kind, readiness_queue_mark_cancelled, readiness_queue_new,
    readiness_queue_pending, readiness_queue_submit, readiness_queue_take_ready,
};

fn err_code(e: DriverError) -> u64 {
    match e {
        DriverError::QueueFull => 1,
        DriverError::UnknownRequest => 2,
        DriverError::InvalidCapacity => 3,
    }
}

pub fn check_queue() -> u64 {
    let bad_cap = handle Fail[DriverError] in {
        let _q = readiness_queue_new(DriverKind::Epoll, 0);
        0
    } with {
        return x => x,
        raise(e, _k) => err_code(e),
    };
    handle Fail[DriverError] in {
        let mut q = readiness_queue_new(DriverKind::Epoll, 2);
        let k = widen_u64(driver_kind_id(readiness_queue_kind(in q)));
        let r1 = readiness_queue_submit(inout q, 3, Operation::Read, 0, 64, 11);
        let r2 = readiness_queue_submit(inout q, 4, Operation::Write, 8, 32, 22);
        let full = handle Fail[DriverError] in {
            let _r3 = readiness_queue_submit(inout q, 5, Operation::Read, 0, 16, 33);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let _c1 = readiness_queue_mark_cancelled(inout q, r1);
        readiness_queue_complete(inout q, r1, false, 0, 125);
        // `r1` is in `ready`, `r2` is in `waiting`, so `pending` == 2 (still full).
        let still_full = handle Fail[DriverError] in {
            let _r3 = readiness_queue_submit(inout q, 5, Operation::Read, 0, 16, 33);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let inf = readiness_queue_in_flight(in q);
        let pend = readiness_queue_pending(in q);
        let has_r = if readiness_queue_has_ready(in q) { 1 } else { 0 };
        let drained = readiness_queue_take_ready(inout q);
        let c_all = readiness_queue_cancel_all(inout q);
        readiness_queue_complete(inout q, r2, true, 32, 0);
        let drained2 = readiness_queue_take_ready(inout q);
        let unk = handle Fail[DriverError] in {
            readiness_queue_complete(inout q, r2, true, 0, 0);
            0
        } with {
            return x => x,
            raise(e, _k) => err_code(e),
        };
        let s1 = match drained.try_get(0) {
            Option::Some(v) => if v.cancelled && !v.ok && v.id.bits == r1.bits { 1 } else { 0 },
            Option::None => 0,
        };
        let s2 = match drained2.try_get(0) {
            Option::Some(v) => if v.cancelled && v.ok && v.bytes == 32 { 1 } else { 0 },
            Option::None => 0,
        };
        let mut acc = bad_cap;
        acc = wrapping_add(acc, wrapping_mul(k, 10));
        acc = wrapping_add(acc, wrapping_mul(full, 100));
        acc = wrapping_add(acc, wrapping_mul(still_full, 1000));
        acc = wrapping_add(acc, wrapping_mul(inf, 10000));
        acc = wrapping_add(acc, wrapping_mul(pend, 100000));
        acc = wrapping_add(acc, wrapping_mul(has_r, 1000000));
        acc = wrapping_add(acc, wrapping_mul(c_all, 10000000));
        acc = wrapping_add(acc, wrapping_mul(unk, 100000000));
        acc = wrapping_add(acc, wrapping_mul(s1, 1000000000));
        acc = wrapping_add(acc, wrapping_mul(s2, 10000000000));
        acc
    } with {
        return x => x,
        raise(_e, _k) => 0,
    }
}
"#;
    let checked = check_pulse_driver(src);
    let expected = 3
        + 10
        + 100
        + 1000
        + 10000
        + 2 * 100000
        + 1000000
        + 10000000
        + 2 * 100000000
        + 1000000000
        + 10000000000;
    assert_eq!(agree(&checked, "check_queue", &MODES), expected);
}

#[test]
fn scheduler_state_machine_invariants() {
    let src = r#"
use @vesper/pulse::{
    FiberEvent, StepKind, scheduler_apply_step, scheduler_cancel_all, scheduler_completed_count,
    scheduler_enqueue_io, scheduler_failure_count, scheduler_is_idle, scheduler_new,
    scheduler_pop_ready, scheduler_ready_count, scheduler_spawn, scheduler_take_completed,
    scheduler_take_failure,
};

pub fn check_scheduler() -> u64 {
    let mut s = scheduler_new();
    let idle0: u64 = if scheduler_is_idle(in s) { 1 } else { 0 };
    scheduler_spawn(inout s, 10);
    scheduler_spawn(inout s, 20);
    let rc0 = scheduler_ready_count(in s);
    let e1 = match scheduler_pop_ready(inout s) {
        Option::Some(e) => if e.fiber_id == 10 {
            match e.event {
                FiberEvent::Start => 1,
                _ => 0,
            }
        } else {
            0
        },
        Option::None => 0,
    };
    scheduler_apply_step(inout s, StepKind::Yield(10));
    scheduler_apply_step(inout s, StepKind::Io(1));
    let idle1: u64 = if scheduler_is_idle(in s) { 1 } else { 0 };
    let cancelled = scheduler_cancel_all(inout s);
    let e2 = match scheduler_pop_ready(inout s) {
        Option::Some(e) => if e.fiber_id == 20 {
            match e.event {
                FiberEvent::Cancelled => 1,
                _ => 0,
            }
        } else {
            0
        },
        Option::None => 0,
    };
    scheduler_apply_step(inout s, StepKind::Failed(7));
    scheduler_enqueue_io(inout s, 30, true, 64, 0);
    let _e3 = scheduler_pop_ready(inout s);
    let e4 = match scheduler_pop_ready(inout s) {
        Option::Some(e) => if e.fiber_id == 30 {
            match e.event {
                FiberEvent::IoOk(n) => if n == 64 { 1 } else { 0 },
                _ => 0,
            }
        } else {
            0
        },
        Option::None => 0,
    };
    scheduler_apply_step(inout s, StepKind::Complete(42));
    let cc = scheduler_completed_count(in s);
    let fc = scheduler_failure_count(in s);
    let cval = match scheduler_take_completed(inout s) {
        Option::Some(v) => v,
        Option::None => 0,
    };
    let fval = match scheduler_take_failure(inout s) {
        Option::Some(v) => widen_u64(v),
        Option::None => 0,
    };
    let idle2: u64 = if scheduler_is_idle(in s) { 1 } else { 0 };
    let mut acc = idle0;
    acc = wrapping_add(acc, wrapping_mul(rc0, 10));
    acc = wrapping_add(acc, wrapping_mul(e1, 100));
    acc = wrapping_add(acc, wrapping_mul(idle1, 1000));
    acc = wrapping_add(acc, wrapping_mul(cancelled, 10000));
    acc = wrapping_add(acc, wrapping_mul(e2, 100000));
    acc = wrapping_add(acc, wrapping_mul(e4, 1000000));
    acc = wrapping_add(acc, wrapping_mul(cc, 10000000));
    acc = wrapping_add(acc, wrapping_mul(fc, 100000000));
    acc = wrapping_add(acc, wrapping_mul(cval, 1000000000));
    acc = wrapping_add(acc, wrapping_mul(fval, 100000000000));
    acc = wrapping_add(acc, wrapping_mul(idle2, 1000000000000));
    acc
}
"#;
    let checked = check_pulse_driver(src);
    let expected = 1
        + 2 * 10
        + 100
        + 2 * 10000
        + 100000
        + 1000000
        + 10000000
        + 100000000
        + 42 * 1000000000
        + 7 * 100000000000
        + 1000000000000;
    assert_eq!(agree(&checked, "check_scheduler", &MODES), expected);
}

#[test]
fn select_and_preference_match_rust() {
    use vesper_pulse::driver::DriverKind as RustDriverKind;
    use vesper_pulse::select::{Preference as RustPref, host_order as rust_host_order};

    assert_eq!(RustPref::parse("").unwrap(), RustPref::Auto);
    assert_eq!(RustPref::parse("  AUTO \n").unwrap(), RustPref::Auto);
    assert_eq!(
        RustPref::parse("epoll").unwrap(),
        RustPref::Only(RustDriverKind::Epoll)
    );
    assert!(RustPref::parse("bogus").is_err());

    let rust_order = rust_host_order();
    let rust_order_code: u64 = rust_order.iter().enumerate().fold(0u64, |acc, (i, k)| {
        let id = match k {
            RustDriverKind::IoUring => 1u64,
            RustDriverKind::Epoll => 2u64,
            RustDriverKind::Kqueue => 3u64,
            RustDriverKind::Iocp => 4u64,
        };
        acc + id * 10u64.pow(i as u32)
    });
    let rust_compiled_mask: u64 = RustDriverKind::ALL
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, k)| {
            acc + if k.compiled() { 1u64 << i } else { 0 }
        });

    let src = r#"
use @vesper/pulse::{
    DriverKind, Preference, SelectError, driver_kind_compiled, driver_kind_id, host_order,
    preference_parse, select_driver_for_host,
};

fn pref_code(p: Preference) -> u64 {
    match p {
        Preference::Auto => 1,
        Preference::Only(k) => wrapping_add(widen_u64(driver_kind_id(k)), 2),
    }
}

pub fn check_select() -> u64 {
    let p_empty = handle Fail[SelectError] in {
        pref_code(preference_parse(in "".as_bytes()))
    } with {
        return x => x,
        raise(_e, _k) => 0,
    };
    let p_auto = handle Fail[SelectError] in {
        pref_code(preference_parse(in "  AUTO \n".as_bytes()))
    } with {
        return x => x,
        raise(_e, _k) => 0,
    };
    let p_epoll = handle Fail[SelectError] in {
        pref_code(preference_parse(in "epoll".as_bytes()))
    } with {
        return x => x,
        raise(_e, _k) => 0,
    };
    let p_bad = handle Fail[SelectError] in {
        let _ = preference_parse(in "bogus".as_bytes());
        0
    } with {
        return x => x,
        raise(e, _k) => match e {
            SelectError::UnknownDriver => 9,
            _ => 0,
        },
    };
    let order = host_order();
    let mut order_code: u64 = 0;
    let mut mult: u64 = 1;
    for i in 0..order.len() {
        match order.try_get(i) {
            Option::Some(k) => {
                let id = wrapping_add(widen_u64(driver_kind_id(k)), 1);
                order_code = wrapping_add(order_code, wrapping_mul(id, mult));
                mult = wrapping_mul(mult, 10);
            }
            Option::None => {}
        }
    }
    let mut mask: u64 = 0;
    if driver_kind_compiled(DriverKind::IoUring) { mask = mask | 1; }
    if driver_kind_compiled(DriverKind::Epoll) { mask = mask | 2; }
    if driver_kind_compiled(DriverKind::Kqueue) { mask = mask | 4; }
    if driver_kind_compiled(DriverKind::Iocp) { mask = mask | 8; }
    let auto_sel = handle Fail[SelectError] in {
        wrapping_add(widen_u64(driver_kind_id(select_driver_for_host(Preference::Auto))), 1)
    } with {
        return x => x,
        raise(_e, _k) => 0,
    };
    let mut acc = p_empty;
    acc = wrapping_add(acc, wrapping_mul(p_auto, 10));
    acc = wrapping_add(acc, wrapping_mul(p_epoll, 100));
    acc = wrapping_add(acc, wrapping_mul(p_bad, 1000));
    acc = wrapping_add(acc, wrapping_mul(order_code, 10000));
    acc = wrapping_add(acc, wrapping_mul(mask, 1000000));
    acc = wrapping_add(acc, wrapping_mul(auto_sel, 100000000));
    acc
}
"#;
    let checked = check_pulse_driver(src);
    let expected_auto_sel: u64 = if cfg!(target_os = "linux") {
        1
    } else if cfg!(target_os = "macos") {
        3
    } else {
        4
    };
    let expected = 1
        + 10
        + 3 * 100
        + 9 * 1000
        + rust_order_code * 10000
        + rust_compiled_mask * 1_000_000
        + expected_auto_sel * 100_000_000;
    assert_eq!(agree(&checked, "check_select", &MODES), expected);
}

#[test]
fn seeded_work_stealing_pool_and_trace_match_rust() {
    use vesper_pulse::driver::DriverKind;
    use vesper_pulse::pool::{Pool, PoolConfig, TraceEvent as RustTraceEvent};
    use vesper_pulse::scheduler::{Event, Resume, Step};
    use vesper_pulse::select::AnyDriver;

    #[derive(Clone, Copy, Debug)]
    struct TestFiber {
        yields: u64,
        fail: bool,
        tag: u64,
    }

    impl Resume for TestFiber {
        fn resume(mut self, _event: Event) -> Step<Self> {
            if self.yields > 0 {
                self.yields -= 1;
                Step::Yield(self)
            } else if self.fail {
                Step::Failed(format!("err-{}", self.tag))
            } else {
                Step::Complete(self.tag.to_le_bytes().to_vec())
            }
        }
    }

    let cfg = PoolConfig::new(4).seed(0x5150_1234).driver_wakeup(false);
    let pool = Pool::new(cfg).expect("rust pool");
    pool.spawn(TestFiber {
        yields: 0,
        fail: false,
        tag: 10,
    });
    pool.spawn(TestFiber {
        yields: 2,
        fail: false,
        tag: 20,
    });
    pool.spawn(TestFiber {
        yields: 3,
        fail: true,
        tag: 30,
    });
    pool.spawn_pinned(
        2,
        TestFiber {
            yields: 1,
            fail: false,
            tag: 40,
        },
    )
    .unwrap();
    pool.spawn(TestFiber {
        yields: 2,
        fail: false,
        tag: 50,
    });

    let kind: DriverKind = DriverKind::ALL
        .iter()
        .copied()
        .find(|k| k.compiled())
        .expect("compiled driver");
    let out = pool
        .run_seeded(|_| {
            AnyDriver::start(kind, 16).map_err(|e| std::io::Error::other(format!("{e:?}")))
        })
        .expect("rust run_seeded");

    let fnv_mix = |mut h: u64, v: u64| -> u64 {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let mut h: u64 = 14695981039346656037;
    h = fnv_mix(h, out.completed.len() as u64);
    h = fnv_mix(h, out.failures.len() as u64);
    h = fnv_mix(h, out.abandoned as u64);
    h = fnv_mix(h, out.workers.len() as u64);
    for w in &out.workers {
        h = fnv_mix(h, w.resumed);
        h = fnv_mix(h, w.stolen);
        h = fnv_mix(h, w.steal_attempts);
    }
    h = fnv_mix(h, out.trace.len() as u64);
    for t in &out.trace {
        h = fnv_mix(h, t.step);
        h = fnv_mix(h, t.worker as u64);
        h = fnv_mix(h, t.fiber.get());
        let ev = match t.event {
            RustTraceEvent::Start => 1u64,
            RustTraceEvent::Yield => 2,
            RustTraceEvent::Io => 3,
            RustTraceEvent::Cancelled => 4,
        };
        h = fnv_mix(h, ev);
        match t.stolen_from {
            Some(v) => {
                h = fnv_mix(h, 1);
                h = fnv_mix(h, v as u64);
            }
            None => {
                h = fnv_mix(h, 0);
            }
        }
    }

    let src = r#"
use @vesper/pulse::{
    FiberId, PoolConfig, TraceEntry, TraceEvent, WorkerStats, default_pool_config, new_seeded_pool,
    run_seeded, spawn_fiber, spawn_pinned
};
fn mix_u64(h_in: u64, v: u64) -> u64 {
    let mut h = h_in;
    let mut i: u64 = 0;
    loop {
        if i >= 8 { break; }
        let byte = (wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))) & 255;
        h = wrapping_mul(h ^ byte, 1099511628211);
        i = wrapping_add(i, 1);
    }
    h
}
pub fn check_pool() -> u64 {
    let mut cfg = default_pool_config(4);
    cfg.seed = 0x5150_1234;
    cfg.driver_wakeup = false;
    let mut pool = match new_seeded_pool(cfg) {
        Result::Ok(p) => p,
        Result::Err(_) => return 1,
    };
    let _ = spawn_fiber(inout pool, 0, false, 10, Option::None);
    let _ = spawn_fiber(inout pool, 2, false, 20, Option::None);
    let _ = spawn_fiber(inout pool, 3, true, 30, Option::None);
    let _ = spawn_pinned(inout pool, 2, 1, false, 40);
    let _ = spawn_fiber(inout pool, 2, false, 50, Option::None);
    let out = run_seeded(inout pool);

    let mut h: u64 = 14695981039346656037;
    h = mix_u64(h, out.completed.len());
    h = mix_u64(h, out.failures.len());
    h = mix_u64(h, out.abandoned);
    h = mix_u64(h, out.workers.len());
    let mut wi: u64 = 0;
    loop {
        if wi >= out.workers.len() { break; }
        let w = match out.workers.try_get(wi) {
            Option::Some(v) => v,
            Option::None => WorkerStats {
                resumed: 0, stolen: 0, steal_attempts: 0, submitted: 0, parked: 0, pinned_core: Option::None
            },
        };
        h = mix_u64(h, w.resumed);
        h = mix_u64(h, w.stolen);
        h = mix_u64(h, w.steal_attempts);
        wi = wrapping_add(wi, 1);
    }
    h = mix_u64(h, out.trace.len());
    let mut ti: u64 = 0;
    loop {
        if ti >= out.trace.len() { break; }
        let t = match out.trace.try_get(ti) {
            Option::Some(v) => v,
            Option::None => TraceEntry {
                step: 0, worker: 0, fiber: FiberId { bits: 0 },
                event: TraceEvent::TraceStart, stolen_from: Option::None
            },
        };
        h = mix_u64(h, t.step);
        h = mix_u64(h, t.worker);
        h = mix_u64(h, t.fiber.bits);
        let ev: u64 = match t.event {
            TraceEvent::TraceStart => 1,
            TraceEvent::TraceYield => 2,
            TraceEvent::TraceIo => 3,
            TraceEvent::TraceCancelled => 4,
        };
        h = mix_u64(h, ev);
        match t.stolen_from {
            Option::Some(v) => {
                h = mix_u64(h, 1);
                h = mix_u64(h, v);
            }
            Option::None => {
                h = mix_u64(h, 0);
            }
        }
        ti = wrapping_add(ti, 1);
    }
    h
}
"#;
    let checked = check_pulse_driver(src);
    assert_eq!(agree(&checked, "check_pool", &MODES), h);
}

/// The readiness scenario's Vesper source for this host: the epoll driver
/// on Linux, the kqueue driver on macOS.
fn readiness_scenario(src: &str) -> String {
    if cfg!(target_os = "macos") {
        src.replace("epoll_driver_", "kqueue_driver_")
            .replace("EpollPoll", "KqueuePoll")
            .replace("@PIPE@", "PulseSys::pipe(inout fds)")
    } else {
        src.replace("@PIPE@", "PulseSys::pipe2(inout fds, 524288)")
    }
}

/// The epoll completion driver (#1938): the same scenario through
/// `@vesper/pulse`'s `EpollDriver` and `vesper_pulse::epoll::EpollDriver`,
/// over a pipe and a regular file: a read that waits for data, immediate
/// zero-length and wrong-direction completions, a stream offset refused
/// with `ESPIPE`, inline positional I/O on a file, cancellation, a stream
/// write, and a full queue.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn readiness_completion_driver_matches_rust() {
    use std::io::Write;
    use std::os::fd::{AsRawFd, OwnedFd};
    use vesper_pulse::driver::{
        Completion, CompletionDriver, Operation, PollError, Request, RequestId,
    };
    #[cfg(target_os = "linux")]
    use vesper_pulse::epoll::EpollDriver as Driver32;
    #[cfg(target_os = "macos")]
    use vesper_pulse::kqueue::KqueueDriver as Driver32;

    const QUEUE_FULL: u64 = 1000001;
    fn mix(h: u64, v: u64) -> u64 {
        (h ^ v).wrapping_mul(1099511628211)
    }
    fn hash(s: &[u8]) -> u64 {
        s.iter()
            .fold(14695981039346656037, |h, &b| mix(h, u64::from(b)))
    }
    fn code(e: &std::io::Error) -> u64 {
        match e.raw_os_error() {
            Some(c) => c as u64,
            None if e.kind() == std::io::ErrorKind::WouldBlock => QUEUE_FULL,
            None => 999,
        }
    }
    type Driver = Driver32<u32>;
    fn submit(
        h: u64,
        d: &mut Driver,
        file: &std::fs::File,
        operation: Operation,
        buffer: &[u8],
        offset: u64,
    ) -> (u64, Option<RequestId>) {
        let request = Request {
            file: file.try_clone().unwrap(),
            buffer: buffer.to_vec(),
            continuation: 0,
            operation,
            offset,
        };
        match d.submit(request) {
            Ok(id) => (mix(mix(h, 1), id.get()), Some(id)),
            Err((e, _)) => (mix(mix(h, 2), code(&e)), None),
        }
    }
    fn completions(mut h: u64, done: &[Completion<u32>], error: u64) -> u64 {
        h = mix(mix(h, done.len() as u64), error);
        for c in done {
            h = mix(h, c.id.get());
            h = match &c.result {
                Ok(n) => mix(mix(mix(h, 1), *n as u64), 0),
                Err(e) => mix(mix(mix(h, 0), 0), code(e)),
            };
            h = mix(h, hash(&c.request.buffer));
        }
        h
    }
    fn poll(h: u64, d: &mut Driver, wait: bool) -> u64 {
        match d.poll(wait) {
            Ok(done) => completions(h, &done, 0),
            Err(PollError { error, completed }) => completions(h, &completed, code(&error)),
        }
    }
    fn pipe() -> (std::fs::File, std::fs::File) {
        let (r, w) = std::io::pipe().unwrap();
        (
            std::fs::File::from(OwnedFd::from(r)),
            std::fs::File::from(OwnedFd::from(w)),
        )
    }

    let dir = Temp::new("pulse-epoll-driver");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(dir.path().join("data"))
        .unwrap();

    // The scenario, in Rust.
    let mut h: u64 = 14695981039346656037;
    let mut d = Driver::new(8).unwrap();
    let (r, mut w) = pipe();
    (h, _) = submit(h, &mut d, &r, Operation::Read, &[0; 8], 0);
    h = poll(h, &mut d, false);
    w.write_all(b"hi").unwrap();
    h = poll(h, &mut d, true);
    (h, _) = submit(h, &mut d, &r, Operation::Write, &[0], 0);
    (h, _) = submit(h, &mut d, &r, Operation::Read, &[], 0);
    (h, _) = submit(h, &mut d, &r, Operation::Read, &[0; 8], 5);
    (h, _) = submit(h, &mut d, &file, Operation::Write, b"abc", 3);
    (h, _) = submit(h, &mut d, &file, Operation::Read, &[0; 6], 0);
    h = poll(h, &mut d, false);
    let id;
    (h, id) = submit(h, &mut d, &r, Operation::Read, &[0; 4], 0);
    let id = id.unwrap();
    let cancelled = matches!((d.cancel(id), d.cancel(id)), (Ok(true), Ok(false)));
    h = mix(h, u64::from(cancelled));
    h = poll(h, &mut d, false);
    (h, _) = submit(h, &mut d, &w, Operation::Write, b"xyz", 0);
    h = poll(h, &mut d, true);
    (h, _) = submit(h, &mut d, &r, Operation::Read, &[0; 8], 0);
    h = poll(h, &mut d, true);
    h = mix(h, d.pending() as u64);
    let mut d2 = Driver::new(1).unwrap();
    (h, _) = submit(h, &mut d2, &r, Operation::Read, &[0; 8], 0);
    (h, _) = submit(h, &mut d2, &w, Operation::Write, &[0], 0);
    h = mix(h, d2.pending() as u64);
    drop(d2);
    drop(d);
    let want = h;

    let src = r#"
use @vesper/pulse::{
    EpollPoll, Operation, RequestId, epoll_driver_new,
    epoll_driver_submit, epoll_driver_poll, epoll_driver_cancel, epoll_driver_close,
    epoll_driver_pending
};

fn mix(h: u64, v: u64) -> u64 {
    wrapping_mul(h ^ v, 1099511628211)
}

fn hash(in s: Slice[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for i in 0..s.len() {
        let b = match s.try_get(i) {
            Option::Some(b) => b,
            Option::None => 0,
        };
        h = mix(h, widen_u64(b));
    }
    h
}

fn zeros(n: u64) -> ChunkSeq[u8] {
    let mut z: ChunkSeq[u8] = ChunkSeq::new[u8]();
    z.resize(n, 0);
    z
}

fn submitted(h: u64, r: Result[RequestId, u32]) -> u64 {
    match r {
        Result::Ok(id) => mix(mix(h, 1), id.bits),
        Result::Err(e) => mix(mix(h, 2), widen_u64(e)),
    }
}

fn polled(h: u64, in p: EpollPoll) -> u64 {
    let mut h = mix(mix(h, p.completions.len()), widen_u64(p.error));
    for k in 0..p.completions.len() {
        let c = match p.completions.try_get(k) {
            Option::Some(c) => c,
            Option::None => return 0,
        };
        h = mix(h, c.id.bits);
        h = if c.ok {
            mix(mix(mix(h, 1), c.bytes), 0)
        } else {
            mix(mix(mix(h, 0), 0), widen_u64(c.errno))
        };
        h = mix(h, hash(in p.data[c.start..wrapping_add(c.start, c.len)]));
    }
    h
}

pub fn scenario() -> u64 eff [Ffi[PulseSys]] {
    let file: i32 = @FD@;
    let mut h: u64 = 14695981039346656037;
    let mut d = match epoll_driver_new(8) {
        Result::Ok(d) => d,
        Result::Err(_) => return 1,
    };
    let mut fds = PipeFds {
        read_fd: -1,
        write_fd: -1,
    };
    let _ = @PIPE@;
    let r = fds.read_fd;
    let w = fds.write_fd;
    let z0 = zeros(0);
    let z1 = zeros(1);
    let z4 = zeros(4);
    let z6 = zeros(6);
    let z8 = zeros(8);
    h = submitted(h, epoll_driver_submit(inout d, r, Operation::Read, in z8.as_slice(), 0, 1));
    h = polled(h, in epoll_driver_poll(inout d, false));
    let _ = PulseSys::write(w, in "hi".as_bytes());
    h = polled(h, in epoll_driver_poll(inout d, true));
    h = submitted(h, epoll_driver_submit(inout d, r, Operation::Write, in z1.as_slice(), 0, 2));
    h = submitted(h, epoll_driver_submit(inout d, r, Operation::Read, in z0.as_slice(), 0, 3));
    h = submitted(h, epoll_driver_submit(inout d, r, Operation::Read, in z8.as_slice(), 5, 4));
    h = submitted(h, epoll_driver_submit(inout d, file, Operation::Write, in "abc".as_bytes(), 3, 5));
    h = submitted(h, epoll_driver_submit(inout d, file, Operation::Read, in z6.as_slice(), 0, 6));
    h = polled(h, in epoll_driver_poll(inout d, false));
    let (h7, id) = match epoll_driver_submit(inout d, r, Operation::Read, in z4.as_slice(), 0, 7) {
        Result::Ok(i) => (mix(mix(h, 1), i.bits), i.bits),
        Result::Err(e) => (mix(mix(h, 2), widen_u64(e)), 0),
    };
    h = h7;
    let rid = RequestId {
        bits: id,
    };
    let first = epoll_driver_cancel(inout d, rid);
    let again = epoll_driver_cancel(inout d, rid);
    let cancelled: u64 = match (first, again) {
        (Result::Ok(true), Result::Ok(false)) => 1,
        _ => 0,
    };
    h = mix(h, cancelled);
    h = polled(h, in epoll_driver_poll(inout d, false));
    h = submitted(h, epoll_driver_submit(inout d, w, Operation::Write, in "xyz".as_bytes(), 0, 8));
    h = polled(h, in epoll_driver_poll(inout d, true));
    h = submitted(h, epoll_driver_submit(inout d, r, Operation::Read, in z8.as_slice(), 0, 9));
    h = polled(h, in epoll_driver_poll(inout d, true));
    h = mix(h, epoll_driver_pending(in d));
    let mut d2 = match epoll_driver_new(1) {
        Result::Ok(d) => d,
        Result::Err(_) => return 2,
    };
    h = submitted(h, epoll_driver_submit(inout d2, r, Operation::Read, in z8.as_slice(), 0, 10));
    h = submitted(h, epoll_driver_submit(inout d2, w, Operation::Write, in z1.as_slice(), 0, 11));
    h = mix(h, epoll_driver_pending(in d2));
    let _ = epoll_driver_close(in d2);
    let _ = epoll_driver_close(in d);
    let _ = PulseSys::close(r);
    let _ = PulseSys::close(w);
    h
}
"#
    .replace("@FD@", &file.as_raw_fd().to_string());
    let checked = check_pulse_driver(&readiness_scenario(&src));
    assert_eq!(
        agree(
            &checked,
            "scenario",
            &[std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt]
        ),
        want
    );
}
