//! Endpoint projection with full branch mergeability (design §25.5).
//!
//! `project(chor, role)` maps the global tree onto one role's local session
//! type:
//!
//! * a message `A -> B: L(T)` is `Send` for `A`, `Recv` for `B`, and
//!   invisible to everyone else;
//! * a choice `A -> B { .. }` is `Select` for `A` and `Offer` for `B`; any
//!   other role's branch projections must **merge**;
//! * a local action is `Action` for its role only;
//! * `rec X` projects to `rec X` unless the role does nothing in the loop
//!   body, in which case it projects to the role's behaviour after it.
//!
//! Merging (the "full merge" of multiparty session types): identical types
//! merge to themselves; two receptions (`Recv`/`Offer`) from the **same peer**
//! merge into one `Offer` with the union of their labels, merging the
//! continuations of shared labels (whose payload types must agree); types
//! with an identical first action (same send, same selection labels, same
//! local action, same `rec`) merge their continuations. Anything else is
//! rejected with [`ProjectionError::Unmergeable`], naming the role, the choice
//! it was not told about, the branch path, and the two conflicting behaviours.
//!
//! The merge is the syntactic condition from the literature; the runtime
//! evidence that it is adequate here is the exhaustive asynchronous
//! exploration in [`crate::check`], not a proof.

use crate::chor::{Choreography, RoleId};
use std::fmt;

/// One labelled branch of a local `Select`/`Offer`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LBranch {
    pub label: String,
    pub payload: String,
    pub next: LocalType,
}

/// Projected per-role session type (`Session[P]`).
///
/// `Select` and `Offer` always hold at least two branches sorted by label;
/// one-branch forms are `Send` and `Recv`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LocalType {
    Send {
        peer: RoleId,
        label: String,
        payload: String,
        next: Box<LocalType>,
    },
    Recv {
        peer: RoleId,
        label: String,
        payload: String,
        next: Box<LocalType>,
    },
    Select {
        peer: RoleId,
        branches: Vec<LBranch>,
    },
    Offer {
        peer: RoleId,
        branches: Vec<LBranch>,
    },
    Action {
        name: String,
        next: Box<LocalType>,
    },
    Rec {
        var: String,
        body: Box<LocalType>,
    },
    Var(String),
    End,
}

/// `role` is not told which branch `decider` chose (communicated only to
/// `notified`), and its behaviour differs between the branches on `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmergeable {
    pub role: String,
    pub decider: String,
    pub notified: String,
    /// Choices taken from the root to the conflicting choice, as
    /// `A->B.Label`.
    pub path: Vec<String>,
    pub left_label: String,
    pub right_label: String,
    pub left: String,
    pub right: String,
}

/// Two branches deliver the same label from the same peer with different
/// payload types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadMismatch {
    pub role: String,
    pub peer: String,
    pub label: String,
    pub left: String,
    pub right: String,
}

/// Why a role cannot be projected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    UnknownRole(String),
    Unmergeable(Box<Unmergeable>),
    PayloadMismatch(Box<PayloadMismatch>),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRole(r) => write!(f, "role `{r}` is not declared"),
            Self::Unmergeable(u) => {
                write!(
                    f,
                    "role `{}` cannot tell whether `{}` chose `{}` or `{}` (only `{}` is told)",
                    u.role, u.decider, u.left_label, u.right_label, u.notified
                )?;
                if !u.path.is_empty() {
                    write!(f, " after {}", u.path.join(", "))?;
                }
                write!(
                    f,
                    ": it does `{}` in one branch and `{}` in the other",
                    u.left, u.right
                )
            }
            Self::PayloadMismatch(p) => write!(
                f,
                "role `{}` receives `{}` from `{}` with payload `{}` in one branch and `{}` in another",
                p.role, p.label, p.peer, p.left, p.right
            ),
        }
    }
}

impl std::error::Error for ProjectionError {}

/// Project `chor` onto the role named `role`.
pub fn project(chor: &Choreography, role: &str) -> Result<LocalType, ProjectionError> {
    let id = chor
        .role_id(role)
        .ok_or_else(|| ProjectionError::UnknownRole(role.to_owned()))?;
    project_role(chor, id)
}

/// Project every role, in declaration order.
///
/// Projection and branch merging are the Vesper library's
/// (`vesper/src/project.ves`, port wave P3b lane 6, #1068).
pub fn project_all(chor: &Choreography) -> Result<Vec<LocalType>, ProjectionError> {
    crate::port::project(chor, None)
}

