//! Pure Lens presentation primitives (§21): obligation-guided completion
//! ranking, proof-lens status bookkeeping and request cancellation.
//!
//! Nothing here computes compiler facts. Callers supply checked receiver
//! modes, effect rows and proof outcomes; this module only orders, bounds and
//! records them. A ranking or a ledger entry is presentation, never evidence
//! that a program is well typed or verified.
use crate::Id;
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Completion order. Lower tiers sort first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompletionTier {
    /// A `sink self` method on a receiver that is a live linear obligation.
    DischargesLiveLinear = 0,
    /// A method that is valid in the current frame but discharges nothing.
    ValidFrameCompatible = 1,
    /// A method whose abortable effect would discard a live linear value.
    BlockedByFrame = 2,
}
impl CompletionTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DischargesLiveLinear => "discharges_live_linear",
            Self::ValidFrameCompatible => "valid_frame_compatible",
            Self::BlockedByFrame => "blocked_by_frame",
        }
    }
}

/// How a method receives its receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverPass {
    In,
    Inout,
    Sink,
}

/// One method candidate, with the facts the caller already checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateMethod {
    pub name: String,
    pub receiver: ReceiverPass,
    /// Names of the method's potentially abortable (non-linear) effects.
    pub abortable_effects: Vec<String>,
    /// Caller-rendered signature text.
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedCompletion {
    pub tier: CompletionTier,
    pub label: String,
    pub detail: String,
    pub badge: Option<String>,
}

/// Rank member completions for a receiver.
///
/// `receiver_is_live_linear` says the receiver is an owned linear binding that
/// has not been consumed at the cursor. `other_live_linears` names the other
/// live linear bindings in scope. A method with an abortable effect is blocked
/// while any linear value would stay live across it: the other live bindings,
/// and the receiver itself unless the method consumes it.
pub fn rank_methods(
    receiver_is_live_linear: bool,
    other_live_linears: &[String],
    methods: &[CandidateMethod],
) -> Vec<RankedCompletion> {
    let mut ranked: Vec<_> = methods
        .iter()
        .map(|method| {
            let consumes = method.receiver == ReceiverPass::Sink;
            let mut stranded: Vec<&str> = other_live_linears.iter().map(String::as_str).collect();
            if receiver_is_live_linear && !consumes {
                stranded.insert(0, "the receiver");
            }
            if let (Some(effect), Some(first)) =
                (method.abortable_effects.first(), stranded.first())
            {
                return RankedCompletion {
                    tier: CompletionTier::BlockedByFrame,
                    label: method.name.clone(),
                    detail: method.detail.clone(),
                    badge: Some(format!(
                        "blocked: abortable effect {effect} while {first} is a live linear value"
                    )),
                };
            }
            if receiver_is_live_linear && consumes {
                RankedCompletion {
                    tier: CompletionTier::DischargesLiveLinear,
                    label: method.name.clone(),
                    detail: method.detail.clone(),
                    badge: Some("discharges linear obligation".into()),
                }
            } else {
                RankedCompletion {
                    tier: CompletionTier::ValidFrameCompatible,
                    label: method.name.clone(),
                    detail: method.detail.clone(),
                    badge: None,
                }
            }
        })
        .collect();
    ranked.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.label.cmp(&b.label)));
    ranked
}

