//! Vesper Ignis Vulkan 1.3 backend (design §19.9), developed and tested on
//! Mesa lavapipe (llvmpipe), a CPU implementation of Vulkan.
//!
//! [`VulkanBackend`] implements [`vesper_ignis::lifecycle::Backend`]:
//!
//! * **Barriers.** Each planner [`vesper_ignis::Barrier`] batch (D3D12
//!   enhanced-barrier vocabulary) is translated by [`translate`] to one
//!   `vkCmdPipelineBarrier2` (synchronization2). Texture layouts are tracked
//!   per subresource across submissions; a barrier whose `layout_before`
//!   disagrees with the tracked layout, or a pass whose texture is not in the
//!   layout its work needs, is a typed [`VulkanError::LayoutMismatch`] before
//!   anything is submitted.
//! * **Fences.** Every runtime timeline is a timeline `VkSemaphore`. Planner
//!   waits and signals, and the runtime's terminal signals, are semaphore
//!   operations of `vkQueueSubmit2`; `poll` reads the counters and reports
//!   each original signal once, in submission order, as a
//!   [`vesper_ignis::lifecycle::Completion`]. [`schedule`] orders the queue
//!   batches so that a batch is never submitted before the signal it waits
//!   for (lavapipe has a single queue, so all Ignis queues share it).
//! * **Lifetimes.** Command pools, buffers, textures and pipelines are
//!   linear owners ([`CommandAllocator`], [`GpuBuffer`], [`GpuTexture`],
//!   [`Pipeline`]) moved into the runtime with the frame and returned only on
//!   retirement, after the original fences completed; each stream ends with a
//!   device-to-host memory barrier so retired buffers can be read. Dropping an
//!   owner instead of calling `destroy` leaks it and is counted by
//!   [`leaked_owners`]. Rust ownership is not evidence that the Vesper source
//!   typing rules (§19.9 `linear extern type`s) are implemented; it is host
//!   library discipline only.
//! * **Shaders.** WGSL is compiled to SPIR-V 1.3 in-process with `naga`
//!   ([`shader`]), so no external shader toolchain is needed.
//! * **Device loss.** `VK_ERROR_DEVICE_LOST` from any call becomes
//!   [`VulkanError::DeviceLost`], after which submissions are rejected and
//!   polls report the error; pending frames never retire.
//!
//! Not implemented: DirectX 12 and Metal (deferred; this crate is the
//! Vulkan path only), work-graph scratch (D3D12-only, rejected), split
//! barriers, ray tracing, transform feedback, VRS, depth/stencil
//! textures, swapchains/presentation, descriptor heaps
//! (`VK_EXT_descriptor_heap`), and queue-family ownership transfers
//! (resources use `CONCURRENT` sharing when several families are used).
//! Only lavapipe has been exercised; no hardware GPU was available.
#![deny(unsafe_code)]

pub mod error;
pub mod schedule;
pub mod shader;
pub mod translate;
mod vk;

pub use error::VulkanError;
pub use schedule::ScheduleError;
pub use shader::{BindingKind, CompiledShader, ShaderError, Stage, compile_wgsl};
pub use vk::*;

/// The WGSL shaders used by the examples and tests.
pub mod shaders {
    /// `vs_main`/`fs_main`: one orange triangle, no vertex buffers.
    pub const TRIANGLE: &str = include_str!("../shaders/triangle.wgsl");
    /// `main`: `dst[i] = pack4x8unorm(1 - src[i])` over a 2D texture, 8x8 groups.
    pub const INVERT: &str = include_str!("../shaders/invert.wgsl");
}

/// Re-export of the `ash` version this crate is built against.
pub use ash;
