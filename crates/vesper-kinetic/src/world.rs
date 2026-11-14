//! Fixed-timestep XPBD world (§36.3): Morton-cell or LBVH broadphase,
//! deterministic colouring and graph-coloured positional projection (scalar
//! or in 16-lane batches), optional Coulomb friction and restitution, with no
//! allocation per step after construction. Per-step scratch lives either in
//! buffers reserved at construction ([`World::step`]) or in a caller's
//! `vesper-alloc` region, rewound after every step ([`World::step_in`]).
//!
//! All float arithmetic goes through [`det`] helpers: every operation is a
//! single IEEE-754 binary32 operation in round-to-nearest-even. Rust never
//! contracts `a * b + c` into a fused multiply-add (it has no fast-math
//! contraction; `f32::mul_add` is the only way to get one and this crate never
//! calls it), and the only non-basic operation used is `sqrt`, which IEEE-754
//! requires to be correctly rounded. No transcendental function is called.

// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::coloring::{Colorer, ColoringError, ConstraintBodies};
use crate::lbvh::{Aabb, Lbvh, LbvhError};
use crate::morton;
use crate::sap::{SapError, SweepAndPrune};
use crate::simd::{self, GroundLanes, PairLanes, SimdBackend, SimdUnavailable};
use crate::snapshot::{KineticSnapshotWireHdr, hash_world};
use crate::tile::{Body, TILE_WIDTH, TiledBodies};
use vesper_alloc::{ChunkBacking, Region, RegionConfig};

/// Deterministic binary32 operations (`det_add`, `det_mul`, ... in §36.3).
pub mod det {
    #[inline]
    pub fn add(a: f32, b: f32) -> f32 {
        a + b
    }
    #[inline]
    pub fn sub(a: f32, b: f32) -> f32 {
        a - b
    }
    #[inline]
    pub fn mul(a: f32, b: f32) -> f32 {
        a * b
    }
    #[inline]
    pub fn div(a: f32, b: f32) -> f32 {
        a / b
    }
    #[inline]
    pub fn neg(a: f32) -> f32 {
        -a
    }
    #[inline]
    pub fn sqrt(a: f32) -> f32 {
        a.sqrt()
    }
    /// `max` with a fixed answer for every input: `b` unless `a > b`, so
    /// signed zeros and NaNs never depend on the platform `max`.
    #[inline]
    pub fn max(a: f32, b: f32) -> f32 {
        if a > b { a } else { b }
    }
    /// Three-component dot product, evaluated left to right.
    #[inline]
    pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        add(add(mul(a[0], b[0]), mul(a[1], b[1])), mul(a[2], b[2]))
    }
}

/// A distance joint between two bodies (XPBD compliance `alpha`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceConstraint {
    pub a: u32,
    pub b: u32,
    pub rest_length: f32,
    pub compliance: f32,
}

/// Sphere collider referenced by a body's `collider_asset_id`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SphereCollider {
    pub radius: f32,
}

/// World parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldConfig {
    /// Fixed tick (default 1/120 s).
    pub dt: f32,
    /// XPBD substeps per tick (default 4).
    pub substeps: u32,
    pub gravity: [f32; 3],
    /// Height of an infinite static ground plane, if any.
    pub ground_y: Option<f32>,
    /// Broadphase cell edge; must be at least the largest collider diameter
    /// plus `contact_margin`.
    pub cell_size: f32,
    /// Corner of the 1024³-cell broadphase grid; positions outside clamp to
    /// the border cells (still correct, just more candidate pairs).
    pub grid_origin: [f32; 3],
    /// Speculative distance at which a contact is kept for the tick.
    pub contact_margin: f32,
    /// Contact storage reserved at construction.
    pub max_contacts: usize,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            dt: 1.0 / 120.0,
            substeps: 4,
            gravity: [0.0, -9.81, 0.0],
            ground_y: Some(0.0),
            cell_size: 1.0,
            grid_origin: [-512.0, -512.0, -512.0],
            contact_margin: 0.05,
            max_contacts: 4096,
        }
    }
}

/// Typed world construction and step failures. A failed step leaves the
/// world unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepError {
    /// Bad configuration value.
    InvalidConfig(&'static str),
    /// A body or joint field is non-finite or negative where it must not be.
    InvalidBody { body: usize, field: &'static str },
    /// A body names a collider that does not exist.
    UnknownCollider { body: usize, collider: u32 },
    /// A joint is malformed.
    InvalidJoint { joint: usize, reason: &'static str },
    /// More candidate contacts than `max_contacts`.
    ContactOverflow { capacity: usize },
    /// Colouring failed.
    Coloring(ColoringError),
    /// The tick counter would overflow.
    TickOverflow,
    /// Bodies move so fast that the swept contact search would need more
    /// than [`MAX_SEARCH_RADIUS`] cells in each direction.
    SweepTooLarge { body: usize },
    /// The step arena region refused the scratch allocation.
    Arena(vesper_alloc::RegionError),
    /// The LBVH broadphase failed.
    Broadphase(LbvhError),
    /// A contact material coefficient is out of range.
    InvalidMaterial(&'static str),
    /// The sort-and-sweep broadphase failed.
    SweepAndPrune(SapError),
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(what) => write!(f, "invalid world configuration: {what}"),
            Self::InvalidBody { body, field } => write!(f, "body {body} has invalid {field}"),
            Self::UnknownCollider { body, collider } => {
                write!(f, "body {body} names unknown collider {collider}")
            }
            Self::InvalidJoint { joint, reason } => write!(f, "joint {joint}: {reason}"),
            Self::ContactOverflow { capacity } => {
                write!(f, "more than {capacity} contacts in one tick")
            }
            Self::Coloring(e) => write!(f, "constraint colouring: {e}"),
            Self::TickOverflow => write!(f, "tick counter overflow"),
            Self::SweepTooLarge { body } => write!(
                f,
                "body {body} moves too far in one tick for the swept contact search"
            ),
            Self::Arena(e) => write!(f, "step arena: {e}"),
            Self::Broadphase(e) => write!(f, "LBVH broadphase: {e}"),
            Self::InvalidMaterial(what) => write!(f, "invalid contact material: {what}"),
            Self::SweepAndPrune(e) => write!(f, "sort-and-sweep broadphase: {e}"),
        }
    }
}

impl std::error::Error for StepError {}

impl From<ColoringError> for StepError {
    fn from(e: ColoringError) -> Self {
        Self::Coloring(e)
    }
}

/// Most broadphase cells searched in each direction around a body. The
/// search radius grows with the largest per-tick sweep so that fast bodies
/// still meet every body they can reach during the tick's substeps.
pub const MAX_SEARCH_RADIUS: i32 = 4;

/// Constraints per batch in [`SolverMode::Batched16`] (one AoSoA tile).
pub const BATCH_WIDTH: usize = TILE_WIDTH;

/// Default smallest colour that [`World::set_solver_threads`] splits across
/// threads ([`World::set_parallel_threshold`]); smaller colours are solved on the calling thread (same bits).
pub const PARALLEL_MIN_CONSTRAINTS: usize = 256;

/// Most solver threads.
pub const MAX_SOLVER_THREADS: usize = 64;

/// The body and contact-state writes of one solved batch.
#[derive(Debug, Clone, Copy)]
struct TileUpdate {
    writes: usize,
    body: [u32; 2 * BATCH_WIDTH],
    pos: [[f32; 3]; 2 * BATCH_WIDTH],
    states: usize,
    slot: [u32; BATCH_WIDTH],
    state: [[f32; 4]; BATCH_WIDTH],
}

impl TileUpdate {
    const EMPTY: Self = Self {
        writes: 0,
        body: [0; 2 * BATCH_WIDTH],
        pos: [[0.0; 3]; 2 * BATCH_WIDTH],
        states: 0,
        slot: [0; BATCH_WIDTH],
        state: [[0.0; 4]; BATCH_WIDTH],
    };

    #[inline]
    fn push_write(&mut self, body: usize, pos: [f32; 3]) {
        self.body[self.writes] = body as u32;
        self.pos[self.writes] = pos;
        self.writes += 1;
    }

    #[inline]
    fn push_state(&mut self, slot: usize, state: [f32; 4]) {
        self.slot[self.states] = slot as u32;
        self.state[self.states] = state;
        self.states += 1;
    }
}

/// Wall-clock time of the phases of the last successful step (a
/// diagnostic; it never influences the simulation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepTimings {
    /// Sweep bounds, broadphase and exact reach tests.
    pub broadphase: std::time::Duration,
    /// Constraint colouring and batch planning.
    pub coloring: std::time::Duration,
    /// Constraint projection, summed over substeps.
    pub solve: std::time::Duration,
    /// The whole step.
    pub total: std::time::Duration,
}

