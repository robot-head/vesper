//! One ring per tenant for mutually distrusting tenants (§30.1 item 3).
//!
//! [`crate::ring::StreamFilterBroker`] runs every tenant's filter over one
//! shared memory: slot leases stop the *host* from handing tenant A's slot
//! to tenant B, but Wasm lets any module that imports a memory load and
//! store anywhere in it, so a filter can read or overwrite other tenants'
//! slots. [`TenantRings`] removes that by construction: each tenant gets its
//! own `Store`, its own fixed-size ring memory and its own filter instance,
//! and a filter module is linked against its tenant's memory only. An
//! address in a tenant's filter can only ever name a byte of that tenant's
//! ring; past its end the guest traps. Nothing is shared between tenants
//! except the compiled-code `Engine`.
//!
//! On the host side every operation names the acting tenant, and a lease is
//! accepted only by the ring that minted it, for the tenant it was minted
//! for: presenting another tenant's lease is [`TenantRingError::CrossTenantLease`],
//! and a lease from another `TenantRings` is a `ForeignLease` ring error.
//!
//! The isolation boundary is Wasm memory safety in Wasmtime (bounds-checked
//! or guard-page-checked linear memory), not hardware Domain-1/MPK
//! protection, which is not exercised here.

use crate::ring::{
    FilterError, PluginSliceGrant, RingError, SharedPluginRing, SlotLease, TenantUdfMetrics,
    run_filter,
};
use crate::table::TenantId;
use std::fmt;
use wasmtime::{Config, Engine, Instance, Module, Store, Trap, TypedFunc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TenantRingError {
    /// The tenant already has a ring.
    DuplicateTenant(TenantId),
    UnknownTenant(TenantId),
    /// `acting` presented a lease minted for `owner`.
    CrossTenantLease {
        acting: TenantId,
        owner: TenantId,
    },
    Filter(FilterError),
}
impl fmt::Display for TenantRingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tenant rings: {self:?}")
    }
}
impl std::error::Error for TenantRingError {}
impl From<FilterError> for TenantRingError {
    fn from(e: FilterError) -> Self {
        Self::Filter(e)
    }
}
impl From<RingError> for TenantRingError {
    fn from(e: RingError) -> Self {
        Self::Filter(FilterError::Ring(e))
    }
}

struct TenantRing {
    tenant: TenantId,
    store: Store<()>,
    ring: SharedPluginRing,
    filter: TypedFunc<i32, i32>,
    fuel_per_call: u64,
    metrics: TenantUdfMetrics,
}

/// A broker whose tenants each own a separate ring memory and store.
pub struct TenantRings {
    engine: Engine,
    tenants: Vec<TenantRing>,
}

impl TenantRings {
    pub fn new() -> Result<Self, TenantRingError> {
        let mut c = Config::new();
        c.consume_fuel(true);
        let engine = Engine::new(&c)
            .map_err(|e| TenantRingError::Filter(FilterError::InitFailed(format!("{e:?}"))))?;
        Ok(Self {
            engine,
            tenants: Vec::new(),
        })
    }

    /// Give `tenant` its own ring of `slots` × `slot_bytes` and instantiate
    /// its filter against that ring only. The module imports the ring as
    /// `(import "nexus" "ring" (memory N))` and exports
    /// `filter: (grant_offset: i32) -> i32`, as for the shared broker.
    pub fn add_tenant(
        &mut self,
        tenant: TenantId,
        slots: u32,
        slot_bytes: u32,
        module_text_or_binary: &[u8],
        fuel_per_call: u64,
    ) -> Result<(), TenantRingError> {
        if self.index(tenant).is_ok() {
            return Err(TenantRingError::DuplicateTenant(tenant));
        }
        let init = |e: wasmtime::Error| FilterError::InitFailed(format!("{e:?}"));
        let module = Module::new(&self.engine, module_text_or_binary).map_err(init)?;
        let mut store = Store::new(&self.engine, ());
        let ring = SharedPluginRing::new(&mut store, slots, slot_bytes)?;
        store.set_fuel(fuel_per_call).map_err(init)?;
        let instance =
            Instance::new(&mut store, &module, &[ring.memory().into()]).map_err(|e| match e
                .downcast_ref::<Trap>()
            {
                Some(Trap::OutOfFuel) => FilterError::InitOutOfFuel,
                _ => init(e),
            })?;
        let filter = instance
            .get_typed_func::<i32, i32>(&mut store, "filter")
            .map_err(init)?;
        self.tenants.push(TenantRing {
            tenant,
            store,
            ring,
            filter,
            fuel_per_call,
            metrics: TenantUdfMetrics::default(),
        });
        Ok(())
    }

