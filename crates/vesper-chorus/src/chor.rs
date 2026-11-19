//! Global choreography AST and well-formedness checks (design §25.1, §25.5).
//!
//! A choreography names a fixed set of roles and describes one protocol from
//! a global point of view:
//!
//! * `A -> B: Label(T)` is a point-to-point message.
//! * `A -> B { L1(T1) { .. } L2(T2) { .. } }` is a choice made by `A` (the
//!   decider) and communicated to `B` by the label it sends. Every other role
//!   must either learn the choice from a later message or behave identically
//!   in every branch; projection checks this (see [`crate::project`]).
//! * `A: action;` is a local action of `A` that no other role observes.
//! * `rec X { .. }` and `continue X;` express guarded recursion.
//! * `end;` terminates the protocol explicitly.
//!
//! The textual syntax is parsed by [`crate::parse`]. This module only holds
//! the tree and the checks every tree must pass before projection.

use std::fmt;

/// Index of a role in [`Choreography::roles`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoleId(pub u16);

/// Hard limits that make malformed or hostile inputs a typed error.
pub const MAX_ROLES: usize = 64;
/// Upper bound on the number of AST nodes after sequencing is expanded.
pub const MAX_NODES: usize = 100_000;
/// Upper bound on nesting depth of the global tree (each sequenced step
/// nests once, so this also bounds protocol length along one path).
pub const MAX_DEPTH: usize = 256;
/// Upper bound on brace nesting in the textual syntax.
pub const MAX_NESTING: usize = 64;
/// Upper bound on identifier and type-name length in bytes.
pub const MAX_IDENT: usize = 128;

/// One labelled alternative of a [`GlobalChor::Comm`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GBranch {
    pub label: String,
    /// Payload type name; empty for a payload-free label `L()`.
    pub payload: String,
    pub next: GlobalChor,
}

/// Global choreography tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GlobalChor {
    /// `from -> to` with one branch (a plain message) or several (a choice
    /// decided by `from` and communicated to `to`).
    Comm {
        from: RoleId,
        to: RoleId,
        branches: Vec<GBranch>,
    },
    /// A local action of `role`, invisible to everyone else.
    Local {
        role: RoleId,
        action: String,
        next: Box<GlobalChor>,
    },
    /// `rec var { body }`.
    Rec {
        var: String,
        body: Box<GlobalChor>,
    },
    /// `continue var`.
    Var(String),
    End,
}

/// A checked choreography: role table plus global tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choreography {
    roles: Vec<String>,
    body: GlobalChor,
}

/// Well-formedness failures. Every variant names the offending item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WellFormedError {
    NoRoles,
    TooManyRoles {
        count: usize,
    },
    DuplicateRole {
        name: String,
    },
    BadIdentifier {
        ident: String,
    },
    UnknownRole {
        role: RoleId,
    },
    SelfCommunication {
        role: String,
    },
    EmptyChoice {
        from: String,
        to: String,
    },
    DuplicateLabel {
        from: String,
        to: String,
        label: String,
    },
    ShadowedRecursionVariable {
        var: String,
    },
    UnboundRecursionVariable {
        var: String,
    },
    /// `continue var` reachable from `rec var` without an intervening
    /// message: the loop would spin without communicating.
    UnguardedRecursion {
        var: String,
    },
    UnusedRole {
        name: String,
    },
    TooLarge {
        nodes: usize,
    },
    TooDeep {
        depth: usize,
    },
}

