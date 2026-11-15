use vesper_ignis::multi_backend::{
    GpuApiTarget, LoweredCommand, NativeBackendBarrier, lower_plan, lower_to_target_barrier,
    lower_to_target_barrier_for_queue, mtl4_stage_bits, try_vk_access_bits, try_vk_stage_bits,
    vk_access_bits, vk_stage_bits,
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

    let vk_barriers: Vec<NativeBackendBarrier> = vk.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    let resolved_subresources = SubresourceRange {
        first_mip: 0,
        mip_count: 1,
        first_slice: 0,
        slice_count: 1,
        first_plane: 0,
        plane_count: 1,
    };
    assert_eq!(
        vk_barriers,
        vec![
            NativeBackendBarrier::Vulkan14ImageBarrier2 {
                resource: gbuf,
                subresources: resolved_subresources,
                src_stage_mask: 0,
                dst_stage_mask: 1 << 11, // COMPUTE_SHADER
                src_access_mask: 0,
                dst_access_mask: (1 << 5) | (1 << 6), // SHADER_READ | SHADER_WRITE
                old_layout: Layout::Undefined,
                new_layout: Layout::UnorderedAccess,
            },
            NativeBackendBarrier::Vulkan14ImageBarrier2 {
                resource: gbuf,
                subresources: resolved_subresources,
                src_stage_mask: 1 << 11,              // COMPUTE_SHADER
                dst_stage_mask: 1 << 7,               // FRAGMENT_SHADER
                src_access_mask: (1 << 5) | (1 << 6), // SHADER_READ | SHADER_WRITE
                dst_access_mask: 1 << 5,              // SHADER_READ
                old_layout: Layout::UnorderedAccess,
                new_layout: Layout::ShaderResource,
            },
            NativeBackendBarrier::Vulkan14BufferBarrier2 {
                resource: staging,
                src_stage_mask: 1 << 11,              // COMPUTE_SHADER
                dst_stage_mask: 1 << 7,               // FRAGMENT_SHADER
                src_access_mask: (1 << 5) | (1 << 6), // SHADER_READ | SHADER_WRITE
                dst_access_mask: 1 << 5,              // SHADER_READ
            },
        ]
    );

    let mtl_barriers: Vec<NativeBackendBarrier> = mtl.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    // Native MTLStages: MTLStageDispatch (1 << 27) -> MTLStageFragment (1 << 1).
    assert!(mtl_barriers.iter().all(|b| *b
        == NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: 1 << 27,
            before_stages_mask: 1 << 1,
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
            assert_eq!(*src_stage_mask, 1 << 10); // COLOR_ATTACHMENT_OUTPUT
            assert_eq!(*dst_stage_mask, 1 << 7); // FRAGMENT_SHADER
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
    // Signal half elided on Metal 4; wait half carries native MTLStageFragment -> MTLStageFragment.
    assert_eq!(
        mtl_barriers,
        vec![NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: 1 << 1,
            before_stages_mask: 1 << 1,
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
            src_stage_mask: 1 << 12,  // ALL_TRANSFER
            dst_stage_mask: 1 << 21,  // RAY_TRACING_SHADER_KHR
            src_access_mask: 1 << 12, // TRANSFER_WRITE
            dst_access_mask: 1 << 5,  // SHADER_READ
        })
    );
    assert_eq!(
        lower_to_target_barrier(GpuApiTarget::Metal4, &barrier, 0),
        Some(NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: 1 << 28,  // MTLStageBlit
            before_stages_mask: 1 << 27, // MTLStageDispatch
        })
    );
}

