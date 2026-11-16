//! Multi-backend barrier target emitter for Vesper Ignis (§38.11).
//!
//! Lowers [`vesper_ignis`](crate) D3D12-vocabulary barriers (`Barrier`,
//! `TextureBarrier`, `BufferBarrier`, and full `Plan` command streams) into
//! target-native descriptors for:
//! - [`GpuApiTarget::D3D12Agility1620`] (direct enhanced barrier pass-through)
//! - [`GpuApiTarget::Vulkan14`] (`VkImageMemoryBarrier2` /
//!   `VkBufferMemoryBarrier2` and split `VkEvent` signal/wait handles)
//! - [`GpuApiTarget::Metal4`] (`MTL4` stage-to-stage barriers, automatically
//!   eliding layout-only initial transitions in unified TBDR memory)

use std::collections::BTreeMap;

use crate::d3d12::{Access, Layout, QueueKind, Sync};
use crate::graph::{BufferHandle, PassId, QueueId, ResourceRef, SubresourceRange, TextureHandle};
use crate::plan::{Barrier, Command, Plan};

/// Supported GPU backend targets for barrier lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuApiTarget {
    D3D12Agility1620,
    Vulkan14,
    Metal4,
}

/// Target-specific barrier representation emitted from the Ignis planner IR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeBackendBarrier {
    /// Native D3D12 Agility SDK 1.620 enhanced barrier.
    D3D12(Barrier),
    /// Vulkan 1.4 synchronization2 buffer memory barrier.
    Vulkan14BufferBarrier2 {
        resource: BufferHandle,
        src_stage_mask: u64,
        dst_stage_mask: u64,
        src_access_mask: u64,
        dst_access_mask: u64,
    },
    /// Vulkan 1.4 synchronization2 image memory barrier.
    Vulkan14ImageBarrier2 {
        resource: TextureHandle,
        subresources: SubresourceRange,
        src_stage_mask: u64,
        dst_stage_mask: u64,
        src_access_mask: u64,
        dst_access_mask: u64,
        old_layout: Layout,
        new_layout: Layout,
    },
    /// Begin half of a Vulkan 1.4 split barrier (`vkCmdSetEvent2`).
    Vulkan14SplitEventSignal {
        event_id: u32,
        resource: ResourceRef,
        src_stage_mask: u64,
    },
    /// End half of a Vulkan 1.4 split barrier (`vkCmdWaitEvents2`).
    Vulkan14SplitEventWait {
        event_id: u32,
        resource: ResourceRef,
        dst_stage_mask: u64,
        src_access_mask: u64,
        dst_access_mask: u64,
        old_layout: Option<Layout>,
        new_layout: Option<Layout>,
        subresources: Option<SubresourceRange>,
    },
    /// Apple Silicon Metal 4 command-encoder stage barrier (`after` -> `before`).
    Metal4StageBarrier {
        after_stages_mask: u32,
        before_stages_mask: u32,
    },
}

/// One command step lowered for a specific [`GpuApiTarget`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweredCommand {
    Wait { queue: QueueId, value: u64 },
    Barriers(Vec<NativeBackendBarrier>),
    Execute(PassId),
    Signal { value: u64 },
}

/// One per-queue command stream lowered for a specific [`GpuApiTarget`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredQueueStream {
    pub queue: QueueId,
    pub kind: QueueKind,
    pub commands: Vec<LoweredCommand>,
}

/// A complete [`Plan`] lowered to target-specific barriers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredPlan {
    pub target: GpuApiTarget,
    pub order: Vec<PassId>,
    pub queues: Vec<LoweredQueueStream>,
}

impl LoweredPlan {
    /// Total count of emitted native barriers across all queues.
    pub fn barrier_count(&self) -> usize {
        self.queues
            .iter()
            .flat_map(|q| &q.commands)
            .map(|c| match c {
                LoweredCommand::Barriers(bs) => bs.len(),
                _ => 0,
            })
            .sum()
    }
}

/// Lower a single planner [`Barrier`] into a [`NativeBackendBarrier`].
///
/// For split barriers (`Sync::SPLIT` in `sync_after` or `sync_before`),
/// `split_event_id` is used on [`GpuApiTarget::Vulkan14`] for the matching
/// `Vulkan14SplitEventSignal` / `Vulkan14SplitEventWait` pair.
///
/// On [`GpuApiTarget::Metal4`]:
/// - Initial layout-only transitions (`sync_before == Sync::NONE` with no
///   prior access) are elided (`None`) because Apple Silicon TBDR unified
///   memory does not require explicit layout transitions.
/// - Split-barrier begin halves (`sync_after.contains(Sync::SPLIT)`) are
///   deferred (`None`) because Metal 4 expresses the synchronization at the
///   consuming stage barrier (`sync_before.contains(Sync::SPLIT)`).
pub fn lower_to_target_barrier(
    target: GpuApiTarget,
    barrier: &Barrier,
    split_event_id: u32,
) -> Option<NativeBackendBarrier> {
    lower_barrier_with_split_hint(target, barrier, split_event_id, None)
}

