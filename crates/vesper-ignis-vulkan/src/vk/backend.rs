//! The `vesper_ignis::lifecycle::Backend` implementation.

use super::device::{
    CommandAllocator, Device, GpuBuffer, GpuTexture, Inner, Pipeline, PipelineKind, count_leak,
    descriptor_type,
};
use crate::error::VulkanError;
use crate::ranges::{Bytes, barrier_range};
use crate::schedule::{Segment, schedule};
use crate::shader::BindingKind;
use crate::split::{self, Role};
use crate::translate::{self, Barrier2, QueueCaps};
use ash::vk;
use std::collections::{BTreeMap, VecDeque};
use std::mem::ManuallyDrop;
use std::sync::Arc;
use vesper_ignis::lifecycle::{
    Backend, BackendSubmit, Completion, FencePoint, OwnerViews, PollBatch, QueueCommand,
    Submission, TimelineId,
};
use vesper_ignis::{Barrier, BufferHandle, PassId, QueueKind, TextureHandle};

/// Index of a pipeline in [`FrameResources`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PipelineId(usize);

/// A push-descriptor binding of a dispatch, in binding order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bind {
    /// Mip 0 / layer 0 of the color plane, or the depth plane of a
    /// depth/stencil texture.
    SampledTexture(TextureHandle),
    StorageTexture(TextureHandle),
    StorageBuffer(BufferHandle),
    /// `size` bytes at `offset` (aligned to `minStorageBufferOffsetAlignment`).
    /// Buffer barriers protecting the dispatch are narrowed to this range.
    StorageBufferRange {
        buffer: BufferHandle,
        offset: u64,
        size: u64,
    },
}

/// The color attachment of [`PassWork::Render`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorTarget {
    pub texture: TextureHandle,
    pub clear: Option<[f32; 4]>,
}

/// The depth/stencil attachment of [`PassWork::Render`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthTarget {
    pub texture: TextureHandle,
    pub clear_depth: Option<f32>,
    /// Clears the stencil plane (formats with stencil only).
    pub clear_stencil: Option<u32>,
}

/// The GPU work recorded for one planned pass. The planner decides the
/// barriers around it; the backend checks each texture is in the layout the
/// work needs.
#[derive(Clone, Debug, PartialEq)]
pub enum PassWork {
    /// No commands (a pass that exists only for its synchronization).
    Empty,
    /// Dynamic rendering into mip 0 / layer 0 of `target` (which must be in
    /// `COLOR_ATTACHMENT_OPTIMAL`), with an optional clear.
    Draw {
        pipeline: PipelineId,
        target: TextureHandle,
        clear: Option<[f32; 4]>,
        vertices: u32,
    },
    Dispatch {
        pipeline: PipelineId,
        bindings: Vec<Bind>,
        groups: [u32; 3],
    },
    /// Copies mip 0 / layer 0 of `src` (tightly packed RGBA8 rows) to the
    /// start of `dst`.
    CopyTextureToBuffer {
        src: TextureHandle,
        dst: BufferHandle,
    },
    /// `vkCmdFillBuffer` over the whole buffer.
    FillBuffer { dst: BufferHandle, value: u32 },
    /// `vkCmdFillBuffer` over `size` bytes at `offset` (both multiples of 4).
    FillBufferRange {
        dst: BufferHandle,
        offset: u64,
        size: u64,
        value: u32,
    },
    /// `vkCmdCopyBuffer` of `size` bytes.
    CopyBuffer {
        src: BufferHandle,
        src_offset: u64,
        dst: BufferHandle,
        dst_offset: u64,
        size: u64,
    },
    /// Copies layer 0 of `mip` of one plane of `src` (plane 1 is stencil),
    /// tightly packed ([`crate::Format::copy_texel_bytes`]), to `dst` at
    /// `offset` (a multiple of 4).
    CopyPlaneToBuffer {
        src: TextureHandle,
        plane: u32,
        mip: u32,
        dst: BufferHandle,
        offset: u64,
    },
    /// Dynamic rendering into mip 0 / layer 0 of an optional color target
    /// and an optional depth/stencil target, whose formats must match the
    /// pipeline's [`crate::GraphicsDesc`]. Draws `vertices` x `instances`
    /// with the given stencil reference.
    Render {
        pipeline: PipelineId,
        color: Option<ColorTarget>,
        depth: Option<DepthTarget>,
        vertices: u32,
        instances: u32,
        stencil_reference: u32,
    },
}

impl PassWork {
    /// Byte ranges of buffers this work touches (for barrier narrowing).
    fn buffer_ranges(&self, r: &FrameResources) -> Vec<(BufferHandle, Bytes)> {
        let at = |offset, size| Bytes { offset, size };
        match self {
            PassWork::Empty | PassWork::Draw { .. } | PassWork::Render { .. } => Vec::new(),
            PassWork::Dispatch { bindings, .. } => bindings
                .iter()
                .filter_map(|b| match b {
                    Bind::StorageBuffer(h) => Some((*h, Bytes::WHOLE)),
                    Bind::StorageBufferRange {
                        buffer,
                        offset,
                        size,
                    } => Some((*buffer, at(*offset, *size))),
                    _ => None,
                })
                .collect(),
            PassWork::CopyTextureToBuffer { src, dst } => {
                let bytes = r.texture(*src).map_or(u64::MAX, |t| {
                    u64::from(t.info().width) * u64::from(t.info().height) * 4
                });
                vec![(*dst, at(0, bytes))]
            }
            PassWork::FillBuffer { dst, .. } => vec![(*dst, Bytes::WHOLE)],
            PassWork::FillBufferRange {
                dst, offset, size, ..
            } => vec![(*dst, at(*offset, *size))],
            PassWork::CopyBuffer {
                src,
                src_offset,
                dst,
                dst_offset,
                size,
            } => vec![
                (*src, at(*src_offset, *size)),
                (*dst, at(*dst_offset, *size)),
            ],
            PassWork::CopyPlaneToBuffer {
                src,
                plane,
                mip,
                dst,
                offset,
            } => {
                let bytes = r
                    .texture(*src)
                    .and_then(|t| plane_copy_bytes(t, *plane, *mip))
                    .unwrap_or(u64::MAX);
                vec![(*dst, at(*offset, bytes))]
            }
        }
    }
}

fn mip_extent(t: &GpuTexture, mip: u32) -> vk::Extent2D {
    vk::Extent2D {
        width: (t.info().width >> mip).max(1),
        height: (t.info().height >> mip).max(1),
    }
}

fn plane_copy_bytes(t: &GpuTexture, plane: u32, mip: u32) -> Option<u64> {
    if mip >= t.info().mip_levels {
        return None;
    }
    let e = mip_extent(t, mip);
    let texel = t.format().copy_texel_bytes(plane)?;
    Some(u64::from(e.width) * u64::from(e.height) * u64::from(texel))
}

/// The backend's `Resources` payload: native objects bound to one graph's
/// resource handles, the pipelines its passes use, and per-pass work. The
/// lifecycle runtime holds it until the frame's original fences complete.
#[derive(Debug, Default)]
pub struct FrameResources {
    buffers: Vec<(BufferHandle, GpuBuffer)>,
    textures: Vec<(TextureHandle, GpuTexture)>,
    pipelines: Vec<Pipeline>,
    passes: Vec<(PassId, PassWork)>,
}

/// The owners inside a [`FrameResources`].
#[derive(Debug)]
pub struct FrameParts {
    pub buffers: Vec<(BufferHandle, GpuBuffer)>,
    pub textures: Vec<(TextureHandle, GpuTexture)>,
    pub pipelines: Vec<Pipeline>,
}

impl FrameParts {
    pub fn destroy(self) {
        for (_, b) in self.buffers {
            b.destroy();
        }
        for (_, t) in self.textures {
            t.destroy();
        }
        for p in self.pipelines {
            p.destroy();
        }
    }
}

/// Bound on objects per frame, to keep validation linear.
pub const MAX_FRAME_OBJECTS: usize = 4096;

impl FrameResources {
    pub fn new() -> Self {
        Self::default()
    }

    fn full(&self) -> bool {
        self.buffers.len() + self.textures.len() + self.pipelines.len() + self.passes.len()
            >= MAX_FRAME_OBJECTS
    }

    /// Binds `buffer` to a graph buffer. On error the buffer is returned.
    pub fn bind_buffer(
        &mut self,
        handle: BufferHandle,
        buffer: GpuBuffer,
    ) -> Result<(), (GpuBuffer, VulkanError)> {
        if self.buffers.iter().any(|(h, _)| *h == handle) {
            return Err((buffer, VulkanError::DuplicateBinding(handle.into())));
        }
        if self.full() {
            return Err((buffer, VulkanError::Limit("frame objects")));
        }
        self.buffers.push((handle, buffer));
        Ok(())
    }

    /// Binds `texture` to a graph texture. On error the texture is returned.
    // Owners are returned by value on failure, as in `vesper_ignis::lifecycle`.
    #[allow(clippy::result_large_err)]
    pub fn bind_texture(
        &mut self,
        handle: TextureHandle,
        texture: GpuTexture,
    ) -> Result<(), (GpuTexture, VulkanError)> {
        if self.textures.iter().any(|(h, _)| *h == handle) {
            return Err((texture, VulkanError::DuplicateBinding(handle.into())));
        }
        if self.full() {
            return Err((texture, VulkanError::Limit("frame objects")));
        }
        self.textures.push((handle, texture));
        Ok(())
    }