pub fn project_role(chor: &Choreography, target: RoleId) -> Result<LocalType, ProjectionError> {
    if chor.role_name(target).is_none() {
        return Err(ProjectionError::UnknownRole(format!("#{}", target.0)));
    }
    crate::port::project(chor, Some(target)).map(|mut v| v.pop().unwrap_or(LocalType::End))
}

impl LocalType {
    /// Canonical constructor: one branch is `Send`/`Recv`; more are sorted
    /// `Select`/`Offer`.
    pub fn choice(send: bool, peer: RoleId, mut branches: Vec<LBranch>) -> LocalType {
        if branches.len() == 1 {
            let b = branches.pop().expect("one branch");
            let next = Box::new(b.next);
            return if send {
                LocalType::Send {
                    peer,
                    label: b.label,
                    payload: b.payload,
                    next,
                }
            } else {
                LocalType::Recv {
                    peer,
                    label: b.label,
                    payload: b.payload,
                    next,
                }
            };
        }
        branches.sort_by(|x, y| x.label.cmp(&y.label));
        if send {
            LocalType::Select { peer, branches }
        } else {
            LocalType::Offer { peer, branches }
        }
    }

    /// Whether `var` occurs free.
    pub fn mentions(&self, var: &str) -> bool {
        match self {
            LocalType::Var(v) => v == var,
            LocalType::End => false,
            LocalType::Send { next, .. }
            | LocalType::Recv { next, .. }
            | LocalType::Action { next, .. } => next.mentions(var),
            LocalType::Select { branches, .. } | LocalType::Offer { branches, .. } => {
                branches.iter().any(|b| b.next.mentions(var))
            }
            LocalType::Rec { var: v, body } => v != var && body.mentions(var),
        }
    }

    /// Substitute `with` for free occurrences of `var`.
    pub fn subst(&self, var: &str, with: &LocalType) -> LocalType {
        match self {
            LocalType::Var(v) if v == var => with.clone(),
            LocalType::Var(_) | LocalType::End => self.clone(),
            LocalType::Send {
                peer,
                label,
                payload,
                next,
            } => LocalType::Send {
                peer: *peer,
                label: label.clone(),
                payload: payload.clone(),
                next: Box::new(next.subst(var, with)),
            },
            LocalType::Recv {
                peer,
                label,
                payload,
                next,
            } => LocalType::Recv {
                peer: *peer,
                label: label.clone(),
                payload: payload.clone(),
                next: Box::new(next.subst(var, with)),
            },
            LocalType::Action { name, next } => LocalType::Action {
                name: name.clone(),
                next: Box::new(next.subst(var, with)),
            },
            LocalType::Select { peer, branches } | LocalType::Offer { peer, branches } => {
                let branches = branches
                    .iter()
                    .map(|b| LBranch {
                        label: b.label.clone(),
                        payload: b.payload.clone(),
                        next: b.next.subst(var, with),
                    })
                    .collect();
                if matches!(self, LocalType::Select { .. }) {
                    LocalType::Select {
                        peer: *peer,
                        branches,
                    }
                } else {
                    LocalType::Offer {
                        peer: *peer,
                        branches,
                    }
                }
            }
            LocalType::Rec { var: v, body } => {
                if v == var {
                    self.clone()
                } else {
                    LocalType::Rec {
                        var: v.clone(),
                        body: Box::new(body.subst(var, with)),
                    }
                }
            }
        }
    }

    /// Unfold leading `rec` binders: `rec X. T` becomes `T[X := rec X. T]`.
    /// Guardedness (checked on the global tree) bounds the loop; a defensive
    /// cap returns the type unchanged if it is exceeded.
    pub fn unfold(&self) -> LocalType {
        let mut t = self.clone();
        for _ in 0..64 {
            match t {
                LocalType::Rec { ref var, ref body } => {
                    let next = body.subst(var, &t);
                    t = next;
                }
                _ => return t,
            }
        }
        t
    }

    /// Alpha-equivalence: equal up to the names of recursion variables.
    pub fn alpha_eq(&self, other: &LocalType) -> bool {
        self.de_bruijn(&mut Vec::new()) == other.de_bruijn(&mut Vec::new())
    }

