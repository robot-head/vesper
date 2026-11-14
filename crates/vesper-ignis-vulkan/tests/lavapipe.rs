//! End-to-end tests on a real Vulkan implementation (Mesa lavapipe in CI).
//!
//! Without a Vulkan loader or a suitable device each test prints why and
//! passes, unless `VESPER_IGNIS_VULKAN_REQUIRE=1` (set by the
//! `ignis-vulkan` workflow), which makes an unavailable device a failure.
//! With the validation layer loaded (synchronization validation on), every
//! test except the deliberate-hazard one asserts that no validation error
//! was reported.

use vesper_ignis::lifecycle::{
    BatchInputs, Limits, PollError, PreparedPlan, QueueBinding, QueueConfig, Runtime, Shutdown,
    SubmitError, SubmitResult,
};
use vesper_ignis::{
    Access, BufferDesc, BufferHandle, ByteRange, Command, Layout, PassDesc, QueueKind, RenderGraph,
    SubresourceRange, Sync, TextureDesc, TextureHandle,
};
use vesper_ignis_vulkan::*;

const W: u32 = 64;
const H: u32 = 64;
const BYTES: u64 = (W * H * 4) as u64;
const CLEAR: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const TRIANGLE_RGBA: [u8; 4] = [255, 64, 0, 255];
const CLEAR_RGBA: [u8; 4] = [0, 0, 255, 255];

