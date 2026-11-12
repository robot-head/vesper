//! Vesper Nexus (design §30): multi-tenant WebAssembly component plugins
//! with generational WIT resource tables, deterministic fuel quanta and
//! zero-copy shared plugin rings.
//!
//! * [`table`]: the §30.5 O(1) generational WIT resource table with tenant
//!   tags and the Component Model `num_lends == 0` drop rule.
//! * [`host`]: a Wasmtime 50 component host whose resource `blob` is backed
//!   by that table, plus fuel/epoch preemption under a [`fuel::QuantumScheduler`].
//! * [`ring`]: `SharedPluginRing`, padding-free wire records in a Wasm memory
//!   that guests read and transform in place, and the §30.3 two-tenant
//!   broker stream filter.
//! * [`wasmfx`]: the WasmFX (stack-switching) probe and one lowered effect
//!   handler, available only where Wasmtime supports the proposal.
//!
//! Linear obligations here are host-library discipline (non-Clone,
//! `#[must_use]` owners with counted leaks), not an implementation of the
//! Vesper source typing rules for `Mode::Linear`.
#![forbid(unsafe_code)]

pub mod fuel;
pub mod guests;
pub mod host;
pub mod ring;
pub mod table;
pub mod wasmfx;

pub use fuel::{FuelAccountant, FuelError, QuantumScheduler, RoundRobin, SliceEvent, TenantUsage};
pub use host::{
    Blob, CompiledComponent, DestroyReport, HOST_INTERFACE, NexusConfig, NexusEngine, PLUGIN_WIT,
    PluginError, PluginInstance, ScheduledCall, ScheduledOutcome,
};
pub use table::{TenantId, WitBorrow, WitHandle, WitOwn, WitResourceTable, WitTableError};
