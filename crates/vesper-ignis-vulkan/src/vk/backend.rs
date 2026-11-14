//! The `vesper_ignis::lifecycle::Backend` implementation.

use super::device::{
    CommandAllocator, Device, GpuBuffer, GpuTexture, Inner, Pipeline, PipelineKind, count_leak,
    descriptor_type,
};
use crate::error::VulkanError;
use crate::schedule::{Segment, schedule};
use crate::shader::BindingKind;
use crate::translate::{self, Barrier2, Planes};
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
    SampledTexture(TextureHandle),
    StorageTexture(TextureHandle),
    StorageBuffer(BufferHandle),
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
        let inner = device.inner.clone();
        let mut backend = VulkanBackend {
            inner: ManuallyDrop::new(inner.clone()),
            timelines: Vec::with_capacity(queues.len()),
            pending: VecDeque::new(),
            max_pending: MAX_PENDING_SIGNALS,
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
            backend.timelines.push(TimelineState {
                kind: *kind,
                family: family.index,
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
        self.destroy_semaphores();
        self.release();
        Ok(())
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
    ) -> Result<(), VulkanError> {
        let (i, tex) = find_texture(r, handle)?;
        let info = tex.info();
        let resolve = |base: u32, count: u32, len: u32| -> Option<std::ops::Range<u32>> {
            if base >= len || count == 0 {
                return None;
            }
            let end = if count == vk::REMAINING_MIP_LEVELS {
                len
            } else {
                base.checked_add(count).filter(|e| *e <= len)?
            };
            Some(base..end)
        };
        let oob = VulkanError::RangeOutOfBounds(handle.into());
        let mips = resolve(b.range.base_mip_level, b.range.level_count, info.mip_levels)
            .ok_or(oob.clone())?;
        let layers = resolve(
            b.range.base_array_layer,
            b.range.layer_count,
            info.array_layers,
        )
        .ok_or(oob)?;
        if b.range.aspect_mask != vk::ImageAspectFlags::COLOR {
            return Err(VulkanError::RangeOutOfBounds(handle.into()));
        }
        for layer in layers {
            for mip in mips.clone() {
                let slot = &mut self.states[i][(mip + info.mip_levels * layer) as usize];
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
        Ok(())
    }
    fn require(
        &self,
        r: &FrameResources,
        handle: TextureHandle,
        allowed: &[vk::ImageLayout],
    ) -> Result<(usize, vk::ImageLayout), VulkanError> {
        let (i, _) = find_texture(r, handle)?;
        let actual = self.states[i][0];
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

/// A fully validated, translated command of a segment body.
enum Cmd {
    Barriers {
        buffers: Vec<(vk::Buffer, translate::BufferBarrier2)>,
        images: Vec<(vk::Image, translate::ImageBarrier2)>,
    },
    Draw {
        pipeline: vk::Pipeline,
        view: vk::ImageView,
        extent: vk::Extent2D,
        clear: Option<[f32; 4]>,
        vertices: u32,
    },
    Dispatch {
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        writes: Vec<Write>,
        groups: [u32; 3],
    },
    CopyTextureToBuffer {
        image: vk::Image,
        layout: vk::ImageLayout,
        extent: vk::Extent2D,
        buffer: vk::Buffer,
    },
    Fill {
        buffer: vk::Buffer,
        value: u32,
    },
}

enum Write {
    Image(vk::DescriptorType, vk::DescriptorImageInfo),
    Buffer(vk::DescriptorBufferInfo),
}

struct Batch {
    segment: Segment,
    commands: Vec<Cmd>,
    /// The segment is its stream's last: end with a device-to-host barrier.
    host_barrier: bool,
}

const SHADER_READ: [vk::ImageLayout; 2] = [
    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    vk::ImageLayout::GENERAL,
];
const COPY_READ: [vk::ImageLayout; 2] = [
    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
    vk::ImageLayout::GENERAL,
];

impl VulkanBackend {
    fn check_device(&self, other: &Arc<Inner>) -> Result<(), VulkanError> {
        if Arc::ptr_eq(&self.inner, other) {
            Ok(())
        } else {
            Err(VulkanError::ForeignDevice)
        }
    }

    fn prepare(
        &self,
        submission: &Submission,
        owners: &OwnerViews<'_, CommandAllocator, FrameResources, NoScratch>,
    ) -> Result<(Vec<Batch>, Layouts), VulkanError> {
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
        let mut last = vec![usize::MAX; submission.streams.len()];
        for (i, seg) in order.iter().enumerate() {
            last[seg.stream] = i;
        }
        let mut layouts = Layouts::new(r);
        let mut batches = Vec::with_capacity(order.len());
        for (i, segment) in order.into_iter().enumerate() {
            let stream = &submission.streams[segment.stream];
            let mut commands = Vec::with_capacity(segment.body.len());
            for &c in &segment.body {
                match &stream.commands[c] {
                    QueueCommand::Barriers(list) => {
                        commands.push(self.barriers(r, list, &mut layouts)?);
                    }
                    QueueCommand::Execute(pass) => {
                        if let Some(cmd) = self.work(r, *pass, stream.kind, &layouts)? {
                            commands.push(cmd);
                        }
                    }
                    _ => unreachable!("segment bodies hold barriers and passes only"),
                }
            }
            batches.push(Batch {
                host_barrier: last[segment.stream] == i,
                segment,
                commands,
            });
        }
        Ok((batches, layouts))
    }

    fn barriers(
        &self,
        r: &FrameResources,
        list: &[Barrier],
        layouts: &mut Layouts,
    ) -> Result<Cmd, VulkanError> {
        let mut buffers = Vec::new();
        let mut images = Vec::new();
        for b in list {
            match (b, translate::barrier(b, Planes::Color)?) {
                (Barrier::Buffer(bb), Barrier2::Buffer(t)) => {
                    buffers.push((find_buffer(r, bb.resource)?.buffer, t));
                }
                (Barrier::Texture(tb), Barrier2::Image(t)) => {
                    layouts.apply(r, tb.resource, &t)?;
                    images.push((find_texture(r, tb.resource)?.1.image, t));
                }
                _ => unreachable!("translation preserves the barrier kind"),
            }
        }
        Ok(Cmd::Barriers { buffers, images })
    }

    fn work(
        &self,
        r: &FrameResources,
        pass: PassId,
        kind: QueueKind,
        layouts: &Layouts,
    ) -> Result<Option<Cmd>, VulkanError> {
        let work = r
            .passes
            .iter()
            .find(|(p, _)| *p == pass)
            .map(|(_, w)| w)
            .ok_or(VulkanError::MissingPassWork(pass))?;
        let pipeline = |id: PipelineId| {
            r.pipelines
                .get(id.0)
                .ok_or(VulkanError::BadPipeline { pass })
        };
        let limits = &self.inner.limits;
        Ok(Some(match work {
            PassWork::Empty => return Ok(None),
            PassWork::Draw {
                pipeline: id,
                target,
                clear,
                vertices,
            } => {
                if kind != QueueKind::Direct {
                    return Err(VulkanError::WrongQueue { pass, kind });
                }
                let p = pipeline(*id)?;
                if p.kind != PipelineKind::Graphics {
                    return Err(VulkanError::BadPipeline { pass });
                }
                if *vertices > (1 << 24) {
                    return Err(VulkanError::OutOfBounds("vertex count"));
                }
                let (i, _) =
                    layouts.require(r, *target, &[vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL])?;
                let tex = &r.textures[i].1;
                Cmd::Draw {
                    pipeline: p.pipeline,
                    view: tex.view,
                    extent: vk::Extent2D {
                        width: tex.info().width,
                        height: tex.info().height,
                    },
                    clear: *clear,
                    vertices: *vertices,
                }
            }
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
                            let (i, layout) = layouts.require(r, *t, &SHADER_READ)?;
                            Write::Image(
                                descriptor_type(*want),
                                vk::DescriptorImageInfo {
                                    sampler: vk::Sampler::null(),
                                    image_view: r.textures[i].1.view,
                                    image_layout: layout,
                                },
                            )
                        }
                        (Bind::StorageTexture(t), BindingKind::StorageTexture) => {
                            let (i, layout) =
                                layouts.require(r, *t, &[vk::ImageLayout::GENERAL])?;
                            Write::Image(
                                descriptor_type(*want),
                                vk::DescriptorImageInfo {
                                    sampler: vk::Sampler::null(),
                                    image_view: r.textures[i].1.view,
                                    image_layout: layout,
                                },
                            )
                        }
                        (Bind::StorageBuffer(b), BindingKind::StorageBuffer) => {
                            Write::Buffer(vk::DescriptorBufferInfo {
                                buffer: find_buffer(r, *b)?.buffer,
                                offset: 0,
                                range: vk::WHOLE_SIZE,
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
                let (i, layout) = layouts.require(r, *src, &COPY_READ)?;
                let tex = &r.textures[i].1;
                let buf = find_buffer(r, *dst)?;
                let bytes = u64::from(tex.info().width) * u64::from(tex.info().height) * 4;
                if bytes > buf.size() {
                    return Err(VulkanError::OutOfBounds("readback buffer too small"));
                }
                Cmd::CopyTextureToBuffer {
                    image: tex.image,
                    layout,
                    extent: vk::Extent2D {
                        width: tex.info().width,
                        height: tex.info().height,
                    },
                    buffer: buf.buffer,
                }
            }
            PassWork::FillBuffer { dst, value } => Cmd::Fill {
                buffer: find_buffer(r, *dst)?.buffer,
                value: *value,
            },
        }))
    }

    /// Records one batch into `cb`.
    fn record(&self, cb: vk::CommandBuffer, batch: &Batch) -> Result<(), VulkanError> {
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
        for cmd in &batch.commands {
            // SAFETY (whole match): every handle was validated against the
            // frame's owners, which the runtime keeps alive until the
            // original signals of this submission complete; arrays borrow
            // locals that outlive each call.
            match cmd {
                Cmd::Barriers { buffers, images } => {
                    let b: Vec<_> = buffers
                        .iter()
                        .map(|(buffer, t)| {
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(t.src_stage)
                                .src_access_mask(t.src_access)
                                .dst_stage_mask(t.dst_stage)
                                .dst_access_mask(t.dst_access)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(*buffer)
                                .offset(0)
                                .size(vk::WHOLE_SIZE)
                        })
                        .collect();
                    let i: Vec<_> = images
                        .iter()
                        .map(|(image, t)| {
                            vk::ImageMemoryBarrier2::default()
                                .src_stage_mask(t.src_stage)
                                .src_access_mask(t.src_access)
                                .dst_stage_mask(t.dst_stage)
                                .dst_access_mask(t.dst_access)
                                .old_layout(t.old_layout)
                                .new_layout(t.new_layout)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .image(*image)
                                .subresource_range(t.range)
                        })
                        .collect();
                    let dep = vk::DependencyInfo::default()
                        .buffer_memory_barriers(&b)
                        .image_memory_barriers(&i);
                    unsafe { d.cmd_pipeline_barrier2(cb, &dep) };
                }
                Cmd::Draw {
                    pipeline,
                    view,
                    extent,
                    clear,
                    vertices,
                } => {
                    let attachment = [vk::RenderingAttachmentInfo::default()
                        .image_view(*view)
                        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .load_op(if clear.is_some() {
                            vk::AttachmentLoadOp::CLEAR
                        } else {
                            vk::AttachmentLoadOp::LOAD
                        })
                        .store_op(vk::AttachmentStoreOp::STORE)
                        .clear_value(vk::ClearValue {
                            color: vk::ClearColorValue {
                                float32: clear.unwrap_or_default(),
                            },
                        })];
                    let area = vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: *extent,
                    };
                    let info = vk::RenderingInfo::default()
                        .render_area(area)
                        .layer_count(1)
                        .color_attachments(&attachment);
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
                        d.cmd_draw(cb, *vertices, 1, 0, 0);
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
                                Write::Image(ty, _) => {
                                    base.descriptor_type(*ty).image_info(&images[i])
                                }
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
                Cmd::CopyTextureToBuffer {
                    image,
                    layout,
                    extent,
                    buffer,
                } => {
                    let region = vk::BufferImageCopy {
                        buffer_offset: 0,
                        buffer_row_length: 0,
                        buffer_image_height: 0,
                        image_subresource: vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
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
                Cmd::Fill { buffer, value } => unsafe {
                    d.cmd_fill_buffer(cb, *buffer, 0, vk::WHOLE_SIZE, *value)
                },
            }
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
        let (batches, layouts) = self.prepare(submission, owners).map_err(|e| (0, e))?;
        let d = &self.inner.device;
        // Reset each stream's pool once and allocate its command buffers.
        let mut per_stream: Vec<Vec<usize>> = vec![Vec::new(); submission.streams.len()];
        for (i, b) in batches.iter().enumerate() {
            if !b.commands.is_empty() || b.host_barrier {
                per_stream[b.segment.stream].push(i);
            }
        }
        let mut buffers = vec![vk::CommandBuffer::null(); batches.len()];
        for (s, list) in per_stream.iter().enumerate() {
            let stream = &submission.streams[s];
            let pool = owners
                .allocators
                .iter()
                .find(|a| a.timeline() == stream.timeline)
                .expect("checked in prepare")
                .value()
                .pool;
            // SAFETY: the pool's earlier command buffers belong to retired
            // frames (the runtime hands allocators out only after retirement).
            self.inner
                .check("vkResetCommandPool", unsafe {
                    d.reset_command_pool(pool, vk::CommandPoolResetFlags::empty())
                })
                .map_err(|e| (0, e))?;
            if list.is_empty() {
                continue;
            }
            let info = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(list.len() as u32);
            // SAFETY: valid pool.
            let cbs = self
                .inner
                .check("vkAllocateCommandBuffers", unsafe {
                    d.allocate_command_buffers(&info)
                })
                .map_err(|e| (0, e))?;
            for (slot, cb) in list.iter().zip(cbs) {
                buffers[*slot] = cb;
            }
        }
        for (b, cb) in batches.iter().zip(&buffers) {
            if *cb != vk::CommandBuffer::null() {
                self.record(*cb, b).map_err(|e| (0, e))?;
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
            let signal_infos: Vec<_> = batch
                .segment
                .signals
                .iter()
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
            // queue access is serialized by `&mut self`.
            let r = unsafe { d.queue_submit2(queue, &[submit], vk::Fence::null()) };
            self.inner.check("vkQueueSubmit2", r).map_err(|e| (n, e))?;
            for s in &batch.segment.signals {
                self.timelines[s.timeline.index()].submitted = s.value;
                self.pending.push_back(*s);
            }
        }
        layouts.commit(owners.resources);
        Ok(())
    }
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