fn device() -> Option<Device> {
    match Device::new(&DeviceConfig::from_env()) {
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

fn assert_clean(device: &Device) {
    let errors = device.validation_errors();
    assert!(errors.is_empty(), "validation errors: {errors:#?}");
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    bytes[i..i + 4].try_into().unwrap()
}

struct Pipelines {
    triangle: Pipeline,
    invert: Pipeline,
}

fn pipelines(device: &Device) -> Pipelines {
    let vs = compile_wgsl(shaders::TRIANGLE, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::TRIANGLE, Stage::Fragment, "fs_main").unwrap();
    let cs = compile_wgsl(shaders::INVERT, Stage::Compute, "main").unwrap();
    Pipelines {
        triangle: device.create_graphics_pipeline(&vs, &fs).unwrap(),
        invert: device
            .create_compute_pipeline(
                &cs,
                &[BindingKind::SampledTexture, BindingKind::StorageBuffer],
            )
            .unwrap(),
    }
}

type Rt = Runtime<VulkanBackend>;

fn runtime(device: &Device, kinds: &[QueueKind]) -> Rt {
    let backend = VulkanBackend::new(device, kinds).unwrap();
    Runtime::new(
        backend,
        kinds.iter().copied().map(QueueConfig::new).collect(),
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error))
}

/// Everything one frame of the triangle + compute graph needs.
struct Frame {
    graph: RenderGraph,
    queues: Vec<vesper_ignis::QueueId>,
    color: TextureHandle,
    readback: BufferHandle,
    inverted: BufferHandle,
    passes: Vec<(vesper_ignis::PassId, Work)>,
}

#[derive(Clone, Copy)]
enum Work {
    Draw,
    Invert,
    Readback,
    Fill,
}

/// Single direct queue: draw → compute (sample the color target) → copy the
/// color target to a host-visible buffer.
fn single_queue_frame(initial: Layout) -> Frame {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("color", initial).with_final_layout(Layout::CopySource))
        .unwrap();
    let readback = g
        .add_buffer(BufferDesc {
            name: "readback".into(),
            size: BYTES,
        })
        .unwrap();
    let inverted = g
        .add_buffer(BufferDesc {
            name: "inverted".into(),
            size: BYTES,
        })
        .unwrap();
    let draw = g
        .add_pass(PassDesc::new("draw", direct).texture(
            color,
            SubresourceRange::ALL,
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
    let invert = g
        .add_pass(
            PassDesc::new("invert", direct)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    Layout::ShaderResource,
                )
                .buffer(
                    inverted,
                    ByteRange::WHOLE,
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                ),
        )
        .unwrap();
    let copy = g
        .add_pass(
            PassDesc::new("readback", direct)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::COPY_SOURCE,
                    Sync::COPY,
                    Layout::CopySource,
                )
                .buffer(readback, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    Frame {
        graph: g,
        queues: vec![direct],
        color,
        readback,
        inverted,
        passes: vec![
            (draw, Work::Draw),
            (invert, Work::Invert),
            (copy, Work::Readback),
        ],
    }
}

/// Direct, compute and copy queues: the copy queue fills the output buffer,
/// the direct queue draws, the compute queue samples the color target and
/// overwrites the buffer, and the direct queue copies the color target out.
fn three_queue_frame() -> Frame {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let compute = g.add_queue(QueueKind::Compute);
    let copy = g.add_queue(QueueKind::Copy);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined))
        .unwrap();
    let readback = g
        .add_buffer(BufferDesc {
            name: "readback".into(),
            size: BYTES,
        })
        .unwrap();
    let inverted = g
        .add_buffer(BufferDesc {
            name: "inverted".into(),
            size: BYTES,
        })
        .unwrap();
    let fill = g
        .add_pass(PassDesc::new("fill", copy).buffer(
            inverted,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let draw = g
        .add_pass(PassDesc::new("draw", direct).texture(
            color,
            SubresourceRange::ALL,
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
    let invert = g
        .add_pass(
            PassDesc::new("invert", compute)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    None,
                )
                .buffer(
                    inverted,
                    ByteRange::WHOLE,
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                ),
        )
        .unwrap();
    let out = g
        .add_pass(
            PassDesc::new("readback", direct)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::COPY_SOURCE,
                    Sync::COPY,
                    None,
                )
                .buffer(readback, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    Frame {
        graph: g,
        queues: vec![direct, compute, copy],
        color,
        readback,
        inverted,
        passes: vec![
            (fill, Work::Fill),
            (draw, Work::Draw),
            (invert, Work::Invert),
            (out, Work::Readback),
        ],
    }
}

struct Owned {
    texture: GpuTexture,
    readback: GpuBuffer,
    inverted: GpuBuffer,
    pipelines: Pipelines,
}

fn owned(device: &Device) -> Owned {
    Owned {
        texture: device.create_texture(TextureInfo::new(W, H)).unwrap(),
        readback: device.create_buffer(BYTES).unwrap(),
        inverted: device.create_buffer(BYTES).unwrap(),
        pipelines: pipelines(device),
    }
}

fn resources(frame: &Frame, owned: Owned) -> FrameResources {
    let mut r = FrameResources::new();
    r.bind_texture(frame.color, owned.texture).unwrap();
    r.bind_buffer(frame.readback, owned.readback).unwrap();
    r.bind_buffer(frame.inverted, owned.inverted).unwrap();
    let triangle = r.add_pipeline(owned.pipelines.triangle).unwrap();
    let invert = r.add_pipeline(owned.pipelines.invert).unwrap();
    for (pass, work) in &frame.passes {
        let work = match work {
            Work::Draw => PassWork::Draw {
                pipeline: triangle,
                target: frame.color,
                clear: Some(CLEAR),
                vertices: 3,
            },
            Work::Invert => PassWork::Dispatch {
                pipeline: invert,
                bindings: vec![
                    Bind::SampledTexture(frame.color),
                    Bind::StorageBuffer(frame.inverted),
                ],
                groups: [W / 8, H / 8, 1],
            },
            Work::Readback => PassWork::CopyTextureToBuffer {
                src: frame.color,
                dst: frame.readback,
            },
            Work::Fill => PassWork::FillBuffer {
                dst: frame.inverted,
                value: 0xdead_beef,
            },
        };
        r.set_pass(*pass, work).unwrap();
    }
    r
}

fn unbind(frame: &Frame, r: FrameResources) -> Owned {
    let mut parts = r.into_parts();
    let take_buf = |parts: &mut FrameParts, h| {
        let i = parts.buffers.iter().position(|(b, _)| *b == h).unwrap();
        parts.buffers.remove(i).1
    };
    let readback = take_buf(&mut parts, frame.readback);
    let inverted = take_buf(&mut parts, frame.inverted);
    let texture = parts.textures.pop().unwrap().1;
    let invert = parts.pipelines.pop().unwrap();
    let triangle = parts.pipelines.pop().unwrap();
    Owned {
        texture,
        readback,
        inverted,
        pipelines: Pipelines { triangle, invert },
    }
}

fn inputs(
    rt: &Rt,
    frame: &Frame,
    allocators: Vec<CommandAllocator>,
    resources: FrameResources,
) -> BatchInputs<CommandAllocator, FrameResources> {
    let bindings: Vec<_> = frame
        .queues
        .iter()
        .enumerate()
        .map(|(i, q)| QueueBinding {
            queue: *q,
            timeline: rt.timeline(i).unwrap(),
        })
        .collect();
    let allocators = allocators
        .into_iter()
        .enumerate()
        .map(|(i, a)| rt.allocator(rt.timeline(i).unwrap(), a).unwrap())
        .collect();
    BatchInputs {
        plan: PreparedPlan::from_graph(&frame.graph).unwrap(),
        bindings,
        allocators,
        resources,
    }
}

fn check_pixels(readback: &[u8], inverted: &[u8]) {
    assert_eq!(
        pixel(readback, W / 2, H / 2),
        TRIANGLE_RGBA,
        "triangle center"
    );
    assert_eq!(pixel(readback, 1, 1), CLEAR_RGBA, "top-left clear");
    assert_eq!(pixel(readback, W - 2, 1), CLEAR_RGBA, "top-right clear");
    // Bottom edge inside the triangle's base (y = 0.75 NDC ~ row 56).
    assert_eq!(pixel(readback, W / 2, 54), TRIANGLE_RGBA, "triangle base");
    let covered = readback
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| **p == TRIANGLE_RGBA)
        .count();
    // Area of the triangle: 0.5 * 1.5 * 1.5 / 4 of the viewport = 28.1%.
    let expected = (W * H) as f64 * 0.28125;
    assert!(
        (covered as f64 - expected).abs() < expected * 0.05,
        "covered {covered}, expected about {expected}"
    );
    for (src, dst) in readback
        .as_chunks::<4>()
        .0
        .iter()
        .zip(inverted.as_chunks::<4>().0)
    {
        assert_eq!(*dst, src.map(|c| 255 - c), "compute pass output");
    }
}

/// Submits, drains to completion, and returns the retired owners.
fn run_frame(
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
    assert_eq!(rt.pending(), vec![ticket]);
    // lavapipe executes on worker threads: poll until retirement.
    let start = std::time::Instant::now();
    loop {
        let mut report = rt.poll(64);
        assert!(report.error.is_none(), "{:?}", report.error);
        assert!(report.unprocessed.is_empty());
        if let Some(frame) = report.retired.pop() {
            assert_eq!(frame.ticket, ticket);
            assert!(rt.pending().is_empty());
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

fn finish(rt: Rt) {
    match rt.shutdown(16, 64) {
        Shutdown::Complete { backend, .. } => backend.destroy().map_err(|(_, e)| e).unwrap(),
        Shutdown::Incomplete { report, .. } => panic!("pending at shutdown: {:?}", report.pending),
    }
}

#[test]
fn triangle_and_compute_read_back_on_one_queue() {
    let Some(device) = device() else { return };
    let frame = single_queue_frame(Layout::Undefined);
    let rt = runtime(&device, &[QueueKind::Direct]);
    let alloc = device.create_command_allocator(QueueKind::Direct).unwrap();
    let r = resources(&frame, owned(&device));
    let batch = inputs(&rt, &frame, vec![alloc], r);
    let (rt, allocators, r) = run_frame(rt, batch);
    check_pixels(
        &r.buffer(frame.readback).unwrap().read(),
        &r.buffer(frame.inverted).unwrap().read(),
    );
    assert_eq!(
        r.texture(frame.color).unwrap().layout(0, 0),
        Some(ash::vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
    );
    finish(rt);
    for a in allocators {
        a.destroy();
    }
    r.destroy();
    assert_clean(&device);
}

#[test]
fn three_queues_synchronize_with_timeline_semaphores() {
    let Some(device) = device() else { return };
    let frame = three_queue_frame();
    let plan = PreparedPlan::from_graph(&frame.graph).unwrap();
    let waits = plan
        .plan()
        .queues
        .iter()
        .flat_map(|q| &q.commands)
        .filter(|c| matches!(c, Command::Wait { .. }))
        .count();
    assert!(waits >= 2, "cross-queue waits in plan: {waits}");
    let kinds = [QueueKind::Direct, QueueKind::Compute, QueueKind::Copy];
    let rt = runtime(&device, &kinds);
    let allocators = kinds
        .iter()
        .map(|k| device.create_command_allocator(*k).unwrap())
        .collect();
    let r = resources(&frame, owned(&device));
    let batch = inputs(&rt, &frame, allocators, r);
    let (rt, allocators, r) = run_frame(rt, batch);
    check_pixels(
        &r.buffer(frame.readback).unwrap().read(),
        &r.buffer(frame.inverted).unwrap().read(),
    );
    finish(rt);
    for a in allocators {
        a.destroy();
    }
    r.destroy();
    assert_clean(&device);
}

#[test]
fn frames_reuse_owners_and_layouts_carry_across_frames() {
    let Some(device) = device() else { return };
    let mut rt = runtime(&device, &[QueueKind::Direct]);
    let mut alloc = vec![device.create_command_allocator(QueueKind::Direct).unwrap()];
    let mut owners = owned(&device);
    let mut initial = Layout::Undefined;
    for _ in 0..3 {
        let frame = single_queue_frame(initial);
        let batch = inputs(&rt, &frame, alloc, resources(&frame, owners));
        let (next, allocators, r) = run_frame(rt, batch);
        rt = next;
        alloc = allocators;
        check_pixels(
            &r.buffer(frame.readback).unwrap().read(),
            &r.buffer(frame.inverted).unwrap().read(),
        );
        owners = unbind(&frame, r);
        initial = Layout::CopySource;
    }
    // A graph whose initial layout disagrees with the tracked one is rejected
    // before any work is submitted, and every owner comes back.
    let frame = single_queue_frame(Layout::ShaderResource);
    let batch = inputs(&rt, &frame, alloc, resources(&frame, owners));
    let recording = rt.record(batch).unwrap_or_else(|e| panic!("{:?}", e.error));
    let SubmitResult::Rejected(rejected) = rt.submit(recording.close()) else {
        panic!("wrong initial layout must be rejected");
    };
    assert!(
        matches!(
            rejected.error,
            SubmitError::Backend(VulkanError::LayoutMismatch { .. })
        ),
        "{:?}",
        rejected.error
    );
    let parts = rejected.owner.into_parts();
    for a in parts.inputs.allocators {
        a.into_inner().destroy();
    }
    parts.inputs.resources.destroy();
    finish(rt);
    assert_clean(&device);
}

#[test]
fn subresource_barriers_are_accepted_by_validation() {
    let Some(device) = device() else { return };
    // Mip 0 is rendered and sampled; mip 1 is moved through its own layouts
    // by a pass with no GPU work. The planner emits per-mip barriers.
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined).with_mips(2))
        .unwrap();
    let out = g
        .add_buffer(BufferDesc {
            name: "out".into(),
            size: BYTES,
        })
        .unwrap();
    let draw = g
        .add_pass(PassDesc::new("draw", direct).texture(
            color,
            SubresourceRange::mips(0, 1),
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
    let side = g
        .add_pass(PassDesc::new("mip1", direct).texture(
            color,
            SubresourceRange::mips(1, 1),
            Access::COPY_DEST,
            Sync::COPY,
            Layout::CopyDest,
        ))
        .unwrap();
    let sample = g
        .add_pass(
            PassDesc::new("invert", direct)
                .texture(
                    color,
                    SubresourceRange::mips(0, 1),
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    Layout::ShaderResource,
                )
                .texture(
                    color,
                    SubresourceRange::mips(1, 1),
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    Layout::ShaderResource,
                )
                .buffer(
                    out,
                    ByteRange::WHOLE,
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                ),
        )
        .unwrap();
    let plan = PreparedPlan::from_graph(&g).unwrap();
    let per_mip = plan
        .plan()
        .queues
        .iter()
        .flat_map(|q| &q.commands)
        .filter_map(|c| match c {
            Command::Barriers(b) => Some(b),
            _ => None,
        })
        .flatten()
        .filter(|b| matches!(b, vesper_ignis::Barrier::Texture(t) if t.subresources.mip_count == 1))
        .count();
    assert!(per_mip >= 2, "per-mip barriers: {per_mip}");
    let rt = runtime(&device, &[QueueKind::Direct]);
    let p = pipelines(&device);
    let mut tex_info = TextureInfo::new(W, H);
    tex_info.mip_levels = 2;
    let mut r = FrameResources::new();
    r.bind_texture(color, device.create_texture(tex_info).unwrap())
        .unwrap();
    r.bind_buffer(out, device.create_buffer(BYTES).unwrap())
        .unwrap();
    let triangle = r.add_pipeline(p.triangle).unwrap();
    let invert = r.add_pipeline(p.invert).unwrap();
    r.set_pass(
        draw,
        PassWork::Draw {
            pipeline: triangle,
            target: color,
            clear: Some(CLEAR),
            vertices: 3,
        },
    )
    .unwrap();
    r.set_pass(side, PassWork::Empty).unwrap();
    r.set_pass(
        sample,
        PassWork::Dispatch {
            pipeline: invert,
            bindings: vec![Bind::SampledTexture(color), Bind::StorageBuffer(out)],
            groups: [W / 8, H / 8, 1],
        },
    )
    .unwrap();
    let alloc = device.create_command_allocator(QueueKind::Direct).unwrap();
    let batch = BatchInputs {
        plan,
        bindings: vec![QueueBinding {
            queue: direct,
            timeline: rt.timeline(0).unwrap(),
        }],
        allocators: vec![rt.allocator(rt.timeline(0).unwrap(), alloc).unwrap()],
        resources: r,
    };
    let (rt, allocators, r) = run_frame(rt, batch);
    let inverted = r.buffer(out).unwrap().read();
    assert_eq!(
        &inverted[((H / 2 * W + W / 2) * 4) as usize..][..4],
        &[0, 191, 255, 0],
        "complement of the triangle color"
    );
    let tex = r.texture(color).unwrap();
    assert_eq!(
        tex.layout(1, 0),
        Some(ash::vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
    );
    finish(rt);
    for a in allocators {
        a.destroy();
    }
    r.destroy();
    assert_clean(&device);
}

#[test]
fn missing_declaration_is_caught_by_synchronization_validation() {
    let Some(device) = device() else { return };
    if !device.info().sync_validation {
        eprintln!("skipped: synchronization validation not loaded");
        return;
    }
    // A second fill writes `out` right after the first, but its pass does
    // not declare the buffer, so the planner (correctly, for the declared
    // graph) emits no barrier between them: a transfer write-after-write in
    // one command buffer. The validation layer must report it, which proves
    // the layer is active, so the clean runs above are meaningful.
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let out = g
        .add_buffer(BufferDesc {
            name: "out".into(),
            size: BYTES,
        })
        .unwrap();
    let fill = g
        .add_pass(PassDesc::new("fill", direct).buffer(
            out,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let write = g.add_pass(PassDesc::new("undeclared", direct)).unwrap();
    let rt = runtime(&device, &[QueueKind::Direct]);
    let mut r = FrameResources::new();
    r.bind_buffer(out, device.create_buffer(BYTES).unwrap())
        .unwrap();
    r.set_pass(fill, PassWork::FillBuffer { dst: out, value: 7 })
        .unwrap();
    r.set_pass(write, PassWork::FillBuffer { dst: out, value: 9 })
        .unwrap();
    let alloc = device.create_command_allocator(QueueKind::Direct).unwrap();
    let batch = BatchInputs {
        plan: PreparedPlan::from_graph(&g).unwrap(),
        bindings: vec![QueueBinding {
            queue: direct,
            timeline: rt.timeline(0).unwrap(),
        }],
        allocators: vec![rt.allocator(rt.timeline(0).unwrap(), alloc).unwrap()],
        resources: r,
    };
    let (rt, allocators, r) = run_frame(rt, batch);
    let messages = device.take_validation_messages();
    let hazards = messages
        .iter()
        .filter(|m| m.error && (m.id.contains("HAZARD") || m.text.contains("Hazard")))
        .count();
    assert!(
        hazards > 0,
        "expected a synchronization hazard error for the undeclared write; got {messages:#?}"
    );
    finish(rt);
    for a in allocators {
        a.destroy();
    }
    r.destroy();
    assert_clean(&device);
}

#[test]
fn lost_device_is_a_typed_error_and_pending_owners_stay_held() {
    let Some(device) = device() else { return };
    let frame = single_queue_frame(Layout::Undefined);
    let mut rt = runtime(&device, &[QueueKind::Direct]);
    let alloc = device.create_command_allocator(QueueKind::Direct).unwrap();
    let batch = inputs(&rt, &frame, vec![alloc], resources(&frame, owned(&device)));
    let recording = rt.record(batch).unwrap_or_else(|e| panic!("{:?}", e.error));
    let SubmitResult::Submitted(ticket) = rt.submit(recording.close()) else {
        panic!("submit");
    };
    device.inject_device_lost();
    assert!(device.is_lost());
    let report = rt.poll(16);
    assert!(report.retired.is_empty());
    assert!(
        matches!(
            report.error,
            Some(PollError::Backend(VulkanError::DeviceLost { .. }))
        ),
        "{:?}",
        report.error
    );
    // A later frame is rejected by the backend and its owners come back.
    let alloc = device.create_command_allocator(QueueKind::Direct);
    assert!(
        matches!(alloc, Err(VulkanError::DeviceLost { .. })),
        "{alloc:?}"
    );
    // The pending frame never retires: shutdown keeps its owners.
    let Shutdown::Incomplete { runtime, report } = rt.shutdown(4, 16) else {
        panic!("a lost device must not retire pending work");
    };
    assert_eq!(report.pending, vec![ticket]);
    assert!(
        report
            .error
            .is_some_and(|e| matches!(e, PollError::Backend(VulkanError::DeviceLost { .. })))
    );
    // Dropping a runtime with pending work leaks it by design (owners are
    // never released while their GPU work might still run).
    drop(runtime);
}

#[test]
fn dropped_owner_is_counted_not_released() {
    let Some(device) = device() else { return };
    let before = leaked_owners();
    let buffer = device.create_buffer(16).unwrap();
    drop(buffer);
    assert!(leaked_owners() > before);
    let backend = VulkanBackend::new(&device, &[QueueKind::Direct]).unwrap();
    backend.destroy().map_err(|(_, e)| e).unwrap();
}

#[test]
fn work_on_the_wrong_queue_and_unbound_resources_are_rejected() {
    let Some(device) = device() else { return };
    let mut g = RenderGraph::new();
    let copy = g.add_queue(QueueKind::Copy);
    let buf = g
        .add_buffer(BufferDesc {
            name: "b".into(),
            size: 64,
        })
        .unwrap();
    let pass = g
        .add_pass(PassDesc::new("fill", copy).buffer(
            buf,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let mut rt = runtime(&device, &[QueueKind::Copy]);
    let p = pipelines(&device);
    let attempt = |rt: &mut Rt, r: FrameResources, alloc: CommandAllocator| {
        let batch = BatchInputs {
            plan: PreparedPlan::from_graph(&g).unwrap(),
            bindings: vec![QueueBinding {
                queue: copy,
                timeline: rt.timeline(0).unwrap(),
            }],
            allocators: vec![rt.allocator(rt.timeline(0).unwrap(), alloc).unwrap()],
            resources: r,
        };
        let recording = rt.record(batch).unwrap_or_else(|e| panic!("{:?}", e.error));
        match rt.submit(recording.close()) {
            SubmitResult::Rejected(e) => {
                let parts = e.owner.into_parts();
                let alloc = parts
                    .inputs
                    .allocators
                    .into_iter()
                    .next()
                    .unwrap()
                    .into_inner();
                (e.error, parts.inputs.resources, alloc)
            }
            _ => panic!("expected rejection"),
        }
    };
    // Unbound buffer.
    let mut r = FrameResources::new();
    r.set_pass(pass, PassWork::FillBuffer { dst: buf, value: 0 })
        .unwrap();
    let alloc = device.create_command_allocator(QueueKind::Copy).unwrap();
    let (error, r, alloc) = attempt(&mut rt, r, alloc);
    assert!(matches!(
        error,
        SubmitError::Backend(VulkanError::UnboundResource(_))
    ));
    // A dispatch on a copy queue.
    let mut r = r;
    r.bind_buffer(buf, device.create_buffer(64).unwrap())
        .unwrap();
    let invert = r.add_pipeline(p.invert).unwrap();
    r.set_pass(
        pass,
        PassWork::Dispatch {
            pipeline: invert,
            bindings: vec![],
            groups: [1, 1, 1],
        },
    )
    .unwrap();
    let (error, r, alloc) = attempt(&mut rt, r, alloc);
    assert!(
        matches!(error, SubmitError::Backend(VulkanError::WrongQueue { .. })),
        "{error:?}"
    );
    // No pass work at all.
    let mut parts = r.into_parts();
    let r = FrameResources::new();
    let (error, r, alloc) = attempt(&mut rt, r, alloc);
    assert!(matches!(
        error,
        SubmitError::Backend(VulkanError::MissingPassWork(_))
    ));
    r.destroy();
    parts.pipelines.push(p.triangle);
    parts.destroy();
    alloc.destroy();
    finish(rt);
    assert_clean(&device);
}