    fn index(&self, tenant: TenantId) -> Result<usize, TenantRingError> {
        self.tenants
            .iter()
            .position(|t| t.tenant == tenant)
            .ok_or(TenantRingError::UnknownTenant(tenant))
    }

    /// The acting tenant's ring, after checking the lease was minted for it.
    fn owned(&self, acting: TenantId, lease: &SlotLease) -> Result<usize, TenantRingError> {
        let idx = self.index(acting)?;
        if lease.tenant() != acting {
            return Err(TenantRingError::CrossTenantLease {
                acting,
                owner: lease.tenant(),
            });
        }
        Ok(idx)
    }

    pub fn tenants(&self) -> impl Iterator<Item = TenantId> + '_ {
        self.tenants.iter().map(|t| t.tenant)
    }
    pub fn metrics(&self, tenant: TenantId) -> Option<TenantUdfMetrics> {
        self.index(tenant).ok().map(|i| self.tenants[i].metrics)
    }
    pub fn free_slots(&self, tenant: TenantId) -> Option<usize> {
        self.index(tenant)
            .ok()
            .map(|i| self.tenants[i].ring.free_slots())
    }
    /// Leases dropped without release, over every tenant's ring.
    pub fn leaked_leases(&self) -> u64 {
        self.tenants.iter().map(|t| t.ring.leaked_leases()).sum()
    }
    /// Base address and byte length of a tenant's ring memory, for
    /// zero-copy and disjointness assertions.
    pub fn ring_span(&self, tenant: TenantId) -> Option<(*const u8, usize)> {
        let t = &self.tenants[self.index(tenant).ok()?];
        let mem = t.ring.memory();
        Some((mem.data_ptr(&t.store), mem.data_size(&t.store)))
    }
    /// Byte offset of the lease's grant descriptor in its tenant's ring.
    pub fn grant_offset(
        &self,
        tenant: TenantId,
        lease: &SlotLease,
    ) -> Result<u32, TenantRingError> {
        let i = self.owned(tenant, lease)?;
        Ok(self.tenants[i].ring.grant_offset(lease))
    }

    /// Lease a slot of `tenant`'s own ring.
    pub fn lease(&mut self, tenant: TenantId) -> Result<SlotLease, TenantRingError> {
        let i = self.index(tenant)?;
        Ok(self.tenants[i].ring.lease(tenant)?)
    }

    /// Return `lease` to `tenant`'s ring; any mismatch hands it back.
    pub fn release(
        &mut self,
        tenant: TenantId,
        lease: SlotLease,
    ) -> Result<(), (SlotLease, TenantRingError)> {
        let i = match self.owned(tenant, &lease) {
            Ok(i) => i,
            Err(e) => return Err((lease, e)),
        };
        self.tenants[i]
            .ring
            .release(lease)
            .map_err(|(l, e)| (l, e.into()))
    }

    /// Producer access to the slot's payload area.
    pub fn payload_mut(
        &mut self,
        tenant: TenantId,
        lease: &SlotLease,
    ) -> Result<&mut [u8], TenantRingError> {
        let i = self.owned(tenant, lease)?;
        let t = &mut self.tenants[i];
        Ok(t.ring.payload_mut(&mut t.store, lease)?)
    }

    /// The first `len` payload bytes, read in place.
    pub fn payload(
        &self,
        tenant: TenantId,
        lease: &SlotLease,
        len: u32,
    ) -> Result<&[u8], TenantRingError> {
        let i = self.owned(tenant, lease)?;
        let t = &self.tenants[i];
        Ok(t.ring.payload(&t.store, lease, len)?)
    }

    pub fn read_grant(
        &self,
        tenant: TenantId,
        lease: &SlotLease,
    ) -> Result<PluginSliceGrant, TenantRingError> {
        let i = self.owned(tenant, lease)?;
        let t = &self.tenants[i];
        Ok(t.ring.read_grant(&t.store, lease)?)
    }

    /// `apply_tenant_wasm_udf` (§30.3) on `tenant`'s own ring: publish the
    /// grant, run the tenant's filter in place under its fuel budget and
    /// return the bytes it wrote. Metrics are updated on the happy and the
    /// trap path alike.
    pub fn apply(
        &mut self,
        tenant: TenantId,
        lease: &SlotLease,
        len: u32,
        schema_id: u32,
        record_timestamp_ns: u64,
    ) -> Result<u32, TenantRingError> {
        let i = self.owned(tenant, lease)?;
        let t = &mut self.tenants[i];
        Ok(run_filter(
            &mut t.store,
            &t.ring,
            &t.filter,
            t.fuel_per_call,
            &mut t.metrics,
            lease,
            len,
            schema_id,
            record_timestamp_ns,
        )?)
    }
}
