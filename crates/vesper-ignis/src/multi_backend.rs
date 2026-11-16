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

/// Native Vulkan `VkPipelineStageFlagBits2` bit constants (`vulkan_core.h`).
const VK_STAGE_DRAW_INDIRECT: u64 = 1 << 1;
const VK_STAGE_VERTEX_INPUT: u64 = 1 << 2;
const VK_STAGE_VERTEX_SHADER: u64 = 1 << 3;
const VK_STAGE_FRAGMENT_SHADER: u64 = 1 << 7;
const VK_STAGE_EARLY_FRAGMENT_TESTS: u64 = 1 << 8;
const VK_STAGE_LATE_FRAGMENT_TESTS: u64 = 1 << 9;
const VK_STAGE_COLOR_ATTACHMENT_OUTPUT: u64 = 1 << 10;
const VK_STAGE_COMPUTE_SHADER: u64 = 1 << 11;
const VK_STAGE_ALL_TRANSFER: u64 = 1 << 12;
const VK_STAGE_ALL_GRAPHICS: u64 = 1 << 15;
const VK_STAGE_ALL_COMMANDS: u64 = 1 << 16;
const VK_STAGE_RAY_TRACING_SHADER_KHR: u64 = 1 << 21;
const VK_STAGE_FRAGMENT_SHADING_RATE_ATTACHMENT_KHR: u64 = 1 << 22;
const VK_STAGE_TRANSFORM_FEEDBACK_EXT: u64 = 1 << 24;
const VK_STAGE_ACCELERATION_STRUCTURE_BUILD_KHR: u64 = 1 << 25;
const VK_STAGE_ACCELERATION_STRUCTURE_COPY_KHR: u64 = 1 << 28;
const VK_STAGE_RESOLVE: u64 = 1 << 33;
const VK_STAGE_CLEAR: u64 = 1 << 35;
const VK_STAGE_VERTEX_ATTRIBUTE_INPUT: u64 = 1 << 37;
const VK_STAGE_PRE_RASTERIZATION_SHADERS: u64 = 1 << 38;

/// Native Vulkan `VkAccessFlagBits2` bit constants (`vulkan_core.h`).
const VK_ACCESS_INDIRECT_COMMAND_READ: u64 = 1 << 0;
const VK_ACCESS_INDEX_READ: u64 = 1 << 1;
const VK_ACCESS_VERTEX_ATTRIBUTE_READ: u64 = 1 << 2;
const VK_ACCESS_UNIFORM_READ: u64 = 1 << 3;
const VK_ACCESS_SHADER_READ: u64 = 1 << 5;
const VK_ACCESS_SHADER_WRITE: u64 = 1 << 6;
const VK_ACCESS_COLOR_ATTACHMENT_READ: u64 = 1 << 7;
const VK_ACCESS_COLOR_ATTACHMENT_WRITE: u64 = 1 << 8;
const VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ: u64 = 1 << 9;
const VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE: u64 = 1 << 10;
const VK_ACCESS_TRANSFER_READ: u64 = 1 << 11;
const VK_ACCESS_TRANSFER_WRITE: u64 = 1 << 12;
const VK_ACCESS_MEMORY_READ: u64 = 1 << 15;
const VK_ACCESS_MEMORY_WRITE: u64 = 1 << 16;
const VK_ACCESS_ACCELERATION_STRUCTURE_READ_KHR: u64 = 1 << 21;
const VK_ACCESS_ACCELERATION_STRUCTURE_WRITE_KHR: u64 = 1 << 22;
const VK_ACCESS_FRAGMENT_SHADING_RATE_ATTACHMENT_READ_KHR: u64 = 1 << 23;
const VK_ACCESS_TRANSFORM_FEEDBACK_WRITE_EXT: u64 = 1 << 25;

/// Native Apple Metal 4 `MTLStages` bit constants (`MTLCommandEncoder.hpp`).
const MTL_STAGE_VERTEX: u32 = 1 << 0;
const MTL_STAGE_FRAGMENT: u32 = 1 << 1;
const MTL_STAGE_TILE: u32 = 1 << 2;
const MTL_STAGE_DISPATCH: u32 = 1 << 27;
const MTL_STAGE_BLIT: u32 = 1 << 28;
const MTL_STAGE_ACCELERATION_STRUCTURE: u32 = 1 << 29;

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
    lower_barrier_with_split_hint(target, QueueKind::Direct, barrier, split_event_id, None)
}

