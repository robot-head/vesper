//! `SharedPluginRing` (§30.1 item 3, §30.2) and the §30.3 multi-tenant
//! broker stream filter.
//!
//! The ring is one fixed-size Wasm linear memory created by the host and
//! imported by every tenant's filter module. Each slot starts with a
//! padding-free [`PluginSliceGrantWire`] descriptor (24 bytes, written
//! through a `vesper-ffi` wire view) followed by the payload area. The
//! producer fills the payload in place, the guest reads and rewrites it in
//! place with ordinary loads and stores, and the consumer reads the result in
//! place: no canonical-ABI lowering, no `cabi_realloc`, no copy between host
//! and guest memories.
//!
//! Boundaries, recorded in `docs/parallel-results/nexus.md`:
//! * The Component Model cannot import a memory, so ring plugins are core
//!   modules (with their own exports) rather than components. Shared-
//!   everything threads/multi-memory components are not available in
//!   Wasmtime 50's component model.
//! * Every module importing the ring can address all of it. Slot leases stop
//!   the *host* from granting one tenant's slot to another, but Wasm does not
//!   stop a filter from touching other slots. Mutually distrusting tenants
//!   need one ring each; Domain-1 hardware protection (MPK) is not exercised.

use crate::table::{LeakAuditor, Obligation, TenantId};
use std::fmt;
use vesper_ffi::{
    Declaration, Endian, Field, Integer, Registry, ViewError, WireLayout, WireType, split_wire_mut,
    view_wire,
};
use wasmtime::{
    AsContext, AsContextMut, Config, Engine, Instance, Memory, MemoryType, Module, Store, Trap,
    TypedFunc,
};

/// Wasm page size.
pub const PAGE: u32 = 65_536;
/// Byte size of [`PluginSliceGrantWire`].
pub const GRANT_BYTES: u32 = 24;

/// The §30.2 `PluginSliceGrantWire` layout: five little-endian fields, 24
/// bytes, no padding.
pub fn grant_layout() -> WireLayout {
    let u32le = WireType::Integer(Integer::unsigned(32, Endian::Little));
    let u64le = WireType::Integer(Integer::unsigned(64, Endian::Little));
    let decl = Declaration::new(
        "PluginSliceGrantWire",
        vec![
            Field::new("ring_offset", u32le.clone()),
            Field::new("byte_len", u32le.clone()),
            Field::new("schema_id", u32le.clone()),
            Field::new("tenant_flags", u32le),
            Field::new("record_timestamp_ns", u64le),
        ],
    );
    Registry::new([decl])
        .and_then(|r| r.compile("PluginSliceGrantWire"))
        .expect("PluginSliceGrantWire is a valid padding-free layout")
}

/// A decoded grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginSliceGrant {
    pub ring_offset: u32,
    pub byte_len: u32,
    pub schema_id: u32,
    pub tenant_flags: u32,
    pub record_timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingError {
    Config(String),
    /// Every slot is leased.
    Exhausted,
    /// Payload length exceeds the slot's payload area.
    PayloadTooLarge {
        len: u32,
        capacity: u32,
    },
    /// The lease does not belong to this ring or is not live.
    ForeignLease,
    Wire(ViewError),
    /// Slot geometry overflows the 32-bit Wasm address space.
    Geometry,
}
impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "shared plugin ring: {self:?}")
    }
}
impl std::error::Error for RingError {}
impl From<ViewError> for RingError {
    fn from(e: ViewError) -> Self {
        Self::Wire(e)
    }
}

/// Linear lease of one ring slot. Exits only through
/// [`SharedPluginRing::release`]; a dropped lease is counted as a leak and
/// its slot stays unavailable.
#[must_use = "a ring slot lease must be released exactly once"]
#[derive(Debug)]
pub struct SlotLease {
    slot: u32,
    tenant: TenantId,
    obligation: Obligation,
}
impl SlotLease {
    pub fn slot(&self) -> u32 {
        self.slot
    }
    pub fn tenant(&self) -> TenantId {
        self.tenant
    }
}

