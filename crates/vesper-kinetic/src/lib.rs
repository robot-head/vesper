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
//! Not here: the GPU broadphase (Ignis), Tensor gradients and the compiler's
//! `eff [DetFloat]` source effect. Ownership-based linearity in this crate is
//! host-library discipline, not an implementation of Vesper's source typing
//! rules.

#![deny(unsafe_code)]

pub mod coloring;
pub mod detfloat;
pub mod morton;
pub mod rollback;
pub mod snapshot;
pub mod tile;
pub mod world;

pub use coloring::{BodyId, Colorer, ColoringError, ConstraintBodies};
pub use detfloat::{AuditError, DetFloatPolicy, ReductionOrder, VerifierFinding, audit};
pub use morton::MortonError;
pub use rollback::{ResimulateError, RollbackError, RollbackTicket, SnapshotRing};
pub use snapshot::{KineticSnapshotWireHdr, SNAPSHOT_HEADER_LEN, WorldHashes, hash_world};
pub use tile::{Body, RigidBodyTile, TILE_WIDTH, TiledBodies};
pub use world::{DistanceConstraint, SphereCollider, StepError, World, WorldConfig};
