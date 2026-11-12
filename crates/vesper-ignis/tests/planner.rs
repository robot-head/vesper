use vesper_ignis::*;

const ALL: SubresourceRange = SubresourceRange::ALL;

fn buffer(g: &mut RenderGraph, name: &str, size: u64) -> BufferHandle {
    g.add_buffer(BufferDesc {
        name: name.into(),
        size,
    })
    .unwrap()
}

fn texture(g: &mut RenderGraph, name: &str, initial: Layout) -> TextureHandle {
    g.add_texture(TextureDesc::simple(name, initial)).unwrap()
}

fn barriers(plan: &Plan, queue: QueueId) -> Vec<Barrier> {
    plan.queues[queue.index()]
        .commands
        .iter()
        .flat_map(|c| match c {
            Command::Barriers(b) => b.clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn texture_barriers(plan: &Plan, queue: QueueId) -> Vec<TextureBarrier> {
    barriers(plan, queue)
        .into_iter()
        .filter_map(|b| {
            if let Barrier::Texture(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect()
}

fn buffer_barriers(plan: &Plan, queue: QueueId) -> Vec<BufferBarrier> {
    barriers(plan, queue)
        .into_iter()
        .filter_map(|b| {
            if let Barrier::Buffer(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect()
}

fn fences(plan: &Plan, queue: QueueId) -> Vec<Command> {
    plan.queues[queue.index()]
        .commands
        .iter()
        .filter(|c| matches!(c, Command::Wait { .. } | Command::Signal { .. }))
        .cloned()
        .collect()
}

fn no_split() -> PlanOptions {
    PlanOptions {
        split_barriers: false,
    }
}

// ---------------------------------------------------------------------------
// Hazards on one queue

#[test]
fn read_after_write_transitions_texture_with_exact_scopes() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "gbuffer", Layout::UnorderedAccess);
    g.add_pass(PassDesc::new("write", q).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("read", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(
        b[0],
        TextureBarrier {
            resource: t,
            sync_before: Sync::COMPUTE_SHADING,
            sync_after: Sync::PIXEL_SHADING,
            access_before: Access::UNORDERED_ACCESS,
            access_after: Access::SHADER_RESOURCE,
            layout_before: Layout::UnorderedAccess,
            layout_after: Layout::ShaderResource,
            subresources: SubresourceRange::single(0, 0, 0),
        }
    );
    assert_eq!(plan.dependencies[0].kind, DependencyKind::ReadAfterWrite);
    // The barrier precedes the reader.
    let cmds = &plan.queues[0].commands;
    assert!(matches!(cmds[0], Command::Execute(_)));
    assert!(matches!(cmds[1], Command::Barriers(_)));
    assert!(matches!(cmds[2], Command::Execute(_)));
}

#[test]
fn write_after_read_on_buffer_waits_for_reader_scope() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let b = buffer(&mut g, "lights", 4096);
    g.add_pass(PassDesc::new("shade", q).buffer(
        b,
        ByteRange::WHOLE,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("update", q).buffer(
        b,
        ByteRange::WHOLE,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert_eq!(
        buffer_barriers(&plan, q),
        vec![BufferBarrier {
            resource: b,
            sync_before: Sync::PIXEL_SHADING,
            sync_after: Sync::COMPUTE_SHADING,
            access_before: Access::SHADER_RESOURCE,
            access_after: Access::UNORDERED_ACCESS,
        }]
    );
    assert_eq!(plan.dependencies[0].kind, DependencyKind::WriteAfterRead);
}

#[test]
fn first_buffer_read_needs_no_barrier() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let b = buffer(&mut g, "constants", 256);
    g.add_pass(PassDesc::new("a", q).buffer(
        b,
        ByteRange::WHOLE,
        Access::CONSTANT_BUFFER,
        Sync::VERTEX_SHADING,
    ))
    .unwrap();
    assert_eq!(g.plan().unwrap().barrier_count(), 0);
}

#[test]
fn write_after_write_emits_uav_barrier_without_layout_change() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Compute);
    let t = texture(&mut g, "accum", Layout::UnorderedAccess);
    for name in ["pass0", "pass1"] {
        g.add_pass(PassDesc::new(name, q).texture(
            t,
            ALL,
            Access::UNORDERED_ACCESS,
            Sync::COMPUTE_SHADING,
            Layout::UnorderedAccess,
        ))
        .unwrap();
    }
    let plan = g.plan().unwrap();
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(
        (b[0].layout_before, b[0].layout_after),
        (Layout::UnorderedAccess, Layout::UnorderedAccess)
    );
    assert_eq!(
        (b[0].access_before, b[0].access_after),
        (Access::UNORDERED_ACCESS, Access::UNORDERED_ACCESS)
    );
    assert_eq!(plan.dependencies[0].kind, DependencyKind::WriteAfterWrite);
}

#[test]
fn disjoint_buffer_ranges_do_not_synchronize() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Compute);
    let b = buffer(&mut g, "scratch", 1024);
    g.add_pass(PassDesc::new("lo", q).buffer(
        b,
        ByteRange::new(0, 512),
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("hi", q).buffer(
        b,
        ByteRange::new(512, 512),
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert!(plan.dependencies.is_empty());
    assert_eq!(plan.barrier_count(), 0);

    // A read that overlaps only the upper half depends only on `hi`.
    g.add_pass(PassDesc::new("read", q).buffer(
        b,
        ByteRange::new(600, 8),
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert_eq!(plan.dependencies.len(), 1);
    assert_eq!(plan.dependencies[0].before.index(), 1);
    assert_eq!(buffer_barriers(&plan, q).len(), 1);
}

#[test]
fn adjacent_but_not_overlapping_ranges_are_disjoint() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Copy);
    let b = buffer(&mut g, "staging", 100);
    g.add_pass(PassDesc::new("a", q).buffer(
        b,
        ByteRange::new(0, 50),
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("b", q).buffer(
        b,
        ByteRange::new(50, ByteRange::WHOLE.size),
        Access::COPY_DEST,
        Sync::COPY,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("c", q).buffer(
        b,
        ByteRange::new(49, 2),
        Access::COPY_SOURCE,
        Sync::COPY,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let befores: Vec<usize> = plan.dependencies.iter().map(|d| d.before.index()).collect();
    assert_eq!(befores, vec![0, 1]);
    // Both writers are merged into one whole-buffer barrier.
    let b = buffer_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].access_before, Access::COPY_DEST);
    assert_eq!(b[0].access_after, Access::COPY_SOURCE);
}

#[test]
fn mip_chain_generation_synchronizes_only_adjacent_mips() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Compute);
    let t = g
        .add_texture(TextureDesc::simple("pyramid", Layout::UnorderedAccess).with_mips(4))
        .unwrap();
    for mip in 1..4 {
        g.add_pass(
            PassDesc::new(format!("down{mip}"), q)
                .texture(
                    t,
                    SubresourceRange::mips(mip - 1, 1),
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    None,
                )
                .texture(
                    t,
                    SubresourceRange::mips(mip, 1),
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                    None,
                ),
        )
        .unwrap();
    }
    let plan = g.plan_with(no_split()).unwrap();
    // down1 reads mip0 (initial contents, layout change only) and writes mip1.
    // down2 reads mip1 (RAW on down1) and writes mip2, and so on. No pass
    // depends on a non-adjacent pass.
    for d in &plan.dependencies {
        assert_eq!(d.after.index(), d.before.index() + 1, "{d:?}");
    }
    let b = texture_barriers(&plan, q);
    assert!(b.iter().all(|b| b.subresources.mip_count == 1));
    let mip_transitions: Vec<(u32, Layout, Layout)> = b
        .iter()
        .map(|b| (b.subresources.first_mip, b.layout_before, b.layout_after))
        .collect();
    assert_eq!(
        mip_transitions,
        vec![
            (0, Layout::UnorderedAccess, Layout::ShaderResource),
            (1, Layout::UnorderedAccess, Layout::ShaderResource),
            (2, Layout::UnorderedAccess, Layout::ShaderResource),
        ]
    );
    // Mip 1 was written by down1, so its transition waits for that write.
    let mip1 = b.iter().find(|b| b.subresources.first_mip == 1).unwrap();
    assert_eq!(
        (mip1.sync_before, mip1.access_before),
        (Sync::COMPUTE_SHADING, Access::UNORDERED_ACCESS)
    );
    // Mip 0 holds initial contents: nothing to wait for.
    let mip0 = b.iter().find(|b| b.subresources.first_mip == 0).unwrap();
    assert_eq!(
        (mip0.sync_before, mip0.access_before),
        (Sync::NONE, Access::NO_ACCESS)
    );
}

#[test]
fn whole_texture_transition_is_one_box() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = g
        .add_texture(
            TextureDesc::simple("shadow", Layout::Undefined)
                .with_mips(4)
                .with_array_size(6),
        )
        .unwrap();
    g.add_pass(PassDesc::new("render", q).texture(
        t,
        ALL,
        Access::DEPTH_STENCIL_WRITE,
        Sync::DEPTH_STENCIL,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(
        b[0].subresources,
        SubresourceRange {
            first_mip: 0,
            mip_count: 4,
            first_slice: 0,
            slice_count: 6,
            first_plane: 0,
            plane_count: 1
        }
    );
    assert_eq!(
        (b[0].layout_before, b[0].layout_after),
        (Layout::Undefined, Layout::DepthStencilWrite)
    );
}

#[test]
fn partial_slice_transition_splits_into_boxes() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = g
        .add_texture(
            TextureDesc::simple("cube", Layout::ShaderResource)
                .with_mips(2)
                .with_array_size(6),
        )
        .unwrap();
    g.add_pass(PassDesc::new("face2", q).texture(
        t,
        SubresourceRange::slices(2, 1),
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        Layout::RenderTarget,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(
        b[0].subresources,
        SubresourceRange {
            first_mip: 0,
            mip_count: 2,
            first_slice: 2,
            slice_count: 1,
            first_plane: 0,
            plane_count: 1
        }
    );
}

// ---------------------------------------------------------------------------
// Read reuse and layout changes

#[test]
fn compatible_reads_reuse_layout_without_barriers() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "albedo", Layout::RenderTarget);
    g.add_pass(PassDesc::new("gbuffer", q).texture(
        t,
        ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        None,
    ))
    .unwrap();
    for name in ["light", "fog", "bloom"] {
        g.add_pass(PassDesc::new(name, q).texture(
            t,
            ALL,
            Access::SHADER_RESOURCE,
            Sync::PIXEL_SHADING,
            None,
        ))
        .unwrap();
    }
    let plan = g.plan().unwrap();
    // One RENDER_TARGET -> SHADER_RESOURCE transition before `light`; the
    // other readers share it.
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].layout_after, Layout::ShaderResource);
}

#[test]
fn generic_read_layout_serves_shader_and_copy_reads() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "lut", Layout::GenericRead);
    g.add_pass(PassDesc::new("sample", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("copy", q).texture(t, ALL, Access::COPY_SOURCE, Sync::COPY, None))
        .unwrap();
    let plan = g.plan().unwrap();
    assert_eq!(plan.barrier_count(), 0);
    assert_eq!(plan.dependencies[0].kind, DependencyKind::ReadOrder);
}

#[test]
fn read_in_new_sync_scope_after_write_needs_its_own_barrier() {
    // The barrier before the pixel read does not order compute-shader reads.
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "ao", Layout::UnorderedAccess);
    g.add_pass(PassDesc::new("ao", q).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("ps", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("cs", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("ps2", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    let b = texture_barriers(&g.plan_with(no_split()).unwrap(), q);
    assert_eq!(b.len(), 2);
    assert_eq!(
        b[1].sync_before,
        Sync::PIXEL_SHADING,
        "chains on the first barrier's after-scope"
    );
    assert_eq!(b[1].sync_after, Sync::COMPUTE_SHADING);
    assert_eq!(
        (b[1].layout_before, b[1].layout_after),
        (Layout::ShaderResource, Layout::ShaderResource)
    );
    assert_eq!(b[1].access_before, Access::SHADER_RESOURCE);
}

#[test]
fn aggregate_sync_covers_later_reads() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "ao", Layout::UnorderedAccess);
    g.add_pass(PassDesc::new("ao", q).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("all", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::ALL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("ps", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("cs", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    assert_eq!(g.plan().unwrap().barrier_count(), 1);
}

#[test]
fn layout_round_trip_render_target_to_shader_resource_and_back() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "color", Layout::Undefined);
    let rt = |name: &str| {
        PassDesc::new(name, q).texture(t, ALL, Access::RENDER_TARGET, Sync::RENDER_TARGET, None)
    };
    g.add_pass(rt("draw")).unwrap();
    g.add_pass(PassDesc::new("sample", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(rt("overlay")).unwrap();
    let b = texture_barriers(&g.plan_with(no_split()).unwrap(), q);
    let layouts: Vec<_> = b
        .iter()
        .map(|b| (b.layout_before, b.layout_after))
        .collect();
    assert_eq!(
        layouts,
        vec![
            (Layout::Undefined, Layout::RenderTarget),
            (Layout::RenderTarget, Layout::ShaderResource),
            (Layout::ShaderResource, Layout::RenderTarget),
        ]
    );
    assert_eq!(
        (b[0].sync_before, b[0].access_before),
        (Sync::NONE, Access::NO_ACCESS)
    );
    assert_eq!(
        (b[2].sync_before, b[2].access_before),
        (Sync::PIXEL_SHADING, Access::SHADER_RESOURCE)
    );
}

#[test]
fn depth_read_plus_shader_read_chooses_direct_queue_generic_read() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "depth", Layout::DepthStencilWrite);
    g.add_pass(
        PassDesc::new("decal", q)
            .texture(
                t,
                ALL,
                Access::DEPTH_STENCIL_READ,
                Sync::DEPTH_STENCIL,
                None,
            )
            .texture(t, ALL, Access::SHADER_RESOURCE, Sync::PIXEL_SHADING, None),
    )
    .unwrap();
    let b = texture_barriers(&g.plan().unwrap(), q);
    assert_eq!(b[0].layout_after, Layout::DirectQueueGenericRead);
    assert_eq!(
        b[0].access_after,
        Access::DEPTH_STENCIL_READ | Access::SHADER_RESOURCE
    );
    assert_eq!(b[0].sync_after, Sync::DEPTH_STENCIL | Sync::PIXEL_SHADING);
}

#[test]
fn final_layout_is_reached_on_last_queue() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = g
        .add_texture(
            TextureDesc::simple("backbuffer", Layout::PRESENT).with_final_layout(Layout::PRESENT),
        )
        .unwrap();
    g.add_pass(PassDesc::new("ui", q).texture(
        t,
        ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let cmds = &plan.queues[0].commands;
    let Command::Barriers(last) = cmds.last().unwrap() else {
        panic!("final barrier batch")
    };
    let Barrier::Texture(b) = last[0] else {
        panic!()
    };
    assert_eq!(
        (b.layout_before, b.layout_after),
        (Layout::RenderTarget, Layout::PRESENT)
    );
    assert_eq!(
        (b.sync_before, b.sync_after),
        (Sync::RENDER_TARGET, Sync::NONE)
    );
    assert_eq!(
        (b.access_before, b.access_after),
        (Access::RENDER_TARGET, Access::NO_ACCESS)
    );
}

// ---------------------------------------------------------------------------
// Split barriers

#[test]
fn long_distance_dependency_uses_split_barrier() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let t = texture(&mut g, "shadow", Layout::Undefined);
    let other = texture(&mut g, "other", Layout::RenderTarget);
    g.add_pass(PassDesc::new("shadow", q).texture(
        t,
        ALL,
        Access::DEPTH_STENCIL_WRITE,
        Sync::DEPTH_STENCIL,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("unrelated", q).texture(
        other,
        ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("lit", q).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let rendered = plan.render(&g);
    let expected = "\
order: shadow, unrelated, lit
queue 0 (direct):
  barriers:
    texture shadow [mips 0+1 slices 0+1 planes 0+1] sync NONE->DEPTH_STENCIL access NO_ACCESS->DEPTH_STENCIL_WRITE layout UNDEFINED->DEPTH_STENCIL_WRITE
  execute shadow
  barriers:
    texture shadow [mips 0+1 slices 0+1 planes 0+1] sync DEPTH_STENCIL->SPLIT access DEPTH_STENCIL_WRITE->SHADER_RESOURCE layout DEPTH_STENCIL_WRITE->SHADER_RESOURCE
  execute unrelated
  barriers:
    texture shadow [mips 0+1 slices 0+1 planes 0+1] sync SPLIT->PIXEL_SHADING access DEPTH_STENCIL_WRITE->SHADER_RESOURCE layout DEPTH_STENCIL_WRITE->SHADER_RESOURCE
  execute lit
";
    assert_eq!(rendered, expected);

    let plan = g.plan_with(no_split()).unwrap();
    let b = texture_barriers(&plan, q);
    assert_eq!(b.len(), 2);
    assert!(
        b.iter()
            .all(|b| b.sync_before != Sync::SPLIT && b.sync_after != Sync::SPLIT)
    );
}

#[test]
fn split_is_not_used_across_an_intervening_access_to_the_resource() {
    // `mid` touches a different range of the same buffer; buffer barriers are
    // whole-buffer, so the split must not straddle it.
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Compute);
    let b = buffer(&mut g, "b", 64);
    let u = |name: &str, r: ByteRange| {
        PassDesc::new(name, q).buffer(b, r, Access::UNORDERED_ACCESS, Sync::COMPUTE_SHADING)
    };
    g.add_pass(u("first", ByteRange::new(0, 32))).unwrap();
    g.add_pass(u("mid", ByteRange::new(32, 32))).unwrap();
    g.add_pass(u("last", ByteRange::new(0, 32))).unwrap();
    let plan = g.plan().unwrap();
    let bb = buffer_barriers(&plan, q);
    assert_eq!(bb.len(), 1);
    assert_eq!(bb[0].sync_before, Sync::COMPUTE_SHADING);
}

// ---------------------------------------------------------------------------
// Ordering and cycles

#[test]
fn explicit_dependency_against_data_order_is_a_cycle() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let b = buffer(&mut g, "b", 16);
    let w = g
        .add_pass(PassDesc::new("w", q).buffer(b, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY))
        .unwrap();
    let r = g
        .add_pass(PassDesc::new("r", q).buffer(
            b,
            ByteRange::WHOLE,
            Access::COPY_SOURCE,
            Sync::COPY,
        ))
        .unwrap();
    g.add_dependency(r, w).unwrap();
    let err = g.plan().unwrap_err();
    assert_eq!(err, PlanError::Cycle { passes: vec![w, r] });
    assert!(err.to_string().contains("cycle"));
}

#[test]
fn three_pass_cycle_is_reported_in_edge_order() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let a = g.add_pass(PassDesc::new("a", q)).unwrap();
    let b = g.add_pass(PassDesc::new("b", q)).unwrap();
    let c = g.add_pass(PassDesc::new("c", q)).unwrap();
    let free = g.add_pass(PassDesc::new("free", q)).unwrap();
    g.add_dependency(a, b).unwrap();
    g.add_dependency(b, c).unwrap();
    g.add_dependency(c, a).unwrap();
    g.add_dependency(free, a).unwrap();
    assert_eq!(
        g.plan().unwrap_err(),
        PlanError::Cycle {
            passes: vec![a, b, c]
        }
    );
}

#[test]
fn explicit_dependency_reorders_independent_passes() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let first = g.add_pass(PassDesc::new("declared_first", q)).unwrap();
    let second = g.add_pass(PassDesc::new("declared_second", q)).unwrap();
    g.add_dependency(second, first).unwrap();
    assert_eq!(g.plan().unwrap().order, vec![second, first]);
}

#[test]
fn self_dependency_is_rejected() {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let a = g.add_pass(PassDesc::new("a", q)).unwrap();
    assert_eq!(
        g.add_dependency(a, a),
        Err(GraphError::SelfDependency { pass: a })
    );
}

// ---------------------------------------------------------------------------
// Queues and fences

#[test]
fn cross_queue_read_after_write_uses_fence_not_barrier() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let compute = g.add_queue(QueueKind::Compute);
    let b = buffer(&mut g, "particles", 1 << 16);
    g.add_pass(PassDesc::new("simulate", compute).buffer(
        b,
        ByteRange::WHOLE,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("draw", direct).buffer(
        b,
        ByteRange::WHOLE,
        Access::VERTEX_BUFFER,
        Sync::VERTEX_SHADING,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert_eq!(plan.barrier_count(), 0);
    assert_eq!(fences(&plan, compute), vec![Command::Signal { value: 1 }]);
    assert_eq!(
        fences(&plan, direct),
        vec![Command::Wait {
            queue: compute,
            value: 1
        }]
    );
    // The wait precedes the consumer.
    assert!(matches!(plan.queues[0].commands[0], Command::Wait { .. }));
}

#[test]
fn cross_queue_write_after_read_waits_for_reader() {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let compute = g.add_queue(QueueKind::Compute);
    let b = buffer(&mut g, "indirect", 64);
    g.add_pass(PassDesc::new("draw", direct).buffer(
        b,
        ByteRange::WHOLE,
        Access::INDIRECT_ARGUMENT,
        Sync::EXECUTE_INDIRECT,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("rebuild", compute).buffer(
        b,
        ByteRange::WHOLE,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert_eq!(fences(&plan, direct), vec![Command::Signal { value: 1 }]);
    assert_eq!(
        fences(&plan, compute),
        vec![Command::Wait {
            queue: direct,
            value: 1
        }]
    );
    assert_eq!(plan.barrier_count(), 0);
}

#[test]
fn redundant_and_transitive_waits_are_elided() {
    let mut g = RenderGraph::new();
    let q0 = g.add_queue(QueueKind::Direct);
    let q1 = g.add_queue(QueueKind::Compute);
    let q2 = g.add_queue(QueueKind::Compute);
    let x = buffer(&mut g, "x", 16);
    let y = buffer(&mut g, "y", 16);
    let w = |name: &str, q, b| {
        PassDesc::new(name, q).buffer(
            b,
            ByteRange::WHOLE,
            Access::UNORDERED_ACCESS,
            Sync::COMPUTE_SHADING,
        )
    };
    let r = |d: PassDesc, b| {
        d.buffer(
            b,
            ByteRange::WHOLE,
            Access::SHADER_RESOURCE,
            Sync::COMPUTE_SHADING,
        )
    };
    g.add_pass(w("a", q0, x)).unwrap(); // writes x on q0
    g.add_pass(r(w("b", q1, y), x)).unwrap(); // q1 waits for a, writes y
    g.add_pass(r(r(PassDesc::new("c", q2), x), y)).unwrap(); // needs a and b
    g.add_pass(r(PassDesc::new("d", q2), x)).unwrap(); // needs a again
    let plan = g.plan().unwrap();
    // q2 waits only for q1 (b), which already waited for a.
    assert_eq!(
        fences(&plan, q2),
        vec![Command::Wait {
            queue: q1,
            value: 1
        }]
    );
    assert_eq!(
        fences(&plan, q1),
        vec![
            Command::Wait {
                queue: q0,
                value: 1
            },
            Command::Signal { value: 1 }
        ]
    );
    assert_eq!(fences(&plan, q0), vec![Command::Signal { value: 1 }]);
}

#[test]
fn fence_values_increase_per_queue_and_waits_never_precede_their_signal() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let bufs: Vec<_> = (0..4)
        .map(|i| buffer(&mut g, &format!("b{i}"), 16))
        .collect();
    // Ping-pong: each pass reads the previous buffer on the other queue.
    for (i, pair) in bufs.windows(2).enumerate() {
        let q = if i % 2 == 0 { c } else { d };
        g.add_pass(
            PassDesc::new(format!("p{i}"), q)
                .buffer(
                    pair[0],
                    ByteRange::WHOLE,
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                )
                .buffer(
                    pair[1],
                    ByteRange::WHOLE,
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                ),
        )
        .unwrap();
    }
    let plan = g.plan().unwrap();
    assert_valid_fences(&plan);
    assert_eq!(
        fences(&plan, c),
        vec![
            Command::Signal { value: 1 },
            Command::Wait { queue: d, value: 1 }
        ]
    );
    assert_eq!(
        fences(&plan, d),
        vec![
            Command::Wait { queue: c, value: 1 },
            Command::Signal { value: 1 }
        ]
    );
}

/// Simulates the queues: each wait must be satisfiable by a signal that the
/// other queue reaches without itself waiting on this one (no deadlock), and
/// each queue's signal values are strictly increasing.
fn assert_valid_fences(plan: &Plan) {
    let n = plan.queues.len();
    let mut pc = vec![0usize; n];
    let mut fence = vec![0u64; n];
    let mut last_signal = vec![0u64; n];
    loop {
        let mut progressed = false;
        for q in 0..n {
            while let Some(cmd) = plan.queues[q].commands.get(pc[q]) {
                match cmd {
                    Command::Wait { queue, value } if fence[queue.index()] < *value => break,
                    Command::Signal { value } => {
                        assert!(*value > last_signal[q], "fence values must increase");
                        last_signal[q] = *value;
                        fence[q] = *value;
                    }
                    _ => {}
                }
                pc[q] += 1;
                progressed = true;
            }
        }
        if (0..n).all(|q| pc[q] == plan.queues[q].commands.len()) {
            return;
        }
        assert!(progressed, "fence plan deadlocks");
    }
}

#[test]
fn concurrent_reads_on_two_queues_share_a_layout() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let t = texture(&mut g, "hiz", Layout::UnorderedAccess);
    g.add_pass(PassDesc::new("build", d).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("cull_gfx", d).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("cull_async", c).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("rebuild", d).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    let plan = g.plan_with(no_split()).unwrap();
    assert_valid_fences(&plan);
    // The async read reuses SHADER_RESOURCE without a barrier on compute.
    assert!(barriers(&plan, c).is_empty());
    assert_eq!(
        fences(&plan, c),
        vec![
            Command::Wait { queue: d, value: 1 },
            Command::Signal { value: 1 }
        ]
    );
    // Rebuilding waits for the compute reader and transitions back.
    let db = texture_barriers(&plan, d);
    assert_eq!(db.len(), 2);
    assert_eq!(
        (db[1].layout_before, db[1].layout_after),
        (Layout::ShaderResource, Layout::UnorderedAccess)
    );
    assert_eq!(db[1].sync_before, Sync::COMPUTE_SHADING);
    assert_eq!(
        fences(&plan, d),
        vec![
            Command::Signal { value: 1 },
            Command::Wait { queue: c, value: 1 }
        ]
    );
}

#[test]
fn render_target_handoff_to_compute_transitions_on_producer_queue() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let t = texture(&mut g, "scene", Layout::Undefined);
    g.add_pass(PassDesc::new("draw", d).texture(
        t,
        ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("tonemap", c).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let expected = "\
order: draw, tonemap
queue 0 (direct):
  barriers:
    texture scene [mips 0+1 slices 0+1 planes 0+1] sync NONE->RENDER_TARGET access NO_ACCESS->RENDER_TARGET layout UNDEFINED->RENDER_TARGET
  execute draw
  barriers:
    texture scene [mips 0+1 slices 0+1 planes 0+1] sync RENDER_TARGET->NONE access RENDER_TARGET->NO_ACCESS layout RENDER_TARGET->SHADER_RESOURCE
  signal 1
queue 1 (compute):
  wait q0 >= 1
  execute tonemap
";
    assert_eq!(plan.render(&g), expected);
}

#[test]
fn copy_queue_cannot_transition_textures() {
    let mut g = RenderGraph::new();
    let copy = g.add_queue(QueueKind::Copy);
    let undefined = texture(&mut g, "fresh", Layout::Undefined);
    let pass = g
        .add_pass(PassDesc::new("upload", copy).texture(
            undefined,
            ALL,
            Access::COPY_DEST,
            Sync::COPY,
            None,
        ))
        .unwrap();
    assert_eq!(
        g.plan().unwrap_err(),
        PlanError::UnsupportedQueueHandoff {
            pass,
            resource: undefined,
            subresource: 0,
            from: Layout::Undefined,
            to: Layout::Common,
        }
    );

    // Created in COMMON, the same upload needs no barrier at all.
    let mut g = RenderGraph::new();
    let copy = g.add_queue(QueueKind::Copy);
    let common = texture(&mut g, "common", Layout::Common);
    g.add_pass(PassDesc::new("upload", copy).texture(
        common,
        ALL,
        Access::COPY_DEST,
        Sync::COPY,
        None,
    ))
    .unwrap();
    assert_eq!(g.plan().unwrap().barrier_count(), 0);
}

#[test]
fn copy_queue_consumer_gets_common_layout_from_direct_queue() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let copy = g.add_queue(QueueKind::Copy);
    let t = texture(&mut g, "capture", Layout::Undefined);
    g.add_pass(PassDesc::new("draw", d).texture(
        t,
        ALL,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("readback", copy).texture(
        t,
        ALL,
        Access::COPY_SOURCE,
        Sync::COPY,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    let db = texture_barriers(&plan, d);
    assert_eq!(
        (db[1].layout_before, db[1].layout_after),
        (Layout::RenderTarget, Layout::Common)
    );
    assert!(barriers(&plan, copy).is_empty());
    assert_eq!(
        fences(&plan, copy),
        vec![Command::Wait { queue: d, value: 1 }]
    );
}

#[test]
fn final_layout_after_reads_on_two_queues_joins_on_one_queue() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let t = g
        .add_texture(
            TextureDesc::simple("history", Layout::ShaderResource)
                .with_final_layout(Layout::UnorderedAccess),
        )
        .unwrap();
    g.add_pass(PassDesc::new("taa", d).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ))
    .unwrap();
    g.add_pass(PassDesc::new("motion", c).texture(
        t,
        ALL,
        Access::SHADER_RESOURCE,
        Sync::COMPUTE_SHADING,
        None,
    ))
    .unwrap();
    let plan = g.plan().unwrap();
    assert_valid_fences(&plan);
    // The latest reader's queue (compute) transitions after waiting for taa.
    let cmds = &plan.queues[c.index()].commands;
    assert_eq!(cmds[cmds.len() - 2], Command::Wait { queue: d, value: 1 });
    let cb = texture_barriers(&plan, c);
    assert_eq!(cb.len(), 1);
    assert_eq!(
        (cb[0].sync_before, cb[0].access_before),
        (Sync::COMPUTE_SHADING, Access::SHADER_RESOURCE)
    );
    assert_eq!(
        (cb[0].layout_before, cb[0].layout_after),
        (Layout::ShaderResource, Layout::UnorderedAccess)
    );
}

#[test]
fn queue_specific_layout_is_rejected_on_the_other_queue() {
    let mut g = RenderGraph::new();
    let c = g.add_queue(QueueKind::Compute);
    let t = texture(&mut g, "t", Layout::Common);
    let err = g
        .add_pass(PassDesc::new("p", c).texture(
            t,
            ALL,
            Access::SHADER_RESOURCE,
            Sync::COMPUTE_SHADING,
            Layout::DirectQueueShaderResource,
        ))
        .unwrap_err();
    assert!(matches!(
        err,
        GraphError::QueueUnsupported {
            restriction: QueueRestriction::Layout(Layout::DirectQueueShaderResource),
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Determinism

fn frame_graph() -> RenderGraph {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let copy = g.add_queue(QueueKind::Copy);
    let upload = buffer(&mut g, "upload", 1 << 20);
    let instances = buffer(&mut g, "instances", 1 << 20);
    let depth = g
        .add_texture(TextureDesc::simple("depth", Layout::Undefined))
        .unwrap();
    let hiz = g
        .add_texture(TextureDesc::simple("hiz", Layout::Undefined).with_mips(5))
        .unwrap();
    let color = g
        .add_texture(
            TextureDesc::simple("color", Layout::Undefined).with_final_layout(Layout::PRESENT),
        )
        .unwrap();
    g.add_pass(
        PassDesc::new("upload", copy)
            .buffer(instances, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY)
            .buffer(upload, ByteRange::WHOLE, Access::COPY_SOURCE, Sync::COPY),
    )
    .unwrap();
    g.add_pass(
        PassDesc::new("depth_prepass", d)
            .buffer(
                instances,
                ByteRange::WHOLE,
                Access::VERTEX_BUFFER,
                Sync::VERTEX_SHADING,
            )
            .texture(
                depth,
                ALL,
                Access::DEPTH_STENCIL_WRITE,
                Sync::DEPTH_STENCIL,
                None,
            ),
    )
    .unwrap();
    g.add_pass(
        PassDesc::new("hiz_mip0", d)
            .texture(
                depth,
                ALL,
                Access::SHADER_RESOURCE,
                Sync::COMPUTE_SHADING,
                None,
            )
            .texture(
                hiz,
                SubresourceRange::mips(0, 1),
                Access::UNORDERED_ACCESS,
                Sync::COMPUTE_SHADING,
                None,
            ),
    )
    .unwrap();
    for mip in 1..5 {
        g.add_pass(
            PassDesc::new(format!("hiz_mip{mip}"), c)
                .texture(
                    hiz,
                    SubresourceRange::mips(mip - 1, 1),
                    Access::SHADER_RESOURCE,
                    Sync::COMPUTE_SHADING,
                    None,
                )
                .texture(
                    hiz,
                    SubresourceRange::mips(mip, 1),
                    Access::UNORDERED_ACCESS,
                    Sync::COMPUTE_SHADING,
                    None,
                ),
        )
        .unwrap();
    }
    g.add_pass(
        PassDesc::new("cull", c)
            .texture(
                hiz,
                ALL,
                Access::SHADER_RESOURCE,
                Sync::COMPUTE_SHADING,
                None,
            )
            .buffer(
                instances,
                ByteRange::new(0, 4096),
                Access::UNORDERED_ACCESS,
                Sync::COMPUTE_SHADING,
            ),
    )
    .unwrap();
    g.add_pass(
        PassDesc::new("main", d)
            .buffer(
                instances,
                ByteRange::WHOLE,
                Access::VERTEX_BUFFER,
                Sync::VERTEX_SHADING,
            )
            .texture(
                depth,
                ALL,
                Access::DEPTH_STENCIL_READ,
                Sync::DEPTH_STENCIL,
                None,
            )
            .texture(color, ALL, Access::RENDER_TARGET, Sync::RENDER_TARGET, None),
    )
    .unwrap();
    g
}

#[test]
fn plans_are_deterministic_and_executable() {
    let a = frame_graph();
    let b = frame_graph();
    let pa = a.plan().unwrap();
    let pb = b.plan().unwrap();
    assert_eq!(pa.render(&a), pb.render(&b));
    assert_eq!(pa.render(&a), a.plan().unwrap().render(&a));
    assert_valid_fences(&pa);
    // Queue streams are consistent with the global order.
    for q in &pa.queues {
        let exec: Vec<usize> = q
            .commands
            .iter()
            .filter_map(|c| {
                if let Command::Execute(p) = c {
                    Some(pa.order.iter().position(|x| x == p).unwrap())
                } else {
                    None
                }
            })
            .collect();
        assert!(exec.windows(2).all(|w| w[0] < w[1]));
    }
    // Every split begin has a matching end on the same queue.
    for q in &pa.queues {
        let all: Vec<Barrier> = q
            .commands
            .iter()
            .flat_map(|c| {
                if let Command::Barriers(b) = c {
                    b.clone()
                } else {
                    vec![]
                }
            })
            .collect();
        let begins = all
            .iter()
            .filter(|b| {
                matches!(b, Barrier::Texture(t) if t.sync_after == Sync::SPLIT)
                    || matches!(b, Barrier::Buffer(t) if t.sync_after == Sync::SPLIT)
            })
            .count();
        let ends = all
            .iter()
            .filter(|b| {
                matches!(b, Barrier::Texture(t) if t.sync_before == Sync::SPLIT)
                    || matches!(b, Barrier::Buffer(t) if t.sync_before == Sync::SPLIT)
            })
            .count();
        assert_eq!(begins, ends);
    }
    // Every emitted texture barrier respects the modeled D3D12 rules.
    for q in &pa.queues {
        for b in q.commands.iter().flat_map(|c| {
            if let Command::Barriers(b) = c {
                b.clone()
            } else {
                vec![]
            }
        }) {
            if let Barrier::Texture(t) = b {
                assert!(
                    q.kind.can_transition(t.layout_before, t.layout_after)
                        || t.layout_before == t.layout_after
                );
                if t.sync_before == Sync::NONE {
                    assert_eq!(t.access_before, Access::NO_ACCESS);
                }
                if t.sync_after == Sync::NONE {
                    assert_eq!(t.access_after, Access::NO_ACCESS);
                }
                if t.access_before != Access::NO_ACCESS {
                    assert!(t.layout_before.allows(t.access_before), "{t:?}");
                }
                if t.access_after != Access::NO_ACCESS {
                    assert!(t.layout_after.allows(t.access_after), "{t:?}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Declaration errors

#[test]
fn declaration_errors_are_structured() {
    let mut g = RenderGraph::new();
    let d = g.add_queue(QueueKind::Direct);
    let c = g.add_queue(QueueKind::Compute);
    let t = texture(&mut g, "t", Layout::Common);
    let b = buffer(&mut g, "b", 16);

    let e = g.add_pass(PassDesc::new("p", d).texture(
        t,
        ALL,
        Access::RENDER_TARGET,
        Sync::COMPUTE_SHADING,
        None,
    ));
    assert!(matches!(
        e,
        Err(GraphError::AccessSyncMismatch {
            access: Access::RENDER_TARGET,
            ..
        })
    ));

    let e = g.add_pass(PassDesc::new("p", d).texture(
        t,
        ALL,
        Access::UNORDERED_ACCESS,
        Sync::COMPUTE_SHADING,
        Layout::ShaderResource,
    ));
    assert!(matches!(
        e,
        Err(GraphError::AccessLayoutMismatch {
            layout: Layout::ShaderResource,
            ..
        })
    ));

    let e =
        g.add_pass(PassDesc::new("p", c).texture(t, ALL, Access::RENDER_TARGET, Sync::ALL, None));
    assert!(matches!(
        e,
        Err(GraphError::QueueUnsupported {
            restriction: QueueRestriction::Access(_),
            ..
        })
    ));

    let e = g.add_pass(PassDesc::new("p", c).buffer(
        b,
        ByteRange::WHOLE,
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
    ));
    assert!(matches!(
        e,
        Err(GraphError::QueueUnsupported {
            restriction: QueueRestriction::Sync(_),
            ..
        })
    ));

    let e = g.add_pass(PassDesc::new("p", d).buffer(
        b,
        ByteRange::new(8, 9),
        Access::COPY_DEST,
        Sync::COPY,
    ));
    assert!(matches!(
        e,
        Err(GraphError::InvalidRange { use_index: 0, .. })
    ));

    let e = g.add_pass(PassDesc::new("p", d).buffer(
        b,
        ByteRange::new(0, 0),
        Access::COPY_DEST,
        Sync::COPY,
    ));
    assert!(matches!(e, Err(GraphError::InvalidRange { .. })));

    let e = g.add_pass(PassDesc::new("p", d).texture(
        t,
        SubresourceRange::mips(1, 1),
        Access::SHADER_RESOURCE,
        Sync::PIXEL_SHADING,
        None,
    ));
    assert!(matches!(e, Err(GraphError::InvalidRange { .. })));

    let e = g.add_pass(PassDesc::new("p", d).buffer(
        b,
        ByteRange::WHOLE,
        Access::RENDER_TARGET,
        Sync::RENDER_TARGET,
    ));
    assert!(matches!(
        e,
        Err(GraphError::AccessResourceKindMismatch { .. })
    ));

    let e = g.add_pass(PassDesc::new("p", d).texture(
        t,
        ALL,
        Access::VERTEX_BUFFER,
        Sync::VERTEX_SHADING,
        None,
    ));
    assert!(matches!(
        e,
        Err(GraphError::AccessResourceKindMismatch { .. })
    ));

    for access in [
        Access::NO_ACCESS,
        Access::COMMON,
        Access::from_bits(1 << 20),
    ] {
        let e = g.add_pass(PassDesc::new("p", d).buffer(b, ByteRange::WHOLE, access, Sync::ALL));
        assert!(
            matches!(e, Err(GraphError::InvalidAccess { .. })),
            "{access:?}"
        );
    }
    for sync in [Sync::NONE, Sync::SPLIT, Sync::COPY | Sync::SPLIT] {
        let e =
            g.add_pass(PassDesc::new("p", d).buffer(b, ByteRange::WHOLE, Access::COPY_DEST, sync));
        assert!(matches!(e, Err(GraphError::InvalidSync { .. })), "{sync:?}");
    }

    let e = g.add_pass(
        PassDesc::new("p", d)
            .buffer(b, ByteRange::new(0, 8), Access::COPY_DEST, Sync::COPY)
            .buffer(b, ByteRange::new(4, 8), Access::COPY_SOURCE, Sync::COPY),
    );
    assert_eq!(
        e,
        Err(GraphError::ConflictingUses {
            pass: "p".into(),
            first_use: 0,
            second_use: 1
        })
    );

    let e = g.add_pass(
        PassDesc::new("p", d)
            .texture(
                t,
                ALL,
                Access::SHADER_RESOURCE,
                Sync::PIXEL_SHADING,
                Layout::ShaderResource,
            )
            .texture(t, ALL, Access::COPY_SOURCE, Sync::COPY, Layout::CopySource),
    );
    assert!(matches!(e, Err(GraphError::ConflictingUses { .. })));

    // Nothing was added by the failed calls.
    assert_eq!(g.pass_count(), 0);

    let mut other = RenderGraph::new();
    let foreign_queue = other.add_queue(QueueKind::Direct);
    let foreign_buffer = buffer(&mut other, "b", 16);
    assert_eq!(
        g.add_pass(PassDesc::new("p", foreign_queue)),
        Err(GraphError::ForeignHandle)
    );
    let e = g.add_pass(PassDesc::new("p", d).buffer(
        foreign_buffer,
        ByteRange::WHOLE,
        Access::COPY_DEST,
        Sync::COPY,
    ));
    assert_eq!(e, Err(GraphError::ForeignHandle));

    assert!(matches!(
        g.add_buffer(BufferDesc {
            name: "z".into(),
            size: 0
        }),
        Err(GraphError::InvalidResource { .. })
    ));
    assert!(matches!(
        g.add_texture(TextureDesc::simple("z", Layout::Common).with_mips(0)),
        Err(GraphError::InvalidResource { .. })
    ));
    assert!(matches!(
        g.add_texture(
            TextureDesc::simple("z", Layout::Common).with_final_layout(Layout::Undefined)
        ),
        Err(GraphError::InvalidResource { .. })
    ));
}

#[test]
fn sync_aggregates_expand_as_specified() {
    assert!(Sync::ALL.covers(Sync::PIXEL_SHADING | Sync::COPY));
    assert!(Sync::DRAW.covers(Sync::RENDER_TARGET | Sync::DEPTH_STENCIL));
    assert!(!Sync::DRAW.covers(Sync::COMPUTE_SHADING));
    assert!(Sync::ALL_SHADING.covers(Sync::COMPUTE_SHADING));
    assert!(Sync::NON_PIXEL_SHADING.covers(Sync::VERTEX_SHADING));
    assert!(!Sync::NON_PIXEL_SHADING.covers(Sync::PIXEL_SHADING));
    // Conservative: ALL is not assumed to cover acceleration-structure builds.
    assert!(!Sync::ALL.covers(Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE));
    assert_eq!(
        format!("{}", Sync::PIXEL_SHADING | Sync::COPY),
        "PIXEL_SHADING|COPY"
    );
    assert_eq!(Layout::PRESENT.raw(), 0);
    assert_eq!(Layout::Undefined.raw(), u32::MAX);
}