    fn de_bruijn(&self, env: &mut Vec<String>) -> LocalType {
        match self {
            LocalType::Var(v) => match env.iter().rev().position(|e| e == v) {
                Some(i) => LocalType::Var(format!("#{i}")),
                None => LocalType::Var(format!("free:{v}")),
            },
            LocalType::Rec { var, body } => {
                env.push(var.clone());
                let b = body.de_bruijn(env);
                env.pop();
                LocalType::Rec {
                    var: String::new(),
                    body: Box::new(b),
                }
            }
            LocalType::End => LocalType::End,
            LocalType::Send {
                peer,
                label,
                payload,
                next,
            } => LocalType::Send {
                peer: *peer,
                label: label.clone(),
                payload: payload.clone(),
                next: Box::new(next.de_bruijn(env)),
            },
            LocalType::Recv {
                peer,
                label,
                payload,
                next,
            } => LocalType::Recv {
                peer: *peer,
                label: label.clone(),
                payload: payload.clone(),
                next: Box::new(next.de_bruijn(env)),
            },
            LocalType::Action { name, next } => LocalType::Action {
                name: name.clone(),
                next: Box::new(next.de_bruijn(env)),
            },
            LocalType::Select { peer, branches } | LocalType::Offer { peer, branches } => {
                let branches = branches
                    .iter()
                    .map(|b| LBranch {
                        label: b.label.clone(),
                        payload: b.payload.clone(),
                        next: b.next.de_bruijn(env),
                    })
                    .collect();
                if matches!(self, LocalType::Select { .. }) {
                    LocalType::Select {
                        peer: *peer,
                        branches,
                    }
                } else {
                    LocalType::Offer {
                        peer: *peer,
                        branches,
                    }
                }
            }
        }
    }

    /// One-line description of the first action, used in error messages.
    pub fn head_summary(&self, chor: &Choreography) -> String {
        let n = |r: &RoleId| chor.role_name(*r).unwrap_or("?").to_owned();
        let labels = |bs: &[LBranch]| {
            bs.iter()
                .map(|b| b.label.as_str())
                .collect::<Vec<_>>()
                .join("|")
        };
        match self {
            LocalType::Send { peer, label, .. } => format!("send {label} to {}", n(peer)),
            LocalType::Recv { peer, label, .. } => format!("receive {label} from {}", n(peer)),
            LocalType::Select { peer, branches } => {
                format!("select {} with {}", labels(branches), n(peer))
            }
            LocalType::Offer { peer, branches } => {
                format!("offer {} from {}", labels(branches), n(peer))
            }
            LocalType::Action { name, .. } => format!("local action {name}"),
            LocalType::Rec { var, .. } => format!("loop {var}"),
            LocalType::Var(v) => format!("continue {v}"),
            LocalType::End => "end".to_owned(),
        }
    }

    /// Render in the textual local-type syntax, using role names.
    pub fn render(&self, chor: &Choreography) -> String {
        let mut s = String::new();
        self.render_into(chor, &mut s);
        s
    }

    fn render_into(&self, chor: &Choreography, s: &mut String) {
        let n = |r: &RoleId| chor.role_name(*r).unwrap_or("?").to_owned();
        let branches = |kw: &str, peer: &RoleId, bs: &[LBranch], s: &mut String| {
            s.push_str(&format!("{kw} {} {{ ", n(peer)));
            for (i, b) in bs.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format!("{}({}): ", b.label, b.payload));
                b.next.render_into(chor, s);
            }
            s.push_str(" }");
        };
        match self {
            LocalType::Send {
                peer,
                label,
                payload,
                next,
            } => {
                s.push_str(&format!("{}!{label}({payload}); ", n(peer)));
                next.render_into(chor, s);
            }
            LocalType::Recv {
                peer,
                label,
                payload,
                next,
            } => {
                s.push_str(&format!("{}?{label}({payload}); ", n(peer)));
                next.render_into(chor, s);
            }
            LocalType::Select { peer, branches: bs } => branches("select", peer, bs, s),
            LocalType::Offer { peer, branches: bs } => branches("offer", peer, bs, s),
            LocalType::Action { name, next } => {
                s.push_str(&format!("do {name}; "));
                next.render_into(chor, s);
            }
            LocalType::Rec { var, body } => {
                s.push_str(&format!("rec {var} {{ "));
                body.render_into(chor, s);
                s.push_str(" }");
            }
            LocalType::Var(v) => s.push_str(&format!("continue {v}")),
            LocalType::End => s.push_str("end"),
        }
    }
}

/// Binary duality: `s_a` (role `a`) and `s_b` (role `b`) are exact duals
/// when every send of one is the matching receive of the other, every
/// selection is offered with the same labels, and both end together.
/// Recursion is compared structurally (both must loop at the same points).
pub fn are_dual(a: RoleId, s_a: &LocalType, b: RoleId, s_b: &LocalType) -> bool {
    crate::port::are_dual(a, s_a, b, s_b)
}
