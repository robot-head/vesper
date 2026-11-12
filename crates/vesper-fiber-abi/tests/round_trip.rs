//! Direct park/resume round trips of Cranelift-generated code on the host.
//!
//! Runs wherever direct fibers are implemented (Linux x86_64/AArch64, macOS
//! AArch64); it needs no Pulse scheduler.
#![cfg(vesper_direct_fibers)]

use vesper_fiber_abi::abi::{
    ABI_VERSION, CONTEXT_MAGIC, FiberContext, FiberState, ParkRequest, ResumeSignal, exit,
};
use vesper_fiber_abi::data::POISON;
use vesper_fiber_abi::evidence::EvidenceRegistry;
use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
use vesper_fiber_abi::{ContinuationTable, Outcome, Parked, ResumeError, Resumption};
use vesper_pulse::frames::FrameStack;

const OPTS: [OptLevel; 2] = [OptLevel::None, OptLevel::Speed];

fn word(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn parked(o: Outcome) -> Parked {
    match o {
        Outcome::Parked(k) => k,
        Outcome::Exited(e) => panic!("expected a park, fiber exited: {e:?}"),
    }
}

#[test]
fn live_scalar_aggregate_and_places_survive_two_parks_and_two_moves() {
    for opt in OPTS {
        let jit = JitPrototype::new(opt).unwrap();
        let mut registry = EvidenceRegistry::new();
        let (evidence, clock_calls) = prototype::pulse_read_evidence(&mut registry).unwrap();
        let async_id = evidence.identity(1).unwrap();
        let net_id = evidence.identity(2).unwrap();
        let arg = 17;
        let fiber = jit.spawn_pulse_read(arg, evidence.clone()).unwrap();
        assert_eq!(fiber.state(), FiberState::Created);

        // Park 1: Async::yield_now. The pre-park place write is visible.
        let mut k = parked(fiber.start());
        assert_eq!(
            k.request(),
            ParkRequest {
                evidence: async_id,
                op: prototype::OP_YIELD,
                flags: 0,
                arg0: 0,
                arg1: 0
            }
        );
        assert_eq!(word(k.data(), prototype::COUNTER), 1);
        // The tail-resumptive Clock handler ran inline exactly once.
        assert_eq!(unsafe { *clock_calls }, 1);
        let first_base = k.data_base();
        k.relocate_data();
        assert_ne!(k.data_base(), first_base);

        // Park 2: Net::read_some(len = 8, dest = inbox).
        let mut k = parked(k.resume(Resumption::value(async_id, 0)).unwrap());
        let request = k.request();
        assert_eq!(request.evidence, net_id);
        assert_eq!(request.op, prototype::OP_READ);
        assert_eq!(
            (request.arg0, request.arg1),
            (prototype::READ_LEN, prototype::INBOX as u64)
        );
        assert_eq!(word(k.data(), prototype::COUNTER), 11);
        // Deliver into the mutable inbox place while parked (completion DMA).
        let inbox = [3u8, 1, 4, 1, 5, 9, 2, 6];
        k.data_mut()[prototype::INBOX..prototype::INBOX + 8].copy_from_slice(&inbox);
        // Move the suspended data through vesper-pulse frame storage.
        let second_base = k.data_base();
        let mut frames = FrameStack::new(32).unwrap();
        k.relocate_data_through(&mut frames).unwrap();
        assert_ne!(k.data_base(), second_base);
        assert_eq!(frames.depth(), 0);

        let e = k
            .resume(Resumption::value(net_id, 8))
            .unwrap()
            .exited()
            .unwrap();
        assert_eq!(e.status, exit::COMPLETE, "{opt:?}");
        assert_eq!(e.result, prototype::expected_pulse_read(arg, 1001, &inbox));
        assert_eq!(e.word(0), prototype::PULSE_READ_COUNTER);
        assert_eq!(e.parks, 2);
        assert_eq!(e.data_relocations, 2);
    }
}

#[test]
fn suspended_physical_stack_contains_self_pointers() {
    // saved_sp alone cannot relocate a native stack: frame pointers and other
    // saved words in the suspended stack point back into the same stack.
    for opt in OPTS {
        let jit = JitPrototype::new(opt).unwrap();
        let mut registry = EvidenceRegistry::new();
        let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
        let k = parked(jit.spawn_pulse_read(1, evidence).unwrap().start());
        let ctx = unsafe { &*k.fiber().context_ptr() };
        assert_eq!(ctx.state, FiberState::Parked as u32);
        let stack = ctx.stack_base..ctx.stack_base + ctx.stack_len;
        assert!(stack.contains(&ctx.saved_sp));
        let self_pointers = k.stack_self_pointers();
        assert!(
            !self_pointers.is_empty(),
            "{opt:?}: expected saved frame pointers into the fiber stack"
        );
        for (at, value) in &self_pointers {
            assert!(stack.contains(at) && stack.contains(value));
        }
    }
}

#[test]
fn stale_data_base_across_a_park_observes_relocation() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    // Without relocation the invalid fixture happens to read the right value.
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let async_id = evidence.identity(1).unwrap();
    let mut k = parked(jit.spawn_stale_base(evidence.clone()).unwrap().start());
    k.data_mut()[..8].copy_from_slice(&77u64.to_le_bytes());
    let e = k
        .resume(Resumption::value(async_id, 0))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.result, 77);
    // With relocation the stale base reads the poisoned retired allocation.
    let mut k = parked(jit.spawn_stale_base(evidence).unwrap().start());
    k.data_mut()[..8].copy_from_slice(&77u64.to_le_bytes());
    k.relocate_data();
    let e = k
        .resume(Resumption::value(async_id, 0))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.result, POISON);
    assert_eq!(e.word(0), 77);
}

