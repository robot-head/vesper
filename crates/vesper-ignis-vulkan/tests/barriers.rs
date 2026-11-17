//! Split barriers (`vkCmdSetEvent2`/`vkCmdWaitEvents2`), buffer sub-range
//! barriers, stage filtering by queue capability and exclusive sharing, on a
//! real Vulkan implementation (Mesa lavapipe in CI) with validation required
//! there. Synchronization validation checks that every narrowed or split
//! dependency still covers the hazards the work has.

mod common;

use common::*;
use vesper_ignis::{
    Access, Barrier, BufferDesc, ByteRange, Command, Layout, PassDesc, QueueKind, RenderGraph,
    SubresourceRange, Sync, TextureDesc,
};
use vesper_ignis_vulkan::*;

const W: u32 = 64;
const H: u32 = 64;
const BYTES: u64 = (W * H * 4) as u64;

fn buffer(g: &mut RenderGraph, name: &str, size: u64) -> vesper_ignis::BufferHandle {
    g.add_buffer(BufferDesc {
        name: name.into(),
        size,
    })
    .unwrap()
}

fn split_halves(g: &RenderGraph) -> usize {
    PreparedPlanExt::plan_of(g)
        .queues
        .iter()
        .flat_map(|q| &q.commands)
        .filter_map(|c| match c {
            Command::Barriers(b) => Some(b),
            _ => None,
        })
        .flatten()
        .filter(|b| {
            let (before, after) = match b {
                Barrier::Buffer(b) => (b.sync_before, b.sync_after),
                Barrier::Texture(t) => (t.sync_before, t.sync_after),
            };
            before == Sync::SPLIT || after == Sync::SPLIT
        })
        .count()
}

struct PreparedPlanExt;
impl PreparedPlanExt {
    fn plan_of(g: &RenderGraph) -> vesper_ignis::Plan {
        vesper_ignis::lifecycle::PreparedPlan::from_graph(g)
            .unwrap()
            .plan()
            .clone()
    }
}

