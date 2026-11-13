//! Wasmtime component host backing WIT `own<T>`/`borrow<T>` with the
//! generational [`WitResourceTable`] (§30.2, §30.5) and running guests under
//! deterministic fuel quanta and epoch deadlines (§30.1 item 4).
//!
//! The WIT world is [`PLUGIN_WIT`]. The host defines resource `blob`; every
//! tenant's component instance shares one table, and each handle carries its
//! tenant tag. Wasmtime's own per-instance handle tables already reject
//! guest misuse of component handles (double drop, use after move); the
//! Nexus table adds the tenant tag, `num_lends` for host lends that outlive a
//! call, and a raw 64-bit integer path on which a guest can present any
//! integer at all. Both layers are exercised in `tests/components.rs`.
//!
//! At the guest boundary, a stale, forged and cross-tenant integer all map to
//! the same error code (`-1`), so a guest learns nothing about other tenants'
//! slots. The host keeps the precise [`WitTableError`] in an audit log.

use crate::fuel::{QuantumScheduler, SliceEvent, TenantUsage};
use crate::table::{
    LeakAuditor, Obligation, TenantId, WitBorrow, WitHandle, WitResourceTable, WitTableError,
};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use wasmtime::component::{Component, Linker, Resource, ResourceType, TypedFunc};
use wasmtime::{Config, Engine, Store, StoreContextMut, Trap};

/// The interface name the host implements.
pub const HOST_INTERFACE: &str = "vesper:nexus/host@0.1.0";

/// The WIT world guests are written against. Test guests are hand-written
/// component text implementing exactly this world.
pub const PLUGIN_WIT: &str = r#"package vesper:nexus@0.1.0;

interface host {
  resource blob {
    constructor(tag: u32);
    /// Call-scoped borrow: num_lends is 1 for the duration of the call.
    tag: func() -> u32;
    /// Long host lend: num_lends stays raised until `unlend(ticket)`.
    lend: func() -> u64;
    /// The guest-integer form of this handle (generation << 32 | index).
    raw: func() -> u64;
  }
  unlend: func(ticket: u64) -> bool;
  /// Moves the own<blob> to the host, which drops it; returns its tag.
  consume: func(b: blob) -> u32;
  /// Resolve any integer as a handle of the calling tenant: tag, or -1.
  peek-raw: func(raw: u64) -> s32;
}

world plugin {
  import host;
  export run: func(scenario: u32, arg: u64) -> s64;
}
"#;

/// The host's resource representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    pub tag: u32,
}

/// One boundary rejection, for audits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryRejection {
    pub tenant: TenantId,
    pub operation: &'static str,
    pub error: WitTableError,
}

/// State shared by every tenant instance of one [`NexusEngine`].
pub struct HostShared {
    pub table: WitResourceTable<Blob>,
    lends: WitResourceTable<WitBorrow>,
    pub rejections: Vec<BoundaryRejection>,
    /// Tenants with a live (or leaked) instance. Teardown reclaims by
    /// tenant, so at most one instance per tenant may exist.
    live_tenants: Vec<TenantId>,
}

impl HostShared {
    fn new(capacity: u32) -> Self {
        Self {
            table: WitResourceTable::with_capacity(capacity),
            lends: WitResourceTable::with_capacity(capacity),
            rejections: Vec::new(),
            live_tenants: Vec::new(),
        }
    }
    /// Outstanding long lends for `tenant`.
    pub fn open_lends(&self, tenant: TenantId) -> usize {
        self.lends.live_count_for(tenant)
    }
    /// Release every lend, then every resource, `tenant` holds.
    fn reclaim(&mut self, tenant: TenantId) -> (usize, usize) {
        let HostShared { table, lends, .. } = self;
        let reclaimed_lends = lends.reclaim_tenant(tenant, |lend| {
            // Lends are only ever taken on the tenant's own resources, which
            // are reclaimed next, so ending them cannot fail; if it somehow
            // did, the resource reclaim below still releases the slot.
            let _ = table
                .end_borrow(lend)
                .map_err(|(lend, _)| table.forget_reclaimed_borrow(lend));
        });
        let reclaimed_resources = table.reclaim_tenant(tenant, |_| {});
        (reclaimed_resources, reclaimed_lends)
    }
    fn reject(&mut self, tenant: TenantId, operation: &'static str, error: WitTableError) {
        self.rejections.push(BoundaryRejection {
            tenant,
            operation,
            error,
        });
    }
    /// The component `rep` is the slot index; Wasmtime guarantees the guest
    /// handle it came from is live in the guest's table, and this re-checks
    /// liveness and tenant against ours.
    fn handle_for_rep(&self, tenant: TenantId, rep: u32) -> Result<WitHandle, WitTableError> {
        let state = self
            .table
            .slot_state(rep)
            .ok_or(WitTableError::InvalidOrStaleHandle(WitHandle {
                idx: rep,
                generation: 0,
            }))?;
        let h = WitHandle {
            idx: rep,
            generation: state.generation,
        };
        self.table.lookup(tenant, h)?;
        Ok(h)
    }
}

