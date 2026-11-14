//! Translation of the planner's D3D12 enhanced-barrier vocabulary to Vulkan
//! synchronization2 (`VkPipelineStageFlags2`, `VkAccessFlags2`,
//! `VkImageLayout`).
//!
//! This module is pure data mapping and contains no `unsafe` code. The
//! mapping is conservative: aggregate D3D12 scopes become the smallest Vulkan
//! stage set that covers every scope they supersede, and every D3D12 access
//! becomes all of the Vulkan accesses it can perform (a UAV is a read and a
//! write). Scopes and accesses that need features this backend does not
//! enable (ray tracing, transform feedback, variable-rate shading, split
//! barriers) are rejected with [`TranslateError`], never dropped.

use ash::vk;
use vesper_ignis::{
    Access, Barrier, BufferBarrier, Layout, SubresourceRange, Sync, TextureBarrier,
};

/// A D3D12 scope, access or layout that has no Vulkan translation here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslateError {
    /// Sync bits with no supported Vulkan stage (raw remaining bits).
    UnsupportedSync(Sync),
    /// Access bits with no supported Vulkan access (raw remaining bits).
    UnsupportedAccess(Access),
    UnsupportedLayout(Layout),
    /// `SYNC_SPLIT` barriers would need `vkCmdSetEvent2`/`vkCmdWaitEvents2`.
    SplitBarrier,
    /// The planner never emits `LayoutAfter = UNDEFINED`; a hand-built one is
    /// rejected because Vulkan forbids transitioning *to* `UNDEFINED`.
    UndefinedLayoutAfter,
    /// The subresource range has no plane this image's format exposes.
    PlaneOutOfRange {
        first_plane: u32,
        plane_count: u32,
    },
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSync(s) => write!(f, "no Vulkan stage for D3D12 sync {s}"),
            Self::UnsupportedAccess(a) => write!(f, "no Vulkan access for D3D12 access {a}"),
            Self::UnsupportedLayout(l) => write!(f, "no Vulkan image layout for D3D12 layout {l}"),
            Self::SplitBarrier => f.write_str("split barriers are not supported"),
            Self::UndefinedLayoutAfter => f.write_str("layout after must not be UNDEFINED"),
            Self::PlaneOutOfRange {
                first_plane,
                plane_count,
            } => write!(f, "planes {first_plane}+{plane_count} out of range"),
        }
    }
}
impl std::error::Error for TranslateError {}

type S2 = vk::PipelineStageFlags2;
type A2 = vk::AccessFlags2;

const SYNC_TABLE: &[(Sync, S2)] = &[
    (Sync::ALL, S2::ALL_COMMANDS),
    (Sync::DRAW, S2::ALL_GRAPHICS),
    // D3D12 index input is the input assembler, which also fetches vertex
    // buffers; VERTEX_INPUT is INDEX_INPUT | VERTEX_ATTRIBUTE_INPUT.
    (Sync::INDEX_INPUT, S2::VERTEX_INPUT),
    // D3D12 VERTEX_BUFFER accesses synchronize with VERTEX_SHADING.
    (
        Sync::VERTEX_SHADING,
        S2::from_raw(S2::VERTEX_SHADER.as_raw() | S2::VERTEX_ATTRIBUTE_INPUT.as_raw()),
    ),
    (Sync::PIXEL_SHADING, S2::FRAGMENT_SHADER),
    (
        Sync::DEPTH_STENCIL,
        S2::from_raw(S2::EARLY_FRAGMENT_TESTS.as_raw() | S2::LATE_FRAGMENT_TESTS.as_raw()),
    ),
    (Sync::RENDER_TARGET, S2::COLOR_ATTACHMENT_OUTPUT),
    (Sync::COMPUTE_SHADING, S2::COMPUTE_SHADER),
    // `vkCmdFillBuffer`/`vkCmdClearColorImage` run in the CLEAR stage, which
    // D3D12 counts as copy work.
    (
        Sync::COPY,
        S2::from_raw(S2::COPY.as_raw() | S2::CLEAR.as_raw()),
    ),
    (Sync::RESOLVE, S2::RESOLVE),
    (Sync::EXECUTE_INDIRECT, S2::DRAW_INDIRECT),
    (
        Sync::ALL_SHADING,
        S2::from_raw(
            S2::PRE_RASTERIZATION_SHADERS.as_raw()
                | S2::VERTEX_ATTRIBUTE_INPUT.as_raw()
                | S2::FRAGMENT_SHADER.as_raw()
                | S2::COMPUTE_SHADER.as_raw(),
        ),
    ),
    (
        Sync::NON_PIXEL_SHADING,
        S2::from_raw(
            S2::PRE_RASTERIZATION_SHADERS.as_raw()
                | S2::VERTEX_ATTRIBUTE_INPUT.as_raw()
                | S2::COMPUTE_SHADER.as_raw(),
        ),
    ),
    (Sync::CLEAR_UNORDERED_ACCESS_VIEW, S2::CLEAR),
];