#[test]
fn error_resumption_takes_the_error_edge_with_live_state() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let (async_id, net_id) = (evidence.identity(1).unwrap(), evidence.identity(2).unwrap());
    let k = parked(jit.spawn_pulse_read(5, evidence).unwrap().start());
    let mut k = parked(k.resume(Resumption::value(async_id, 0)).unwrap());
    k.relocate_data();
    let e = k
        .resume(Resumption::error(net_id, 5))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.status, exit::FAILED);
    assert_eq!(e.result, 5);
    assert_eq!(e.word(1), 5);
    // Progress before the failing operation is preserved in the place.
    assert_eq!(e.word(0), 11);
}

#[test]
fn cancellation_resumes_once_and_runs_cleanup() {
    let jit = JitPrototype::new(OptLevel::None).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let async_id = evidence.identity(1).unwrap();
    let mut k = parked(jit.spawn_pulse_read(5, evidence).unwrap().start());
    k.relocate_data();
    let e = k
        .resume(Resumption::cancelled(async_id))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.status, exit::CANCELLED);
    assert_eq!(e.word(1), prototype::CANCEL_NOTE);
    assert_eq!(e.result, 1);
    assert_eq!(e.parks, 1);
}

#[test]
fn dropping_a_parked_continuation_delivers_cancellation() {
    let jit = JitPrototype::new(OptLevel::None).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, clock_calls) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let before = vesper_fiber_abi::fiber::abandoned_fibers();
    let k = parked(jit.spawn_pulse_read(5, evidence.clone()).unwrap().start());
    drop(k);
    // The cancellation edge exits without parking again: nothing abandoned.
    assert_eq!(vesper_fiber_abi::fiber::abandoned_fibers(), before);
    assert_eq!(unsafe { *clock_calls }, 1);
    // A never-started fiber can be dropped without running.
    drop(jit.spawn_pulse_read(5, evidence.clone()).unwrap());
    assert_eq!(unsafe { *clock_calls }, 1);
}

#[test]
fn wrong_evidence_is_rejected_and_ownership_returned() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let (clock_id, async_id) = (evidence.identity(0).unwrap(), evidence.identity(1).unwrap());
    let k = parked(jit.spawn_pulse_read(5, evidence).unwrap().start());
    let rejected = k.resume(Resumption::value(clock_id, 0)).unwrap_err();
    assert_eq!(
        rejected.error,
        ResumeError::WrongEvidence {
            parked: async_id,
            offered: clock_id
        }
    );
    // The continuation is intact and still resumes exactly once.
    let k = rejected.continuation;
    assert_eq!(word(k.data(), 0), 1);
    let k = parked(k.resume(Resumption::value(async_id, 0)).unwrap());
    assert_eq!(k.request().op, prototype::OP_READ);
}