    #[allow(clippy::result_large_err)]
    pub fn add_pipeline(
        &mut self,
        pipeline: Pipeline,
    ) -> Result<PipelineId, (Pipeline, VulkanError)> {
        if self.full() {
            return Err((pipeline, VulkanError::Limit("frame objects")));
        }
        self.pipelines.push(pipeline);
        Ok(PipelineId(self.pipelines.len() - 1))
    }

    /// Sets (or replaces) the work of `pass`.
    pub fn set_pass(&mut self, pass: PassId, work: PassWork) -> Result<(), VulkanError> {
        if let Some(slot) = self.passes.iter_mut().find(|(p, _)| *p == pass) {
            slot.1 = work;
            return Ok(());
        }
        if self.full() {
            return Err(VulkanError::Limit("frame objects"));
        }
        self.passes.push((pass, work));
        Ok(())
    }

    pub fn buffer(&self, handle: BufferHandle) -> Option<&GpuBuffer> {
        self.buffers
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, b)| b)
    }
    pub fn texture(&self, handle: TextureHandle) -> Option<&GpuTexture> {
        self.textures
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, t)| t)
    }

    /// Releases the bindings and pass table, returning the owners (to bind
    /// them to the next frame's graph, or to destroy them).
    pub fn into_parts(self) -> FrameParts {
        FrameParts {
            buffers: self.buffers,
            textures: self.textures,
            pipelines: self.pipelines,
        }
    }

    pub fn destroy(self) {
        self.into_parts().destroy();
    }
}

/// The work-graph scratch payload, which cannot be constructed: work-graph
/// scratch is D3D12-only, and a submission that carries any is rejected.
#[derive(Debug)]
pub enum NoScratch {}

struct TimelineState {
    kind: QueueKind,
    family: u32,
    caps: QueueCaps,
    queue: vk::Queue,
    semaphore: vk::Semaphore,
    submitted: u64,
}

/// Vulkan 1.3 implementation of the Ignis lifecycle backend.
///
/// Timeline `i` of the runtime is a `VkSemaphore` of type `TIMELINE` on the
/// `i`-th queue configured here. Planner barriers become
/// `vkCmdPipelineBarrier2`; planner and terminal signals become timeline
/// semaphore signal operations of `vkQueueSubmit2`; `poll` reads the
/// semaphores' counters and reports each original signal once, in
/// submission order.
#[must_use = "a VulkanBackend must be released with destroy() after its work completes"]
pub struct VulkanBackend {
    inner: ManuallyDrop<Arc<Inner>>,
    timelines: Vec<TimelineState>,
    /// Original signals submitted and not yet reported, in submission order.
    pending: VecDeque<FencePoint>,
    max_pending: usize,
    last_stats: SubmitStats,
    #[cfg(unix)]
    exports: super::syncfd::Exports,
    live: bool,
}

impl std::fmt::Debug for VulkanBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanBackend")
            .field("timelines", &self.timelines.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

/// Upper bound on unreported signals held by the backend.
pub const MAX_PENDING_SIGNALS: usize = 1 << 16;

impl VulkanBackend {
    /// One timeline per entry of `queues`, in the order of the runtime's
    /// `QueueConfig`s, all starting at fence value 0. Timelines of one kind
    /// are spread over that family's queues; on devices with fewer queues
    /// (lavapipe has one) several timelines share a `VkQueue`.
    pub fn new(device: &Device, queues: &[QueueKind]) -> Result<Self, VulkanError> {
        if queues.is_empty() || queues.len() > 64 {
            return Err(VulkanError::Limit("timelines"));
        }
        device.inner.ensure_live("vkCreateSemaphore")?;
        let inner = device.inner.clone();
        let mut backend = VulkanBackend {
            inner: ManuallyDrop::new(inner.clone()),
            timelines: Vec::with_capacity(queues.len()),
            pending: VecDeque::new(),
            max_pending: MAX_PENDING_SIGNALS,
            last_stats: SubmitStats::default(),
            #[cfg(unix)]
            exports: Default::default(),
            live: true,
        };
        let mut used: BTreeMap<u32, u32> = BTreeMap::new();
        for kind in queues {
            let family = inner.family(*kind);
            let slot = used.entry(family.index).or_insert(0);
            let queue = inner.queue(family.index, *slot);
            *slot += 1;
            let mut type_info = vk::SemaphoreTypeCreateInfo::default()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0);
            let info = vk::SemaphoreCreateInfo::default().push_next(&mut type_info);
            let semaphore = match inner.check("vkCreateSemaphore", unsafe {
                // SAFETY: valid create info.
                inner.device.create_semaphore(&info, None)
            }) {
                Ok(s) => s,
                Err(e) => {
                    backend.destroy_semaphores();
                    backend.release();
                    return Err(e);
                }
            };
            #[cfg(unix)]
            backend
                .exports
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .submitted
                .push(0);
            backend.timelines.push(TimelineState {
                kind: *kind,
                family: family.index,
                caps: inner.caps(*kind),
                queue,
                semaphore,
                submitted: 0,
            });
        }
        Ok(backend)
    }

    /// Current counter value of timeline `index`.
    pub fn counter(&self, index: usize) -> Result<u64, VulkanError> {
        let t = self
            .timelines
            .get(index)
            .ok_or(VulkanError::Limit("timeline index"))?;
        self.inner.check("vkGetSemaphoreCounterValue", unsafe {
            // SAFETY: live timeline semaphore.
            self.inner.device.get_semaphore_counter_value(t.semaphore)
        })
    }

    /// Blocks up to `timeout_ns` until every submitted signal has completed.
    /// Completions are still reported by `poll`.
    pub fn wait_idle(&self, timeout_ns: u64) -> Result<bool, VulkanError> {
        let (semaphores, values): (Vec<_>, Vec<_>) = self
            .timelines
            .iter()
            .filter(|t| t.submitted > 0)
            .map(|t| (t.semaphore, t.submitted))
            .unzip();
        if semaphores.is_empty() {
            return Ok(true);
        }
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: live semaphores; arrays borrow locals.
        match unsafe { self.inner.device.wait_semaphores(&info, timeout_ns) } {
            Err(vk::Result::TIMEOUT) => Ok(false),
            r => self.inner.check("vkWaitSemaphores", r).map(|()| true),
        }
    }

    pub fn pending_signals(&self) -> usize {
        self.pending.len()
    }

    fn destroy_semaphores(&mut self) {
        for t in self.timelines.drain(..) {
            // SAFETY: no pending signal uses it (checked by callers).
            unsafe { self.inner.device.destroy_semaphore(t.semaphore, None) };
        }
    }

    fn release(&mut self) {
        // SAFETY: called once, as the last use of `inner`.
        unsafe { ManuallyDrop::drop(&mut self.inner) };
        self.live = false;
    }

    /// Destroys the semaphores. Fails (returning the backend) while any
    /// submitted signal has not been observed complete on the device.
    #[allow(clippy::result_large_err)]
    pub fn destroy(mut self) -> Result<(), (Self, VulkanError)> {
        #[cfg(unix)]
        if super::syncfd::exports_open(&self.exports) > 0 {
            return Err((self, VulkanError::Pending));
        }
        if !self.inner.is_lost() {
            for t in &self.timelines {
                match self.inner.check("vkGetSemaphoreCounterValue", unsafe {
                    // SAFETY: live semaphore.
                    self.inner.device.get_semaphore_counter_value(t.semaphore)
                }) {
                    Ok(v) if v >= t.submitted => {}
                    Ok(_) => return Err((self, VulkanError::Pending)),
                    Err(e) => return Err((self, e)),
                }
            }
        } else if self.timelines.iter().any(|t| t.submitted > 0) {
            // After a loss nothing proves completion; keep the objects.
            return Err((self, VulkanError::DeviceLost { call: "destroy" }));
        }
        #[cfg(unix)]
        if !super::syncfd::close_exports(&self.exports) {
            return Err((self, VulkanError::Pending));
        }
        self.destroy_semaphores();
        self.release();
        Ok(())
    }

    /// An exporter of sync fds for this backend's timelines, to be taken
    /// before the backend moves into a `Runtime`. While any fd it exported is
    /// open, [`destroy`](Self::destroy) fails with [`VulkanError::Pending`].
    #[cfg(unix)]
    pub fn sync_fd_exporter(&self) -> super::syncfd::SyncFdExporter {
        super::syncfd::SyncFdExporter {
            inner: Arc::clone(&self.inner),
            timelines: self
                .timelines
                .iter()
                .map(|t| (t.semaphore, t.queue))
                .collect(),
            exports: self.exports.clone(),
        }
    }

    fn timeline(&self, id: TimelineId) -> Result<&TimelineState, VulkanError> {
        self.timelines.get(id.index()).ok_or(VulkanError::Schedule(
            crate::ScheduleError::UnknownTimeline(id),
        ))
    }
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}

fn find_buffer(r: &FrameResources, h: BufferHandle) -> Result<&GpuBuffer, VulkanError> {
    r.buffer(h).ok_or(VulkanError::UnboundResource(h.into()))
}
fn find_texture(r: &FrameResources, h: TextureHandle) -> Result<(usize, &GpuTexture), VulkanError> {
    r.textures
        .iter()
        .position(|(t, _)| *t == h)
        .map(|i| (i, &r.textures[i].1))
        .ok_or(VulkanError::UnboundResource(h.into()))
}

