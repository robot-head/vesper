use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vesper_ignis::lifecycle::*;
use vesper_ignis::recording::{RecordingBackend, RecordingError, SubmitMode};
use vesper_ignis::{
    Access, BufferDesc, ByteRange, PassDesc, PassId, QueueId, QueueKind, RenderGraph, Sync,
};

#[derive(Debug)]
struct Tracked(Arc<AtomicUsize>);
impl Drop for Tracked {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
fn tracked() -> (Tracked, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    (Tracked(count.clone()), count)
}
type Device = RecordingBackend<Tracked, Tracked, Tracked>;
fn runtime(backend: Device, kinds: &[QueueKind], limits: Limits) -> Runtime<Device> {
    Runtime::new(
        backend,
        kinds.iter().copied().map(QueueConfig::new).collect(),
        limits,
    )
    .unwrap_or_else(|e| panic!("runtime: {:?}", e.error))
}
fn single(kind: QueueKind) -> (RenderGraph, QueueId, PassId) {
    let mut graph = RenderGraph::new();
    let q = graph.add_queue(kind);
    let p = graph.add_pass(PassDesc::new("work", q)).unwrap();
    (graph, q, p)
}
fn two_queues() -> (RenderGraph, Vec<QueueId>) {
    let mut graph = RenderGraph::new();
    let direct = graph.add_queue(QueueKind::Direct);
    let compute = graph.add_queue(QueueKind::Compute);
    let buffer = graph
        .add_buffer(BufferDesc {
            name: "shared".into(),
            size: 64,
        })
        .unwrap();
    graph
        .add_pass(PassDesc::new("produce", direct).buffer(
            buffer,
            ByteRange::WHOLE,
            Access::UNORDERED_ACCESS,
            Sync::COMPUTE_SHADING,
        ))
        .unwrap();
    graph
        .add_pass(PassDesc::new("consume", compute).buffer(
            buffer,
            ByteRange::WHOLE,
            Access::SHADER_RESOURCE,
            Sync::COMPUTE_SHADING,
        ))
        .unwrap();
    (graph, vec![direct, compute])
}
fn record(
    rt: &Runtime<Device>,
    graph: &RenderGraph,
    queues: &[QueueId],
    resource: Tracked,
) -> (
    RecordingBatch<Tracked, Tracked, Tracked>,
    Vec<Arc<AtomicUsize>>,
) {
    let mut allocators = Vec::new();
    let mut counts = Vec::new();
    let mut bindings = Vec::new();
    for (index, queue) in queues.iter().enumerate() {
        let timeline = rt.timeline(index).unwrap();
        let (allocator, count) = tracked();
        counts.push(count);
        allocators.push(rt.allocator(timeline, allocator).unwrap());
        bindings.push(QueueBinding {
            queue: *queue,
            timeline,
        });
    }
    let inputs = BatchInputs {
        plan: PreparedPlan::from_graph(graph).unwrap(),
        bindings,
        allocators,
        resources: resource,
    };
    (rt.record(inputs).unwrap(), counts)
}
fn submitted(
    rt: &mut Runtime<Device>,
    batch: ClosedBatch<Tracked, Tracked, Tracked>,
) -> FrameTicket {
    match rt.submit(batch) {
        SubmitResult::Submitted(ticket) => ticket,
        _ => panic!("submission rejected"),
    }
}
fn stop(rt: Runtime<Device>) -> (Device, DrainReport<Device>) {
    match rt.shutdown(128, 1) {
        Shutdown::Complete { backend, report } => (backend, report),
        Shutdown::Incomplete { report, .. } => panic!("incomplete: {:?}", report.pending),
    }
}

#[test]
fn real_planner_streams_execute_and_hold_every_owner_until_all_original_fences() {
    let (graph, queues) = two_queues();
    let (resource, resource_drops) = tracked();
    let mut rt = runtime(
        Device::default(),
        &[QueueKind::Direct, QueueKind::Compute],
        Limits::default(),
    );
    let (batch, alloc_drops) = record(&rt, &graph, &queues, resource);
    assert!(
        batch
            .plan()
            .queues
            .iter()
            .flat_map(|s| &s.commands)
            .any(|c| matches!(c, vesper_ignis::Command::Wait { .. }))
    );
    let ticket = submitted(&mut rt, batch.close());
    let terminal = rt.submission(ticket).unwrap().terminal.clone();
    assert_eq!(resource_drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        rt.complete_original(Completion::reported(terminal[1]))
            .unwrap_err()
            .error,
        LifecycleError::WaitNotObserved(FencePoint {
            timeline: terminal[0].timeline,
            value: 1
        })
    );
    assert_eq!(rt.pending(), vec![ticket]);
    // Producer completion alone cannot release a frame containing consumer work.
    assert!(
        rt.complete_original(Completion::reported(terminal[0]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(resource_drops.load(Ordering::SeqCst), 0);
    let mut retired = rt
        .complete_original(Completion::reported(terminal[1]))
        .unwrap();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].receipt.ticket(), ticket);
    assert_eq!(retired[0].receipt.points(), terminal);
    assert!(rt.pending().is_empty());
    assert_eq!(
        resource_drops.load(Ordering::SeqCst),
        0,
        "ownership is returned, not dropped"
    );
    retired.clear();
    assert_eq!(resource_drops.load(Ordering::SeqCst), 1);
    assert!(alloc_drops.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    // Backend queued reports for the manually reported signals are stale; an
    // empty runtime can be shut down without polling them/reusing owners.
    let _ = stop(rt);
}

#[test]
fn recorder_follows_cross_queue_dependencies_and_budget_slicing() {
    let (mut graph, queues) = two_queues();
    let buffer = graph
        .add_buffer(BufferDesc {
            name: "tail".into(),
            size: 64,
        })
        .unwrap();
    let last = graph
        .add_pass(PassDesc::new("join", queues[0]).buffer(
            buffer,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let consumer = graph.plan().unwrap().order[1];
    graph.add_dependency(consumer, last).unwrap();
    let run = |events| {
        let mut rt = runtime(
            Device::default(),
            &[QueueKind::Direct, QueueKind::Compute],
            Limits::default(),
        );
        let (resource, _) = tracked();
        let (batch, _) = record(&rt, &graph, &queues, resource);
        let ticket = submitted(&mut rt, batch.close());
        let mut retired = Vec::new();
        for _ in 0..64 {
            let mut report = rt.drain(1, events);
            assert!(report.error.is_none());
            assert!(report.unprocessed.is_empty());
            retired.append(&mut report.retired);
            if report.pending.is_empty() {
                break;
            }
        }
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].ticket, ticket);
        let (backend, _) = stop(rt);
        let submission = &backend.history()[0];
        // Relative queue signal values and actual pass/barrier command order
        // are independent of how many completions are harvested per call.
        submission
            .streams
            .iter()
            .map(|s| {
                s.commands
                    .iter()
                    .map(|c| match c {
                        QueueCommand::Wait(p) => format!("wait:{}:{}", p.timeline.index(), p.value),
                        QueueCommand::Signal(p) => {
                            format!("signal:{}:{}", p.timeline.index(), p.value)
                        }
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(1), run(32));
}

#[test]
fn definite_rejection_returns_closed_allocators_resources_and_uninitialized_scratch() {
    let mut backend = Device::default();
    backend.push_submit_mode(SubmitMode::Reject).unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, pass) = single(QueueKind::Direct);
    let (resource, resource_drops) = tracked();
    let (scratch, scratch_drops) = tracked();
    let (mut batch, alloc_drops) = record(&rt, &graph, &[queue], resource);
    batch
        .initialize_scratch(pass, WorkGraphScratch::new([7; 32], scratch))
        .unwrap();
    let batch = match rt.submit(batch.close()) {
        SubmitResult::Rejected(OwnedError {
            owner,
            error: SubmitError::Backend(RecordingError::Rejected),
        }) => owner,
        _ => panic!("expected definite rejection"),
    };
    assert!(rt.pending().is_empty());
    assert_eq!(resource_drops.load(Ordering::SeqCst), 0);
    assert_eq!(scratch_drops.load(Ordering::SeqCst), 0);
    let parts = batch.into_parts();
    assert!(matches!(parts.scratch[0], ScratchOwner::Uninitialized(_)));
    drop(parts);
    assert_eq!(resource_drops.load(Ordering::SeqCst), 1);
    assert_eq!(scratch_drops.load(Ordering::SeqCst), 1);
    assert_eq!(alloc_drops[0].load(Ordering::SeqCst), 1);
    let _ = stop(rt);
}

#[test]
fn rejected_batch_can_retry_and_fence_reservations_never_wrap_or_reuse() {
    let mut backend = Device::default();
    backend.push_submit_mode(SubmitMode::Reject).unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, _) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    let batch = match rt.submit(batch.close()) {
        SubmitResult::Rejected(e) => e.owner,
        _ => panic!("expected rejection"),
    };
    let first = submitted(&mut rt, batch);
    assert_eq!(
        rt.submission(first).unwrap().terminal[0].value,
        2,
        "rejection consumed reservation 1"
    );
    let mut report = rt.drain(8, 1);
    assert!(report.error.is_none());
    let retired = report.retired.pop().unwrap();
    let inputs = BatchInputs {
        plan: PreparedPlan::from_graph(&graph).unwrap(),
        bindings: vec![QueueBinding {
            queue,
            timeline: rt.timeline(0).unwrap(),
        }],
        allocators: retired.allocators,
        resources: retired.resources,
    };
    let batch = rt.record(inputs).unwrap().close();
    let second = submitted(&mut rt, batch);
    assert_eq!(rt.submission(second).unwrap().terminal[0].value, 3);
    let _ = stop(rt);
}

#[test]
fn overflow_and_queue_binding_failures_return_the_original_inputs() {
    let configs = vec![QueueConfig {
        kind: QueueKind::Direct,
        initial_fence: MAX_FENCE_VALUE,
    }];
    let mut rt = Runtime::new(Device::default(), configs, Limits::default())
        .unwrap_or_else(|e| panic!("{:?}", e.error));
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    let rejected = match rt.submit(batch.close()) {
        SubmitResult::Rejected(e) => {
            assert!(matches!(
                e.error,
                SubmitError::Lifecycle(LifecycleError::FenceOverflow)
            ));
            e.owner
        }
        _ => panic!("overflow accepted"),
    };
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(rejected);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let _ = stop(rt);

    let a = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let mut b = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (resource, drops) = tracked();
    let (batch, _) = record(&a, &graph, &[queue], resource);
    let e = match b.submit(batch.close()) {
        SubmitResult::Rejected(e) => e,
        _ => panic!("foreign batch"),
    };
    assert!(matches!(
        e.error,
        SubmitError::Lifecycle(LifecycleError::ForeignTimeline)
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(e);
    let _ = stop(a);
    let _ = stop(b);
}

#[test]
fn stale_foreign_and_future_completions_return_the_report_and_keep_work() {
    let (graph, queue, _) = single(QueueKind::Direct);
    let mut rt = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let foreign = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    let ticket = submitted(&mut rt, batch.close());
    let point = rt.submission(ticket).unwrap().terminal[0];
    for (bad, expected) in [
        (
            FencePoint {
                timeline: foreign.timeline(0).unwrap(),
                value: 1,
            },
            LifecycleError::ForeignTimeline,
        ),
        (
            FencePoint {
                value: point.value + 1,
                ..point
            },
            LifecycleError::UnknownCompletion,
        ),
        (
            FencePoint { value: 0, ..point },
            LifecycleError::StaleCompletion,
        ),
    ] {
        let e = rt.complete_original(Completion::reported(bad)).unwrap_err();
        assert_eq!(e.owner.point(), bad);
        assert_eq!(e.error, expected);
        assert_eq!(rt.pending(), vec![ticket]);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }
    drop(rt.complete_original(Completion::reported(point)).unwrap());
    let e = rt
        .complete_original(Completion::reported(point))
        .unwrap_err();
    assert_eq!(e.error, LifecycleError::StaleCompletion);
    assert_eq!(e.owner.point(), point);
    let _ = stop(rt);
    let _ = stop(foreign);
}

#[test]
fn uncertain_submission_keeps_owners_blocks_reuse_and_can_finish_only_original_work() {
    let mut backend = Device::default();
    backend
        .push_submit_mode(SubmitMode::AcceptUncertain)
        .unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    let ticket = match rt.submit(batch.close()) {
        SubmitResult::Uncertain { ticket, .. } => ticket,
        _ => panic!("uncertain expected"),
    };
    let (other, _) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], other);
    let batch = match rt.submit(batch.close()) {
        SubmitResult::Rejected(e) => {
            assert!(matches!(
                e.error,
                SubmitError::Lifecycle(LifecycleError::SubmissionUncertain)
            ));
            e.owner
        }
        _ => panic!("must block while uncertain"),
    };
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let Shutdown::Incomplete {
        mut runtime,
        report,
    } = rt.shutdown(0, 1)
    else {
        panic!("must retain work");
    };
    assert_eq!(report.pending, vec![ticket]);
    let mut report = runtime.drain(8, 1);
    assert!(report.error.is_none());
    assert_eq!(report.retired.len(), 1);
    drop(report.retired.pop());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let _ = submitted(&mut runtime, batch);
    let _ = stop(runtime);
}

#[test]
fn stalled_shutdown_returns_runtime_without_reclaiming_allocator_or_resource() {
    let mut backend = Device::default();
    backend
        .push_submit_mode(SubmitMode::StallUncertain)
        .unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, alloc_drops) = record(&rt, &graph, &[queue], resource);
    assert!(matches!(
        rt.submit(batch.close()),
        SubmitResult::Uncertain { .. }
    ));
    let Shutdown::Incomplete { runtime, report } = rt.shutdown(3, 1) else {
        panic!("cannot manufacture completion");
    };
    assert_eq!(report.polls, 3);
    assert_eq!(report.pending.len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(alloc_drops[0].load(Ordering::SeqCst), 0);
    drop(runtime); // Quarantines, rather than releasing GPU-visible owners.
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(alloc_drops[0].load(Ordering::SeqCst), 0);
}

#[test]
fn backend_unwind_after_publication_cannot_release_pending_owners() {
    let mut backend = Device::default();
    backend
        .push_submit_mode(SubmitMode::PanicAfterAcceptance)
        .unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, alloc_drops) = record(&rt, &graph, &[queue], resource);
    let caught =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.submit(batch.close())));
    assert!(caught.is_err());
    assert_eq!(rt.pending().len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // Recording fault accepted the original work; its genuine reported signals
    // can still retire the uncertain flight after unwinding was caught.
    let (_, report) = stop(rt);
    assert_eq!(report.retired.len(), 1);
    assert_eq!(alloc_drops[0].load(Ordering::SeqCst), 0);
    drop(report);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(alloc_drops[0].load(Ordering::SeqCst), 1);
}

#[test]
fn work_graph_scratch_is_exclusive_and_initialization_is_not_ready_until_retirement() {
    let mut rt = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (graph, queue, pass) = single(QueueKind::Direct);
    let (resource, _) = tracked();
    let (scratch, drops) = tracked();
    let (mut batch, _) = record(&rt, &graph, &[queue], resource);
    batch
        .initialize_scratch(pass, WorkGraphScratch::new([3; 32], scratch))
        .unwrap();
    let ticket = submitted(&mut rt, batch.close());
    let commands = &rt.submission(ticket).unwrap().streams[0].commands;
    assert!(matches!(
        commands[0],
        QueueCommand::Scratch {
            action: ScratchAction::Initialize,
            ..
        }
    ));
    assert!(matches!(commands[1], QueueCommand::Execute(p) if p == pass));
    let zero = rt.drain(0, 1);
    assert!(zero.retired.is_empty());
    assert_eq!(zero.pending, vec![ticket]);
    let mut report = rt.drain(8, 1);
    let mut frame = report.retired.pop().unwrap();
    let ready = frame.scratch.pop().unwrap();
    assert_eq!(ready.program(), &[3; 32]);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let inputs = BatchInputs {
        plan: PreparedPlan::from_graph(&graph).unwrap(),
        bindings: vec![QueueBinding {
            queue,
            timeline: rt.timeline(0).unwrap(),
        }],
        allocators: frame.allocators,
        resources: frame.resources,
    };
    let mut batch = rt.record(inputs).unwrap();
    batch.use_scratch(pass, ready).unwrap();
    let ticket = submitted(&mut rt, batch.close());
    assert!(matches!(
        rt.submission(ticket).unwrap().streams[0].commands[0],
        QueueCommand::Scratch {
            action: ScratchAction::Use,
            ..
        }
    ));
    let (_, report) = stop(rt);
    assert_eq!(report.retired.len(), 1);
    drop(report);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn scratch_rejects_foreign_pass_copy_queue_and_foreign_initialized_device_without_losing_owner() {
    let mut rt = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (graph, queue, pass) = single(QueueKind::Direct);
    let (_, _, foreign_pass) = single(QueueKind::Direct);
    let (resource, _) = tracked();
    let (scratch, drops) = tracked();
    let (mut batch, _) = record(&rt, &graph, &[queue], resource);
    let e = batch
        .initialize_scratch(foreign_pass, WorkGraphScratch::new([0; 32], scratch))
        .unwrap_err();
    assert_eq!(e.error, LifecycleError::ForeignPass);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    batch.initialize_scratch(pass, e.owner).unwrap();
    submitted(&mut rt, batch.close());
    let mut report = rt.drain(8, 1);
    let ready = report.retired.pop().unwrap().scratch.pop().unwrap();
    let other = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (resource, _) = tracked();
    let (mut other_batch, _) = record(&other, &graph, &[queue], resource);
    let e = other_batch.use_scratch(pass, ready).unwrap_err();
    assert_eq!(e.error, LifecycleError::ForeignTimeline);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(e);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let _ = stop(rt);
    let _ = stop(other);

    let copy = runtime(Device::default(), &[QueueKind::Copy], Limits::default());
    let (graph, queue, pass) = single(QueueKind::Copy);
    let (resource, _) = tracked();
    let (mut batch, _) = record(&copy, &graph, &[queue], resource);
    let (scratch, _) = tracked();
    assert_eq!(
        batch
            .initialize_scratch(pass, WorkGraphScratch::new([0; 32], scratch))
            .unwrap_err()
            .error,
        LifecycleError::ScratchOnCopyQueue
    );
    let _ = stop(copy);
}

#[test]
fn poll_failure_still_returns_already_retired_work() {
    let mut backend = Device::default();
    backend.push_poll_failure(true).unwrap();
    let mut rt = runtime(backend, &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    submitted(&mut rt, batch.close());
    let report = rt.poll(16);
    assert!(matches!(
        report.error,
        Some(PollError::Backend(RecordingError::PollFailure))
    ));
    assert_eq!(report.retired.len(), 1);
    assert!(report.unprocessed.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(report);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let _ = stop(rt);
}

#[test]
fn budgets_reject_before_submission_and_zero_drain_retains_work() {
    let limits = Limits {
        max_inflight: 1,
        max_poll_events: 1,
        max_drain_polls: 2,
        max_scratch: 0,
        ..Limits::default()
    };
    let mut rt = runtime(Device::default(), &[QueueKind::Direct], limits);
    let (graph, queue, pass) = single(QueueKind::Direct);
    let (resource, _) = tracked();
    let (mut batch, _) = record(&rt, &graph, &[queue], resource);
    let (scratch, _) = tracked();
    assert_eq!(
        batch
            .initialize_scratch(pass, WorkGraphScratch::new([0; 32], scratch))
            .unwrap_err()
            .error,
        LifecycleError::Limit("scratch per frame")
    );
    let first = submitted(&mut rt, batch.close());
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    let held = match rt.submit(batch.close()) {
        SubmitResult::Rejected(e) => {
            assert!(matches!(
                e.error,
                SubmitError::Lifecycle(LifecycleError::Limit("inflight frames"))
            ));
            e.owner
        }
        _ => panic!("limit"),
    };
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(rt.drain(0, 1).pending, vec![first]);
    assert!(matches!(
        rt.poll(2).error,
        Some(PollError::Lifecycle(LifecycleError::Limit("poll events")))
    ));
    assert!(matches!(
        rt.drain(3, 1).error,
        Some(PollError::Lifecycle(LifecycleError::Limit("drain polls")))
    ));
    assert_eq!(rt.pending(), vec![first]);
    let report = rt.drain(2, 1);
    assert_eq!(report.retired.len(), 1);
    submitted(&mut rt, held);
    assert!(matches!(rt.shutdown(2, 1), Shutdown::Complete { .. }));
}

#[derive(Clone, Copy)]
enum PollFault {
    Reorder,
    ExceedBound,
    ValidThenInvalidWithBackendError,
}
struct FaultBackend {
    fault: PollFault,
    submission: Option<Submission>,
}
impl Backend for FaultBackend {
    type Allocator = ();
    type Resources = Tracked;
    type Scratch = ();
    type Error = &'static str;
    fn submit(
        &mut self,
        submission: &Submission,
        _: OwnerViews<'_, (), Tracked, ()>,
    ) -> BackendSubmit<Self::Error> {
        self.submission = Some(submission.clone());
        BackendSubmit::Accepted
    }
    fn poll(&mut self, _: usize) -> PollBatch<Self::Error> {
        let Some(submission) = self.submission.take() else {
            return PollBatch {
                completions: vec![],
                error: None,
            };
        };
        let first = submission.terminal[0];
        let (points, error) = match self.fault {
            PollFault::Reorder => (vec![submission.terminal[1], first], None),
            PollFault::ExceedBound => (vec![first, first], None),
            PollFault::ValidThenInvalidWithBackendError => (
                vec![
                    first,
                    FencePoint {
                        value: first.value + 100,
                        ..first
                    },
                    submission.terminal[1],
                ],
                Some("harvested then failed"),
            ),
        };
        PollBatch {
            completions: points.into_iter().map(Completion::reported).collect(),
            error,
        }
    }
}
fn fault_runtime(
    fault: PollFault,
    graph: &RenderGraph,
    queues: &[QueueId],
    resource: Tracked,
) -> Runtime<FaultBackend> {
    let configs = queues
        .iter()
        .map(|q| QueueConfig::new(graph.queue_kind(*q).unwrap()))
        .collect();
    let mut rt = Runtime::new(
        FaultBackend {
            fault,
            submission: None,
        },
        configs,
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error));
    let bindings = queues
        .iter()
        .enumerate()
        .map(|(i, q)| QueueBinding {
            queue: *q,
            timeline: rt.timeline(i).unwrap(),
        })
        .collect();
    let allocators = (0..queues.len())
        .map(|i| rt.allocator(rt.timeline(i).unwrap(), ()).unwrap())
        .collect();
    let batch = rt
        .record(BatchInputs {
            plan: PreparedPlan::from_graph(graph).unwrap(),
            bindings,
            allocators,
            resources: resource,
        })
        .unwrap()
        .close();
    assert!(matches!(rt.submit(batch), SubmitResult::Submitted(_)));
    rt
}
#[test]
fn reordered_polled_reports_return_all_unprocessed_completions_for_retry() {
    let (graph, queues) = two_queues();
    let (resource, drops) = tracked();
    let mut rt = fault_runtime(PollFault::Reorder, &graph, &queues, resource);
    let report = rt.poll(8);
    assert!(matches!(
        report.error,
        Some(PollError::Lifecycle(LifecycleError::WaitNotObserved(_)))
    ));
    assert_eq!(report.unprocessed.len(), 2);
    assert!(report.retired.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let mut reports = report.unprocessed;
    let producer = reports.pop().unwrap();
    let consumer = reports.pop().unwrap();
    assert!(rt.complete_original(producer).unwrap().is_empty());
    drop(rt.complete_original(consumer).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(matches!(rt.shutdown(0, 1), Shutdown::Complete { .. }));
}
#[test]
fn backend_exceeding_poll_bound_cannot_lose_completion_or_release_work() {
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let mut rt = fault_runtime(PollFault::ExceedBound, &graph, &[queue], resource);
    let mut report = rt.poll(1);
    assert!(matches!(
        report.error,
        Some(PollError::Lifecycle(LifecycleError::Limit(
            "backend exceeded poll bound"
        )))
    ));
    assert_eq!(report.unprocessed.len(), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let first = report.unprocessed.pop().unwrap();
    drop(rt.complete_original(first).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let e = rt
        .complete_original(report.unprocessed.pop().unwrap())
        .unwrap_err();
    assert_eq!(e.error, LifecycleError::StaleCompletion);
    assert!(matches!(rt.shutdown(0, 1), Shutdown::Complete { .. }));
}
#[test]
fn partial_harvest_preserves_backend_error_and_remaining_reports() {
    let (graph, queues) = two_queues();
    let (resource, drops) = tracked();
    let mut rt = fault_runtime(
        PollFault::ValidThenInvalidWithBackendError,
        &graph,
        &queues,
        resource,
    );
    let mut report = rt.poll(8);
    assert!(matches!(
        report.error,
        Some(PollError::Both {
            lifecycle: LifecycleError::UnknownCompletion,
            backend: "harvested then failed"
        })
    ));
    assert_eq!(report.unprocessed.len(), 2);
    assert!(report.retired.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let valid_consumer = report.unprocessed.pop().unwrap();
    drop(rt.complete_original(valid_consumer).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(matches!(rt.shutdown(0, 1), Shutdown::Complete { .. }));
}

#[test]
fn allocator_and_queue_associations_are_checked_before_owners_enter_recording() {
    let rt = runtime(
        Device::default(),
        &[QueueKind::Direct, QueueKind::Compute],
        Limits::default(),
    );
    let foreign = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    for (timeline, allocator_timeline, expected) in [
        (
            rt.timeline(1).unwrap(),
            rt.timeline(1).unwrap(),
            LifecycleError::QueueKindMismatch,
        ),
        (
            foreign.timeline(0).unwrap(),
            rt.timeline(0).unwrap(),
            LifecycleError::ForeignTimeline,
        ),
        (
            rt.timeline(0).unwrap(),
            rt.timeline(1).unwrap(),
            LifecycleError::AllocatorBinding,
        ),
    ] {
        let (resource, drops) = tracked();
        let (allocator, alloc_drops) = tracked();
        let allocator = rt.allocator(allocator_timeline, allocator).unwrap();
        let inputs = BatchInputs {
            plan: PreparedPlan::from_graph(&graph).unwrap(),
            bindings: vec![QueueBinding { queue, timeline }],
            allocators: vec![allocator],
            resources: resource,
        };
        let e = rt.record(inputs).unwrap_err();
        assert_eq!(e.error, expected);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(alloc_drops.load(Ordering::SeqCst), 0);
        drop(e);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(alloc_drops.load(Ordering::SeqCst), 1);
    }
    let _ = stop(rt);
    let _ = stop(foreign);
}

#[test]
fn device_removal_sentinel_is_never_a_completion_or_initial_timeline() {
    let e = match Runtime::new(
        Device::default(),
        vec![QueueConfig {
            kind: QueueKind::Direct,
            initial_fence: u64::MAX,
        }],
        Limits::default(),
    ) {
        Err(e) => e,
        Ok(_) => panic!("device removal sentinel accepted"),
    };
    assert_eq!(e.error, LifecycleError::FenceOverflow);
    let mut rt = runtime(Device::default(), &[QueueKind::Direct], Limits::default());
    let (graph, queue, _) = single(QueueKind::Direct);
    let (resource, drops) = tracked();
    let (batch, _) = record(&rt, &graph, &[queue], resource);
    submitted(&mut rt, batch.close());
    let e = rt
        .complete_original(Completion::reported(FencePoint {
            timeline: rt.timeline(0).unwrap(),
            value: u64::MAX,
        }))
        .unwrap_err();
    assert_eq!(e.error, LifecycleError::UnknownCompletion);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let _ = stop(rt);
}

#[test]
fn partial_multi_queue_publication_cannot_retire_when_unsubmitted_terminal_never_occurs() {
    let mut backend = Device::default();
    backend
        .push_submit_mode(SubmitMode::AcceptFirstQueueUncertain)
        .unwrap();
    let mut rt = runtime(
        backend,
        &[QueueKind::Direct, QueueKind::Compute],
        Limits::default(),
    );
    let (graph, queues) = two_queues();
    let (resource, drops) = tracked();
    let (batch, alloc_drops) = record(&rt, &graph, &queues, resource);
    let ticket = match rt.submit(batch.close()) {
        SubmitResult::Uncertain {
            ticket,
            error: RecordingError::Uncertain,
        } => ticket,
        _ => panic!("partially accepted publication must stay uncertain"),
    };
    let terminals = rt.submission(ticket).unwrap().terminal.clone();
    let Shutdown::Incomplete {
        runtime: rt,
        report,
    } = rt.shutdown(8, 1)
    else {
        panic!("first queue completion cannot certify never-submitted second queue");
    };
    assert!(report.error.is_none());
    assert!(report.retired.is_empty());
    assert_eq!(report.pending, vec![ticket]);
    assert_eq!(report.polls, 8);
    assert_eq!(
        rt.observed(terminals[0].timeline).unwrap(),
        terminals[0].value
    );
    assert_eq!(rt.observed(terminals[1].timeline).unwrap(), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(alloc_drops.iter().all(|d| d.load(Ordering::SeqCst) == 0));
    let (other, _) = tracked();
    let (batch, _) = record(&rt, &graph, &queues, other);
    let mut rt = rt;
    assert!(matches!(
        rt.submit(batch.close()),
        SubmitResult::Rejected(OwnedError {
            error: SubmitError::Lifecycle(LifecycleError::SubmissionUncertain),
            ..
        })
    ));
    drop(rt); // Only an actual device quiescence proof could reclaim this work.
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(alloc_drops.iter().all(|d| d.load(Ordering::SeqCst) == 0));
}