/// Lower a single planner [`Barrier`] on a specific [`QueueKind`].
///
/// On [`GpuApiTarget::Vulkan14`] with [`QueueKind::Copy`] (transfer-only queue
/// families where `vkCmdSetEvent2`/`vkCmdWaitEvents2` are invalid), split
/// barrier halves are collapsed into ordinary barriers using the producer's
/// sync scope at the consumer.
pub fn lower_to_target_barrier_for_queue(
    target: GpuApiTarget,
    queue_kind: QueueKind,
    barrier: &Barrier,
    split_event_id: u32,
) -> Option<NativeBackendBarrier> {
    lower_barrier_with_split_hint(target, queue_kind, barrier, split_event_id, None)
}

fn vk_stage_bits_for_access(sync: Sync, acc: Access) -> u64 {
    let mut out = vk_stage_bits(sync);
    if out != 0 {
        if acc.bits() & Access::STREAM_OUTPUT.bits() != 0 {
            out |= VK_STAGE_TRANSFORM_FEEDBACK_EXT;
        }
        if acc.bits() & Access::SHADING_RATE_SOURCE.bits() != 0 {
            out |= VK_STAGE_FRAGMENT_SHADING_RATE_ATTACHMENT_KHR;
        }
    }
    out
}

fn lower_barrier_with_split_hint(
    target: GpuApiTarget,
    queue_kind: QueueKind,
    barrier: &Barrier,
    split_event_id: u32,
    split_producer_sync: Option<Sync>,
) -> Option<NativeBackendBarrier> {
    match target {
        GpuApiTarget::D3D12Agility1620 => Some(NativeBackendBarrier::D3D12(*barrier)),
        GpuApiTarget::Vulkan14 => {
            let collapse_splits = queue_kind == QueueKind::Copy;
            match *barrier {
                Barrier::Buffer(bb) => {
                    if bb.sync_after.contains(Sync::SPLIT) {
                        if collapse_splits {
                            None
                        } else {
                            Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
                                event_id: split_event_id,
                                resource: ResourceRef::Buffer(bb.resource),
                                src_stage_mask: vk_stage_bits_for_access(
                                    bb.sync_before,
                                    bb.access_before,
                                ),
                            })
                        }
                    } else if bb.sync_before.contains(Sync::SPLIT) && !collapse_splits {
                        let effective_before = split_producer_sync.unwrap_or(Sync::ALL);
                        let dst_stage_mask =
                            vk_stage_bits_for_access(bb.sync_after, bb.access_after);
                        Some(NativeBackendBarrier::Vulkan14SplitEventWait {
                            event_id: split_event_id,
                            resource: ResourceRef::Buffer(bb.resource),
                            dst_stage_mask,
                            src_access_mask: vk_access_bits_for_sync(
                                bb.access_before,
                                effective_before,
                            ),
                            dst_access_mask: if dst_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(bb.access_after, bb.sync_after)
                            },
                            old_layout: None,
                            new_layout: None,
                            subresources: None,
                        })
                    } else {
                        let effective_before = if bb.sync_before.contains(Sync::SPLIT) {
                            split_producer_sync.unwrap_or(Sync::ALL)
                        } else {
                            bb.sync_before
                        };
                        let src_stage_mask =
                            vk_stage_bits_for_access(effective_before, bb.access_before);
                        let dst_stage_mask =
                            vk_stage_bits_for_access(bb.sync_after, bb.access_after);
                        Some(NativeBackendBarrier::Vulkan14BufferBarrier2 {
                            resource: bb.resource,
                            src_stage_mask,
                            dst_stage_mask,
                            src_access_mask: if src_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(bb.access_before, effective_before)
                            },
                            dst_access_mask: if dst_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(bb.access_after, bb.sync_after)
                            },
                        })
                    }
                }
                Barrier::Texture(tb) => {
                    if tb.sync_after.contains(Sync::SPLIT) {
                        if collapse_splits {
                            None
                        } else {
                            Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
                                event_id: split_event_id,
                                resource: ResourceRef::Texture(tb.resource),
                                src_stage_mask: vk_stage_bits_for_access(
                                    tb.sync_before,
                                    tb.access_before,
                                ),
                            })
                        }
                    } else if tb.sync_before.contains(Sync::SPLIT) && !collapse_splits {
                        let effective_before = split_producer_sync.unwrap_or(Sync::ALL);
                        let dst_stage_mask =
                            vk_stage_bits_for_access(tb.sync_after, tb.access_after);
                        Some(NativeBackendBarrier::Vulkan14SplitEventWait {
                            event_id: split_event_id,
                            resource: ResourceRef::Texture(tb.resource),
                            dst_stage_mask,
                            src_access_mask: vk_access_bits_for_sync(
                                tb.access_before,
                                effective_before,
                            ),
                            dst_access_mask: if dst_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(tb.access_after, tb.sync_after)
                            },
                            old_layout: Some(tb.layout_before),
                            new_layout: Some(tb.layout_after),
                            subresources: Some(tb.subresources),
                        })
                    } else {
                        let effective_before = if tb.sync_before.contains(Sync::SPLIT) {
                            split_producer_sync.unwrap_or(Sync::ALL)
                        } else {
                            tb.sync_before
                        };
                        let src_stage_mask =
                            vk_stage_bits_for_access(effective_before, tb.access_before);
                        let dst_stage_mask =
                            vk_stage_bits_for_access(tb.sync_after, tb.access_after);
                        Some(NativeBackendBarrier::Vulkan14ImageBarrier2 {
                            resource: tb.resource,
                            subresources: tb.subresources,
                            src_stage_mask,
                            dst_stage_mask,
                            src_access_mask: if src_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(tb.access_before, effective_before)
                            },
                            dst_access_mask: if dst_stage_mask == 0 {
                                0
                            } else {
                                vk_access_bits_for_sync(tb.access_after, tb.sync_after)
                            },
                            old_layout: tb.layout_before,
                            new_layout: tb.layout_after,
                        })
                    }
                }
            }
        }
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
/// matching monotonic `event_id`s for [`GpuApiTarget::Vulkan14`] on graphics
/// and compute queues, collapsed into ordinary synchronization2 barriers on
/// [`QueueKind::Copy`] (transfer-only Vulkan queues forbid `vkCmdSetEvent2`),
/// and merged into producer-to-consumer stage barriers on
/// [`GpuApiTarget::Metal4`].
pub fn lower_plan(target: GpuApiTarget, plan: &Plan) -> LoweredPlan {
    let mut next_event_id: u32 = 1;
    let mut queues = Vec::with_capacity(plan.queues.len());

    for stream in &plan.queues {
        let mut active_splits: BTreeMap<SplitKey, (u32, Sync)> = BTreeMap::new();
        let mut commands = Vec::with_capacity(stream.commands.len());
        let needs_vulkan_event_ids =
            target == GpuApiTarget::Vulkan14 && stream.kind != QueueKind::Copy;

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
                            let id = if needs_vulkan_event_ids {
                                let id = next_event_id;
                                next_event_id = next_event_id.saturating_add(1);
                                id
                            } else {
                                0
                            };
                            active_splits.insert(key, (id, sync_before));
                            (id, None)
                        } else if sync_before.contains(Sync::SPLIT) {
                            if let Some((id, prod_sync)) = active_splits.remove(&key) {
                                (id, Some(prod_sync))
                            } else if needs_vulkan_event_ids {
                                let id = next_event_id;
                                next_event_id = next_event_id.saturating_add(1);
                                (id, None)
                            } else {
                                (0, None)
                            }
                        } else {
                            (0, None)
                        };

                        if let Some(native) = lower_barrier_with_split_hint(
                            target,
                            stream.kind,
                            b,
                            event_id,
                            producer_sync,
                        ) {
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
    match try_vk_stage_bits(sync) {
        Ok(bits) => bits,
        Err(_) => vk_stage_bits_lossy(sync),
    }
}

const fn vk_stage_bits_lossy(sync: Sync) -> u64 {
    let bits = sync.bits() & !Sync::SPLIT.bits();
    let mut out: u64 = 0;

    if bits & Sync::ALL.bits() != 0 {
        out |= VK_STAGE_ALL_COMMANDS;
    }
    if bits & Sync::DRAW.bits() != 0 {
        out |= VK_STAGE_ALL_GRAPHICS;
    }
    if bits & Sync::INDEX_INPUT.bits() != 0 {
        out |= VK_STAGE_VERTEX_INPUT;
    }
    if bits & Sync::VERTEX_SHADING.bits() != 0 {
        out |= VK_STAGE_VERTEX_SHADER | VK_STAGE_VERTEX_ATTRIBUTE_INPUT;
    }
    if bits & Sync::PIXEL_SHADING.bits() != 0 {
        out |= VK_STAGE_FRAGMENT_SHADER;
    }
    if bits & Sync::DEPTH_STENCIL.bits() != 0 {
        out |= VK_STAGE_EARLY_FRAGMENT_TESTS | VK_STAGE_LATE_FRAGMENT_TESTS;
    }
    if bits & Sync::RENDER_TARGET.bits() != 0 {
        out |= VK_STAGE_COLOR_ATTACHMENT_OUTPUT;
    }
    if bits & Sync::COMPUTE_SHADING.bits() != 0 {
        out |= VK_STAGE_COMPUTE_SHADER;
    }
    if bits & Sync::RAYTRACING.bits() != 0 {
        out |= VK_STAGE_RAY_TRACING_SHADER_KHR;
    }
    if bits & Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE.bits() != 0 {
        out |= VK_STAGE_ACCELERATION_STRUCTURE_BUILD_KHR;
    }
    if bits
        & (Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE.bits()
            | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits())
        != 0
    {
        out |= VK_STAGE_ACCELERATION_STRUCTURE_COPY_KHR;
    }
    if bits & Sync::COPY.bits() != 0 {
        out |= VK_STAGE_ALL_TRANSFER;
    }
    if bits & Sync::RESOLVE.bits() != 0 {
        out |= VK_STAGE_RESOLVE;
    }
    if bits & Sync::EXECUTE_INDIRECT.bits() != 0 {
        out |= VK_STAGE_DRAW_INDIRECT;
    }
    if bits & Sync::ALL_SHADING.bits() != 0 {
        out |= VK_STAGE_PRE_RASTERIZATION_SHADERS
            | VK_STAGE_VERTEX_ATTRIBUTE_INPUT
            | VK_STAGE_FRAGMENT_SHADER
            | VK_STAGE_COMPUTE_SHADER;
    }
    if bits & Sync::NON_PIXEL_SHADING.bits() != 0 {
        out |= VK_STAGE_PRE_RASTERIZATION_SHADERS
            | VK_STAGE_VERTEX_ATTRIBUTE_INPUT
            | VK_STAGE_COMPUTE_SHADER;
    }
    if bits & Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits() != 0 {
        out |= VK_STAGE_CLEAR;
    }

    out
}

/// Convert Ignis/D3D12 [`Sync`] scope bits to `VkPipelineStageFlags2` bits,
/// returning `Err(unmapped)` if any unknown sync bit is present.
pub const fn try_vk_stage_bits(sync: Sync) -> Result<u64, Sync> {
    const MAPPED_SYNC: u32 = Sync::ALL.bits()
        | Sync::DRAW.bits()
        | Sync::INDEX_INPUT.bits()
        | Sync::VERTEX_SHADING.bits()
        | Sync::PIXEL_SHADING.bits()
        | Sync::DEPTH_STENCIL.bits()
        | Sync::RENDER_TARGET.bits()
        | Sync::COMPUTE_SHADING.bits()
        | Sync::RAYTRACING.bits()
        | Sync::COPY.bits()
        | Sync::RESOLVE.bits()
        | Sync::EXECUTE_INDIRECT.bits()
        | Sync::ALL_SHADING.bits()
        | Sync::NON_PIXEL_SHADING.bits()
        | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits()
        | Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits()
        | Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE.bits()
        | Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE.bits();

    let rest = (sync.bits() & !Sync::SPLIT.bits()) & !MAPPED_SYNC;
    if rest != 0 {
        return Err(Sync::from_bits(rest));
    }
    Ok(vk_stage_bits_lossy(sync))
}

/// Convert Ignis/D3D12 [`Access`] bits to `VkAccessFlags2` bits.
pub const fn vk_access_bits(acc: Access) -> u64 {
    match try_vk_access_bits(acc) {
        Ok(bits) => bits,
        Err(_) => vk_access_bits_lossy(acc),
    }
}

/// Convert Ignis/D3D12 [`Access`] bits to `VkAccessFlags2` bits for a specific
/// [`Sync`] scope.
///
/// Operations executed outside shader stages—such as
/// [`Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO`]
/// (`ACCELERATION_STRUCTURE_COPY_KHR`) and
/// [`Sync::CLEAR_UNORDERED_ACCESS_VIEW`] (`CLEAR`)—write their UAV output via
/// `VK_ACCESS_TRANSFER_WRITE`, not `VK_ACCESS_SHADER_READ | VK_ACCESS_SHADER_WRITE`.
pub const fn vk_access_bits_for_sync(acc: Access, sync: Sync) -> u64 {
    let base = vk_access_bits(acc);
    if (acc.bits() & Access::UNORDERED_ACCESS.bits()) == 0
        || acc.bits() == Access::COMMON.bits()
        || acc.bits() == Access::NO_ACCESS.bits()
    {
        return base;
    }
    let sync_bits = sync.bits() & !Sync::SPLIT.bits();
    let transfer_uav_scope = Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits()
        | Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits();
    if sync_bits & transfer_uav_scope == 0 {
        return base;
    }
    let shader_or_all_scope = Sync::ALL.bits()
        | Sync::DRAW.bits()
        | Sync::VERTEX_SHADING.bits()
        | Sync::PIXEL_SHADING.bits()
        | Sync::COMPUTE_SHADING.bits()
        | Sync::RAYTRACING.bits()
        | Sync::ALL_SHADING.bits()
        | Sync::NON_PIXEL_SHADING.bits();
    let mut adjusted = base;
    if (sync_bits & shader_or_all_scope) == 0 {
        let remaining = acc.bits() & !Access::UNORDERED_ACCESS.bits();
        adjusted = if remaining == 0 {
            0
        } else {
            vk_access_bits_lossy(Access::from_bits(remaining))
        };
    }
    adjusted | VK_ACCESS_TRANSFER_WRITE
}

const fn vk_access_bits_lossy(acc: Access) -> u64 {
    if acc.bits() == Access::NO_ACCESS.bits() {
        return 0;
    }
    let bits = acc.bits();
    if bits == Access::COMMON.bits() {
        return VK_ACCESS_MEMORY_READ | VK_ACCESS_MEMORY_WRITE;
    }
    let mut out: u64 = 0;

    if bits & Access::INDIRECT_ARGUMENT.bits() != 0 {
        out |= VK_ACCESS_INDIRECT_COMMAND_READ;
    }
    if bits & Access::INDEX_BUFFER.bits() != 0 {
        out |= VK_ACCESS_INDEX_READ;
    }
    if bits & Access::VERTEX_BUFFER.bits() != 0 {
        out |= VK_ACCESS_VERTEX_ATTRIBUTE_READ;
    }
    if bits & Access::CONSTANT_BUFFER.bits() != 0 {
        out |= VK_ACCESS_UNIFORM_READ;
    }
    if bits & Access::SHADER_RESOURCE.bits() != 0 {
        out |= VK_ACCESS_SHADER_READ;
    }
    if bits & Access::UNORDERED_ACCESS.bits() != 0 {
        out |= VK_ACCESS_SHADER_READ | VK_ACCESS_SHADER_WRITE;
    }
    if bits & Access::RENDER_TARGET.bits() != 0 {
        out |= VK_ACCESS_COLOR_ATTACHMENT_READ | VK_ACCESS_COLOR_ATTACHMENT_WRITE;
    }
    if bits & Access::DEPTH_STENCIL_READ.bits() != 0 {
        out |= VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ;
    }
    if bits & Access::DEPTH_STENCIL_WRITE.bits() != 0 {
        out |= VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ | VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE;
    }
    if bits & Access::STREAM_OUTPUT.bits() != 0 {
        out |= VK_ACCESS_TRANSFORM_FEEDBACK_WRITE_EXT;
    }
    if bits & (Access::COPY_SOURCE.bits() | Access::RESOLVE_SOURCE.bits()) != 0 {
        out |= VK_ACCESS_TRANSFER_READ;
    }
    if bits & (Access::COPY_DEST.bits() | Access::RESOLVE_DEST.bits()) != 0 {
        out |= VK_ACCESS_TRANSFER_WRITE;
    }
    if bits & Access::RAYTRACING_ACCELERATION_STRUCTURE_READ.bits() != 0 {
        out |= VK_ACCESS_ACCELERATION_STRUCTURE_READ_KHR;
    }
    if bits & Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE.bits() != 0 {
        out |= VK_ACCESS_ACCELERATION_STRUCTURE_WRITE_KHR;
    }
    if bits & Access::SHADING_RATE_SOURCE.bits() != 0 {
        out |= VK_ACCESS_FRAGMENT_SHADING_RATE_ATTACHMENT_READ_KHR;
    }

    out
}

/// Convert Ignis/D3D12 [`Access`] bits to `VkAccessFlags2` bits, returning
/// `Err(unmapped)` if any unsupported or unknown access bit is present.
pub const fn try_vk_access_bits(acc: Access) -> Result<u64, Access> {
    if acc.bits() == Access::NO_ACCESS.bits() || acc.bits() == Access::COMMON.bits() {
        return Ok(vk_access_bits_lossy(acc));
    }
    const MAPPED_ACCESS: u32 = Access::VERTEX_BUFFER.bits()
        | Access::CONSTANT_BUFFER.bits()
        | Access::INDEX_BUFFER.bits()
        | Access::RENDER_TARGET.bits()
        | Access::UNORDERED_ACCESS.bits()
        | Access::DEPTH_STENCIL_WRITE.bits()
        | Access::DEPTH_STENCIL_READ.bits()
        | Access::SHADER_RESOURCE.bits()
        | Access::STREAM_OUTPUT.bits()
        | Access::INDIRECT_ARGUMENT.bits()
        | Access::COPY_DEST.bits()
        | Access::COPY_SOURCE.bits()
        | Access::RESOLVE_DEST.bits()
        | Access::RESOLVE_SOURCE.bits()
        | Access::RAYTRACING_ACCELERATION_STRUCTURE_READ.bits()
        | Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE.bits()
        | Access::SHADING_RATE_SOURCE.bits();

    let rest = acc.bits() & !MAPPED_ACCESS;
    if rest != 0 {
        return Err(Access::from_bits(rest));
    }
    Ok(vk_access_bits_lossy(acc))
}

/// Convert Ignis/D3D12 [`Sync`] scope bits to native Apple Metal 4 `MTLStages`
/// bitmask (`MTLStageVertex` = `1 << 0`, `MTLStageFragment` = `1 << 1`,
/// `MTLStageTile` = `1 << 2`, `MTLStageDispatch` = `1 << 27`,
/// `MTLStageBlit` = `1 << 28`, `MTLStageAccelerationStructure` = `1 << 29`).
pub const fn mtl4_stage_bits(sync: Sync) -> u32 {
    let bits = sync.bits() & !Sync::SPLIT.bits();
    if bits == 0 {
        return 0;
    }
    let mut out: u32 = 0;

    if bits & Sync::ALL.bits() != 0 {
        out |= MTL_STAGE_VERTEX
            | MTL_STAGE_FRAGMENT
            | MTL_STAGE_TILE
            | MTL_STAGE_DISPATCH
            | MTL_STAGE_BLIT
            | MTL_STAGE_ACCELERATION_STRUCTURE;
    }
    if bits & Sync::DRAW.bits() != 0 {
        out |= MTL_STAGE_VERTEX | MTL_STAGE_FRAGMENT | MTL_STAGE_TILE;
    }
    if bits & (Sync::INDEX_INPUT.bits() | Sync::VERTEX_SHADING.bits()) != 0 {
        out |= MTL_STAGE_VERTEX;
    }
    if bits & (Sync::PIXEL_SHADING.bits() | Sync::DEPTH_STENCIL.bits() | Sync::RENDER_TARGET.bits())
        != 0
    {
        out |= MTL_STAGE_FRAGMENT;
    }
    if bits & Sync::RESOLVE.bits() != 0 {
        out |= MTL_STAGE_FRAGMENT | MTL_STAGE_BLIT;
    }
    if bits & Sync::EXECUTE_INDIRECT.bits() != 0 {
        out |= MTL_STAGE_VERTEX | MTL_STAGE_DISPATCH;
    }
    if bits & (Sync::COMPUTE_SHADING.bits() | Sync::RAYTRACING.bits()) != 0 {
        out |= MTL_STAGE_DISPATCH;
    }
    if bits & Sync::CLEAR_UNORDERED_ACCESS_VIEW.bits() != 0 {
        out |= MTL_STAGE_DISPATCH | MTL_STAGE_BLIT;
    }
    if bits & Sync::COPY.bits() != 0 {
        out |= MTL_STAGE_BLIT;
    }
    if bits
        & (Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE.bits()
            | Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE.bits()
            | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO.bits())
        != 0
    {
        out |= MTL_STAGE_ACCELERATION_STRUCTURE;
    }
    if bits & Sync::ALL_SHADING.bits() != 0 {
        out |= MTL_STAGE_VERTEX | MTL_STAGE_FRAGMENT | MTL_STAGE_TILE | MTL_STAGE_DISPATCH;
    }
    if bits & Sync::NON_PIXEL_SHADING.bits() != 0 {
        out |= MTL_STAGE_VERTEX | MTL_STAGE_DISPATCH;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{PassDesc, RenderGraph, TextureDesc};
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
        // Native MTLStageFragment = 1 << 1 for both RENDER_TARGET and PIXEL_SHADING.
        assert_eq!(
            emitted,
            NativeBackendBarrier::Metal4StageBarrier {
                after_stages_mask: 1 << 1,
                before_stages_mask: 1 << 1,
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
}
