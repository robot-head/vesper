//! Byte images of parked fibers on every direct-fiber target (Linux
//! x86_64/AArch64, macOS AArch64, Windows x86_64): capture the stack bytes
//! and register frame, restore onto a fixed-address template after the
//! original's reservation is gone, resume to the same result without
//! re-running the tail-resumptive Clock handler, and reject every template
//! or image that differs from the captured fiber.
#![cfg(vesper_direct_fibers)]
use std::rc::Rc;
use vesper_fiber_abi::abi::FiberState;
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
use vesper_fiber_abi::switch::SWITCH_FRAME_BYTES;
use vesper_fiber_abi::{
    Fiber, FiberConfig, FiberImage, FrameAbi, ImageError, Outcome, Parked, Resumption,
};

/// Fixed-address tests free a reservation and then map the same range
/// again; serialize them so another test's mapping cannot take it in between.
static FIXED: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    FIXED.lock().unwrap_or_else(|e| e.into_inner())
}

const ARG: u64 = 17;
const INBOX: [u8; 8] = [3, 1, 4, 1, 5, 9, 2, 6];

fn parked(o: Outcome) -> Parked {
    match o {
        Outcome::Parked(k) => k,
        Outcome::Exited(e) => panic!("expected a park, fiber exited: {e:?}"),
    }
}

fn evidence() -> (Rc<EvidenceVector>, *const u64) {
    let mut registry = EvidenceRegistry::new();
    prototype::pulse_read_evidence(&mut registry).unwrap()
}

fn fixed() -> FiberConfig {
    FiberConfig {
        data_bytes: prototype::DATA_BYTES,
        ..Default::default()
    }
    .at_free_fixed_base()
    .unwrap()
}

fn resume(k: Parked, value: u64) -> Outcome {
    let ev = k.request().evidence;
    k.resume(Resumption::value(ev, value)).unwrap()
}

/// Run to the read park (park 2) and deliver the inbox.
fn to_read(jit: &JitPrototype, ev: &Rc<EvidenceVector>, config: FiberConfig) -> Parked {
    let k = parked(
        jit.spawn_pulse_read_with(ARG, ev.clone(), config)
            .unwrap()
            .start(),
    );
    assert_eq!(k.request().op, prototype::OP_YIELD);
    let mut k = parked(resume(k, 0));
    assert_eq!(k.request().op, prototype::OP_READ);
    k.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&INBOX);
    k
}

fn finish(k: Parked) -> u64 {
    let e = resume(k, 8).exited().unwrap();
    assert!(e.completed(), "{e:?}");
    assert_eq!(e.word(0), prototype::PULSE_READ_COUNTER);
    e.result
}

fn opts() -> [OptLevel; 2] {
    [OptLevel::None, OptLevel::Speed]
}

#[test]
fn stack_image_and_register_frame_describe_the_parked_fiber() {
    let _serial = serial();
    for opt in opts() {
        let jit = JitPrototype::new(opt).unwrap();
        for config in [
            FiberConfig {
                data_bytes: prototype::DATA_BYTES,
                ..Default::default()
            },
            fixed(),
        ] {
            let (ev, _) = evidence();
            let k = to_read(&jit, &ev, config);
            let (base, top) = k.fiber().stack_range();
            let image = k.stack_image();
            assert_eq!(image.top, top);
            assert!(image.saved_sp >= base && image.saved_sp < top);
            assert_eq!(image.saved_sp % 16, 0);
            assert_eq!(image.bytes.len() as u64, top - image.saved_sp);
            assert!(image.bytes.len() >= SWITCH_FRAME_BYTES);
            // The self-pointing words the relocation check reports are the
            // image's words at the same addresses.
            for (at, value) in k.stack_self_pointers() {
                assert_eq!(image.word_at(at), Some(value));
            }
            let frame = k.register_frame();
            assert_eq!(frame.abi, FrameAbi::HOST);
            assert_eq!(Some(&frame), image.register_frame().as_ref());
            assert_ne!(frame.return_address, 0);
            // `vesper_fiber_park` has no frame of its own (it tail-jumps into
            // the switch), so the switch returns straight into the generated
            // code that called it.
            assert_eq!(
                jit.cfi()
                    .function_at(frame.return_address - 1)
                    .map(|f| f.name.as_str()),
                Some(vesper_fiber_abi::prototype::PULSE_READ_SYMBOL),
                "{opt:?}: return address {:#x}",
                frame.return_address
            );
            match frame.abi {
                FrameAbi::SysVX86_64 | FrameAbi::WindowsX64 => {
                    #[cfg(target_arch = "x86_64")]
                    {
                        let env = vesper_fiber_abi::fpenv::FpEnv::DEFAULT;
                        assert_eq!(frame.get("mxcsr").unwrap() & 0xffc0, u64::from(env.mxcsr));
                        assert_eq!(frame.get("x87_cw"), Some(u64::from(env.x87_control)));
                    }
                }
                FrameAbi::Aarch64 => {
                    assert_eq!(frame.get("fpcr"), Some(0));
                    assert_eq!(frame.general.len(), 12);
                    assert_eq!(frame.vector.len(), 8);
                }
            }
            if frame.abi == FrameAbi::WindowsX64 {
                assert_eq!(frame.get("stack_base"), Some(top));
                assert_eq!(frame.get("stack_limit"), Some(base));
            }
            assert_eq!(
                k.fiber().is_fixed_address(),
                config.fixed_stack_base.is_some()
            );
            assert_eq!(finish(k), prototype::expected_pulse_read(ARG, 1001, &INBOX));
        }
    }
}

