use vesper_ignis::multi_backend::{
    GpuApiTarget, LoweredCommand, NativeBackendBarrier, lower_plan, lower_to_target_barrier,
    mtl4_stage_bits, vk_access_bits, vk_stage_bits,
};
use vesper_ignis::{
    Access, Barrier, BufferBarrier, BufferDesc, ByteRange, Layout, PassDesc, PlanOptions,
    QueueKind, RenderGraph, ResourceRef, SubresourceRange, Sync, TextureDesc,
};

#[test]
fn tri_backend_plan_lowering_preserves_compute_to_pixel_hazard_and_elides_init() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let gbuf = g
        .add_texture(TextureDesc::simple("gbuffer", Layout::Undefined))
        .unwrap();
    let staging = g
        .add_buffer(BufferDesc {
            name: "staging".into(),
            size: 1024,
        })
        .unwrap();

    g.add_pass(
        PassDesc::new("cs_gen", direct)
            .texture(
                gbuf,
                SubresourceRange::ALL,
                Access::UNORDERED_ACCESS,
                Sync::COMPUTE_SHADING,
                Layout::UnorderedAccess,
            )
            .buffer(
                staging,
                ByteRange::WHOLE,
                Access::UNORDERED_ACCESS,
                Sync::COMPUTE_SHADING,
            ),
    )
    .unwrap();
    g.add_pass(
        PassDesc::new("ps_shade", direct)
            .texture(
                gbuf,
                SubresourceRange::ALL,
                Access::SHADER_RESOURCE,
                Sync::PIXEL_SHADING,
                Layout::ShaderResource,
            )
            .buffer(
                staging,
                ByteRange::WHOLE,
                Access::SHADER_RESOURCE,
                Sync::PIXEL_SHADING,
            ),
    )
    .unwrap();

    let plan = g
        .plan_with(PlanOptions {
            split_barriers: false,
        })
        .unwrap();

    let d3d = lower_plan(GpuApiTarget::D3D12Agility1620, &plan);
    let vk = lower_plan(GpuApiTarget::Vulkan14, &plan);
    let mtl = lower_plan(GpuApiTarget::Metal4, &plan);

    // D3D12 & Vulkan emit initial Undefined -> UnorderedAccess texture barrier plus both RAW barriers.
    assert_eq!(d3d.barrier_count(), 3);
    assert_eq!(vk.barrier_count(), 3);
    // Metal 4 elides Undefined -> UnorderedAccess (sync_before=None, access_before=NoAccess).
    assert_eq!(mtl.barrier_count(), 2);

    let mtl_barriers: Vec<NativeBackendBarrier> = mtl.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert!(mtl_barriers.iter().all(|b| *b
        == NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: mtl4_stage_bits(Sync::COMPUTE_SHADING),
            before_stages_mask: mtl4_stage_bits(Sync::PIXEL_SHADING),
        }));
}

#[test]
fn vulkan14_pairs_split_barriers_and_metal4_merges_wait() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("split_tex", Layout::RenderTarget))
        .unwrap();

    // Pass 0 writes color, passes 1 and 2 are unrelated, pass 3 reads color -> triggers split barrier.
    g.add_pass(PassDesc::new("write_rt", direct).texture(
        color,
        SubresourceRange::ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        Layout::RenderTarget,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("work_a", direct)).unwrap();
    g.add_pass(PassDesc::new("work_b", direct)).unwrap();
    g.add_pass(PassDesc::new("read_ps", direct).texture(
        color,
        SubresourceRange::ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        Layout::ShaderResource,
    ))
    .unwrap();

    let plan = g
        .plan_with(PlanOptions {
            split_barriers: true,
        })
        .unwrap();

    let vk = lower_plan(GpuApiTarget::Vulkan14, &plan);
    let vk_barriers: Vec<NativeBackendBarrier> = vk.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(vk_barriers.len(), 2);
    match (&vk_barriers[0], &vk_barriers[1]) {
        (
            NativeBackendBarrier::Vulkan14SplitEventSignal {
                event_id: id_sig,
                resource: res_sig,
                src_stage_mask,
            },
            NativeBackendBarrier::Vulkan14SplitEventWait {
                event_id: id_wait,
                resource: res_wait,
                dst_stage_mask,
                old_layout,
                new_layout,
                ..
            },
        ) => {
            assert_eq!(id_sig, id_wait);
            assert_eq!(*res_sig, ResourceRef::Texture(color));
            assert_eq!(*res_wait, ResourceRef::Texture(color));
            assert_eq!(*src_stage_mask, vk_stage_bits(Sync::RENDER_TARGET));
            assert_eq!(*dst_stage_mask, vk_stage_bits(Sync::PIXEL_SHADING));
            assert_eq!(*old_layout, Some(Layout::RenderTarget));
            assert_eq!(*new_layout, Some(Layout::ShaderResource));
        }
        other => panic!("expected split event signal/wait pair, got {other:?}"),
    }

    let mtl = lower_plan(GpuApiTarget::Metal4, &plan);
    let mtl_barriers: Vec<NativeBackendBarrier> = mtl.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    // Signal half elided on Metal 4; wait half carries the producer RENDER_TARGET -> consumer PIXEL_SHADING stages.
    assert_eq!(
        mtl_barriers,
        vec![NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: mtl4_stage_bits(Sync::RENDER_TARGET),
            before_stages_mask: mtl4_stage_bits(Sync::PIXEL_SHADING),
        }]
    );
}

#[test]
fn buffer_and_bitflag_mappings_are_consistent() {
    let mut g = RenderGraph::new();
    let buf = g
        .add_buffer(BufferDesc {
            name: "uav_buf".into(),
            size: 256,
        })
        .unwrap();
    let barrier = Barrier::Buffer(BufferBarrier {
        resource: buf,
        sync_before: Sync::COPY,
        sync_after: Sync::RAYTRACING,
        access_before: Access::COPY_DEST,
        access_after: Access::SHADER_RESOURCE,
    });

    assert_eq!(
        lower_to_target_barrier(GpuApiTarget::D3D12Agility1620, &barrier, 0),
        Some(NativeBackendBarrier::D3D12(barrier))
    );
    assert_eq!(
        lower_to_target_barrier(GpuApiTarget::Vulkan14, &barrier, 0),
        Some(NativeBackendBarrier::Vulkan14BufferBarrier2 {
            resource: buf,
            src_stage_mask: vk_stage_bits(Sync::COPY),
            dst_stage_mask: vk_stage_bits(Sync::RAYTRACING),
            src_access_mask: vk_access_bits(Access::COPY_DEST),
            dst_access_mask: vk_access_bits(Access::SHADER_RESOURCE),
        })
    );
    assert_eq!(
        lower_to_target_barrier(GpuApiTarget::Metal4, &barrier, 0),
        Some(NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: mtl4_stage_bits(Sync::COPY),
            before_stages_mask: mtl4_stage_bits(Sync::RAYTRACING),
        })
    );
}
