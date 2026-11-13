//! Opt-in observations of selected empty linear structs in the reference
//! evaluator. Identities travel in actual values; this ledger never predicts
//! events from syntax and does not claim whole-program exhaustion.

use super::Value;
use crate::{ast::Mode, ast::Span, check::World};
use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

/// Finite provider limits, independent of evaluator/process allocation limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceTraceLimits {
    pub max_selected_types: usize,
    pub max_name_bytes: usize,
    pub max_selected_name_bytes: usize,
    /// Total minted identities, including consumed tombstones in this run.
    pub max_resources: usize,
    pub max_events: usize,
    pub max_value_nodes: usize,
    pub max_value_depth: usize,
}
impl ResourceTraceLimits {
    pub const DEFAULT: Self = Self {
        max_selected_types: 8,
        max_name_bytes: 256,
        max_selected_name_bytes: 2048,
        max_resources: 256,
        max_events: 512,
        max_value_nodes: 4096,
        max_value_depth: 64,
    };
    pub const HARD: Self = Self {
        max_selected_types: 64,
        max_name_bytes: 1024,
        max_selected_name_bytes: 16384,
        max_resources: 4096,
        max_events: 8192,
        max_value_nodes: 65536,
        max_value_depth: 256,
    };
    fn validate(self) -> Result<(), ResourceTraceError> {
        let pairs = [
            (self.max_selected_types, Self::HARD.max_selected_types),
            (self.max_name_bytes, Self::HARD.max_name_bytes),
            (
                self.max_selected_name_bytes,
                Self::HARD.max_selected_name_bytes,
            ),
            (self.max_resources, Self::HARD.max_resources),
            (self.max_events, Self::HARD.max_events),
            (self.max_value_nodes, Self::HARD.max_value_nodes),
            (self.max_value_depth, Self::HARD.max_value_depth),
        ];
        if pairs.into_iter().any(|(actual, hard)| actual > hard) {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::InvalidLimits,
                None,
            ));
        }
        Ok(())
    }
}
impl Default for ResourceTraceLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Stable typed failure categories; arbitrary evaluator messages are not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceTraceFailure {
    InvalidLimits,
    SelectedTypeLimit,
    NameLimit,
    NameBytesLimit,
    DuplicateSelection,
    UnknownSelectedType,
    UnsupportedSelectedType,
    Reconfigured,
    UnsupportedExternal,
    MissingSite,
    ValueNodeLimit,
    ValueDepthLimit,
    UnsupportedValue,
    InstanceExhausted,
    RunExhausted,
    SerialExhausted,
    Allocation,
    NotRunning,
    AlreadyRunning,
    ResourceLimit,
    EventLimit,
    MissingIdentity,
    ForeignInstance,
    ForeignRun,
    UnknownIdentity,
    WrongKind,
    AlreadyConsumed,
    MalformedCarrier,
    DuplicateIdentity,
    BorrowConflict,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceTraceError {
    reason: ResourceTraceFailure,
    site: Option<Span>,
}
impl ResourceTraceError {
    pub(super) fn new(reason: ResourceTraceFailure, site: Option<Span>) -> Self {
        Self { reason, site }
    }
    pub fn reason(&self) -> ResourceTraceFailure {
        self.reason
    }
    pub fn site(&self) -> Option<&Span> {
        self.site.as_ref()
    }
}
impl fmt::Display for ResourceTraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "resource trace {:?}", self.reason)?;
        if let Some(site) = &self.site {
            write!(f, " at {}..{}", site.start, site.end)?;
        }
        Ok(())
    }
}
impl std::error::Error for ResourceTraceError {}

