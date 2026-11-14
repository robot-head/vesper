//! A small Vulkan compute runtime: one device and queue, host-visible storage
//! buffers, compute kernels compiled from WGSL, recorded dispatches with
//! descriptor sets and immediates, and submissions ordered and completed by a
//! timeline semaphore.
//!
//! This is the only module of the crate that contains `unsafe` code. Every
//! Vulkan call goes through `ash`. The safety argument for each block is in
//! the comment next to it; the invariants it relies on are:
//!
//! * every Vulkan object is owned by exactly one Rust value, and each owner
//!   holds an [`Arc`] of the device, so the device outlives all its objects;
//! * a [`Recording`] borrows every buffer it binds for its lifetime `'a`, and
//!   the [`Submission`] it becomes keeps that borrow until the GPU has
//!   signalled its timeline value, so a buffer cannot be written by the host,
//!   or freed, while a dispatch can still read it;
//! * host access to mapped memory happens only through `&Buffer` / `&mut
//!   Buffer`, which the borrow checker excludes while a submission is live.
//!
//! Memory is host-visible and host-coherent. On lavapipe all memory is; on a
//! discrete GPU this would put buffers in system memory, which is correct but
//! slow. Device-local staging is a remaining gap (see the lane report).
use std::ffi::c_char;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ash::vk;

use crate::error::GpuError;
use crate::shader::{self, ShaderError};

/// Number of [`Submission`]s dropped without [`Submission::wait`] in this
/// process. The drop still waits for the GPU (so no memory is freed while in
/// use), but the missed obligation is counted, never silently ignored.
pub fn leaked_submissions() -> usize {
    LEAKED_SUBMISSIONS.load(Ordering::Relaxed)
}
static LEAKED_SUBMISSIONS: AtomicUsize = AtomicUsize::new(0);

/// Upper bound on the bytes of one buffer (1 GiB). Larger requests are a
/// typed [`GpuError::Limit`].
pub const MAX_BUFFER_BYTES: u64 = 1 << 30;
/// Upper bound on the storage buffers one kernel binds.
pub const MAX_BINDINGS: u32 = 8;
/// Upper bound on a kernel's immediate (push-constant) block, the Vulkan
/// guaranteed minimum.
pub const MAX_IMMEDIATE_BYTES: u32 = 128;
/// Descriptor sets per pool before a recording chains another pool.
const SETS_PER_POOL: u32 = 256;

/// What the runtime found when it opened the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    /// `VkPhysicalDeviceDriverProperties::driverName`, e.g. `llvmpipe`.
    pub driver_name: String,
    /// `VkPhysicalDeviceDriverProperties::driverInfo`, e.g. `Mesa 25.2.8 (LLVM 20.1.2)`.
    pub driver_info: String,
    /// `major.minor.patch` of the device's Vulkan API version.
    pub api_version: (u32, u32, u32),
    pub device_type: DeviceType,
    pub max_workgroup_invocations: u32,
    pub max_storage_buffer_range: u32,
}

impl DeviceInfo {
    /// Whether this is Mesa's software rasterizer (lavapipe / llvmpipe).
    pub fn is_lavapipe(&self) -> bool {
        self.device_type == DeviceType::Cpu
            && (self.name.contains("llvmpipe") || self.driver_name.contains("llvmpipe"))
    }
}

/// `VkPhysicalDeviceType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceType {
    Other,
    IntegratedGpu,
    DiscreteGpu,
    VirtualGpu,
    Cpu,
}

struct DeviceInner {
    // Field order is irrelevant: `Drop` below destroys explicitly.
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: Mutex<vk::Queue>,
    queue_family: u32,
    memory: vk::PhysicalDeviceMemoryProperties,
    timeline: vk::Semaphore,
    /// Last timeline value handed to a submission.
    issued: AtomicU64,
    info: DeviceInfo,
}