/// Per-submission simulation of texture layouts, committed only once work is
/// on the device.
struct Layouts {
    states: Vec<Vec<vk::ImageLayout>>,
}

fn resolve(base: u32, count: u32, len: u32) -> Option<std::ops::Range<u32>> {
    if base >= len || count == 0 {
        return None;
    }
    let end = if count == vk::REMAINING_MIP_LEVELS {
        len
    } else {
        base.checked_add(count).filter(|e| *e <= len)?
    };
    Some(base..end)
}

impl Layouts {
    fn new(r: &FrameResources) -> Self {
        Self {
            states: r
                .textures
                .iter()
                .map(|(_, t)| t.layouts.borrow().clone())
                .collect(),
        }
    }
    fn apply(
        &mut self,
        r: &FrameResources,
        handle: TextureHandle,
        b: &translate::ImageBarrier2,
        separate_ds: bool,
    ) -> Result<(), VulkanError> {
        let (i, tex) = find_texture(r, handle)?;
        let info = tex.info();
        let oob = VulkanError::RangeOutOfBounds(handle.into());
        let mips = resolve(b.range.base_mip_level, b.range.level_count, info.mip_levels)
            .ok_or(oob.clone())?;
        let layers = resolve(
            b.range.base_array_layer,
            b.range.layer_count,
            info.array_layers,
        )
        .ok_or(oob.clone())?;
        let format = info.format;
        let planes: Vec<u32> = (0..format.plane_count())
            .filter(|p| {
                format
                    .aspect(*p)
                    .is_some_and(|a| b.range.aspect_mask.contains(a))
            })
            .collect();
        if planes.is_empty()
            || b.range.aspect_mask & !format.aspects() != vk::ImageAspectFlags::empty()
        {
            return Err(oob);
        }
        if format.has_stencil() && planes.len() == 1 && !separate_ds {
            return Err(VulkanError::Unsupported("separateDepthStencilLayouts"));
        }
        for plane in planes {
            for layer in layers.clone() {
                for mip in mips.clone() {
                    let slot = tex.slot(plane, mip, layer).ok_or(oob.clone())?;
                    let slot = &mut self.states[i][slot];
                    if b.old_layout != vk::ImageLayout::UNDEFINED && *slot != b.old_layout {
                        return Err(VulkanError::LayoutMismatch {
                            resource: handle.into(),
                            expected: b.old_layout,
                            actual: *slot,
                        });
                    }
                    *slot = b.new_layout;
                }
            }
        }
        Ok(())
    }
    /// Mip 0 / layer 0 of `plane` must be in one of `allowed`.
    fn require(
        &self,
        r: &FrameResources,
        handle: TextureHandle,
        plane: u32,
        allowed: &[vk::ImageLayout],
    ) -> Result<(usize, vk::ImageLayout), VulkanError> {
        self.require_at(r, handle, plane, 0, allowed)
    }
    fn require_at(
        &self,
        r: &FrameResources,
        handle: TextureHandle,
        plane: u32,
        mip: u32,
        allowed: &[vk::ImageLayout],
    ) -> Result<(usize, vk::ImageLayout), VulkanError> {
        let (i, tex) = find_texture(r, handle)?;
        let slot = tex
            .slot(plane, mip, 0)
            .ok_or(VulkanError::RangeOutOfBounds(handle.into()))?;
        let actual = self.states[i][slot];
        if allowed.contains(&actual) {
            Ok((i, actual))
        } else {
            Err(VulkanError::LayoutMismatch {
                resource: handle.into(),
                expected: allowed[0],
                actual,
            })
        }
    }
    fn commit(self, r: &FrameResources) {
        for ((_, t), state) in r.textures.iter().zip(self.states) {
            *t.layouts.borrow_mut() = state;
        }
    }
}

/// Counters of what the last accepted submission recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmitStats {
    /// Split barriers recorded as `vkCmdSetEvent2`/`vkCmdWaitEvents2` pairs.
    pub split_events: usize,
    /// Split barriers collapsed into one barrier at the end half (queues
    /// without event support).
    pub collapsed_splits: usize,
    /// Buffer barriers narrowed to a sub-range.
    pub narrowed_buffer_barriers: usize,
    /// Barriers whose stage or access masks were reduced to what the queue
    /// family and queue kind can execute.
    pub filtered_barriers: usize,
    /// Queue-family ownership release/acquire pairs.
    pub ownership_transfers: usize,
}

struct BufDep {
    buffer: vk::Buffer,
    t: translate::BufferBarrier2,
    range: Bytes,
    families: (u32, u32),
}

struct ImgDep {
    image: vk::Image,
    t: translate::ImageBarrier2,
    families: (u32, u32),
}

#[derive(Default)]
struct Dep {
    buffers: Vec<BufDep>,
    images: Vec<ImgDep>,
}

impl Dep {
    fn is_empty(&self) -> bool {
        self.buffers.is_empty() && self.images.is_empty()
    }
}

const IGNORED: (u32, u32) = (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED);

struct RenderColor {
    view: vk::ImageView,
    layout: vk::ImageLayout,
    clear: Option<[f32; 4]>,
}

struct RenderDepth {
    view: vk::ImageView,
    depth_layout: vk::ImageLayout,
    /// The stencil plane's layout, for formats with stencil.
    stencil_layout: Option<vk::ImageLayout>,
    clear_depth: Option<f32>,
    clear_stencil: Option<u32>,
}

/// A fully validated, translated command of a segment body.
enum Cmd {
    Barrier(Dep),
    /// `vkCmdSetEvent2` of the stream's event `event`.
    SetEvent {
        event: usize,
        dep: Dep,
    },
    /// `vkCmdWaitEvents2` on the stream's event `event`.
    WaitEvent {
        event: usize,
        dep: Dep,
    },
    Render {
        pipeline: vk::Pipeline,
        color: Option<RenderColor>,
        depth: Option<RenderDepth>,
        extent: vk::Extent2D,
        vertices: u32,
        instances: u32,
        stencil_reference: Option<u32>,
    },
    Dispatch {
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        writes: Vec<Write>,
        groups: [u32; 3],
    },
    CopyImageToBuffer {
        image: vk::Image,
        layout: vk::ImageLayout,
        aspect: vk::ImageAspectFlags,
        mip: u32,
        extent: vk::Extent2D,
        buffer: vk::Buffer,
        offset: u64,
    },
    Fill {
        buffer: vk::Buffer,
        offset: u64,
        size: u64,
        value: u32,
    },
    CopyBuffer {
        src: vk::Buffer,
        dst: vk::Buffer,
        region: vk::BufferCopy,
    },
}

enum Write {
    Image(vk::DescriptorType, vk::DescriptorImageInfo),
    Buffer(vk::DescriptorBufferInfo),
}

struct Batch {
    segment: Segment,
    commands: Vec<Cmd>,
    /// Ownership releases recorded after `commands` (before the signals).
    releases: Dep,
    /// The segment is its stream's last: end with a device-to-host barrier.
    host_barrier: bool,
}

/// Everything `prepare` validated, ready to record.
struct Prepared {
    batches: Vec<Batch>,
    layouts: Layouts,
    /// Events each stream needs (one per split pair recorded with events).
    events: Vec<usize>,
    stats: SubmitStats,
    /// Owning families after the submission (exclusive sharing only).
    owners: Option<BTreeMap<Res, u32>>,
}

/// A frame resource, by its index in the frame's binding tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Res {
    Buffer(usize),
    Texture(usize),
}

const SHADER_READ: [vk::ImageLayout; 4] = [
    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    vk::ImageLayout::GENERAL,
    vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
    vk::ImageLayout::DEPTH_READ_ONLY_OPTIMAL,
];
const COPY_READ: [vk::ImageLayout; 2] = [
    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
    vk::ImageLayout::GENERAL,
];
const DEPTH_WRITE: [vk::ImageLayout; 3] = [
    vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    vk::ImageLayout::GENERAL,
    vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
];
const DEPTH_READ: [vk::ImageLayout; 5] = [
    vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    vk::ImageLayout::GENERAL,
    vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
    vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
    vk::ImageLayout::DEPTH_READ_ONLY_OPTIMAL,
];

fn buffer_index(r: &FrameResources, h: BufferHandle) -> Option<usize> {
    r.buffers.iter().position(|(b, _)| *b == h)
}

fn pass_work(r: &FrameResources, pass: PassId) -> Option<&PassWork> {
    r.passes.iter().find(|(p, _)| *p == pass).map(|(_, w)| w)
}

/// Resources a pass's work references (for ownership tracking).
fn work_resources(r: &FrameResources, w: &PassWork) -> Vec<Res> {
    let tex = |h: TextureHandle| find_texture(r, h).ok().map(|(i, _)| Res::Texture(i));
    let buf = |h: BufferHandle| buffer_index(r, h).map(Res::Buffer);
    let mut out: Vec<Option<Res>> = Vec::new();
    match w {
        PassWork::Empty => {}
        PassWork::Draw { target, .. } => out.push(tex(*target)),
        PassWork::Render { color, depth, .. } => {
            out.extend(color.iter().map(|c| tex(c.texture)));
            out.extend(depth.iter().map(|d| tex(d.texture)));
        }
        PassWork::Dispatch { bindings, .. } => {
            for b in bindings {
                out.push(match b {
                    Bind::SampledTexture(t) | Bind::StorageTexture(t) => tex(*t),
                    Bind::StorageBuffer(b) => buf(*b),
                    Bind::StorageBufferRange { buffer, .. } => buf(*buffer),
                });
            }
        }
        PassWork::CopyTextureToBuffer { src, dst }
        | PassWork::CopyPlaneToBuffer { src, dst, .. } => {
            out.push(tex(*src));
            out.push(buf(*dst));
        }
        PassWork::FillBuffer { dst, .. } | PassWork::FillBufferRange { dst, .. } => {
            out.push(buf(*dst))
        }
        PassWork::CopyBuffer { src, dst, .. } => {
            out.push(buf(*src));
            out.push(buf(*dst));
        }
    }
    out.into_iter().flatten().collect()
}

