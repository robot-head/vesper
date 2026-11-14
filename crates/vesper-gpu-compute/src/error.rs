//! The crate's typed errors.
use crate::shader::ShaderError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuError {
    /// No Vulkan loader, instance or suitable device on this host. Tests
    /// report this rather than claiming a result.
    Unavailable(String),
    /// A Vulkan call failed with `VkResult` `code`.
    Vulkan {
        call: &'static str,
        code: i32,
    },
    Shader(ShaderError),
    /// Operand lengths or dispatch arguments that do not fit.
    Shape(&'static str),
    /// A size above a documented ceiling.
    Limit(&'static str),
    /// A valid request this runtime does not implement.
    Unsupported(&'static str),
    /// A buffer, kernel or session from another context or model.
    ForeignObject,
    /// A non-finite value where the operation requires finite input or output.
    NonFinite,
    /// The CPU reference library rejected an input.
    Tensor(vesper_tensor::Error),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "GPU unavailable: {m}"),
            Self::Vulkan { call, code } => write!(f, "{call} failed with VkResult {code}"),
            Self::Shader(e) => write!(f, "{e}"),
            Self::Shape(m) => write!(f, "shape: {m}"),
            Self::Limit(m) => write!(f, "limit exceeded: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported: {m}"),
            Self::ForeignObject => write!(f, "object belongs to another context"),
            Self::NonFinite => write!(f, "non-finite value"),
            Self::Tensor(e) => write!(f, "tensor: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

impl From<ShaderError> for GpuError {
    fn from(e: ShaderError) -> Self {
        Self::Shader(e)
    }
}

impl From<vesper_tensor::Error> for GpuError {
    fn from(e: vesper_tensor::Error) -> Self {
        Self::Tensor(e)
    }
}