#[test]
fn repeated_and_stale_raw_resumes_are_rejected() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let (async_id, net_id) = (evidence.identity(1).unwrap(), evidence.identity(2).unwrap());
    let mut table = ContinuationTable::new();
    let first = table.insert(parked(jit.spawn_pulse_read(9, evidence).unwrap().start()));

    // A forged generation for the same fiber is stale.
    let forged = vesper_fiber_abi::RawContinuation {
        generation: first.generation + 2,
        ..first
    };
    assert!(matches!(
        table.resume(forged, Resumption::value(async_id, 0)),
        Err(ResumeError::StaleToken { .. })
    ));
    // A wrong-evidence resume leaves the entry in the table.
    assert!(matches!(
        table.resume(first, Resumption::value(net_id, 0)),
        Err(ResumeError::WrongEvidence { .. })
    ));
    assert_eq!(table.len(), 1);

    let k = parked(table.resume(first, Resumption::value(async_id, 0)).unwrap());
    // The duplicate completion for the same token finds nothing to resume.
    assert_eq!(
        table
            .resume(first, Resumption::value(async_id, 0))
            .unwrap_err(),
        ResumeError::UnknownToken
    );
    let second = table.insert(k);
    assert_eq!(second.fiber_id, first.fiber_id);
    assert!(second.generation > first.generation);
    // Once reparked, the old token is stale rather than resuming the new park.
    assert_eq!(
        table
            .resume(first, Resumption::value(async_id, 0))
            .unwrap_err(),
        ResumeError::StaleToken {
            current: second.generation,
            offered: first.generation
        }
    );
    table.get_mut(second).unwrap().data_mut()[16..24].copy_from_slice(&[1; 8]);
    let e = table
        .resume(second, Resumption::value(net_id, 8))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.result, prototype::expected_pulse_read(9, 1001, &[1; 8]));
    assert!(table.is_empty());
}

#[test]
fn nested_handlers_of_one_effect_have_distinct_identities() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let evidence = prototype::nested_evidence(&mut registry).unwrap();
    let (outer, inner) = prototype::nested_identities(&evidence);
    assert_ne!(outer, inner);
    let k = parked(jit.spawn_nested(4, evidence.clone()).unwrap().start());
    // The innermost installation answers the unqualified operation.
    assert_eq!(k.request().evidence, inner);
    assert_eq!(k.request().arg0, 4);
    // Answering with the outer installation's identity is rejected.
    let k = k
        .resume(Resumption::value(outer, 1))
        .unwrap_err()
        .continuation;
    let mut k = parked(k.resume(Resumption::value(inner, 7)).unwrap());
    assert_eq!(k.request().evidence, outer);
    assert_eq!(word(k.data(), prototype::NESTED_INNER), 7);
    k.relocate_data();
    let e = k
        .resume(Resumption::value(outer, 9))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.status, exit::COMPLETE);
    assert_eq!(e.result, 7009);
    assert_eq!(e.word(prototype::NESTED_OUTER / 8), 9);
    // A second installation of the same shape gets fresh identities.
    let again = prototype::nested_evidence(&mut registry).unwrap();
    let (o2, i2) = prototype::nested_identities(&again);
    assert!(![outer, inner].contains(&o2) && ![outer, inner].contains(&i2));
}

#[test]
fn continuation_handles_move_between_owners() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let (async_id, net_id) = (evidence.identity(1).unwrap(), evidence.identity(2).unwrap());
    let mut queue: Vec<Parked> = (0..4)
        .map(|i| parked(jit.spawn_pulse_read(i, evidence.clone()).unwrap().start()))
        .collect();
    // Resume in reverse order, moving each continuation through a Box.
    let mut second: Vec<Box<Parked>> = vec![];
    while let Some(k) = queue.pop() {
        second.push(Box::new(parked(
            k.resume(Resumption::value(async_id, 0)).unwrap(),
        )));
    }
    let mut results = vec![];
    for mut k in second {
        k.relocate_data();
        let arg = (k.fiber_id(), *k);
        let e = arg
            .1
            .resume(Resumption::value(net_id, 0))
            .unwrap()
            .exited()
            .unwrap();
        results.push(e.result);
    }
    results.sort();
    let mut expected: Vec<u64> = (0..4)
        .map(|i| prototype::expected_pulse_read(i, 0, &[]))
        .collect();
    // Clock answers are 1001..=1004 in start order; arguments 0..4 likewise.
    for (i, e) in expected.iter_mut().enumerate() {
        *e += 1001 + i as u64;
    }
    expected.sort();
    assert_eq!(results, expected);
}