impl VulkanBackend {
    fn check_device(&self, other: &Arc<Inner>) -> Result<(), VulkanError> {
        if Arc::ptr_eq(&self.inner, other) {
            Ok(())
        } else {
            Err(VulkanError::ForeignDevice)
        }
    }

    /// Statistics of the last accepted submission.
    pub fn last_submit_stats(&self) -> SubmitStats {
        self.last_stats
    }

    fn prepare(
        &self,
        submission: &Submission,
        owners: &OwnerViews<'_, CommandAllocator, FrameResources, NoScratch>,
    ) -> Result<Prepared, VulkanError> {
        let r = owners.resources;
        for (_, b) in &r.buffers {
            self.check_device(&b.inner)?;
        }
        for (_, t) in &r.textures {
            self.check_device(&t.inner)?;
        }
        for p in &r.pipelines {
            self.check_device(&p.inner)?;
        }
        for a in owners.allocators {
            self.check_device(&a.value().inner)?;
        }
        for stream in &submission.streams {
            let t = self.timeline(stream.timeline)?;
            if t.kind != stream.kind {
                return Err(VulkanError::Limit("timeline queue kind"));
            }
            let alloc = owners
                .allocators
                .iter()
                .find(|a| a.timeline() == stream.timeline)
                .ok_or(VulkanError::AllocatorFamily)?;
            if alloc.value().family != t.family {
                return Err(VulkanError::AllocatorFamily);
            }
        }
        let submitted: BTreeMap<TimelineId, u64> = submission
            .streams
            .iter()
            .map(|s| s.timeline)
            .chain(submission.streams.iter().flat_map(|s| {
                s.commands.iter().filter_map(|c| match c {
                    QueueCommand::Wait(p) => Some(p.timeline),
                    _ => None,
                })
            }))
            .map(|id| self.timeline(id).map(|t| (id, t.submitted)))
            .collect::<Result<_, _>>()?;
        let order = schedule(submission, &submitted)?;
        let pairings = submission
            .streams
            .iter()
            .map(|s| split::pair(&s.commands))
            .collect::<Result<Vec<_>, _>>()?;
        let mut last = vec![usize::MAX; submission.streams.len()];
        for (i, seg) in order.iter().enumerate() {
            last[seg.stream] = i;
        }
        let exclusive = self.inner.exclusive && self.inner.sharing.len() > 1;
        let (transfers, owners_after) = if exclusive {
            let (t, o) = self.plan_ownership(submission, &order, r);
            (t, Some(o))
        } else {
            (Vec::new(), None)
        };
        let reach = if transfers.is_empty() {
            Vec::new()
        } else {
            semaphore_reach(&order)
        };
        let mut stats = SubmitStats::default();
        let mut events = vec![0usize; submission.streams.len()];
        let mut layouts = Layouts::new(r);
        let mut batches: Vec<Batch> = Vec::with_capacity(order.len());
        for (i, segment) in order.into_iter().enumerate() {
            let stream = &submission.streams[segment.stream];
            let t = self.timeline(stream.timeline)?;
            let caps = t.caps;
            let mut commands = Vec::with_capacity(segment.body.len() + 1);
            let mut acquire = Dep::default();
            for x in transfers.iter().filter(|x| x.acquire_in == i) {
                let resource = || match x.resource {
                    Res::Buffer(b) => r.buffers[b].0.into(),
                    Res::Texture(t) => r.textures[t].0.into(),
                };
                let Some(k) = x.release_in else {
                    return Err(VulkanError::OwnershipTransfer(resource()));
                };
                // The release must happen before the acquire through a
                // semaphore (submission order is not an execution
                // dependency). The planner orders hazards only, so two
                // reads on different families may have none: reject.
                let ordered = batches[k].segment.signals.iter().any(|sig| {
                    reach[i]
                        .get(&sig.timeline.index())
                        .is_some_and(|v| *v >= sig.value)
                });
                if !ordered {
                    return Err(VulkanError::OwnershipTransfer(resource()));
                }
                let (release, acq) =
                    ownership_deps(r, &layouts, x.resource, x.from_family, x.to_family);
                batches[k].releases.buffers.extend(release.buffers);
                batches[k].releases.images.extend(release.images);
                acquire.buffers.extend(acq.buffers);
                acquire.images.extend(acq.images);
                stats.ownership_transfers += 1;
            }
            if !acquire.is_empty() {
                commands.push(Cmd::Barrier(acquire));
            }
            for &c in &segment.body {
                match &stream.commands[c] {
                    QueueCommand::Barriers(list) => {
                        self.barriers(
                            r,
                            &stream.commands,
                            c,
                            list,
                            &pairings[segment.stream],
                            caps,
                            &mut layouts,
                            &mut stats,
                            &mut commands,
                        )?;
                    }
                    QueueCommand::Execute(pass) => {
                        if let Some(cmd) = self.work(r, *pass, stream.kind, &layouts)? {
                            commands.push(cmd);
                        }
                    }
                    _ => unreachable!("segment bodies hold barriers and passes only"),
                }
            }
            if caps.events() {
                events[segment.stream] = pairings[segment.stream].pairs.len();
            }
            batches.push(Batch {
                host_barrier: last[segment.stream] == i,
                segment,
                commands,
                releases: Dep::default(),
            });
        }
        Ok(Prepared {
            batches,
            layouts,
            events,
            stats,
            owners: owners_after,
        })
    }

    /// Ownership transfers of this submission under exclusive sharing.
    fn plan_ownership(
        &self,
        submission: &Submission,
        order: &[Segment],
        r: &FrameResources,
    ) -> (Vec<crate::ownership::Transfer<Res>>, BTreeMap<Res, u32>) {
        use crate::ownership::{Use, plan};
        let mut owners: BTreeMap<Res, u32> = BTreeMap::new();
        for (i, (_, b)) in r.buffers.iter().enumerate() {
            if let Some(f) = b.owner.get() {
                owners.insert(Res::Buffer(i), f);
            }
        }
        for (i, (_, t)) in r.textures.iter().enumerate() {
            if let Some(f) = t.owner.get() {
                owners.insert(Res::Texture(i), f);
            }
        }
        let mut segments = Vec::with_capacity(order.len());
        for seg in order {
            let stream = &submission.streams[seg.stream];
            let family = self
                .timeline(stream.timeline)
                .map(|t| t.family)
                .unwrap_or(0);
            let mut uses: Vec<Use<Res>> = Vec::new();
            for &c in &seg.body {
                match &stream.commands[c] {
                    QueueCommand::Barriers(list) => {
                        for b in list {
                            let (res, discards) = match b {
                                Barrier::Buffer(bb) => {
                                    (buffer_index(r, bb.resource).map(Res::Buffer), false)
                                }
                                Barrier::Texture(tb) => (
                                    find_texture(r, tb.resource)
                                        .ok()
                                        .map(|(i, _)| Res::Texture(i)),
                                    tb.layout_before == vesper_ignis::Layout::Undefined,
                                ),
                            };
                            if let Some(resource) = res {
                                uses.push(Use { resource, discards });
                            }
                        }
                    }
                    QueueCommand::Execute(pass) => {
                        if let Some(w) = pass_work(r, *pass) {
                            uses.extend(work_resources(r, w).into_iter().map(|resource| Use {
                                resource,
                                discards: false,
                            }));
                        }
                    }
                    _ => {}
                }
            }
            segments.push((family, uses));
        }
        let transfers = plan(&segments, &mut owners);
        (transfers, owners)
    }

