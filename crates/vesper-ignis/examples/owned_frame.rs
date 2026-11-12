//! CPU-only demonstration. No D3D12 calls, shaders, IOCP, or hardware evidence.
use vesper_ignis::lifecycle::{
    BatchInputs, Limits, PreparedPlan, QueueBinding, QueueConfig, Runtime, Shutdown, SubmitResult,
    WorkGraphScratch,
};
use vesper_ignis::recording::RecordingBackend;
use vesper_ignis::{PassDesc, QueueKind, RenderGraph};

fn main() {
    let mut graph = RenderGraph::new();
    let graphics = graph.add_queue(QueueKind::Direct);
    let draw = graph.add_pass(PassDesc::new("draw", graphics)).unwrap();
    let backend = RecordingBackend::<String, Vec<u8>, Vec<u8>>::default();
    let mut runtime = Runtime::new(
        backend,
        vec![QueueConfig::new(QueueKind::Direct)],
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error));
    let timeline = runtime.timeline(0).unwrap();
    let inputs = BatchInputs {
        plan: PreparedPlan::from_graph(&graph).unwrap(),
        bindings: vec![QueueBinding {
            queue: graphics,
            timeline,
        }],
        allocators: vec![
            runtime
                .allocator(timeline, String::from("frame allocator"))
                .unwrap(),
        ],
        resources: vec![42; 16],
    };
    let mut recording = runtime.record(inputs).unwrap();
    recording
        .initialize_scratch(draw, WorkGraphScratch::new([1; 32], vec![0; 128]))
        .unwrap();
    let ticket = match runtime.submit(recording.close()) {
        SubmitResult::Submitted(ticket) => ticket,
        _ => panic!("recording backend rejected frame"),
    };
    // A zero budget returns the runtime holding the live resource owners.
    let Shutdown::Incomplete { runtime, report } = runtime.shutdown(0, 1) else {
        panic!("expected live work");
    };
    assert_eq!(report.pending, vec![ticket]);
    let Shutdown::Complete {
        backend,
        mut report,
    } = runtime.shutdown(8, 1)
    else {
        panic!("model did not complete");
    };
    let frame = report.retired.pop().unwrap();
    assert_eq!(frame.scratch[0].program(), &[1; 32]);
    assert_eq!(frame.resources, vec![42; 16]);
    println!(
        "retired frame {} after {} recorded queue signals; allocator and initialized scratch returned",
        frame.ticket.sequence(),
        backend.history()[0].terminal.len()
    );
}
