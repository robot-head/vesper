//! `ObligationLineage` (§31.1 item 4): provenance of `Mode::Linear` obligations.
//!
//! Every obligation gets a monotonic id at its mint. Afterwards the lineage
//! records sink moves, field projections, cross-fiber handoffs and the single
//! terminal event (an unpack discharge or a reported leak). A query returns the
//! whole chain, including the chains of the obligations it was split from.
//!
//! This records what an instrumented program reports. It does not implement
//! the Vesper source typing rules for linear values.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::wire::ObligationRecord;

/// Monotonic obligation id. Ids start at 1; 0 is never minted.
pub type ObligationId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopKind {
    /// Creation. `parent` names the obligation this one was split or derived from.
    Mint { parent: Option<ObligationId> },
    /// Moved into a `sink` parameter.
    SinkMove,
    /// Handed to another fiber over a `LinearTx` channel.
    ChannelHandoff { to_fiber: u64 },
    /// Moved through a field projection of the place with this hash.
    Projection { place_hash: u64 },
    /// Terminal `[T-Match]` unpacking.
    UnpackDischarge,
    /// Terminal: reported leaked (scope exit without discharge, stall alert).
    Leak,
}

impl HopKind {
    /// Tags 0–3 are the design's `hop_kind_tag`; 4 and 5 are added here.
    pub fn tag(self) -> u8 {
        match self {
            Self::Mint { .. } => 0,
            Self::SinkMove => 1,
            Self::ChannelHandoff { .. } => 2,
            Self::UnpackDischarge => 3,
            Self::Projection { .. } => 4,
            Self::Leak => 5,
        }
    }
    fn aux(self) -> u64 {
        match self {
            Self::Mint { parent } => parent.unwrap_or(0),
            Self::ChannelHandoff { to_fiber } => to_fiber,
            Self::Projection { place_hash } => place_hash,
            _ => 0,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::UnpackDischarge | Self::Leak)
    }
    pub fn from_record(record: &ObligationRecord) -> Result<Self, LineageError> {
        let aux = record.aux;
        let zero = |kind: Self| {
            if aux == 0 {
                Ok(kind)
            } else {
                Err(LineageError::MalformedRecord {
                    reason: "nonzero aux on a hop kind without one",
                })
            }
        };
        match record.hop_kind_tag {
            0 => Ok(Self::Mint {
                parent: (aux != 0).then_some(aux),
            }),
            1 => zero(Self::SinkMove),
            2 => Ok(Self::ChannelHandoff { to_fiber: aux }),
            3 => zero(Self::UnpackDischarge),
            4 => Ok(Self::Projection { place_hash: aux }),
            5 => zero(Self::Leak),
            _ => Err(LineageError::MalformedRecord {
                reason: "unknown hop kind tag",
            }),
        }
    }
    pub fn to_record(self, obligation_id: ObligationId, source_span_id: u32) -> ObligationRecord {
        ObligationRecord {
            obligation_id,
            aux: self.aux(),
            source_span_id,
            hop_kind_tag: self.tag(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObligationHop {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub source_span_id: u32,
    pub kind: HopKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationStatus {
    Live,
    Discharged,
    Leaked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageError {
    UnknownObligation {
        obligation_id: ObligationId,
    },
    /// The log was truncated (ring overwrite) and this obligation's mint was
    /// among the evicted records; its chain cannot be answered completely.
    CreationEvicted {
        obligation_id: ObligationId,
    },
    /// Ids must be minted as 1, 2, 3, ... in log order.
    NonMonotonicId {
        expected: ObligationId,
        got: ObligationId,
    },
    DoubleMint {
        obligation_id: ObligationId,
    },
    /// A hop after the terminal discharge or leak.
    AfterTerminal {
        obligation_id: ObligationId,
    },
    UnknownParent {
        parent: ObligationId,
    },
    OutOfOrder {
        previous: u64,
        got: u64,
    },
    IdExhausted,
    MalformedRecord {
        reason: &'static str,
    },
}

impl fmt::Display for LineageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "obligation lineage: {self:?}")
    }
}
impl std::error::Error for LineageError {}

/// The chain of one obligation and of every ancestor it was derived from,
/// oldest ancestor first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub chains: Vec<(ObligationId, Vec<ObligationHop>)>,
    /// Set when the oldest ancestor's mint was evicted from a truncated log,
    /// so the chain is known to be incomplete at its start.
    pub evicted_ancestor: Option<ObligationId>,
}

impl Provenance {
    /// All hops, ancestors first.
    pub fn hops(&self) -> impl Iterator<Item = (ObligationId, &ObligationHop)> {
        self.chains
            .iter()
            .flat_map(|(id, hops)| hops.iter().map(move |hop| (*id, hop)))
    }
}

#[derive(Debug, Default, Clone)]
pub struct ObligationLineage {
    next_id: ObligationId,
    chains: BTreeMap<ObligationId, Vec<ObligationHop>>,
    /// Ids seen without their mint because the log prefix was evicted.
    truncated: BTreeSet<ObligationId>,
    allow_truncated_prefix: bool,
    /// First id minted inside the retained window.
    base: Option<ObligationId>,
    last_seq: Option<u64>,
}

impl ObligationLineage {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }
    /// For a log whose oldest records were overwritten: hops of obligations
    /// minted before the retained window, i.e. with ids below the first
    /// retained mint, are accepted but those ids answer
    /// [`LineageError::CreationEvicted`].
    pub fn for_truncated_log() -> Self {
        Self {
            allow_truncated_prefix: true,
            ..Self::new()
        }
    }

    fn check_order(&self, seq: u64) -> Result<(), LineageError> {
        match self.last_seq {
            Some(previous) if seq <= previous => {
                Err(LineageError::OutOfOrder { previous, got: seq })
            }
            _ => Ok(()),
        }
    }

    /// Mint the next obligation id with a creation record.
    pub fn mint(
        &mut self,
        lamport_seq: u64,
        fiber_id: u64,
        source_span_id: u32,
        parent: Option<ObligationId>,
    ) -> Result<ObligationId, LineageError> {
        let id = match (self.base, self.allow_truncated_prefix) {
            (None, true) => self.truncated.last().map_or(1, |max| max + 1),
            _ => self.next_id.max(1),
        };
        self.record(
            id,
            ObligationHop {
                lamport_seq,
                fiber_id,
                source_span_id,
                kind: HopKind::Mint { parent },
            },
        )?;
        Ok(id)
    }

    /// Append one hop, enforcing order, monotonic mint ids and a single terminal.
    pub fn record(&mut self, id: ObligationId, hop: ObligationHop) -> Result<(), LineageError> {
        self.check_order(hop.lamport_seq)?;
        if id == 0 {
            return Err(LineageError::UnknownObligation { obligation_id: 0 });
        }
        if let HopKind::Mint { parent } = hop.kind {
            if self.chains.contains_key(&id) || self.truncated.contains(&id) {
                return Err(LineageError::DoubleMint { obligation_id: id });
            }
            let expected = self.next_mint_id(id);
            if id != expected {
                return Err(LineageError::NonMonotonicId { expected, got: id });
            }
            if let Some(parent) = parent
                && (parent >= id
                    || !(self.chains.contains_key(&parent) || self.is_pre_window(parent)))
            {
                return Err(LineageError::UnknownParent { parent });
            }
            self.base.get_or_insert(id);
            self.next_id = id.checked_add(1).ok_or(LineageError::IdExhausted)?;
            self.chains.insert(id, vec![hop]);
        } else {
            let pre_window = self.is_pre_window(id);
            match self.chains.get_mut(&id) {
                Some(chain) => {
                    if chain.last().is_some_and(|h| h.kind.is_terminal()) {
                        return Err(LineageError::AfterTerminal { obligation_id: id });
                    }
                    chain.push(hop);
                }
                None if pre_window => {
                    self.truncated.insert(id);
                }
                None => return Err(LineageError::UnknownObligation { obligation_id: id }),
            }
        }
        self.last_seq = Some(hop.lamport_seq);
        Ok(())
    }

    /// The id the next mint must carry. In a truncated log the first retained
    /// mint fixes the window base (`proposed`), which must exceed every id
    /// already seen without its mint.
    fn next_mint_id(&self, proposed: ObligationId) -> ObligationId {
        match (self.base, self.allow_truncated_prefix) {
            (None, true) => match self.truncated.last() {
                Some(max) if proposed <= *max => max + 1,
                _ => proposed,
            },
            _ => self.next_id.max(1),
        }
    }

    /// Whether `id` was minted before the retained window of a truncated log.
    /// Before the first retained mint every unknown id qualifies: its mint
    /// precedes this record and is not in the window.
    fn is_pre_window(&self, id: ObligationId) -> bool {
        self.allow_truncated_prefix && self.base.is_none_or(|base| id < base)
    }

    pub fn status(&self, id: ObligationId) -> Result<ObligationStatus, LineageError> {
        let chain = self.chain(id)?;
        Ok(match chain.last().map(|h| h.kind) {
            Some(HopKind::UnpackDischarge) => ObligationStatus::Discharged,
            Some(HopKind::Leak) => ObligationStatus::Leaked,
            _ => ObligationStatus::Live,
        })
    }

    /// This obligation's own chain, starting with its mint.
    pub fn chain(&self, id: ObligationId) -> Result<&[ObligationHop], LineageError> {
        if self.truncated.contains(&id) {
            return Err(LineageError::CreationEvicted { obligation_id: id });
        }
        self.chains
            .get(&id)
            .map(Vec::as_slice)
            .ok_or(LineageError::UnknownObligation { obligation_id: id })
    }

    /// "Where did this obligation come from": its chain plus every ancestor's.
    pub fn provenance(&self, id: ObligationId) -> Result<Provenance, LineageError> {
        let mut chains = Vec::new();
        let mut evicted_ancestor = None;
        let mut current = Some(id);
        while let Some(ancestor) = current {
            if ancestor != id
                && !self.chains.contains_key(&ancestor)
                && self.is_pre_window(ancestor)
            {
                evicted_ancestor = Some(ancestor);
                break;
            }
            let chain = self.chain(ancestor)?;
            current = match chain.first().map(|h| h.kind) {
                Some(HopKind::Mint { parent }) => parent,
                _ => None,
            };
            chains.push((ancestor, chain.to_vec()));
        }
        chains.reverse();
        Ok(Provenance {
            chains,
            evicted_ancestor,
        })
    }

    /// Obligations with no terminal record: at the end of a complete run these
    /// are leaks or stalls.
    pub fn open_obligations(&self) -> Vec<ObligationId> {
        self.chains
            .iter()
            .filter(|(_, chain)| !chain.last().is_some_and(|h| h.kind.is_terminal()))
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn leaked(&self) -> Vec<ObligationId> {
        self.chains
            .iter()
            .filter(|(_, chain)| chain.last().is_some_and(|h| h.kind == HopKind::Leak))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Ids whose mint was evicted from a truncated log.
    pub fn truncated(&self) -> impl Iterator<Item = ObligationId> + '_ {
        self.truncated.iter().copied()
    }

    /// Runtime check of the property `lean/ChronoReplay.lean` proves for the
    /// model: every discharged or leaked obligation that is answerable has a
    /// mint as the first hop of its chain.
    pub fn audit_complete(&self) -> bool {
        self.chains.values().all(|chain| {
            matches!(chain.first().map(|h| h.kind), Some(HopKind::Mint { .. }))
                && chain
                    .iter()
                    .skip(1)
                    .all(|h| !matches!(h.kind, HopKind::Mint { .. }))
                && chain.iter().rev().skip(1).all(|h| !h.kind.is_terminal())
        })
    }

    pub fn len(&self) -> usize {
        self.chains.len()
    }
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty()
    }
}