    #[allow(clippy::too_many_arguments)]
    fn barriers(
        &self,
        r: &FrameResources,
        stream: &[QueueCommand],
        at: usize,
        list: &[Barrier],
        pairing: &split::Pairing,
        caps: QueueCaps,
        layouts: &mut Layouts,
        stats: &mut SubmitStats,
        out: &mut Vec<Cmd>,
    ) -> Result<(), VulkanError> {
        let separate_ds = self.inner.separate_ds;
        let touches = |pass: PassId| {
            pass_work(r, pass)
                .map(|w| w.buffer_ranges(r))
                .unwrap_or_default()
        };
        let mut plain = Dep::default();
        let mut sets: Vec<(usize, Dep)> = Vec::new();
        let mut waits: Vec<(usize, Dep)> = Vec::new();
        for (bi, b) in list.iter().enumerate() {
            let role = pairing.role(at, bi);
            // Which barrier to translate, where its dst side starts (for
            // buffer narrowing), and whether it changes tracked layouts.
            let (whole, dst_at, apply) = match role {
                Role::Plain => (*b, at, true),
                Role::Begin(n) if caps.events() => (pairing.pairs[n], pairing.end_command[n], true),
                Role::Begin(_) => {
                    stats.collapsed_splits += 1;
                    continue;
                }
                Role::End(n) => (pairing.pairs[n], at, !caps.events()),
            };
            let mut dep = Dep::default();
            match whole {
                Barrier::Buffer(bb) => {
                    let Barrier2::Buffer(t) = translate::barrier(&whole, translate::Planes::Color)?
                    else {
                        unreachable!("translation preserves the barrier kind")
                    };
                    let filtered = translate::filter_buffer_barrier(t, caps);
                    if filtered != t {
                        stats.filtered_barriers += 1;
                    }
                    let buf = find_buffer(r, bb.resource)?;
                    let mut range = barrier_range(stream, dst_at, bb.resource, touches);
                    if !range.is_whole() {
                        range.size = range.size.min(buf.size().saturating_sub(range.offset));
                        if range.offset >= buf.size()
                            || (range.offset == 0 && range.size == buf.size())
                        {
                            range = Bytes::WHOLE;
                        } else {
                            stats.narrowed_buffer_barriers += 1;
                        }
                    }
                    dep.buffers.push(BufDep {
                        buffer: buf.buffer,
                        t: filtered,
                        range,
                        families: IGNORED,
                    });
                }
                Barrier::Texture(tb) => {
                    let (_, tex) = find_texture(r, tb.resource)?;
                    let planes = tex.format().planes();
                    let Barrier2::Image(t) = translate::barrier(&whole, planes)? else {
                        unreachable!("translation preserves the barrier kind")
                    };
                    let filtered = translate::filter_image_barrier(t, caps);
                    if filtered.src_stage != t.src_stage
                        || filtered.dst_stage != t.dst_stage
                        || filtered.src_access != t.src_access
                        || filtered.dst_access != t.dst_access
                    {
                        stats.filtered_barriers += 1;
                    }
                    if apply {
                        layouts.apply(r, tb.resource, &filtered, separate_ds)?;
                    }
                    dep.images.push(ImgDep {
                        image: tex.image,
                        t: filtered,
                        families: IGNORED,
                    });
                }
            }
            match role {
                Role::Begin(n) => sets.push((n, dep)),
                Role::End(n) if caps.events() => waits.push((n, dep)),
                Role::End(_) | Role::Plain => {
                    plain.buffers.extend(dep.buffers);
                    plain.images.extend(dep.images);
                }
            }
        }
        for (event, dep) in waits {
            stats.split_events += 1;
            out.push(Cmd::WaitEvent { event, dep });
        }
        if !plain.is_empty() {
            out.push(Cmd::Barrier(plain));
        }
        for (event, dep) in sets {
            out.push(Cmd::SetEvent { event, dep });
        }
        Ok(())
    }

