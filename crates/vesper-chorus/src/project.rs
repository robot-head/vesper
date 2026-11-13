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

use crate::chor::{Choreography, GlobalChor, RoleId};
use std::collections::BTreeMap;
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
pub fn project_all(chor: &Choreography) -> Result<Vec<LocalType>, ProjectionError> {
    (0..chor.roles().len())
        .map(|i| project_role(chor, RoleId(i as u16)))
        .collect()
}

pub fn project_role(chor: &Choreography, target: RoleId) -> Result<LocalType, ProjectionError> {
    if chor.role_name(target).is_none() {
        return Err(ProjectionError::UnknownRole(format!("#{}", target.0)));
    }
    let mut p = Projector {
        chor,
        target,
        path: Vec::new(),
    };
    p.go(chor.body())
}

struct Projector<'a> {
    chor: &'a Choreography,
    target: RoleId,
    path: Vec<String>,
}

impl Projector<'_> {
    fn name(&self, r: RoleId) -> String {
        self.chor.role_name(r).unwrap_or("?").to_owned()
    }

    fn go(&mut self, g: &GlobalChor) -> Result<LocalType, ProjectionError> {
        match g {
            GlobalChor::End => Ok(LocalType::End),
            GlobalChor::Var(v) => Ok(LocalType::Var(v.clone())),
            GlobalChor::Rec { var, body } => {
                // A role absent from a loop that can only repeat itself or
                // finish has nothing to do in it: it ends (standard rule).
                if !body.involves(self.target) && body.free_vars_within(&[var.as_str()]) {
                    return Ok(LocalType::End);
                }
                let b = self.go(body)?;
                Ok(match b {
                    LocalType::Var(ref v) if v == var => LocalType::End,
                    b if !b.mentions(var) => b,
                    b => LocalType::Rec {
                        var: var.clone(),
                        body: Box::new(b),
                    },
                })
            }
            GlobalChor::Local { role, action, next } => {
                let n = self.go(next)?;
                Ok(if *role == self.target {
                    LocalType::Action {
                        name: action.clone(),
                        next: Box::new(n),
                    }
                } else {
                    n
                })
            }
            GlobalChor::Comm { from, to, branches } => {
                let mut projected = Vec::with_capacity(branches.len());
                let choice = branches.len() > 1;
                for b in branches {
                    if choice {
                        self.path.push(format!(
                            "{}->{}.{}",
                            self.name(*from),
                            self.name(*to),
                            b.label
                        ));
                    }
                    let r = self.go(&b.next);
                    if choice {
                        self.path.pop();
                    }
                    projected.push(LBranch {
                        label: b.label.clone(),
                        payload: b.payload.clone(),
                        next: r?,
                    });
                }
                if *from == self.target {
                    Ok(LocalType::choice(true, *to, projected))
                } else if *to == self.target {
                    Ok(LocalType::choice(false, *from, projected))
                } else {
                    let mut iter = projected.into_iter();
                    let first = iter.next().expect("well-formed choice has a branch");
                    let mut acc = first.next;
                    let mut acc_label = first.label;
                    for b in iter {
                        acc = merge(self.chor, acc, b.next).map_err(|e| match e {
                            MergeFail::Conflict(l, r) => {
                                ProjectionError::Unmergeable(Box::new(Unmergeable {
                                    role: self.name(self.target),
                                    decider: self.name(*from),
                                    notified: self.name(*to),
                                    path: self.path.clone(),
                                    left_label: acc_label.clone(),
                                    right_label: b.label.clone(),
                                    left: l,
                                    right: r,
                                }))
                            }
                            MergeFail::Payload {
                                peer,
                                label,
                                left,
                                right,
                            } => ProjectionError::PayloadMismatch(Box::new(PayloadMismatch {
                                role: self.name(self.target),
                                peer: self.name(peer),
                                label,
                                left,
                                right,
                            })),
                        })?;
                        acc_label = b.label;
                    }
                    Ok(acc)
                }
            }
        }
    }
}

enum MergeFail {
    Conflict(String, String),
    Payload {
        peer: RoleId,
        label: String,
        left: String,
        right: String,
    },
}

/// View a reception as an `Offer` branch list.
fn as_offer(t: LocalType) -> Result<(RoleId, Vec<LBranch>), LocalType> {
    match t {
        LocalType::Recv {
            peer,
            label,
            payload,
            next,
        } => Ok((
            peer,
            vec![LBranch {
                label,
                payload,
                next: *next,
            }],
        )),
        LocalType::Offer { peer, branches } => Ok((peer, branches)),
        other => Err(other),
    }
}

