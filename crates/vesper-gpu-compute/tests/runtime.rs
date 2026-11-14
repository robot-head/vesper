//! The Vulkan runtime: shader compilation (runs everywhere, no device
//! needed), timeline-ordered submissions, the submission obligation, bounds
//! clamping and typed argument errors.
use vesper_gpu_compute::shader::{ShaderError, compile_wgsl};
use vesper_gpu_compute::{Context, GpuError, broadphase, leaked_submissions, tensor, test_context};

const SPIRV_MAGIC: u32 = 0x0723_0203;

#[test]
fn every_bundled_kernel_compiles_to_spirv() {
    for (entry, source) in tensor::SOURCES.iter().chain(&broadphase::SOURCES) {
        let compiled = compile_wgsl(source, entry).unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(compiled.spirv[0], SPIRV_MAGIC, "{entry}");
        assert!(compiled.storage_bindings >= 1, "{entry}");
        assert!(compiled.immediate_bytes <= 16, "{entry}");
        assert!(compiled.workgroup_size[0] >= 64, "{entry}");
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
fn malformed_shaders_are_typed_errors() {
    assert!(matches!(
        compile_wgsl("fn (", "x"),
        Err(ShaderError::Parse(_))
    ));
    assert!(matches!(
        compile_wgsl(ADD, "missing"),
        Err(ShaderError::EntryPoint(_))
    ));
    let not_compute = "@fragment fn f() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }";
    assert!(matches!(
        compile_wgsl(not_compute, "f"),
        Err(ShaderError::EntryPoint(_))
    ));
    let uniform = "@group(0) @binding(0) var<uniform> u: vec4<f32>;
        @compute @workgroup_size(1) fn m() { _ = u; }";
    assert!(matches!(
        compile_wgsl(uniform, "m"),
        Err(ShaderError::Interface(_))
    ));
    let gap = "@group(0) @binding(1) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1u; }";
    assert!(matches!(
        compile_wgsl(gap, "m"),
        Err(ShaderError::Interface(_))
    ));
    let group1 = "@group(1) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1u; }";
    assert!(matches!(
        compile_wgsl(group1, "m"),
        Err(ShaderError::Interface(_))
    ));
    let ill_typed = "@group(0) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[0] = 1.5; }";
    assert!(matches!(
        compile_wgsl(ill_typed, "m"),
        Err(ShaderError::Parse(_) | ShaderError::Validation(_))
    ));
    let huge = "a".repeat((1 << 20) + 1);
    assert!(matches!(
        compile_wgsl(&huge, "m"),
        Err(ShaderError::Parse(_))
    ));
}

#[test]
fn chained_submissions_complete_in_timeline_order() {
    let Some(ctx) = test_context() else { return };
    let kernel = ctx.kernel(ADD, "add", 2, 8).unwrap();
    let n = 1000u32;
    let a = ctx.buffer_from(&(0..n).collect::<Vec<u32>>()).unwrap();
    let b = ctx.buffer((n * 4) as u64).unwrap();
    let c = ctx.buffer((n * 4) as u64).unwrap();
    let start = ctx.issued_value();
    let mut r1 = ctx.record().unwrap();
    r1.dispatch(&kernel, &[&a, &b], &[n, 10], [16, 1, 1])
        .unwrap();
    let s1 = r1.submit().unwrap();
    // Recorded and submitted before s1 is waited for: the timeline wait
    // makes it observe s1's writes.
    let mut r2 = ctx.record().unwrap();
    r2.dispatch(&kernel, &[&b, &c], &[n, 100], [16, 1, 1])
        .unwrap();
    let s2 = r2.submit().unwrap();
    assert_eq!(s1.value(), start + 1);
    assert_eq!(s2.value(), start + 2);
    let done2 = s2.wait().unwrap();
    assert_eq!(done2.value, start + 2);
    assert!(s1.is_complete().unwrap());
    s1.wait().unwrap();
    assert!(ctx.completed_value().unwrap() >= start + 2);
    assert_eq!(c.to_u32(), (0..n).map(|i| i + 110).collect::<Vec<u32>>());
}

#[test]
fn a_dropped_submission_is_counted_and_still_completes() {
    let Some(ctx) = test_context() else { return };
    let kernel = ctx.kernel(ADD, "add", 2, 8).unwrap();
    let a = ctx.buffer_from(&[1u32, 2, 3, 4]).unwrap();
    let b = ctx.buffer(16).unwrap();
    let before = leaked_submissions();
    {
        let mut rec = ctx.record().unwrap();
        rec.dispatch(&kernel, &[&a, &b], &[4, 1], [1, 1, 1])
            .unwrap();
        #[allow(unused_must_use)]
        {
            rec.submit().unwrap();
        }
    }
    assert_eq!(leaked_submissions(), before + 1);
    assert_eq!(b.to_u32(), vec![2, 3, 4, 5]);
}

#[test]
fn out_of_bounds_kernel_accesses_are_clamped() {
    let Some(ctx) = test_context() else { return };
    let wild = "@group(0) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[1000000u] = 7u; }";
    let kernel = ctx.kernel(wild, "m", 1, 0).unwrap();
    let b = ctx.buffer(16).unwrap();
    let mut rec = ctx.record().unwrap();
    rec.dispatch(&kernel, &[&b], &[], [1, 1, 1]).unwrap();
    rec.submit().unwrap().wait().unwrap();
    // naga's Restrict policy clamps the index to the last element.
    assert_eq!(b.to_u32(), vec![0, 0, 0, 7]);
}

#[test]
fn runtime_arguments_are_validated() {
    let Some(ctx) = test_context() else { return };
    let info = ctx.info().clone();
    eprintln!("device: {info:?}");
    assert!(info.api_version >= (1, 2, 0));
    assert_eq!(
        ctx.buffer(0).unwrap_err(),
        GpuError::Shape("zero-sized buffer")
    );
    assert_eq!(
        ctx.buffer(1 << 40).unwrap_err(),
        GpuError::Limit("buffer size")
    );
    let mut b = ctx.buffer(6).unwrap();
    assert_eq!(b.len(), 8);
    assert!(b.write(4, &[1u32, 2]).is_err());
    let mut two = [0u32; 3];
    assert!(b.read(0, &mut two).is_err());
    assert!(matches!(
        ctx.kernel(ADD, "add", 3, 8),
        Err(GpuError::Shader(ShaderError::Interface(_)))
    ));
    assert!(matches!(
        ctx.kernel(ADD, "add", 2, 4),
        Err(GpuError::Shader(ShaderError::Interface(_)))
    ));
    let kernel = ctx.kernel(ADD, "add", 2, 8).unwrap();
    let other = Context::new().unwrap();
    let foreign = other.buffer(16).unwrap();
    let a = ctx.buffer(16).unwrap();
    let mut rec = ctx.record().unwrap();
    assert_eq!(
        rec.dispatch(&kernel, &[&a, &foreign], &[4, 1], [1, 1, 1]),
        Err(GpuError::ForeignObject)
    );
    assert!(rec.dispatch(&kernel, &[&a], &[4, 1], [1, 1, 1]).is_err());
    assert!(rec.dispatch(&kernel, &[&a, &b], &[4], [1, 1, 1]).is_err());
    assert!(
        rec.dispatch(&kernel, &[&a, &b], &[4, 1], [0, 1, 1])
            .is_err()
    );
    assert!(
        rec.dispatch(&kernel, &[&a, &b], &[4, 1], [70_000, 1, 1])
            .is_err()
    );
    assert_eq!(rec.dispatches(), 0);
    // An unsubmitted recording is simply dropped.
    drop(rec);
    let _ = b.bytes_mut();
}
