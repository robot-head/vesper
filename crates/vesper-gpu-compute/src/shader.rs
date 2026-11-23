//! Kernels are SPIR-V 1.3 modules. The crate's own kernels are WGSL
//! (`src/shaders/*.wgsl`) compiled ahead of time with naga into the committed
//! `src/shaders/spirv/*.spv`, which `tests/spirv.rs` regenerates and checks;
//! nothing compiles WGSL at run time. Before a pipeline is created, the
//! Vesper library (`spirv_interface` in `vesper/src/lib.ves`) reads the
//! module's binding interface and refuses one that differs from what the
//! runtime binds.
use crate::runtime::binding;

/// Typed shader failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderError {
    /// Bytes that are not a SPIR-V module.
    Malformed(String),
    /// The module has no compute entry point of that name.
    EntryPoint(String),
    /// Bindings or immediates differ from what the runtime binds.
    Interface(String),
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "SPIR-V: {m}"),
            Self::EntryPoint(m) => write!(f, "entry point: {m}"),
            Self::Interface(m) => write!(f, "binding interface: {m}"),
        }
    }
}

impl std::error::Error for ShaderError {}

/// A module's binding interface for one compute entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface {
    /// Number of storage buffers, all in set 0 at bindings `0..n`.
    pub storage_bindings: u32,
    /// Size of the immediate (push-constant) block, or 0.
    pub immediate_bytes: u32,
}

/// The binding interface of `entry` in the SPIR-V module `spirv`
/// (little-endian words): every storage buffer in set 0 at bindings `0..n`
/// without gaps, at most one immediate block of scalars, vectors and structs
/// of them, and no uniform buffers, images or samplers.
pub fn interface(spirv: &[u8], entry: &str) -> Result<Interface, ShaderError> {
    match binding::spirv_interface(spirv, entry) {
        Ok(i) => Ok(Interface {
            storage_bindings: i.bindings as u32,
            immediate_bytes: i.immediate_bytes as u32,
        }),
        Err(binding::SpirvInterfaceError::GpuErr(g)) => Err(shader_error(g.detail, g.code, entry)),
        Err(e) => panic!("the Vesper SPIR-V reader failed internally: {e:?}"),
    }
}

/// The crate's error for a Vesper `Kind::Shader` failure with `detail`.
pub(crate) fn shader_error(detail: u32, code: i64, entry: &str) -> ShaderError {
    match detail {
        1 => ShaderError::Malformed("not a SPIR-V module".into()),
        2 => ShaderError::EntryPoint(format!("no compute entry point named {entry:?}")),
        3 => ShaderError::Interface(format!(
            "kernel {entry} binds {code} storage buffers, a different number than requested"
        )),
        4 => ShaderError::Interface(format!(
            "kernel {entry} declares {code} immediate bytes, more than requested"
        )),
        _ => ShaderError::Interface(
            "storage buffers must be in set 0 at bindings 0..n without gaps, with at most one \
             immediate block of scalars, vectors and structs of them and no uniform buffers, \
             images or samplers"
                .into(),
        ),
    }
}
