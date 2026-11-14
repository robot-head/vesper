//! Byte-image activation checkpoints (§31.1 item 2) on real
//! `vesper-fiber-abi` fibers with fixed stack addresses: restore without
//! re-execution, detect a different entry argument or code, survive a
//! snapshot-epoch round trip, and keep decoding version-1 images.

use vesper_chrono::checkpoint::{CHECKPOINT_MAGIC, CHECKPOINT_VERSION_1};
use vesper_chrono::{ActivationCheckpoint, CheckpointError, CheckpointLimits, ParkPoint};

#[test]
fn version_one_images_still_decode_without_a_byte_image() {
    let c = ActivationCheckpoint {
        fiber_id: 3,
        generation: 2,
        park: ParkPoint {
            evidence_depth: 1,
            op: 2,
            flags: 0,
            arg0: 0,
            arg1: 0,
        },
        data_at_park: vec![1; 8],
        data: vec![2; 8],
        stack: None,
        journal: vec![],
        image: None,
    };
    let v2 = c.encode();
    // A version-1 body is the version-2 body without the image flag byte.
    let mut v1 = v2[..v2.len() - 32 - 1].to_vec();
    v1[8..10].copy_from_slice(&CHECKPOINT_VERSION_1.to_le_bytes());
    let digest = blake3::hash(&v1);
    v1.extend_from_slice(digest.as_bytes());
    assert_eq!(&v1[..8], &CHECKPOINT_MAGIC);
    let limits = CheckpointLimits::default();
    assert_eq!(ActivationCheckpoint::decode(&v1, &limits).unwrap(), c);
    assert_eq!(ActivationCheckpoint::decode(&v2, &limits).unwrap(), c);
    let tight = CheckpointLimits {
        max_image_bytes: 3,
        ..limits
    };
    let with_image = ActivationCheckpoint {
        image: Some(vec![0; 4]),
        ..c
    };
    assert!(matches!(
        ActivationCheckpoint::decode(&with_image.encode(), &tight),
        Err(CheckpointError::TooLarge { what: "image", .. })
    ));
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64")
))]
mod native {
    use super::*;
    use std::rc::Rc;
    use vesper_chrono::checkpoint::{
        RecordedActivation, Step, capture_epoch, checkpoint_from_bundle,
    };
    use vesper_chrono::{
        AddressPolicy, Bundle, BundleKey, BundleLimits, FlightRecorder, RecorderConfig,
        RedactionPolicy,
    };
    use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
    use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
    use vesper_fiber_abi::{FiberConfig, ImageError, Resumption};

