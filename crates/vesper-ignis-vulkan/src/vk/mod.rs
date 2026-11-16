//! The only module that calls Vulkan (through `ash`), and the only one where
//! `unsafe` is allowed.
#![allow(unsafe_code)]

mod backend;
mod device;
#[cfg(target_os = "linux")]
mod present;
#[cfg(unix)]
mod syncfd;

pub use backend::{
    Bind, ColorTarget, DepthTarget, FrameParts, FrameResources, MAX_FRAME_OBJECTS,
    MAX_PENDING_SIGNALS, NoScratch, PassWork, PipelineId, SubmitStats, VulkanBackend,
};
pub use device::{
    CommandAllocator, Device, DeviceConfig, DeviceInfo, GpuBuffer, GpuTexture, GraphicsDesc,
    Pipeline, Sharing, StencilDesc, TextureInfo, Validation, ValidationMessage, leaked_owners,
};
#[cfg(target_os = "linux")]
pub use present::{Swapchain, SwapchainDesc, XcbWindow};
#[cfg(unix)]
pub use syncfd::{CloseError, SyncFd, SyncFdExporter, SyncFdKind, SyncFdMode};
