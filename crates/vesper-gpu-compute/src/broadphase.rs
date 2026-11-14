//! The Kinetic broadphase (design §36.3) as compute kernels: sort and sweep
//! on the device, giving exactly the pair set of `vesper-kinetic`'s CPU
//! [`Lbvh`](vesper_kinetic::lbvh::Lbvh) and brute-force broadphases.
//!
//! The device sorts boxes by minimum x with a bitonic network over
//! `(key, index)` (a strict total order, so the sorted sequence is unique),
//! counts each box's overlaps with the boxes after it in that order, takes
//! an exclusive prefix sum of the counts, and writes every pair at its
//! offset. The output order is therefore a function of the input alone, and
//! every comparison is an integer comparison of order-preserving encodings
//! of the f32 bounds, so there is no floating-point arithmetic on the device
//! at all. [`GpuBroadphase::overlapping_pairs`] then returns the pairs sorted
//! ascending, exactly the CPU contract.
use vesper_kinetic::lbvh::{Aabb, LbvhError};

use crate::error::GpuError;
use crate::runtime::{Buffer, Context, Kernel};

/// Largest number of boxes one broadphase accepts. With 65,536 boxes the
/// pair count stays below 2^31, so `u32` counts and offsets cannot overflow.
pub const MAX_PRIMITIVES: usize = 1 << 16;
/// Largest pair capacity (2^24 pairs, 128 MiB of output).
pub const MAX_PAIRS: usize = 1 << 24;

const KEYS: &str = concat!(
    include_str!("shaders/broadphase.wgsl"),
    include_str!("shaders/bp_keys.wgsl")
);
const SORT: &str = concat!(
    include_str!("shaders/broadphase.wgsl"),
    include_str!("shaders/bp_sort.wgsl")
);
const SWEEP: &str = concat!(
    include_str!("shaders/broadphase.wgsl"),
    include_str!("shaders/bp_sweep.wgsl")
);
const SCAN: &str = concat!(
    include_str!("shaders/broadphase.wgsl"),
    include_str!("shaders/bp_scan.wgsl")
);

/// Every WGSL source of this module with its entry point.
pub const SOURCES: [(&str, &str); 4] = [
    ("keys_main", KEYS),
    ("sort_main", SORT),
    ("sweep_main", SWEEP),
    ("scan_main", SCAN),
];

/// Broadphase failures: the CPU broadphase's own typed errors where the
/// condition is the same, or a device error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BroadphaseError {
    Kinetic(LbvhError),
    Gpu(GpuError),
}