#[test]
fn code_stays_mapped_until_the_last_fiber_is_gone() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let liveness = jit.code_liveness();
    let mut registry = EvidenceRegistry::new();
    let evidence = prototype::nested_evidence(&mut registry).unwrap();
    let (outer, inner) = prototype::nested_identities(&evidence);
    let k = parked(jit.spawn_nested(1, evidence).unwrap().start());
    drop(jit);
    assert!(
        liveness.upgrade().is_some(),
        "parked stack returns into the code"
    );
    let k = parked(k.resume(Resumption::value(inner, 1)).unwrap());
    assert!(liveness.upgrade().is_some());
    let e = k
        .resume(Resumption::value(outer, 2))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.result, 1002);
    assert!(
        liveness.upgrade().is_none(),
        "code freed with the last fiber"
    );
}

#[test]
fn park_outside_a_running_fiber_is_an_abi_violation() {
    // Call a generated entry directly on this thread with a context that is
    // not Running. vesper_fiber_park refuses (returns null) without switching
    // and generated code exits with ABI_VIOLATION.
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let evidence = prototype::nested_evidence(&mut registry).unwrap();
    let mut data = [0u64; 6];
    for state in [FiberState::Created, FiberState::Parked, FiberState::Exited] {
        let mut ctx = Box::new(FiberContext {
            magic: CONTEXT_MAGIC,
            abi_version: ABI_VERSION,
            state: state as u32,
            flags: 0,
            generation: 0,
            fiber_id: 0,
            saved_sp: 0,
            scheduler_sp: 0,
            evidence: evidence.header(),
            data_base: data.as_mut_ptr() as u64,
            data_len: 48,
            park: ParkRequest::default(),
            resume: ResumeSignal::default(),
            entry: 0,
            entry_arg: 0,
            exit_status: 0,
            result: 0,
            stack_base: 0,
            stack_len: 0,
        });
        let status = unsafe { (jit.nested_entry())(&mut *ctx, 0) };
        assert_eq!(status, exit::ABI_VIOLATION);
        assert_eq!(ctx.state, state as u32);
    }
    // A Running context with the wrong version is refused too.
    let mut ctx = Box::new(FiberContext {
        magic: CONTEXT_MAGIC,
        abi_version: ABI_VERSION + 1,
        state: FiberState::Running as u32,
        flags: 0,
        generation: 0,
        fiber_id: 0,
        saved_sp: 0,
        scheduler_sp: 0,
        evidence: evidence.header(),
        data_base: data.as_mut_ptr() as u64,
        data_len: 48,
        park: ParkRequest::default(),
        resume: ResumeSignal::default(),
        entry: 0,
        entry_arg: 0,
        exit_status: 0,
        result: 0,
        stack_base: 0,
        stack_len: 0,
    });
    assert_eq!(
        unsafe { (jit.nested_entry())(&mut *ctx, 0) },
        exit::ABI_VIOLATION
    );
}

#[test]
fn evidence_requirements_are_checked_before_spawning() {
    let jit = JitPrototype::new(OptLevel::None).unwrap();
    let mut registry = EvidenceRegistry::new();
    let nested = prototype::nested_evidence(&mut registry).unwrap();
    assert!(jit.spawn_pulse_read(0, nested).is_err());
    let (pulse, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    assert!(jit.spawn_nested(0, pulse).is_err());
}

#[test]
fn generated_clif_calls_the_park_import() {
    let jit = JitPrototype::new(OptLevel::None).unwrap();
    let text = jit.clif();
    assert!(text.contains("call fn0"), "{text}");
    assert!(text.contains("call_indirect"), "{text}");
    assert!(text.contains("stack_store"), "{text}");
}
