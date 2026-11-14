//! The only module that calls Vulkan (through `ash`), and the only one where
//! `unsafe` is allowed.
#![allow(unsafe_code)]

mod backend;
mod device;

pub use backend::{
    Bind, FrameParts, FrameResources, MAX_FRAME_OBJECTS, MAX_PENDING_SIGNALS, NoScratch, PassWork,
    PipelineId, VulkanBackend,
};
pub use device::{
    CommandAllocator, Device, DeviceConfig, DeviceInfo, GpuBuffer, GpuTexture, Pipeline,
    TextureInfo, Validation, ValidationMessage, leaked_owners,
};