/// Maps a D3D12 sync scope to Vulkan stages. `NONE` maps to `NONE`.
pub fn stages(sync: Sync) -> Result<S2, TranslateError> {
    if sync.contains(Sync::SPLIT) && sync != Sync::NONE {
        return Err(TranslateError::SplitBarrier);
    }
    let mut rest = sync.bits();
    let mut out = S2::NONE;
    for (d3d, vk_stage) in SYNC_TABLE {
        if rest & d3d.bits() != 0 {
            out |= *vk_stage;
            rest &= !d3d.bits();
        }
    }
    if rest != 0 {
        return Err(TranslateError::UnsupportedSync(Sync::from_bits(rest)));
    }
    Ok(out)
}

const ACCESS_TABLE: &[(Access, A2)] = &[
    (Access::VERTEX_BUFFER, A2::VERTEX_ATTRIBUTE_READ),
    (Access::CONSTANT_BUFFER, A2::UNIFORM_READ),
    (Access::INDEX_BUFFER, A2::INDEX_READ),
    (
        Access::RENDER_TARGET,
        A2::from_raw(A2::COLOR_ATTACHMENT_READ.as_raw() | A2::COLOR_ATTACHMENT_WRITE.as_raw()),
    ),
    (
        Access::UNORDERED_ACCESS,
        A2::from_raw(A2::SHADER_STORAGE_READ.as_raw() | A2::SHADER_STORAGE_WRITE.as_raw()),
    ),
    (
        Access::DEPTH_STENCIL_WRITE,
        A2::from_raw(
            A2::DEPTH_STENCIL_ATTACHMENT_READ.as_raw()
                | A2::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw(),
        ),
    ),
    (
        Access::DEPTH_STENCIL_READ,
        A2::DEPTH_STENCIL_ATTACHMENT_READ,
    ),
    (
        Access::SHADER_RESOURCE,
        A2::from_raw(A2::SHADER_SAMPLED_READ.as_raw() | A2::SHADER_STORAGE_READ.as_raw()),
    ),
    (Access::INDIRECT_ARGUMENT, A2::INDIRECT_COMMAND_READ),
    (Access::COPY_DEST, A2::TRANSFER_WRITE),
    (Access::COPY_SOURCE, A2::TRANSFER_READ),
    (Access::RESOLVE_DEST, A2::TRANSFER_WRITE),
    (Access::RESOLVE_SOURCE, A2::TRANSFER_READ),
];

/// Maps a D3D12 access to Vulkan accesses performed under `stages`.
///
/// `NO_ACCESS`, or any access under an empty stage mask, is `NONE`.
/// `COMMON` (zero) is any access compatible with the common layout, so it
/// conservatively becomes `MEMORY_READ | MEMORY_WRITE`.
pub fn accesses(access: Access, stages: S2) -> Result<A2, TranslateError> {
    if access == Access::NO_ACCESS || stages == S2::NONE {
        return Ok(A2::NONE);
    }
    if access == Access::COMMON {
        return Ok(A2::MEMORY_READ | A2::MEMORY_WRITE);
    }
    let mut rest = access.bits();
    let mut out = A2::NONE;
    for (d3d, vk_access) in ACCESS_TABLE {
        if rest & d3d.bits() != 0 {
            out |= *vk_access;
            rest &= !d3d.bits();
        }
    }
    if rest != 0 {
        return Err(TranslateError::UnsupportedAccess(Access::from_bits(rest)));
    }
    Ok(out)
}

