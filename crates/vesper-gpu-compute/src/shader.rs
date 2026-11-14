//! WGSL to SPIR-V with `naga`: parse, validate, check the binding interface
//! the runtime expects, and emit SPIR-V 1.3 with every buffer and array
//! access bounds-checked (`Restrict`), so a kernel indexing past a buffer
//! reads a clamped element instead of touching other memory.
use naga::back::spv;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};

/// A kernel compiled by naga.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiled {
    pub spirv: Vec<u32>,
    /// Number of `var<storage>` bindings, all in group 0 at `0..n`.
    pub storage_bindings: u32,
    /// Size of the `var<immediate>` block, or 0.
    pub immediate_bytes: u32,
    /// `@workgroup_size` of the entry point.
    pub workgroup_size: [u32; 3],
}

/// Typed shader failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderError {
    /// WGSL that does not parse; the message is naga's.
    Parse(String),
    /// A module naga's validator rejects.
    Validation(String),
    /// The entry point is missing or is not a compute shader.
    EntryPoint(String),
    /// Bindings or immediates differ from what the runtime binds.
    Interface(String),
    /// The SPIR-V backend failed.
    Backend(String),
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "WGSL parse error: {m}"),
            Self::Validation(m) => write!(f, "WGSL validation error: {m}"),
            Self::EntryPoint(m) => write!(f, "entry point: {m}"),
            Self::Interface(m) => write!(f, "binding interface: {m}"),
            Self::Backend(m) => write!(f, "SPIR-V backend: {m}"),
        }
    }
}

impl std::error::Error for ShaderError {}

/// Upper bound on WGSL source size accepted (1 MiB).
pub const MAX_SOURCE_BYTES: usize = 1 << 20;

/// Compiles the compute entry point `entry` of `source`.
pub fn compile_wgsl(source: &str, entry: &str) -> Result<Compiled, ShaderError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ShaderError::Parse("source larger than 1 MiB".into()));
    }
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|e| ShaderError::Parse(e.emit_to_string(source)))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::IMMEDIATES)
        .validate(&module)
        .map_err(|e| ShaderError::Validation(e.emit_to_string(source)))?;
    let ep = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry)
        .ok_or_else(|| ShaderError::EntryPoint(format!("no entry point named {entry:?}")))?;
    if ep.stage != naga::ShaderStage::Compute {
        return Err(ShaderError::EntryPoint(format!(
            "{entry:?} is not a compute entry point"
        )));
    }
    let mut bindings = Vec::new();
    let mut immediate_bytes = 0u32;
    for (_, var) in module.global_variables.iter() {
        match var.space {
            naga::AddressSpace::Storage { .. } => {
                let b = var.binding.as_ref().ok_or_else(|| {
                    ShaderError::Interface("storage variable without @binding".into())
                })?;
                if b.group != 0 {
                    return Err(ShaderError::Interface(format!(
                        "storage binding in group {}, only group 0 is bound",
                        b.group
                    )));
                }
                bindings.push(b.binding);
            }
            naga::AddressSpace::Immediate => {
                immediate_bytes = module.types[var.ty].inner.size(module.to_ctx());
            }
            naga::AddressSpace::Uniform | naga::AddressSpace::Handle => {
                return Err(ShaderError::Interface(
                    "only storage buffers and immediates are supported".into(),
                ));
            }
            _ => {}
        }
    }
    bindings.sort_unstable();
    if bindings.iter().enumerate().any(|(i, b)| *b != i as u32) {
        return Err(ShaderError::Interface(format!(
            "storage bindings must be 0..n without gaps, found {bindings:?}"
        )));
    }
    let policies = BoundsCheckPolicies {
        index: BoundsCheckPolicy::Restrict,
        buffer: BoundsCheckPolicy::Restrict,
        image_load: BoundsCheckPolicy::Restrict,
        binding_array: BoundsCheckPolicy::Restrict,
    };
    let options = spv::Options {
        lang_version: (1, 3),
        flags: spv::WriterFlags::empty(),
        // With an empty binding map, naga emits the WGSL @group/@binding
        // numbers unchanged.
        fake_missing_bindings: true,
        bounds_check_policies: policies,
        ..spv::Options::default()
    };
    let pipeline = spv::PipelineOptions {
        shader_stage: naga::ShaderStage::Compute,
        entry_point: entry.to_owned(),
    };
    let spirv = spv::write_vec(&module, &info, &options, Some(&pipeline))
        .map_err(|e| ShaderError::Backend(e.to_string()))?;
    Ok(Compiled {
        spirv,
        storage_bindings: bindings.len() as u32,
        immediate_bytes,
        workgroup_size: ep.workgroup_size,
    })
}