/// Fixed-geometry ring of slots in one host-created Wasm memory.
pub struct SharedPluginRing {
    memory: Memory,
    slot_bytes: u32,
    slots: u32,
    free: Vec<u32>,
    leased: Vec<bool>,
    layout: WireLayout,
    auditor: LeakAuditor,
}

impl SharedPluginRing {
    /// Create the ring memory in `store`. The memory has a fixed size (no
    /// growth), so its base address is stable for the store's lifetime.
    pub fn new<T>(store: &mut Store<T>, slots: u32, slot_bytes: u32) -> Result<Self, RingError> {
        if slots == 0 || slot_bytes <= GRANT_BYTES || !slot_bytes.is_multiple_of(8) {
            return Err(RingError::Geometry);
        }
        let total = slots.checked_mul(slot_bytes).ok_or(RingError::Geometry)?;
        let pages = total.div_ceil(PAGE).max(1);
        let memory = Memory::new(&mut *store, MemoryType::new(pages, Some(pages)))
            .map_err(|e| RingError::Config(format!("{e:?}")))?;
        Ok(Self {
            memory,
            slot_bytes,
            slots,
            free: (0..slots).rev().collect(),
            leased: vec![false; slots as usize],
            layout: grant_layout(),
            auditor: LeakAuditor::default(),
        })
    }

    pub fn memory(&self) -> Memory {
        self.memory
    }
    pub fn slots(&self) -> u32 {
        self.slots
    }
    pub fn payload_capacity(&self) -> u32 {
        self.slot_bytes - GRANT_BYTES
    }
    pub fn leaked_leases(&self) -> u64 {
        self.auditor.leaked()
    }
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    /// Byte offset of a slot's grant descriptor in ring memory.
    pub fn grant_offset(&self, lease: &SlotLease) -> u32 {
        lease.slot * self.slot_bytes
    }
    fn payload_offset(&self, slot: u32) -> u32 {
        slot * self.slot_bytes + GRANT_BYTES
    }

    fn check(&self, lease: &SlotLease) -> Result<(), RingError> {
        if lease.slot < self.slots
            && self.leased[lease.slot as usize]
            && lease.obligation.same_auditor(&self.auditor)
        {
            Ok(())
        } else {
            Err(RingError::ForeignLease)
        }
    }

    /// Lease a free slot for `tenant`. O(1), no allocation.
    pub fn lease(&mut self, tenant: TenantId) -> Result<SlotLease, RingError> {
        let slot = self.free.pop().ok_or(RingError::Exhausted)?;
        self.leased[slot as usize] = true;
        Ok(SlotLease {
            slot,
            tenant,
            obligation: Obligation::new(&self.auditor),
        })
    }

    /// Return a lease. A lease from another ring is handed back.
    pub fn release(&mut self, mut lease: SlotLease) -> Result<(), (SlotLease, RingError)> {
        if let Err(e) = self.check(&lease) {
            return Err((lease, e));
        }
        self.leased[lease.slot as usize] = false;
        // `free` was allocated with room for every slot.
        self.free.push(lease.slot);
        lease.obligation.discharge();
        Ok(())
    }