#[test]
fn image_restores_at_the_same_address_without_re_execution() {
    let _serial = serial();
    for opt in opts() {
        let jit = JitPrototype::new(opt).unwrap();
        let (ev, clock) = evidence();
        let expected = prototype::expected_pulse_read(ARG, 1001, &INBOX);
        let config = fixed();
        let k = to_read(&jit, &ev, config);
        let (reservation, _) = k.fiber().reservation();
        assert_eq!(Some(reservation as usize), config.fixed_stack_base);
        let image = k.image().unwrap();
        assert_eq!(image.reservation().0, reservation);
        assert_eq!(image.fiber_id(), k.fiber_id());
        assert_eq!(image.generation(), k.token().generation);
        assert_eq!(image.park(), k.request());
        assert_eq!(image.parks(), 2);
        assert_eq!(
            &image.data()[prototype::INBOX..prototype::INBOX + 8],
            &INBOX
        );
        assert_eq!(image.entry_arg(), ARG);
        assert_eq!(image.stack(), &k.stack_image());

        // While the original is parked its reservation is in use: a template
        // at the same base cannot be created, and nothing is replaced.
        let err = jit
            .spawn_pulse_read_with(ARG, ev.clone(), config)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");

        // The live run finishes; the Clock handler ran once.
        assert_eq!(finish(k), expected, "{opt:?} live");
        assert_eq!(unsafe { *clock }, 1);

        // Restore twice from the same image onto fresh templates at the
        // released address: same result, and the direct Clock handler is
        // not run again (no re-execution).
        for round in 0..2 {
            let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
            let template_id = template.id();
            let k = Fiber::from_image(template, &image).unwrap();
            assert_eq!(k.fiber_id(), template_id);
            assert_eq!(k.fiber().state(), FiberState::Parked);
            assert_eq!(k.token().generation, image.generation());
            assert_eq!(k.request(), image.park());
            assert_eq!(k.stack_image(), *image.stack());
            assert_eq!(k.data(), image.data());
            let e = resume(k, 8).exited().unwrap();
            assert_eq!(e.result, expected, "{opt:?} restore {round}");
            assert_eq!(e.parks, 2, "the restored fiber keeps its park count");
            assert_eq!(unsafe { *clock }, 1, "{opt:?}: the Clock handler re-ran");
        }

        // The data frame may move after a restore, as after any park.
        let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
        let mut k = Fiber::from_image(template, &image).unwrap();
        let before = k.data_base();
        k.relocate_data();
        assert_ne!(k.data_base(), before);
        assert_eq!(finish(k), expected);
    }
}

#[test]
fn image_from_the_first_park_resumes_through_both_parks() {
    let _serial = serial();
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let (ev, clock) = evidence();
    let config = fixed();
    let k = parked(
        jit.spawn_pulse_read_with(ARG, ev.clone(), config)
            .unwrap()
            .start(),
    );
    let image = k.image().unwrap();
    let mut k = parked(resume(k, 0));
    k.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&INBOX);
    let expected = finish(k);
    let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
    let k = Fiber::from_image(template, &image).unwrap();
    let mut k = parked(resume(k, 0));
    // A different delivery after the restore point takes effect.
    let other = [1u8; 8];
    k.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&other);
    let e = resume(k, 8).exited().unwrap();
    assert_eq!(e.result, prototype::expected_pulse_read(ARG, 1001, &other));
    assert_ne!(e.result, expected);
    assert_eq!(unsafe { *clock }, 1);
}