/// `BatchPlan::slot` of a joint (no contact state).
const NO_SLOT: u32 = u32::MAX;

/// One batch of a colour, fixed for a tick: the constraint lanes without
/// positions (gathered every substep).
#[derive(Debug, Clone, Copy)]
struct BatchPlan {
    ground: bool,
    count: u8,
    a: [u32; BATCH_WIDTH],
    b: [u32; BATCH_WIDTH],
    slot: [u32; BATCH_WIDTH],
    lanes: PairLanes,
}

impl BatchPlan {
    const EMPTY: Self = Self {
        ground: false,
        count: 0,
        a: [0; BATCH_WIDTH],
        b: [0; BATCH_WIDTH],
        slot: [NO_SLOT; BATCH_WIDTH],
        lanes: PairLanes {
            pa: [[0.0; BATCH_WIDTH]; 3],
            pb: [[0.0; BATCH_WIDTH]; 3],
            wa: [0.0; BATCH_WIDTH],
            wb: [0.0; BATCH_WIDTH],
            target: [0.0; BATCH_WIDTH],
            alpha: [0.0; BATCH_WIDTH],
            inequality: 0,
        },
    };
}

/// A reusable barrier that spins briefly before yielding: colour phases
/// last microseconds, shorter than a futex wake-up.
struct SpinBarrier {
    threads: usize,
    arrived: std::sync::atomic::AtomicUsize,
    generation: std::sync::atomic::AtomicUsize,
}

impl SpinBarrier {
    fn new(threads: usize) -> Self {
        Self {
            threads,
            arrived: std::sync::atomic::AtomicUsize::new(0),
            generation: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn wait(&self) {
        use std::sync::atomic::Ordering::{AcqRel, Acquire, Release};
        let generation = self.generation.load(Acquire);
        if self.arrived.fetch_add(1, AcqRel) + 1 == self.threads {
            self.arrived.store(0, Release);
            self.generation.fetch_add(1, Release);
            return;
        }
        let mut spins = 0u32;
        while self.generation.load(Acquire) == generation {
            if spins < 512 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                std::thread::yield_now();
            }
        }
    }
}

#[inline]
fn apply_update(bodies: &mut TiledBodies, u: &TileUpdate, state: &mut [[f32; 4]]) {
    for k in 0..u.writes {
        bodies.set_pos(u.body[k] as usize, u.pos[k]);
    }
    for k in 0..u.states {
        state[u.slot[k] as usize] = u.state[k];
    }
}

static NEXT_WORLD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// How each colour's constraints are projected. Both modes give the same
/// bits: the constraints of one colour touch disjoint bodies and every lane
/// performs exactly the scalar operation sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SolverMode {
    /// One constraint at a time, in colour order.
    #[default]
    Scalar,
    /// Gather up to [`BATCH_WIDTH`] constraints of one colour into lane
    /// arrays (a constraint AoSoA tile), solve them lane-wise, scatter back.
    Batched16,
}

/// Broadphase used by [`World::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BroadphaseMode {
    /// Sorted 30-bit Morton cells with a bounded neighbour search.
    #[default]
    MortonCells,
    /// 64-bit Morton LBVH ([`crate::lbvh`]); no search-radius limit.
    Lbvh,
    /// The data-parallel sort-and-sweep kernels ([`crate::sap`]) over the
    /// same boxes as [`BroadphaseMode::Lbvh`]: the same pairs, so the same
    /// bits.
    SweepAndPrune,
}

/// Contact material for every contact of a world (XPBD friction after
/// Macklin et al. 2014 and restitution after Müller et al. 2020). All zero
/// (the default) disables both and keeps the frictionless solver's bits.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContactMaterial {
    /// Coulomb static coefficient: tangential motion during a substep is
    /// cancelled while it is below `static_friction ×` the normal correction.
    pub static_friction: f32,
    /// Coulomb dynamic coefficient: otherwise the tangential motion shrinks
    /// by `dynamic_friction ×` the normal correction.
    pub dynamic_friction: f32,
    /// Normal restitution in `[0, 1]`, applied to the pre-substep normal
    /// approach speed; speeds below `2·|g|₁·h` do not bounce.
    pub restitution: f32,
}

/// A contact as stored in step scratch: `[a, b]`, `b == GROUND` for the
/// ground plane.
const GROUND: u32 = u32::MAX;

/// A deterministic XPBD world.
#[derive(Debug)]
pub struct World {
    config: WorldConfig,
    pub(crate) bodies: TiledBodies,
    pub(crate) tick: u64,
    pub(crate) id: u64,
    colliders: Vec<SphereCollider>,
    joints: Vec<DistanceConstraint>,
    solver: SolverMode,
    broadphase: BroadphaseMode,
    material: ContactMaterial,
    simd: SimdBackend,
    threads: usize,
    parallel_min: usize,
    par_out: Vec<Vec<TileUpdate>>,
    plans: Vec<BatchPlan>,
    plan_ranges: Vec<(u32, u32)>,
    timings: StepTimings,
    // Per-step scratch, reserved at construction.
    prev: Vec<[f32; 3]>,
    prev_vel: Vec<[f32; 3]>,
    sweep: Vec<f32>,
    cells_buf: Vec<[u32; 2]>,
    contacts_buf: Vec<[u32; 2]>,
    state_buf: Vec<[f32; 4]>,
    constraint_bodies: Vec<ConstraintBodies>,
    colorer: Colorer,
    lbvh: Option<LbvhScratch>,
    last_layer_count: usize,
    last_manifolds: usize,
}

#[derive(Debug)]
struct LbvhScratch {
    tree: Lbvh,
    sap: SweepAndPrune,
    boxes: Vec<Aabb>,
    pairs: Vec<(u32, u32)>,
    max_pairs: usize,
}

fn finite(v: f32) -> bool {
    v.is_finite()
}

/// Bytes of per-step scratch for `bodies` bodies and `contacts` contacts.
fn scratch_bytes(bodies: usize, contacts: usize) -> Option<usize> {
    let cells = bodies.checked_mul(8)?;
    let pairs = contacts.checked_mul(8)?;
    let state = contacts.checked_mul(16)?;
    state.checked_add(pairs)?.checked_add(cells)
}