    /// The slot's whole payload area, for the producer to fill in place.
    pub fn payload_mut<'a, T: 'static>(
        &self,
        store: impl Into<wasmtime::StoreContextMut<'a, T>>,
        lease: &SlotLease,
    ) -> Result<&'a mut [u8], RingError> {
        self.check(lease)?;
        let start = self.payload_offset(lease.slot) as usize;
        let end = start + self.payload_capacity() as usize;
        Ok(&mut self.memory.data_mut(store.into())[start..end])
    }

    /// The first `len` payload bytes, read in place.
    pub fn payload<'a>(
        &self,
        store: &'a impl AsContext,
        lease: &SlotLease,
        len: u32,
    ) -> Result<&'a [u8], RingError> {
        self.check(lease)?;
        if len > self.payload_capacity() {
            return Err(RingError::PayloadTooLarge {
                len,
                capacity: self.payload_capacity(),
            });
        }
        let start = self.payload_offset(lease.slot) as usize;
        Ok(&self.memory.data(store)[start..start + len as usize])
    }

    /// Write the slot's grant descriptor for a `len`-byte payload.
    pub fn publish(
        &self,
        mut store: impl AsContextMut,
        lease: &SlotLease,
        len: u32,
        schema_id: u32,
        tenant_flags: u32,
        record_timestamp_ns: u64,
    ) -> Result<PluginSliceGrant, RingError> {
        self.check(lease)?;
        if len > self.payload_capacity() {
            return Err(RingError::PayloadTooLarge {
                len,
                capacity: self.payload_capacity(),
            });
        }
        let grant = PluginSliceGrant {
            ring_offset: self.payload_offset(lease.slot),
            byte_len: len,
            schema_id,
            tenant_flags,
            record_timestamp_ns,
        };
        let at = self.grant_offset(lease) as usize;
        let bytes = &mut self.memory.data_mut(&mut store)[at..at + GRANT_BYTES as usize];
        let (mut view, _) = split_wire_mut(&self.layout, bytes)?;
        view.write_u64(&["ring_offset"], grant.ring_offset.into())?;
        view.write_u64(&["byte_len"], grant.byte_len.into())?;
        view.write_u64(&["schema_id"], grant.schema_id.into())?;
        view.write_u64(&["tenant_flags"], grant.tenant_flags.into())?;
        view.write_u64(&["record_timestamp_ns"], grant.record_timestamp_ns)?;
        Ok(grant)
    }

    /// Decode a slot's grant descriptor through the wire view.
    pub fn read_grant(
        &self,
        store: impl AsContext,
        lease: &SlotLease,
    ) -> Result<PluginSliceGrant, RingError> {
        self.check(lease)?;
        let at = self.grant_offset(lease) as usize;
        let data = self.memory.data(&store);
        let view = view_wire(&self.layout, &data[at..at + GRANT_BYTES as usize])?;
        let u32f = |name: &str| -> Result<u32, RingError> {
            u32::try_from(view.read_u64(&[name])?)
                .map_err(|_| RingError::Wire(ViewError::OutOfRange))
        };
        Ok(PluginSliceGrant {
            ring_offset: u32f("ring_offset")?,
            byte_len: u32f("byte_len")?,
            schema_id: u32f("schema_id")?,
            tenant_flags: u32f("tenant_flags")?,
            record_timestamp_ns: view.read_u64(&["record_timestamp_ns"])?,
        })
    }
}

/// §30.3 `TenantUdfMetrics`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TenantUdfMetrics {
    pub udf_invocations: u64,
    pub udf_traps: u64,
    pub fuel_consumed: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterError {
    InitFailed(String),
    UnknownTenant(TenantId),
    /// The lease belongs to a different tenant than the filter being run.
    LeaseTenantMismatch {
        lease: TenantId,
        filter: TenantId,
    },
    Ring(RingError),
    OutOfFuel,
    GuestTrapped(String),
    /// The guest returned a length outside the grant or not record-aligned.
    GuestContract {
        returned: i32,
        byte_len: u32,
    },
}
impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stream filter: {self:?}")
    }
}
impl std::error::Error for FilterError {}
impl From<RingError> for FilterError {
    fn from(e: RingError) -> Self {
        Self::Ring(e)
    }
}

/// Record size the broker filters operate on: `key: u64le, value: u64le`.
pub const RECORD_BYTES: u32 = 16;

struct TenantFilter {
    tenant: TenantId,
    filter: TypedFunc<i32, i32>,
    fuel_per_call: u64,
    metrics: TenantUdfMetrics,
}