fn lower_barrier_with_split_hint(
    target: GpuApiTarget,
    barrier: &Barrier,
    split_event_id: u32,
    split_producer_sync: Option<Sync>,
) -> Option<NativeBackendBarrier> {
    match target {
        GpuApiTarget::D3D12Agility1620 => Some(NativeBackendBarrier::D3D12(*barrier)),
        GpuApiTarget::Vulkan14 => match *barrier {
            Barrier::Buffer(bb) => {
                if bb.sync_after.contains(Sync::SPLIT) {
                    Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
                        event_id: split_event_id,
                        resource: ResourceRef::Buffer(bb.resource),
                        src_stage_mask: vk_stage_bits(bb.sync_before),
                    })
                } else if bb.sync_before.contains(Sync::SPLIT) {
                    Some(NativeBackendBarrier::Vulkan14SplitEventWait {
                        event_id: split_event_id,
                        resource: ResourceRef::Buffer(bb.resource),
                        dst_stage_mask: vk_stage_bits(bb.sync_after),
                        src_access_mask: vk_access_bits(bb.access_before),
                        dst_access_mask: vk_access_bits(bb.access_after),
                        old_layout: None,
                        new_layout: None,
                        subresources: None,
                    })
                } else {
                    Some(NativeBackendBarrier::Vulkan14BufferBarrier2 {
                        resource: bb.resource,
                        src_stage_mask: vk_stage_bits(bb.sync_before),
                        dst_stage_mask: vk_stage_bits(bb.sync_after),
                        src_access_mask: vk_access_bits(bb.access_before),
                        dst_access_mask: vk_access_bits(bb.access_after),
                    })
                }
            }
            Barrier::Texture(tb) => {
                if tb.sync_after.contains(Sync::SPLIT) {
                    Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
                        event_id: split_event_id,
                        resource: ResourceRef::Texture(tb.resource),
                        src_stage_mask: vk_stage_bits(tb.sync_before),
                    })
                } else if tb.sync_before.contains(Sync::SPLIT) {
                    Some(NativeBackendBarrier::Vulkan14SplitEventWait {
                        event_id: split_event_id,
                        resource: ResourceRef::Texture(tb.resource),
                        dst_stage_mask: vk_stage_bits(tb.sync_after),
                        src_access_mask: vk_access_bits(tb.access_before),
                        dst_access_mask: vk_access_bits(tb.access_after),
                        old_layout: Some(tb.layout_before),
                        new_layout: Some(tb.layout_after),
                        subresources: Some(tb.subresources),
                    })
                } else {
                    Some(NativeBackendBarrier::Vulkan14ImageBarrier2 {
                        resource: tb.resource,
                        subresources: tb.subresources,
                        src_stage_mask: vk_stage_bits(tb.sync_before),
                        dst_stage_mask: vk_stage_bits(tb.sync_after),
                        src_access_mask: vk_access_bits(tb.access_before),
                        dst_access_mask: vk_access_bits(tb.access_after),
                        old_layout: tb.layout_before,
                        new_layout: tb.layout_after,
                    })
                }
            }
        },
        GpuApiTarget::Metal4 => {
            let (sync_before, sync_after, access_before) = match *barrier {
                Barrier::Buffer(bb) => (bb.sync_before, bb.sync_after, bb.access_before),
                Barrier::Texture(tb) => (tb.sync_before, tb.sync_after, tb.access_before),
            };
            if sync_after.contains(Sync::SPLIT) {
                return None;
            }
            let effective_before = if sync_before.contains(Sync::SPLIT) {
                split_producer_sync.unwrap_or(Sync::COMPUTE_SHADING)
            } else {
                sync_before
            };
            if effective_before == Sync::NONE
                && (access_before == Access::NO_ACCESS || access_before == Access::COMMON)
            {
                return None;
            }
            let after_stages_mask = mtl4_stage_bits(effective_before);
            let before_stages_mask = mtl4_stage_bits(sync_after);
            if after_stages_mask == 0 || before_stages_mask == 0 {
                return None;
            }
            Some(NativeBackendBarrier::Metal4StageBarrier {
                after_stages_mask,
                before_stages_mask,
            })
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SplitKey {
    Buffer(BufferHandle),
    Texture(TextureHandle, SubresourceRangeKey),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SubresourceRangeKey {
    first_mip: u32,
    mip_count: u32,
    first_slice: u32,
    slice_count: u32,
    first_plane: u32,
    plane_count: u32,
}

impl From<SubresourceRange> for SubresourceRangeKey {
    fn from(r: SubresourceRange) -> Self {
        Self {
            first_mip: r.first_mip,
            mip_count: r.mip_count,
            first_slice: r.first_slice,
            slice_count: r.slice_count,
            first_plane: r.first_plane,
            plane_count: r.plane_count,
        }
    }
}

fn barrier_split_key(barrier: &Barrier) -> SplitKey {
    match *barrier {
        Barrier::Buffer(bb) => SplitKey::Buffer(bb.resource),
        Barrier::Texture(tb) => SplitKey::Texture(tb.resource, tb.subresources.into()),
    }
}

/// Lower every queue stream and barrier batch in a [`Plan`] to `target`.
///
/// Split barrier begin/end pairs on the same queue and resource are assigned
/// matching monotonic `event_id`s for [`GpuApiTarget::Vulkan14`], and the
/// producer's `sync_before` scope is preserved for the consuming stage
/// barrier on [`GpuApiTarget::Metal4`].
pub fn lower_plan(target: GpuApiTarget, plan: &Plan) -> LoweredPlan {
    let mut next_event_id: u32 = 1;
    let mut queues = Vec::with_capacity(plan.queues.len());

    for stream in &plan.queues {
        let mut active_splits: BTreeMap<SplitKey, (u32, Sync)> = BTreeMap::new();
        let mut commands = Vec::with_capacity(stream.commands.len());

        for cmd in &stream.commands {
            match cmd {
                Command::Wait { queue, value } => commands.push(LoweredCommand::Wait {
                    queue: *queue,
                    value: *value,
                }),
                Command::Execute(pass) => commands.push(LoweredCommand::Execute(*pass)),
                Command::Signal { value } => {
                    commands.push(LoweredCommand::Signal { value: *value })
                }
                Command::Barriers(barriers) => {
                    let mut lowered_batch = Vec::new();
                    for b in barriers {
                        let (sync_before, sync_after) = match *b {
                            Barrier::Buffer(bb) => (bb.sync_before, bb.sync_after),
                            Barrier::Texture(tb) => (tb.sync_before, tb.sync_after),
                        };
                        let key = barrier_split_key(b);
                        let (event_id, producer_sync) = if sync_after.contains(Sync::SPLIT) {
                            let id = next_event_id;
                            next_event_id = next_event_id.saturating_add(1);
                            active_splits.insert(key, (id, sync_before));
                            (id, None)
                        } else if sync_before.contains(Sync::SPLIT) {
                            if let Some((id, prod_sync)) = active_splits.remove(&key) {
                                (id, Some(prod_sync))
                            } else {
                                let id = next_event_id;
                                next_event_id = next_event_id.saturating_add(1);
                                (id, None)
                            }
                        } else {
                            (0, None)
                        };

                        if let Some(native) =
                            lower_barrier_with_split_hint(target, b, event_id, producer_sync)
                        {
                            lowered_batch.push(native);
                        }
                    }
                    if !lowered_batch.is_empty() {
                        commands.push(LoweredCommand::Barriers(lowered_batch));
                    }
                }
            }
        }

        queues.push(LoweredQueueStream {
            queue: stream.queue,
            kind: stream.kind,
            commands,
        });
    }

    LoweredPlan {
        target,
        order: plan.order.clone(),
        queues,
    }
}

/// Convert Ignis/D3D12 [`Sync`] scope bits to `VkPipelineStageFlags2` bits.
pub const fn vk_stage_bits(sync: Sync) -> u64 {
    let bits = sync.bits() & !Sync::SPLIT.bits();
    let mut out: u64 = 0;

    if bits & Sync::ALL.bits() != 0 {
        out |= 1 << 16; // VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT
    }
    if bits & Sync::DRAW.bits() != 0 {
        out |= (1 << 1) | (1 << 15); // VERTEX_INPUT | ALL_GRAPHICS
    }
    if bits & Sync::INDEX_INPUT.bits() != 0 {
        out |= 1 << 1; // VERTEX_INPUT
    }
    if bits & Sync::VERTEX_SHADING.bits() != 0 {
        out |= 1 << 3; // VERTEX_SHADER
    }
    if bits & Sync::PIXEL_SHADING.bits() != 0 {
        out |= 1 << 7; // FRAGMENT_SHADER
    }
    if bits & Sync::DEPTH_STENCIL.bits() != 0 {
        out |= (1 << 8) | (1 << 9); // EARLY_FRAGMENT_TESTS | LATE_FRAGMENT_TESTS
    }
    if bits & Sync::RENDER_TARGET.bits() != 0 {
        out |= 1 << 10; // COLOR_ATTACHMENT_OUTPUT
    }
    if bits & Sync::COMPUTE_SHADING.bits() != 0 {
        out |= 1 << 11; // COMPUTE_SHADER
    }
    if bits
        & (Sync::RAYTRACING.bits()
            | Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE.bits()
            | Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE.bits()
            | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits())
        != 0
    {
        out |= 1 << 21; // RAY_TRACING_SHADER_BIT_KHR
    }
    if bits & Sync::COPY.bits() != 0 {
        out |= 1 << 12; // ALL_TRANSFER
    }
    if bits & Sync::RESOLVE.bits() != 0 {
        out |= 1 << 34; // RESOLVE_BIT
    }
    if bits & Sync::EXECUTE_INDIRECT.bits() != 0 {
        out |= 1 << 0; // DRAW_INDIRECT
    }
    if bits & Sync::ALL_SHADING.bits() != 0 {
        out |= (1 << 3) | (1 << 7) | (1 << 11);
    }
    if bits & Sync::NON_PIXEL_SHADING.bits() != 0 {
        out |= (1 << 3) | (1 << 11);
    }
    if bits & Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits() != 0 {
        out |= 1 << 36; // CLEAR_BIT
    }

    out
}

/// Convert Ignis/D3D12 [`Access`] bits to `VkAccessFlags2` bits.
pub const fn vk_access_bits(acc: Access) -> u64 {
    if acc.bits() == Access::NO_ACCESS.bits() {
        return 0;
    }
    let bits = acc.bits();
    let mut out: u64 = 0;

    if bits == Access::COMMON.bits() {
        return (1 << 15) | (1 << 16); // MEMORY_READ | MEMORY_WRITE
    }
    if bits & Access::INDIRECT_ARGUMENT.bits() != 0 {
        out |= 1 << 0; // INDIRECT_COMMAND_READ
    }
    if bits & Access::INDEX_BUFFER.bits() != 0 {
        out |= 1 << 1; // INDEX_READ
    }
    if bits & Access::VERTEX_BUFFER.bits() != 0 {
        out |= 1 << 2; // VERTEX_ATTRIBUTE_READ
    }
    if bits & Access::CONSTANT_BUFFER.bits() != 0 {
        out |= 1 << 3; // UNIFORM_READ
    }
    if bits & Access::SHADER_RESOURCE.bits() != 0 {
        out |= 1 << 5; // SHADER_READ
    }
    if bits & Access::UNORDERED_ACCESS.bits() != 0 {
        out |= (1 << 5) | (1 << 6); // SHADER_READ | SHADER_WRITE
    }
    if bits & Access::RENDER_TARGET.bits() != 0 {
        out |= (1 << 7) | (1 << 8); // COLOR_ATTACHMENT_READ | COLOR_ATTACHMENT_WRITE
    }
    if bits & Access::DEPTH_STENCIL_READ.bits() != 0 {
        out |= 1 << 9; // DEPTH_STENCIL_ATTACHMENT_READ
    }
    if bits & Access::DEPTH_STENCIL_WRITE.bits() != 0 {
        out |= (1 << 9) | (1 << 10); // DEPTH_STENCIL_ATTACHMENT_READ | WRITE
    }
    if bits & (Access::COPY_SOURCE.bits() | Access::RESOLVE_SOURCE.bits()) != 0 {
        out |= 1 << 11; // TRANSFER_READ
    }
    if bits & (Access::COPY_DEST.bits() | Access::RESOLVE_DEST.bits()) != 0 {
        out |= 1 << 12; // TRANSFER_WRITE
    }
    if bits & Access::RAYTRACING_ACCELERATION_STRUCTURE_READ.bits() != 0 {
        out |= 1 << 21; // ACCELERATION_STRUCTURE_READ_KHR
    }
    if bits & Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE.bits() != 0 {
        out |= 1 << 22; // ACCELERATION_STRUCTURE_WRITE_KHR
    }

    out
}

/// Convert Ignis/D3D12 [`Sync`] scope bits to Metal 4 `MTL4Stages` mask bits.
///
/// Bit 0 = render/graphics (`Vertex | Fragment | RenderTarget | DepthStencil`),
/// bit 1 = compute/raytracing dispatch, bit 2 = blit/copy.
pub const fn mtl4_stage_bits(sync: Sync) -> u32 {
    let bits = sync.bits() & !Sync::SPLIT.bits();
    if bits == 0 {
        return 0;
    }
    let mut out: u32 = 0;

    let render_mask = Sync::DRAW.bits()
        | Sync::INDEX_INPUT.bits()
        | Sync::VERTEX_SHADING.bits()
        | Sync::PIXEL_SHADING.bits()
        | Sync::DEPTH_STENCIL.bits()
        | Sync::RENDER_TARGET.bits()
        | Sync::RESOLVE.bits()
        | Sync::EXECUTE_INDIRECT.bits();
    let compute_mask = Sync::COMPUTE_SHADING.bits()
        | Sync::RAYTRACING.bits()
        | Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits()
        | Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE.bits()
        | Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE.bits()
        | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits();
    let blit_mask = Sync::COPY.bits();

    if bits & render_mask != 0 {
        out |= 1 << 0;
    }
    if bits & compute_mask != 0 {
        out |= 1 << 1;
    }
    if bits & blit_mask != 0 {
        out |= 1 << 2;
    }
    if bits & Sync::ALL_SHADING.bits() != 0 {
        out |= (1 << 0) | (1 << 1);
    }
    if bits & Sync::NON_PIXEL_SHADING.bits() != 0 {
        out |= (1 << 0) | (1 << 1);
    }
    if bits & Sync::ALL.bits() != 0 {
        out |= (1 << 0) | (1 << 1) | (1 << 2);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{PassDesc, RenderGraph};
    use crate::plan::TextureBarrier;

    #[test]
    fn metal4_elides_layout_only_transition_but_keeps_hazard() {
        let mut g = RenderGraph::new();
        let direct = g.add_queue(QueueKind::Direct);
        let tex = g
            .add_texture(TextureDesc::simple("color", Layout::Undefined))
            .unwrap();
        g.add_pass(PassDesc::new("draw", direct).texture(
            tex,
            SubresourceRange::ALL,
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
        g.add_pass(PassDesc::new("post", direct).texture(
            tex,
            SubresourceRange::ALL,
            Access::SHADER_RESOURCE,
            Sync::PIXEL_SHADING,
            Layout::ShaderResource,
        ))
        .unwrap();
        let plan = g.plan().unwrap();
        assert_eq!(plan.barrier_count(), 2);

        let d3d = lower_plan(GpuApiTarget::D3D12Agility1620, &plan);
        assert_eq!(d3d.barrier_count(), 2);

        let mtl = lower_plan(GpuApiTarget::Metal4, &plan);
        // Undefined -> RenderTarget layout-only transition is elided; only the RAW barrier remains.
        assert_eq!(mtl.barrier_count(), 1);
        let emitted = mtl.queues[0]
            .commands
            .iter()
            .find_map(|c| match c {
                LoweredCommand::Barriers(bs) => bs.first().copied(),
                _ => None,
            })
            .expect("RAW barrier emitted for Metal4");
        assert_eq!(
            emitted,
            NativeBackendBarrier::Metal4StageBarrier {
                after_stages_mask: 1 << 0,
                before_stages_mask: 1 << 0,
            }
        );
    }

    #[test]
    fn vulkan14_maps_texture_barrier_bits() {
        let mut g = RenderGraph::new();
        let direct = g.add_queue(QueueKind::Direct);
        let tex = g
            .add_texture(TextureDesc::simple("rt", Layout::RenderTarget))
            .unwrap();
        let barrier = Barrier::Texture(TextureBarrier {
            resource: tex,
            sync_before: Sync::COMPUTE_SHADING,
            sync_after: Sync::PIXEL_SHADING,
            access_before: Access::UNORDERED_ACCESS,
            access_after: Access::SHADER_RESOURCE,
            layout_before: Layout::UnorderedAccess,
            layout_after: Layout::ShaderResource,
            subresources: SubresourceRange::ALL,
        });
        let vk = lower_to_target_barrier(GpuApiTarget::Vulkan14, &barrier, 0).unwrap();
        assert_eq!(
            vk,
            NativeBackendBarrier::Vulkan14ImageBarrier2 {
                resource: tex,
                subresources: SubresourceRange::ALL,
                src_stage_mask: 1 << 11,
                dst_stage_mask: 1 << 7,
                src_access_mask: (1 << 5) | (1 << 6),
                dst_access_mask: 1 << 5,
                old_layout: Layout::UnorderedAccess,
                new_layout: Layout::ShaderResource,
            }
        );
        // Keep `g` live without unused warning.
        let _ = direct;
    }

    use crate::graph::TextureDesc;
}