/// Maps a D3D12 layout to a Vulkan image layout. Queue-specific D3D12 layouts
/// map to the same Vulkan layout as their generic form: Vulkan expresses queue
/// affinity through queue families, not layouts.
pub fn image_layout(layout: Layout) -> Result<vk::ImageLayout, TranslateError> {
    use Layout::*;
    Ok(match layout {
        Undefined => vk::ImageLayout::UNDEFINED,
        Common | DirectQueueCommon | ComputeQueueCommon => vk::ImageLayout::GENERAL,
        // READ_ONLY_OPTIMAL does not admit transfer reads; GENERAL does.
        GenericRead | DirectQueueGenericRead | ComputeQueueGenericRead => vk::ImageLayout::GENERAL,
        UnorderedAccess | DirectQueueUnorderedAccess | ComputeQueueUnorderedAccess => {
            vk::ImageLayout::GENERAL
        }
        RenderTarget => vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        DepthStencilWrite => vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        DepthStencilRead => vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
        ShaderResource | DirectQueueShaderResource | ComputeQueueShaderResource => {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        }
        CopySource | DirectQueueCopySource | ComputeQueueCopySource | ResolveSource => {
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
        }
        CopyDest | DirectQueueCopyDest | ComputeQueueCopyDest | ResolveDest => {
            vk::ImageLayout::TRANSFER_DST_OPTIMAL
        }
        ShadingRateSource => return Err(TranslateError::UnsupportedLayout(layout)),
    })
}

/// Image aspect layout of a format, which decides how D3D12 planes map to
/// Vulkan aspects (D3D12 depth/stencil formats expose depth as plane 0 and
/// stencil as plane 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planes {
    Color,
    Depth,
    DepthStencil,
}

impl Planes {
    fn count(self) -> u32 {
        match self {
            Planes::Color | Planes::Depth => 1,
            Planes::DepthStencil => 2,
        }
    }
    fn aspect(self, plane: u32) -> vk::ImageAspectFlags {
        match (self, plane) {
            (Planes::Color, _) => vk::ImageAspectFlags::COLOR,
            (_, 0) => vk::ImageAspectFlags::DEPTH,
            _ => vk::ImageAspectFlags::STENCIL,
        }
    }
}

/// Maps a D3D12 subresource box to a `VkImageSubresourceRange`. `u32::MAX`
/// counts are `VK_REMAINING_*`, which has the same value.
pub fn subresource_range(
    range: SubresourceRange,
    planes: Planes,
) -> Result<vk::ImageSubresourceRange, TranslateError> {
    let total = planes.count();
    let err = TranslateError::PlaneOutOfRange {
        first_plane: range.first_plane,
        plane_count: range.plane_count,
    };
    if range.first_plane >= total || range.plane_count == 0 {
        return Err(err);
    }
    let last = if range.plane_count == u32::MAX {
        total
    } else {
        range
            .first_plane
            .checked_add(range.plane_count)
            .filter(|end| *end <= total)
            .ok_or(err)?
    };
    let mut aspect = vk::ImageAspectFlags::empty();
    for plane in range.first_plane..last {
        aspect |= planes.aspect(plane);
    }
    Ok(vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: range.first_mip,
        level_count: range.mip_count,
        base_array_layer: range.first_slice,
        layer_count: range.slice_count,
    })
}

/// A translated synchronization2 buffer barrier (no Vulkan handles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferBarrier2 {
    pub src_stage: S2,
    pub src_access: A2,
    pub dst_stage: S2,
    pub dst_access: A2,
}

