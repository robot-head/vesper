//! Linear `Session<P>` typestate channels (design §25.2).
//!
//! A protocol is a Rust type built from [`Send`], [`Recv`], [`Select`],
//! [`Offer`], [`Act`], [`Rec`], [`Var`] and [`End`]. Each operation consumes
//! the session and returns it at the next protocol state, so out-of-order
//! use does not compile. Binding a session ([`Session::bind`]) also checks,
//! at run time, that the Rust protocol type is alpha-equivalent to the
//! choreography's projection for that role, and every frame is checked again
//! by a dynamic [`Monitor`] on both sides, so a misbehaving peer that does
//! not use these types is rejected at the receiving boundary.
//!
//! Linearity: `Session`, [`RawSession`] and [`Fault`] are non-`Clone`,
//! `#[must_use]` owners. Their only exits are `close` (at `End`),
//! [`Fault::into_error`] (after a transport failure), and the explicit
//! [`RawSession::fault`] recovery exit. Dropping one any other way is
//! recorded by the [`LeakAuditor`] and the peer is told the channel was
//! abandoned. This is host-library discipline enforced by Rust ownership plus
//! run-time auditing; it is not an implementation of the Vesper source
//! typing rules for `Session[P]`.

use crate::chor::{Choreography, RoleId};
use crate::project::{LBranch, LocalType, ProjectionError, project};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex};

// ---------------------------------------------------------------- errors

/// Transport-level failure (`NetFail`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetFail {
    /// Nothing to receive yet (non-blocking transports only).
    WouldBlock,
    /// The peer dropped or faulted its end of this channel.
    PeerGone,
    /// The network was shut down.
    Closed,
    Timeout,
    Crashed,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChorusError {
    Projection(ProjectionError),
    UnknownRole(String),
    /// The Rust protocol type does not match the projected local type.
    TypestateMismatch {
        role: String,
        projected: String,
        rust_type: String,
    },
    /// A frame or call does not match the projected local type.
    ProtocolViolation {
        role: String,
        expected: String,
        found: String,
    },
    /// A frame arrived on the wrong channel or between the wrong roles.
    Misrouted {
        channel: u64,
    },
    Decode {
        label: String,
        reason: String,
    },
    Net(NetFail),
}

impl fmt::Display for ChorusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(e) => write!(f, "{e}"),
            Self::UnknownRole(r) => write!(f, "role `{r}` is not declared"),
            Self::TypestateMismatch {
                role,
                projected,
                rust_type,
            } => write!(
                f,
                "session type for `{role}` is `{rust_type}` but the choreography projects `{projected}`"
            ),
            Self::ProtocolViolation {
                role,
                expected,
                found,
            } => write!(f, "`{role}` expected {expected}, found {found}"),
            Self::Misrouted { channel } => write!(f, "frame misrouted on channel {channel}"),
            Self::Decode { label, reason } => write!(f, "cannot decode `{label}`: {reason}"),
            Self::Net(n) => write!(f, "network failure: {n:?}"),
        }
    }
}

impl std::error::Error for ChorusError {}

impl From<ProjectionError> for ChorusError {
    fn from(e: ProjectionError) -> Self {
        ChorusError::Projection(e)
    }
}

// ---------------------------------------------------------------- frames, messages

/// One message on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub channel: u64,
    pub from: RoleId,
    pub to: RoleId,
    /// Per-(channel, from, to) sequence number starting at 0.
    pub seq: u64,
    pub label: String,
    pub payload_ty: String,
    pub body: Vec<u8>,
}

/// A message type that a choreography label carries.
pub trait Message: Sized {
    const LABEL: &'static str;
    /// Payload type name as written in the choreography (empty for `L()`).
    const PAYLOAD: &'static str;
    fn encode(&self, out: &mut Vec<u8>);
    fn decode(bytes: &[u8]) -> Result<Self, String>;
}

/// Type-level role name.
pub trait RoleName {
    const NAME: &'static str;
}

/// Type-level local action name.
pub trait ActionName {
    const NAME: &'static str;
}

/// Declare a zero-sized role marker: `role!(Buyer = "B");`.
#[macro_export]
macro_rules! role {
    ($t:ident = $n:literal) => {
        pub struct $t;
        impl $crate::session::RoleName for $t {
            const NAME: &'static str = $n;
        }
    };
}