impl Drop for DeviceInner {
    fn drop(&mut self) {
        // SAFETY: every child object holds an `Arc<DeviceInner>`, so they have
        // all been destroyed by now; waiting idle first makes destroying the
        // semaphore and device valid even if a submission was leaked by
        // `mem::forget`.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_semaphore(self.timeline, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

/// An open Vulkan device with one compute queue.
#[derive(Clone)]
pub struct Context {
    inner: Arc<DeviceInner>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("info", &self.inner.info)
            .finish()
    }
}

fn vk_err(call: &'static str) -> impl Fn(vk::Result) -> GpuError {
    move |r| GpuError::Vulkan {
        call,
        code: r.as_raw(),
    }
}

fn cstr_field(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Context {
    /// Loads the system Vulkan loader, creates an instance and opens the
    /// first physical device that has a compute queue and supports Vulkan 1.2
    /// timeline semaphores. If `VESPER_GPU_DEVICE` is set, only a device whose
    /// name contains it is accepted.
    ///
    /// No loader, no instance or no suitable device is
    /// [`GpuError::Unavailable`]; callers (and the tests) report that instead
    /// of claiming a result.
    pub fn new() -> Result<Self, GpuError> {
        // SAFETY: `Entry::load` dlopens the platform Vulkan loader; the
        // returned function pointers are valid while `entry` lives, and
        // `entry` is kept in `DeviceInner` for as long as any object exists.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| GpuError::Unavailable(format!("no Vulkan loader: {e}")))?;
        // SAFETY: plain query on a valid entry.
        let instance_version = unsafe { entry.try_enumerate_instance_version() }
            .map_err(vk_err("vkEnumerateInstanceVersion"))?
            .unwrap_or(vk::API_VERSION_1_0);
        if instance_version < vk::API_VERSION_1_2 {
            return Err(GpuError::Unavailable(format!(
                "Vulkan instance version {}.{} is below 1.2",
                vk::api_version_major(instance_version),
                vk::api_version_minor(instance_version)
            )));
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"vesper-gpu-compute")
            .engine_name(c"vesper")
            .api_version(vk::API_VERSION_1_2);
        let create = vk::InstanceCreateInfo::default().application_info(&app);
        // SAFETY: `create` and `app` are valid for the call; no extensions or
        // layers are requested.
        let instance = unsafe { entry.create_instance(&create, None) }
            .map_err(|e| GpuError::Unavailable(format!("vkCreateInstance failed: {e}")))?;
        match Self::open(entry, instance) {
            Ok(ctx) => Ok(ctx),
            Err((instance, err)) => {
                // SAFETY: no child object of `instance` exists on this path.
                unsafe { instance.destroy_instance(None) };
                Err(err)
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn open(entry: ash::Entry, instance: ash::Instance) -> Result<Self, (ash::Instance, GpuError)> {
        let wanted = std::env::var("VESPER_GPU_DEVICE").ok();
        // SAFETY: valid instance.
        let physical = match unsafe { instance.enumerate_physical_devices() } {
            Ok(p) => p,
            Err(e) => return Err((instance, vk_err("vkEnumeratePhysicalDevices")(e))),
        };
        let mut chosen = None;
        for pd in physical {
            let mut driver = vk::PhysicalDeviceDriverProperties::default();
            let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
            // SAFETY: `pd` came from this instance; `props2` chain is valid.
            unsafe { instance.get_physical_device_properties2(pd, &mut props2) };
            let props = props2.properties;
            if props.api_version < vk::API_VERSION_1_2 {
                continue;
            }
            let name = cstr_field(&props.device_name);
            if let Some(w) = &wanted
                && !name.contains(w.as_str())
            {
                continue;
            }
            let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
            let mut features2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut timeline);
            // SAFETY: as above.
            unsafe { instance.get_physical_device_features2(pd, &mut features2) };
            if timeline.timeline_semaphore == vk::FALSE {
                continue;
            }
            // SAFETY: as above.
            let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
            let Some(family) = families
                .iter()
                .position(|f| f.queue_flags.contains(vk::QueueFlags::COMPUTE))
            else {
                continue;
            };
            let device_type = match props.device_type {
                vk::PhysicalDeviceType::INTEGRATED_GPU => DeviceType::IntegratedGpu,
                vk::PhysicalDeviceType::DISCRETE_GPU => DeviceType::DiscreteGpu,
                vk::PhysicalDeviceType::VIRTUAL_GPU => DeviceType::VirtualGpu,
                vk::PhysicalDeviceType::CPU => DeviceType::Cpu,
                _ => DeviceType::Other,
            };
            let info = DeviceInfo {
                name,
                driver_name: cstr_field(&driver.driver_name),
                driver_info: cstr_field(&driver.driver_info),
                api_version: (
                    vk::api_version_major(props.api_version),
                    vk::api_version_minor(props.api_version),
                    vk::api_version_patch(props.api_version),
                ),
                device_type,
                max_workgroup_invocations: props.limits.max_compute_work_group_invocations,
                max_storage_buffer_range: props.limits.max_storage_buffer_range,
            };
            chosen = Some((pd, family as u32, info));
            break;
        }
        let Some((pd, queue_family, info)) = chosen else {
            return Err((
                instance,
                GpuError::Unavailable(match wanted {
                    Some(w) => format!("no Vulkan 1.2 compute device whose name contains {w:?}"),
                    None => "no Vulkan 1.2 compute device with timeline semaphores".into(),
                }),
            ));
        };
        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let mut features12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
        let create = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .push_next(&mut features12);
        // SAFETY: `pd` supports the requested queue family and feature
        // (checked above); the create-info chain lives for the call.
        let device = match unsafe { instance.create_device(pd, &create, None) } {
            Ok(d) => d,
            Err(e) => return Err((instance, vk_err("vkCreateDevice")(e))),
        };
        // SAFETY: family `queue_family` with one queue was created above.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        // SAFETY: valid physical device.
        let memory = unsafe { instance.get_physical_device_memory_properties(pd) };
        let mut type_info = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let sem_create = vk::SemaphoreCreateInfo::default().push_next(&mut type_info);
        // SAFETY: timeline semaphores were enabled at device creation.
        let timeline = match unsafe { device.create_semaphore(&sem_create, None) } {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: no other child of `device` exists yet.
                unsafe { device.destroy_device(None) };
                return Err((instance, vk_err("vkCreateSemaphore")(e)));
            }
        };
        Ok(Self {
            inner: Arc::new(DeviceInner {
                _entry: entry,
                instance,
                device,
                queue: Mutex::new(queue),
                queue_family,
                memory,
                timeline,
                issued: AtomicU64::new(0),
                info,
            }),
        })
    }

    /// The opened device.
    pub fn info(&self) -> &DeviceInfo {
        &self.inner.info
    }

    /// The timeline value of the last completed submission.
    pub fn completed_value(&self) -> Result<u64, GpuError> {
        // SAFETY: the semaphore belongs to this device and is a timeline one.
        unsafe {
            self.inner
                .device
                .get_semaphore_counter_value(self.inner.timeline)
        }
        .map_err(vk_err("vkGetSemaphoreCounterValue"))
    }

    /// The timeline value of the last submission handed out.
    pub fn issued_value(&self) -> u64 {
        self.inner.issued.load(Ordering::Acquire)
    }

    /// A zero-filled storage buffer of `bytes` bytes (rounded up to 4).
    pub fn buffer(&self, bytes: u64) -> Result<Buffer, GpuError> {
        Buffer::new(self, bytes)
    }

    /// A storage buffer holding `data`.
    pub fn buffer_from<T: bytemuck::Pod>(&self, data: &[T]) -> Result<Buffer, GpuError> {
        let bytes = std::mem::size_of_val(data) as u64;
        let mut buffer = Buffer::new(self, bytes.max(4))?;
        buffer.write(0, data)?;
        Ok(buffer)
    }

    /// Compiles `wgsl` with naga and creates a compute pipeline for `entry`.
    /// The module must bind exactly `bindings` storage buffers at
    /// `@group(0) @binding(0..bindings)` and may declare one
    /// `var<immediate>` block of at most `immediate_bytes` bytes.
    pub fn kernel(
        &self,
        wgsl: &str,
        entry: &str,
        bindings: u32,
        immediate_bytes: u32,
    ) -> Result<Kernel, GpuError> {
        let compiled = shader::compile_wgsl(wgsl, entry)?;
        if compiled.storage_bindings != bindings {
            return Err(GpuError::Shader(ShaderError::Interface(format!(
                "kernel {entry} binds {} storage buffers, expected {bindings}",
                compiled.storage_bindings
            ))));
        }
        if compiled.immediate_bytes > immediate_bytes {
            return Err(GpuError::Shader(ShaderError::Interface(format!(
                "kernel {entry} declares {} immediate bytes, expected at most {immediate_bytes}",
                compiled.immediate_bytes
            ))));
        }
        Kernel::new(self, &compiled.spirv, entry, bindings, immediate_bytes)
    }

    /// Starts recording a command buffer.
    pub fn record(&self) -> Result<Recording<'_>, GpuError> {
        Recording::new(self)
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        let m = &self.inner.memory;
        (0..m.memory_type_count).find(|&i| {
            bits & (1 << i) != 0 && m.memory_types[i as usize].property_flags.contains(flags)
        })
    }
}

/// A host-visible, host-coherent storage buffer, mapped for its lifetime.
pub struct Buffer {
    ctx: Arc<DeviceInner>,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    len: u64,
}

// SAFETY: the mapped pointer is only dereferenced through `&self`/`&mut self`
// methods, so Rust's aliasing rules apply to it as to a `Vec<u8>`; the Vulkan
// handles are plain identifiers usable from any thread.
unsafe impl Send for Buffer {}
// SAFETY: see above; shared references only read.
unsafe impl Sync for Buffer {}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer").field("len", &self.len).finish()
    }
}

