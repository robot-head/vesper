//! Vesper Kinetic (design §36): bit-deterministic physics host library.
//!
//! * [`detfloat`]: the §36.5 DetFloat auditor over real Cranelift IR.
//! * [`morton`]: branchless 30-bit 3D Morton codes (optional BMI2 path).
//! * [`coloring`]: deterministic greedy constraint-graph colouring.
//! * [`tile`] and [`world`]: AoSoA rigid-body tiles and a fixed-timestep XPBD
//!   solver that projects colours in fixed order with no fused multiply-add
//!   and no allocation per step after construction.
//! * [`rollback`]: snapshot ring with linear [`rollback::RollbackTicket`]s.
//! * [`snapshot`]: the 48-byte §36.2 header and canonical world hashes.
//!
//! * [`crmath`]: correctly rounded `sin`, `cos`, `atan2`, `exp`, `log`.
//! * [`lbvh`]: 64-bit Morton LBVH broadphase (Karras construction).
//! * [`gjk`]: GJK distance and EPA penetration for convex shapes.
//! * [`diff`]: reverse-mode gradients of an XPBD simulation as Tensor host
//!   tensors.
//! * [`rigid`]: GJK/EPA rigid bodies with manifolds, XPBD joints (ball,
//!   hinge, prismatic), a gyroscopic term, sleeping islands and CCD.
//! * [`soft`]: XPBD soft bodies (distance and tetrahedral volume
//!   constraints) with a thread-count-independent colour-parallel solve.
//! * [`rigid_diff`]: forward-mode contact gradients for boxes, capsules and
//!   convex hulls.
//!
//! Not here: the GPU broadphase (Ignis; no GPU in CI) and the compiler's
//! `eff [DetFloat]` source effect. Ownership-based linearity in this crate is
//! host-library discipline, not an implementation of Vesper's source typing
//! rules.

// `unsafe` is denied everywhere except the `std::arch` kernels in [`simd`].
#![deny(unsafe_code)]

pub mod coloring;
pub mod crmath;
pub mod detfloat;
pub mod diff;
pub mod fastmath;
pub mod gjk;
pub mod lbvh;
pub mod morton;
pub mod rigid;
pub mod rigid_diff;
pub mod rollback;
pub mod sap;
pub mod simd;
pub mod snapshot;
pub mod soft;
pub mod tile;
pub mod world;

pub use coloring::{BodyId, Colorer, ColoringError, ConstraintBodies, MultiColorer};
pub use detfloat::{AuditError, DetFloatPolicy, ReductionOrder, VerifierFinding, audit};
pub use morton::MortonError;
pub use rigid::{
    CcdConfig, ColliderShape, ContactManifold, GroundPlane, JointKind, RigidBody, RigidBroadphase,
    RigidCollider, RigidConfig, RigidError, RigidJoint, RigidMaterial, RigidWorld, SleepConfig,
};
pub use rollback::{ResimulateError, RollbackError, RollbackTicket, SnapshotRing};
pub use sap::{SapError, SweepAndPrune};
pub use simd::{SimdBackend, SimdUnavailable};
pub use snapshot::{KineticSnapshotWireHdr, SNAPSHOT_HEADER_LEN, WorldHashes, hash_world};
pub use soft::{SoftConfig, SoftConstraint, SoftError, SoftParticle, SoftWorld};
pub use tile::{Body, RigidBodyTile, TILE_WIDTH, TiledBodies};
pub use world::{
    BATCH_WIDTH, BroadphaseMode, ContactMaterial, DistanceConstraint, MAX_SOLVER_THREADS,
    PARALLEL_MIN_CONSTRAINTS, SolverMode, SphereCollider, StepError, StepTimings, World,
    WorldConfig,
};