/// Per-store data: which tenant this store runs and the shared table.
pub struct TenantState {
    tenant: TenantId,
    shared: Arc<Mutex<HostShared>>,
}
impl TenantState {
    fn shared(&self) -> MutexGuard<'_, HostShared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
    Engine(String),
    Compile(String),
    Link(String),
    Instantiate(String),
    /// The tenant's total fuel budget ran out (`Trap::OutOfFuel`).
    OutOfFuel,
    /// The epoch deadline passed (`Trap::Interrupt`).
    EpochDeadline,
    /// A host import rejected a resource operation and trapped the guest.
    Table(WitTableError),
    /// Any other guest trap, including Wasmtime's own handle checks.
    GuestTrap(String),
    /// The instance trapped earlier and cannot be re-entered.
    Poisoned,
    /// The tenant already has a live instance (teardown reclaims by tenant).
    TenantBusy(TenantId),
    /// Fuel or epoch configuration was rejected.
    Config(String),
}
impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "nexus plugin: {self:?}")
    }
}
impl std::error::Error for PluginError {}

fn classify(e: wasmtime::Error) -> PluginError {
    if let Some(t) = e.downcast_ref::<WitTableError>() {
        return PluginError::Table(t.clone());
    }
    if let Some(trap) = e.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel => PluginError::OutOfFuel,
            Trap::Interrupt => PluginError::EpochDeadline,
            other => PluginError::GuestTrap(format!("{other}: {e:?}")),
        };
    }
    PluginError::GuestTrap(format!("{e:?}"))
}

fn trap(e: WitTableError) -> wasmtime::Error {
    wasmtime::Error::new(e)
}

/// A compiled component for the [`PLUGIN_WIT`] world.
#[derive(Clone)]
pub struct CompiledComponent {
    component: Component,
}

/// Linear owner of one tenant's component instance and its store.
///
/// The only exit is [`NexusEngine::destroy_instance`], which also reclaims
/// every resource and lend the tenant still holds. Dropping it instead is
/// counted in [`NexusEngine::leaked_instances`].
#[must_use = "a PluginInstance must be destroyed with NexusEngine::destroy_instance"]
pub struct PluginInstance {
    instance_id: u64,
    tenant: TenantId,
    budget: u64,
    store: Store<TenantState>,
    run: TypedFunc<(u32, u64), (i64,)>,
    poisoned: bool,
    obligation: Obligation,
}
impl PluginInstance {
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }
    pub fn tenant(&self) -> TenantId {
        self.tenant
    }
    pub fn remaining_fuel(&self) -> u64 {
        self.store.get_fuel().unwrap_or(0)
    }
    pub fn fuel_consumed(&self) -> u64 {
        self.budget - self.remaining_fuel().min(self.budget)
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

/// What teardown released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestroyReport {
    pub instance_id: u64,
    pub tenant: TenantId,
    pub fuel_consumed: u64,
    pub reclaimed_resources: usize,
    pub reclaimed_lends: usize,
}