/// Declare a payload-free message: `unit_message!(Quit = "Quit");`.
#[macro_export]
macro_rules! unit_message {
    ($t:ident = $l:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub struct $t;
        impl $crate::session::Message for $t {
            const LABEL: &'static str = $l;
            const PAYLOAD: &'static str = "";
            fn encode(&self, _out: &mut Vec<u8>) {}
            fn decode(bytes: &[u8]) -> Result<Self, String> {
                if bytes.is_empty() {
                    Ok($t)
                } else {
                    Err(format!("{} trailing bytes", bytes.len()))
                }
            }
        }
    };
}

/// Declare a message carrying one `u64` (big-endian, exactly 8 bytes).
#[macro_export]
macro_rules! u64_message {
    ($t:ident = $l:literal, $ty:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $t(pub u64);
        impl $crate::session::Message for $t {
            const LABEL: &'static str = $l;
            const PAYLOAD: &'static str = $ty;
            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.0.to_be_bytes());
            }
            fn decode(bytes: &[u8]) -> Result<Self, String> {
                let arr: [u8; 8] = bytes
                    .try_into()
                    .map_err(|_| format!("expected 8 bytes, got {}", bytes.len()))?;
                Ok($t(u64::from_be_bytes(arr)))
            }
        }
    };
}

/// Declare a message carrying a UTF-8 string of at most 4096 bytes.
#[macro_export]
macro_rules! string_message {
    ($t:ident = $l:literal, $ty:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $t(pub String);
        impl $crate::session::Message for $t {
            const LABEL: &'static str = $l;
            const PAYLOAD: &'static str = $ty;
            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(self.0.as_bytes());
            }
            fn decode(bytes: &[u8]) -> Result<Self, String> {
                if bytes.len() > 4096 {
                    return Err(format!("{} bytes exceeds 4096", bytes.len()));
                }
                String::from_utf8(bytes.to_vec())
                    .map($t)
                    .map_err(|e| e.to_string())
            }
        }
    };
}

// ---------------------------------------------------------------- protocol markers

pub struct Send<To, M, Next>(PhantomData<(To, M, Next)>);
pub struct Recv<From, M, Next>(PhantomData<(From, M, Next)>);
/// One labelled branch: message `M`, then protocol `P`.
pub struct Branch<M, P>(PhantomData<(M, P)>);
/// Binary internal choice towards `To`.
pub struct Select<To, L, R>(PhantomData<(To, L, R)>);
/// Binary external choice offered by `From`.
pub struct Offer<From, L, R>(PhantomData<(From, L, R)>);
/// Local action `A`, then `Next`.
pub struct Act<A, Next>(PhantomData<(A, Next)>);
/// `rec` binder (de Bruijn).
pub struct Rec<P>(PhantomData<P>);
/// Reference to the `N`-th enclosing `Rec` (`Z` is the innermost).
pub struct Var<N>(PhantomData<N>);
pub struct Z;
pub struct S<N>(PhantomData<N>);
pub struct End;

pub trait Nat {
    const VALUE: usize;
}
impl Nat for Z {
    const VALUE: usize = 0;
}
impl<N: Nat> Nat for S<N> {
    const VALUE: usize = N::VALUE + 1;
}

/// Context for describing a protocol type as a [`LocalType`].
pub struct Describe<'a> {
    chor: &'a Choreography,
    depth: usize,
}

impl Describe<'_> {
    fn role(&self, name: &str) -> Result<RoleId, ChorusError> {
        self.chor
            .role_id(name)
            .ok_or_else(|| ChorusError::UnknownRole(name.to_owned()))
    }
}

/// Every protocol marker type describes itself as a local session type.
pub trait Protocol {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError>;
}

