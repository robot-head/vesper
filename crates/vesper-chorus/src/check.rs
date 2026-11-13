//! Bounded exhaustive execution of projected endpoints.
//!
//! Runs all projected roles of a choreography together under asynchronous
//! semantics (one FIFO queue per ordered pair of roles; sends never block)
//! and explores every interleaving and every internal choice up to a state
//! budget. It reports a reachable deadlock (some role waits forever), a
//! reception whose queue head carries a label the role does not accept, or
//! messages left in a queue when every role has ended (orphans).
//!
//! This is executable evidence for a particular choreography under the
//! stated bounds, not a proof of the projection theorem.

use crate::chor::{Choreography, RoleId};
use crate::project::{LocalType, ProjectionError, project_all};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;

#[derive(Clone, Copy, Debug)]
pub struct ExploreLimits {
    /// Maximum distinct global states visited.
    pub max_states: usize,
    /// Maximum messages queued on one directed link; deeper sends are not
    /// explored and the run is reported as bounded.
    pub max_queue: usize,
}

impl Default for ExploreLimits {
    fn default() -> Self {
        Self {
            max_states: 200_000,
            max_queue: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreReport {
    pub states: usize,
    /// Number of terminal states where every role reached `End`.
    pub completed: usize,
    /// Whether some send was cut off by `max_queue`.
    pub queue_bound_hit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExploreError {
    Projection(ProjectionError),
    /// No role can move, yet not all have ended. `waiting` lists each blocked
    /// role and the first action it is waiting for.
    Deadlock {
        waiting: Vec<(String, String)>,
    },
    /// `role` expected one of `expected` from `peer` but the queue head was
    /// `found`.
    UnexpectedMessage {
        role: String,
        peer: String,
        expected: Vec<String>,
        found: String,
    },
    /// All roles ended with undelivered messages.
    Orphans {
        messages: Vec<(String, String, String)>,
    },
    StateBudgetExceeded {
        states: usize,
    },
}

impl fmt::Display for ExploreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ExploreError {}

#[derive(Clone, PartialEq, Eq, Hash)]
struct State {
    roles: Vec<LocalType>,
    queues: BTreeMap<(RoleId, RoleId), VecDeque<(String, String)>>,
}

/// Explore every reachable state of the projected system.
pub fn explore(chor: &Choreography, limits: ExploreLimits) -> Result<ExploreReport, ExploreError> {
    let locals = project_all(chor).map_err(ExploreError::Projection)?;
    explore_locals(chor, locals, limits)
}

/// Explore an explicit set of local types (one per role, in declaration
/// order). Used to show that hand-written, non-projected endpoints can
/// deadlock.
pub fn explore_locals(
    chor: &Choreography,
    locals: Vec<LocalType>,
    limits: ExploreLimits,
) -> Result<ExploreReport, ExploreError> {
    let name = |r: RoleId| chor.role_name(r).unwrap_or("?").to_owned();
    let start = State {
        roles: locals.iter().map(LocalType::unfold).collect(),
        queues: BTreeMap::new(),
    };
    let mut seen: HashSet<State> = HashSet::new();
    let mut work = vec![start.clone()];
    seen.insert(start);
    let mut report = ExploreReport {
        states: 0,
        completed: 0,
        queue_bound_hit: false,
    };
    while let Some(st) = work.pop() {
        report.states += 1;
        if report.states > limits.max_states {
            return Err(ExploreError::StateBudgetExceeded {
                states: report.states,
            });
        }
        let mut succ: Vec<State> = Vec::new();
        let mut cut = false;
        for (i, t) in st.roles.iter().enumerate() {
            let me = RoleId(i as u16);
            match t {
                LocalType::End | LocalType::Var(_) | LocalType::Rec { .. } => {}
                LocalType::Action { next, .. } => {
                    let mut s = st.clone();
                    s.roles[i] = next.unfold();
                    succ.push(s);
                }
                LocalType::Send {
                    peer,
                    label,
                    payload,
                    next,
                } => {
                    let q = st.queues.get(&(me, *peer)).map_or(0, VecDeque::len);
                    if q >= limits.max_queue {
                        cut = true;
                        continue;
                    }
                    let mut s = st.clone();
                    s.queues
                        .entry((me, *peer))
                        .or_default()
                        .push_back((label.clone(), payload.clone()));
                    s.roles[i] = next.unfold();
                    succ.push(s);
                }
                LocalType::Select { peer, branches } => {
                    let q = st.queues.get(&(me, *peer)).map_or(0, VecDeque::len);
                    if q >= limits.max_queue {
                        cut = true;
                        continue;
                    }
                    for b in branches {
                        let mut s = st.clone();
                        s.queues
                            .entry((me, *peer))
                            .or_default()
                            .push_back((b.label.clone(), b.payload.clone()));
                        s.roles[i] = b.next.unfold();
                        succ.push(s);
                    }
                }
                LocalType::Recv { peer, .. } | LocalType::Offer { peer, .. } => {
                    let Some(head) = st.queues.get(&(*peer, me)).and_then(|q| q.front()) else {
                        continue;
                    };
                    let accepted: Vec<(&str, &str, &LocalType)> = match t {
                        LocalType::Recv {
                            label,
                            payload,
                            next,
                            ..
                        } => vec![(label, payload, next)],
                        LocalType::Offer { branches, .. } => branches
                            .iter()
                            .map(|b| (b.label.as_str(), b.payload.as_str(), &b.next))
                            .collect(),
                        _ => unreachable!(),
                    };
                    match accepted
                        .iter()
                        .find(|(l, p, _)| *l == head.0 && *p == head.1)
                    {
                        Some((_, _, next)) => {
                            let mut s = st.clone();
                            let q = s.queues.get_mut(&(*peer, me)).expect("queue exists");
                            q.pop_front();
                            if q.is_empty() {
                                s.queues.remove(&(*peer, me));
                            }
                            s.roles[i] = next.unfold();
                            succ.push(s);
                        }
                        None => {
                            return Err(ExploreError::UnexpectedMessage {
                                role: name(me),
                                peer: name(*peer),
                                expected: accepted
                                    .iter()
                                    .map(|(l, p, _)| format!("{l}({p})"))
                                    .collect(),
                                found: format!("{}({})", head.0, head.1),
                            });
                        }
                    }
                }
            }
        }
        report.queue_bound_hit |= cut;
        if succ.is_empty() && !cut {
            let all_end = st.roles.iter().all(|t| *t == LocalType::End);
            if all_end && st.queues.is_empty() {
                report.completed += 1;
            } else if all_end {
                return Err(ExploreError::Orphans {
                    messages: st
                        .queues
                        .iter()
                        .flat_map(|((f, t), q)| {
                            q.iter().map(move |(l, _)| (name(*f), name(*t), l.clone()))
                        })
                        .collect(),
                });
            } else {
                return Err(ExploreError::Deadlock {
                    waiting: st
                        .roles
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| **t != LocalType::End)
                        .map(|(i, t)| (name(RoleId(i as u16)), t.head_summary(chor)))
                        .collect(),
                });
            }
        }
        for s in succ {
            if seen.insert(s.clone()) {
                work.push(s);
            }
        }
    }
    Ok(report)
}