/// The §30.3 multi-tenant broker stream filter: several tenants' filter
/// modules over one [`SharedPluginRing`], each call fuel-bounded.
pub struct StreamFilterBroker {
    store: Store<()>,
    ring: SharedPluginRing,
    tenants: Vec<TenantFilter>,
}

impl StreamFilterBroker {
    pub fn new(slots: u32, slot_bytes: u32) -> Result<Self, FilterError> {
        let mut c = Config::new();
        c.consume_fuel(true);
        let engine = Engine::new(&c).map_err(|e| FilterError::InitFailed(format!("{e:?}")))?;
        let mut store = Store::new(&engine, ());
        let ring = SharedPluginRing::new(&mut store, slots, slot_bytes)?;
        Ok(Self {
            store,
            ring,
            tenants: Vec::new(),
        })
    }

    /// Instantiate a tenant filter module. The module must import the ring
    /// as `(import "nexus" "ring" (memory N))` and export
    /// `filter: (grant_offset: i32) -> i32` returning bytes written.
    pub fn add_tenant(
        &mut self,
        tenant: TenantId,
        module_text_or_binary: &[u8],
        fuel_per_call: u64,
    ) -> Result<(), FilterError> {
        let init = |e: wasmtime::Error| FilterError::InitFailed(format!("{e:?}"));
        let module = Module::new(self.store.engine(), module_text_or_binary).map_err(init)?;
        self.store.set_fuel(u64::MAX).map_err(init)?;
        let instance =
            Instance::new(&mut self.store, &module, &[self.ring.memory().into()]).map_err(init)?;
        let filter = instance
            .get_typed_func::<i32, i32>(&mut self.store, "filter")
            .map_err(init)?;
        self.tenants.push(TenantFilter {
            tenant,
            filter,
            fuel_per_call,
            metrics: TenantUdfMetrics::default(),
        });
        Ok(())
    }

    pub fn ring(&self) -> &SharedPluginRing {
        &self.ring
    }
    pub fn metrics(&self, tenant: TenantId) -> Option<TenantUdfMetrics> {
        self.tenants
            .iter()
            .find(|t| t.tenant == tenant)
            .map(|t| t.metrics)
    }

    pub fn lease(&mut self, tenant: TenantId) -> Result<SlotLease, FilterError> {
        Ok(self.ring.lease(tenant)?)
    }
    pub fn release(&mut self, lease: SlotLease) -> Result<(), (SlotLease, FilterError)> {
        self.ring.release(lease).map_err(|(l, e)| (l, e.into()))
    }
    /// Producer access to the slot's payload area.
    pub fn payload_mut(&mut self, lease: &SlotLease) -> Result<&mut [u8], FilterError> {
        Ok(self.ring.payload_mut(&mut self.store, lease)?)
    }
    pub fn payload(&self, lease: &SlotLease, len: u32) -> Result<&[u8], FilterError> {
        Ok(self.ring.payload(&self.store, lease, len)?)
    }
    /// Decode a slot's grant descriptor.
    pub fn read_grant(&self, lease: &SlotLease) -> Result<PluginSliceGrant, FilterError> {
        Ok(self.ring.read_grant(&self.store, lease)?)
    }
    /// Base address of ring memory, for zero-copy assertions.
    pub fn ring_base(&self) -> *const u8 {
        self.ring.memory().data_ptr(&self.store)
    }