/// Validated declaration selection. No caller-supplied World address or hash
/// authorizes a different World; attachment must call `validate_world` again.
#[derive(Clone, Debug)]
pub struct ResourceTraceConfig {
    selected: Rc<Vec<String>>,
    limits: ResourceTraceLimits,
}
impl ResourceTraceConfig {
    pub fn new(
        world: &World,
        selected: &[&str],
        limits: ResourceTraceLimits,
    ) -> Result<Self, ResourceTraceError> {
        limits.validate()?;
        if selected.len() > limits.max_selected_types {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::SelectedTypeLimit,
                None,
            ));
        }
        let mut bytes = 0usize;
        // Check every limit and declaration before retaining any name copies.
        for (i, name) in selected.iter().enumerate() {
            if name.is_empty() || name.len() > limits.max_name_bytes {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::NameLimit,
                    None,
                ));
            }
            bytes = bytes.checked_add(name.len()).ok_or_else(|| {
                ResourceTraceError::new(ResourceTraceFailure::NameBytesLimit, None)
            })?;
            if bytes > limits.max_selected_name_bytes {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::NameBytesLimit,
                    None,
                ));
            }
            if selected[..i].contains(name) {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::DuplicateSelection,
                    None,
                ));
            }
            validate_declaration(world, name)?;
        }
        let mut names = Vec::new();
        names
            .try_reserve_exact(selected.len())
            .map_err(|_| allocation())?;
        for name in selected {
            let mut copy = String::new();
            copy.try_reserve_exact(name.len())
                .map_err(|_| allocation())?;
            copy.push_str(name);
            names.push(copy);
        }
        names.sort_unstable();
        Ok(Self {
            selected: Rc::new(names),
            limits,
        })
    }
    pub(crate) fn validate_world(&self, world: &World) -> Result<(), ResourceTraceError> {
        for name in self.selected.iter() {
            validate_declaration(world, name)?;
        }
        Ok(())
    }
    pub fn selected_types(&self) -> &[String] {
        &self.selected
    }
    pub fn limits(&self) -> ResourceTraceLimits {
        self.limits
    }
    fn kind(&self, ty: &str) -> Option<usize> {
        self.selected
            .binary_search_by(|name| name.as_str().cmp(ty))
            .ok()
    }
}
fn validate_declaration(world: &World, name: &str) -> Result<(), ResourceTraceError> {
    let adt = world
        .adts
        .get(name)
        .ok_or_else(|| ResourceTraceError::new(ResourceTraceFailure::UnknownSelectedType, None))?;
    if adt.name != name
        || adt.mode != Mode::Linear
        || adt.is_enum
        || !adt.generics.is_empty()
        || adt.variants.len() != 1
        || adt.variants[0].name != name
        || !adt.variants[0].fields.is_empty()
    {
        return Err(ResourceTraceError::new(
            ResourceTraceFailure::UnsupportedSelectedType,
            None,
        ));
    }
    Ok(())
}
fn allocation() -> ResourceTraceError {
    ResourceTraceError::new(ResourceTraceFailure::Allocation, None)
}