/// A translated synchronization2 image barrier (no Vulkan handles).
#[derive(Clone, Copy, Debug)]
pub struct ImageBarrier2 {
    pub src_stage: S2,
    pub src_access: A2,
    pub dst_stage: S2,
    pub dst_access: A2,
    pub old_layout: vk::ImageLayout,
    pub new_layout: vk::ImageLayout,
    pub range: vk::ImageSubresourceRange,
}

pub fn buffer_barrier(b: &BufferBarrier) -> Result<BufferBarrier2, TranslateError> {
    let src_stage = stages(b.sync_before)?;
    let dst_stage = stages(b.sync_after)?;
    Ok(BufferBarrier2 {
        src_stage,
        src_access: accesses(b.access_before, src_stage)?,
        dst_stage,
        dst_access: accesses(b.access_after, dst_stage)?,
    })
}

pub fn image_barrier(b: &TextureBarrier, planes: Planes) -> Result<ImageBarrier2, TranslateError> {
    if b.layout_after == Layout::Undefined {
        return Err(TranslateError::UndefinedLayoutAfter);
    }
    let mut src_stage = stages(b.sync_before)?;
    let mut dst_stage = stages(b.sync_after)?;
    let src_access = accesses(b.access_before, src_stage)?;
    let dst_access = accesses(b.access_after, dst_stage)?;
    let old_layout = image_layout(b.layout_before)?;
    let new_layout = image_layout(b.layout_after)?;
    if old_layout != new_layout {
        // D3D12 queue handoffs put `SyncAfter = NONE` before a fence signal
        // and `SyncBefore = NONE` after a fence wait, relying on fences to
        // order the layout change. In Vulkan an empty stage mask on either
        // side of a layout transition does not chain with the semaphore
        // signal/wait, so the transition would be unordered. Widen the empty
        // side to ALL_COMMANDS (execution only; no access is added).
        if src_stage == S2::NONE {
            src_stage = S2::ALL_COMMANDS;
        }
        if dst_stage == S2::NONE {
            dst_stage = S2::ALL_COMMANDS;
        }
    }
    Ok(ImageBarrier2 {
        src_stage,
        src_access,
        dst_stage,
        dst_access,
        old_layout,
        new_layout,
        range: subresource_range(b.subresources, planes)?,
    })
}

/// Either translated barrier.
#[derive(Clone, Copy, Debug)]
pub enum Barrier2 {
    Buffer(BufferBarrier2),
    Image(ImageBarrier2),
}