#[test]
fn mismatched_templates_are_rejected_and_handed_back() {
    let _serial = serial();
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    // Map everything else first: a JIT module created after the live fiber
    // exits could land in its released range.
    let other_jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let (other_ev, _) = evidence();
    let (ev, _) = evidence();
    let config = fixed();
    let k = to_read(&jit, &ev, config);
    let image = k.image().unwrap();
    let expected = finish(k);

    let reject = |template: Fiber| -> (ImageError, Fiber) {
        let r = *Fiber::from_image(template, &image).unwrap_err();
        assert_eq!(r.template.state(), FiberState::Created);
        (r.error, r.template)
    };

    // A different entry argument (the case re-execution could not detect).
    let e = reject(
        jit.spawn_pulse_read_with(ARG + 1, ev.clone(), config)
            .unwrap(),
    )
    .0;
    assert_eq!(
        e,
        ImageError::ArgumentMismatch {
            captured: ARG,
            template: ARG + 1
        }
    );

    // Different code: another entry of the same module, and the same entry
    // address owned by a different code `Rc`.
    let mut registry = EvidenceRegistry::new();
    let nested_ev = prototype::nested_evidence(&mut registry).unwrap();
    let e = reject(jit.spawn_nested_with(ARG, nested_ev, config).unwrap()).0;
    assert!(matches!(e, ImageError::EntryMismatch { .. }), "{e:?}");
    let other_owner: Rc<dyn std::any::Any> = Rc::new(());
    // SAFETY: the entry is owned by `jit`, which outlives this fiber; the
    // fiber is never started (it is rejected and dropped while Created).
    let foreign = unsafe {
        Fiber::new(jit.pulse_read_entry(), ARG, ev.clone(), other_owner, config).unwrap()
    };
    assert_eq!(reject(foreign).0, ImageError::CodeMismatch);
    let e = reject(
        other_jit
            .spawn_pulse_read_with(ARG, ev.clone(), config)
            .unwrap(),
    )
    .0;
    assert!(matches!(e, ImageError::EntryMismatch { .. }), "{e:?}");

    // A different (equally shaped) evidence vector.
    let e = reject(jit.spawn_pulse_read_with(ARG, other_ev, config).unwrap()).0;
    assert_eq!(e, ImageError::EvidenceMismatch);

    // A different data-frame size, stack size and address.
    let bigger = FiberConfig {
        data_bytes: prototype::DATA_BYTES + 8,
        ..config
    };
    let e = reject(jit.spawn_pulse_read_with(ARG, ev.clone(), bigger).unwrap()).0;
    assert!(matches!(e, ImageError::DataSize { .. }), "{e:?}");
    let smaller_stack = FiberConfig {
        stack_size: config.stack_size / 2,
        ..config
    };
    let e = reject(
        jit.spawn_pulse_read_with(ARG, ev.clone(), smaller_stack)
            .unwrap(),
    )
    .0;
    assert!(matches!(e, ImageError::StackSize { .. }), "{e:?}");
    // Hold the captured range so that the other base is elsewhere.
    let holder = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
    let elsewhere = fixed();
    drop(holder);
    assert_ne!(elsewhere.fixed_stack_base, config.fixed_stack_base);
    let e = reject(
        jit.spawn_pulse_read_with(ARG, ev.clone(), elsewhere)
            .unwrap(),
    )
    .0;
    assert!(matches!(e, ImageError::AddressMismatch { .. }), "{e:?}");
    let floating = FiberConfig {
        fixed_stack_base: None,
        ..config
    };
    let e = reject(
        jit.spawn_pulse_read_with(ARG, ev.clone(), floating)
            .unwrap(),
    )
    .0;
    assert_eq!(e, ImageError::NotFixedAddress);

    // A fiber without a fixed address cannot be imaged.
    let floating_k = to_read(&jit, &ev, floating);
    assert_eq!(floating_k.image().unwrap_err(), ImageError::NotFixedAddress);
    drop(floating_k);

    // After all those rejections the matching template still restores.
    let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
    assert_eq!(
        finish(Fiber::from_image(template, &image).unwrap()),
        expected
    );

    // Once the code is gone, a new module cannot stand in for it even if
    // its allocations land at the old addresses.
    // (A bare reservation holds the range while the new module maps.)
    let hold = vesper_fiber_abi::stack::FiberStack::with_fixed_base(
        config.fixed_stack_base.unwrap(),
        config.stack_size,
    )
    .unwrap();
    drop(jit);
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    drop(hold);
    let e = *Fiber::from_image(
        jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap(),
        &image,
    )
    .unwrap_err();
    assert!(
        matches!(
            e.error,
            ImageError::CodeMismatch | ImageError::EntryMismatch { .. }
        ),
        "{:?}",
        e.error
    );
}