/// A branch of `Select`/`Offer`.
pub trait BranchSpec {
    fn describe_branch(cx: &mut Describe<'_>) -> Result<LBranch, ChorusError>;
}

impl<M: Message, P: Protocol> BranchSpec for Branch<M, P> {
    fn describe_branch(cx: &mut Describe<'_>) -> Result<LBranch, ChorusError> {
        Ok(LBranch {
            label: M::LABEL.to_owned(),
            payload: M::PAYLOAD.to_owned(),
            next: P::describe(cx)?,
        })
    }
}

impl Protocol for End {
    fn describe(_: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        Ok(LocalType::End)
    }
}
impl<To: RoleName, M: Message, N: Protocol> Protocol for Send<To, M, N> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        Ok(LocalType::Send {
            peer: cx.role(To::NAME)?,
            label: M::LABEL.to_owned(),
            payload: M::PAYLOAD.to_owned(),
            next: Box::new(N::describe(cx)?),
        })
    }
}
impl<F: RoleName, M: Message, N: Protocol> Protocol for Recv<F, M, N> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        Ok(LocalType::Recv {
            peer: cx.role(F::NAME)?,
            label: M::LABEL.to_owned(),
            payload: M::PAYLOAD.to_owned(),
            next: Box::new(N::describe(cx)?),
        })
    }
}
impl<To: RoleName, L: BranchSpec, R: BranchSpec> Protocol for Select<To, L, R> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        let peer = cx.role(To::NAME)?;
        let branches = vec![L::describe_branch(cx)?, R::describe_branch(cx)?];
        Ok(LocalType::choice(true, peer, branches))
    }
}
impl<F: RoleName, L: BranchSpec, R: BranchSpec> Protocol for Offer<F, L, R> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        let peer = cx.role(F::NAME)?;
        let branches = vec![L::describe_branch(cx)?, R::describe_branch(cx)?];
        Ok(LocalType::choice(false, peer, branches))
    }
}
impl<A: ActionName, N: Protocol> Protocol for Act<A, N> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        Ok(LocalType::Action {
            name: A::NAME.to_owned(),
            next: Box::new(N::describe(cx)?),
        })
    }
}
impl<P: Protocol> Protocol for Rec<P> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        let var = format!("#{}", cx.depth);
        cx.depth += 1;
        let body = P::describe(cx);
        cx.depth -= 1;
        Ok(LocalType::Rec {
            var,
            body: Box::new(body?),
        })
    }
}
impl<N: Nat> Protocol for Var<N> {
    fn describe(cx: &mut Describe<'_>) -> Result<LocalType, ChorusError> {
        match cx.depth.checked_sub(1 + N::VALUE) {
            Some(i) => Ok(LocalType::Var(format!("#{i}"))),
            None => Err(ChorusError::UnknownRole(format!(
                "Var<{}> outside of {} enclosing Rec",
                N::VALUE,
                cx.depth
            ))),
        }
    }
}

/// Describe a protocol type against a choreography's role table.
pub fn describe<P: Protocol>(chor: &Choreography) -> Result<LocalType, ChorusError> {
    P::describe(&mut Describe { chor, depth: 0 })
}