fn merge(chor: &Choreography, a: LocalType, b: LocalType) -> Result<LocalType, MergeFail> {
    if a == b || a.alpha_eq(&b) {
        return Ok(a);
    }
    let summary = |t: &LocalType| t.head_summary(chor);
    // Receptions from the same peer: union of labels.
    let (a, b) = match (as_offer(a), as_offer(b)) {
        (Ok((pa, ba)), Ok((pb, bb))) if pa == pb => {
            let mut map: BTreeMap<String, LBranch> = BTreeMap::new();
            for br in ba {
                map.insert(br.label.clone(), br);
            }
            for br in bb {
                match map.remove(&br.label) {
                    None => {
                        map.insert(br.label.clone(), br);
                    }
                    Some(existing) => {
                        if existing.payload != br.payload {
                            return Err(MergeFail::Payload {
                                peer: pa,
                                label: br.label,
                                left: existing.payload,
                                right: br.payload,
                            });
                        }
                        let next = merge(chor, existing.next, br.next)?;
                        map.insert(
                            br.label.clone(),
                            LBranch {
                                label: br.label,
                                payload: br.payload,
                                next,
                            },
                        );
                    }
                }
            }
            return Ok(LocalType::choice(false, pa, map.into_values().collect()));
        }
        (a, b) => (unview(a), unview(b)),
    };
    match (a, b) {
        (
            LocalType::Send {
                peer: p1,
                label: l1,
                payload: t1,
                next: n1,
            },
            LocalType::Send {
                peer: p2,
                label: l2,
                payload: t2,
                next: n2,
            },
        ) if p1 == p2 && l1 == l2 && t1 == t2 => Ok(LocalType::Send {
            peer: p1,
            label: l1,
            payload: t1,
            next: Box::new(merge(chor, *n1, *n2)?),
        }),
        (
            LocalType::Select {
                peer: p1,
                branches: b1,
            },
            LocalType::Select {
                peer: p2,
                branches: b2,
            },
        ) if p1 == p2
            && b1.len() == b2.len()
            && b1
                .iter()
                .zip(&b2)
                .all(|(x, y)| x.label == y.label && x.payload == y.payload) =>
        {
            let mut out = Vec::with_capacity(b1.len());
            for (x, y) in b1.into_iter().zip(b2) {
                out.push(LBranch {
                    label: x.label,
                    payload: x.payload,
                    next: merge(chor, x.next, y.next)?,
                });
            }
            Ok(LocalType::Select {
                peer: p1,
                branches: out,
            })
        }
        (LocalType::Action { name: a1, next: n1 }, LocalType::Action { name: a2, next: n2 })
            if a1 == a2 =>
        {
            Ok(LocalType::Action {
                name: a1,
                next: Box::new(merge(chor, *n1, *n2)?),
            })
        }
        // Loops merge up to the names of their binders: rename the right
        // binder to the left one when that captures nothing.
        (LocalType::Rec { var: v1, body: b1 }, LocalType::Rec { var: v2, body: b2 })
            if v1 == v2 || !b2.mentions(&v1) =>
        {
            let b2 = if v1 == v2 {
                *b2
            } else {
                b2.subst(&v2, &LocalType::Var(v1.clone()))
            };
            Ok(LocalType::Rec {
                var: v1,
                body: Box::new(merge(chor, *b1, b2)?),
            })
        }
        (a, b) => Err(MergeFail::Conflict(summary(&a), summary(&b))),
    }
}

fn unview(r: Result<(RoleId, Vec<LBranch>), LocalType>) -> LocalType {
    match r {
        Ok((peer, branches)) => LocalType::choice(false, peer, branches),
        Err(t) => t,
    }
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
    dual_in(a, s_a, b, s_b, &mut Vec::new())
}

fn dual_in(
    a: RoleId,
    s_a: &LocalType,
    b: RoleId,
    s_b: &LocalType,
    vars: &mut Vec<(String, String)>,
) -> bool {
    use LocalType as L;
    match (s_a, s_b) {
        (L::End, L::End) => true,
        (L::Var(x), L::Var(y)) => {
            vars.iter().rev().find(|(p, _)| p == x).map(|(_, q)| q) == Some(y)
        }
        (L::Rec { var: x, body: bx }, L::Rec { var: y, body: by }) => {
            vars.push((x.clone(), y.clone()));
            let r = dual_in(a, bx, b, by, vars);
            vars.pop();
            r
        }
        (
            L::Send {
                peer: p1,
                label: l1,
                payload: t1,
                next: n1,
            },
            L::Recv {
                peer: p2,
                label: l2,
                payload: t2,
                next: n2,
            },
        )
        | (
            L::Recv {
                peer: p1,
                label: l1,
                payload: t1,
                next: n1,
            },
            L::Send {
                peer: p2,
                label: l2,
                payload: t2,
                next: n2,
            },
        ) => *p1 == b && *p2 == a && l1 == l2 && t1 == t2 && dual_in(a, n1, b, n2, vars),
        (
            L::Select {
                peer: p1,
                branches: b1,
            },
            L::Offer {
                peer: p2,
                branches: b2,
            },
        )
        | (
            L::Offer {
                peer: p1,
                branches: b1,
            },
            L::Select {
                peer: p2,
                branches: b2,
            },
        ) => {
            *p1 == b
                && *p2 == a
                && b1.len() == b2.len()
                && b1.iter().zip(b2).all(|(x, y)| {
                    x.label == y.label
                        && x.payload == y.payload
                        && dual_in(a, &x.next, b, &y.next, vars)
                })
        }
        // Local actions are invisible to the peer.
        (L::Action { next, .. }, other) => dual_in(a, next, b, other, vars),
        (other, L::Action { next, .. }) => dual_in(a, other, b, next, vars),
        _ => false,
    }
}