/// Engine-wide options.
#[derive(Clone, Copy, Debug)]
pub struct NexusConfig {
    /// Capacity of the shared resource and lend tables.
    pub table_capacity: u32,
    /// Engine epoch ticks (one per scheduled slice) before a call traps.
    /// `None` disables the epoch deadline.
    pub epoch_deadline_slices: Option<u64>,
}
impl Default for NexusConfig {
    fn default() -> Self {
        Self {
            table_capacity: 1024,
            epoch_deadline_slices: None,
        }
    }
}

/// The multi-tenant plugin host (`PluginHost` in §30.2).
pub struct NexusEngine {
    engine: Engine,
    linker: Linker<TenantState>,
    shared: Arc<Mutex<HostShared>>,
    config: NexusConfig,
    next_instance: u64,
    instances: LeakAuditor,
}

impl NexusEngine {
    pub fn new(config: NexusConfig) -> Result<Self, PluginError> {
        let mut c = Config::new();
        c.wasm_component_model(true);
        c.consume_fuel(true);
        c.epoch_interruption(true);
        let engine = Engine::new(&c).map_err(|e| PluginError::Engine(format!("{e:?}")))?;
        let mut linker = Linker::new(&engine);
        Self::define_host(&mut linker).map_err(|e| PluginError::Link(format!("{e:?}")))?;
        Ok(Self {
            engine,
            linker,
            shared: Arc::new(Mutex::new(HostShared::new(config.table_capacity))),
            config,
            next_instance: 1,
            instances: LeakAuditor::default(),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Lock the shared table for inspection.
    pub fn shared(&self) -> MutexGuard<'_, HostShared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Instances dropped without `destroy_instance`.
    pub fn leaked_instances(&self) -> u64 {
        self.instances.leaked()
    }

    fn define_host(linker: &mut Linker<TenantState>) -> wasmtime::Result<()> {
        let mut host = linker.instance(HOST_INTERFACE)?;
        host.resource(
            "blob",
            ResourceType::host::<Blob>(),
            |cx: StoreContextMut<'_, TenantState>, rep| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let h = match sh.handle_for_rep(tenant, rep) {
                    Ok(h) => h,
                    Err(e) => {
                        sh.reject(tenant, "resource.drop", e.clone());
                        return Err(trap(e));
                    }
                };
                match sh.table.drop_own_raw(tenant, h) {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        sh.reject(tenant, "resource.drop", e.clone());
                        Err(trap(e))
                    }
                }
            },
        )?;
        host.func_wrap(
            "[constructor]blob",
            |cx: StoreContextMut<'_, TenantState>, (tag,): (u32,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let own = sh
                    .table
                    .insert_own(tenant, Blob { tag })
                    .map_err(|(_, e)| trap(e))?;
                let h = sh.table.lower_to_guest(own);
                Ok((Resource::<Blob>::new_own(h.idx),))
            },
        )?;
        host.func_wrap(
            "[method]blob.tag",
            |cx: StoreContextMut<'_, TenantState>, (b,): (Resource<Blob>,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let h = sh.handle_for_rep(tenant, b.rep()).map_err(trap)?;
                // A call-scoped borrow raises num_lends for the call only.
                let lend = sh.table.begin_borrow(tenant, h).map_err(trap)?;
                let tag = sh.table.get(&lend).map_err(trap)?.tag;
                sh.table.end_borrow(lend).map_err(|(_, e)| trap(e))?;
                Ok((tag,))
            },
        )?;
        host.func_wrap(
            "[method]blob.lend",
            |cx: StoreContextMut<'_, TenantState>, (b,): (Resource<Blob>,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let h = sh.handle_for_rep(tenant, b.rep()).map_err(trap)?;
                let lend = sh.table.begin_borrow(tenant, h).map_err(trap)?;
                match sh.lends.insert_own(tenant, lend) {
                    Ok(ticket) => Ok((sh.lends.lower_to_guest(ticket).to_bits(),)),
                    Err((lend, e)) => {
                        // Undo the lend: the guest never received a ticket.
                        sh.table.end_borrow(lend).map_err(|(_, e)| trap(e))?;
                        Err(trap(e))
                    }
                }
            },
        )?;
        host.func_wrap(
            "[method]blob.raw",
            |cx: StoreContextMut<'_, TenantState>, (b,): (Resource<Blob>,)| {
                let tenant = cx.data().tenant;
                let sh = cx.data().shared();
                let h = sh.handle_for_rep(tenant, b.rep()).map_err(trap)?;
                Ok((h.to_bits(),))
            },
        )?;
        host.func_wrap(
            "unlend",
            |cx: StoreContextMut<'_, TenantState>, (ticket,): (u64,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let t = WitHandle::from_bits(ticket);
                let lend = match sh.lends.drop_own_raw(tenant, t) {
                    Ok(l) => l,
                    Err(e) => {
                        sh.reject(tenant, "unlend", e);
                        return Ok((false,));
                    }
                };
                match sh.table.end_borrow(lend) {
                    Ok(()) => Ok((true,)),
                    Err((lend, e)) => {
                        // Unreachable while the lend is outstanding (drop
                        // needs num_lends == 0). If the slot is gone the lend
                        // is discharged; otherwise dropping it counts a leak.
                        sh.reject(tenant, "unlend", e);
                        let _ = sh.table.forget_reclaimed_borrow(lend);
                        Ok((false,))
                    }
                }
            },
        )?;
        host.func_wrap(
            "consume",
            |cx: StoreContextMut<'_, TenantState>, (b,): (Resource<Blob>,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let h = sh.handle_for_rep(tenant, b.rep()).map_err(trap)?;
                match sh.table.drop_own_raw(tenant, h) {
                    Ok(blob) => Ok((blob.tag,)),
                    Err(e) => {
                        sh.reject(tenant, "consume", e.clone());
                        Err(trap(e))
                    }
                }
            },
        )?;
        host.func_wrap(
            "peek-raw",
            |cx: StoreContextMut<'_, TenantState>, (raw,): (u64,)| {
                let tenant = cx.data().tenant;
                let mut sh = cx.data().shared();
                let h = WitHandle::from_bits(raw);
                match sh.table.lookup(tenant, h) {
                    Ok(blob) => Ok((blob.tag as i32,)),
                    Err(e) => {
                        sh.reject(tenant, "peek-raw", e);
                        Ok((-1i32,))
                    }
                }
            },
        )?;
        Ok(())
    }

    /// Compile component binary or component text.
    pub fn compile(&self, bytes: &[u8]) -> Result<CompiledComponent, PluginError> {
        let component = Component::new(&self.engine, bytes)
            .map_err(|e| PluginError::Compile(format!("{e:?}")))?;
        Ok(CompiledComponent { component })
    }

    /// `PluginHost::instantiate_with_fuel` (§30.2).
    pub fn instantiate_with_fuel(
        &mut self,
        compiled: &CompiledComponent,
        tenant: TenantId,
        fuel_budget: u64,
    ) -> Result<PluginInstance, PluginError> {
        {
            let mut sh = self.shared();
            if sh.live_tenants.contains(&tenant) {
                return Err(PluginError::TenantBusy(tenant));
            }
            sh.live_tenants.push(tenant);
        }
        let r = self.instantiate_inner(compiled, tenant, fuel_budget);
        if r.is_err() {
            // Roll back whatever the failed start-up created: resources and
            // lends minted by guest initialization, and the tenant slot.
            let mut sh = self.shared();
            sh.reclaim(tenant);
            sh.live_tenants.retain(|t| *t != tenant);
        }
        r
    }

    fn instantiate_inner(
        &mut self,
        compiled: &CompiledComponent,
        tenant: TenantId,
        fuel_budget: u64,
    ) -> Result<PluginInstance, PluginError> {
        let mut store = Store::new(
            &self.engine,
            TenantState {
                tenant,
                shared: self.shared.clone(),
            },
        );
        store
            .set_fuel(fuel_budget)
            .map_err(|e| PluginError::Config(format!("{e:?}")))?;
        match self.config.epoch_deadline_slices {
            Some(n) => {
                store.set_epoch_deadline(n);
                store.epoch_deadline_trap();
            }
            None => store.set_epoch_deadline(u64::MAX),
        }
        let instance = block_on(
            self.linker
                .instantiate_async(&mut store, &compiled.component),
        )
        .map_err(|e| PluginError::Instantiate(format!("{e:?}")))?;
        let run = instance
            .get_typed_func::<(u32, u64), (i64,)>(&mut store, "run")
            .map_err(|e| PluginError::Instantiate(format!("{e:?}")))?;
        let instance_id = self.next_instance;
        self.next_instance += 1;
        Ok(PluginInstance {
            instance_id,
            tenant,
            budget: fuel_budget,
            store,
            run,
            poisoned: false,
            obligation: Obligation::new(&self.instances),
        })
    }

    /// Run the guest export to completion (or trap) without preemption.
    pub fn call_run(
        &self,
        inst: &mut PluginInstance,
        scenario: u32,
        arg: u64,
    ) -> Result<i64, PluginError> {
        if inst.poisoned {
            return Err(PluginError::Poisoned);
        }
        inst.store
            .fuel_async_yield_interval(None)
            .map_err(|e| PluginError::Config(format!("{e:?}")))?;
        let run = inst.run;
        let r = block_on(run.call_async(&mut inst.store, (scenario, arg)));
        finish_call(inst, r)
    }

    /// `PluginHost::destroy_instance` (§30.2): drops the store and releases
    /// every lend and resource the tenant still holds.
    pub fn destroy_instance(&self, mut inst: PluginInstance) -> DestroyReport {
        inst.obligation.discharge();
        let fuel_consumed = inst.fuel_consumed();
        let tenant = inst.tenant;
        let instance_id = inst.instance_id;
        drop(inst);
        let mut sh = self.shared();
        let (reclaimed_resources, reclaimed_lends) = sh.reclaim(tenant);
        sh.live_tenants.retain(|t| *t != tenant);
        DestroyReport {
            instance_id,
            tenant,
            fuel_consumed,
            reclaimed_resources,
            reclaimed_lends,
        }
    }

    /// Run `calls` preemptively: each guest yields after burning its
    /// quantum of fuel, and `scheduler` picks who runs next. When the engine
    /// has an epoch deadline, the engine epoch advances by one tick after
    /// every slice. Returns each instance with its result and usage, in
    /// input order, plus the slice trace. Deterministic for fixed inputs.
    pub fn run_scheduled<S: QuantumScheduler>(
        &self,
        calls: Vec<ScheduledCall>,
        scheduler: &mut S,
    ) -> Result<ScheduledRun, ScheduleError> {
        type Job = Pin<Box<dyn Future<Output = (PluginInstance, wasmtime::Result<(i64,)>)>>>;
        // Validate everything before any job starts, so a bad parameter hands
        // every instance back instead of stranding it.
        let invalid = calls.iter().find_map(|c| {
            if c.quantum == 0 {
                Some(PluginError::Config("zero quantum".into()))
            } else if c.instance.poisoned {
                Some(PluginError::Poisoned)
            } else {
                None
            }
        });
        let mut calls = calls;
        let mut config_error = invalid;
        if config_error.is_none() {
            for c in &mut calls {
                if let Err(e) = c.instance.store.fuel_async_yield_interval(Some(c.quantum)) {
                    config_error = Some(PluginError::Config(format!("{e:?}")));
                    break;
                }
            }
        }
        if let Some(error) = config_error {
            return Err(ScheduleError {
                error,
                instances: calls.into_iter().map(|c| c.instance).collect(),
            });
        }
        let n = calls.len();
        let mut jobs: Vec<Option<Job>> = Vec::with_capacity(n);
        let mut slices = vec![0u32; n];
        let mut tenants = Vec::with_capacity(n);
        let mut done: Vec<Option<ScheduledOutcome>> = (0..n).map(|_| None).collect();
        for (i, call) in calls.into_iter().enumerate() {
            let ScheduledCall {
                mut instance,
                scenario,
                arg,
                ..
            } = call;
            tenants.push(instance.tenant);
            scheduler.admit(i, instance.tenant);
            let run = instance.run;
            jobs.push(Some(Box::pin(async move {
                let r = run.call_async(&mut instance.store, (scenario, arg)).await;
                (instance, r)
            })));
        }
        let mut trace = Vec::new();
        let mut scheduler_violations = 0u32;
        let mut cx = Context::from_waker(Waker::noop());
        let mut fallback = 0usize;
        loop {
            // A scheduler that names an unknown or finished job, or goes idle
            // with runnable jobs, must not strand instances inside their
            // futures: count the violation and drain the rest round robin.
            let (token, job) = match scheduler.next_job() {
                Some((token, job)) if jobs.get(job).is_some_and(Option::is_some) => {
                    (Some(token), job)
                }
                other => {
                    if !jobs.iter().any(Option::is_some) && other.is_none() {
                        break;
                    }
                    scheduler_violations += 1;
                    if let Some((token, _)) = other {
                        scheduler.finished(token);
                    }
                    let Some(next) = (0..n)
                        .map(|k| (fallback + k) % n)
                        .find(|&j| jobs[j].is_some())
                    else {
                        break;
                    };
                    fallback = next + 1;
                    (None, next)
                }
            };
            let fut = jobs[job].as_mut().expect("job checked runnable");
            slices[job] += 1;
            let slice = slices[job];
            let tenant = tenants[job];
            let polled = fut.as_mut().poll(&mut cx);
            if self.config.epoch_deadline_slices.is_some() {
                self.engine.increment_epoch();
            }
            match polled {
                Poll::Pending => {
                    trace.push(SliceEvent::Preempted { tenant, slice });
                    if let Some(token) = token {
                        scheduler.preempted(token);
                    }
                }
                Poll::Ready((mut instance, r)) => {
                    jobs[job] = None;
                    let result = finish_call(&mut instance, r);
                    trace.push(if result.is_ok() {
                        SliceEvent::Completed { tenant, slice }
                    } else {
                        SliceEvent::Trapped { tenant, slice }
                    });
                    if let Some(token) = token {
                        scheduler.finished(token);
                    }
                    done[job] = Some(ScheduledOutcome {
                        usage: TenantUsage {
                            tenant,
                            slices: slice,
                            fuel_consumed: instance.fuel_consumed(),
                        },
                        instance,
                        result,
                    });
                }
            }
        }
        Ok(ScheduledRun {
            outcomes: done
                .into_iter()
                .map(|o| o.expect("every job finished"))
                .collect(),
            trace,
            scheduler_violations,
        })
    }
}