    fn work(
        &self,
        r: &FrameResources,
        pass: PassId,
        kind: QueueKind,
        layouts: &Layouts,
    ) -> Result<Option<Cmd>, VulkanError> {
        let work = pass_work(r, pass).ok_or(VulkanError::MissingPassWork(pass))?;
        let pipeline = |id: PipelineId| {
            r.pipelines
                .get(id.0)
                .ok_or(VulkanError::BadPipeline { pass })
        };
        let limits = &self.inner.limits;
        // `whole_words`: the size must also be a multiple of 4 (buffer fills);
        // copies constrain only the offset.
        let buffer_span =
            |b: BufferHandle, offset: u64, size: u64, align: u64, whole_words: bool| {
                let buf = find_buffer(r, b)?;
                let end = offset
                    .checked_add(size)
                    .filter(|e| *e <= buf.size() && size > 0)
                    .ok_or(VulkanError::OutOfBounds("buffer range"))?;
                let _ = end;
                if !offset.is_multiple_of(align) || (whole_words && !size.is_multiple_of(4)) {
                    return Err(VulkanError::OutOfBounds("buffer range alignment"));
                }
                Ok(buf)
            };
        Ok(Some(match work {
            PassWork::Empty => return Ok(None),
            PassWork::Draw {
                pipeline: id,
                target,
                clear,
                vertices,
            } => self.render(
                r,
                pass,
                kind,
                layouts,
                *id,
                Some(ColorTarget {
                    texture: *target,
                    clear: *clear,
                }),
                None,
                *vertices,
                1,
                0,
            )?,
            PassWork::Render {
                pipeline: id,
                color,
                depth,
                vertices,
                instances,
                stencil_reference,
            } => self.render(
                r,
                pass,
                kind,
                layouts,
                *id,
                *color,
                *depth,
                *vertices,
                *instances,
                *stencil_reference,
            )?,
            PassWork::Dispatch {
                pipeline: id,
                bindings,
                groups,
            } => {
                if kind == QueueKind::Copy {
                    return Err(VulkanError::WrongQueue { pass, kind });
                }
                let p = pipeline(*id)?;
                let PipelineKind::Compute(declared) = &p.kind else {
                    return Err(VulkanError::BadPipeline { pass });
                };
                if declared.len() != bindings.len() {
                    return Err(VulkanError::BindingMismatch { pass });
                }
                if groups
                    .iter()
                    .zip(limits.max_compute_work_group_count)
                    .any(|(g, max)| *g == 0 || *g > max)
                {
                    return Err(VulkanError::OutOfBounds("dispatch groups"));
                }
                let mut writes = Vec::with_capacity(bindings.len());
                for (bind, want) in bindings.iter().zip(declared) {
                    writes.push(match (bind, want) {
                        (Bind::SampledTexture(t), BindingKind::SampledTexture) => {
                            let (i, layout) = layouts.require(r, *t, 0, &SHADER_READ)?;
                            let tex = &r.textures[i].1;
                            if !tex.usage.contains(vk::ImageUsageFlags::SAMPLED) {
                                return Err(VulkanError::BindingMismatch { pass });
                            }
                            Write::Image(
                                descriptor_type(*want),
                                vk::DescriptorImageInfo {
                                    sampler: vk::Sampler::null(),
                                    image_view: tex.sampled_view,
                                    image_layout: layout,
                                },
                            )
                        }
                        (Bind::StorageTexture(t), BindingKind::StorageTexture) => {
                            let (i, layout) =
                                layouts.require(r, *t, 0, &[vk::ImageLayout::GENERAL])?;
                            let tex = &r.textures[i].1;
                            if !tex.usage.contains(vk::ImageUsageFlags::STORAGE) {
                                return Err(VulkanError::BindingMismatch { pass });
                            }
                            Write::Image(
                                descriptor_type(*want),
                                vk::DescriptorImageInfo {
                                    sampler: vk::Sampler::null(),
                                    image_view: tex.view,
                                    image_layout: layout,
                                },
                            )
                        }
                        (Bind::StorageBuffer(b), BindingKind::StorageBuffer) => {
                            let buf = find_buffer(r, *b)?;
                            if buf.size() > u64::from(limits.max_storage_buffer_range) {
                                return Err(VulkanError::OutOfBounds(
                                    "storage buffer exceeds maxStorageBufferRange",
                                ));
                            }
                            Write::Buffer(vk::DescriptorBufferInfo {
                                buffer: buf.buffer,
                                offset: 0,
                                range: vk::WHOLE_SIZE,
                            })
                        }
                        (
                            Bind::StorageBufferRange {
                                buffer,
                                offset,
                                size,
                            },
                            BindingKind::StorageBuffer,
                        ) => {
                            let align = limits.min_storage_buffer_offset_alignment.max(1);
                            let buf = buffer_span(*buffer, *offset, *size, align, false)?;
                            if *size > u64::from(limits.max_storage_buffer_range) {
                                return Err(VulkanError::OutOfBounds(
                                    "storage range exceeds maxStorageBufferRange",
                                ));
                            }
                            Write::Buffer(vk::DescriptorBufferInfo {
                                buffer: buf.buffer,
                                offset: *offset,
                                range: *size,
                            })
                        }
                        _ => return Err(VulkanError::BindingMismatch { pass }),
                    });
                }
                Cmd::Dispatch {
                    pipeline: p.pipeline,
                    layout: p.layout,
                    writes,
                    groups: *groups,
                }
            }
            PassWork::CopyTextureToBuffer { src, dst } => {
                let (i, layout) = layouts.require(r, *src, 0, &COPY_READ)?;
                let tex = &r.textures[i].1;
                if tex.format().copy_texel_bytes(0) != Some(4) || !tex.format().is_color() {
                    return Err(VulkanError::InvalidDesc(
                        "CopyTextureToBuffer copies 4-byte color texels; use CopyPlaneToBuffer",
                    ));
                }
                let buf = find_buffer(r, *dst)?;
                let bytes = u64::from(tex.info().width) * u64::from(tex.info().height) * 4;
                if bytes > buf.size() {
                    return Err(VulkanError::OutOfBounds("readback buffer too small"));
                }
                Cmd::CopyImageToBuffer {
                    image: tex.image,
                    layout,
                    aspect: vk::ImageAspectFlags::COLOR,
                    mip: 0,
                    extent: mip_extent(tex, 0),
                    buffer: buf.buffer,
                    offset: 0,
                }
            }
            PassWork::CopyPlaneToBuffer {
                src,
                plane,
                mip,
                dst,
                offset,
            } => {
                let (i, layout) = layouts.require_at(r, *src, *plane, *mip, &COPY_READ)?;
                let tex = &r.textures[i].1;
                // Depth/stencil image-to-buffer copies need a graphics
                // family (VUID-vkCmdCopyImageToBuffer-commandBuffer-10216;
                // maintenance10 is not enabled).
                if !tex.format().is_color()
                    && !self
                        .inner
                        .family(kind)
                        .flags
                        .contains(vk::QueueFlags::GRAPHICS)
                {
                    return Err(VulkanError::WrongQueue { pass, kind });
                }
                let aspect = tex
                    .format()
                    .aspect(*plane)
                    .ok_or(VulkanError::RangeOutOfBounds((*src).into()))?;
                let bytes = plane_copy_bytes(tex, *plane, *mip)
                    .ok_or(VulkanError::RangeOutOfBounds((*src).into()))?;
                let buf = buffer_span(*dst, *offset, bytes, 4, false)?;
                Cmd::CopyImageToBuffer {
                    image: tex.image,
                    layout,
                    aspect,
                    mip: *mip,
                    extent: mip_extent(tex, *mip),
                    buffer: buf.buffer,
                    offset: *offset,
                }
            }
            PassWork::FillBuffer { dst, value } => Cmd::Fill {
                buffer: find_buffer(r, *dst)?.buffer,
                offset: 0,
                size: vk::WHOLE_SIZE,
                value: *value,
            },
            PassWork::FillBufferRange {
                dst,
                offset,
                size,
                value,
            } => Cmd::Fill {
                buffer: buffer_span(*dst, *offset, *size, 4, true)?.buffer,
                offset: *offset,
                size: *size,
                value: *value,
            },
            PassWork::CopyBuffer {
                src,
                src_offset,
                dst,
                dst_offset,
                size,
            } => {
                let s = buffer_span(*src, *src_offset, *size, 1, false)?;
                let d = buffer_span(*dst, *dst_offset, *size, 1, false)?;
                if s.buffer == d.buffer {
                    let (a, b) = (*src_offset, *dst_offset);
                    if a < b + size && b < a + size {
                        return Err(VulkanError::OutOfBounds("overlapping buffer copy"));
                    }
                }
                Cmd::CopyBuffer {
                    src: s.buffer,
                    dst: d.buffer,
                    region: vk::BufferCopy {
                        src_offset: *src_offset,
                        dst_offset: *dst_offset,
                        size: *size,
                    },
                }
            }
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        r: &FrameResources,
        pass: PassId,
        kind: QueueKind,
        layouts: &Layouts,
        id: PipelineId,
        color: Option<ColorTarget>,
        depth: Option<DepthTarget>,
        vertices: u32,
        instances: u32,
        stencil_reference: u32,
    ) -> Result<Cmd, VulkanError> {
        if kind != QueueKind::Direct {
            return Err(VulkanError::WrongQueue { pass, kind });
        }
        let p = r
            .pipelines
            .get(id.0)
            .ok_or(VulkanError::BadPipeline { pass })?;
        let PipelineKind::Graphics(desc) = &p.kind else {
            return Err(VulkanError::BadPipeline { pass });
        };
        if vertices > (1 << 24) || instances == 0 || instances > (1 << 16) {
            return Err(VulkanError::OutOfBounds("vertex or instance count"));
        }
        let mut extent = None;
        let mut same_extent = |e: vk::Extent2D| match extent {
            None => {
                extent = Some(e);
                Ok(())
            }
            Some(x) if x == e => Ok(()),
            Some(_) => Err(VulkanError::InvalidDesc("attachment extents differ")),
        };
        let color = match (color, desc.color) {
            (None, None) => None,
            (Some(c), Some(fmt)) => {
                let (i, layout) = layouts.require(
                    r,
                    c.texture,
                    0,
                    &[
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::ImageLayout::GENERAL,
                    ],
                )?;
                let tex = &r.textures[i].1;
                if tex.format() != fmt {
                    return Err(VulkanError::BadPipeline { pass });
                }
                same_extent(mip_extent(tex, 0))?;
                Some(RenderColor {
                    view: tex.view,
                    layout,
                    clear: c.clear,
                })
            }
            _ => return Err(VulkanError::BadPipeline { pass }),
        };
        let depth = match (depth, desc.depth) {
            (None, None) => None,
            (Some(d), Some(fmt)) => {
                let (i, _) = find_texture(r, d.texture)?;
                let tex = &r.textures[i].1;
                if tex.format() != fmt {
                    return Err(VulkanError::BadPipeline { pass });
                }
                let writes_depth = d.clear_depth.is_some() || desc.depth_write;
                let (_, depth_layout) = layouts.require(
                    r,
                    d.texture,
                    0,
                    if writes_depth {
                        &DEPTH_WRITE
                    } else {
                        &DEPTH_READ
                    },
                )?;
                let stencil_layout = if fmt.has_stencil() {
                    let writes = d.clear_stencil.is_some()
                        || desc.stencil.is_some_and(|s| s.pass != vk::StencilOp::KEEP);
                    let allowed: &[vk::ImageLayout] = if writes {
                        &[
                            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                            vk::ImageLayout::GENERAL,
                            vk::ImageLayout::STENCIL_ATTACHMENT_OPTIMAL,
                        ]
                    } else {
                        &[
                            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                            vk::ImageLayout::GENERAL,
                            vk::ImageLayout::STENCIL_ATTACHMENT_OPTIMAL,
                            vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
                            vk::ImageLayout::STENCIL_READ_ONLY_OPTIMAL,
                        ]
                    };
                    Some(layouts.require(r, d.texture, 1, allowed)?.1)
                } else if d.clear_stencil.is_some() {
                    return Err(VulkanError::InvalidDesc(
                        "stencil clear without a stencil plane",
                    ));
                } else {
                    None
                };
                same_extent(mip_extent(tex, 0))?;
                Some(RenderDepth {
                    view: tex.view,
                    depth_layout,
                    stencil_layout,
                    clear_depth: d.clear_depth,
                    clear_stencil: d.clear_stencil,
                })
            }
            _ => return Err(VulkanError::BadPipeline { pass }),
        };
        Ok(Cmd::Render {
            pipeline: p.pipeline,
            color,
            depth,
            extent: extent.ok_or(VulkanError::BadPipeline { pass })?,
            vertices,
            instances,
            stencil_reference: desc.stencil.map(|_| stencil_reference),
        })
    }

    /// Records one batch into `cb`; `events` are the stream's events.
    fn record(
        &self,
        cb: vk::CommandBuffer,
        batch: &Batch,
        events: &[vk::Event],
    ) -> Result<(), VulkanError> {
        let d = &self.inner.device;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: `cb` was freshly allocated from a reset pool.
        self.inner.check("vkBeginCommandBuffer", unsafe {
            d.begin_command_buffer(cb, &begin)
        })?;
        // D3D12 completes and flushes all prior work on a queue at every
        // ExecuteCommandLists boundary (each planner Wait/Signal is one, and
        // so is the start of a frame), and the planner relies on that: its
        // first barrier on a resource has `SyncBefore = NONE`. Vulkan gives no
        // such guarantee between submissions, so each batch starts with a
        // full memory dependency on everything submitted before it on this
        // queue (and, through semaphore waits, on the queues it waits for).
        let boundary = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)];
        // SAFETY: recording command buffer; locals outlive the call.
        unsafe {
            d.cmd_pipeline_barrier2(
                cb,
                &vk::DependencyInfo::default().memory_barriers(&boundary),
            )
        };
        for cmd in batch.commands.iter() {
            self.record_cmd(cb, cmd, events)?;
        }
        if !batch.releases.is_empty() {
            self.with_dependency(&batch.releases, |dep| {
                // SAFETY: recording command buffer; `dep` borrows locals.
                unsafe { d.cmd_pipeline_barrier2(cb, dep) }
            });
        }
        if batch.host_barrier {
            let barrier = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)];
            let dep = vk::DependencyInfo::default().memory_barriers(&barrier);
            // SAFETY: recording command buffer; locals outlive the call.
            unsafe { d.cmd_pipeline_barrier2(cb, &dep) };
        }
        // SAFETY: recording command buffer.
        self.inner
            .check("vkEndCommandBuffer", unsafe { d.end_command_buffer(cb) })
    }

    /// Builds the `VkDependencyInfo` of `dep` and passes it to `f`.
    fn with_dependency(&self, dep: &Dep, f: impl FnOnce(&vk::DependencyInfo<'_>)) {
        let b: Vec<_> = dep
            .buffers
            .iter()
            .map(|x| {
                vk::BufferMemoryBarrier2::default()
                    .src_stage_mask(x.t.src_stage)
                    .src_access_mask(x.t.src_access)
                    .dst_stage_mask(x.t.dst_stage)
                    .dst_access_mask(x.t.dst_access)
                    .src_queue_family_index(x.families.0)
                    .dst_queue_family_index(x.families.1)
                    .buffer(x.buffer)
                    .offset(x.range.offset)
                    .size(if x.range.is_whole() {
                        vk::WHOLE_SIZE
                    } else {
                        x.range.size
                    })
            })
            .collect();
        let i: Vec<_> = dep
            .images
            .iter()
            .map(|x| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(x.t.src_stage)
                    .src_access_mask(x.t.src_access)
                    .dst_stage_mask(x.t.dst_stage)
                    .dst_access_mask(x.t.dst_access)
                    .old_layout(x.t.old_layout)
                    .new_layout(x.t.new_layout)
                    .src_queue_family_index(x.families.0)
                    .dst_queue_family_index(x.families.1)
                    .image(x.image)
                    .subresource_range(x.t.range)
            })
            .collect();
        f(&vk::DependencyInfo::default()
            .buffer_memory_barriers(&b)
            .image_memory_barriers(&i));
    }

    fn record_cmd(
        &self,
        cb: vk::CommandBuffer,
        cmd: &Cmd,
        events: &[vk::Event],
    ) -> Result<(), VulkanError> {
        let d = &self.inner.device;
        let event = |n: usize| {
            events
                .get(n)
                .copied()
                .ok_or(VulkanError::Limit("split barrier events"))
        };
        // SAFETY (whole match): every handle was validated against the
        // frame's owners, which the runtime keeps alive until the original
        // signals of this submission complete; arrays borrow locals that
        // outlive each call.
        match cmd {
            Cmd::Barrier(dep) => {
                self.with_dependency(dep, |info| unsafe { d.cmd_pipeline_barrier2(cb, info) })
            }
            Cmd::SetEvent { event: n, dep } => {
                let e = event(*n)?;
                self.with_dependency(dep, |info| unsafe { d.cmd_set_event2(cb, e, info) })
            }
            Cmd::WaitEvent { event: n, dep } => {
                let e = event(*n)?;
                self.with_dependency(dep, |info| unsafe {
                    d.cmd_wait_events2(cb, &[e], std::slice::from_ref(info))
                })
            }
            Cmd::Render {
                pipeline,
                color,
                depth,
                extent,
                vertices,
                instances,
                stencil_reference,
            } => {
                let load = |clear: bool| {
                    if clear {
                        vk::AttachmentLoadOp::CLEAR
                    } else {
                        vk::AttachmentLoadOp::LOAD
                    }
                };
                let colors: Vec<_> = color
                    .iter()
                    .map(|c| {
                        vk::RenderingAttachmentInfo::default()
                            .image_view(c.view)
                            .image_layout(c.layout)
                            .load_op(load(c.clear.is_some()))
                            .store_op(vk::AttachmentStoreOp::STORE)
                            .clear_value(vk::ClearValue {
                                color: vk::ClearColorValue {
                                    float32: c.clear.unwrap_or_default(),
                                },
                            })
                    })
                    .collect();
                let ds_clear = |z: &RenderDepth| vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: z.clear_depth.unwrap_or(1.0),
                        stencil: z.clear_stencil.unwrap_or(0),
                    },
                };
                let depth_att = depth.as_ref().map(|z| {
                    vk::RenderingAttachmentInfo::default()
                        .image_view(z.view)
                        .image_layout(z.depth_layout)
                        .load_op(load(z.clear_depth.is_some()))
                        .store_op(vk::AttachmentStoreOp::STORE)
                        .clear_value(ds_clear(z))
                });
                let stencil_att = depth.as_ref().and_then(|z| {
                    z.stencil_layout.map(|layout| {
                        vk::RenderingAttachmentInfo::default()
                            .image_view(z.view)
                            .image_layout(layout)
                            .load_op(load(z.clear_stencil.is_some()))
                            .store_op(vk::AttachmentStoreOp::STORE)
                            .clear_value(ds_clear(z))
                    })
                });
                let area = vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: *extent,
                };
                let mut info = vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&colors);
                if let Some(a) = depth_att.as_ref() {
                    info = info.depth_attachment(a);
                }
                if let Some(a) = stencil_att.as_ref() {
                    info = info.stencil_attachment(a);
                }
                let viewport = vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                };
                unsafe {
                    d.cmd_begin_rendering(cb, &info);
                    d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, *pipeline);
                    d.cmd_set_viewport(cb, 0, &[viewport]);
                    d.cmd_set_scissor(cb, 0, &[area]);
                    if let Some(reference) = stencil_reference {
                        d.cmd_set_stencil_reference(
                            cb,
                            vk::StencilFaceFlags::FRONT_AND_BACK,
                            *reference,
                        );
                    }
                    d.cmd_draw(cb, *vertices, *instances, 0, 0);
                    d.cmd_end_rendering(cb);
                }
            }
            Cmd::Dispatch {
                pipeline,
                layout,
                writes,
                groups,
            } => {
                let images: Vec<[vk::DescriptorImageInfo; 1]> = writes
                    .iter()
                    .map(|w| match w {
                        Write::Image(_, info) => [*info],
                        Write::Buffer(_) => [vk::DescriptorImageInfo::default()],
                    })
                    .collect();
                let buffers: Vec<[vk::DescriptorBufferInfo; 1]> = writes
                    .iter()
                    .map(|w| match w {
                        Write::Buffer(info) => [*info],
                        Write::Image(..) => [vk::DescriptorBufferInfo::default()],
                    })
                    .collect();
                let sets: Vec<_> = writes
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let base = vk::WriteDescriptorSet::default().dst_binding(i as u32);
                        match w {
                            Write::Image(ty, _) => base.descriptor_type(*ty).image_info(&images[i]),
                            Write::Buffer(_) => base
                                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                                .buffer_info(&buffers[i]),
                        }
                    })
                    .collect();
                unsafe {
                    d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, *pipeline);
                    if !sets.is_empty() {
                        self.inner.push.cmd_push_descriptor_set(
                            cb,
                            vk::PipelineBindPoint::COMPUTE,
                            *layout,
                            0,
                            &sets,
                        );
                    }
                    d.cmd_dispatch(cb, groups[0], groups[1], groups[2]);
                }
            }
            Cmd::CopyImageToBuffer {
                image,
                layout,
                aspect,
                mip,
                extent,
                buffer,
                offset,
            } => {
                let region = vk::BufferImageCopy {
                    buffer_offset: *offset,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: *aspect,
                        mip_level: *mip,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    },
                };
                unsafe { d.cmd_copy_image_to_buffer(cb, *image, *layout, *buffer, &[region]) };
            }
            Cmd::Fill {
                buffer,
                offset,
                size,
                value,
            } => unsafe { d.cmd_fill_buffer(cb, *buffer, *offset, *size, *value) },
            Cmd::CopyBuffer { src, dst, region } => unsafe {
                d.cmd_copy_buffer(cb, *src, *dst, &[*region])
            },
        }
        Ok(())
    }

    fn try_submit(
        &mut self,
        submission: &Submission,
        owners: &OwnerViews<'_, CommandAllocator, FrameResources, NoScratch>,
    ) -> Result<(), (usize, VulkanError)> {
        if self.inner.is_lost() {
            return Err((0, VulkanError::DeviceLost { call: "submit" }));
        }
        let signals: usize = submission
            .streams
            .iter()
            .map(|s| {
                s.commands
                    .iter()
                    .filter(|c| matches!(c, QueueCommand::Signal(_)))
                    .count()
            })
            .sum();
        if self.pending.len() + signals > self.max_pending {
            return Err((0, VulkanError::Limit("pending signals")));
        }
        let prepared = self.prepare(submission, owners).map_err(|e| (0, e))?;
        let Prepared {
            batches,
            layouts,
            events,
            stats,
            owners: owners_after,
        } = prepared;
        let d = &self.inner.device;
        // Reset each stream's pool once and allocate its command buffers.
        let mut per_stream: Vec<Vec<usize>> = vec![Vec::new(); submission.streams.len()];
        for (i, b) in batches.iter().enumerate() {
            if !b.commands.is_empty() || b.host_barrier || !b.releases.is_empty() {
                per_stream[b.segment.stream].push(i);
            }
        }
        let mut buffers = vec![vk::CommandBuffer::null(); batches.len()];
        let mut stream_events: Vec<Vec<vk::Event>> = vec![Vec::new(); submission.streams.len()];
        for (s, list) in per_stream.iter().enumerate() {
            let stream = &submission.streams[s];
            let alloc = owners
                .allocators
                .iter()
                .find(|a| a.timeline() == stream.timeline)
                .expect("checked in prepare")
                .value();
            let pool = alloc.pool;
            // SAFETY: the pool's earlier command buffers belong to retired
            // frames (the runtime hands allocators out only after retirement).
            self.inner
                .check("vkResetCommandPool", unsafe {
                    d.reset_command_pool(pool, vk::CommandPoolResetFlags::empty())
                })
                .map_err(|e| (0, e))?;
            stream_events[s] = alloc.events(events[s]).map_err(|e| (0, e))?;
            if list.is_empty() {
                continue;
            }
            // Reuse the pool's reset command buffers; allocate only the rest.
            let mut cached = alloc.buffers.borrow_mut();
            if cached.len() < list.len() {
                if list.len() > 4096 {
                    return Err((0, VulkanError::Limit("command buffers per stream")));
                }
                let info = vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count((list.len() - cached.len()) as u32);
                // SAFETY: valid pool.
                let more = self
                    .inner
                    .check("vkAllocateCommandBuffers", unsafe {
                        d.allocate_command_buffers(&info)
                    })
                    .map_err(|e| (0, e))?;
                cached.extend(more);
            }
            for (slot, cb) in list.iter().zip(cached.iter()) {
                buffers[*slot] = *cb;
            }
        }
        for (b, cb) in batches.iter().zip(&buffers) {
            if *cb != vk::CommandBuffer::null() {
                self.record(*cb, b, &stream_events[b.segment.stream])
                    .map_err(|e| (0, e))?;
            }
        }
        // Everything is validated and recorded; now publish in order.
        for (n, (batch, cb)) in batches.iter().zip(&buffers).enumerate() {
            let stream = &submission.streams[batch.segment.stream];
            let t = self.timeline(stream.timeline).map_err(|e| (n, e))?;
            let queue = t.queue;
            let waits: Vec<_> = batch
                .segment
                .waits
                .iter()
                .map(|w| {
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(self.timelines[w.timeline.index()].semaphore)
                        .value(w.value)
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                })
                .collect();
            // Signal operations within one batch are unordered, so a timeline
            // is signalled once, with the batch's highest value for it; every
            // original point is still reported by `poll`.
            let mut highest: BTreeMap<usize, FencePoint> = BTreeMap::new();
            for s in &batch.segment.signals {
                let e = highest.entry(s.timeline.index()).or_insert(*s);
                if s.value > e.value {
                    *e = *s;
                }
            }
            let signal_infos: Vec<_> = highest
                .values()
                .map(|s| {
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(self.timelines[s.timeline.index()].semaphore)
                        .value(s.value)
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                })
                .collect();
            let cb_infos: Vec<_> = if *cb == vk::CommandBuffer::null() {
                Vec::new()
            } else {
                vec![vk::CommandBufferSubmitInfo::default().command_buffer(*cb)]
            };
            let submit = vk::SubmitInfo2::default()
                .wait_semaphore_infos(&waits)
                .command_buffer_infos(&cb_infos)
                .signal_semaphore_infos(&signal_infos);
            // SAFETY: all handles are live and owned by in-flight owners;
            // host access to the queue is serialized by the device lock.
            let r = {
                let _guard = self
                    .inner
                    .submit_lock
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                unsafe { d.queue_submit2(queue, &[submit], vk::Fence::null()) }
            };
            self.inner.check("vkQueueSubmit2", r).map_err(|e| (n, e))?;
            for s in &batch.segment.signals {
                self.timelines[s.timeline.index()].submitted = s.value;
                self.pending.push_back(*s);
                #[cfg(unix)]
                if let Some(v) = self
                    .exports
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .submitted
                    .get_mut(s.timeline.index())
                {
                    *v = s.value;
                }
            }
        }
        layouts.commit(owners.resources);
        if let Some(map) = owners_after {
            for (res, family) in map {
                match res {
                    Res::Buffer(i) => owners.resources.buffers[i].1.owner.set(Some(family)),
                    Res::Texture(i) => owners.resources.textures[i].1.owner.set(Some(family)),
                }
            }
        }
        self.last_stats = stats;
        if let Ok(mut s) = self.inner.stats.lock() {
            *s = stats;
        }
        Ok(())
    }
}

