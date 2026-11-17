//! Helpers shared by the device tests.
#![allow(dead_code)]

use vesper_ignis::lifecycle::{
    BatchInputs, Limits, PreparedPlan, QueueBinding, QueueConfig, Runtime, Shutdown, SubmitResult,
};
use vesper_ignis::{QueueId, QueueKind, RenderGraph};
use vesper_ignis_vulkan::golden::{Image, diff};
use vesper_ignis_vulkan::*;

pub type Rt = Runtime<VulkanBackend>;

pub fn device_with(config: DeviceConfig) -> Option<Device> {
    match Device::new(&config) {
        Ok(d) => {
            eprintln!("device: {:?}", d.info());
            Some(d)
        }
        Err(e) if std::env::var_os("VESPER_IGNIS_VULKAN_REQUIRE").is_some() => {
            panic!("VESPER_IGNIS_VULKAN_REQUIRE is set but: {e}")
        }
        Err(e) => {
            eprintln!("skipped, Vulkan device unavailable: {e}");
            None
        }
    }
}

pub fn device() -> Option<Device> {
    device_with(DeviceConfig::from_env())
}

pub fn assert_clean(device: &Device) {
    let errors = device.validation_errors();
    assert!(errors.is_empty(), "validation errors: {errors:#?}");
}

pub fn runtime(device: &Device, kinds: &[QueueKind]) -> Rt {
    let backend = VulkanBackend::new(device, kinds).unwrap();
    Runtime::new(
        backend,
        kinds.iter().copied().map(QueueConfig::new).collect(),
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error))
}

/// One allocator per queue, bound in order to the runtime's timelines.
pub fn batch(
    rt: &Rt,
    device: &Device,
    graph: &RenderGraph,
    queues: &[(QueueId, QueueKind)],
    resources: FrameResources,
) -> BatchInputs<CommandAllocator, FrameResources> {
    let bindings = queues
        .iter()
        .enumerate()
        .map(|(i, (q, _))| QueueBinding {
            queue: *q,
            timeline: rt.timeline(i).unwrap(),
        })
        .collect();
    let allocators = queues
        .iter()
        .enumerate()
        .map(|(i, (_, k))| {
            rt.allocator(
                rt.timeline(i).unwrap(),
                device.create_command_allocator(*k).unwrap(),
            )
            .unwrap()
        })
        .collect();
    BatchInputs {
        plan: PreparedPlan::from_graph(graph).unwrap(),
        bindings,
        allocators,
        resources,
    }
}

/// Submits, drains to completion, and returns the retired owners.
pub fn run_frame(
    rt: Rt,
    batch: BatchInputs<CommandAllocator, FrameResources>,
) -> (Rt, Vec<CommandAllocator>, FrameResources) {
    let mut rt = rt;
    let recording = rt.record(batch).unwrap_or_else(|e| panic!("{:?}", e.error));
    let ticket = match rt.submit(recording.close()) {
        SubmitResult::Submitted(t) => t,
        SubmitResult::Rejected(e) => panic!("rejected: {:?}", e.error),
        SubmitResult::Uncertain { error, .. } => panic!("uncertain: {error:?}"),
    };
    let start = std::time::Instant::now();
    loop {
        let mut report = rt.poll(64);
        assert!(report.error.is_none(), "{:?}", report.error);
        if let Some(frame) = report.retired.pop() {
            assert_eq!(frame.ticket, ticket);
            let allocators = frame
                .allocators
                .into_iter()
                .map(|a| a.into_inner())
                .collect();
            return (rt, allocators, frame.resources);
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "frame did not retire"
        );
        std::thread::yield_now();
    }
}

/// Shuts the runtime down and releases every owner.
pub fn finish(rt: Rt, allocators: Vec<CommandAllocator>, resources: FrameResources) {
    match rt.shutdown(16, 64) {
        Shutdown::Complete { backend, .. } => backend.destroy().map_err(|(_, e)| e).unwrap(),
        Shutdown::Incomplete { report, .. } => panic!("pending at shutdown: {:?}", report.pending),
    }
    for a in allocators {
        a.destroy();
    }
    resources.destroy();
}

/// Compares `actual` with the committed golden `bytes` (allowing a
/// per-channel difference of 2 on at most 0.5 % of the pixels, for
/// rasterizer rounding differences between Mesa releases). With
/// `VESPER_IGNIS_VULKAN_BLESS=1` the golden is rewritten instead.
pub fn check_golden(name: &str, bytes: &[u8], actual: &Image) {
    if std::env::var_os("VESPER_IGNIS_VULKAN_BLESS").is_some() {
        let path = format!("{}/tests/goldens/{name}.pam", env!("CARGO_MANIFEST_DIR"));
        std::fs::write(&path, actual.to_pam()).unwrap();
        eprintln!("blessed {path}");
        return;
    }
    let expected = Image::from_pam(bytes).unwrap_or_else(|e| panic!("golden {name}: {e}"));
    let d = diff(actual, &expected, 2).expect("golden size");
    eprintln!("golden {name}: {d:?}");
    if d.differing * 200 > d.pixels {
        let path = std::env::temp_dir().join(format!("{name}.actual.pam"));
        let _ = std::fs::write(&path, actual.to_pam());
        panic!("{name} differs from its golden: {d:?} (actual image: {path:?})");
    }
}
