//! The Vulkan compute runtime over the Vesper port (feature `vesper-port`,
//! SH6 lane 15, #1410). Every Vulkan call is made by the Vesper library in
//! `vesper/` (`vesper/src/lib.ves`), which imports the loader and libc
//! through `.vsys` with no Rust in between. This module keeps the crate's
//! public API and its ownership rules on top of it, so the crate's tests run
//! unchanged against both builds:
//!
//! * every Vulkan object is owned through an [`Arc`], and each owner holds an
//!   `Arc` of the device, so the device outlives all its objects;
//! * a [`Recording`] holds an `Arc` of every buffer and kernel it binds until
//!   its command pool is freed, which happens only after the GPU signalled the
//!   submission's timeline value (or never, if the [`Submission`] is leaked);
//! * host access to mapped memory is only by copying ([`Buffer::read`],
//!   [`Buffer::write`]), in Vesper, under the queue lock, after waiting for
//!   the last submission that bound the buffer.
//!
//! WGSL is still compiled with naga here ([`crate::shader`]); the Vesper
//! library takes the SPIR-V words.
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::GpuError;
use crate::shader::{self, ShaderError};

#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;
use binding::{Buf, Dev, GpuErr, Kern, Rec};

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

/// The Vulkan calls the Vesper library reports by index (`c_*` in
/// vesper/src/lib.ves).
const CALLS: [&str; 23] = [
    "?",
    "vkEnumerateInstanceVersion",
    "vkEnumeratePhysicalDevices",
    "vkCreateDevice",
    "vkCreateSemaphore",
    "vkGetSemaphoreCounterValue",
    "vkWaitSemaphores",
    "vkCreateBuffer",
    "vkAllocateMemory",
    "vkBindBufferMemory",
    "vkMapMemory",
    "vkCreateDescriptorSetLayout",
    "vkCreatePipelineLayout",
    "vkCreateShaderModule",
    "vkCreateComputePipelines",
    "vkCreateCommandPool",
    "vkAllocateCommandBuffers",
    "vkBeginCommandBuffer",
    "vkCreateDescriptorPool",
    "vkAllocateDescriptorSets",
    "vkEndCommandBuffer",
    "vkQueueSubmit",
    "vkDeviceWaitIdle",
];
/// `s_*` details.
const SHAPES: [&str; 8] = [
    "?",
    "zero-sized buffer",
    "buffer range",
    "dispatch buffer count differs from the kernel",
    "dispatch immediates differ from the kernel",
    "dispatch with zero workgroups",
    "descriptor pool",
    "kernel entry point contains NUL",
];
/// `l_*` details.
const LIMITS: [&str; 5] = [
    "?",
    "buffer size",
    "kernel storage bindings",
    "kernel immediate bytes",
    "workgroup count",
];