/// For each segment (in submission order), the timeline values that are
/// known to have been reached before it starts: its own waits, and,
/// transitively, what the segments signalling those values had waited for.
/// Submission order on one queue adds nothing (it is not an execution
/// dependency between batches).
fn semaphore_reach(order: &[Segment]) -> Vec<BTreeMap<usize, u64>> {
    let points = |ps: &[vesper_ignis::lifecycle::FencePoint]| {
        ps.iter().map(|p| (p.timeline.index(), p.value)).collect()
    };
    let segments: Vec<WaitsSignals> = order
        .iter()
        .map(|s| (points(&s.waits), points(&s.signals)))
        .collect();
    reach_of(&segments)
}

/// A segment's waits and signals as `(timeline index, value)`.
type WaitsSignals = (Vec<(usize, u64)>, Vec<(usize, u64)>);

/// [`semaphore_reach`] over `(waits, signals)` of `(timeline, value)`.
fn reach_of(segments: &[WaitsSignals]) -> Vec<BTreeMap<usize, u64>> {
    let mut reach: Vec<BTreeMap<usize, u64>> = Vec::with_capacity(segments.len());
    for (waits, _) in segments {
        let mut known: BTreeMap<usize, u64> = BTreeMap::new();
        let merge = |t: usize, v: u64, known: &mut BTreeMap<usize, u64>| {
            let e = known.entry(t).or_insert(0);
            *e = (*e).max(v);
        };
        for &(t, v) in waits {
            merge(t, v, &mut known);
            // The earliest earlier segment whose signal satisfies the wait.
            let producer = segments[..reach.len()]
                .iter()
                .position(|(_, signals)| signals.iter().any(|&(st, sv)| st == t && sv >= v));
            if let Some(p) = producer {
                for (&pt, &pv) in &reach[p] {
                    merge(pt, pv, &mut known);
                }
            }
        }
        reach.push(known);
    }
    reach
}

