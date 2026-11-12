//! Vesper Ignis render-graph core (design §19.4 and §19.7).
//!
//! A GPU-independent, typed resource/pass graph and a hazard planner that
//! produces deterministic D3D12 enhanced-barrier batches and cross-queue
//! fence signal/wait plans.
//!
//! * Resources are typed: [`BufferHandle`]s are tracked per byte range and have
//!   no layout; [`TextureHandle`]s are tracked per subresource
//!   (mip, array slice, plane), each with its own [`Layout`].
//! * Passes run on a [`QueueId`] of a given [`QueueKind`] and declare
//!   read/write accesses with [`Access`] and [`Sync`] scopes. Declaration order
//!   defines the meaning of the graph; [`RenderGraph::add_dependency`] adds
//!   further ordering. Overlap is computed per byte range / subresource, so
//!   disjoint ranges never synchronize.
//! * [`RenderGraph::plan`] returns a [`Plan`]: a global order, the dependency
//!   edges, and per-queue command streams of `Wait`, `Barriers`, `Execute`, and
//!   `Signal`, or a structured [`PlanError`].
//!
//! [`lifecycle`] adds consuming recording/closed command batches, original-fence
//! retirement and owned bounded shutdown above immutable planner output.
//! [`recording`] is a deterministic CPU backend for that protocol; it does not
//! execute shaders. Scratch initialization produces Ready only after retirement.
//!
//! This crate does not call D3D12. Its rules are modeled on the published
//! Enhanced Barriers specification and have not been validated by the D3D12
//! debug layer or on hardware. It does not prove barrier soundness; the tests
//! exercise the modeled rules only.
//!
//! ```
//! use vesper_ignis::*;
//!
//! let mut g = RenderGraph::new();
//! let direct = g.add_queue(QueueKind::Direct);
//! let color = g.add_texture(TextureDesc::simple("color", Layout::Undefined)).unwrap();
//! g.add_pass(PassDesc::new("draw", direct).texture(
//!     color, SubresourceRange::ALL, Access::RENDER_TARGET, Sync::RENDER_TARGET, Layout::RenderTarget,
//! )).unwrap();
//! g.add_pass(PassDesc::new("post", direct).texture(
//!     color, SubresourceRange::ALL, Access::SHADER_RESOURCE, Sync::PIXEL_SHADING, None,
//! )).unwrap();
//! let plan = g.plan().unwrap();
//! assert_eq!(plan.barrier_count(), 2); // UNDEFINED->RENDER_TARGET, RENDER_TARGET->SHADER_RESOURCE
//! ```

mod d3d12;
mod graph;
pub mod lifecycle;
mod plan;
pub mod recording;

pub use d3d12::{Access, Layout, QueueKind, Sync};
pub use graph::{
    BufferDesc, BufferHandle, ByteRange, GraphError, PassDesc, PassId, QueueId, QueueRestriction,
    RenderGraph, ResourceRef, SubresourceRange, TextureDesc, TextureHandle,
};
pub use plan::{
    Barrier, BufferBarrier, Command, Dependency, DependencyKind, Plan, PlanError, PlanOptions,
    QueueStream, TextureBarrier,
};

/// Version of the plan data contract consumed by a future D3D12 executor.
pub const PLAN_CONTRACT_VERSION: u32 = 0;