impl World {
    /// Build a world. Validates every input and reserves all per-step
    /// storage.
    pub fn new(
        config: WorldConfig,
        bodies: &[Body],
        colliders: &[SphereCollider],
        joints: &[DistanceConstraint],
    ) -> Result<Self, StepError> {
        if !(finite(config.dt) && config.dt > 0.0) {
            return Err(StepError::InvalidConfig("dt must be finite and positive"));
        }
        if config.substeps == 0 || config.substeps > 64 {
            return Err(StepError::InvalidConfig("substeps must be in 1..=64"));
        }
        if !(config.gravity.iter().all(|g| finite(*g))
            && config.grid_origin.iter().all(|g| finite(*g))
            && config.ground_y.is_none_or(finite))
        {
            return Err(StepError::InvalidConfig("non-finite vector"));
        }
        if !(finite(config.cell_size) && config.cell_size > 0.0) {
            return Err(StepError::InvalidConfig("cell_size must be positive"));
        }
        if !(finite(config.contact_margin) && config.contact_margin >= 0.0) {
            return Err(StepError::InvalidConfig("contact_margin must be >= 0"));
        }
        if bodies.len() > u32::MAX as usize / 2 {
            return Err(StepError::InvalidConfig("too many bodies"));
        }
        let mut max_diameter = 0.0f32;
        for c in colliders {
            if !(finite(c.radius) && c.radius > 0.0) {
                return Err(StepError::InvalidConfig("collider radius must be positive"));
            }
            max_diameter = det::max(det::add(c.radius, c.radius), max_diameter);
        }
        if det::add(max_diameter, config.contact_margin) > config.cell_size {
            return Err(StepError::InvalidConfig(
                "cell_size smaller than largest collider diameter plus margin",
            ));
        }
        for (i, b) in bodies.iter().enumerate() {
            if !b.pos.iter().chain(&b.vel).all(|v| finite(*v)) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "position or velocity",
                });
            }
            if !(finite(b.inv_mass) && b.inv_mass >= 0.0) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "inv_mass",
                });
            }
            if !(finite(b.compliance_alpha) && b.compliance_alpha >= 0.0) {
                return Err(StepError::InvalidBody {
                    body: i,
                    field: "compliance_alpha",
                });
            }
            if b.collider_asset_id as usize >= colliders.len() {
                return Err(StepError::UnknownCollider {
                    body: i,
                    collider: b.collider_asset_id,
                });
            }
        }
        for (j, joint) in joints.iter().enumerate() {
            if joint.a as usize >= bodies.len() || joint.b as usize >= bodies.len() {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "unknown body",
                });
            }
            if joint.a == joint.b {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "links a body to itself",
                });
            }
            if !(finite(joint.rest_length)
                && joint.rest_length >= 0.0
                && finite(joint.compliance)
                && joint.compliance >= 0.0)
            {
                return Err(StepError::InvalidJoint {
                    joint: j,
                    reason: "rest length and compliance must be finite and >= 0",
                });
            }
        }
        let constraint_capacity = joints
            .len()
            .checked_add(config.max_contacts)
            .ok_or(StepError::InvalidConfig("max_contacts too large"))?;
        if config.max_contacts >= GROUND as usize
            || scratch_bytes(bodies.len(), config.max_contacts).is_none()
        {
            return Err(StepError::InvalidConfig("max_contacts too large"));
        }
        Ok(Self {
            config,
            bodies: TiledBodies::from_bodies(bodies),
            tick: 0,
            id: NEXT_WORLD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            colliders: colliders.to_vec(),
            joints: joints.to_vec(),
            solver: SolverMode::Scalar,
            broadphase: BroadphaseMode::MortonCells,
            material: ContactMaterial::default(),
            simd: SimdBackend::detect(),
            threads: 1,
            parallel_min: PARALLEL_MIN_CONSTRAINTS,
            par_out: Vec::new(),
            plans: Vec::new(),
            plan_ranges: Vec::new(),
            timings: StepTimings::default(),
            prev: vec![[0.0; 3]; bodies.len()],
            prev_vel: vec![[0.0; 3]; bodies.len()],
            sweep: vec![0.0; bodies.len()],
            cells_buf: vec![[0; 2]; bodies.len()],
            contacts_buf: vec![[0; 2]; config.max_contacts],
            state_buf: vec![[0.0; 4]; config.max_contacts],
            constraint_bodies: Vec::with_capacity(constraint_capacity),
            colorer: Colorer::new(bodies.len(), constraint_capacity),
            lbvh: None,
            last_layer_count: 0,
            last_manifolds: 0,
        })
    }

    /// Ticks simulated so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The bodies.
    pub fn bodies(&self) -> &TiledBodies {
        &self.bodies
    }

    /// The configuration.
    pub fn config(&self) -> &WorldConfig {
        &self.config
    }

    /// Phase timings of the last successful step.
    pub fn last_timings(&self) -> StepTimings {
        self.timings
    }

    /// Colours used by the last step.
    pub fn last_layer_count(&self) -> usize {
        self.last_layer_count
    }

    /// The solver mode.
    pub fn solver_mode(&self) -> SolverMode {
        self.solver
    }

    /// Choose scalar or 16-lane batched projection (same bits either way).
    pub fn set_solver_mode(&mut self, mode: SolverMode) {
        self.solver = mode;
        if mode == SolverMode::Batched16 {
            // Each colour wastes at most one partial pair and one partial
            // ground plan.
            let plans =
                self.colorer_capacity().div_ceil(BATCH_WIDTH) + 2 * crate::coloring::MAX_COLORS + 2;
            self.plans.reserve(plans.saturating_sub(self.plans.len()));
            self.plan_ranges.reserve(crate::coloring::MAX_COLORS + 1);
        }
    }

    /// The instruction set used by [`SolverMode::Batched16`] (default: the
    /// widest the host supports, [`SimdBackend::detect`]).
    pub fn simd_backend(&self) -> SimdBackend {
        self.simd
    }

    /// Choose the batch kernel backend. Every backend gives the same bits;
    /// one the host cannot run is a typed error and changes nothing.
    pub fn set_simd_backend(&mut self, backend: SimdBackend) -> Result<(), SimdUnavailable> {
        self.simd = backend.require()?;
        Ok(())
    }

    /// Solver threads used by [`SolverMode::Batched16`] for colours of at
    /// least [`PARALLEL_MIN_CONSTRAINTS`] constraints.
    pub fn solver_threads(&self) -> usize {
        self.threads
    }

    /// Split large colours across `threads` scoped threads (1 = serial,
    /// at most [`MAX_SOLVER_THREADS`]). The result is bit-identical for
    /// every thread count. Reserves each thread's output buffer now; the
    /// threads themselves are spawned per colour, which allocates, so the
    /// zero-allocation guarantee of [`World::step_in`] holds for one thread
    /// only. Only the batched solver is parallel.
    pub fn set_solver_threads(&mut self, threads: usize) -> Result<(), StepError> {
        if threads == 0 || threads > MAX_SOLVER_THREADS {
            return Err(StepError::InvalidConfig("solver threads must be in 1..=64"));
        }
        let tiles =
            self.colorer_capacity().div_ceil(BATCH_WIDTH) + 2 * crate::coloring::MAX_COLORS + 2;
        while self.par_out.len() < threads {
            self.par_out.push(Vec::with_capacity(tiles));
        }
        self.threads = threads;
        Ok(())
    }

    /// Smallest colour split across threads (default
    /// [`PARALLEL_MIN_CONSTRAINTS`]; at least 1). Any value gives the same
    /// bits; it only trades thread start-up cost against parallel work.
    pub fn set_parallel_threshold(&mut self, min_constraints: usize) {
        self.parallel_min = min_constraints.max(1);
    }

    /// Whether some colour of this tick is large enough to split.
    fn parallel_worthwhile(&self) -> bool {
        self.threads > 1
            && (0..self.colorer.layer_count())
                .any(|c| self.colorer.layer(c).len() >= self.parallel_min)
    }

    fn colorer_capacity(&self) -> usize {
        self.joints.len() + self.config.max_contacts
    }

    /// The broadphase.
    pub fn broadphase(&self) -> BroadphaseMode {
        self.broadphase
    }

    /// Choose the broadphase. Switching to [`BroadphaseMode::Lbvh`] reserves
    /// the tree and room for `4 · max_contacts + bodies` candidate pairs, so
    /// steps still do not allocate; more candidates than that fail the step
    /// with [`StepError::Broadphase`].
    pub fn set_broadphase(&mut self, mode: BroadphaseMode) {
        self.broadphase = mode;
        if mode != BroadphaseMode::MortonCells && self.lbvh.is_none() {
            let n = self.bodies.len();
            let max_pairs = self.config.max_contacts.saturating_mul(4).saturating_add(n);
            self.lbvh = Some(LbvhScratch {
                tree: Lbvh::with_capacity(if mode == BroadphaseMode::Lbvh { n } else { 0 }),
                sap: SweepAndPrune::with_capacity(0),
                boxes: Vec::with_capacity(n),
                pairs: Vec::with_capacity(max_pairs),
                max_pairs,
            });
        }
        if let Some(scratch) = self.lbvh.as_mut() {
            let n = self.bodies.len();
            match mode {
                BroadphaseMode::Lbvh if scratch.tree.capacity() < n => {
                    scratch.tree = Lbvh::with_capacity(n)
                }
                BroadphaseMode::SweepAndPrune if scratch.sap.capacity() < n => {
                    scratch.sap = SweepAndPrune::with_capacity(n);
                    scratch.sap.reserve_pairs(scratch.max_pairs);
                }
                _ => {}
            }
        }
    }

    /// The contact material.
    pub fn contact_material(&self) -> ContactMaterial {
        self.material
    }

    /// Set friction and restitution for every contact.
    pub fn set_contact_material(&mut self, material: ContactMaterial) -> Result<(), StepError> {
        let ContactMaterial {
            static_friction,
            dynamic_friction,
            restitution,
        } = material;
        if !(finite(static_friction) && static_friction >= 0.0) {
            return Err(StepError::InvalidMaterial("static_friction must be >= 0"));
        }
        if !(finite(dynamic_friction) && dynamic_friction >= 0.0) {
            return Err(StepError::InvalidMaterial("dynamic_friction must be >= 0"));
        }
        if !(finite(restitution) && (0.0..=1.0).contains(&restitution)) {
            return Err(StepError::InvalidMaterial("restitution must be in [0, 1]"));
        }
        self.material = material;
        Ok(())
    }

    /// Add `dv` to the velocity of a dynamic body (a rollback "input").
    /// Rejects unknown bodies, static bodies and non-finite deltas.
    pub fn apply_velocity_change(&mut self, body: usize, dv: [f32; 3]) -> Result<(), StepError> {
        if body >= self.bodies.len() || !dv.iter().all(|v| finite(*v)) {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change",
            });
        }
        if self.bodies.inv_mass(body) == 0.0 {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change on a static body",
            });
        }
        let v = self.bodies.vel(body);
        let next = [
            det::add(v[0], dv[0]),
            det::add(v[1], dv[1]),
            det::add(v[2], dv[2]),
        ];
        if !next.iter().all(|x| finite(*x)) {
            return Err(StepError::InvalidBody {
                body,
                field: "velocity change overflows",
            });
        }
        self.bodies.set_vel(body, next);
        Ok(())
    }

    fn radius(&self, body: usize) -> f32 {
        self.colliders[self.bodies.collider(body) as usize].radius
    }

    fn cell_of(&self, p: [f32; 3]) -> [u16; 3] {
        let mut out = [0u16; 3];
        for axis in 0..3 {
            let q = det::div(
                det::sub(p[axis], self.config.grid_origin[axis]),
                self.config.cell_size,
            )
            .floor();
            // Saturating float-to-int conversion is defined for every input.
            out[axis] = (q as i32).clamp(0, i32::from(morton::AXIS_MAX)) as u16;
        }
        out
    }

    /// Conservative bound on how far body `i` can travel this tick:
    /// `(|vx| + |vy| + |vz|)·dt + (|gx| + |gy| + |gz|)·dt²` (an L1 bound on the
    /// L2 distance covered by the substeps' ballistic motion; constraint
    /// corrections are not included).
    fn sweep_of(&self, i: usize) -> f32 {
        if self.bodies.inv_mass(i) == 0.0 {
            return 0.0;
        }
        let v = self.bodies.vel(i);
        let g = self.config.gravity;
        let dt = self.config.dt;
        let speed = det::add(det::add(v[0].abs(), v[1].abs()), v[2].abs());
        let accel = det::add(det::add(g[0].abs(), g[1].abs()), g[2].abs());
        det::add(det::mul(speed, dt), det::mul(det::mul(accel, dt), dt))
    }

    /// Whether bodies `i < j` are a contact this tick (exact narrowphase).
    fn pair_in_reach(&self, i: usize, j: usize) -> bool {
        let wi = self.bodies.inv_mass(i);
        let wj = self.bodies.inv_mass(j);
        if wi == 0.0 && wj == 0.0 {
            return false;
        }
        if self.bodies.layer_mask(i) & self.bodies.layer_mask(j) == 0 {
            return false;
        }
        let pi = self.bodies.pos(i);
        let pj = self.bodies.pos(j);
        let d = [
            det::sub(pi[0], pj[0]),
            det::sub(pi[1], pj[1]),
            det::sub(pi[2], pj[2]),
        ];
        let reach = det::add(
            det::add(
                det::add(self.radius(i), self.radius(j)),
                self.config.contact_margin,
            ),
            det::add(self.sweep[i], self.sweep[j]),
        );
        det::dot(d, d) < det::mul(reach, reach)
    }

    fn ground_in_reach(&self, i: usize) -> bool {
        match self.config.ground_y {
            Some(ground) => {
                let wi = self.bodies.inv_mass(i);
                let pi = self.bodies.pos(i);
                wi > 0.0
                    && det::sub(det::sub(pi[1], self.radius(i)), ground)
                        < det::add(self.config.contact_margin, self.sweep[i])
            }
            None => false,
        }
    }

    /// Broadphase and narrowphase: fills `contacts` with every pair (and
    /// ground contact) that is within reach during the tick: radii plus
    /// margin plus both bodies' sweeps. Contacts are kept for all substeps
    /// and only push when they actually penetrate. Returns the count.
    fn detect_contacts(
        &mut self,
        cells: &mut [[u32; 2]],
        contacts: &mut [[u32; 2]],
    ) -> Result<usize, StepError> {
        let n = self.bodies.len();
        let mut max_sweep = 0.0f32;
        for i in 0..n {
            let s = self.sweep_of(i);
            if !s.is_finite() {
                return Err(StepError::SweepTooLarge { body: i });
            }
            self.sweep[i] = s;
            max_sweep = det::max(s, max_sweep);
        }
        let mut count = 0usize;
        let capacity = contacts.len();
        let mut push = |c: [u32; 2], count: &mut usize| {
            if *count == capacity {
                return Err(StepError::ContactOverflow { capacity });
            }
            contacts[*count] = c;
            *count += 1;
            Ok(())
        };
        if self.broadphase != BroadphaseMode::MortonCells {
            let mut scratch = self.lbvh.take().expect("reserved by set_broadphase");
            let result: Result<(), StepError> = (|| {
                scratch.boxes.clear();
                for i in 0..n {
                    if self.ground_in_reach(i) {
                        push([i as u32, GROUND], &mut count)?;
                    }
                    // Half extent: radius + sweep + margin/2, inflated so that
                    // box rounding never drops a pair the exact test keeps.
                    let p = self.bodies.pos(i);
                    let half = det::add(
                        det::add(self.radius(i), self.sweep[i]),
                        det::mul(self.config.contact_margin, 0.5),
                    );
                    let mag = det::max(det::max(p[0].abs(), p[1].abs()), p[2].abs());
                    let half = det::add(
                        det::mul(half, 1.0078125),
                        det::mul(det::add(mag, 1.0), 1.0 / 1_048_576.0),
                    );
                    scratch.boxes.push(Aabb::around(p, half));
                }
                if self.broadphase == BroadphaseMode::Lbvh {
                    scratch
                        .tree
                        .build(&scratch.boxes)
                        .map_err(StepError::Broadphase)?;
                    scratch
                        .tree
                        .overlapping_pairs(&mut scratch.pairs, scratch.max_pairs)
                        .map_err(StepError::Broadphase)?;
                } else {
                    scratch
                        .sap
                        .build(&scratch.boxes)
                        .map_err(StepError::SweepAndPrune)?;
                    scratch
                        .sap
                        .overlapping_pairs(&mut scratch.pairs, scratch.max_pairs)
                        .map_err(StepError::SweepAndPrune)?;
                }
                for &(i, j) in &scratch.pairs {
                    if self.pair_in_reach(i as usize, j as usize) {
                        push([i, j], &mut count)?;
                    }
                }
                Ok(())
            })();
            self.lbvh = Some(scratch);
            result?;
            return Ok(count);
        }
        for (i, cell) in cells.iter_mut().enumerate().take(n) {
            let [x, y, z] = self.cell_of(self.bodies.pos(i));
            *cell = [morton::encode_masked(x, y, z), i as u32];
        }
        let cells = &mut cells[..n];
        cells.sort_unstable();
        let cell_size = self.config.cell_size;
        for i in 0..n {
            let pi = self.bodies.pos(i);
            let si = self.sweep[i];
            if self.ground_in_reach(i) {
                push([i as u32, GROUND], &mut count)?;
            }
            // Cells are at least one largest diameter plus margin wide, so a
            // partner within reach lies within `1 + ceil((si + max_sweep) /
            // cell_size)` cells.
            let extra = det::div(det::add(si, max_sweep), cell_size).ceil();
            if extra >= MAX_SEARCH_RADIUS as f32 {
                return Err(StepError::SweepTooLarge { body: i });
            }
            let r = 1 + extra as i32;
            let cell = self.cell_of(pi);
            for dz in -r..=r {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let neighbour = [
                            i32::from(cell[0]) + dx,
                            i32::from(cell[1]) + dy,
                            i32::from(cell[2]) + dz,
                        ];
                        if neighbour
                            .iter()
                            .any(|c| *c < 0 || *c > i32::from(morton::AXIS_MAX))
                        {
                            continue;
                        }
                        let code = morton::encode_masked(
                            neighbour[0] as u16,
                            neighbour[1] as u16,
                            neighbour[2] as u16,
                        );
                        let start = cells.partition_point(|e| e[0] < code);
                        let mut k = start;
                        while k < cells.len() && cells[k][0] == code {
                            let j = cells[k][1] as usize;
                            k += 1;
                            if j <= i {
                                continue;
                            }
                            if self.pair_in_reach(i, j) {
                                push([i as u32, j as u32], &mut count)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(count)
    }

    /// Advance one fixed tick using the scratch buffers reserved at
    /// construction. Returns the §36.2 snapshot header for the new state. On
    /// error the world is unchanged.
    pub fn step(&mut self) -> Result<KineticSnapshotWireHdr, StepError> {
        let mut cells = std::mem::take(&mut self.cells_buf);
        let mut contacts = std::mem::take(&mut self.contacts_buf);
        let mut state = std::mem::take(&mut self.state_buf);
        let result = self.advance(&mut cells, &mut contacts, &mut state);
        self.cells_buf = cells;
        self.contacts_buf = contacts;
        self.state_buf = state;
        result
    }

    /// A region configuration that holds one step's scratch for this world
    /// in a single retained chunk, for [`World::step_in`].
    pub fn step_arena_config(&self) -> RegionConfig {
        let bytes = scratch_bytes(self.bodies.len(), self.config.max_contacts)
            .expect("checked at construction")
            .max(64)
            .next_multiple_of(64);
        RegionConfig {
            initial_chunk_bytes: bytes,
            max_chunk_bytes: bytes,
            max_reserved_bytes: bytes,
            max_align: 64,
            max_allocations: 16,
            max_checkpoints: 4,
            retained_bytes: bytes,
        }
    }

    /// [`World::step`] with this step's scratch (Morton cells, contacts and
    /// contact state) allocated in `arena` after a watermark and released by
    /// rewinding to it before returning, success or not. Same bits as
    /// [`World::step`]. An arena too small for the scratch fails with
    /// [`StepError::Arena`] and leaves the world unchanged.
    pub fn step_in<B: ChunkBacking>(
        &mut self,
        arena: &mut Region<B>,
    ) -> Result<KineticSnapshotWireHdr, StepError> {
        let n = self.bodies.len();
        let c = self.config.max_contacts;
        let total = scratch_bytes(n, c)
            .expect("checked at construction")
            .max(16);
        let mark = arena.checkpoint().map_err(StepError::Arena)?;
        let result = match arena.alloc_bytes(total, 16) {
            Err(e) => Err(StepError::Arena(e.into())),
            Ok(handle) => match arena.bytes_mut(&handle) {
                Err(e) => Err(StepError::Arena(e.into())),
                Ok(bytes) => {
                    let (state, rest) = bytes.split_at_mut(16 * c);
                    let (contacts, rest) = rest.split_at_mut(8 * c);
                    let cells = &mut rest[..8 * n];
                    let state: &mut [[f32; 4]] = bytemuck::cast_slice_mut(state);
                    let contacts: &mut [[u32; 2]] = bytemuck::cast_slice_mut(contacts);
                    let cells: &mut [[u32; 2]] = bytemuck::cast_slice_mut(cells);
                    self.advance(cells, contacts, state)
                }
            },
        };
        // Nothing linear was allocated after `mark`, so the rewind cannot
        // fail; if it somehow did, the arena keeps the scratch until reset.
        let rewound = arena.rewind(mark);
        debug_assert!(rewound.is_ok(), "step arena rewind: {rewound:?}");
        result
    }

    fn advance(
        &mut self,
        cells: &mut [[u32; 2]],
        contacts: &mut [[u32; 2]],
        state: &mut [[f32; 4]],
    ) -> Result<KineticSnapshotWireHdr, StepError> {
        let next_tick = self.tick.checked_add(1).ok_or(StepError::TickOverflow)?;
        let t0 = std::time::Instant::now();
        let count = self.detect_contacts(cells, contacts)?;
        let contacts = &contacts[..count];
        let state = &mut state[..count];
        self.constraint_bodies.clear();
        for j in &self.joints {
            self.constraint_bodies.push(ConstraintBodies {
                a: j.a,
                b: Some(j.b),
            });
        }
        for c in contacts {
            self.constraint_bodies.push(ConstraintBodies {
                a: c[0],
                b: (c[1] != GROUND).then_some(c[1]),
            });
        }
        let t1 = std::time::Instant::now();
        self.colorer.color(&self.constraint_bodies)?;
        if self.solver == SolverMode::Batched16 {
            self.build_plans(contacts);
        }
        self.last_layer_count = self.colorer.layer_count();
        self.last_manifolds = count;

        let t2 = std::time::Instant::now();
        let mut solve = std::time::Duration::ZERO;
        // Bodies are only mutated from here on; nothing below can fail.
        let h = det::div(self.config.dt, self.config.substeps as f32);
        let inv_h = det::div(1.0, h);
        let inv_h2 = det::mul(inv_h, inv_h);
        let g = self.config.gravity;
        let n = self.bodies.len();
        let restitution = self.material.restitution > 0.0;
        for _ in 0..self.config.substeps {
            for i in 0..n {
                let p = self.bodies.pos(i);
                self.prev[i] = p;
                self.prev_vel[i] = self.bodies.vel(i);
                if self.bodies.inv_mass(i) == 0.0 {
                    continue;
                }
                let v = self.bodies.vel(i);
                let v = [
                    det::add(v[0], det::mul(g[0], h)),
                    det::add(v[1], det::mul(g[1], h)),
                    det::add(v[2], det::mul(g[2], h)),
                ];
                self.bodies.set_vel(i, v);
                self.bodies.set_pos(
                    i,
                    [
                        det::add(p[0], det::mul(v[0], h)),
                        det::add(p[1], det::mul(v[1], h)),
                        det::add(p[2], det::mul(v[2], h)),
                    ],
                );
            }
            if restitution {
                state.fill([0.0; 4]);
            }
            let ts = std::time::Instant::now();
            match self.solver {
                SolverMode::Scalar => {
                    for color in 0..self.colorer.layer_count() {
                        let layer_len = self.colorer.layer(color).len();
                        for k in 0..layer_len {
                            let index = self.colorer.layer(color)[k] as usize;
                            self.project(index, inv_h2, contacts, state);
                        }
                    }
                }
                SolverMode::Batched16 if self.parallel_worthwhile() => {
                    self.project_colors_parallel(inv_h2, state)
                }
                SolverMode::Batched16 => {
                    for color in 0..self.plan_ranges.len() {
                        self.project_color_batched(color, inv_h2, state);
                    }
                }
            }
            solve += ts.elapsed();
            for i in 0..n {
                if self.bodies.inv_mass(i) == 0.0 {
                    continue;
                }
                let p = self.bodies.pos(i);
                let q = self.prev[i];
                self.bodies.set_vel(
                    i,
                    [
                        det::mul(det::sub(p[0], q[0]), inv_h),
                        det::mul(det::sub(p[1], q[1]), inv_h),
                        det::mul(det::sub(p[2], q[2]), inv_h),
                    ],
                );
            }
            if restitution {
                self.apply_restitution(h, contacts, state);
            }
        }
        self.tick = next_tick;
        self.timings = StepTimings {
            broadphase: t1 - t0,
            coloring: t2 - t1,
            solve,
            total: t0.elapsed(),
        };
        Ok(self.header())
    }

    /// The snapshot header (tick, counts, BLAKE3 of canonical state).
    pub fn header(&self) -> KineticSnapshotWireHdr {
        KineticSnapshotWireHdr {
            tick_number: self.tick,
            active_body_count: self.bodies.len() as u32,
            manifold_count: self.last_manifolds as u32,
            world_state_blake3: hash_world(self.tick, &self.bodies).blake3,
        }
    }

    /// Decode constraint `index`: `(a, b, target, compliance, inequality,
    /// contact slot)`; `b == None` is the ground plane.
    #[inline]
    fn constraint(
        &self,
        index: usize,
        contacts: &[[u32; 2]],
    ) -> (usize, Option<usize>, f32, f32, bool, Option<usize>) {
        let joints = self.joints.len();
        if index < joints {
            let j = self.joints[index];
            (
                j.a as usize,
                Some(j.b as usize),
                j.rest_length,
                j.compliance,
                false,
                None,
            )
        } else {
            let slot = index - joints;
            let [a, b] = contacts[slot];
            let a = a as usize;
            if b == GROUND {
                (a, None, 0.0, self.bodies.compliance(a), true, Some(slot))
            } else {
                let b = b as usize;
                (
                    a,
                    Some(b),
                    det::add(self.radius(a), self.radius(b)),
                    det::add(self.bodies.compliance(a), self.bodies.compliance(b)),
                    true,
                    Some(slot),
                )
            }
        }
    }

    fn friction_enabled(&self) -> bool {
        self.material.static_friction > 0.0 || self.material.dynamic_friction > 0.0
    }

    /// Project constraint `index` (joints first, then this tick's contacts).
    fn project(
        &mut self,
        index: usize,
        inv_h2: f32,
        contacts: &[[u32; 2]],
        state: &mut [[f32; 4]],
    ) {
        let (a, b, target, alpha, inequality, slot) = self.constraint(index, contacts);
        let alpha_tilde = det::mul(alpha, inv_h2);
        let wa = self.bodies.inv_mass(a);
        let pa = self.bodies.pos(a);
        match b {
            Some(b) => {
                let wb = self.bodies.inv_mass(b);
                let pb = self.bodies.pos(b);
                let d = [
                    det::sub(pa[0], pb[0]),
                    det::sub(pa[1], pb[1]),
                    det::sub(pa[2], pb[2]),
                ];
                let len = det::sqrt(det::dot(d, d));
                // Coincident centres have no direction; separate along +y
                // (a deterministic choice) so the correction is never dropped.
                let (len, d) = if len <= EPS {
                    (0.0, [0.0, 1.0, 0.0])
                } else {
                    (len, d)
                };
                let c = det::sub(len, target);
                if inequality && c >= 0.0 {
                    return;
                }
                let w = det::add(wa, wb);
                if w == 0.0 {
                    return;
                }
                let denom = det::max(det::add(w, alpha_tilde), EPS);
                let d_lambda = det::div(det::neg(c), denom);
                let inv_len = if len == 0.0 { 1.0 } else { det::div(1.0, len) };
                let n = [
                    det::mul(d[0], inv_len),
                    det::mul(d[1], inv_len),
                    det::mul(d[2], inv_len),
                ];
                let sa = det::mul(wa, d_lambda);
                let sb = det::mul(wb, d_lambda);
                let mut na = [
                    det::add(pa[0], det::mul(sa, n[0])),
                    det::add(pa[1], det::mul(sa, n[1])),
                    det::add(pa[2], det::mul(sa, n[2])),
                ];
                let mut nb = [
                    det::sub(pb[0], det::mul(sb, n[0])),
                    det::sub(pb[1], det::mul(sb, n[1])),
                    det::sub(pb[2], det::mul(sb, n[2])),
                ];
                if let Some(slot) = slot {
                    let corr = det::add(sa, sb);
                    if self.friction_enabled() {
                        let (qa, qb) = (self.prev[a], self.prev[b]);
                        self.apply_friction_pair(&mut na, &mut nb, qa, qb, n, corr, wa, wb, w);
                    }
                    state[slot] = [corr, n[0], n[1], n[2]];
                }
                self.bodies.set_pos(a, na);
                self.bodies.set_pos(b, nb);
            }
            None => {
                let ground = self.config.ground_y.unwrap_or(0.0);
                let c = det::sub(det::sub(pa[1], self.radius(a)), ground);
                if c >= 0.0 || wa == 0.0 {
                    return;
                }
                let denom = det::max(det::add(wa, alpha_tilde), EPS);
                let d_lambda = det::div(det::neg(c), denom);
                let mut na = [pa[0], det::add(pa[1], det::mul(wa, d_lambda)), pa[2]];
                let corr = det::mul(wa, d_lambda);
                if self.friction_enabled() {
                    let qa = self.prev[a];
                    self.apply_friction_ground(&mut na, qa, corr);
                }
                if let Some(slot) = slot {
                    state[slot] = [corr, 0.0, 1.0, 0.0];
                }
                self.bodies.set_pos(a, na);
            }
        }
    }

    /// Coulomb friction on a pair contact at the position level: the
    /// relative tangential displacement of this substep is cancelled
    /// (static) or reduced by `μ_k · corr` (dynamic), shared by inverse mass.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn apply_friction_pair(
        &self,
        na: &mut [f32; 3],
        nb: &mut [f32; 3],
        qa: [f32; 3],
        qb: [f32; 3],
        n: [f32; 3],
        corr: f32,
        wa: f32,
        wb: f32,
        w: f32,
    ) {
        let dp = [
            det::sub(det::sub(na[0], qa[0]), det::sub(nb[0], qb[0])),
            det::sub(det::sub(na[1], qa[1]), det::sub(nb[1], qb[1])),
            det::sub(det::sub(na[2], qa[2]), det::sub(nb[2], qb[2])),
        ];
        if let Some(t) = friction_correction(dp, n, corr, self.material) {
            let fa = det::div(wa, w);
            let fb = det::div(wb, w);
            for k in 0..3 {
                na[k] = det::add(na[k], det::mul(t[k], fa));
                nb[k] = det::sub(nb[k], det::mul(t[k], fb));
            }
        }
    }

    #[inline]
    fn apply_friction_ground(&self, na: &mut [f32; 3], qa: [f32; 3], corr: f32) {
        let dp = [
            det::sub(na[0], qa[0]),
            det::sub(na[1], qa[1]),
            det::sub(na[2], qa[2]),
        ];
        if let Some(t) = friction_correction(dp, [0.0, 1.0, 0.0], corr, self.material) {
            for k in 0..3 {
                na[k] = det::add(na[k], t[k]);
            }
        }
    }

    /// Group every colour's constraints into batch plans of up to
    /// [`BATCH_WIDTH`] lanes, once per tick: two-body constraints and
    /// ground contacts go to separate plans. Bodies, targets, compliance,
    /// inverse masses and contact slots do not change during a tick, so the
    /// substeps only gather positions.
    fn build_plans(&mut self, contacts: &[[u32; 2]]) {
        let mut plans = std::mem::take(&mut self.plans);
        let mut ranges = std::mem::take(&mut self.plan_ranges);
        plans.clear();
        ranges.clear();
        let joints = self.joints.len();
        for color in 0..self.colorer.layer_count() {
            let start = plans.len();
            let (mut pair, mut ground) = (None::<usize>, None::<usize>);
            for &index in self.colorer.layer(color) {
                let is_ground =
                    index as usize >= joints && contacts[index as usize - joints][1] == GROUND;
                let open = if is_ground { &mut ground } else { &mut pair };
                let p = match *open {
                    Some(p) if usize::from(plans[p].count) < BATCH_WIDTH => p,
                    _ => {
                        plans.push(BatchPlan::EMPTY);
                        let p = plans.len() - 1;
                        plans[p].ground = is_ground;
                        *open = Some(p);
                        p
                    }
                };
                let (a, b, target, alpha, inequality, slot) =
                    self.constraint(index as usize, contacts);
                let plan = &mut plans[p];
                let l = usize::from(plan.count);
                plan.count += 1;
                plan.a[l] = a as u32;
                plan.slot[l] = slot.map_or(NO_SLOT, |s| s as u32);
                plan.lanes.wa[l] = self.bodies.inv_mass(a);
                plan.lanes.alpha[l] = alpha;
                match b {
                    Some(b) => {
                        plan.b[l] = b as u32;
                        plan.lanes.wb[l] = self.bodies.inv_mass(b);
                        plan.lanes.target[l] = target;
                        plan.lanes.inequality |= u16::from(inequality) << l;
                    }
                    // Ground plans keep the radius in `target`.
                    None => plan.lanes.target[l] = self.radius(a),
                }
            }
            ranges.push((start as u32, plans.len() as u32));
        }
        self.plans = plans;
        self.plan_ranges = ranges;
    }

    /// Project one colour plan by plan on this thread.
    fn project_color_batched(&mut self, color: usize, inv_h2: f32, state: &mut [[f32; 4]]) {
        let (lo, hi) = self.plan_ranges[color];
        let mut u = TileUpdate::EMPTY;
        for p in lo as usize..hi as usize {
            self.solve_plan(&self.bodies, &self.plans[p], inv_h2, &mut u);
            self.apply_update(&u, state);
        }
    }

    /// Every colour of one substep with the plans of each large colour split
    /// across `threads` threads. The threads live for the substep; per
    /// colour they solve their contiguous range of plans against the
    /// colour's starting state (bodies behind a read lock), and the calling
    /// thread then applies all updates in plan order under the write lock.
    /// The plans of a colour touch disjoint bodies, so this is bit-identical
    /// to the serial solve.
    fn project_colors_parallel(&mut self, inv_h2: f32, state: &mut [[f32; 4]]) {
        use std::sync::{Mutex, RwLock};
        let threads = self.threads;
        let mut outs = std::mem::take(&mut self.par_out);
        let bodies = RwLock::new(std::mem::take(&mut self.bodies));
        {
            let this = &*self;
            let colors = this.plan_ranges.len();
            // The range of plans thread `t` solves in `color`, or nothing if
            // the colour is small (then the calling thread solves it alone).
            let range = |color: usize, t: usize| -> (usize, usize) {
                let (lo, hi) = this.plan_ranges[color];
                let (lo, hi) = (lo as usize, hi as usize);
                let constraints: usize = this.plans[lo..hi]
                    .iter()
                    .map(|p| usize::from(p.count))
                    .sum();
                if constraints < this.parallel_min {
                    return if t == 0 { (lo, hi) } else { (hi, hi) };
                }
                let per = (hi - lo).div_ceil(threads);
                ((lo + t * per).min(hi), (lo + (t + 1) * per).min(hi))
            };
            let slots: Vec<Mutex<&mut Vec<TileUpdate>>> =
                outs.iter_mut().take(threads).map(Mutex::new).collect();
            let barrier = SpinBarrier::new(threads);
            std::thread::scope(|s| {
                for (t, slot) in slots.iter().enumerate().skip(1) {
                    let (bodies, barrier) = (&bodies, &barrier);
                    s.spawn(move || {
                        let mut u = TileUpdate::EMPTY;
                        for color in 0..colors {
                            barrier.wait();
                            {
                                let b = bodies.read().expect("solver lock");
                                let mut out = slot.lock().expect("solver output");
                                out.clear();
                                let (lo, hi) = range(color, t);
                                for p in lo..hi {
                                    this.solve_plan(&b, &this.plans[p], inv_h2, &mut u);
                                    out.push(u);
                                }
                            }
                            barrier.wait();
                        }
                    });
                }
                let mut u = TileUpdate::EMPTY;
                for color in 0..colors {
                    barrier.wait();
                    let (lo, hi) = range(color, 0);
                    let mut mine = slots[0].lock().expect("solver output");
                    mine.clear();
                    {
                        let b = bodies.read().expect("solver lock");
                        for p in lo..hi {
                            this.solve_plan(&b, &this.plans[p], inv_h2, &mut u);
                            mine.push(u);
                        }
                    }
                    drop(mine);
                    barrier.wait();
                    let mut b = bodies.write().expect("solver lock");
                    for slot in &slots {
                        for u in slot.lock().expect("solver output").iter() {
                            apply_update(&mut b, u, state);
                        }
                    }
                }
            });
        }
        self.bodies = bodies.into_inner().expect("solver lock");
        self.par_out = outs;
    }

    /// Write a tile update back to the bodies and the contact state.
    #[inline]
    fn apply_update(&mut self, u: &TileUpdate, state: &mut [[f32; 4]]) {
        apply_update(&mut self.bodies, u, state);
    }

    /// Solve one plan against `bodies`: gather positions, run the SIMD
    /// kernel, apply friction per active lane and collect the writes.
    fn solve_plan(&self, bodies: &TiledBodies, plan: &BatchPlan, inv_h2: f32, u: &mut TileUpdate) {
        u.writes = 0;
        u.states = 0;
        let m = usize::from(plan.count);
        let friction = self.friction_enabled();
        if plan.ground {
            let ground = self.config.ground_y.unwrap_or(0.0);
            let mut lanes = GroundLanes {
                radius: plan.lanes.target,
                wa: plan.lanes.wa,
                alpha: plan.lanes.alpha,
                ..GroundLanes::default()
            };
            let mut px = [0.0f32; BATCH_WIDTH];
            let mut pz = [0.0f32; BATCH_WIDTH];
            for l in 0..m {
                let q = bodies.pos(plan.a[l] as usize);
                px[l] = q[0];
                lanes.py[l] = q[1];
                pz[l] = q[2];
            }
            let solved = simd::solve_ground(self.simd, &lanes, ground, inv_h2);
            for l in 0..m {
                if solved.active >> l & 1 == 0 {
                    continue;
                }
                let a = plan.a[l] as usize;
                let mut na = [px[l], solved.y[l], pz[l]];
                if friction {
                    self.apply_friction_ground(&mut na, self.prev[a], solved.corr[l]);
                }
                u.push_state(plan.slot[l] as usize, [solved.corr[l], 0.0, 1.0, 0.0]);
                u.push_write(a, na);
            }
            return;
        }
        let mut lanes = plan.lanes;
        for l in 0..m {
            let p = bodies.pos(plan.a[l] as usize);
            let q = bodies.pos(plan.b[l] as usize);
            for k in 0..3 {
                lanes.pa[k][l] = p[k];
                lanes.pb[k][l] = q[k];
            }
        }
        let solved = simd::solve_pair(self.simd, &lanes, inv_h2);
        for l in 0..m {
            if solved.active >> l & 1 == 0 {
                continue;
            }
            let (a, b) = (plan.a[l] as usize, plan.b[l] as usize);
            let mut pa_l = [solved.na[0][l], solved.na[1][l], solved.na[2][l]];
            let mut pb_l = [solved.nb[0][l], solved.nb[1][l], solved.nb[2][l]];
            if plan.slot[l] != NO_SLOT {
                let n_l = [
                    solved.normal[0][l],
                    solved.normal[1][l],
                    solved.normal[2][l],
                ];
                let corr = det::add(solved.sa[l], solved.sb[l]);
                if friction {
                    self.apply_friction_pair(
                        &mut pa_l,
                        &mut pb_l,
                        self.prev[a],
                        self.prev[b],
                        n_l,
                        corr,
                        lanes.wa[l],
                        lanes.wb[l],
                        solved.w[l],
                    );
                }
                u.push_state(plan.slot[l] as usize, [corr, n_l[0], n_l[1], n_l[2]]);
            }
            u.push_write(a, pa_l);
            u.push_write(b, pb_l);
        }
    }

    /// Velocity pass (Müller et al. 2020 §3.6): for every contact that
    /// pushed this substep, set the normal relative velocity to
    /// `−e · v̄n` (the restitution of the pre-substep approach speed), with
    /// `e = 0` when `|v̄n|` is below `2·|g|₁·h`. Contacts are visited in colour order.
    fn apply_restitution(&mut self, h: f32, contacts: &[[u32; 2]], state: &[[f32; 4]]) {
        let joints = self.joints.len();
        let g = self.config.gravity;
        let g1 = det::add(det::add(g[0].abs(), g[1].abs()), g[2].abs());
        let threshold = det::mul(det::mul(2.0, g1), h);
        let e = self.material.restitution;
        for color in 0..self.colorer.layer_count() {
            let layer_len = self.colorer.layer(color).len();
            for k in 0..layer_len {
                let index = self.colorer.layer(color)[k] as usize;
                if index < joints {
                    continue;
                }
                let slot = index - joints;
                let [corr, n0, n1, n2] = state[slot];
                if !(corr > 0.0) {
                    continue;
                }
                let n = [n0, n1, n2];
                let [a, b] = contacts[slot];
                let a = a as usize;
                let (vb, vb_prev, wb) = if b == GROUND {
                    ([0.0; 3], [0.0; 3], 0.0)
                } else {
                    let b = b as usize;
                    (
                        self.bodies.vel(b),
                        self.prev_vel[b],
                        self.bodies.inv_mass(b),
                    )
                };
                let va = self.bodies.vel(a);
                let wa = self.bodies.inv_mass(a);
                let w = det::add(wa, wb);
                if w == 0.0 {
                    continue;
                }
                let rel = [
                    det::sub(va[0], vb[0]),
                    det::sub(va[1], vb[1]),
                    det::sub(va[2], vb[2]),
                ];
                let va_prev = self.prev_vel[a];
                let rel_prev = [
                    det::sub(va_prev[0], vb_prev[0]),
                    det::sub(va_prev[1], vb_prev[1]),
                    det::sub(va_prev[2], vb_prev[2]),
                ];
                let vn = det::dot(rel, n);
                let vn_prev = det::dot(rel_prev, n);
                // The low-speed cutoff looks at the approach speed, like `wanted`.
                let e_here = if vn_prev.abs() <= threshold { 0.0 } else { e };
                let wanted = if vn_prev < 0.0 {
                    det::neg(det::mul(e_here, vn_prev))
                } else {
                    0.0
                };
                let dv = det::sub(wanted, vn);
                let fa = det::div(wa, w);
                self.bodies.set_vel(
                    a,
                    [
                        det::add(va[0], det::mul(det::mul(dv, fa), n[0])),
                        det::add(va[1], det::mul(det::mul(dv, fa), n[1])),
                        det::add(va[2], det::mul(det::mul(dv, fa), n[2])),
                    ],
                );
                if b != GROUND {
                    let b = b as usize;
                    let fb = det::div(wb, w);
                    self.bodies.set_vel(
                        b,
                        [
                            det::sub(vb[0], det::mul(det::mul(dv, fb), n[0])),
                            det::sub(vb[1], det::mul(det::mul(dv, fb), n[1])),
                            det::sub(vb[2], det::mul(det::mul(dv, fb), n[2])),
                        ],
                    );
                }
            }
        }
    }

    /// Total kinetic energy `Σ ½ m |v|²` over dynamic bodies (diagnostics).
    pub fn kinetic_energy(&self) -> f64 {
        (0..self.bodies.len())
            .filter(|&i| self.bodies.inv_mass(i) > 0.0)
            .map(|i| {
                let v = self.bodies.vel(i).map(f64::from);
                0.5 * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) / f64::from(self.bodies.inv_mass(i))
            })
            .sum()
    }

    /// Total gravitational potential energy relative to `y = 0`
    /// (diagnostics, assumes gravity along -y).
    pub fn potential_energy(&self) -> f64 {
        let g = -f64::from(self.config.gravity[1]);
        (0..self.bodies.len())
            .filter(|&i| self.bodies.inv_mass(i) > 0.0)
            .map(|i| g * f64::from(self.bodies.pos(i)[1]) / f64::from(self.bodies.inv_mass(i)))
            .sum()
    }

    /// Largest violation of any distance joint, `|len - rest|`.
    pub fn max_joint_error(&self) -> f32 {
        let mut worst = 0.0f32;
        for j in &self.joints {
            let pa = self.bodies.pos(j.a as usize);
            let pb = self.bodies.pos(j.b as usize);
            let d = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
            let len = det::sqrt(det::dot(d, d));
            worst = det::max((len - j.rest_length).abs(), worst);
        }
        worst
    }

    /// Deepest penetration of any pair or ground contact (0 if none).
    pub fn max_penetration(&self) -> f32 {
        let n = self.bodies.len();
        let mut worst = 0.0f32;
        for i in 0..n {
            let pi = self.bodies.pos(i);
            let ri = self.radius(i);
            if let Some(ground) = self.config.ground_y
                && self.bodies.inv_mass(i) > 0.0
            {
                worst = det::max(ground - (pi[1] - ri), worst);
            }
            for j in i + 1..n {
                if self.bodies.layer_mask(i) & self.bodies.layer_mask(j) == 0 {
                    continue;
                }
                let pj = self.bodies.pos(j);
                let d = [pi[0] - pj[0], pi[1] - pj[1], pi[2] - pj[2]];
                let len = det::sqrt(det::dot(d, d));
                worst = det::max(ri + self.radius(j) - len, worst);
            }
        }
        worst
    }
}

const EPS: f32 = 1.0e-9;

/// Position-level Coulomb friction (Macklin et al. 2014, eq. 24): the
/// correction to add to the relative displacement `dp` of a contact with
/// normal `n` whose normal correction this substep was `corr`.
#[inline]
fn friction_correction(
    dp: [f32; 3],
    n: [f32; 3],
    corr: f32,
    material: ContactMaterial,
) -> Option<[f32; 3]> {
    let dn = det::dot(dp, n);
    let t = [
        det::sub(dp[0], det::mul(dn, n[0])),
        det::sub(dp[1], det::mul(dn, n[1])),
        det::sub(dp[2], det::mul(dn, n[2])),
    ];
    let lt = det::sqrt(det::dot(t, t));
    if !(lt > EPS) {
        return None;
    }
    let k = if lt < det::mul(material.static_friction, corr) {
        1.0
    } else {
        let r = det::div(det::mul(material.dynamic_friction, corr), lt);
        if r < 1.0 { r } else { 1.0 }
    };
    if k == 0.0 {
        return None;
    }
    Some([
        det::neg(det::mul(k, t[0])),
        det::neg(det::mul(k, t[1])),
        det::neg(det::mul(k, t[2])),
    ])
}
