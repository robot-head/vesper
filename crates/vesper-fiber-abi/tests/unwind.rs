//! Register unwinding below the saved switch frame through DWARF CFI: a
//! parked fiber three generated frames deep (`deep_entry` -> `deep_mid` ->
//! `deep_leaf` -> park) is walked with the JIT's `.eh_frame`, and every
//! recovered stack and frame pointer is checked against the value the frame
//! itself recorded at its call site. Runs on every direct-fiber target.
#![cfg(vesper_direct_fibers)]
use std::rc::Rc;
use vesper_fiber_abi::abi::exit;
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
use vesper_fiber_abi::image::{FiberImage, FrameAbi};
use vesper_fiber_abi::prototype::{
    self, DEEP_ENTRY_SECRET, DEEP_ENTRY_SYMBOL, DEEP_ENTRY_WORDS, DEEP_LEAF_SYMBOL,
    DEEP_LEAF_WORDS, DEEP_MID_SECRET, DEEP_MID_SYMBOL, DEEP_MID_WORDS, JitPrototype, OptLevel,
};
use vesper_fiber_abi::unwind::{Stop, UnwindError, unwind};
use vesper_fiber_abi::{Parked, Resumption};

const FP: &str = if cfg!(target_arch = "aarch64") {
    "x29"
} else {
    "rbp"
};

fn evidence() -> Rc<EvidenceVector> {
    let mut registry = EvidenceRegistry::new();
    prototype::pulse_read_evidence(&mut registry).unwrap().0
}

fn word(k: &Parked, index: usize) -> u64 {
    u64::from_le_bytes(k.data()[index * 8..index * 8 + 8].try_into().unwrap())
}

const ENTRY_SECRET: u64 = 0x5ec0_0000_e1e1_0001;
const MID_SECRET: u64 = 0x5ec0_0000_d1d1_0002;

/// Spawn the deep fixture with its secrets filled in, and park it.
fn park_deep(
    jit: &JitPrototype,
    arg: u64,
    config: Option<vesper_fiber_abi::FiberConfig>,
) -> Parked {
    let evidence = evidence();
    let mut fiber = match config {
        Some(c) => jit.spawn_deep_with(arg, evidence, c),
        None => jit.spawn_deep(arg, evidence),
    }
    .unwrap();
    let data = fiber.data_mut();
    data[DEEP_ENTRY_SECRET * 8..][..8].copy_from_slice(&ENTRY_SECRET.to_le_bytes());
    data[DEEP_MID_SECRET * 8..][..8].copy_from_slice(&MID_SECRET.to_le_bytes());
    fiber.start().parked().unwrap()
}

#[test]
fn parked_fiber_unwinds_through_every_generated_frame() {
    for opt in [OptLevel::None, OptLevel::Speed] {
        let jit = JitPrototype::new(opt).unwrap();
        let k = park_deep(&jit, 7, None);
        let stack = k.stack_image();
        let frame = k.register_frame();
        // The park entry has no frame: the switch returns straight into the
        // leaf's call site.
        assert_eq!(
            jit.cfi()
                .function_at(frame.return_address - 1)
                .map(|f| f.name.as_str()),
            Some(DEEP_LEAF_SYMBOL),
            "{opt:?}"
        );
        let bt = unwind(&stack, &frame, jit.cfi(), 16).unwrap();
        assert_eq!(
            bt.functions(),
            vec![DEEP_LEAF_SYMBOL, DEEP_MID_SYMBOL, DEEP_ENTRY_SYMBOL],
            "{opt:?}: {bt:#?}"
        );
        // The entry was called by the host's fiber_main, which has no CFI
        // in the table.
        let Stop::NoCfi { pc } = bt.stop else {
            panic!("{opt:?}: {:?}", bt.stop)
        };
        assert!(jit.cfi().function_at(pc - 1).is_none());
        for (f, words) in bt
            .frames
            .iter()
            .zip([DEEP_LEAF_WORDS, DEEP_MID_WORDS, DEEP_ENTRY_WORDS])
        {
            assert_eq!(
                f.sp,
                word(&k, words),
                "{opt:?} {}: stack pointer",
                f.function
            );
            assert_eq!(
                f.get(FP),
                Some(word(&k, words + 1)),
                "{opt:?} {}: frame pointer",
                f.function
            );
            assert!(f.sp < f.cfa && f.cfa <= stack.top);
            // Each frame's pc is a return address inside its own code.
            assert_eq!(
                jit.cfi().function_at(f.pc - 1).map(|c| c.name.as_str()),
                Some(f.function.as_str())
            );
        }
        // A value loaded before a call and used after it must survive the
        // call in a callee-saved register; the unwinder recovers it in the
        // caller's frame from the callee's save slots.
        for (f, secret) in bt.frames[1..].iter().zip([MID_SECRET, ENTRY_SECRET]) {
            assert!(
                f.registers.iter().any(|(_, v)| *v == secret),
                "{opt:?} {}: {:x?}",
                f.function,
                f.registers
            );
        }
        // Frames nest: each caller's stack pointer is the callee's CFA.
        for w in bt.frames.windows(2) {
            assert_eq!(w[0].cfa, w[1].sp);
        }
        // The unwinder also reports the switch frame's vector registers.
        match frame.abi {
            FrameAbi::Aarch64 => assert_eq!(bt.vector.len(), 8),
            FrameAbi::WindowsX64 => assert_eq!(bt.vector.len(), 10),
            FrameAbi::SysVX86_64 => assert!(bt.vector.is_empty()),
        }
        // The fiber still resumes to the right result.
        let id = k.request().evidence;
        let e = k
            .resume(Resumption::value(id, 1000))
            .unwrap()
            .exited()
            .unwrap();
        assert_eq!(e.status, exit::COMPLETE);
        assert_eq!(
            e.result,
            prototype::expected_deep(7, 1000, ENTRY_SECRET, MID_SECRET),
            "{opt:?}"
        );
    }
}