fn api_parts(v: u64) -> (u32, u32, u32) {
    let v = v as u32;
    ((v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
}

fn gpu_error(e: GpuErr, wanted: Option<&str>) -> GpuError {
    let pick = |table: &[&'static str], i: u32| table.get(i as usize).copied().unwrap_or("?");
    match e.kind {
        1 => GpuError::Unavailable(match e.detail {
            2 => {
                let (major, minor, _) = api_parts(e.code as u64);
                format!("Vulkan instance version {major}.{minor} is below 1.2")
            }
            3 => format!("vkCreateInstance failed: VkResult {}", e.code),
            _ => match wanted {
                Some(w) => format!("no Vulkan 1.2 compute device whose name contains {w:?}"),
                None => "no Vulkan 1.2 compute device with timeline semaphores".into(),
            },
        }),
        2 => GpuError::Vulkan {
            call: pick(&CALLS, e.detail),
            code: e.code as i32,
        },
        3 => GpuError::Shape(pick(&SHAPES, e.detail)),
        4 => GpuError::Limit(pick(&LIMITS, e.detail)),
        _ => GpuError::Unsupported("no host-visible coherent memory type for storage buffers"),
    }
}

/// Converts a Vesper failure. Arithmetic or indexing failures inside the
/// library are bugs of the port, not input errors.
macro_rules! vesper {
    ($call:expr, $err:ident) => {
        vesper!($call, $err, None)
    };
    ($call:expr, $err:ident, $wanted:expr) => {
        $call.map_err(|e| match e {
            binding::$err::GpuErr(g) => gpu_error(g, $wanted),
            other => panic!("the Vesper GPU runtime failed internally: {other:?}"),
        })
    };
}

struct DeviceInner {
    dev: Dev,
    queue: Mutex<()>,
    /// Last timeline value handed to a submission.
    issued: AtomicU64,
    info: DeviceInfo,
}

impl Drop for DeviceInner {
    fn drop(&mut self) {
        // Every child object holds an `Arc<DeviceInner>`, so they have all
        // been destroyed by now; `close` waits idle first.
        binding::close(self.dev);
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

fn wait_error(e: binding::WaitValueError) -> GpuError {
    match e {
        binding::WaitValueError::GpuErr(g) => gpu_error(g, None),
        other => panic!("the Vesper GPU runtime failed internally: {other:?}"),
    }
}

impl Context {
    /// Creates an instance and opens the first physical device that has a
    /// compute queue and supports Vulkan 1.2 timeline semaphores. If
    /// `VESPER_GPU_DEVICE` is set, only a device whose name contains it is
    /// accepted.
    ///
    /// No instance or no suitable device is [`GpuError::Unavailable`];
    /// callers (and the tests) report that instead of claiming a result.
    /// The loader itself is linked, so a host without it cannot run this
    /// build at all (docs/parallel-results/rust-exit-graphics.md, gap 7).
    pub fn new() -> Result<Self, GpuError> {
        let wanted = std::env::var("VESPER_GPU_DEVICE").ok();
        let dev = vesper!(
            binding::open(wanted.as_deref().unwrap_or("")),
            OpenError,
            wanted.as_deref()
        )?;
        let strings = match binding::device_strings(dev) {
            Ok(s) => s,
            Err(e) => {
                binding::close(dev);
                panic!("the Vesper GPU runtime failed internally: {e:?}");
            }
        };
        let mut parts = strings
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned());
        let name = parts.next().unwrap_or_default();
        let driver_name = parts.next().unwrap_or_default();
        let driver_info = parts.next().unwrap_or_default();
        let device_type = match dev.device_type {
            1 => DeviceType::IntegratedGpu,
            2 => DeviceType::DiscreteGpu,
            3 => DeviceType::VirtualGpu,
            4 => DeviceType::Cpu,
            _ => DeviceType::Other,
        };
        let info = DeviceInfo {
            name,
            driver_name,
            driver_info,
            api_version: api_parts(dev.api_version),
            device_type,
            max_workgroup_invocations: dev.max_invocations as u32,
            max_storage_buffer_range: dev.max_range as u32,
        };
        Ok(Self {
            inner: Arc::new(DeviceInner {
                dev,
                queue: Mutex::new(()),
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
        binding::completed(self.inner.dev).map_err(|g| gpu_error(g, None))
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
        let spirv: &[u8] = bytemuck::cast_slice(&compiled.spirv);
        let kern = vesper!(
            binding::kernel_new(
                self.inner.dev,
                spirv,
                entry,
                u64::from(bindings),
                u64::from(immediate_bytes)
            ),
            KernelNewError
        )?;
        Ok(Kernel {
            inner: Arc::new(KernelInner {
                ctx: self.inner.clone(),
                kern,
                name: entry.to_owned(),
            }),
        })
    }

    /// Starts recording a command buffer.
    pub fn record(&self) -> Result<Recording<'_>, GpuError> {
        let rec = vesper!(binding::rec_new(self.inner.dev), RecNewError)?;
        Ok(Recording {
            ctx: self,
            commands: Commands {
                ctx: self.inner.clone(),
                rec: Some(rec),
                buffers: Vec::new(),
                kernels: Vec::new(),
            },
            dispatches: 0,
            _borrows: PhantomData,
        })
    }
}

/// A host-visible, host-coherent storage buffer, mapped for its lifetime.
///
/// Every submission that binds the buffer keeps a reference to its storage
/// until the GPU has signalled that submission's timeline value, so the
/// memory stays allocated even if a [`Submission`] is leaked with
/// `mem::forget`. Host access copies in and out ([`Buffer::read`],
/// [`Buffer::write`]) and first waits until every submission that bound the
/// buffer has completed, so the host never reads or writes it while the
/// device may. No reference into the mapped memory is ever handed out.
pub struct Buffer {
    inner: Arc<BufferInner>,
}

struct BufferInner {
    ctx: Arc<DeviceInner>,
    buf: Buf,
    /// Highest timeline value of a submission that bound this buffer.
    busy_until: AtomicU64,
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("len", &self.inner.buf.len)
            .finish()
    }
}

impl Buffer {
    fn new(ctx: &Context, bytes: u64) -> Result<Self, GpuError> {
        let buf = vesper!(binding::buffer_new(ctx.inner.dev, bytes), BufferNewError)?;
        Ok(Self {
            inner: Arc::new(BufferInner {
                ctx: ctx.inner.clone(),
                buf,
                busy_until: AtomicU64::new(0),
            }),
        })
    }

    /// Size in bytes (a multiple of 4).
    pub fn len(&self) -> u64 {
        self.inner.buf.len
    }

    /// Always false: zero-sized buffers are rejected at creation.
    pub fn is_empty(&self) -> bool {
        self.inner.buf.len == 0
    }

    /// Runs `copy` once every submission that bound this buffer has
    /// completed. The queue lock is held throughout, so no submission can
    /// start between the check and the copy.
    fn with_idle(&self, copy: impl FnOnce(Buf) -> Result<(), GpuErr>) -> Result<(), GpuError> {
        let ctx = &self.inner.ctx;
        let _queue = ctx
            .queue
            .lock()
            .map_err(|_| GpuError::Shape("queue mutex poisoned"))?;
        let value = self.inner.busy_until.load(Ordering::Acquire);
        if value != 0 {
            binding::wait_value(ctx.dev, value).map_err(wait_error)?;
        }
        copy(self.inner.buf).map_err(|g| gpu_error(g, None))
    }

    /// Copies `data` to byte offset `offset`, after waiting for every
    /// submission that bound this buffer.
    pub fn write<T: bytemuck::Pod>(&mut self, offset: u64, data: &[T]) -> Result<(), GpuError> {
        let src: &[u8] = bytemuck::cast_slice(data);
        self.with_idle(|b| binding::buffer_write(b, offset, src))
    }

    /// Copies from byte offset `offset` into `out`, after waiting for every
    /// submission that bound this buffer.
    pub fn read<T: bytemuck::Pod>(&self, offset: u64, out: &mut [T]) -> Result<(), GpuError> {
        let dst: &mut [u8] = bytemuck::cast_slice_mut(out);
        self.with_idle(|b| binding::buffer_read(b, offset, dst))
    }

    /// The whole buffer as `u32` words.
    pub fn to_u32(&self) -> Result<Vec<u32>, GpuError> {
        let mut out = vec![0u32; (self.inner.buf.len / 4) as usize];
        self.read(0, &mut out)?;
        Ok(out)
    }

    fn same_device(&self, ctx: &Context) -> bool {
        Arc::ptr_eq(&self.inner.ctx, &ctx.inner)
    }
}

impl Drop for BufferInner {
    fn drop(&mut self) {
        // Every recording that bound this buffer holds an `Arc` to it until
        // its submission completed (or was never submitted), so the device
        // no longer uses it.
        binding::buffer_free(self.ctx.dev, self.buf);
    }
}

/// A compute pipeline with `bindings` storage buffers and an optional
/// immediate block. Submissions that use it keep its pipeline alive until
/// they complete.
pub struct Kernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    ctx: Arc<DeviceInner>,
    kern: Kern,
    name: String,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel")
            .field("name", &self.inner.name)
            .field("bindings", &self.inner.kern.bindings)
            .finish()
    }
}

impl Kernel {
    pub fn name(&self) -> &str {
        &self.inner.name
    }
}

impl Drop for KernelInner {
    fn drop(&mut self) {
        // Every recording that used this kernel holds an `Arc` to it until
        // its submission completed, so no pending work uses it.
        binding::kernel_free(self.ctx.dev, self.kern);
    }
}

/// Command-buffer resources owned by one recording, freed when its
/// submission completes.
struct Commands {
    ctx: Arc<DeviceInner>,
    rec: Option<Rec>,
    /// Everything the command buffer references, kept alive until the
    /// commands are freed (after completion, or never if leaked).
    buffers: Vec<Arc<BufferInner>>,
    kernels: Vec<Arc<KernelInner>>,
}

impl Drop for Commands {
    fn drop(&mut self) {
        // Only reached once the command buffer is not pending (never
        // submitted, or its timeline value was waited for).
        if let Some(rec) = self.rec.take()
            && let Err(e) = binding::rec_free(self.ctx.dev, &rec)
        {
            panic!("the Vesper GPU runtime failed internally: {e:?}");
        }
    }
}

/// A command buffer being recorded. It keeps a reference to every buffer and
/// kernel it binds (the `'a` borrows are a convenience, not what memory
/// safety rests on). A full compute-to-compute memory barrier follows each
/// dispatch, so dispatches observe their predecessors' writes in order.
#[must_use = "a recording does nothing until it is submitted"]
pub struct Recording<'a> {
    ctx: &'a Context,
    commands: Commands,
    dispatches: usize,
    _borrows: PhantomData<&'a Buffer>,
}

impl<'a> Recording<'a> {
    /// Dispatches recorded so far.
    pub fn dispatches(&self) -> usize {
        self.dispatches
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
        let k = &kernel.inner;
        if !Arc::ptr_eq(&k.ctx, &self.ctx.inner) || buffers.iter().any(|b| !b.same_device(self.ctx))
        {
            return Err(GpuError::ForeignObject);
        }
        let handles: Vec<u64> = buffers.iter().map(|b| b.inner.buf.buffer).collect();
        let rec = self
            .commands
            .rec
            .as_mut()
            .ok_or(GpuError::Shape("recording already submitted"))?;
        vesper!(
            binding::rec_dispatch(
                self.ctx.inner.dev,
                rec,
                k.kern,
                &handles,
                bytemuck::cast_slice(immediates),
                groups[0],
                groups[1],
                groups[2],
            ),
            RecDispatchError
        )?;
        self.commands.kernels.push(kernel.inner.clone());
        self.commands
            .buffers
            .extend(buffers.iter().map(|b| b.inner.clone()));
        self.dispatches += 1;
        Ok(())
    }

    /// Ends recording and submits to the queue. The submission waits for the
    /// previous timeline value (so submissions complete in order and see each
    /// other's writes) and signals a new one.
    pub fn submit(self) -> Result<Submission<'a>, GpuError> {
        let inner = &self.ctx.inner;
        let queue = inner
            .queue
            .lock()
            .map_err(|_| GpuError::Shape("queue mutex poisoned"))?;
        let previous = inner.issued.load(Ordering::Acquire);
        let rec = self
            .commands
            .rec
            .as_ref()
            .ok_or(GpuError::Shape("recording already submitted"))?;
        // On failure nothing was handed to the device; the recording's
        // resources are freed when `self` drops.
        let value = vesper!(
            binding::rec_submit(inner.dev, rec, previous),
            RecSubmitError
        )?;
        // Marked under the queue lock, before any host copy can observe the
        // buffer again (`Buffer::with_idle` takes the same lock).
        for b in &self.commands.buffers {
            b.busy_until.fetch_max(value, Ordering::AcqRel);
        }
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

/// Work in flight on the queue, identified by its timeline value. Its only
/// intended exit is [`Submission::wait`]. Dropping it without waiting is
/// counted by [`leaked_submissions`] (the drop still blocks until the GPU is
/// done); `mem::forget` leaks its command buffer and the buffer and kernel
/// references it holds, which is a leak, never a use after free.
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
        binding::wait_value(self.ctx.inner.dev, self.value).map_err(wait_error)
    }

    /// Blocks until the GPU signals this submission's timeline value, then
    /// frees its command buffer and descriptor sets and releases its references.
    pub fn wait(mut self) -> Result<Completed, GpuError> {
        let result = self.block();
        if result.is_err() {
            // Device lost or similar: wait for idle before anything is freed.
            binding::wait_idle(self.ctx.inner.dev);
        }
        self.commands = None;
        let value = self.value;
        std::mem::forget(self);
        result.map(|()| Completed { value })
    }
}

impl Drop for Submission<'_> {
    fn drop(&mut self) {
        LEAKED_SUBMISSIONS.fetch_add(1, Ordering::Relaxed);
        if self.block().is_err() {
            binding::wait_idle(self.ctx.inner.dev);
        }
        self.commands = None;
    }
}
