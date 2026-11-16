//! The GPU broadphase for [`vesper_kinetic::World`]: the Vulkan
//! sort-and-sweep kernels of `vesper-gpu-compute` behind
//! [`vesper_kinetic::BroadphaseMode::Gpu`].
//!
//! `vesper-gpu-compute` already depends on `vesper-kinetic` (its kernels
//! reproduce the CPU broadphase's pair contract), so the adapter lives in
//! this small crate instead of either of them. The device computes the
//! pair set with integer comparisons of order-preserving encodings of the
//! f32 bounds only, so the pairs, and therefore the world's bits, are the
//! same as with [`vesper_kinetic::BroadphaseMode::Lbvh`]. The world checks
//! the shape of the device's output before using it.
//!
//! Validated on Mesa lavapipe (`.github/workflows/gpu-compute.yml`); no
//! hardware GPU is exercised anywhere in CI.

#![deny(unsafe_code)]

use vesper_gpu_compute::Context;
use vesper_gpu_compute::broadphase::{BroadphaseError, GpuBroadphase};
use vesper_kinetic::lbvh::{Aabb, LbvhError};
use vesper_kinetic::{PairBackend, PairBackendError, World};

/// [`GpuBroadphase`] as a [`PairBackend`].
#[derive(Debug)]
pub struct GpuPairBackend {
    inner: GpuBroadphase,
    max_pairs: usize,
    name: String,
}

impl GpuPairBackend {
    /// Device buffers for up to `capacity` boxes and `max_pairs` pairs.
    pub fn new(ctx: &Context, capacity: usize, max_pairs: usize) -> Result<Self, BroadphaseError> {
        Ok(Self {
            inner: GpuBroadphase::new(ctx, capacity, max_pairs)?,
            max_pairs,
            name: format!("vulkan:{}", ctx.info().name),
        })
    }

    /// A backend sized for `world` (its body count and
    /// [`World::max_candidate_pairs`]).
    pub fn for_world(ctx: &Context, world: &World) -> Result<Self, BroadphaseError> {
        let n = world.bodies().len().max(1);
        let pairs = world
            .max_candidate_pairs()
            .clamp(1, vesper_gpu_compute::broadphase::MAX_PAIRS);
        Self::new(ctx, n, pairs)
    }
}

impl PairBackend for GpuPairBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn overlapping_pairs(
        &mut self,
        boxes: &[Aabb],
        out: &mut Vec<(u32, u32)>,
        max_pairs: usize,
    ) -> Result<(), PairBackendError> {
        self.inner
            .overlapping_pairs(boxes, out)
            .map_err(|e| match e {
                BroadphaseError::Kinetic(k) => PairBackendError::Broadphase(k),
                BroadphaseError::Gpu(_) => PairBackendError::Device,
            })?;
        let cap = max_pairs.min(self.max_pairs);
        if out.len() > cap {
            out.clear();
            return Err(PairBackendError::Broadphase(LbvhError::PairOverflow {
                capacity: cap,
            }));
        }
        Ok(())
    }
}