/// Finding 1: Verify all Vulkan `VkPipelineStageFlagBits2` constants against
/// independently specified Khronos `vulkan_core.h` bit indices.
#[test]
fn vulkan14_stage_bits_match_khronos_header_constants() {
    let expected: &[(Sync, u64)] = &[
        (Sync::NONE, 0),
        (Sync::ALL, 1 << 16),                         // ALL_COMMANDS
        (Sync::DRAW, 1 << 15),                        // ALL_GRAPHICS
        (Sync::INDEX_INPUT, 1 << 2),                  // VERTEX_INPUT
        (Sync::VERTEX_SHADING, (1 << 3) | (1 << 37)), // VERTEX_SHADER | VERTEX_ATTRIBUTE_INPUT
        (Sync::PIXEL_SHADING, 1 << 7),                // FRAGMENT_SHADER
        (Sync::DEPTH_STENCIL, (1 << 8) | (1 << 9)),   // EARLY | LATE_FRAGMENT_TESTS
        (Sync::RENDER_TARGET, 1 << 10),               // COLOR_ATTACHMENT_OUTPUT
        (Sync::COMPUTE_SHADING, 1 << 11),             // COMPUTE_SHADER
        (Sync::RAYTRACING, 1 << 21),                  // RAY_TRACING_SHADER_KHR
        (Sync::COPY, 1 << 12),                        // ALL_TRANSFER
        (Sync::RESOLVE, 1 << 33),                     // RESOLVE
        (Sync::EXECUTE_INDIRECT, 1 << 1),             // DRAW_INDIRECT
        (
            Sync::ALL_SHADING,
            (1 << 38) | (1 << 37) | (1 << 7) | (1 << 11),
        ),
        (Sync::NON_PIXEL_SHADING, (1 << 38) | (1 << 37) | (1 << 11)),
        (
            Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO,
            1 << 28,
        ),
        (Sync::CLEAR_UNORDERED_ACCESS_VIEW, 1 << 35), // CLEAR
        (Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE, 1 << 25),
        (Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE, 1 << 28),
    ];
    for &(sync, want) in expected {
        assert_eq!(vk_stage_bits(sync), want, "sync {sync}");
        assert_eq!(try_vk_stage_bits(sync), Ok(want), "try sync {sync}");
    }
    let unknown = Sync::from_bits(1 << 29);
    assert_eq!(try_vk_stage_bits(unknown), Err(unknown));
}

/// Finding 2: Verify Metal 4 emits native `MTLStages` bitmasks rather than
/// synthetic category bits 1/2/4, including end-to-end plan lowering.
#[test]
fn metal4_emits_native_mtlstages_bitmasks() {
    const MTL_VERTEX: u32 = 1 << 0;
    const MTL_FRAGMENT: u32 = 1 << 1;
    const MTL_TILE: u32 = 1 << 2;
    const MTL_DISPATCH: u32 = 1 << 27;
    const MTL_BLIT: u32 = 1 << 28;
    const MTL_AS: u32 = 1 << 29;

    assert_eq!(mtl4_stage_bits(Sync::NONE), 0);
    assert_eq!(mtl4_stage_bits(Sync::INDEX_INPUT), MTL_VERTEX);
    assert_eq!(mtl4_stage_bits(Sync::VERTEX_SHADING), MTL_VERTEX);
    assert_eq!(mtl4_stage_bits(Sync::PIXEL_SHADING), MTL_FRAGMENT);
    assert_eq!(mtl4_stage_bits(Sync::DEPTH_STENCIL), MTL_FRAGMENT);
    assert_eq!(mtl4_stage_bits(Sync::RENDER_TARGET), MTL_FRAGMENT);
    assert_eq!(
        mtl4_stage_bits(Sync::DRAW),
        MTL_VERTEX | MTL_FRAGMENT | MTL_TILE
    );
    assert_eq!(mtl4_stage_bits(Sync::COMPUTE_SHADING), MTL_DISPATCH);
    assert_eq!(mtl4_stage_bits(Sync::RAYTRACING), MTL_DISPATCH);
    assert_eq!(mtl4_stage_bits(Sync::COPY), MTL_BLIT);
    assert_eq!(mtl4_stage_bits(Sync::RESOLVE), MTL_FRAGMENT | MTL_BLIT);
    assert_eq!(
        mtl4_stage_bits(Sync::EXECUTE_INDIRECT),
        MTL_VERTEX | MTL_DISPATCH
    );
    assert_eq!(
        mtl4_stage_bits(Sync::CLEAR_UNORDERED_ACCESS_VIEW),
        MTL_DISPATCH | MTL_BLIT
    );
    assert_eq!(
        mtl4_stage_bits(Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE),
        MTL_AS
    );
    assert_eq!(
        mtl4_stage_bits(Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE),
        MTL_AS
    );
    assert_eq!(
        mtl4_stage_bits(Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO),
        MTL_AS
    );
    assert_eq!(
        mtl4_stage_bits(Sync::ALL_SHADING),
        MTL_VERTEX | MTL_FRAGMENT | MTL_TILE | MTL_DISPATCH
    );
    assert_eq!(
        mtl4_stage_bits(Sync::NON_PIXEL_SHADING),
        MTL_VERTEX | MTL_DISPATCH
    );
    assert_eq!(
        mtl4_stage_bits(Sync::ALL),
        MTL_VERTEX | MTL_FRAGMENT | MTL_TILE | MTL_DISPATCH | MTL_BLIT | MTL_AS
    );
}

