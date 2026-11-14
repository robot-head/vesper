use crate::schedule::ScheduleError;
use crate::shader::ShaderError;
use crate::translate::TranslateError;
use ash::vk;
use vesper_ignis::{PassId, QueueKind, ResourceRef};

/// Every failure of the Vulkan backend. Device loss is its own variant so
/// callers can tell it from ordinary API errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VulkanError {
    /// No Vulkan loader, no instance, or no device with the required
    /// Vulkan 1.3 features. The string says which.
    Unavailable(String),
    /// The validation layer was required but is not installed.
    ValidationLayerMissing,
    /// `VK_ERROR_DEVICE_LOST` from the named call (or injected by
    /// [`crate::Device::inject_device_lost`]). The device is unusable
    /// afterwards; pending work never retires.
    DeviceLost {
        call: &'static str,
    },
    /// Any other failing `VkResult`.
    Vk {
        call: &'static str,
        result: vk::Result,
    },
    Translate(TranslateError),
    Schedule(ScheduleError),
    Shader(ShaderError),
    /// No memory type with the needed properties.
    NoMemoryType,
    /// A creation descriptor is out of range or inconsistent.
    InvalidDesc(&'static str),
    /// A plan barrier or pass names a graph resource with no bound native
    /// object, or a binding has the wrong kind.
    UnboundResource(ResourceRef),
    /// The same graph resource was bound twice.
    DuplicateBinding(ResourceRef),
    /// A planned pass has no recorded work.
    MissingPassWork(PassId),
    /// The pass's work cannot run on its queue kind (for example a draw on a
    /// copy queue).
    WrongQueue {
        pass: PassId,
        kind: QueueKind,
    },
    /// A pipeline index is out of range or of the wrong type for the pass.
    BadPipeline {
        pass: PassId,
    },
    /// Push-descriptor bindings do not match the pipeline layout.
    BindingMismatch {
        pass: PassId,
    },
    /// A texture is not in the Vulkan layout its next use requires (as
    /// tracked from earlier barriers), or a barrier's `layout_before`
    /// disagrees with the tracked layout.
    LayoutMismatch {
        resource: ResourceRef,
        expected: vk::ImageLayout,
        actual: vk::ImageLayout,
    },
    /// A barrier subresource range lies outside the image.
    RangeOutOfBounds(ResourceRef),
    /// A copy or dispatch exceeds a buffer or device limit.
    OutOfBounds(&'static str),
    /// An object belongs to another `Device`.
    ForeignDevice,
    /// The allocator's queue family does not serve the stream's queue kind.
    AllocatorFamily,
    /// Destruction was requested while submitted work may still be running.
    Pending,
    /// A configured bound was exceeded.
    Limit(&'static str),
}

impl VulkanError {
    pub(crate) fn vk(call: &'static str, result: vk::Result) -> Self {
        if result == vk::Result::ERROR_DEVICE_LOST {
            VulkanError::DeviceLost { call }
        } else {
            VulkanError::Vk { call, result }
        }
    }

    /// True for [`VulkanError::DeviceLost`].
    pub fn is_device_lost(&self) -> bool {
        matches!(self, VulkanError::DeviceLost { .. })
    }
}

impl std::fmt::Display for VulkanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "Vulkan unavailable: {why}"),
            Self::ValidationLayerMissing => f.write_str("VK_LAYER_KHRONOS_validation is missing"),
            Self::DeviceLost { call } => write!(f, "device lost in {call}"),
            Self::Vk { call, result } => write!(f, "{call} failed: {result}"),
            Self::Translate(e) => e.fmt(f),
            Self::Schedule(e) => e.fmt(f),
            Self::Shader(e) => e.fmt(f),
            Self::NoMemoryType => f.write_str("no suitable memory type"),
            Self::InvalidDesc(what) => write!(f, "invalid descriptor: {what}"),
            Self::UnboundResource(r) => write!(f, "graph resource {r:?} has no native binding"),
            Self::DuplicateBinding(r) => write!(f, "graph resource {r:?} bound twice"),
            Self::MissingPassWork(p) => write!(f, "pass {p:?} has no recorded work"),
            Self::WrongQueue { pass, kind } => {
                write!(f, "pass {pass:?} cannot run on a {} queue", kind.name())
            }
            Self::BadPipeline { pass } => write!(f, "pass {pass:?} names an unusable pipeline"),
            Self::BindingMismatch { pass } => {
                write!(f, "pass {pass:?} bindings do not match its pipeline")
            }
            Self::LayoutMismatch {
                resource,
                expected,
                actual,
            } => write!(
                f,
                "{resource:?} is in layout {actual:?}, but {expected:?} is required"
            ),
            Self::RangeOutOfBounds(r) => write!(f, "subresource range outside {r:?}"),
            Self::OutOfBounds(what) => write!(f, "out of bounds: {what}"),
            Self::ForeignDevice => f.write_str("object belongs to another device"),
            Self::AllocatorFamily => f.write_str("allocator queue family does not match stream"),
            Self::Pending => f.write_str("submitted work may still be running"),
            Self::Limit(what) => write!(f, "limit exceeded: {what}"),
        }
    }
}
impl std::error::Error for VulkanError {}

impl From<TranslateError> for VulkanError {
    fn from(e: TranslateError) -> Self {
        VulkanError::Translate(e)
    }
}
impl From<ScheduleError> for VulkanError {
    fn from(e: ScheduleError) -> Self {
        VulkanError::Schedule(e)
    }
}
impl From<ShaderError> for VulkanError {
    fn from(e: ShaderError) -> Self {
        VulkanError::Shader(e)
    }
}
