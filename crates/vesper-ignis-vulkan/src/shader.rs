//! WGSL to SPIR-V compilation with `naga`, so no external shader toolchain
//! (glslc, dxc, spirv-tools) is needed, plus the binding reflection the
//! backend uses to check pipeline layouts against the shader.

use naga::back::spv;
use naga::valid::{Capabilities, ValidationFlags, Validator};

/// Shader stage of a compiled entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

impl Stage {
    fn naga(self) -> naga::ShaderStage {
        match self {
            Stage::Vertex => naga::ShaderStage::Vertex,
            Stage::Fragment => naga::ShaderStage::Fragment,
            Stage::Compute => naga::ShaderStage::Compute,
        }
    }
}

/// A descriptor kind in set 0 (`@group(0)`), the only set this backend binds,
/// with push descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// `texture_2d<f32>` read with `textureLoad` (no sampler).
    SampledTexture,
    /// `texture_storage_2d<rgba8unorm, write>` or `read_write`.
    StorageTexture,
    /// `var<storage, read>` or `var<storage, read_write>`.
    StorageBuffer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderError {
    Parse(String),
    Validate(String),
    Emit(String),
    MissingEntryPoint {
        name: String,
        stage: Stage,
    },
    /// A resource uses a group other than 0.
    UnsupportedGroup {
        group: u32,
        binding: u32,
    },
    /// A resource kind this backend does not bind (uniforms, samplers, ...).
    UnsupportedResource {
        binding: u32,
    },
    /// The shader's bindings differ from the declared pipeline layout.
    LayoutMismatch {
        declared: Vec<BindingKind>,
        shader: Vec<Option<BindingKind>>,
    },
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "WGSL parse error: {e}"),
            Self::Validate(e) => write!(f, "WGSL validation error: {e}"),
            Self::Emit(e) => write!(f, "SPIR-V emission error: {e}"),
            Self::MissingEntryPoint { name, stage } => {
                write!(f, "no {stage:?} entry point named {name:?}")
            }
            Self::UnsupportedGroup { group, binding } => write!(
                f,
                "@group({group}) @binding({binding}): only group 0 is supported"
            ),
            Self::UnsupportedResource { binding } => {
                write!(f, "@binding({binding}) has an unsupported resource type")
            }
            Self::LayoutMismatch { declared, shader } => write!(
                f,
                "declared bindings {declared:?} do not match shader bindings {shader:?}"
            ),
        }
    }
}
impl std::error::Error for ShaderError {}

/// A validated SPIR-V module for one entry point.
#[derive(Clone, Debug)]
pub struct CompiledShader {
    pub stage: Stage,
    pub entry_point: String,
    pub spirv: Vec<u32>,
    /// `bindings[i]` is the kind at `@group(0) @binding(i)`, if any.
    pub bindings: Vec<Option<BindingKind>>,
}

/// Upper bound on WGSL source size accepted from callers.
pub const MAX_WGSL_BYTES: usize = 1 << 20;
/// Upper bound on the binding index this backend reflects.
pub const MAX_BINDINGS: u32 = 16;

/// Parses, validates and compiles one entry point of `source` to SPIR-V 1.3.
pub fn compile_wgsl(
    source: &str,
    stage: Stage,
    entry_point: &str,
) -> Result<CompiledShader, ShaderError> {
    if source.len() > MAX_WGSL_BYTES {
        return Err(ShaderError::Parse(format!(
            "source exceeds {MAX_WGSL_BYTES} bytes"
        )));
    }
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|e| ShaderError::Parse(e.emit_to_string(source)))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|e| ShaderError::Validate(e.emit_to_string(source)))?;
    if !module
        .entry_points
        .iter()
        .any(|ep| ep.name == entry_point && ep.stage == stage.naga())
    {
        return Err(ShaderError::MissingEntryPoint {
            name: entry_point.to_owned(),
            stage,
        });
    }
    let bindings = reflect(&module)?;
    let options = spv::Options {
        lang_version: (1, 3),
        ..spv::Options::default()
    };
    let pipeline = spv::PipelineOptions {
        shader_stage: stage.naga(),
        entry_point: entry_point.to_owned(),
    };
    let spirv = spv::write_vec(&module, &info, &options, Some(&pipeline))
        .map_err(|e| ShaderError::Emit(e.to_string()))?;
    Ok(CompiledShader {
        stage,
        entry_point: entry_point.to_owned(),
        spirv,
        bindings,
    })
}