impl Buffer {
    fn new(ctx: &Context, bytes: u64) -> Result<Self, GpuError> {
        if bytes == 0 {
            return Err(GpuError::Shape("zero-sized buffer"));
        }
        if bytes > MAX_BUFFER_BYTES || bytes > u64::from(ctx.inner.info.max_storage_buffer_range) {
            return Err(GpuError::Limit("buffer size"));
        }
        let len = bytes.div_ceil(4) * 4;
        let d = &ctx.inner.device;
        let create = vk::BufferCreateInfo::default()
            .size(len)
            .usage(
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: valid device and create info.
        let buffer = unsafe { d.create_buffer(&create, None) }.map_err(vk_err("vkCreateBuffer"))?;
        // SAFETY: `buffer` was just created on `d`.
        let req = unsafe { d.get_buffer_memory_requirements(buffer) };
        let flags = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let Some(type_index) = ctx.memory_type(req.memory_type_bits, flags) else {
            // SAFETY: unbound, unused buffer.
            unsafe { d.destroy_buffer(buffer, None) };
            return Err(GpuError::Unsupported(
                "no host-visible coherent memory type for storage buffers",
            ));
        };
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(type_index);
        // SAFETY: valid allocate info for this device.
        let memory = match unsafe { d.allocate_memory(&alloc, None) } {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: unbound, unused buffer.
                unsafe { d.destroy_buffer(buffer, None) };
                return Err(vk_err("vkAllocateMemory")(e));
            }
        };
        let cleanup = |d: &ash::Device| {
            // SAFETY: neither object is in use by the device yet.
            unsafe {
                d.destroy_buffer(buffer, None);
                d.free_memory(memory, None);
            }
        };
        // SAFETY: `memory` satisfies `req` and is not bound to anything else.
        if let Err(e) = unsafe { d.bind_buffer_memory(buffer, memory, 0) } {
            cleanup(d);
            return Err(vk_err("vkBindBufferMemory")(e));
        }
        // SAFETY: host-visible memory, mapped once for the whole allocation.
        let ptr =
            match unsafe { d.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) } {
                Ok(p) => p.cast::<u8>(),
                Err(e) => {
                    cleanup(d);
                    return Err(vk_err("vkMapMemory")(e));
                }
            };
        // SAFETY: the mapping covers at least `len` bytes (req.size >= len);
        // zeroing gives deterministic initial contents.
        unsafe { std::ptr::write_bytes(ptr, 0, len as usize) };
        Ok(Self {
            ctx: ctx.inner.clone(),
            buffer,
            memory,
            ptr,
            len,
        })
    }

    /// Size in bytes (a multiple of 4).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Always false: zero-sized buffers are rejected at creation.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mapped contents.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` maps `len` bytes for the buffer's lifetime; no
        // submission can write it while `&self` exists outside a recording
        // (a live `Submission` borrows the buffer).
        unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) }
    }

    /// The mapped contents, writable.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`, with exclusive access through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len as usize) }
    }

    /// Copies `data` to byte offset `offset`.
    pub fn write<T: bytemuck::Pod>(&mut self, offset: u64, data: &[T]) -> Result<(), GpuError> {
        let src: &[u8] = bytemuck::cast_slice(data);
        let start = usize::try_from(offset).map_err(|_| GpuError::Shape("buffer offset"))?;
        let end = start
            .checked_add(src.len())
            .ok_or(GpuError::Shape("buffer write range"))?;
        let dst = self
            .bytes_mut()
            .get_mut(start..end)
            .ok_or(GpuError::Shape("buffer write range"))?;
        dst.copy_from_slice(src);
        Ok(())
    }

    /// Copies from byte offset `offset` into `out`.
    pub fn read<T: bytemuck::Pod>(&self, offset: u64, out: &mut [T]) -> Result<(), GpuError> {
        let dst: &mut [u8] = bytemuck::cast_slice_mut(out);
        let start = usize::try_from(offset).map_err(|_| GpuError::Shape("buffer offset"))?;
        let end = start
            .checked_add(dst.len())
            .ok_or(GpuError::Shape("buffer read range"))?;
        let src = self
            .bytes()
            .get(start..end)
            .ok_or(GpuError::Shape("buffer read range"))?;
        dst.copy_from_slice(src);
        Ok(())
    }

    /// The whole buffer as `u32` words.
    pub fn to_u32(&self) -> Vec<u32> {
        self.bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn same_device(&self, ctx: &Context) -> bool {
        Arc::ptr_eq(&self.ctx, &ctx.inner)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: a buffer bound into a live submission is borrowed by it, so
        // it cannot be dropped before the submission completed.
        unsafe {
            self.ctx.device.unmap_memory(self.memory);
            self.ctx.device.destroy_buffer(self.buffer, None);
            self.ctx.device.free_memory(self.memory, None);
        }
    }
}

