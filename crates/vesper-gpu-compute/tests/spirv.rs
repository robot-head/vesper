//! The committed SPIR-V (`src/shaders/spirv/*.spv`) is naga's compilation of
//! the committed WGSL, and the Vesper interface reader agrees with naga on
//! every kernel's bindings and immediates. Runs everywhere: no device
//! needed. `VESPER_SPIRV_BLESS=1` rewrites the `.spv` files from the WGSL.
#[path = "support/wgsl.rs"]
mod wgsl;

use vesper_gpu_compute::shader::{self, Interface, ShaderError};
use vesper_gpu_compute::{broadphase, tensor};

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Every bundled kernel: entry point, WGSL source and committed SPIR-V.
fn kernels() -> Vec<(&'static str, &'static str, &'static [u8])> {
    let sources = tensor::SOURCES.iter().chain(&broadphase::SOURCES);
    let spirv = tensor::SPIRV.iter().chain(&broadphase::SPIRV);
    sources
        .zip(spirv)
        .map(|((entry, source), (name, words))| {
            assert_eq!(entry, name);
            (*entry, *source, *words)
        })
        .collect()
}

#[test]
fn committed_spirv_is_the_compiled_wgsl() {
    let bless = std::env::var("VESPER_SPIRV_BLESS").is_ok_and(|v| v == "1");
    let mut stale = Vec::new();
    for (entry, source, committed) in kernels() {
        let compiled = wgsl::compile_wgsl(source, entry).unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(compiled.spirv[0], SPIRV_MAGIC, "{entry}");
        let bytes = wgsl::bytes(&compiled.spirv);
        if bytes != committed {
            if bless {
                let path = format!(
                    "{}/src/shaders/spirv/{entry}.spv",
                    env!("CARGO_MANIFEST_DIR")
                );
                std::fs::write(&path, &bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
            }
            stale.push(entry);
        }
    }
    assert!(
        bless || stale.is_empty(),
        "stale SPIR-V for {stale:?}: rerun with VESPER_SPIRV_BLESS=1 and review the diff"
    );
}

#[test]
fn the_vesper_reader_agrees_with_naga_on_every_interface() {
    for (entry, source, committed) in kernels() {
        let compiled = wgsl::compile_wgsl(source, entry).unwrap();
        assert_eq!(
            shader::interface(committed, entry),
            Ok(Interface {
                storage_bindings: compiled.storage_bindings,
                immediate_bytes: compiled.immediate_bytes,
            }),
            "{entry}"
        );
        assert!(compiled.storage_bindings >= 1, "{entry}");
        assert!(compiled.immediate_bytes <= 16, "{entry}");
        assert!(compiled.workgroup_size[0] >= 64, "{entry}");
    }
}

/// No `ArrayStride` may decorate an array type that a Workgroup pointer
/// reaches (VUID-StandaloneSpirv-None-10684, reported by recent validation
/// layers); storage-buffer arrays keep theirs.
#[test]
fn workgroup_arrays_carry_no_explicit_layout() {
    for (entry, _, committed) in kernels() {
        let words: Vec<u32> = committed
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        let mut at = 5;
        let (mut arrays, mut strided, mut workgroup) = (Vec::new(), Vec::new(), Vec::new());
        while at < words.len() {
            let count = (words[at] >> 16) as usize;
            let ops = &words[at + 1..at + count];
            match words[at] & 0xffff {
                28 => arrays.push(ops[0]),
                32 if ops[1] == 4 => workgroup.push(ops[2]),
                71 if ops[1] == 6 => strided.push(ops[0]),
                _ => {}
            }
            at += count;
        }
        for t in &workgroup {
            assert!(
                !strided.contains(t),
                "{entry}: workgroup array {t} has ArrayStride"
            );
        }
        assert!(
            !strided.is_empty(),
            "{entry}: buffer arrays keep ArrayStride"
        );
        if ["rms_norm", "attention", "scan_main"].contains(&entry) {
            assert!(workgroup.iter().any(|t| arrays.contains(t)), "{entry}");
        }
    }
}

const ADD: &str = "
struct P { n: u32, add: u32 }
var<immediate> p: P;
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@compute @workgroup_size(64)
fn add(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < p.n { dst[id.x] = src[id.x] + p.add; }
}
";

#[test]
fn the_reader_refuses_what_the_runtime_does_not_bind() {
    let add = wgsl::spirv(ADD, "add");
    assert_eq!(
        shader::interface(&add, "add"),
        Ok(Interface {
            storage_bindings: 2,
            immediate_bytes: 8
        })
    );
    assert!(matches!(
        shader::interface(&add, "missing"),
        Err(ShaderError::EntryPoint(_))
    ));
    // Not SPIR-V: empty, not whole words, a wrong magic, a truncated
    // instruction, an id above the bound.
    assert!(matches!(
        shader::interface(&[], "add"),
        Err(ShaderError::Malformed(_))
    ));
    assert!(matches!(
        shader::interface(&add[..add.len() - 1], "add"),
        Err(ShaderError::Malformed(_))
    ));
    let mut magic = add.clone();
    magic[0] ^= 1;
    assert!(matches!(
        shader::interface(&magic, "add"),
        Err(ShaderError::Malformed(_))
    ));
    let mut truncated = add[..24].to_vec();
    truncated[22] = 0xff;
    assert!(matches!(
        shader::interface(&truncated, "add"),
        Err(ShaderError::Malformed(_))
    ));
    let mut small_bound = add.clone();
    small_bound[12..16].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        shader::interface(&small_bound, "add"),
        Err(ShaderError::Malformed(_))
    ));
    let uniform = "@group(0) @binding(0) var<uniform> u: vec4<f32>;
        @group(0) @binding(1) var<storage, read_write> b: array<f32>;
        @compute @workgroup_size(1) fn m() { b[0] = u.x; }";
    let gap = "@group(0) @binding(1) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1u; }";
    let group1 = "@group(1) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1u; }";
    for source in [uniform, gap, group1] {
        assert!(
            matches!(
                shader::interface(&wgsl::spirv_unchecked(source, "m"), "m"),
                Err(ShaderError::Interface(_))
            ),
            "{source}"
        );
    }
    // A fragment entry point is not a compute entry point.
    let fragment = "@fragment fn f() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }";
    let module = naga::front::wgsl::parse_str(fragment).unwrap();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap();
    let words =
        naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
            .unwrap();
    assert!(matches!(
        shader::interface(&wgsl::bytes(&words), "f"),
        Err(ShaderError::EntryPoint(_))
    ));
}

#[test]
fn the_test_compiler_refuses_bad_wgsl() {
    assert!(matches!(
        wgsl::compile_wgsl("fn (", "x"),
        Err(wgsl::ShaderError::Parse(_))
    ));
    let ill_typed = "@group(0) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1.5; }";
    assert!(matches!(
        wgsl::compile_wgsl(ill_typed, "m"),
        Err(wgsl::ShaderError::Parse(_) | wgsl::ShaderError::Validation(_))
    ));
}