/// Opaque, process-local identity. There is no public constructor, decoder,
/// Default implementation or guest field from which one can be fabricated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceIdentity {
    instance: u64,
    run: u64,
    serial: u64,
}
impl ResourceIdentity {
    pub fn instance(self) -> u64 {
        self.instance
    }
    pub fn run(self) -> u64 {
        self.run
    }
    pub fn serial(self) -> u64 {
        self.serial
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceAction {
    Mint,
    Consume,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceEvent {
    action: ResourceAction,
    kind: usize,
    identity: ResourceIdentity,
    site: Span,
}
impl ResourceEvent {
    pub fn action(&self) -> ResourceAction {
        self.action
    }
    pub fn kind(&self) -> usize {
        self.kind
    }
    pub fn identity(&self) -> ResourceIdentity {
        self.identity
    }
    pub fn site(&self) -> &Span {
        &self.site
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLive {
    kind: usize,
    identity: ResourceIdentity,
    mint_site: Span,
}
impl ResourceLive {
    pub fn kind(&self) -> usize {
        self.kind
    }
    pub fn identity(&self) -> ResourceIdentity {
        self.identity
    }
    pub fn mint_site(&self) -> &Span {
        &self.mint_site
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceTraceStatus {
    Idle,
    Running,
    Completed,
    RuntimeError,
    FuelExhausted,
    Abandoned,
    TraceFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceTraceCompletion {
    Completed,
    RuntimeError,
    FuelExhausted,
    Abandoned,
}
impl From<ResourceTraceCompletion> for ResourceTraceStatus {
    fn from(value: ResourceTraceCompletion) -> Self {
        match value {
            ResourceTraceCompletion::Completed => Self::Completed,
            ResourceTraceCompletion::RuntimeError => Self::RuntimeError,
            ResourceTraceCompletion::FuelExhausted => Self::FuelExhausted,
            ResourceTraceCompletion::Abandoned => Self::Abandoned,
        }
    }
}
/// An owned, immutable snapshot. A subsequent epoch cannot mutate it.
#[derive(Debug)]
pub struct ResourceTraceReport {
    selected: Rc<Vec<String>>,
    instance: u64,
    run: u64,
    events: Vec<ResourceEvent>,
    live: Vec<ResourceLive>,
    status: ResourceTraceStatus,
    failure: Option<ResourceTraceError>,
}
impl ResourceTraceReport {
    pub fn selected_types(&self) -> &[String] {
        &self.selected
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    /// Zero is the idle epoch; actual runs start at one.
    pub fn run(&self) -> u64 {
        self.run
    }
    pub fn events(&self) -> &[ResourceEvent] {
        &self.events
    }
    pub fn live(&self) -> &[ResourceLive] {
        &self.live
    }
    pub fn status(&self) -> ResourceTraceStatus {
        self.status
    }
    pub fn failure(&self) -> Option<&ResourceTraceError> {
        self.failure.as_ref()
    }
}

#[derive(Clone, Copy)]
struct FailureRecord {
    reason: ResourceTraceFailure,
    site: Option<(usize, usize)>,
}
impl FailureRecord {
    fn from_error(error: &ResourceTraceError) -> Self {
        Self {
            reason: error.reason,
            site: error.site.as_ref().map(|site| (site.start, site.end)),
        }
    }
    fn error(self) -> ResourceTraceError {
        ResourceTraceError::new(self.reason, self.site.map(|(start, end)| start..end))
    }
}
struct LedgerEntry {
    resource: ResourceLive,
    live: bool,
}
struct State {
    config: ResourceTraceConfig,
    instance: u64,
    run: u64,
    next_serial: u64,
    events: Vec<ResourceEvent>,
    ledger: Vec<LedgerEntry>,
    seen: Vec<bool>,
    status: ResourceTraceStatus,
    failure: Option<FailureRecord>,
}
impl State {
    fn record(&mut self, error: &ResourceTraceError) {
        if self.failure.is_none() {
            self.failure = Some(FailureRecord::from_error(error));
        }
        self.status = ResourceTraceStatus::TraceFailed;
    }
    fn running(&self) -> Result<(), ResourceTraceError> {
        if let Some(error) = self.failure {
            return Err(error.error());
        }
        if self.status != ResourceTraceStatus::Running {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::NotRunning,
                None,
            ));
        }
        Ok(())
    }
    fn carrier(
        &self,
        ty: &str,
        constructor: &str,
        fields: usize,
        identity: Option<ResourceIdentity>,
        site: Option<Span>,
    ) -> Result<Option<usize>, ResourceTraceError> {
        let Some(kind) = self.config.kind(ty) else {
            return if identity.is_some() {
                Err(ResourceTraceError::new(
                    ResourceTraceFailure::WrongKind,
                    site,
                ))
            } else {
                Ok(None)
            };
        };
        if constructor != ty || fields != 0 {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::MalformedCarrier,
                site,
            ));
        }
        let identity = identity.ok_or_else(|| {
            ResourceTraceError::new(ResourceTraceFailure::MissingIdentity, site.clone())
        })?;
        let reason = if identity.instance != self.instance {
            Some(ResourceTraceFailure::ForeignInstance)
        } else if identity.run != self.run {
            Some(ResourceTraceFailure::ForeignRun)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(ResourceTraceError::new(reason, site));
        }
        let index = usize::try_from(identity.serial).ok();
        let entry = index
            .and_then(|index| self.ledger.get(index))
            .filter(|entry| entry.resource.identity == identity)
            .ok_or_else(|| {
                ResourceTraceError::new(ResourceTraceFailure::UnknownIdentity, site.clone())
            })?;
        if entry.resource.kind != kind {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::WrongKind,
                site,
            ));
        }
        if !entry.live {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::AlreadyConsumed,
                site,
            ));
        }
        Ok(index)
    }
}
struct Shared {
    state: RefCell<State>,
    pending: Cell<Option<FailureRecord>>,
    alive: Cell<bool>,
}
/// Read-only access survives an evaluator error or destruction of the runtime.
#[derive(Clone)]
pub struct ResourceTraceObserver {
    shared: Rc<Shared>,
}
impl ResourceTraceObserver {
    pub fn snapshot(&self) -> Result<ResourceTraceReport, ResourceTraceError> {
        let state = self
            .shared
            .state
            .try_borrow()
            .map_err(|_| ResourceTraceError::new(ResourceTraceFailure::BorrowConflict, None))?;
        let mut events = Vec::new();
        events
            .try_reserve_exact(state.events.len())
            .map_err(|_| allocation())?;
        events.extend(state.events.iter().cloned());
        let mut live = Vec::new();
        live.try_reserve_exact(state.ledger.len())
            .map_err(|_| allocation())?;
        live.extend(
            state
                .ledger
                .iter()
                .filter(|entry| entry.live)
                .map(|entry| entry.resource.clone()),
        );
        let failure = state.failure.or(self.shared.pending.get());
        let status = if failure.is_some() {
            ResourceTraceStatus::TraceFailed
        } else if !self.shared.alive.get() && state.status == ResourceTraceStatus::Running {
            ResourceTraceStatus::Abandoned
        } else {
            state.status
        };
        Ok(ResourceTraceReport {
            selected: state.config.selected.clone(),
            instance: state.instance,
            run: state.run,
            events,
            live,
            status,
            failure: failure.map(FailureRecord::error),
        })
    }
}

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
fn next_instance(counter: &AtomicU64) -> Result<u64, ResourceTraceError> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1).ok_or_else(|| {
            ResourceTraceError::new(ResourceTraceFailure::InstanceExhausted, None)
        })?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(current),
            Err(found) => current = found,
        }
    }
}
/// Only Runtime owns mutation authority. Observers and value identities cannot
/// mint, reset, consume, deserialize or reconfigure this engine.
pub(super) struct ResourceTraceEngine {
    shared: Rc<Shared>,
    config: ResourceTraceConfig,
}
pub(super) struct ResourceConsume<'a> {
    pub ty: &'a str,
    pub constructor: &'a str,
    pub field_count: usize,
    pub identity: Option<ResourceIdentity>,
    pub site: Span,
}
impl ResourceTraceEngine {
    pub(super) fn new(config: ResourceTraceConfig) -> Result<Self, ResourceTraceError> {
        let mut events = Vec::new();
        events
            .try_reserve_exact(config.limits.max_events)
            .map_err(|_| allocation())?;
        let mut ledger = Vec::new();
        ledger
            .try_reserve_exact(config.limits.max_resources)
            .map_err(|_| allocation())?;
        let mut seen = Vec::new();
        seen.try_reserve_exact(config.limits.max_resources)
            .map_err(|_| allocation())?;
        seen.resize(config.limits.max_resources, false);
        let instance = next_instance(&NEXT_INSTANCE)?;
        let state = State {
            config: config.clone(),
            instance,
            run: 0,
            next_serial: 0,
            events,
            ledger,
            seen,
            status: ResourceTraceStatus::Idle,
            failure: None,
        };
        Ok(Self {
            shared: Rc::new(Shared {
                state: RefCell::new(state),
                pending: Cell::new(None),
                alive: Cell::new(true),
            }),
            config,
        })
    }
    pub(super) fn limits(&self) -> ResourceTraceLimits {
        self.config.limits
    }
    pub(super) fn is_selected(&self, ty: &str) -> bool {
        self.config.kind(ty).is_some()
    }
    pub(super) fn observer(&self) -> ResourceTraceObserver {
        ResourceTraceObserver {
            shared: self.shared.clone(),
        }
    }
    fn apply<T>(
        &mut self,
        operation: impl FnOnce(&mut State) -> Result<T, ResourceTraceError>,
    ) -> Result<T, ResourceTraceError> {
        let mut state = self.shared.state.try_borrow_mut().map_err(|_| {
            let record = FailureRecord {
                reason: ResourceTraceFailure::BorrowConflict,
                site: None,
            };
            if self.shared.pending.get().is_none() {
                self.shared.pending.set(Some(record));
            }
            record.error()
        })?;
        if let Some(record) = self.shared.pending.take() {
            state.record(&record.error());
        }
        let result = operation(&mut state);
        if let Err(error) = &result {
            state.record(error);
        }
        result
    }
    pub(super) fn begin_run(&mut self) -> Result<(), ResourceTraceError> {
        self.apply(|state| {
            if state.status == ResourceTraceStatus::Running {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::AlreadyRunning,
                    None,
                ));
            }
            let run = state
                .run
                .checked_add(1)
                .ok_or_else(|| ResourceTraceError::new(ResourceTraceFailure::RunExhausted, None))?;
            state.run = run;
            state.next_serial = 0;
            state.events.clear();
            state.ledger.clear();
            state.seen.fill(false);
            state.failure = None;
            state.status = ResourceTraceStatus::Running;
            Ok(())
        })
    }
    pub(super) fn mint(
        &mut self,
        ty: &str,
        constructor: &str,
        field_count: usize,
        site: Span,
    ) -> Result<Option<ResourceIdentity>, ResourceTraceError> {
        self.apply(|state| {
            state.running()?;
            let Some(kind) = state.config.kind(ty) else {
                return Ok(None);
            };
            if constructor != ty || field_count != 0 {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::MalformedCarrier,
                    Some(site),
                ));
            }
            if site.start >= site.end {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::MissingSite,
                    Some(site),
                ));
            }
            if state.ledger.len() >= state.config.limits.max_resources {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::ResourceLimit,
                    Some(site),
                ));
            }
            if state.events.len() >= state.config.limits.max_events {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::EventLimit,
                    Some(site),
                ));
            }
            let next = state.next_serial.checked_add(1).ok_or_else(|| {
                ResourceTraceError::new(ResourceTraceFailure::SerialExhausted, Some(site.clone()))
            })?;
            let identity = ResourceIdentity {
                instance: state.instance,
                run: state.run,
                serial: state.next_serial,
            };
            state.ledger.push(LedgerEntry {
                resource: ResourceLive {
                    kind,
                    identity,
                    mint_site: site.clone(),
                },
                live: true,
            });
            state.events.push(ResourceEvent {
                action: ResourceAction::Mint,
                kind,
                identity,
                site,
            });
            state.next_serial = next;
            Ok(Some(identity))
        })
    }
    pub(super) fn consume_batch(
        &mut self,
        batch: &[ResourceConsume<'_>],
    ) -> Result<(), ResourceTraceError> {
        self.apply(|state| {
            state.running()?;
            if batch.len() > state.config.limits.max_value_nodes {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::ValueNodeLimit,
                    None,
                ));
            }
            let mut plan = Vec::new();
            plan.try_reserve_exact(batch.len().min(state.ledger.len()))
                .map_err(|_| allocation())?;
            state.seen.fill(false);
            for item in batch {
                if let Some(index) = state.carrier(
                    item.ty,
                    item.constructor,
                    item.field_count,
                    item.identity,
                    Some(item.site.clone()),
                )? {
                    if item.site.start >= item.site.end {
                        return Err(ResourceTraceError::new(
                            ResourceTraceFailure::MissingSite,
                            Some(item.site.clone()),
                        ));
                    }
                    if state.seen[index] {
                        return Err(ResourceTraceError::new(
                            ResourceTraceFailure::DuplicateIdentity,
                            Some(item.site.clone()),
                        ));
                    }
                    state.seen[index] = true;
                    plan.push((index, item.site.clone()));
                }
            }
            if state
                .events
                .len()
                .checked_add(plan.len())
                .is_none_or(|n| n > state.config.limits.max_events)
            {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::EventLimit,
                    None,
                ));
            }
            // Every carrier, duplicate and capacity check completed. Private
            // ledger indices remain stable throughout this infallible commit.
            for (index, site) in plan {
                let entry = &mut state.ledger[index];
                entry.live = false;
                state.events.push(ResourceEvent {
                    action: ResourceAction::Consume,
                    kind: entry.resource.kind,
                    identity: entry.resource.identity,
                    site,
                });
            }
            Ok(())
        })
    }
    /// Only owning public entry/result boundaries use this walk. Internal
    /// shared aliases and inactive SSA slots are not additional owned values.
    pub(super) fn validate_values(&mut self, values: &[Value]) -> Result<(), ResourceTraceError> {
        self.apply(|state| {
            state.running()?;
            let limits = state.config.limits;
            if values.len() > limits.max_value_nodes {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::ValueNodeLimit,
                    None,
                ));
            }
            if !values.is_empty() && limits.max_value_depth == 0 {
                return Err(ResourceTraceError::new(
                    ResourceTraceFailure::ValueDepthLimit,
                    None,
                ));
            }
            let mut stack = Vec::new();
            stack
                .try_reserve_exact(values.len())
                .map_err(|_| allocation())?;
            stack.extend(values.iter().rev().map(|value| (value, 1usize)));
            state.seen.fill(false);
            let mut visited = 0usize;
            while let Some((value, depth)) = stack.pop() {
                visited = visited.checked_add(1).ok_or_else(|| {
                    ResourceTraceError::new(ResourceTraceFailure::ValueNodeLimit, None)
                })?;
                if visited > limits.max_value_nodes {
                    return Err(ResourceTraceError::new(
                        ResourceTraceFailure::ValueNodeLimit,
                        None,
                    ));
                }
                if depth > limits.max_value_depth {
                    return Err(ResourceTraceError::new(
                        ResourceTraceFailure::ValueDepthLimit,
                        None,
                    ));
                }
                let child_count = match value {
                    Value::Tuple(children) => children.len(),
                    Value::Data {
                        ty,
                        constructor,
                        fields,
                        resource_identity,
                    } => {
                        if let Some(index) = state.carrier(
                            ty,
                            constructor,
                            fields.len(),
                            *resource_identity,
                            None,
                        )? {
                            if state.seen[index] {
                                return Err(ResourceTraceError::new(
                                    ResourceTraceFailure::DuplicateIdentity,
                                    None,
                                ));
                            }
                            state.seen[index] = true;
                        }
                        fields.len()
                    }
                    Value::Int(_)
                    | Value::UInt(_, _)
                    | Value::Float(_)
                    | Value::Bool(_)
                    | Value::String(_) => 0,
                    _ => {
                        return Err(ResourceTraceError::new(
                            ResourceTraceFailure::UnsupportedValue,
                            None,
                        ));
                    }
                };
                if visited
                    .checked_add(stack.len())
                    .and_then(|n| n.checked_add(child_count))
                    .is_none_or(|n| n > limits.max_value_nodes)
                {
                    return Err(ResourceTraceError::new(
                        ResourceTraceFailure::ValueNodeLimit,
                        None,
                    ));
                }
                if child_count == 0 {
                    continue;
                }
                let next_depth = depth.checked_add(1).ok_or_else(|| {
                    ResourceTraceError::new(ResourceTraceFailure::ValueDepthLimit, None)
                })?;
                if next_depth > limits.max_value_depth {
                    return Err(ResourceTraceError::new(
                        ResourceTraceFailure::ValueDepthLimit,
                        None,
                    ));
                }
                stack
                    .try_reserve_exact(child_count)
                    .map_err(|_| allocation())?;
                match value {
                    Value::Tuple(children) => {
                        stack.extend(children.iter().rev().map(|value| (value, next_depth)))
                    }
                    Value::Data { fields, .. } => {
                        stack.extend(fields.values().rev().map(|value| (value, next_depth)))
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
    pub(super) fn finish(
        &mut self,
        completion: ResourceTraceCompletion,
    ) -> Result<(), ResourceTraceError> {
        self.apply(|state| {
            if state.status == ResourceTraceStatus::Running {
                state.status = completion.into();
            }
            Ok(())
        })
    }
    pub(super) fn fail(
        &mut self,
        reason: ResourceTraceFailure,
        site: Option<Span>,
    ) -> ResourceTraceError {
        let error = ResourceTraceError::new(reason, site);
        match self.apply(|_| Err::<(), _>(error.clone())) {
            Err(error) => error,
            Ok(()) => error,
        }
    }
}
impl Drop for ResourceTraceEngine {
    fn drop(&mut self) {
        self.shared.alive.set(false);
        if let Ok(mut state) = self.shared.state.try_borrow_mut() {
            if let Some(record) = self.shared.pending.take() {
                state.record(&record.error());
            }
            if state.status == ResourceTraceStatus::Running {
                state.status = ResourceTraceStatus::Abandoned;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Adt, Variant};
    use std::collections::BTreeMap;

    fn world() -> World {
        let mut world = World::default();
        for name in ["A", "B"] {
            world.adts.insert(
                name.into(),
                Adt {
                    name: name.into(),
                    mode: Mode::Linear,
                    generics: vec![],
                    variants: vec![Variant {
                        name: name.into(),
                        fields: vec![],
                        named: true,
                    }],
                    is_enum: false,
                    span: 0..1,
                },
            );
        }
        world
    }
    fn engine(limits: ResourceTraceLimits) -> ResourceTraceEngine {
        let config = ResourceTraceConfig::new(&world(), &["B", "A"], limits).unwrap();
        let mut engine = ResourceTraceEngine::new(config).unwrap();
        engine.begin_run().unwrap();
        engine
    }
    fn mint(engine: &mut ResourceTraceEngine, ty: &str) -> ResourceIdentity {
        engine.mint(ty, ty, 0, 10..20).unwrap().unwrap()
    }
    fn consume(identity: ResourceIdentity) -> ResourceConsume<'static> {
        ResourceConsume {
            ty: "A",
            constructor: "A",
            field_count: 0,
            identity: Some(identity),
            site: 30..40,
        }
    }
    fn value(identity: Option<ResourceIdentity>) -> Value {
        Value::Data {
            ty: "A".into(),
            constructor: "A".into(),
            fields: BTreeMap::new(),
            resource_identity: identity,
        }
    }

    #[test]
    fn zero_limits_and_total_mint_tombstones() {
        let mut empty = ResourceTraceEngine::new(
            ResourceTraceConfig::new(
                &world(),
                &[],
                ResourceTraceLimits {
                    max_selected_types: 0,
                    max_name_bytes: 0,
                    max_selected_name_bytes: 0,
                    max_resources: 0,
                    max_events: 0,
                    max_value_nodes: 0,
                    max_value_depth: 0,
                },
            )
            .unwrap(),
        )
        .unwrap();
        empty.begin_run().unwrap();
        empty.validate_values(&[]).unwrap();
        empty.finish(ResourceTraceCompletion::Completed).unwrap();
        assert_eq!(
            empty.observer().snapshot().unwrap().status(),
            ResourceTraceStatus::Completed
        );
        for (limits, reason) in [
            (
                ResourceTraceLimits {
                    max_resources: 0,
                    ..ResourceTraceLimits::DEFAULT
                },
                ResourceTraceFailure::ResourceLimit,
            ),
            (
                ResourceTraceLimits {
                    max_events: 0,
                    ..ResourceTraceLimits::DEFAULT
                },
                ResourceTraceFailure::EventLimit,
            ),
        ] {
            let mut engine = engine(limits);
            assert_eq!(engine.mint("A", "A", 0, 1..2).unwrap_err().reason(), reason);
            let report = engine.observer().snapshot().unwrap();
            assert!(report.events().is_empty());
            assert!(report.live().is_empty());
        }
        let mut engine = engine(ResourceTraceLimits {
            max_resources: 1,
            ..ResourceTraceLimits::DEFAULT
        });
        let identity = mint(&mut engine, "A");
        engine.consume_batch(&[consume(identity)]).unwrap();
        assert_eq!(
            engine.mint("A", "A", 0, 1..2).unwrap_err().reason(),
            ResourceTraceFailure::ResourceLimit
        );
        let report = engine.observer().snapshot().unwrap();
        assert_eq!(report.events().len(), 2);
        assert!(report.live().is_empty());
    }

    #[test]
    fn event_capacity_and_duplicate_batches_commit_nothing() {
        let mut engine = engine(ResourceTraceLimits {
            max_events: 3,
            ..ResourceTraceLimits::DEFAULT
        });
        let a = mint(&mut engine, "A");
        let b = mint(&mut engine, "A");
        assert_eq!(
            engine
                .consume_batch(&[consume(a), consume(b)])
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::EventLimit
        );
        let report = engine.observer().snapshot().unwrap();
        assert_eq!(report.events().len(), 2);
        assert_eq!(report.live().len(), 2);
        assert_eq!(report.status(), ResourceTraceStatus::TraceFailed);
        let mut duplicate = self::engine(ResourceTraceLimits::DEFAULT);
        let a = mint(&mut duplicate, "A");
        assert_eq!(
            duplicate
                .consume_batch(&[consume(a), consume(a)])
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::DuplicateIdentity
        );
        assert_eq!(duplicate.observer().snapshot().unwrap().live().len(), 1);
    }

    #[test]
    fn event_one_retains_actual_live_prefix_and_first_failure() {
        let mut engine = engine(ResourceTraceLimits {
            max_events: 1,
            ..ResourceTraceLimits::DEFAULT
        });
        let identity = mint(&mut engine, "A");
        assert_eq!(
            engine
                .consume_batch(&[consume(identity)])
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::EventLimit
        );
        engine.fail(ResourceTraceFailure::UnsupportedExternal, None);
        engine.finish(ResourceTraceCompletion::Completed).unwrap();
        let report = engine.observer().snapshot().unwrap();
        assert_eq!(
            report.failure().unwrap().reason(),
            ResourceTraceFailure::EventLimit
        );
        assert_eq!(report.events().len(), 1);
        assert_eq!(report.events()[0].action(), ResourceAction::Mint);
        assert_eq!(report.live()[0].identity(), identity);
        assert_eq!(report.selected_types(), ["A", "B"]);
    }

    #[test]
    fn identities_survive_value_clones_and_reject_actual_duplicate_carriers() {
        let mut engine = engine(ResourceTraceLimits::DEFAULT);
        let identity = mint(&mut engine, "A");
        let actual = value(Some(identity));
        engine
            .validate_values(std::slice::from_ref(&actual))
            .unwrap();
        let corrupted = Value::Tuple(vec![actual.clone(), actual]);
        assert_eq!(
            engine.validate_values(&[corrupted]).unwrap_err().reason(),
            ResourceTraceFailure::DuplicateIdentity
        );
        let report = engine.observer().snapshot().unwrap();
        assert_eq!(report.events().len(), 1);
        assert_eq!(report.live()[0].identity(), identity);
    }

    #[test]
    fn epochs_foreign_kinds_missing_and_dead_values_refuse() {
        let mut first = engine(ResourceTraceLimits::DEFAULT);
        let old = mint(&mut first, "A");
        let snapshot = first.observer().snapshot().unwrap();
        first.finish(ResourceTraceCompletion::Completed).unwrap();
        assert_eq!(first.observer().snapshot().unwrap().live().len(), 1);
        first.begin_run().unwrap();
        let current = mint(&mut first, "A");
        assert_eq!(old.instance(), current.instance());
        assert_ne!(old.run(), current.run());
        assert_eq!(old.serial(), current.serial());
        assert_eq!(
            first
                .validate_values(&[value(Some(old))])
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::ForeignRun
        );
        assert_eq!(snapshot.run(), old.run());
        assert_eq!(snapshot.events().len(), 1);
        for reason in [
            ResourceTraceFailure::ForeignInstance,
            ResourceTraceFailure::WrongKind,
            ResourceTraceFailure::MissingIdentity,
            ResourceTraceFailure::AlreadyConsumed,
        ] {
            let mut engine = engine(ResourceTraceLimits::DEFAULT);
            let identity = mint(&mut engine, "A");
            let carrier = match reason {
                ResourceTraceFailure::ForeignInstance => value(Some(old)),
                ResourceTraceFailure::WrongKind => Value::Data {
                    ty: "B".into(),
                    constructor: "B".into(),
                    fields: BTreeMap::new(),
                    resource_identity: Some(identity),
                },
                ResourceTraceFailure::MissingIdentity => value(None),
                ResourceTraceFailure::AlreadyConsumed => {
                    engine.consume_batch(&[consume(identity)]).unwrap();
                    value(Some(identity))
                }
                _ => unreachable!(),
            };
            assert_eq!(
                engine.validate_values(&[carrier]).unwrap_err().reason(),
                reason
            );
        }
    }

    #[test]
    fn actual_carrier_shape_and_iterative_traversal_limits() {
        let mut malformed = engine(ResourceTraceLimits::DEFAULT);
        let identity = mint(&mut malformed, "A");
        let carrier = Value::Data {
            ty: "Other".into(),
            constructor: "Other".into(),
            fields: BTreeMap::new(),
            resource_identity: Some(identity),
        };
        assert_eq!(
            malformed.validate_values(&[carrier]).unwrap_err().reason(),
            ResourceTraceFailure::WrongKind
        );
        for (limits, carrier, reason) in [
            (
                ResourceTraceLimits {
                    max_value_nodes: 1,
                    ..ResourceTraceLimits::DEFAULT
                },
                Value::Tuple(vec![Value::Int(1)]),
                ResourceTraceFailure::ValueNodeLimit,
            ),
            (
                ResourceTraceLimits {
                    max_value_depth: 1,
                    ..ResourceTraceLimits::DEFAULT
                },
                Value::Tuple(vec![Value::Int(1)]),
                ResourceTraceFailure::ValueDepthLimit,
            ),
            (
                ResourceTraceLimits::DEFAULT,
                Value::Function("f".into()),
                ResourceTraceFailure::UnsupportedValue,
            ),
        ] {
            let mut engine = engine(limits);
            assert_eq!(
                engine.validate_values(&[carrier]).unwrap_err().reason(),
                reason
            );
        }
        let mut exact = engine(ResourceTraceLimits {
            max_value_nodes: 2,
            max_value_depth: 2,
            ..ResourceTraceLimits::DEFAULT
        });
        exact
            .validate_values(&[Value::Tuple(vec![Value::Int(1)])])
            .unwrap();
    }

    #[test]
    fn configuration_is_private_bounded_and_revalidated_against_actual_world() {
        let limits = ResourceTraceLimits::DEFAULT;
        assert_eq!(
            ResourceTraceConfig::new(&world(), &["A", "A"], limits)
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::DuplicateSelection
        );
        assert_eq!(
            ResourceTraceConfig::new(&world(), &["Missing"], limits)
                .unwrap_err()
                .reason(),
            ResourceTraceFailure::UnknownSelectedType
        );
        assert_eq!(
            ResourceTraceConfig::new(
                &world(),
                &["A"],
                ResourceTraceLimits {
                    max_selected_name_bytes: 0,
                    ..limits
                }
            )
            .unwrap_err()
            .reason(),
            ResourceTraceFailure::NameBytesLimit
        );
        let config = ResourceTraceConfig::new(&world(), &["A"], limits).unwrap();
        let mut other = world();
        other.adts.get_mut("A").unwrap().mode = Mode::Copy;
        assert_eq!(
            config.validate_world(&other).unwrap_err().reason(),
            ResourceTraceFailure::UnsupportedSelectedType
        );
    }

    #[test]
    fn nonce_exhaustion_borrow_failures_and_drop_never_wrap_or_panic() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_instance(&counter).unwrap(), u64::MAX - 1);
        assert_eq!(
            next_instance(&counter).unwrap_err().reason(),
            ResourceTraceFailure::InstanceExhausted
        );
        let mut run = engine(ResourceTraceLimits::DEFAULT);
        run.finish(ResourceTraceCompletion::Completed).unwrap();
        run.shared.state.borrow_mut().run = u64::MAX;
        assert_eq!(
            run.begin_run().unwrap_err().reason(),
            ResourceTraceFailure::RunExhausted
        );
        let mut serial = engine(ResourceTraceLimits::DEFAULT);
        serial.shared.state.borrow_mut().next_serial = u64::MAX;
        assert_eq!(
            serial.mint("A", "A", 0, 1..2).unwrap_err().reason(),
            ResourceTraceFailure::SerialExhausted
        );
        assert!(serial.observer().snapshot().unwrap().events().is_empty());
        let mut borrowed = engine(ResourceTraceLimits::DEFAULT);
        let shared = borrowed.shared.clone();
        let guard = shared.state.borrow();
        assert_eq!(
            borrowed.mint("A", "A", 0, 1..2).unwrap_err().reason(),
            ResourceTraceFailure::BorrowConflict
        );
        assert_eq!(
            borrowed
                .observer()
                .snapshot()
                .unwrap()
                .failure()
                .unwrap()
                .reason(),
            ResourceTraceFailure::BorrowConflict
        );
        drop(guard);
        let engine = engine(ResourceTraceLimits::DEFAULT);
        let observer = engine.observer();
        let shared = engine.shared.clone();
        let guard = shared.state.borrow_mut();
        assert_eq!(
            observer.snapshot().unwrap_err().reason(),
            ResourceTraceFailure::BorrowConflict
        );
        drop(engine);
        drop(guard);
        assert_eq!(
            observer.snapshot().unwrap().status(),
            ResourceTraceStatus::Abandoned
        );
    }
}