/// Draw into `color`, run an unrelated fill, then sample `color` from a
/// compute pass: the planner splits the render-target -> shader-resource
/// transition around the fill.
#[test]
fn split_barriers_become_events_and_validate() {
    let Some(device) = device() else { return };
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined))
        .unwrap();
    let scratch = buffer(&mut g, "scratch", 256);
    let inverted = buffer(&mut g, "inverted", BYTES);
    let draw = g
        .add_pass(PassDesc::new("draw", q).texture(
            color,
            SubresourceRange::ALL,
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
    let fill = g
        .add_pass(PassDesc::new("unrelated", q).buffer(
            scratch,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let invert = g
        .add_pass(
            PassDesc::new("invert", q)
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
    g.add_dependency(draw, fill).unwrap();
    g.add_dependency(fill, invert).unwrap();
    assert!(split_halves(&g) >= 2, "planner emitted no split barrier");

    let vs = compile_wgsl(shaders::TRIANGLE, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::TRIANGLE, Stage::Fragment, "fs_main").unwrap();
    let cs = compile_wgsl(shaders::INVERT, Stage::Compute, "main").unwrap();
    let mut r = FrameResources::new();
    r.bind_texture(
        color,
        device.create_texture(TextureInfo::new(W, H)).unwrap(),
    )
    .unwrap();
    r.bind_buffer(scratch, device.create_buffer(256).unwrap())
        .unwrap();
    r.bind_buffer(inverted, device.create_buffer(BYTES).unwrap())
        .unwrap();
    let tri = r
        .add_pipeline(device.create_graphics_pipeline(&vs, &fs).unwrap())
        .unwrap();
    let inv = r
        .add_pipeline(
            device
                .create_compute_pipeline(
                    &cs,
                    &[BindingKind::SampledTexture, BindingKind::StorageBuffer],
                )
                .unwrap(),
        )
        .unwrap();
    r.set_pass(
        draw,
        PassWork::Draw {
            pipeline: tri,
            target: color,
            clear: Some([0.0, 0.0, 1.0, 1.0]),
            vertices: 3,
        },
    )
    .unwrap();
    r.set_pass(
        fill,
        PassWork::FillBuffer {
            dst: scratch,
            value: 5,
        },
    )
    .unwrap();
    r.set_pass(
        invert,
        PassWork::Dispatch {
            pipeline: inv,
            bindings: vec![Bind::SampledTexture(color), Bind::StorageBuffer(inverted)],
            groups: [W / 8, H / 8, 1],
        },
    )
    .unwrap();
    let rt = runtime(&device, &[QueueKind::Direct]);
    // Two frames: the second reuses the allocator's (host-reset) events.
    let mut rt = rt;
    let mut r = r;
    let mut allocs: Vec<CommandAllocator> = Vec::new();
    for frame in 0..2 {
        let mut b = batch(&rt, &device, &g, &[(q, QueueKind::Direct)], r);
        if let Some(a) = allocs.pop() {
            // Reuse the retired allocator instead of the fresh one.
            let fresh = std::mem::replace(
                &mut b.allocators[0],
                rt.allocator(rt.timeline(0).unwrap(), a).unwrap(),
            );
            fresh.into_inner().destroy();
        }
        let (next, a, back) = run_frame(rt, b);
        rt = next;
        allocs = a;
        r = back;
        let stats = device.last_submit_stats();
        assert!(stats.split_events >= 1, "frame {frame}: {stats:?}");
        assert_eq!(stats.collapsed_splits, 0);
        let out = r.buffer(inverted).unwrap().read();
        let center = ((H / 2 * W + W / 2) * 4) as usize;
        assert_eq!(&out[center..center + 4], &[0, 191, 255, 0]);
        // The next frame plans the same graph: its first barrier on
        // `color` starts from UNDEFINED (contents discarded), which the
        // backend accepts from any tracked layout.
    }
    finish(rt, allocs, r);
    assert_clean(&device);
}

/// Two fills of disjoint halves of one buffer, then a copy of a slice of
/// the first half: every buffer barrier is narrowed to the bytes the work
/// after it touches, and synchronization validation finds no hazard.
#[test]
fn buffer_barriers_are_narrowed_to_sub_ranges() {
    let Some(device) = device() else { return };
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let buf = buffer(&mut g, "buf", 256);
    let out = buffer(&mut g, "out", 256);
    let fill = |g: &mut RenderGraph, name: &str, at: u64| {
        g.add_pass(PassDesc::new(name, q).buffer(
            buf,
            ByteRange::new(at, 128),
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap()
    };
    let low = fill(&mut g, "low", 0);
    let high = fill(&mut g, "high", 128);
    let copy = g
        .add_pass(
            PassDesc::new("copy", q)
                .buffer(buf, ByteRange::new(64, 64), Access::COPY_SOURCE, Sync::COPY)
                .buffer(out, ByteRange::new(0, 64), Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    let mut r = FrameResources::new();
    r.bind_buffer(buf, device.create_buffer(256).unwrap())
        .unwrap();
    r.bind_buffer(out, device.create_buffer(256).unwrap())
        .unwrap();
    r.set_pass(
        low,
        PassWork::FillBufferRange {
            dst: buf,
            offset: 0,
            size: 128,
            value: 0x1111_1111,
        },
    )
    .unwrap();
    r.set_pass(
        high,
        PassWork::FillBufferRange {
            dst: buf,
            offset: 128,
            size: 128,
            value: 0x2222_2222,
        },
    )
    .unwrap();
    r.set_pass(
        copy,
        PassWork::CopyBuffer {
            src: buf,
            src_offset: 64,
            dst: out,
            dst_offset: 0,
            size: 64,
        },
    )
    .unwrap();
    let rt = runtime(&device, &[QueueKind::Direct]);
    let b = batch(&rt, &device, &g, &[(q, QueueKind::Direct)], r);
    let (rt, allocs, r) = run_frame(rt, b);
    let stats = device.last_submit_stats();
    assert!(stats.narrowed_buffer_barriers >= 2, "{stats:?}");
    let bytes = r.buffer(out).unwrap().read();
    assert!(bytes[..64].iter().all(|b| *b == 0x11));
    assert!(bytes[64..].iter().all(|b| *b == 0));
    let all = r.buffer(buf).unwrap().read();
    assert!(all[..128].iter().all(|b| *b == 0x11));
    assert!(all[128..].iter().all(|b| *b == 0x22));
    finish(rt, allocs, r);
    assert_clean(&device);
}

/// A copy queue cannot record events: the planner's split halves on it are
/// collapsed into one barrier at the end half.
#[test]
fn split_barriers_on_a_copy_queue_are_collapsed() {
    let Some(device) = device() else { return };
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Copy);
    let a = buffer(&mut g, "a", 256);
    let other = buffer(&mut g, "other", 256);
    let out = buffer(&mut g, "out", 256);
    let fill = g
        .add_pass(PassDesc::new("fill", q).buffer(
            a,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let unrelated = g
        .add_pass(PassDesc::new("unrelated", q).buffer(
            other,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let copy = g
        .add_pass(
            PassDesc::new("copy", q)
                .buffer(a, ByteRange::WHOLE, Access::COPY_SOURCE, Sync::COPY)
                .buffer(out, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    g.add_dependency(fill, unrelated).unwrap();
    g.add_dependency(unrelated, copy).unwrap();
    assert!(split_halves(&g) >= 2, "planner emitted no split barrier");
    let mut r = FrameResources::new();
    for h in [a, other, out] {
        r.bind_buffer(h, device.create_buffer(256).unwrap())
            .unwrap();
    }
    r.set_pass(
        fill,
        PassWork::FillBuffer {
            dst: a,
            value: 0x0303_0303,
        },
    )
    .unwrap();
    r.set_pass(
        unrelated,
        PassWork::FillBuffer {
            dst: other,
            value: 4,
        },
    )
    .unwrap();
    r.set_pass(
        copy,
        PassWork::CopyBuffer {
            src: a,
            src_offset: 0,
            dst: out,
            dst_offset: 0,
            size: 256,
        },
    )
    .unwrap();
    let rt = runtime(&device, &[QueueKind::Copy]);
    let b = batch(&rt, &device, &g, &[(q, QueueKind::Copy)], r);
    let (rt, allocs, r) = run_frame(rt, b);
    let stats = device.last_submit_stats();
    assert!(stats.collapsed_splits >= 1, "{stats:?}");
    assert_eq!(stats.split_events, 0);
    assert!(r.buffer(out).unwrap().read().iter().all(|b| *b == 3));
    finish(rt, allocs, r);
    assert_clean(&device);
}

/// A compute-queue pass declared with the D3D12 `ALL_SHADING` scope: on a
/// compute queue the graphics stages and accesses are filtered out of the
/// barrier (lavapipe's single family is universal, so the filter applies
/// from the Ignis queue kind).
#[test]
fn compute_queue_barriers_drop_graphics_stages() {
    let Some(device) = device() else { return };
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Compute);
    let buf = buffer(&mut g, "buf", 256);
    let first = g
        .add_pass(PassDesc::new("first", q).buffer(
            buf,
            ByteRange::WHOLE,
            Access::UNORDERED_ACCESS,
            Sync::ALL_SHADING,
        ))
        .unwrap();
    let second = g
        .add_pass(PassDesc::new("second", q).buffer(
            buf,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let mut r = FrameResources::new();
    r.bind_buffer(buf, device.create_buffer(256).unwrap())
        .unwrap();
    let cs = compile_wgsl(shaders::FILL, Stage::Compute, "main").unwrap();
    let fill = r
        .add_pipeline(
            device
                .create_compute_pipeline(&cs, &[BindingKind::StorageBuffer])
                .unwrap(),
        )
        .unwrap();
    r.set_pass(
        first,
        PassWork::Dispatch {
            pipeline: fill,
            bindings: vec![Bind::StorageBuffer(buf)],
            groups: [1, 1, 1],
        },
    )
    .unwrap();
    r.set_pass(
        second,
        PassWork::FillBuffer {
            dst: buf,
            value: 0x0202_0202,
        },
    )
    .unwrap();
    let rt = runtime(&device, &[QueueKind::Compute]);
    let b = batch(&rt, &device, &g, &[(q, QueueKind::Compute)], r);
    let (rt, allocs, r) = run_frame(rt, b);
    let stats = device.last_submit_stats();
    assert!(stats.filtered_barriers >= 1, "{stats:?}");
    let got = r.buffer(buf).unwrap().read();
    assert!(got.iter().all(|b| *b == 2), "{:?}", &got[..16]);
    finish(rt, allocs, r);
    assert_clean(&device);
}

/// Exclusive sharing on three Ignis queues. Lavapipe exposes one queue
/// family, so no ownership transfer is needed (and none is recorded); the
/// transfer planner itself is unit-tested in `ownership`.
#[test]
fn exclusive_sharing_runs_three_queues() {
    let config = DeviceConfig {
        sharing: Sharing::Exclusive,
        ..DeviceConfig::from_env()
    };
    let Some(device) = device_with(config) else {
        return;
    };
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let copy = g.add_queue(QueueKind::Copy);
    let buf = buffer(&mut g, "buf", 256);
    let out = buffer(&mut g, "out", 256);
    let fill = g
        .add_pass(PassDesc::new("fill", copy).buffer(
            buf,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let read = g
        .add_pass(
            PassDesc::new("read", direct)
                .buffer(buf, ByteRange::WHOLE, Access::COPY_SOURCE, Sync::COPY)
                .buffer(out, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    let mut r = FrameResources::new();
    for h in [buf, out] {
        r.bind_buffer(h, device.create_buffer(256).unwrap())
            .unwrap();
    }
    r.set_pass(
        fill,
        PassWork::FillBuffer {
            dst: buf,
            value: 0x0909_0909,
        },
    )
    .unwrap();
    r.set_pass(
        read,
        PassWork::CopyBuffer {
            src: buf,
            src_offset: 0,
            dst: out,
            dst_offset: 0,
            size: 256,
        },
    )
    .unwrap();
    let kinds = [QueueKind::Direct, QueueKind::Copy];
    let rt = runtime(&device, &kinds);
    let b = batch(
        &rt,
        &device,
        &g,
        &[(direct, QueueKind::Direct), (copy, QueueKind::Copy)],
        r,
    );
    let (rt, allocs, r) = run_frame(rt, b);
    let families = device.info().families.len();
    let stats = device.last_submit_stats();
    if families == 1 {
        assert_eq!(stats.ownership_transfers, 0);
    } else {
        assert!(stats.ownership_transfers >= 1, "{stats:?}");
    }
    assert!(r.buffer(out).unwrap().read().iter().all(|b| *b == 9));
    finish(rt, allocs, r);
    assert_clean(&device);
}