#[test]
fn an_encoded_image_unwinds_like_the_live_fiber_and_limits_hold() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    // Images need a fixed-address stack.
    let config = vesper_fiber_abi::FiberConfig {
        data_bytes: prototype::DEEP_DATA_BYTES,
        ..Default::default()
    }
    .at_free_fixed_base()
    .unwrap();
    let k = park_deep(&jit, 3, Some(config));
    let live = unwind(&k.stack_image(), &k.register_frame(), jit.cfi(), 16).unwrap();
    let image = FiberImage::decode(&k.image().unwrap().encode()).unwrap();
    let decoded = unwind(
        image.stack(),
        &image.register_frame().unwrap(),
        jit.cfi(),
        16,
    )
    .unwrap();
    assert_eq!(live, decoded);

    let limited = unwind(&k.stack_image(), &k.register_frame(), jit.cfi(), 2).unwrap();
    assert_eq!(limited.frames.len(), 2);
    assert_eq!(limited.stop, Stop::FrameLimit);

    // A truncated stack image stops with a typed error, never reads past it.
    let mut short = k.stack_image();
    let keep = short
        .bytes
        .len()
        .min(vesper_fiber_abi::switch::SWITCH_FRAME_BYTES + 8);
    short.bytes.truncate(keep);
    short.top = short.saved_sp + keep as u64;
    match unwind(&short, &k.register_frame(), jit.cfi(), 16) {
        Err(UnwindError::Unreadable { .. }) | Err(UnwindError::BadCfa { .. }) => {}
        other => panic!("{other:?}"),
    }
    // A table without the generated code stops at frame 0.
    let empty = vesper_fiber_abi::unwind::CfiTable::default();
    let none = unwind(&k.stack_image(), &k.register_frame(), &empty, 16).unwrap();
    assert!(none.frames.is_empty());
    assert!(matches!(none.stop, Stop::NoCfi { .. }));
    let id = k.request().evidence;
    let e = k
        .resume(Resumption::value(id, 5))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(
        e.result,
        prototype::expected_deep(3, 5, ENTRY_SECRET, MID_SECRET)
    );
}

#[test]
fn prototype_entries_unwind_to_their_entry_frame() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let k = jit
        .spawn_pulse_read(1, evidence())
        .unwrap()
        .start()
        .parked()
        .unwrap();
    let bt = unwind(&k.stack_image(), &k.register_frame(), jit.cfi(), 16).unwrap();
    assert_eq!(bt.functions(), vec![prototype::PULSE_READ_SYMBOL]);
    assert!(matches!(bt.stop, Stop::NoCfi { .. }));
    drop(k);
}