fn reflect(module: &naga::Module) -> Result<Vec<Option<BindingKind>>, ShaderError> {
    let mut out: Vec<Option<BindingKind>> = Vec::new();
    for (_, var) in module.global_variables.iter() {
        let Some(binding) = &var.binding else {
            continue;
        };
        if binding.group != 0 {
            return Err(ShaderError::UnsupportedGroup {
                group: binding.group,
                binding: binding.binding,
            });
        }
        if binding.binding >= MAX_BINDINGS {
            return Err(ShaderError::UnsupportedResource {
                binding: binding.binding,
            });
        }
        let kind = match (&var.space, &module.types[var.ty].inner) {
            (naga::AddressSpace::Storage { .. }, _) => BindingKind::StorageBuffer,
            (
                naga::AddressSpace::Handle,
                naga::TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed: false,
                    // RGBA8_UNORM views need a float sampled type.
                    class:
                        naga::ImageClass::Sampled {
                            kind: naga::ScalarKind::Float,
                            multi: false,
                        },
                },
            ) => BindingKind::SampledTexture,
            (
                naga::AddressSpace::Handle,
                naga::TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed: false,
                    class:
                        naga::ImageClass::Storage {
                            format: naga::StorageFormat::Rgba8Unorm,
                            ..
                        },
                },
            ) => BindingKind::StorageTexture,
            _ => {
                return Err(ShaderError::UnsupportedResource {
                    binding: binding.binding,
                });
            }
        };
        let index = binding.binding as usize;
        if out.len() <= index {
            out.resize(index + 1, None);
        }
        out[index] = Some(kind);
    }
    Ok(out)
}

/// Checks that a shader's bindings are exactly `declared` (binding `i` has
/// kind `declared[i]`; the shader may leave a declared binding unused).
pub fn check_layout(shader: &CompiledShader, declared: &[BindingKind]) -> Result<(), ShaderError> {
    let ok = shader.bindings.len() <= declared.len()
        && shader
            .bindings
            .iter()
            .zip(declared)
            .all(|(have, want)| have.is_none_or(|have| have == *want));
    if ok {
        Ok(())
    } else {
        Err(ShaderError::LayoutMismatch {
            declared: declared.to_vec(),
            shader: shader.bindings.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTE: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(src);
    if (id.x < size.x && id.y < size.y) {
        dst[id.y * size.x + id.x] = pack4x8unorm(textureLoad(src, vec2<i32>(id.xy), 0));
    }
}
"#;

    #[test]
    fn compiles_and_reflects() {
        let shader = compile_wgsl(COMPUTE, Stage::Compute, "main").unwrap();
        assert_eq!(shader.spirv[0], 0x0723_0203, "SPIR-V magic");
        assert_eq!(
            shader.bindings,
            vec![
                Some(BindingKind::SampledTexture),
                Some(BindingKind::StorageBuffer)
            ]
        );
        check_layout(
            &shader,
            &[BindingKind::SampledTexture, BindingKind::StorageBuffer],
        )
        .unwrap();
        assert!(matches!(
            check_layout(
                &shader,
                &[BindingKind::StorageBuffer, BindingKind::StorageBuffer]
            ),
            Err(ShaderError::LayoutMismatch { .. })
        ));
    }

    #[test]
    fn typed_errors() {
        assert!(matches!(
            compile_wgsl("fn (", Stage::Compute, "main"),
            Err(ShaderError::Parse(_))
        ));
        assert!(matches!(
            compile_wgsl(COMPUTE, Stage::Vertex, "main"),
            Err(ShaderError::MissingEntryPoint { .. })
        ));
        let invalid = "@compute @workgroup_size(1) fn main() { let x: u32 = 1.5; }";
        assert!(compile_wgsl(invalid, Stage::Compute, "main").is_err());
        let group1 = "@group(1) @binding(0) var<storage, read_write> d: array<u32>;\n\
                      @compute @workgroup_size(1) fn main() { d[0] = 1u; }";
        let int_texture = "@group(0) @binding(0) var t: texture_2d<u32>;\n\
                      @compute @workgroup_size(1) fn main() { _ = textureLoad(t, vec2<i32>(0), 0); }";
        assert_eq!(
            compile_wgsl(int_texture, Stage::Compute, "main").unwrap_err(),
            ShaderError::UnsupportedResource { binding: 0 }
        );
        assert_eq!(
            compile_wgsl(group1, Stage::Compute, "main").unwrap_err(),
            ShaderError::UnsupportedGroup {
                group: 1,
                binding: 0
            }
        );
    }
}