    /// `apply_tenant_wasm_udf` (§30.3): publish the grant for `len` payload
    /// bytes already in the slot, run the tenant's filter in place under its
    /// fuel budget, and return the bytes it wrote. Metrics are updated on the
    /// happy and the trap path alike.
    pub fn apply(
        &mut self,
        lease: &SlotLease,
        len: u32,
        schema_id: u32,
        record_timestamp_ns: u64,
    ) -> Result<u32, FilterError> {
        let idx = self
            .tenants
            .iter()
            .position(|t| t.tenant == lease.tenant)
            .ok_or(FilterError::UnknownTenant(lease.tenant))?;
        let grant_offset = self.ring.grant_offset(lease);
        self.ring.publish(
            &mut self.store,
            lease,
            len,
            schema_id,
            1,
            record_timestamp_ns,
        )?;
        let t = &mut self.tenants[idx];
        let fuel = t.fuel_per_call;
        self.store
            .set_fuel(fuel)
            .map_err(|e| FilterError::InitFailed(format!("{e:?}")))?;
        // Grant offsets fit in i32: the ring is at most 4 GiB and slots are
        // addressed by the guest as unsigned i32 loads.
        let r = t.filter.call(&mut self.store, grant_offset as i32);
        let consumed = fuel - self.store.get_fuel().unwrap_or(0).min(fuel);
        t.metrics.fuel_consumed += consumed;
        t.metrics.bytes_in += u64::from(len);
        match r {
            Ok(written)
                if written >= 0
                    && (written as u32) <= len
                    && (written as u32).is_multiple_of(RECORD_BYTES) =>
            {
                t.metrics.udf_invocations += 1;
                t.metrics.bytes_out += written as u64;
                Ok(written as u32)
            }
            Ok(written) => {
                t.metrics.udf_traps += 1;
                Err(FilterError::GuestContract {
                    returned: written,
                    byte_len: len,
                })
            }
            Err(e) => {
                t.metrics.udf_traps += 1;
                Err(match e.downcast_ref::<Trap>() {
                    Some(Trap::OutOfFuel) => FilterError::OutOfFuel,
                    _ => FilterError::GuestTrapped(format!("{e:?}")),
                })
            }
        }
    }
}

/// Tenant filter: keep records whose `value >= 100`, compacting in place.
pub const FILTER_THRESHOLD_WAT: &str = r#"
(module
  (import "nexus" "ring" (memory $ring 1))
  (func (export "filter") (param $grant i32) (result i32)
    (local $src i32) (local $end i32) (local $dst i32) (local $base i32)
    (local.set $base (i32.load offset=0 (local.get $grant)))
    (local.set $src (local.get $base))
    (local.set $dst (local.get $base))
    (local.set $end (i32.add (local.get $base) (i32.load offset=4 (local.get $grant))))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $src) (local.get $end)))
        (if (i64.ge_u (i64.load offset=8 (local.get $src)) (i64.const 100))
          (then
            (i64.store offset=0 (local.get $dst) (i64.load offset=0 (local.get $src)))
            (i64.store offset=8 (local.get $dst) (i64.load offset=8 (local.get $src)))
            (local.set $dst (i32.add (local.get $dst) (i32.const 16)))))
        (local.set $src (i32.add (local.get $src) (i32.const 16)))
        (br $next)))
    (i32.sub (local.get $dst) (local.get $base)))
)
"#;

/// Tenant filter: keep even keys and double their values, in place.
pub const FILTER_EVEN_DOUBLE_WAT: &str = r#"
(module
  (import "nexus" "ring" (memory $ring 1))
  (func (export "filter") (param $grant i32) (result i32)
    (local $src i32) (local $end i32) (local $dst i32) (local $base i32)
    (local.set $base (i32.load offset=0 (local.get $grant)))
    (local.set $src (local.get $base))
    (local.set $dst (local.get $base))
    (local.set $end (i32.add (local.get $base) (i32.load offset=4 (local.get $grant))))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $src) (local.get $end)))
        (if (i64.eqz (i64.and (i64.load offset=0 (local.get $src)) (i64.const 1)))
          (then
            (i64.store offset=0 (local.get $dst) (i64.load offset=0 (local.get $src)))
            (i64.store offset=8 (local.get $dst)
              (i64.shl (i64.load offset=8 (local.get $src)) (i64.const 1)))
            (local.set $dst (i32.add (local.get $dst) (i32.const 16)))))
        (local.set $src (i32.add (local.get $src) (i32.const 16)))
        (br $next)))
    (i32.sub (local.get $dst) (local.get $base)))
)
"#;