impl fmt::Display for WellFormedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoles => write!(f, "a choreography needs at least one role"),
            Self::TooManyRoles { count } => {
                write!(f, "{count} roles exceed the limit of {MAX_ROLES}")
            }
            Self::DuplicateRole { name } => write!(f, "role `{name}` is declared twice"),
            Self::BadIdentifier { ident } => write!(f, "`{ident}` is not a valid identifier"),
            Self::UnknownRole { role } => write!(f, "role index {} is not declared", role.0),
            Self::SelfCommunication { role } => {
                write!(f, "role `{role}` sends a message to itself")
            }
            Self::EmptyChoice { from, to } => write!(f, "choice `{from} -> {to}` has no branches"),
            Self::DuplicateLabel { from, to, label } => {
                write!(f, "choice `{from} -> {to}` repeats label `{label}`")
            }
            Self::ShadowedRecursionVariable { var } => {
                write!(
                    f,
                    "recursion variable `{var}` shadows an enclosing `rec {var}`"
                )
            }
            Self::UnboundRecursionVariable { var } => {
                write!(f, "`continue {var}` has no enclosing `rec {var}`")
            }
            Self::UnguardedRecursion { var } => write!(
                f,
                "`continue {var}` is reachable from `rec {var}` without any message"
            ),
            Self::UnusedRole { name } => write!(f, "role `{name}` never sends or receives"),
            Self::TooLarge { nodes } => {
                write!(
                    f,
                    "choreography expands to {nodes} nodes (limit {MAX_NODES})"
                )
            }
            Self::TooDeep { depth } => {
                write!(
                    f,
                    "choreography nesting {depth} exceeds the limit {MAX_DEPTH}"
                )
            }
        }
    }
}

impl std::error::Error for WellFormedError {}

impl Choreography {
    /// Build and check a choreography. Fails with the first well-formedness
    /// error found.
    /// The checks are the Vesper library's (`vesper/src/tree.ves`).
    pub fn new(roles: Vec<String>, body: GlobalChor) -> Result<Self, WellFormedError> {
        let chor = Self { roles, body };
        crate::port::validate(&chor)?;
        Ok(chor)
    }

    /// A choreography the Vesper parser has already checked.
    pub(crate) fn from_checked_parts(roles: Vec<String>, body: GlobalChor) -> Self {
        Self { roles, body }
    }

    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    pub fn body(&self) -> &GlobalChor {
        &self.body
    }

    pub fn role_name(&self, role: RoleId) -> Option<&str> {
        self.roles.get(usize::from(role.0)).map(String::as_str)
    }

    pub fn role_id(&self, name: &str) -> Option<RoleId> {
        self.roles
            .iter()
            .position(|r| r == name)
            .map(|i| RoleId(i as u16))
    }
}

impl GlobalChor {
    /// Number of nodes in the tree (iterative, so deep trees cannot overflow
    /// the stack here).
    pub fn node_count(&self) -> usize {
        let mut stack = vec![self];
        let mut n = 0usize;
        while let Some(g) = stack.pop() {
            n = n.saturating_add(1);
            match g {
                GlobalChor::Comm { branches, .. } => stack.extend(branches.iter().map(|b| &b.next)),
                GlobalChor::Local { next, .. } => stack.push(next),
                GlobalChor::Rec { body, .. } => stack.push(body),
                GlobalChor::Var(_) | GlobalChor::End => {}
            }
        }
        n
    }

    /// Whether `role` sends, receives or acts anywhere in the tree.
    pub fn involves(&self, role: RoleId) -> bool {
        let mut stack = vec![self];
        while let Some(g) = stack.pop() {
            match g {
                GlobalChor::Comm { from, to, branches } => {
                    if *from == role || *to == role {
                        return true;
                    }
                    stack.extend(branches.iter().map(|b| &b.next));
                }
                GlobalChor::Local { role: r, next, .. } => {
                    if *r == role {
                        return true;
                    }
                    stack.push(next);
                }
                GlobalChor::Rec { body, .. } => stack.push(body),
                GlobalChor::Var(_) | GlobalChor::End => {}
            }
        }
        false
    }

    /// Whether every free recursion variable is one of `allowed`.
    pub fn free_vars_within(&self, allowed: &[&str]) -> bool {
        let mut stack: Vec<(&GlobalChor, Vec<&str>)> = vec![(self, allowed.to_vec())];
        while let Some((g, bound)) = stack.pop() {
            match g {
                GlobalChor::Var(v) => {
                    if !bound.contains(&v.as_str()) {
                        return false;
                    }
                }
                GlobalChor::End => {}
                GlobalChor::Comm { branches, .. } => {
                    stack.extend(branches.iter().map(|b| (&b.next, bound.clone())))
                }
                GlobalChor::Local { next, .. } => stack.push((next, bound)),
                GlobalChor::Rec { var, body } => {
                    let mut b = bound;
                    b.push(var);
                    stack.push((body, b));
                }
            }
        }
        true
    }
}