/// Release (on the old family) and acquire (on the new family) barriers for
/// one resource, keeping every subresource in its current layout.
fn ownership_deps(
    r: &FrameResources,
    layouts: &Layouts,
    res: Res,
    from: u32,
    to: u32,
) -> (Dep, Dep) {
    let mut release = Dep::default();
    let mut acquire = Dep::default();
    let rel = |old: vk::ImageLayout, range| translate::ImageBarrier2 {
        src_stage: vk::PipelineStageFlags2::ALL_COMMANDS,
        src_access: vk::AccessFlags2::MEMORY_WRITE,
        dst_stage: vk::PipelineStageFlags2::NONE,
        dst_access: vk::AccessFlags2::NONE,
        old_layout: old,
        new_layout: old,
        range,
    };
    match res {
        Res::Buffer(i) => {
            let buffer = r.buffers[i].1.buffer;
            release.buffers.push(BufDep {
                buffer,
                t: translate::BufferBarrier2 {
                    src_stage: vk::PipelineStageFlags2::ALL_COMMANDS,
                    src_access: vk::AccessFlags2::MEMORY_WRITE,
                    dst_stage: vk::PipelineStageFlags2::NONE,
                    dst_access: vk::AccessFlags2::NONE,
                },
                range: Bytes::WHOLE,
                families: (from, to),
            });
            acquire.buffers.push(BufDep {
                buffer,
                t: translate::BufferBarrier2 {
                    src_stage: vk::PipelineStageFlags2::NONE,
                    src_access: vk::AccessFlags2::NONE,
                    dst_stage: vk::PipelineStageFlags2::ALL_COMMANDS,
                    dst_access: vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
                },
                range: Bytes::WHOLE,
                families: (from, to),
            });
        }
        Res::Texture(i) => {
            let tex = &r.textures[i].1;
            let info = tex.info();
            for plane in 0..info.format.plane_count() {
                for layer in 0..info.array_layers {
                    for mip in 0..info.mip_levels {
                        let Some(slot) = tex.slot(plane, mip, layer) else {
                            continue;
                        };
                        let layout = layouts.states[i][slot];
                        if layout == vk::ImageLayout::UNDEFINED {
                            continue;
                        }
                        let range = vk::ImageSubresourceRange {
                            aspect_mask: info.format.aspect(plane).unwrap_or_default(),
                            base_mip_level: mip,
                            level_count: 1,
                            base_array_layer: layer,
                            layer_count: 1,
                        };
                        let t = rel(layout, range);
                        release.images.push(ImgDep {
                            image: tex.image,
                            t,
                            families: (from, to),
                        });
                        acquire.images.push(ImgDep {
                            image: tex.image,
                            t: translate::ImageBarrier2 {
                                src_stage: vk::PipelineStageFlags2::NONE,
                                src_access: vk::AccessFlags2::NONE,
                                dst_stage: vk::PipelineStageFlags2::ALL_COMMANDS,
                                dst_access: vk::AccessFlags2::MEMORY_READ
                                    | vk::AccessFlags2::MEMORY_WRITE,
                                ..t
                            },
                            families: (from, to),
                        });
                    }
                }
            }
        }
    }
    (release, acquire)
}

impl Backend for VulkanBackend {
    type Allocator = CommandAllocator;
    type Resources = FrameResources;
    type Scratch = NoScratch;
    type Error = VulkanError;

    fn submit(
        &mut self,
        submission: &Submission,
        owners: OwnerViews<'_, CommandAllocator, FrameResources, NoScratch>,
    ) -> BackendSubmit<VulkanError> {
        match self.try_submit(submission, &owners) {
            Ok(()) => BackendSubmit::Accepted,
            Err((0, e)) => BackendSubmit::Rejected(e),
            Err((_, e)) => BackendSubmit::Uncertain(e),
        }
    }

    fn poll(&mut self, max_events: usize) -> PollBatch<VulkanError> {
        let mut completions = Vec::new();
        let mut cache: BTreeMap<usize, u64> = BTreeMap::new();
        let mut error = None;
        while completions.len() < max_events {
            let Some(point) = self.pending.front().copied() else {
                break;
            };
            let index = point.timeline.index();
            let value = match cache.get(&index) {
                Some(v) => *v,
                None => match self.counter(index) {
                    Ok(v) => {
                        cache.insert(index, v);
                        v
                    }
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                },
            };
            if value < point.value {
                // Strict FIFO: later signals may depend on this one.
                break;
            }
            self.pending.pop_front();
            completions.push(Completion::reported(point));
        }
        PollBatch { completions, error }
    }
}

#[cfg(test)]
mod reach_tests {
    use super::reach_of;

    #[test]
    fn reach_follows_semaphores_not_submission_order() {
        // 0: A signals A=1. 1: B signals B=1 without waiting. 2: B waits
        // A=1, signals B=2. 3: A waits B=2, signals A=2.
        let segments = vec![
            (vec![], vec![(0, 1)]),
            (vec![], vec![(1, 1)]),
            (vec![(0, 1)], vec![(1, 2)]),
            (vec![(1, 2)], vec![(0, 2)]),
        ];
        let reach = reach_of(&segments);
        assert!(reach[0].is_empty() && reach[1].is_empty());
        assert_eq!(reach[2].get(&0), Some(&1));
        // Transitive: segment 3 waited for segment 2, which waited for A=1.
        assert_eq!(reach[3].get(&1), Some(&2));
        assert_eq!(reach[3].get(&0), Some(&1));
    }
}