/// Finding 3: Acceleration-structure build/copy/postbuild stages are separate
/// from `RAY_TRACING_SHADER_BIT_KHR` on Vulkan 1.4 and Metal 4.
#[test]
fn acceleration_structure_build_to_raytrace_separates_stages() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let as_buf = g
        .add_buffer(BufferDesc {
            name: "tlas".into(),
            size: 4096,
        })
        .unwrap();

    g.add_pass(PassDesc::new("build_as", direct).buffer(
        as_buf,
        ByteRange::WHOLE,
        Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE,
        Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("trace_rays", direct).buffer(
        as_buf,
        ByteRange::WHOLE,
        Access::RAYTRACING_ACCELERATION_STRUCTURE_READ,
        Sync::RAYTRACING,
    ))
    .unwrap();

    let plan = g
        .plan_with(PlanOptions {
            split_barriers: false,
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
    assert_eq!(
        vk_barriers,
        vec![NativeBackendBarrier::Vulkan14BufferBarrier2 {
            resource: as_buf,
            src_stage_mask: 1 << 25,  // ACCELERATION_STRUCTURE_BUILD_BIT_KHR
            dst_stage_mask: 1 << 21,  // RAY_TRACING_SHADER_BIT_KHR
            src_access_mask: 1 << 22, // ACCELERATION_STRUCTURE_WRITE_KHR
            dst_access_mask: 1 << 21, // ACCELERATION_STRUCTURE_READ_KHR
        }]
    );

    let mtl = lower_plan(GpuApiTarget::Metal4, &plan);
    let mtl_barriers: Vec<NativeBackendBarrier> = mtl.queues[0]
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        mtl_barriers,
        vec![NativeBackendBarrier::Metal4StageBarrier {
            after_stages_mask: 1 << 29,  // MTLStageAccelerationStructure
            before_stages_mask: 1 << 27, // MTLStageDispatch
        }]
    );
}

/// Finding 4: `VERTEX_SHADING`, `ALL_SHADING`, and `NON_PIXEL_SHADING` include
/// `VERTEX_ATTRIBUTE_INPUT` (`1 << 37`) so `Access::VERTEX_BUFFER` has a valid
/// stage/access pairing.
#[test]
fn vertex_buffer_barrier_includes_vertex_attribute_input_stage() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let vb = g
        .add_buffer(BufferDesc {
            name: "vb".into(),
            size: 1024,
        })
        .unwrap();

    g.add_pass(PassDesc::new("upload_vb", direct).buffer(
        vb,
        ByteRange::WHOLE,
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("draw_vb", direct).buffer(
        vb,
        ByteRange::WHOLE,
        Access::VERTEX_BUFFER,
        Sync::VERTEX_SHADING,
    ))
    .unwrap();

    let plan = g
        .plan_with(PlanOptions {
            split_barriers: false,
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
    assert_eq!(
        vk_barriers,
        vec![NativeBackendBarrier::Vulkan14BufferBarrier2 {
            resource: vb,
            src_stage_mask: 1 << 12,              // ALL_TRANSFER
            dst_stage_mask: (1 << 3) | (1 << 37), // VERTEX_SHADER | VERTEX_ATTRIBUTE_INPUT
            src_access_mask: 1 << 12,             // TRANSFER_WRITE
            dst_access_mask: 1 << 2,              // VERTEX_ATTRIBUTE_READ
        }]
    );
}

/// Finding 5: `Access::STREAM_OUTPUT` and `Access::SHADING_RATE_SOURCE` never
/// drop to empty `out == 0`, pair with compatible Vulkan stages, and
/// `try_vk_access_bits` rejects unmapped raw bits.
#[test]
fn stream_output_and_shading_rate_accesses_map_with_compatible_stages() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let so_buf = g
        .add_buffer(BufferDesc {
            name: "so_buf".into(),
            size: 512,
        })
        .unwrap();
    let vrs_tex = g
        .add_texture(TextureDesc::simple("vrs", Layout::CopyDest))
        .unwrap();

    g.add_pass(PassDesc::new("stream_out", direct).buffer(
        so_buf,
        ByteRange::WHOLE,
        Access::STREAM_OUTPUT,
        Sync::VERTEX_SHADING,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("readback_so", direct).buffer(
        so_buf,
        ByteRange::WHOLE,
        Access::COPY_SOURCE,
        Sync::COPY,
    ))
    .unwrap();

    g.add_pass(PassDesc::new("upload_vrs", direct).texture(
        vrs_tex,
        SubresourceRange::ALL,
        Access::COPY_DEST,
        Sync::COPY,
        Layout::CopyDest,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("use_vrs", direct).texture(
        vrs_tex,
        SubresourceRange::ALL,
        Access::SHADING_RATE_SOURCE,
        Sync::PIXEL_SHADING,
        Layout::ShadingRateSource,
    ))
    .unwrap();

    let plan = g
        .plan_with(PlanOptions {
            split_barriers: false,
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

    // Find the stream-output -> copy-source buffer barrier.
    assert!(
        vk_barriers.contains(&NativeBackendBarrier::Vulkan14BufferBarrier2 {
            resource: so_buf,
            src_stage_mask: (1 << 3) | (1 << 37) | (1 << 24), // VERTEX_SHADER | VERTEX_ATTRIBUTE_INPUT | TRANSFORM_FEEDBACK_EXT
            dst_stage_mask: 1 << 12,                          // ALL_TRANSFER
            src_access_mask: 1 << 25,                         // TRANSFORM_FEEDBACK_WRITE_EXT
            dst_access_mask: 1 << 11,                         // TRANSFER_READ
        })
    );

    // Find the copy-dest -> shading-rate-source texture barrier.
    assert!(
        vk_barriers.contains(&NativeBackendBarrier::Vulkan14ImageBarrier2 {
            resource: vrs_tex,
            subresources: SubresourceRange {
                first_mip: 0,
                mip_count: 1,
                first_slice: 0,
                slice_count: 1,
                first_plane: 0,
                plane_count: 1,
            },
            src_stage_mask: 1 << 12,              // ALL_TRANSFER
            dst_stage_mask: (1 << 7) | (1 << 22), // FRAGMENT_SHADER | FRAGMENT_SHADING_RATE_ATTACHMENT_KHR
            src_access_mask: 1 << 12,             // TRANSFER_WRITE
            dst_access_mask: 1 << 23,             // FRAGMENT_SHADING_RATE_ATTACHMENT_READ_KHR
            old_layout: Layout::CopyDest,
            new_layout: Layout::ShadingRateSource,
        })
    );

    // Verify all modeled Access constants map cleanly and unknown bits are caught.
    for acc in [
        Access::COMMON,
        Access::NO_ACCESS,
        Access::VERTEX_BUFFER,
        Access::CONSTANT_BUFFER,
        Access::INDEX_BUFFER,
        Access::RENDER_TARGET,
        Access::UNORDERED_ACCESS,
        Access::DEPTH_STENCIL_WRITE,
        Access::DEPTH_STENCIL_READ,
        Access::SHADER_RESOURCE,
        Access::STREAM_OUTPUT,
        Access::INDIRECT_ARGUMENT,
        Access::COPY_DEST,
        Access::COPY_SOURCE,
        Access::RESOLVE_DEST,
        Access::RESOLVE_SOURCE,
        Access::RAYTRACING_ACCELERATION_STRUCTURE_READ,
        Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE,
        Access::SHADING_RATE_SOURCE,
    ] {
        let mapped = try_vk_access_bits(acc).expect("modeled access must map");
        if acc != Access::NO_ACCESS {
            assert_ne!(mapped, 0, "access {acc} must not silently become 0");
        }
        assert_eq!(vk_access_bits(acc), mapped);
    }
    let unknown = Access::from_bits(1 << 20);
    assert_eq!(try_vk_access_bits(unknown), Err(unknown));
}

/// Finding 6: Split barriers on `QueueKind::Copy` (transfer-only Vulkan queue)
/// collapse into ordinary synchronization2 barriers rather than emitting
/// forbidden `vkCmdSetEvent2` / `vkCmdWaitEvents2` commands.
#[test]
fn vulkan14_collapses_split_barriers_on_copy_queue_into_ordinary_barriers() {
    let mut g = RenderGraph::new();
    let copy_q = g.add_queue(QueueKind::Copy);
    let direct_q = g.add_queue(QueueKind::Direct);
    let copy_buf = g
        .add_buffer(BufferDesc {
            name: "copy_buf".into(),
            size: 1024,
        })
        .unwrap();
    let other_copy_buf = g
        .add_buffer(BufferDesc {
            name: "other_copy_buf".into(),
            size: 1024,
        })
        .unwrap();
    let gfx_buf = g
        .add_buffer(BufferDesc {
            name: "gfx_buf".into(),
            size: 1024,
        })
        .unwrap();

    // Copy queue: write -> unrelated copy -> read triggers split barrier in Plan.
    g.add_pass(PassDesc::new("copy_write", copy_q).buffer(
        copy_buf,
        ByteRange::WHOLE,
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("copy_unrelated", copy_q).buffer(
        other_copy_buf,
        ByteRange::WHOLE,
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("copy_read", copy_q).buffer(
        copy_buf,
        ByteRange::WHOLE,
        Access::COPY_SOURCE,
        Sync::COPY,
    ))
    .unwrap();

    // Direct queue: write -> unrelated -> read keeps split event signal/wait.
    g.add_pass(PassDesc::new("gfx_write", direct_q).buffer(
        gfx_buf,
        ByteRange::WHOLE,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("gfx_unrelated", direct_q))
        .unwrap();
    g.add_pass(PassDesc::new("gfx_read", direct_q).buffer(
        gfx_buf,
        ByteRange::WHOLE,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
    ))
    .unwrap();

    let plan = g.plan().unwrap();
    assert!(
        plan.queues[0].commands.iter().any(|c| matches!(
            c,
            vesper_ignis::Command::Barriers(bs)
                if bs.iter().any(|b| matches!(b, Barrier::Buffer(bb) if bb.sync_after.contains(Sync::SPLIT)))
        )),
        "planner emits split barrier on copy queue by default"
    );

    let vk = lower_plan(GpuApiTarget::Vulkan14, &plan);
    let copy_stream = vk
        .queues
        .iter()
        .find(|q| q.kind == QueueKind::Copy)
        .unwrap();
    let copy_barriers: Vec<NativeBackendBarrier> = copy_stream
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        copy_barriers,
        vec![NativeBackendBarrier::Vulkan14BufferBarrier2 {
            resource: copy_buf,
            src_stage_mask: 1 << 12,  // ALL_TRANSFER
            dst_stage_mask: 1 << 12,  // ALL_TRANSFER
            src_access_mask: 1 << 12, // TRANSFER_WRITE
            dst_access_mask: 1 << 11, // TRANSFER_READ
        }]
    );

    let direct_stream = vk
        .queues
        .iter()
        .find(|q| q.kind == QueueKind::Direct)
        .unwrap();
    let direct_barriers: Vec<NativeBackendBarrier> = direct_stream
        .commands
        .iter()
        .flat_map(|c| match c {
            LoweredCommand::Barriers(bs) => bs.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        direct_barriers,
        vec![
            NativeBackendBarrier::Vulkan14SplitEventSignal {
                event_id: 1,
                resource: ResourceRef::Buffer(gfx_buf),
                src_stage_mask: 1 << 11,
            },
            NativeBackendBarrier::Vulkan14SplitEventWait {
                event_id: 1,
                resource: ResourceRef::Buffer(gfx_buf),
                dst_stage_mask: 1 << 7,
                src_access_mask: (1 << 5) | (1 << 6),
                dst_access_mask: 1 << 5,
                old_layout: None,
                new_layout: None,
                subresources: None,
            },
        ]
    );

    // Direct queue-aware single-barrier helper also collapses splits on Copy queues.
    let begin = Barrier::Buffer(BufferBarrier {
        resource: copy_buf,
        sync_before: Sync::COPY,
        sync_after: Sync::SPLIT,
        access_before: Access::COPY_DEST,
        access_after: Access::COPY_SOURCE,
    });
    assert_eq!(
        lower_to_target_barrier_for_queue(GpuApiTarget::Vulkan14, QueueKind::Copy, &begin, 1),
        None
    );
}