#[test]
fn encoded_images_round_trip_and_reject_damage_and_foreign_threads() {
    let _serial = serial();
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let (ev, _) = evidence();
    let config = fixed();
    let k = to_read(&jit, &ev, config);
    let image = k.image().unwrap();
    let expected = finish(k);
    let bytes = image.encode();
    let decoded = FiberImage::decode(&bytes).unwrap();
    assert_eq!(decoded, image);
    for i in (0..bytes.len()).step_by(7) {
        let mut bad = bytes.clone();
        bad[i] ^= 0x10;
        assert!(FiberImage::decode(&bad).is_err(), "byte {i}");
    }
    for n in [0, 7, 8, 15, 16, bytes.len() / 2, bytes.len() - 1] {
        assert!(FiberImage::decode(&bytes[..n]).is_err(), "prefix {n}");
    }
    assert_eq!(
        FiberImage::decode(&[0u8; 64]).unwrap_err(),
        ImageError::BadMagic
    );
    // Map the template before spawning a thread: the thread's stack could
    // otherwise take the released fixed range.
    let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
    // Sealed per thread: another thread (or process) cannot decode it.
    let copy = bytes.clone();
    let foreign = std::thread::spawn(move || FiberImage::decode(&copy).unwrap_err())
        .join()
        .unwrap();
    assert_eq!(foreign, ImageError::BadSeal);
    // The decoded image restores like the captured one.
    assert_eq!(
        finish(Fiber::from_image(template, &decoded).unwrap()),
        expected
    );
}

#[test]
fn fixed_stack_bases_are_validated() {
    let _serial = serial();
    use vesper_fiber_abi::stack::FiberStack;
    let page = FiberStack::page_size();
    assert_eq!(
        FiberStack::with_fixed_base(0, 64 * 1024)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    // `free_fixed_base` probes and releases a hole, so another thread of the
    // test process (libtest workers, allocator arenas) can map into it before
    // the fixed mapping does. That race surfaces as `AlreadyExists`; it says
    // nothing about the stack, so restart from a fresh probe.
    let mut attempts = 0;
    loop {
        attempts += 1;
        assert!(attempts <= 16, "no stable free fixed base after 16 probes");
        let base = FiberStack::free_fixed_base(64 * 1024).unwrap();
        assert_eq!(
            FiberStack::with_fixed_base(base + 1, 64 * 1024)
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        let s = match FiberStack::with_fixed_base(base, 64 * 1024) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("fixed stack at a free base: {e}"),
        };
        assert_eq!(s.reservation_base(), base);
        assert_eq!(s.len(), 64 * 1024);
        assert_eq!(s.top(), base + s.reservation_len());
        assert_eq!(s.context_slot(), Some(base as *mut u8));
        assert!(s.base() >= base + 2 * page);
        assert_eq!(
            FiberStack::with_fixed_base(base, 64 * 1024)
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        drop(s);
        let again = match FiberStack::with_fixed_base(base, 64 * 1024) {
            Ok(again) => again,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("fixed stack at a released base: {e}"),
        };
        assert_eq!(again.reservation_base(), base);
        break;
    }
    let floating = FiberStack::new(64 * 1024).unwrap();
    assert_eq!(floating.context_slot(), None);
    assert_eq!(
        floating.reservation_base() + floating.guard_len(),
        floating.base()
    );
    assert!(FiberStack::with_fixed_base(usize::MAX & !(page - 1), 64 * 1024).is_err());
}