    /// Fixed-address tests free a reservation and then map the same range
    /// again; serialize them so another test's mapping cannot take it in
    /// between.
    static FIXED: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        FIXED.lock().unwrap_or_else(|e| e.into_inner())
    }

    const ARG: u64 = 17;
    const INBOX: [u8; 8] = [3, 1, 4, 1, 5, 9, 2, 6];

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

    fn parked(s: Step) -> RecordedActivation {
        match s {
            Step::Parked(a) => a,
            Step::Exited(e) => panic!("exited: {e:?}"),
        }
    }

    fn resume_value(a: RecordedActivation, value: u64) -> Step {
        let ev = a.parked().request().evidence;
        a.resume(Resumption::value(ev, value)).unwrap()
    }

    fn finish_read(mut a: RecordedActivation) -> u64 {
        assert_eq!(a.parked().request().op, prototype::OP_READ);
        a.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&INBOX);
        let e = resume_value(a, 8).exited().unwrap();
        assert!(e.completed());
        e.result
    }

    #[test]
    fn byte_image_restore_needs_no_re_execution() {
        let _serial = serial();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let jit = JitPrototype::new(opt).unwrap();
            let (ev, clock) = evidence();
            let config = fixed();
            let spawn = |arg| jit.spawn_pulse_read_with(arg, ev.clone(), config).unwrap();
            let expected = prototype::expected_pulse_read(ARG, 1001, &INBOX);

            let a = parked(RecordedActivation::start(spawn(ARG)).unwrap());
            let c1 = a.checkpoint().unwrap();
            assert!(c1.image.is_some());
            let a = parked(resume_value(a, 0));
            let c2 = a.checkpoint().unwrap();
            assert!(c2.image.is_some());
            assert_eq!(finish_read(a), expected, "{opt:?} live");
            assert_eq!(unsafe { *clock }, 1);

            // From park 2 and from park 1: same result, and the
            // tail-resumptive Clock handler never runs again.
            let r = c2.restore_image(spawn(ARG)).unwrap();
            assert_eq!(r.stack_displacement, 0);
            assert_eq!(r.activation.journal(), c2.journal.as_slice());
            assert_eq!(r.activation.checkpoint().unwrap().data, c2.data);
            assert_eq!(finish_read(r.activation), expected, "{opt:?} image park 2");
            let r = c1.restore_image(spawn(ARG)).unwrap();
            assert_eq!(finish_read(parked(resume_value(r.activation, 0))), expected);
            assert_eq!(unsafe { *clock }, 1, "{opt:?}: a direct handler re-ran");

            // Re-execution of the same checkpoint does run it again, which
            // is the difference byte images remove.
            let r = c2.restore(spawn(ARG), AddressPolicy::Reexecuted).unwrap();
            assert_eq!(unsafe { *clock }, 2);
            assert_ne!(finish_read(r.activation), expected);
        }
    }

    #[test]
    fn a_different_argument_or_code_is_detected() {
        let _serial = serial();
        let jit = JitPrototype::new(OptLevel::Speed).unwrap();
        // Map everything else first: a JIT module created after the live
        // fiber exits could land in its released range.
        let other = JitPrototype::new(OptLevel::None).unwrap();
        let (ev, _) = evidence();
        let config = fixed();
        let a = parked(
            RecordedActivation::start(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
                .unwrap(),
        );
        let a = parked(resume_value(a, 0));
        let c = a.checkpoint().unwrap();
        let expected = finish_read(a);

        let err = c
            .restore_image(
                jit.spawn_pulse_read_with(ARG + 1, ev.clone(), config)
                    .unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            err,
            CheckpointError::Image(ImageError::ArgumentMismatch {
                captured: ARG,
                template: ARG + 1
            })
        );
        let err = c
            .restore_image(
                other
                    .spawn_pulse_read_with(ARG, ev.clone(), config)
                    .unwrap(),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                CheckpointError::Image(ImageError::EntryMismatch { .. })
            ),
            "{err:?}"
        );
        let mut registry = EvidenceRegistry::new();
        let nested = prototype::nested_evidence(&mut registry).unwrap();
        let err = c
            .restore_image(jit.spawn_nested_with(ARG, nested, config).unwrap())
            .unwrap_err();
        assert!(
            matches!(
                err,
                CheckpointError::Image(ImageError::EntryMismatch { .. })
            ),
            "{err:?}"
        );
        let (other_ev, _) = evidence();
        let err = c
            .restore_image(jit.spawn_pulse_read_with(ARG, other_ev, config).unwrap())
            .unwrap_err();
        assert_eq!(err, CheckpointError::Image(ImageError::EvidenceMismatch));
        // Hold the captured range so that the other base is elsewhere.
        let holder = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
        let elsewhere = fixed();
        drop(holder);
        let err = c
            .restore_image(
                jit.spawn_pulse_read_with(ARG, ev.clone(), elsewhere)
                    .unwrap(),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                CheckpointError::Image(ImageError::AddressMismatch { .. })
            ),
            "{err:?}"
        );

        // A tampered image inside an otherwise well-formed checkpoint.
        let mut bad = c.clone();
        let image = bad.image.as_mut().unwrap();
        let mid = image.len() / 2;
        image[mid] ^= 1;
        let err = bad
            .restore_image(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
            .unwrap_err();
        assert_eq!(err, CheckpointError::Image(ImageError::BadSeal));
        // An image of another park.
        let mut swapped = c.clone();
        swapped.generation += 2;
        let err = swapped
            .restore_image(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
            .unwrap_err();
        assert_eq!(err, CheckpointError::ImageMismatch("generation"));

        // The matching template still restores.
        let r = c
            .restore_image(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
            .unwrap();
        assert_eq!(finish_read(r.activation), expected);

        // A fiber without a fixed address has no image.
        let floating = FiberConfig {
            fixed_stack_base: None,
            ..config
        };
        // (The template is mapped first: the floating stack could otherwise
        // take the released fixed range.)
        let template = jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap();
        let a = parked(
            RecordedActivation::start(
                jit.spawn_pulse_read_with(ARG, ev.clone(), floating)
                    .unwrap(),
            )
            .unwrap(),
        );
        let c = a.checkpoint().unwrap();
        assert_eq!(c.image, None);
        let err = c.restore_image(template).unwrap_err();
        assert_eq!(err, CheckpointError::NoImage);
        drop(a);
    }

    #[test]
    fn image_checkpoints_survive_a_snapshot_epoch_round_trip() {
        let _serial = serial();
        let jit = JitPrototype::new(OptLevel::Speed).unwrap();
        // Large allocations first: a malloc'd mapping made after the live
        // fiber exits could land in its released range.
        let mut recorder = FlightRecorder::new(RecorderConfig {
            snapshot_image_bytes: 1 << 20,
            ..RecorderConfig::small()
        })
        .unwrap();
        let (ev, clock) = evidence();
        let config = fixed();
        let a = parked(
            RecordedActivation::start(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
                .unwrap(),
        );
        let a = parked(resume_value(a, 0));
        let c = a.checkpoint().unwrap();
        let expected = finish_read(a);

        let epoch = capture_epoch(&mut recorder, 5, &c).unwrap();
        let key = BundleKey::new([7; 32]);
        let bytes = Bundle::export(&recorder, &RedactionPolicy::new(), b"", &key).unwrap();
        let bundle = Bundle::open(&bytes, &key, &BundleLimits::default()).unwrap();
        let (_, back) =
            checkpoint_from_bundle(&bundle, epoch, &CheckpointLimits::default()).unwrap();
        assert_eq!(back, c);
        let r = back
            .restore_image(jit.spawn_pulse_read_with(ARG, ev.clone(), config).unwrap())
            .unwrap();
        assert_eq!(finish_read(r.activation), expected);
        assert_eq!(unsafe { *clock }, 1);
    }
}