fn finish_call(inst: &mut PluginInstance, r: wasmtime::Result<(i64,)>) -> Result<i64, PluginError> {
    match r {
        Ok((v,)) => Ok(v),
        Err(e) => {
            inst.poisoned = true;
            Err(classify(e))
        }
    }
}

/// One preemptible call for [`NexusEngine::run_scheduled`].
pub struct ScheduledCall {
    pub instance: PluginInstance,
    pub scenario: u32,
    pub arg: u64,
    pub quantum: u64,
}

/// Result of [`NexusEngine::run_scheduled`].
pub struct ScheduledRun {
    /// One outcome per call, in input order.
    pub outcomes: Vec<ScheduledOutcome>,
    pub trace: Vec<SliceEvent>,
    /// Times the scheduler named an unknown or finished job or went idle with
    /// runnable jobs; those slices were run round robin instead.
    pub scheduler_violations: u32,
}

impl fmt::Debug for ScheduledRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScheduledRun")
            .field("outcomes", &self.outcomes.len())
            .field("trace", &self.trace)
            .field("scheduler_violations", &self.scheduler_violations)
            .finish()
    }
}

/// `run_scheduled` refused to start; every instance is handed back.
pub struct ScheduleError {
    pub error: PluginError,
    pub instances: Vec<PluginInstance>,
}
impl fmt::Debug for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScheduleError")
            .field("error", &self.error)
            .field("instances", &self.instances.len())
            .finish()
    }
}

pub struct ScheduledOutcome {
    pub instance: PluginInstance,
    pub result: Result<i64, PluginError>,
    pub usage: TenantUsage,
}

/// Poll a future to completion on this thread. Wasmtime's fuel yields wake
/// the waker before returning `Pending`, so a no-op waker plus re-polling is
/// a complete executor for these futures.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
