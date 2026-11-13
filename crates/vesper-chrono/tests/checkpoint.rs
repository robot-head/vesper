//! Native activation checkpoints on real `vesper-fiber-abi` fibers running
//! Cranelift-generated code (x86_64 Linux here; AArch64 macOS and Linux in
//! CI): capture at a park, restore by re-execution onto a fresh stack,
//! resume to the same result, reject relocation under `SameAddress`, and
//! store checkpoints as Chrono snapshot epochs.

use vesper_chrono::checkpoint::{CHECKPOINT_MAGIC, capture_epoch, checkpoint_from_bundle};
use vesper_chrono::{ActivationCheckpoint, CheckpointError, CheckpointLimits, ResumeTag};

#[test]
fn image_round_trips_and_rejects_damage() {
    let c = ActivationCheckpoint {
        fiber_id: 9,
        generation: 4,
        park: vesper_chrono::ParkPoint {
            evidence_depth: 2,
            op: 7,
            flags: 0,
            arg0: 8,
            arg1: 16,
        },
        data: vec![1, 2, 3, 4],
        stack: Some(vesper_chrono::StackShape {
            anchor: 0x7000_0000,
            words: vec![(0, 64), (8, -8)],
        }),
        journal: vec![vesper_chrono::JournalEntry {
            park: vesper_chrono::ParkPoint {
                evidence_depth: 1,
                op: 1,
                flags: 0,
                arg0: 0,
                arg1: 0,
            },
            data: vec![9; 4],
            tag: ResumeTag::Value,
            value: 0,
        }],
    };
    let limits = CheckpointLimits::default();
    let bytes = c.encode();
    assert_eq!(&bytes[..8], &CHECKPOINT_MAGIC);
    assert_eq!(ActivationCheckpoint::decode(&bytes, &limits).unwrap(), c);
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 0x40;
        assert!(
            ActivationCheckpoint::decode(&bad, &limits).is_err(),
            "byte {i}"
        );
    }
    for n in 0..bytes.len() {
        assert!(ActivationCheckpoint::decode(&bytes[..n], &limits).is_err());
    }
    let tight = CheckpointLimits {
        max_data_bytes: 3,
        ..limits
    };
    assert!(matches!(
        ActivationCheckpoint::decode(&bytes, &tight),
        Err(CheckpointError::TooLarge { what: "data", .. })
    ));
    assert_eq!(
        ActivationCheckpoint::decode(&[0; 64], &limits),
        Err(CheckpointError::BadMagic)
    );
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod native {
    use super::*;
    use std::rc::Rc;
    use vesper_chrono::checkpoint::{RecordedActivation, Step};
    use vesper_chrono::{
        AddressPolicy, Bundle, BundleKey, BundleLimits, FlightRecorder, RecorderConfig,
        RedactionPolicy, ReplaySession,
    };
    use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
    use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
    use vesper_fiber_abi::{Fiber, Resumption};

    const ARG: u64 = 17;
    const INBOX: [u8; 8] = [3, 1, 4, 1, 5, 9, 2, 6];

    fn evidence() -> Rc<EvidenceVector> {
        let mut registry = EvidenceRegistry::new();
        prototype::pulse_read_evidence(&mut registry).unwrap().0
    }

    fn fiber(jit: &JitPrototype, arg: u64) -> Fiber {
        jit.spawn_pulse_read(arg, evidence()).unwrap()
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

    /// Answer park 2 (the read): deliver the inbox, resume with its length.
    fn finish_read(mut a: RecordedActivation) -> u64 {
        assert_eq!(a.parked().request().op, prototype::OP_READ);
        a.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&INBOX);
        let e = resume_value(a, 8).exited().unwrap();
        assert!(e.completed());
        e.result
    }

    fn opts() -> [OptLevel; 2] {
        [OptLevel::None, OptLevel::Speed]
    }

    #[test]
    fn restore_by_reexecution_resumes_to_the_same_result() {
        for opt in opts() {
            let jit = JitPrototype::new(opt).unwrap();
            let expected = prototype::expected_pulse_read(ARG, 1001, &INBOX);

            // Live run, checkpointing at both parks.
            let a = parked(RecordedActivation::start(fiber(&jit, ARG)).unwrap());
            assert_eq!(a.parked().request().op, prototype::OP_YIELD);
            let c1 = a.checkpoint().unwrap();
            let a = parked(resume_value(a, 0));
            let c2 = a.checkpoint().unwrap();
            assert_eq!(c2.journal.len(), 1);
            assert!(
                c2.stack.is_some(),
                "{opt:?}: no self-pointing stack word to locate the stack by"
            );

            // The original is still live, so a fresh stack is elsewhere:
            // a same-address restore is rejected as a relocation.
            let err = c2
                .restore(fiber(&jit, ARG), AddressPolicy::SameAddress)
                .unwrap_err();
            assert!(matches!(err, CheckpointError::Relocated { .. }), "{err:?}");

            // Re-execution onto a fresh stack, then resume: same result.
            let r = c2
                .restore(fiber(&jit, ARG), AddressPolicy::Reexecuted)
                .unwrap();
            assert_ne!(r.stack_displacement, 0);
            assert_eq!(r.activation.checkpoint().unwrap().data, c2.data);
            assert_eq!(
                finish_read(r.activation),
                expected,
                "{opt:?} restore at park 2"
            );

            // From the first park, replaying both answers.
            let r = c1
                .restore(fiber(&jit, ARG), AddressPolicy::Reexecuted)
                .unwrap();
            assert_eq!(finish_read(parked(resume_value(r.activation, 0))), expected);

            // The live activation finishes to the same result too.
            assert_eq!(finish_read(a), expected, "{opt:?} live");
        }
    }

    #[test]
    fn divergent_or_unsuitable_restores_are_rejected() {
        let jit = JitPrototype::new(OptLevel::Speed).unwrap();
        let a = parked(RecordedActivation::start(fiber(&jit, ARG)).unwrap());
        let a = parked(resume_value(a, 0));
        let c = a.checkpoint().unwrap();

        // The journal names a different park than the code reaches.
        let mut wrong = c.clone();
        wrong.journal[0].park.op ^= 0xff;
        assert!(matches!(
            wrong.restore(fiber(&jit, ARG), AddressPolicy::Reexecuted),
            Err(CheckpointError::Diverged { park_index: 0, .. })
        ));
        // A checkpoint that claims a different data frame.
        let mut wrong = c.clone();
        wrong.data[0] ^= 1;
        assert_eq!(
            wrong
                .restore(fiber(&jit, ARG), AddressPolicy::Reexecuted)
                .unwrap_err(),
            CheckpointError::Mismatch("data frame")
        );
        // A journal longer than the run: the fiber exits first.
        let mut wrong = c.clone();
        for _ in 0..2 {
            let mut e = wrong.journal[0].clone();
            e.park = c.park;
            e.value = 8;
            wrong.journal.push(e);
        }
        assert!(matches!(
            wrong.restore(fiber(&jit, ARG), AddressPolicy::Reexecuted),
            Err(CheckpointError::ExitedEarly { park_index: 2, .. })
        ));
        // An evidence depth the fresh vector does not have.
        let mut wrong = c.clone();
        wrong.journal[0].park.evidence_depth = 9;
        assert!(
            wrong
                .restore(fiber(&jit, ARG), AddressPolicy::Reexecuted)
                .is_err()
        );
        // Limitation: the entry argument lives in registers and non-pointer
        // stack words, which the public API does not expose, so restoring
        // onto a fiber with a different argument is NOT detected; it then
        // resumes to a different result. Callers must supply the same code
        // and argument.
        let r = c
            .restore(fiber(&jit, ARG + 1), AddressPolicy::Reexecuted)
            .unwrap();
        assert_ne!(
            finish_read(r.activation),
            prototype::expected_pulse_read(ARG, 1001, &INBOX)
        );
        // The original is unaffected by every failed restore.
        assert_eq!(
            finish_read(a),
            prototype::expected_pulse_read(ARG, 1001, &INBOX)
        );
    }

    /// Record the run in a flight log, store the park-2 checkpoint as a
    /// snapshot epoch, seal and reopen the bundle, restore from the epoch and
    /// replay the recorded suffix: the result equals the live run.
    #[test]
    fn checkpoint_epoch_plus_recorded_suffix_equals_live_run() {
        const EFFECT_ASYNC: u16 = 1;
        const EFFECT_NET: u16 = 2;
        let jit = JitPrototype::new(OptLevel::Speed).unwrap();
        let mut rec = FlightRecorder::new(RecorderConfig {
            snapshot_image_bytes: 4096,
            ..RecorderConfig::small()
        })
        .unwrap();
        let a = parked(RecordedActivation::start(fiber(&jit, ARG)).unwrap());
        let id = a.parked().fiber_id();
        rec.record_effect(id, 10, EFFECT_ASYNC, 0, &0u64.to_le_bytes())
            .unwrap();
        let mut a = parked(resume_value(a, 0));
        let epoch = capture_epoch(&mut rec, 20, &a.checkpoint().unwrap()).unwrap();
        let mut payload = 8u64.to_le_bytes().to_vec();
        payload.extend_from_slice(&INBOX);
        rec.record_effect(id, 30, EFFECT_NET, 0, &payload).unwrap();
        a.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&INBOX);
        let live = resume_value(a, 8).exited().unwrap().result;

        let key = BundleKey::new([5; 32]);
        let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key).unwrap();
        let session = ReplaySession::open_bundle(&bytes, &key, &BundleLimits::default()).unwrap();
        let (start, c) =
            checkpoint_from_bundle(session.bundle(), epoch, &CheckpointLimits::default()).unwrap();
        let mut r = c
            .restore(fiber(&jit, ARG), AddressPolicy::Reexecuted)
            .unwrap()
            .activation;
        let mut result = None;
        for ev in session.bundle().events().filter(|e| e.lamport_seq > start) {
            assert_eq!(ev.effect_id, EFFECT_NET);
            let value = u64::from_le_bytes(ev.payload[..8].try_into().unwrap());
            r.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&ev.payload[8..]);
            match resume_value(r, value) {
                Step::Exited(e) => {
                    result = Some(e.result);
                    break;
                }
                Step::Parked(next) => r = next,
            }
        }
        assert_eq!(result, Some(live));
        assert_eq!(
            checkpoint_from_bundle(session.bundle(), epoch + 7, &CheckpointLimits::default())
                .unwrap_err(),
            CheckpointError::NoSuchEpoch(epoch + 7)
        );
        let _ = session.close();
    }
}