// ---------------------------------------------------------------- auditor

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeakRecord {
    pub channel: u64,
    pub role: String,
    /// The protocol state the owner was dropped in.
    pub state: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditCounts {
    pub opened: u64,
    pub closed: u64,
    pub faulted: u64,
    pub leaked: u64,
    /// Duplicate frames discarded by sequence number.
    pub duplicates_discarded: u64,
}

/// Counts every session owner's exit. A dropped owner is a counted leak.
#[derive(Clone, Default)]
pub struct LeakAuditor {
    inner: Arc<Mutex<(AuditCounts, Vec<LeakRecord>)>>,
}

impl LeakAuditor {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn counts(&self) -> AuditCounts {
        self.lock().0.clone()
    }
    pub fn leaks(&self) -> Vec<LeakRecord> {
        self.lock().1.clone()
    }
    /// Sessions opened but not yet closed, faulted or leaked.
    pub fn open(&self) -> u64 {
        let c = self.counts();
        c.opened - c.closed - c.faulted - c.leaked
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, (AuditCounts, Vec<LeakRecord>)> {
        // A poisoned auditor still holds valid counters.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn update(&self, f: impl FnOnce(&mut AuditCounts)) {
        f(&mut self.lock().0);
    }
    fn leak(&self, rec: LeakRecord) {
        let mut g = self.lock();
        g.0.leaked += 1;
        g.1.push(rec);
    }
}

// ---------------------------------------------------------------- monitor

/// Dynamic checker for one role's projected local type.
#[derive(Clone, Debug)]
pub struct Monitor {
    role: String,
    current: LocalType,
    roles: Vec<String>,
}

impl Monitor {
    pub fn new(chor: &Choreography, role: &str) -> Result<Self, ChorusError> {
        let t = project(chor, role)?;
        Ok(Self {
            role: role.to_owned(),
            current: t.unfold(),
            roles: chor.roles().to_vec(),
        })
    }

    pub fn current(&self) -> &LocalType {
        &self.current
    }

    pub fn is_end(&self) -> bool {
        self.current == LocalType::End
    }

    fn name(&self, r: RoleId) -> &str {
        self.roles.get(usize::from(r.0)).map_or("?", String::as_str)
    }

    pub fn role_id(&self, name: &str) -> Option<RoleId> {
        self.roles
            .iter()
            .position(|r| r == name)
            .map(|i| RoleId(i as u16))
    }

    fn expected(&self) -> String {
        match &self.current {
            LocalType::Send {
                peer,
                label,
                payload,
                ..
            } => format!("send {label}({payload}) to {}", self.name(*peer)),
            LocalType::Recv {
                peer,
                label,
                payload,
                ..
            } => format!("receive {label}({payload}) from {}", self.name(*peer)),
            LocalType::Select { peer, branches } | LocalType::Offer { peer, branches } => {
                let kw = if matches!(self.current, LocalType::Select { .. }) {
                    "select"
                } else {
                    "offer"
                };
                let ls: Vec<_> = branches.iter().map(|b| b.label.as_str()).collect();
                format!("{kw} {} with {}", ls.join("|"), self.name(*peer))
            }
            LocalType::Action { name, .. } => format!("local action {name}"),
            LocalType::End => "end".to_owned(),
            LocalType::Rec { .. } | LocalType::Var(_) => "unguarded recursion".to_owned(),
        }
    }

    fn violation(&self, found: String) -> ChorusError {
        ChorusError::ProtocolViolation {
            role: self.role.clone(),
            expected: self.expected(),
            found,
        }
    }

    fn step(
        &mut self,
        send: bool,
        peer: RoleId,
        label: &str,
        payload: &str,
    ) -> Result<(), ChorusError> {
        let next = match &self.current {
            LocalType::Send {
                peer: p,
                label: l,
                payload: t,
                next,
            } if send && *p == peer && l == label && t == payload => Some((**next).clone()),
            LocalType::Recv {
                peer: p,
                label: l,
                payload: t,
                next,
            } if !send && *p == peer && l == label && t == payload => Some((**next).clone()),
            LocalType::Select { peer: p, branches } if send && *p == peer => branches
                .iter()
                .find(|b| b.label == label && b.payload == payload)
                .map(|b| b.next.clone()),
            LocalType::Offer { peer: p, branches } if !send && *p == peer => branches
                .iter()
                .find(|b| b.label == label && b.payload == payload)
                .map(|b| b.next.clone()),
            _ => None,
        };
        match next {
            Some(n) => {
                self.current = n.unfold();
                Ok(())
            }
            None => Err(self.violation(format!(
                "{} {label}({payload}) {} {}",
                if send { "send" } else { "receive" },
                if send { "to" } else { "from" },
                self.name(peer)
            ))),
        }
    }

    pub fn on_send(&mut self, peer: RoleId, label: &str, payload: &str) -> Result<(), ChorusError> {
        self.step(true, peer, label, payload)
    }

    pub fn on_recv(&mut self, peer: RoleId, label: &str, payload: &str) -> Result<(), ChorusError> {
        self.step(false, peer, label, payload)
    }

    pub fn on_action(&mut self, name: &str) -> Result<(), ChorusError> {
        match &self.current {
            LocalType::Action { name: n, next } if n == name => {
                self.current = next.unfold();
                Ok(())
            }
            _ => Err(self.violation(format!("local action {name}"))),
        }
    }

    /// The role this monitor is waiting to receive from, if any.
    pub fn awaiting(&self) -> Option<RoleId> {
        match &self.current {
            LocalType::Recv { peer, .. } | LocalType::Offer { peer, .. } => Some(*peer),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- raw linear session

/// Why a [`RawSession`] was abandoned through its recovery exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultReason {
    Net(NetFail),
    /// The peer sent a frame that violates the projection.
    Violation,
    /// The owning node crashed (simulation).
    Crash,
    /// A higher-level protocol superseded the exchange (for example a newer
    /// Raft term).
    Superseded,
}

/// A monitored, transport-agnostic linear session owner. Callers move frames
/// themselves and report each one; the monitor rejects anything outside the
/// projection. Exits: [`close`](Self::close) at `End` or
/// [`fault`](Self::fault).
#[must_use = "a session must be closed at End or explicitly faulted"]
pub struct RawSession {
    channel: u64,
    monitor: Option<Box<Monitor>>,
    auditor: LeakAuditor,
}

impl RawSession {
    pub fn open(
        chor: &Choreography,
        role: &str,
        channel: u64,
        auditor: &LeakAuditor,
    ) -> Result<Self, ChorusError> {
        let monitor = Monitor::new(chor, role)?;
        auditor.update(|c| c.opened += 1);
        Ok(Self {
            channel,
            monitor: Some(Box::new(monitor)),
            auditor: auditor.clone(),
        })
    }

    pub fn channel(&self) -> u64 {
        self.channel
    }

    pub fn monitor(&self) -> &Monitor {
        self.monitor.as_ref().expect("live session")
    }

    fn m(&mut self) -> &mut Monitor {
        self.monitor.as_mut().expect("live session")
    }

    pub fn on_send(&mut self, peer: RoleId, label: &str, payload: &str) -> Result<(), ChorusError> {
        self.m().on_send(peer, label, payload)
    }

    pub fn on_recv(&mut self, peer: RoleId, label: &str, payload: &str) -> Result<(), ChorusError> {
        self.m().on_recv(peer, label, payload)
    }

    pub fn on_action(&mut self, name: &str) -> Result<(), ChorusError> {
        self.m().on_action(name)
    }

    pub fn is_end(&self) -> bool {
        self.monitor().is_end()
    }

    /// Close at `End`. Returns the session unchanged if it is not at `End`.
    pub fn close(mut self) -> Result<(), (ChorusError, RawSession)> {
        if !self.is_end() {
            let e = self.monitor().violation("close".to_owned());
            return Err((e, self));
        }
        self.monitor = None;
        self.auditor.update(|c| c.closed += 1);
        Ok(())
    }

    /// The recovery exit: abandon the exchange explicitly.
    pub fn fault(mut self, _reason: FaultReason) {
        self.monitor = None;
        self.auditor.update(|c| c.faulted += 1);
    }
}

impl Drop for RawSession {
    fn drop(&mut self) {
        if let Some(m) = self.monitor.take() {
            self.auditor.leak(LeakRecord {
                channel: self.channel,
                role: m.role.clone(),
                state: m.expected(),
            });
        }
    }
}

// ---------------------------------------------------------------- transports

/// Frame transport used by typed sessions. Implementations are keyed by
/// `(channel, from, to)`; ordering across different keys is not assumed.
pub trait Transport: std::marker::Send {
    fn send(&mut self, frame: Frame) -> Result<(), NetFail>;
    /// Next available frame for `(channel, from, to)`, if any. Frames may be
    /// returned out of sequence order; the endpoint reorders them.
    fn try_recv(
        &mut self,
        channel: u64,
        from: RoleId,
        to: RoleId,
    ) -> Result<Option<Frame>, NetFail>;
    /// Blocking receive. Non-blocking transports return `WouldBlock`.
    fn recv(&mut self, channel: u64, from: RoleId, to: RoleId) -> Result<Frame, NetFail> {
        match self.try_recv(channel, from, to)? {
            Some(f) => Ok(f),
            None => Err(NetFail::WouldBlock),
        }
    }
    /// The local end `me` of `channel` was dropped or faulted.
    fn abandon(&mut self, _channel: u64, _me: RoleId) {}
}

#[derive(Default)]
struct MemState {
    queues: HashMap<(u64, RoleId, RoleId), VecDeque<Frame>>,
    abandoned: HashSet<(u64, RoleId)>,
    closed: bool,
}

/// In-memory transport shared by threads. Blocking receives wait on a
/// condition variable without timeouts (no clock is read); they return
/// `PeerGone` when the sender abandoned the channel and `Closed` after
/// [`MemoryNet::shutdown`].
#[derive(Clone, Default)]
pub struct MemoryNet {
    inner: Arc<(Mutex<MemState>, Condvar)>,
}

impl MemoryNet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn handle(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
    /// Deliver an arbitrary frame (models a peer that does not use the
    /// typed API).
    pub fn inject(&self, frame: Frame) {
        let (m, cv) = &*self.inner;
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        g.queues
            .entry((frame.channel, frame.from, frame.to))
            .or_default()
            .push_back(frame);
        cv.notify_all();
    }
    pub fn shutdown(&self) {
        let (m, cv) = &*self.inner;
        m.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        cv.notify_all();
    }
    /// Frames sent but never received.
    pub fn undelivered(&self) -> usize {
        let (m, _) = &*self.inner;
        m.lock()
            .unwrap_or_else(|p| p.into_inner())
            .queues
            .values()
            .map(VecDeque::len)
            .sum()
    }
}

impl Transport for MemoryNet {
    fn send(&mut self, frame: Frame) -> Result<(), NetFail> {
        let (m, cv) = &*self.inner;
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        if g.closed {
            return Err(NetFail::Closed);
        }
        if g.abandoned.contains(&(frame.channel, frame.to)) {
            return Err(NetFail::PeerGone);
        }
        g.queues
            .entry((frame.channel, frame.from, frame.to))
            .or_default()
            .push_back(frame);
        cv.notify_all();
        Ok(())
    }
    fn try_recv(
        &mut self,
        channel: u64,
        from: RoleId,
        to: RoleId,
    ) -> Result<Option<Frame>, NetFail> {
        let (m, _) = &*self.inner;
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(f) = g
            .queues
            .get_mut(&(channel, from, to))
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(f));
        }
        if g.abandoned.contains(&(channel, from)) {
            return Err(NetFail::PeerGone);
        }
        if g.closed {
            return Err(NetFail::Closed);
        }
        Ok(None)
    }
    fn recv(&mut self, channel: u64, from: RoleId, to: RoleId) -> Result<Frame, NetFail> {
        let (m, cv) = &*self.inner;
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(f) = g
                .queues
                .get_mut(&(channel, from, to))
                .and_then(VecDeque::pop_front)
            {
                return Ok(f);
            }
            if g.abandoned.contains(&(channel, from)) {
                return Err(NetFail::PeerGone);
            }
            if g.closed {
                return Err(NetFail::Closed);
            }
            g = cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
    }
    fn abandon(&mut self, channel: u64, me: RoleId) {
        let (m, cv) = &*self.inner;
        m.lock()
            .unwrap_or_else(|p| p.into_inner())
            .abandoned
            .insert((channel, me));
        cv.notify_all();
    }
}

// ---------------------------------------------------------------- endpoint

struct Endpoint {
    channel: u64,
    me: RoleId,
    monitor: Monitor,
    transport: Box<dyn Transport>,
    send_seq: BTreeMap<RoleId, u64>,
    recv_seq: BTreeMap<RoleId, u64>,
    reorder: BTreeMap<(RoleId, u64), Frame>,
    auditor: LeakAuditor,
}

impl Endpoint {
    fn peer(&self, name: &str) -> Result<RoleId, ChorusError> {
        self.monitor
            .role_id(name)
            .ok_or_else(|| ChorusError::UnknownRole(name.to_owned()))
    }

    fn send<M: Message>(&mut self, to: &str, msg: &M) -> Result<(), ChorusError> {
        let peer = self.peer(to)?;
        self.monitor.on_send(peer, M::LABEL, M::PAYLOAD)?;
        let seq = self.send_seq.entry(peer).or_insert(0);
        let mut body = Vec::new();
        msg.encode(&mut body);
        let frame = Frame {
            channel: self.channel,
            from: self.me,
            to: peer,
            seq: *seq,
            label: M::LABEL.to_owned(),
            payload_ty: M::PAYLOAD.to_owned(),
            body,
        };
        *seq = seq
            .checked_add(1)
            .ok_or(ChorusError::Net(NetFail::Closed))?;
        self.transport.send(frame).map_err(ChorusError::Net)
    }

    /// Next in-sequence frame from `from`. `Ok(None)` only when non-blocking
    /// and nothing is ready.
    fn next_frame(&mut self, from: RoleId, blocking: bool) -> Result<Option<Frame>, ChorusError> {
        let want = *self.recv_seq.get(&from).unwrap_or(&0);
        loop {
            if let Some(f) = self.reorder.remove(&(from, want)) {
                self.recv_seq.insert(from, want + 1);
                return Ok(Some(f));
            }
            let got = if blocking {
                Some(self.transport.recv(self.channel, from, self.me))
            } else {
                match self.transport.try_recv(self.channel, from, self.me) {
                    Ok(Some(f)) => Some(Ok(f)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
            };
            let frame = match got {
                None => return Ok(None),
                Some(Err(e)) => return Err(ChorusError::Net(e)),
                Some(Ok(f)) => f,
            };
            if frame.channel != self.channel || frame.to != self.me || frame.from != from {
                return Err(ChorusError::Misrouted {
                    channel: frame.channel,
                });
            }
            if frame.seq < want || self.reorder.contains_key(&(from, frame.seq)) {
                self.auditor.update(|c| c.duplicates_discarded += 1);
                continue;
            }
            if frame.seq - want > 1024 {
                return Err(self
                    .monitor
                    .violation(format!("frame sequence {} far ahead of {want}", frame.seq)));
            }
            self.reorder.insert((from, frame.seq), frame);
        }
    }

    /// Receive one frame from `from` and check it against the monitor.
    fn recv_checked(&mut self, from: &str, blocking: bool) -> Result<Option<Frame>, ChorusError> {
        let peer = self.peer(from)?;
        match self.next_frame(peer, blocking)? {
            None => Ok(None),
            Some(f) => {
                self.monitor.on_recv(peer, &f.label, &f.payload_ty)?;
                Ok(Some(f))
            }
        }
    }
}

fn decode<M: Message>(f: &Frame) -> Result<M, ChorusError> {
    if f.label != M::LABEL || f.payload_ty != M::PAYLOAD {
        return Err(ChorusError::Decode {
            label: f.label.clone(),
            reason: format!("expected {}({})", M::LABEL, M::PAYLOAD),
        });
    }
    M::decode(&f.body).map_err(|reason| ChorusError::Decode {
        label: f.label.clone(),
        reason,
    })
}

// ---------------------------------------------------------------- fault owner

/// A session that failed. Its only exit is [`Fault::into_error`], which
/// hands the error to the recovery handler.
#[must_use = "a faulted session must be handed to a recovery handler"]
pub struct Fault {
    error: ChorusError,
    armed: Option<Box<(LeakAuditor, LeakRecord)>>,
}

impl Fault {
    pub fn error(&self) -> &ChorusError {
        &self.error
    }
    pub fn into_error(mut self) -> ChorusError {
        if let Some(armed) = self.armed.take() {
            armed.0.update(|c| c.faulted += 1);
        }
        self.error.clone()
    }
}

impl fmt::Debug for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fault({:?})", self.error)
    }
}

impl Drop for Fault {
    fn drop(&mut self) {
        if let Some(armed) = self.armed.take() {
            let (a, rec) = *armed;
            a.leak(rec);
        }
    }
}

// ---------------------------------------------------------------- typed session

/// Linear typestate channel at protocol state `P` (with recursion
/// environment `E`).
#[must_use = "a session must be driven to End and closed"]
pub struct Session<P, E = ()> {
    ep: Option<Endpoint>,
    _p: PhantomData<fn() -> (P, E)>,
}

/// Result of a non-blocking receive.
#[must_use]
pub enum TryRecv<Ok, S> {
    Ready(Ok),
    /// Nothing yet: the session comes back unchanged.
    Empty(S),
    Fault(Fault),
}

impl<P, E> Session<P, E> {
    fn wrap(ep: Endpoint) -> Self {
        Session {
            ep: Some(ep),
            _p: PhantomData,
        }
    }
    fn take(&mut self) -> Endpoint {
        self.ep.take().expect("live session")
    }
    fn advance<Q, F>(mut self) -> Session<Q, F> {
        Session::wrap(self.take())
    }
    pub fn channel(&self) -> u64 {
        self.ep.as_ref().map_or(0, |e| e.channel)
    }
    /// The dynamic monitor's view of the remaining protocol.
    pub fn remaining(&self) -> Option<&LocalType> {
        self.ep.as_ref().map(|e| e.monitor.current())
    }
}

fn fault(mut ep: Endpoint, error: ChorusError) -> Fault {
    ep.transport.abandon(ep.channel, ep.me);
    let rec = LeakRecord {
        channel: ep.channel,
        role: ep.monitor.role.clone(),
        state: ep.monitor.expected(),
    };
    Fault {
        error,
        armed: Some(Box::new((ep.auditor.clone(), rec))),
    }
}

impl<P: Protocol> Session<P, ()> {
    /// Bind `role`'s end of `channel`, checking that `P` is the projection of
    /// `chor` for `role`.
    pub fn bind(
        chor: &Choreography,
        role: &str,
        channel: u64,
        transport: Box<dyn Transport>,
        auditor: &LeakAuditor,
    ) -> Result<Self, ChorusError> {
        let me = chor
            .role_id(role)
            .ok_or_else(|| ChorusError::UnknownRole(role.to_owned()))?;
        let projected = project(chor, role)?;
        let rust = describe::<P>(chor)?;
        if !projected.alpha_eq(&rust) {
            return Err(ChorusError::TypestateMismatch {
                role: role.to_owned(),
                projected: projected.render(chor),
                rust_type: rust.render(chor),
            });
        }
        let monitor = Monitor::new(chor, role)?;
        auditor.update(|c| c.opened += 1);
        Ok(Session::wrap(Endpoint {
            channel,
            me,
            monitor,
            transport,
            send_seq: BTreeMap::new(),
            recv_seq: BTreeMap::new(),
            reorder: BTreeMap::new(),
            auditor: auditor.clone(),
        }))
    }
}

impl<To: RoleName, M: Message, N, E> Session<Send<To, M, N>, E> {
    pub fn send(mut self, msg: &M) -> Result<Session<N, E>, Fault> {
        let mut ep = self.take();
        match ep.send(To::NAME, msg) {
            Ok(()) => Ok(Session::wrap(ep)),
            Err(e) => Err(fault(ep, e)),
        }
    }
}

impl<F: RoleName, M: Message, N, E> Session<Recv<F, M, N>, E> {
    pub fn recv(mut self) -> Result<(M, Session<N, E>), Fault> {
        let mut ep = self.take();
        let r = ep
            .recv_checked(F::NAME, true)
            .and_then(|f| decode::<M>(&f.expect("blocking receive returns a frame")));
        match r {
            Ok(m) => Ok((m, Session::wrap(ep))),
            Err(e) => Err(fault(ep, e)),
        }
    }

    pub fn try_recv(mut self) -> TryRecv<(M, Session<N, E>), Self> {
        let mut ep = self.take();
        match ep.recv_checked(F::NAME, false) {
            Ok(None) => TryRecv::Empty(Session::wrap(ep)),
            Ok(Some(f)) => match decode::<M>(&f) {
                Ok(m) => TryRecv::Ready((m, Session::wrap(ep))),
                Err(e) => TryRecv::Fault(fault(ep, e)),
            },
            Err(e) => TryRecv::Fault(fault(ep, e)),
        }
    }
}

impl<To: RoleName, ML: Message, PL, MR: Message, PR, E>
    Session<Select<To, Branch<ML, PL>, Branch<MR, PR>>, E>
{
    pub fn select_left(mut self, msg: &ML) -> Result<Session<PL, E>, Fault> {
        let mut ep = self.take();
        match ep.send(To::NAME, msg) {
            Ok(()) => Ok(Session::wrap(ep)),
            Err(e) => Err(fault(ep, e)),
        }
    }
    pub fn select_right(mut self, msg: &MR) -> Result<Session<PR, E>, Fault> {
        let mut ep = self.take();
        match ep.send(To::NAME, msg) {
            Ok(()) => Ok(Session::wrap(ep)),
            Err(e) => Err(fault(ep, e)),
        }
    }
}

/// The branch a peer selected.
pub enum Offered<ML, PL, MR, PR, E> {
    Left(ML, Session<PL, E>),
    Right(MR, Session<PR, E>),
}

impl<F: RoleName, ML: Message, PL, MR: Message, PR, E>
    Session<Offer<F, Branch<ML, PL>, Branch<MR, PR>>, E>
{
    fn branch(ep: Endpoint, f: Frame) -> Result<Offered<ML, PL, MR, PR, E>, Fault> {
        if f.label == ML::LABEL {
            match decode::<ML>(&f) {
                Ok(m) => Ok(Offered::Left(m, Session::wrap(ep))),
                Err(e) => Err(fault(ep, e)),
            }
        } else {
            match decode::<MR>(&f) {
                Ok(m) => Ok(Offered::Right(m, Session::wrap(ep))),
                Err(e) => Err(fault(ep, e)),
            }
        }
    }

    pub fn offer(mut self) -> Result<Offered<ML, PL, MR, PR, E>, Fault> {
        let mut ep = self.take();
        match ep.recv_checked(F::NAME, true) {
            Ok(f) => Self::branch(ep, f.expect("blocking receive returns a frame")),
            Err(e) => Err(fault(ep, e)),
        }
    }

    pub fn try_offer(mut self) -> TryRecv<Offered<ML, PL, MR, PR, E>, Self> {
        let mut ep = self.take();
        match ep.recv_checked(F::NAME, false) {
            Ok(None) => TryRecv::Empty(Session::wrap(ep)),
            Ok(Some(f)) => match Self::branch(ep, f) {
                Ok(o) => TryRecv::Ready(o),
                Err(fl) => TryRecv::Fault(fl),
            },
            Err(e) => TryRecv::Fault(fault(ep, e)),
        }
    }
}

impl<A: ActionName, N, E> Session<Act<A, N>, E> {
    /// Record that the local action ran.
    pub fn act(mut self) -> Session<N, E> {
        let mut ep = self.take();
        ep.monitor
            .on_action(A::NAME)
            .expect("bind checked the action against the projection");
        Session::wrap(ep)
    }
}

impl<P, E> Session<Rec<P>, E> {
    pub fn enter(self) -> Session<P, (P, E)> {
        self.advance()
    }
}

impl<P, E> Session<Var<Z>, (P, E)> {
    /// Jump back to the innermost enclosing `Rec`.
    pub fn repeat(self) -> Session<P, (P, E)> {
        self.advance()
    }
}

impl<N, P, E> Session<Var<S<N>>, (P, E)> {
    /// Leave the innermost `Rec` scope.
    pub fn pop(self) -> Session<Var<N>, E> {
        self.advance()
    }
}

impl<E> Session<End, E> {
    pub fn close(mut self) {
        let ep = self.take();
        debug_assert!(ep.monitor.is_end());
        ep.auditor.update(|c| c.closed += 1);
    }
}

impl<P, E> Drop for Session<P, E> {
    fn drop(&mut self) {
        if let Some(mut ep) = self.ep.take() {
            ep.transport.abandon(ep.channel, ep.me);
            ep.auditor.leak(LeakRecord {
                channel: ep.channel,
                role: ep.monitor.role.clone(),
                state: ep.monitor.expected(),
            });
        }
    }
}
