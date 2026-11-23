//! Vesper GPU compute (design §28, §36) on Vulkan.
//!
//! * [`runtime`]: a small Vulkan compute runtime (buffers, descriptor sets,
//!   dispatches with immediates, timeline-semaphore submissions).
//! * [`shader`]: SPIR-V kernels (WGSL compiled ahead of time by naga) and
//!   their binding interface.
//! * [`tensor`]: matmul, RMSNorm, RoPE and paged attention kernels and a
//!   GPU decoder for `vesper-tensor`'s llama2.c models.
//! * [`broadphase`]: the Kinetic sort-and-sweep broadphase as compute
//!   kernels, exact against `vesper-kinetic`'s CPU pair set.
//!
//! The development and CI device is Mesa lavapipe, a CPU implementation of
//! Vulkan; results validate the kernels and the runtime protocol, not GPU
//! performance or a hardware driver. Every Vulkan call is made by the
//! Vesper library in `vesper/`; the only unsafe code is its generated
//! binding (`port/binding.rs`), used by [`runtime`].
#![deny(unsafe_code)]

pub mod broadphase;
pub mod error;
pub mod runtime;
pub mod shader;
pub mod tensor;

pub use error::GpuError;
pub use runtime::{
    Buffer, Completed, Context, DeviceInfo, DeviceType, Kernel, Recording, Submission,
    leaked_submissions,
};

/// Opens the default device, or `None` with the reason printed to stderr
/// when no Vulkan device is present. With `VESPER_GPU_REQUIRE=1` (set by the
/// GPU compute CI workflow) an unavailable device panics instead, so the
/// differential tests cannot pass vacuously there.
pub fn test_context() -> Option<Context> {
    match Context::new() {
        Ok(ctx) => Some(ctx),
        Err(GpuError::Unavailable(reason)) => {
            if std::env::var("VESPER_GPU_REQUIRE").is_ok_and(|v| v == "1") {
                panic!("VESPER_GPU_REQUIRE=1 but the GPU is unavailable: {reason}");
            }
            eprintln!("GPU unavailable, test not run: {reason}");
            None
        }
        Err(e) => panic!("opening the Vulkan device failed: {e}"),
    }
}