/// A compute pipeline with `bindings` storage buffers and an optional
/// immediate block.
pub struct Kernel {
    ctx: Arc<DeviceInner>,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    bindings: u32,
    immediate_bytes: u32,
    name: String,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel")
            .field("name", &self.name)
            .field("bindings", &self.bindings)
            .finish()
    }
}

impl Kernel {
    fn new(
        ctx: &Context,
        spirv: &[u32],
        entry: &str,
        bindings: u32,
        immediate_bytes: u32,
    ) -> Result<Self, GpuError> {
        if bindings == 0 || bindings > MAX_BINDINGS {
            return Err(GpuError::Limit("kernel storage bindings"));
        }
        if immediate_bytes > MAX_IMMEDIATE_BYTES || !immediate_bytes.is_multiple_of(4) {
            return Err(GpuError::Limit("kernel immediate bytes"));
        }
        let d = &ctx.inner.device;
        let layout_bindings: Vec<_> = (0..bindings)
            .map(|b| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        let set_create = vk::DescriptorSetLayoutCreateInfo::default().bindings(&layout_bindings);
        // SAFETY: valid create info.
        let set_layout = unsafe { d.create_descriptor_set_layout(&set_create, None) }
            .map_err(vk_err("vkCreateDescriptorSetLayout"))?;
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(immediate_bytes)];
        let set_layouts = [set_layout];
        let mut layout_create = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        if immediate_bytes > 0 {
            layout_create = layout_create.push_constant_ranges(&ranges);
        }
        // SAFETY: valid create info referencing a live set layout.
        let layout = match unsafe { d.create_pipeline_layout(&layout_create, None) } {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: unused layout.
                unsafe { d.destroy_descriptor_set_layout(set_layout, None) };
                return Err(vk_err("vkCreatePipelineLayout")(e));
            }
        };
        let destroy_layouts = |d: &ash::Device| {
            // SAFETY: neither layout is used by a pipeline yet.
            unsafe {
                d.destroy_pipeline_layout(layout, None);
                d.destroy_descriptor_set_layout(set_layout, None);
            }
        };
        let module_create = vk::ShaderModuleCreateInfo::default().code(spirv);
        // SAFETY: `spirv` is a SPIR-V module produced and validated by naga.
        let module = match unsafe { d.create_shader_module(&module_create, None) } {
            Ok(m) => m,
            Err(e) => {
                destroy_layouts(d);
                return Err(vk_err("vkCreateShaderModule")(e));
            }
        };
        let entry_c = std::ffi::CString::new(entry)
            .map_err(|_| GpuError::Shape("kernel entry point contains NUL"))?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(entry_c.as_c_str());
        let create = [vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout)];
        // SAFETY: valid module, entry point and layout.
        let result =
            unsafe { d.create_compute_pipelines(vk::PipelineCache::null(), &create, None) };
        // SAFETY: the module is no longer needed once the pipeline exists.
        unsafe { d.destroy_shader_module(module, None) };
        let pipeline = match result {
            Ok(p) => p[0],
            Err((_, e)) => {
                destroy_layouts(d);
                return Err(vk_err("vkCreateComputePipelines")(e));
            }
        };
        Ok(Self {
            ctx: ctx.inner.clone(),
            set_layout,
            layout,
            pipeline,
            bindings,
            immediate_bytes,
            name: entry.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        // SAFETY: recordings that use this kernel borrow it, so none is live.
        unsafe {
            self.ctx.device.destroy_pipeline(self.pipeline, None);
            self.ctx.device.destroy_pipeline_layout(self.layout, None);
            self.ctx
                .device
                .destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// Command-buffer resources owned by one recording, freed when its
/// submission completes.
struct Commands {
    ctx: Arc<DeviceInner>,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    descriptor_pools: Vec<vk::DescriptorPool>,
    sets_left: u32,
}

impl Drop for Commands {
    fn drop(&mut self) {
        // SAFETY: only reached once the command buffer is not pending
        // (never submitted, or its timeline value was waited for).
        unsafe {
            for p in &self.descriptor_pools {
                self.ctx.device.destroy_descriptor_pool(*p, None);
            }
            self.ctx.device.destroy_command_pool(self.pool, None);
        }
    }
}

/// A command buffer being recorded. Every buffer and kernel it uses is
/// borrowed for `'a`. A full compute-to-compute memory barrier follows each
/// dispatch, so dispatches observe their predecessors' writes in order.
#[must_use = "a recording does nothing until it is submitted"]
pub struct Recording<'a> {
    ctx: &'a Context,
    commands: Commands,
    dispatches: usize,
    _borrows: PhantomData<&'a Buffer>,
}

impl<'a> Recording<'a> {
    fn new(ctx: &'a Context) -> Result<Self, GpuError> {
        let d = &ctx.inner.device;
        let pool_create = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ctx.inner.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: valid create info.
        let pool = unsafe { d.create_command_pool(&pool_create, None) }
            .map_err(vk_err("vkCreateCommandPool"))?;
        let mut commands = Commands {
            ctx: ctx.inner.clone(),
            pool,
            cmd: vk::CommandBuffer::null(),
            descriptor_pools: Vec::new(),
            sets_left: 0,
        };
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: valid pool.
        commands.cmd = unsafe { d.allocate_command_buffers(&alloc) }
            .map_err(vk_err("vkAllocateCommandBuffers"))?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: freshly allocated command buffer in the initial state.
        unsafe { d.begin_command_buffer(commands.cmd, &begin) }
            .map_err(vk_err("vkBeginCommandBuffer"))?;
        Ok(Self {
            ctx,
            commands,
            dispatches: 0,
            _borrows: PhantomData,
        })
    }

    /// Dispatches recorded so far.
    pub fn dispatches(&self) -> usize {
        self.dispatches
    }

    fn descriptor_set(&mut self, kernel: &Kernel) -> Result<vk::DescriptorSet, GpuError> {
        let d = &self.ctx.inner.device;
        if self.commands.sets_left == 0 {
            let sizes = [vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(SETS_PER_POOL * MAX_BINDINGS)];
            let create = vk::DescriptorPoolCreateInfo::default()
                .max_sets(SETS_PER_POOL)
                .pool_sizes(&sizes);
            // SAFETY: valid create info.
            let pool = unsafe { d.create_descriptor_pool(&create, None) }
                .map_err(vk_err("vkCreateDescriptorPool"))?;
            self.commands.descriptor_pools.push(pool);
            self.commands.sets_left = SETS_PER_POOL;
        }
        let pool = *self
            .commands
            .descriptor_pools
            .last()
            .ok_or(GpuError::Shape("descriptor pool"))?;
        let layouts = [kernel.set_layout];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: the pool has a set and `MAX_BINDINGS` descriptors left.
        let set = unsafe { d.allocate_descriptor_sets(&alloc) }
            .map_err(vk_err("vkAllocateDescriptorSets"))?[0];
        self.commands.sets_left -= 1;
        Ok(set)
    }

    /// Records `kernel` over `groups` workgroups with `buffers` bound to
    /// bindings `0..` and `immediates` as its immediate block, followed by a
    /// compute-to-compute (and compute-to-host) memory barrier.
    pub fn dispatch(
        &mut self,
        kernel: &'a Kernel,
        buffers: &[&'a Buffer],
        immediates: &[u32],
        groups: [u32; 3],
    ) -> Result<(), GpuError> {
        if !Arc::ptr_eq(&kernel.ctx, &self.ctx.inner)
            || buffers.iter().any(|b| !b.same_device(self.ctx))
        {
            return Err(GpuError::ForeignObject);
        }
        if buffers.len() != kernel.bindings as usize {
            return Err(GpuError::Shape(
                "dispatch buffer count differs from the kernel",
            ));
        }
        if std::mem::size_of_val(immediates) != kernel.immediate_bytes as usize {
            return Err(GpuError::Shape(
                "dispatch immediates differ from the kernel",
            ));
        }
        if groups.contains(&0) {
            return Err(GpuError::Shape("dispatch with zero workgroups"));
        }
        if groups.iter().any(|g| *g > 65_535) {
            return Err(GpuError::Limit("workgroup count"));
        }
        let set = self.descriptor_set(kernel)?;
        let infos: Vec<[vk::DescriptorBufferInfo; 1]> = buffers
            .iter()
            .map(|b| {
                [vk::DescriptorBufferInfo::default()
                    .buffer(b.buffer)
                    .offset(0)
                    .range(vk::WHOLE_SIZE)]
            })
            .collect();
        let writes: Vec<_> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(info)
            })
            .collect();
        let d = &self.ctx.inner.device;
        let cmd = self.commands.cmd;
        let barrier = [vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::SHADER_WRITE)
            .dst_access_mask(
                vk::AccessFlags::SHADER_READ
                    | vk::AccessFlags::SHADER_WRITE
                    | vk::AccessFlags::HOST_READ,
            )];
        // SAFETY: `set` was allocated for `kernel`'s layout and every buffer
        // is a live storage buffer of this device, borrowed for `'a`; the
        // command buffer is in the recording state.
        unsafe {
            d.update_descriptor_sets(&writes, &[]);
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, kernel.pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                kernel.layout,
                0,
                &[set],
                &[],
            );
            if !immediates.is_empty() {
                d.cmd_push_constants(
                    cmd,
                    kernel.layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::cast_slice(immediates),
                );
            }
            d.cmd_dispatch(cmd, groups[0], groups[1], groups[2]);
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::COMPUTE_SHADER | vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &barrier,
                &[],
                &[],
            );
        }
        self.dispatches += 1;
        Ok(())
    }

    /// Ends recording and submits to the queue. The submission waits for the
    /// previous timeline value (so submissions complete in order and see each
    /// other's writes) and signals a new one.
    pub fn submit(self) -> Result<Submission<'a>, GpuError> {
        let inner = &self.ctx.inner;
        let d = &inner.device;
        let cmd = self.commands.cmd;
        // SAFETY: the command buffer is in the recording state.
        unsafe { d.end_command_buffer(cmd) }.map_err(vk_err("vkEndCommandBuffer"))?;
        let queue = inner
            .queue
            .lock()
            .map_err(|_| GpuError::Shape("queue mutex poisoned"))?;
        let previous = inner.issued.load(Ordering::Acquire);
        let value = previous + 1;
        let wait_values = [previous];
        let signal_values = [value];
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let semaphores = [inner.timeline];
        let stages = [vk::PipelineStageFlags::COMPUTE_SHADER];
        let cmds = [cmd];
        let submit = [vk::SubmitInfo::default()
            .wait_semaphores(&semaphores)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&semaphores)
            .push_next(&mut timeline)];
        // SAFETY: the queue is externally synchronized by the mutex; the
        // command buffer is executable; wait and signal values increase
        // monotonically because `issued` is only advanced under the mutex.
        unsafe { d.queue_submit(*queue, &submit, vk::Fence::null()) }
            .map_err(vk_err("vkQueueSubmit"))?;
        inner.issued.store(value, Ordering::Release);
        drop(queue);
        Ok(Submission {
            ctx: self.ctx,
            commands: Some(self.commands),
            value,
            _borrows: PhantomData,
        })
    }
}

