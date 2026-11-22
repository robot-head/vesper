//! The Vulkan runtime: timeline-ordered submissions, the submission
//! obligation, bounds clamping and typed argument errors. The kernels here
//! are compiled from WGSL by the test support compiler (`tests/spirv.rs`
//! covers the interface reader without a device).
#[path = "support/wgsl.rs"]
mod wgsl;

use vesper_gpu_compute::shader::ShaderError;
use vesper_gpu_compute::{Context, GpuError, leaked_submissions, test_context};

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
fn chained_submissions_complete_in_timeline_order() {
    let Some(ctx) = test_context() else { return };
    let kernel = ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 2, 8).unwrap();
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
    assert_eq!(
        c.to_u32().unwrap(),
        (0..n).map(|i| i + 110).collect::<Vec<u32>>()
    );
}

#[test]
fn a_dropped_submission_is_counted_and_still_completes() {
    let Some(ctx) = test_context() else { return };
    let kernel = ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 2, 8).unwrap();
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
    assert_eq!(b.to_u32().unwrap(), vec![2, 3, 4, 5]);
}

/// `mem::forget` on a submission ends its borrows without running its
/// destructor. The submission's command buffer still owns references to every
/// buffer and kernel it bound, so dropping the Rust handles frees nothing
/// the device may use, and host reads wait for the device.
#[test]
fn a_forgotten_submission_keeps_its_resources_alive() {
    let Some(ctx) = test_context() else { return };
    let kernel = ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 2, 8).unwrap();
    let n = 1 << 16;
    let a = ctx.buffer_from(&vec![5u32; n as usize]).unwrap();
    let b = ctx.buffer((n * 4) as u64).unwrap();
    let mut rec = ctx.record().unwrap();
    for _ in 0..64 {
        rec.dispatch(&kernel, &[&a, &b], &[n, 1], [n / 64, 1, 1])
            .unwrap();
    }
    std::mem::forget(rec.submit().unwrap());
    // Reading waits for the device instead of racing its writes.
    assert!(b.to_u32().unwrap().iter().all(|v| *v == 6));
    drop(a);
    drop(kernel);
    drop(b);
    // The context still works.
    let c = ctx.buffer_from(&[1u32]).unwrap();
    assert_eq!(c.to_u32().unwrap(), vec![1]);
}

#[test]
fn out_of_bounds_kernel_accesses_are_clamped() {
    let Some(ctx) = test_context() else { return };
    let wild = "@group(0) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[1000000u] = 7u; }";
    let kernel = ctx.kernel(&wgsl::spirv(wild, "m"), "m", 1, 0).unwrap();
    let b = ctx.buffer(16).unwrap();
    let mut rec = ctx.record().unwrap();
    rec.dispatch(&kernel, &[&b], &[], [1, 1, 1]).unwrap();
    rec.submit().unwrap().wait().unwrap();
    // naga's Restrict policy clamps the index to the last element.
    assert_eq!(b.to_u32().unwrap(), vec![0, 0, 0, 7]);
}

/// A module with no bounds checks of its own, as a caller may supply: the
/// device's `robustBufferAccess` discards the out-of-bounds write, so a
/// buffer next to the target keeps its contents.
#[test]
fn unchecked_out_of_bounds_writes_are_discarded() {
    let Some(ctx) = test_context() else { return };
    let wild = "@group(0) @binding(0) var<storage, read_write> b: array<u32>;
        @compute @workgroup_size(1) fn m() { b[1000000u] = 7u; b[4u] = 9u; }";
    let kernel = ctx
        .kernel(&wgsl::spirv_unbounded(wild, "m"), "m", 1, 0)
        .unwrap();
    let b = ctx.buffer(16).unwrap();
    let next = ctx.buffer_from(&[1u32, 2, 3, 4]).unwrap();
    let mut rec = ctx.record().unwrap();
    rec.dispatch(&kernel, &[&b], &[], [1, 1, 1]).unwrap();
    rec.submit().unwrap().wait().unwrap();
    assert_eq!(b.to_u32().unwrap(), vec![0, 0, 0, 0]);
    assert_eq!(next.to_u32().unwrap(), vec![1, 2, 3, 4]);
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
        ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 3, 8),
        Err(GpuError::Shader(ShaderError::Interface(_)))
    ));
    assert!(matches!(
        ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 2, 4),
        Err(GpuError::Shader(ShaderError::Interface(_)))
    ));
    assert!(matches!(
        ctx.kernel(&[0; 8], "add", 2, 8),
        Err(GpuError::Shader(ShaderError::Malformed(_)))
    ));
    assert!(matches!(
        ctx.kernel(&wgsl::spirv(ADD, "add"), "missing", 2, 8),
        Err(GpuError::Shader(ShaderError::EntryPoint(_)))
    ));
    let kernel = ctx.kernel(&wgsl::spirv(ADD, "add"), "add", 2, 8).unwrap();
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
    b.write(0, &[1u32, 2]).unwrap();
}