impl std::fmt::Display for BroadphaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kinetic(e) => write!(f, "{e}"),
            Self::Gpu(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BroadphaseError {}

impl From<GpuError> for BroadphaseError {
    fn from(e: GpuError) -> Self {
        Self::Gpu(e)
    }
}

fn valid(b: &Aabb) -> bool {
    (0..3).all(|a| b.min[a].is_finite() && b.max[a].is_finite() && b.min[a] <= b.max[a])
}

/// Device buffers and kernels for up to `capacity` boxes and `max_pairs`
/// overlapping pairs.
#[derive(Debug)]
pub struct GpuBroadphase {
    ctx: Context,
    keys_kernel: Kernel,
    sort_kernel: Kernel,
    sweep_kernel: Kernel,
    scan_kernel: Kernel,
    capacity: usize,
    max_pairs: usize,
    boxes: Buffer,
    keys: Buffer,
    ids: Buffer,
    counts: Buffer,
    offsets: Buffer,
    pairs: Buffer,
    last: Vec<(u32, u32)>,
}

impl GpuBroadphase {
    pub fn new(ctx: &Context, capacity: usize, max_pairs: usize) -> Result<Self, BroadphaseError> {
        if capacity == 0 || capacity > MAX_PRIMITIVES {
            return Err(GpuError::Limit("broadphase capacity").into());
        }
        if max_pairs == 0 || max_pairs > MAX_PAIRS {
            return Err(GpuError::Limit("broadphase pair capacity").into());
        }
        let padded = capacity.next_power_of_two();
        let words = |n: usize| ctx.buffer((n * 4) as u64);
        Ok(Self {
            ctx: ctx.clone(),
            keys_kernel: ctx.kernel(KEYS, "keys_main", 3, 16)?,
            sort_kernel: ctx.kernel(SORT, "sort_main", 2, 16)?,
            sweep_kernel: ctx.kernel(SWEEP, "sweep_main", 6, 16)?,
            scan_kernel: ctx.kernel(SCAN, "scan_main", 2, 16)?,
            capacity,
            max_pairs,
            boxes: words(capacity * 6)?,
            keys: words(padded)?,
            ids: words(padded)?,
            counts: words(capacity)?,
            offsets: words(capacity + 1)?,
            pairs: words(max_pairs * 2)?,
            last: Vec::new(),
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Every pair `(a, b)`, `a < b`, of boxes that overlap (closed
    /// intervals on all three axes), in ascending order: the same set and
    /// order as `Lbvh::overlapping_pairs` and `brute_force_pairs`. A
    /// non-finite or inverted box, more boxes than the capacity, or more
    /// pairs than `max_pairs` are the CPU broadphase's typed errors (and
    /// `out` is cleared).
    pub fn overlapping_pairs(
        &mut self,
        boxes: &[Aabb],
        out: &mut Vec<(u32, u32)>,
    ) -> Result<(), BroadphaseError> {
        out.clear();
        self.last.clear();
        if boxes.len() > self.capacity {
            return Err(BroadphaseError::Kinetic(LbvhError::CapacityExceeded {
                primitives: boxes.len(),
                capacity: self.capacity,
            }));
        }
        if let Some(index) = boxes.iter().position(|b| !valid(b)) {
            return Err(BroadphaseError::Kinetic(LbvhError::InvalidBox { index }));
        }
        let n = boxes.len();
        if n < 2 {
            return Ok(());
        }
        let mut words = Vec::with_capacity(n * 6);
        for b in boxes {
            words.extend(b.min.iter().chain(&b.max).map(|v| v.to_bits()));
        }
        self.boxes.write(0, &words)?;
        let padded = n.next_power_of_two();
        let (n32, padded32) = (n as u32, padded as u32);
        let groups = |items: usize| [items.div_ceil(64) as u32, 1, 1];
        let mut rec = self.ctx.record()?;
        rec.dispatch(
            &self.keys_kernel,
            &[&self.boxes, &self.keys, &self.ids],
            &[n32, padded32, 0, 0],
            groups(padded),
        )?;
        let mut k = 2u32;
        while k <= padded32 {
            let mut j = k / 2;
            while j > 0 {
                rec.dispatch(
                    &self.sort_kernel,
                    &[&self.keys, &self.ids],
                    &[n32, padded32, j, k],
                    groups(padded),
                )?;
                j /= 2;
            }
            k *= 2;
        }
        let sweep = [
            &self.boxes,
            &self.keys,
            &self.ids,
            &self.counts,
            &self.offsets,
            &self.pairs,
        ];
        let cap = self.max_pairs as u32;
        rec.dispatch(&self.sweep_kernel, &sweep, &[n32, 0, cap, 0], groups(n))?;
        rec.dispatch(
            &self.scan_kernel,
            &[&self.counts, &self.offsets],
            &[n32, 0, 0, 0],
            [1, 1, 1],
        )?;
        rec.dispatch(&self.sweep_kernel, &sweep, &[n32, 1, cap, 0], groups(n))?;
        rec.submit()?.wait()?;
        let mut total = [0u32];
        self.offsets.read((n * 4) as u64, &mut total)?;
        let total = total[0] as usize;
        if total > self.max_pairs {
            return Err(BroadphaseError::Kinetic(LbvhError::PairOverflow {
                capacity: self.max_pairs,
            }));
        }
        let mut raw = vec![0u32; total * 2];
        self.pairs.read(0, &mut raw)?;
        self.last
            .extend(raw.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])));
        out.extend_from_slice(&self.last);
        out.sort_unstable();
        Ok(())
    }

    /// The pairs of the last successful call in the order the device wrote
    /// them (sweep order), before the final sort. Used to check that the
    /// device output itself is deterministic.
    pub fn device_order(&self) -> &[(u32, u32)] {
        &self.last
    }

    /// The sorted `(key, index)` sequence of the last call, as computed on
    /// the device (exposed for tests of the bitonic sort).
    pub fn sorted_keys(&self, n: usize) -> Result<Vec<(u32, u32)>, GpuError> {
        if n > self.capacity {
            return Err(GpuError::Shape("more keys than the capacity"));
        }
        let mut keys = vec![0u32; n];
        let mut ids = vec![0u32; n];
        self.keys.read(0, &mut keys)?;
        self.ids.read(0, &mut ids)?;
        Ok(keys.into_iter().zip(ids).collect())
    }
}

/// The order-preserving `u32` encoding of a finite `f32` that the kernels
/// compare (with `-0` encoded as `+0`).
pub fn order_key(v: f32) -> u32 {
    let mut bits = v.to_bits();
    if bits == 0x8000_0000 {
        bits = 0;
    }
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits | 0x8000_0000
    }
}