/// Translates one planner barrier; `planes` is consulted for textures only.
pub fn barrier(b: &Barrier, planes: Planes) -> Result<Barrier2, TranslateError> {
    match b {
        Barrier::Buffer(b) => buffer_barrier(b).map(Barrier2::Buffer),
        Barrier::Texture(t) => image_barrier(t, planes).map(Barrier2::Image),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_modelled_sync_bit_but_raytracing_maps() {
        for (d3d, _) in SYNC_TABLE {
            assert_ne!(stages(*d3d).unwrap(), S2::NONE, "{d3d}");
        }
        assert_eq!(stages(Sync::NONE).unwrap(), S2::NONE);
        // Buffer fills execute in the CLEAR stage.
        assert!(stages(Sync::COPY).unwrap().contains(S2::CLEAR));
        for rt in [
            Sync::RAYTRACING,
            Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE,
            Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE,
            Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO,
        ] {
            assert_eq!(stages(rt), Err(TranslateError::UnsupportedSync(rt)));
        }
        assert_eq!(
            stages(Sync::RENDER_TARGET | Sync::SPLIT),
            Err(TranslateError::SplitBarrier)
        );
    }

    #[test]
    fn writes_keep_reads_and_unknown_access_is_rejected() {
        let a = accesses(Access::UNORDERED_ACCESS, S2::COMPUTE_SHADER).unwrap();
        assert!(a.contains(A2::SHADER_STORAGE_READ | A2::SHADER_STORAGE_WRITE));
        assert_eq!(
            accesses(Access::NO_ACCESS, S2::COMPUTE_SHADER).unwrap(),
            A2::NONE
        );
        assert_eq!(accesses(Access::COPY_DEST, S2::NONE).unwrap(), A2::NONE);
        assert_eq!(
            accesses(Access::COMMON, S2::COPY).unwrap(),
            A2::MEMORY_READ | A2::MEMORY_WRITE
        );
        for bad in [
            Access::STREAM_OUTPUT,
            Access::SHADING_RATE_SOURCE,
            Access::RAYTRACING_ACCELERATION_STRUCTURE_READ,
        ] {
            assert_eq!(
                accesses(bad | Access::COPY_SOURCE, S2::ALL_COMMANDS),
                Err(TranslateError::UnsupportedAccess(bad))
            );
        }
    }

    #[test]
    fn layouts() {
        assert_eq!(
            image_layout(Layout::RenderTarget).unwrap(),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
        );
        assert_eq!(
            image_layout(Layout::ComputeQueueShaderResource).unwrap(),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        );
        assert_eq!(
            image_layout(Layout::GenericRead).unwrap(),
            vk::ImageLayout::GENERAL
        );
        assert!(image_layout(Layout::ShadingRateSource).is_err());
    }

    #[test]
    fn planes_to_aspects() {
        let all = subresource_range(SubresourceRange::ALL, Planes::DepthStencil).unwrap();
        assert_eq!(
            all.aspect_mask,
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        );
        assert_eq!(all.level_count, vk::REMAINING_MIP_LEVELS);
        let stencil =
            subresource_range(SubresourceRange::single(0, 0, 1), Planes::DepthStencil).unwrap();
        assert_eq!(stencil.aspect_mask, vk::ImageAspectFlags::STENCIL);
        assert!(subresource_range(SubresourceRange::single(0, 0, 1), Planes::Color).is_err());
        let color = subresource_range(SubresourceRange::mips(1, 2), Planes::Color).unwrap();
        assert_eq!((color.base_mip_level, color.level_count), (1, 2));
    }

    #[test]
    fn handoff_transitions_are_ordered_on_both_sides() {
        let handle = {
            let mut g = vesper_ignis::RenderGraph::new();
            g.add_texture(vesper_ignis::TextureDesc::simple("t", Layout::Undefined))
                .unwrap()
        };
        let before_signal = TextureBarrier {
            resource: handle,
            sync_before: Sync::RENDER_TARGET,
            sync_after: Sync::NONE,
            access_before: Access::RENDER_TARGET,
            access_after: Access::NO_ACCESS,
            layout_before: Layout::RenderTarget,
            layout_after: Layout::ShaderResource,
            subresources: SubresourceRange::ALL,
        };
        let t = image_barrier(&before_signal, Planes::Color).unwrap();
        assert_eq!(t.src_stage, S2::COLOR_ATTACHMENT_OUTPUT);
        assert_eq!(t.dst_stage, S2::ALL_COMMANDS);
        assert_eq!(t.dst_access, A2::NONE);
        let first_use = TextureBarrier {
            sync_before: Sync::NONE,
            sync_after: Sync::RENDER_TARGET,
            access_before: Access::NO_ACCESS,
            access_after: Access::RENDER_TARGET,
            layout_before: Layout::Undefined,
            layout_after: Layout::RenderTarget,
            ..before_signal
        };
        let t = image_barrier(&first_use, Planes::Color).unwrap();
        assert_eq!((t.src_stage, t.src_access), (S2::ALL_COMMANDS, A2::NONE));
        let same_layout = TextureBarrier {
            sync_before: Sync::COMPUTE_SHADING,
            sync_after: Sync::NONE,
            access_before: Access::UNORDERED_ACCESS,
            access_after: Access::NO_ACCESS,
            layout_before: Layout::UnorderedAccess,
            layout_after: Layout::UnorderedAccess,
            ..before_signal
        };
        assert_eq!(
            image_barrier(&same_layout, Planes::Color)
                .unwrap()
                .dst_stage,
            S2::NONE
        );
    }
}