/// Work in flight on the queue, identified by its timeline value. It borrows
/// every buffer the recording bound until [`Submission::wait`] consumes it.
/// Dropping it without waiting is counted by [`leaked_submissions`] (the drop
/// still blocks until the GPU is done, so nothing is freed while in use).
#[must_use = "a submission must be waited for with Submission::wait"]
pub struct Submission<'a> {
    ctx: &'a Context,
    commands: Option<Commands>,
    value: u64,
    _borrows: PhantomData<&'a Buffer>,
}

impl std::fmt::Debug for Submission<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Submission")
            .field("value", &self.value)
            .finish()
    }
}

/// Proof that a submission completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completed {
    pub value: u64,
}

impl Submission<'_> {
    /// The timeline value this submission signals.
    pub fn value(&self) -> u64 {
        self.value
    }

    /// Whether the GPU has signalled this submission's value.
    pub fn is_complete(&self) -> Result<bool, GpuError> {
        Ok(self.ctx.completed_value()? >= self.value)
    }

    fn block(&self) -> Result<(), GpuError> {
        let semaphores = [self.ctx.inner.timeline];
        let values = [self.value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: waiting on a timeline semaphore of this device.
        unsafe { self.ctx.inner.device.wait_semaphores(&info, u64::MAX) }
            .map_err(vk_err("vkWaitSemaphores"))
    }

    /// Blocks until the GPU signals this submission's timeline value, then
    /// frees its command buffer and descriptor sets and releases the borrows.
    pub fn wait(mut self) -> Result<Completed, GpuError> {
        let result = self.block();
        if result.is_ok() {
            self.commands = None; // frees command resources
        } else {
            // Device lost or similar: wait for idle before anything is freed.
            // SAFETY: valid device.
            let _ = unsafe { self.ctx.inner.device.device_wait_idle() };
            self.commands = None;
        }
        let value = self.value;
        std::mem::forget(self);
        result.map(|()| Completed { value })
    }
}

impl Drop for Submission<'_> {
    fn drop(&mut self) {
        LEAKED_SUBMISSIONS.fetch_add(1, Ordering::Relaxed);
        if self.block().is_err() {
            // SAFETY: valid device.
            let _ = unsafe { self.ctx.inner.device.device_wait_idle() };
        }
        self.commands = None;
    }
}
