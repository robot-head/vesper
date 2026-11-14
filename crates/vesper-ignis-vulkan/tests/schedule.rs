//! Device-independent tests of submission segmentation and ordering, using
//! real planner output translated by the real lifecycle runtime (over the
//! CPU recording backend, which only supplies timeline ids).

use std::collections::BTreeMap;
use vesper_ignis::lifecycle::{
    BatchInputs, FencePoint, Limits, PreparedPlan, QueueBinding, QueueCommand, QueueConfig,
    Runtime, ScratchAction, Submission, SubmitResult, TimelineId,
};
use vesper_ignis::recording::RecordingBackend;
use vesper_ignis::{
    Access, BufferDesc, ByteRange, Layout, PassDesc, QueueKind, RenderGraph, SubresourceRange,
    Sync, TextureDesc,
};
use vesper_ignis_vulkan::schedule::{ScheduleError, schedule};

type Rec = RecordingBackend<(), (), ()>;

/// Copy fills a buffer, direct draws, compute reads both, direct copies out.
fn submission() -> (Submission, Vec<TimelineId>) {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let compute = g.add_queue(QueueKind::Compute);
    let copy = g.add_queue(QueueKind::Copy);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined))
        .unwrap();
    let buf = g
        .add_buffer(BufferDesc {
            name: "b".into(),
            size: 256,
        })
        .unwrap();
    g.add_pass(PassDesc::new("fill", copy).buffer(
        buf,
        ByteRange::WHOLE,
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("draw", direct).texture(
        color,
        SubresourceRange::ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        Layout::RenderTarget,
    ))
    .unwrap();
    g.add_pass(
        PassDesc::new("read", compute)
            .texture(
                color,
                SubresourceRange::ALL,
                Access::SHADER_RESOURCE,
                Sync::COMPUTE_SHADING,
                None,
            )
            .buffer(
                buf,
                ByteRange::WHOLE,
                Access::UNORDERED_ACCESS,
                Sync::COMPUTE_SHADING,
            ),
    )
    .unwrap();
    g.add_pass(PassDesc::new("out", direct).texture(
        color,
        SubresourceRange::ALL,
        Access::COPY_SOURCE,
        Sync::COPY,
        None,
    ))
    .unwrap();
    let kinds = [QueueKind::Direct, QueueKind::Compute, QueueKind::Copy];
    let mut rt = Runtime::new(
        Rec::default(),
        kinds.iter().copied().map(QueueConfig::new).collect(),
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error));
    let timelines: Vec<_> = (0..3).map(|i| rt.timeline(i).unwrap()).collect();
    let batch = BatchInputs {
        plan: PreparedPlan::from_graph(&g).unwrap(),
        bindings: [direct, compute, copy]
            .iter()
            .zip(&timelines)
            .map(|(q, t)| QueueBinding {
                queue: *q,
                timeline: *t,
            })
            .collect(),
        allocators: timelines
            .iter()
            .map(|t| rt.allocator(*t, ()).unwrap())
            .collect(),
        resources: (),
    };
    let recording = rt.record(batch).unwrap();
    let SubmitResult::Submitted(ticket) = rt.submit(recording.close()) else {
        panic!("submit")
    };
    (rt.submission(ticket).unwrap().clone(), timelines)
}

fn zero(timelines: &[TimelineId]) -> BTreeMap<TimelineId, u64> {
    timelines.iter().map(|t| (*t, 0)).collect()
}

#[test]
fn every_wait_follows_its_signal_in_submission_order() {
    let (sub, timelines) = submission();
    let order = schedule(&sub, &zero(&timelines)).unwrap();
    let mut reached = zero(&timelines);
    for seg in &order {
        for w in &seg.waits {
            assert!(
                reached[&w.timeline] >= w.value,
                "wait {w:?} before its signal"
            );
        }
        for s in &seg.signals {
            reached.insert(s.timeline, s.value);
        }
    }
    // Every command except waits/signals lands in exactly one body, and every
    // signal (planner and terminal) is submitted.
    for (i, stream) in sub.streams.iter().enumerate() {
        let body: usize = order
            .iter()
            .filter(|s| s.stream == i)
            .map(|s| s.body.len())
            .sum();
        let expected = stream
            .commands
            .iter()
            .filter(|c| matches!(c, QueueCommand::Barriers(_) | QueueCommand::Execute(_)))
            .count();
        assert_eq!(body, expected);
    }
    for t in &sub.terminal {
        assert_eq!(reached[&t.timeline], t.value);
    }
    // The compute stream waits for both other queues, so it cannot be first.
    assert_ne!(order[0].stream, 1);
}

#[test]
fn a_wait_that_is_never_signalled_stalls_before_submission() {
    let (mut sub, timelines) = submission();
    for stream in &mut sub.streams {
        for c in &mut stream.commands {
            if let QueueCommand::Wait(p) = c {
                p.value += 100;
            }
        }
    }
    assert!(matches!(
        schedule(&sub, &zero(&timelines)),
        Err(ScheduleError::Stalled { .. })
    ));
    // Satisfied by work submitted in an earlier frame.
    let earlier: BTreeMap<_, _> = timelines.iter().map(|t| (*t, 0)).collect();
    let mut ahead = earlier.clone();
    for (t, v) in ahead.iter_mut() {
        if *t != timelines[2] && *t != timelines[0] {
            continue;
        }
        *v = 101;
    }
    let mut shifted = sub.clone();
    for stream in &mut shifted.streams {
        for c in &mut stream.commands {
            if let QueueCommand::Signal(p) = c {
                p.value += 200;
            }
        }
    }
    assert!(schedule(&shifted, &ahead).is_ok());
}

#[test]
fn scratch_unknown_timelines_and_stale_signals_are_typed_errors() {
    let (sub, timelines) = submission();
    let mut scratch = sub.clone();
    scratch.streams[0].commands.insert(
        0,
        QueueCommand::Scratch {
            index: 0,
            action: ScratchAction::Initialize,
            program: [0; 32],
        },
    );
    assert!(matches!(
        schedule(&scratch, &zero(&timelines)),
        Err(ScheduleError::ScratchUnsupported {
            stream: 0,
            command: 0
        })
    ));
    let mut partial = zero(&timelines);
    partial.remove(&timelines[1]);
    assert!(matches!(
        schedule(&sub, &partial),
        Err(ScheduleError::UnknownTimeline(_))
    ));
    let mut stale = zero(&timelines);
    stale.insert(timelines[2], 50);
    assert!(matches!(
        schedule(&sub, &stale),
        Err(ScheduleError::NonMonotonicSignal(FencePoint { .. }))
    ));
}