/// The outcome shown by a proof code lens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofStatus {
    NotAttempted,
    Proved { detail: String },
    Failed { stage: String, reason: String },
}
impl ProofStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Proved { .. } => "proved",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Bounded record of proof outcomes keyed by exact source identity and
/// function name. A new source hash never inherits an older outcome.
#[derive(Debug)]
pub struct ProofLedger {
    entries: BTreeMap<([u8; 32], String), (u64, ProofStatus)>,
    max_entries: usize,
    clock: u64,
}
impl ProofLedger {
    /// `max_entries` must be between 1 and 4096.
    pub fn new(max_entries: usize) -> Result<Self, LensError> {
        if max_entries == 0 || max_entries > 4096 {
            return Err(LensError::InvalidLimits);
        }
        Ok(Self {
            entries: BTreeMap::new(),
            max_entries,
            clock: 0,
        })
    }
    pub fn status(&self, source: [u8; 32], function: &str) -> ProofStatus {
        self.entries
            .get(&(source, function.to_owned()))
            .map(|(_, status)| status.clone())
            .unwrap_or(ProofStatus::NotAttempted)
    }
    /// Record an outcome, evicting the oldest entry when full.
    pub fn record(&mut self, source: [u8; 32], function: &str, status: ProofStatus) {
        self.clock += 1;
        let key = (source, function.to_owned());
        if !self.entries.contains_key(&key) && self.entries.len() >= self.max_entries {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (stamp, _))| *stamp)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (self.clock, status));
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A cooperative cancellation flag shared with a running job.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// True when both handles belong to the same job.
    pub fn same_job(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Requests that have not been answered yet. Cancelling a pending request
/// removes it, so its eventual result is discarded and exactly one response
/// (the cancellation error) is sent for its ID.
#[derive(Debug)]
pub struct PendingRequests {
    pending: Vec<(Id, CancelToken)>,
    max_pending: usize,
}
impl PendingRequests {
    /// `max_pending` must be between 1 and 1024.
    pub fn new(max_pending: usize) -> Result<Self, LensError> {
        if max_pending == 0 || max_pending > 1024 {
            return Err(LensError::InvalidLimits);
        }
        Ok(Self {
            pending: Vec::new(),
            max_pending,
        })
    }
    pub fn begin(&mut self, id: Id) -> Result<CancelToken, LensError> {
        if self.pending.iter().any(|(pending, _)| *pending == id) {
            return Err(LensError::DuplicateRequest);
        }
        if self.pending.len() >= self.max_pending {
            return Err(LensError::TooManyPending);
        }
        let token = CancelToken::default();
        self.pending.push((id, token.clone()));
        Ok(token)
    }
    /// True when `id` was pending; its token is then cancelled.
    pub fn cancel(&mut self, id: &Id) -> bool {
        match self.take(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
    /// True when `id` is still pending *for this job* and should now be
    /// answered. A late completion of a cancelled job never matches a later
    /// request that reuses its ID.
    pub fn finish(&mut self, id: &Id, job: &CancelToken) -> bool {
        let Some(index) = self
            .pending
            .iter()
            .position(|(pending, token)| pending == id && token.same_job(job))
        else {
            return false;
        };
        self.pending.swap_remove(index);
        true
    }
    /// Cancel every pending request (server exit), returning their IDs.
    pub fn cancel_all(&mut self) -> Vec<Id> {
        self.pending
            .drain(..)
            .map(|(id, token)| {
                token.cancel();
                id
            })
            .collect()
    }
    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    fn take(&mut self, id: &Id) -> Option<CancelToken> {
        let index = self.pending.iter().position(|(pending, _)| pending == id)?;
        Some(self.pending.swap_remove(index).1)
    }
}

/// Parse `$/cancelRequest` parameters.
pub fn cancel_params(params: Option<&serde_json::Value>) -> Result<Id, LensError> {
    match params.and_then(|params| params.get("id")) {
        Some(serde_json::Value::String(id)) if id.len() <= 1024 => Ok(Id::String(id.clone())),
        Some(serde_json::Value::Number(id)) => id
            .as_i64()
            .and_then(|id| i32::try_from(id).ok())
            .map(Id::Integer)
            .ok_or(LensError::InvalidCancel),
        _ => Err(LensError::InvalidCancel),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensError {
    InvalidLimits,
    DuplicateRequest,
    TooManyPending,
    InvalidCancel,
}
impl fmt::Display for LensError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLimits => "invalid lens limits",
            Self::DuplicateRequest => "request ID is already pending",
            Self::TooManyPending => "too many pending requests",
            Self::InvalidCancel => "cancel parameters need an integer or string id",
        })
    }
}
impl std::error::Error for LensError {}
