//! The Vesper library (port wave P3b lane 6, #1068), the only build:
//! parsing, well-formedness, projection, duality and exploration forward to
//! the Vesper library `chorus` (`vesper/`) through the generated binding
//! (`port/binding.rs`). Only the representation adapters live here: the
//! encoding of the crate's trees as words and text, and the decoding of
//! the library's result words.
//!
//! Inputs are words plus one text buffer: a string is `(offset, length)`
//! into the text. Trees are written in postorder (children first) so that
//! the library rebuilds them with a value stack, without recursion, and
//! interns every local type on the way. The forms are listed in
//! `vesper/src/lib.ves`. Results use the same forms with one word per
//! string: a symbol id into the library's symbol table, read here from the
//! returned handle.

use crate::check::{ExploreError, ExploreLimits, ExploreReport};
use crate::chor::{Choreography, GBranch, GlobalChor, RoleId, WellFormedError};
use crate::parse::{ParseError, ParseErrorKind};
use crate::project::{LBranch, LocalType, PayloadMismatch, ProjectionError, Unmergeable};

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, unused_variables, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;

/// Live sequence allocations of the Vesper library (for leak tests).
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}

// ---------------------------------------------------------------- encoding

#[derive(Default)]
struct Enc {
    w: Vec<u64>,
    t: Vec<u8>,
}

enum GItem<'a> {
    Enter(&'a GlobalChor),
    Exit(&'a GlobalChor),
}

enum LItem<'a> {
    Enter(&'a LocalType),
    Exit(&'a LocalType),
}

impl Enc {
    fn s(&mut self, s: &str) {
        self.w.push(self.t.len() as u64);
        self.w.push(s.len() as u64);
        self.t.extend_from_slice(s.as_bytes());
    }

    fn roles(&mut self, roles: &[String]) {
        self.w.push(roles.len() as u64);
        for r in roles {
            self.s(r);
        }
    }

    /// `nroles roles.. ntree tree..`.
    fn chor(&mut self, chor: &Choreography) {
        self.roles(chor.roles());
        self.global(chor.body());
    }

    /// `ntree tree..`, in postorder.
    fn global(&mut self, g: &GlobalChor) {
        let at = self.w.len();
        self.w.push(0);
        let mut stack = vec![GItem::Enter(g)];
        while let Some(item) = stack.pop() {
            match item {
                GItem::Enter(g) => {
                    stack.push(GItem::Exit(g));
                    match g {
                        GlobalChor::Comm { branches, .. } => {
                            stack.extend(branches.iter().rev().map(|b| GItem::Enter(&b.next)))
                        }
                        GlobalChor::Local { next, .. } => stack.push(GItem::Enter(next)),
                        GlobalChor::Rec { body, .. } => stack.push(GItem::Enter(body)),
                        GlobalChor::Var(_) | GlobalChor::End => {}
                    }
                }
                GItem::Exit(g) => match g {
                    GlobalChor::End => self.w.push(5),
                    GlobalChor::Var(v) => {
                        self.w.push(4);
                        self.s(v);
                    }
                    GlobalChor::Rec { var, .. } => {
                        self.w.push(3);
                        self.s(var);
                    }
                    GlobalChor::Local { role, action, .. } => {
                        self.w.push(2);
                        self.w.push(u64::from(role.0));
                        self.s(action);
                    }
                    GlobalChor::Comm { from, to, branches } => {
                        self.w.push(1);
                        self.w.push(u64::from(from.0));
                        self.w.push(u64::from(to.0));
                        self.w.push(branches.len() as u64);
                        for b in branches {
                            self.s(&b.label);
                            self.s(&b.payload);
                        }
                    }
                },
            }
        }
        self.w[at] = (self.w.len() - at - 1) as u64;
    }

    /// `len local..`, in postorder.
    fn local(&mut self, l: &LocalType) {
        let at = self.w.len();
        self.w.push(0);
        let mut stack = vec![LItem::Enter(l)];
        while let Some(item) = stack.pop() {
            match item {
                LItem::Enter(l) => {
                    stack.push(LItem::Exit(l));
                    match l {
                        LocalType::Send { next, .. }
                        | LocalType::Recv { next, .. }
                        | LocalType::Action { next, .. } => stack.push(LItem::Enter(next)),
                        LocalType::Rec { body, .. } => stack.push(LItem::Enter(body)),
                        LocalType::Select { branches, .. } | LocalType::Offer { branches, .. } => {
                            stack.extend(branches.iter().rev().map(|b| LItem::Enter(&b.next)))
                        }
                        LocalType::Var(_) | LocalType::End => {}
                    }
                }
                LItem::Exit(l) => match l {
                    LocalType::End => self.w.push(8),
                    LocalType::Var(v) => {
                        self.w.push(7);
                        self.s(v);
                    }
                    LocalType::Rec { var, .. } => {
                        self.w.push(6);
                        self.s(var);
                    }
                    LocalType::Action { name, .. } => {
                        self.w.push(5);
                        self.s(name);
                    }
                    LocalType::Send {
                        peer,
                        label,
                        payload,
                        ..
                    }
                    | LocalType::Recv {
                        peer,
                        label,
                        payload,
                        ..
                    } => {
                        let send = matches!(l, LocalType::Send { .. });
                        self.w.push(if send { 1 } else { 2 });
                        self.w.push(u64::from(peer.0));
                        self.s(label);
                        self.s(payload);
                    }
                    LocalType::Select { peer, branches } | LocalType::Offer { peer, branches } => {
                        let select = matches!(l, LocalType::Select { .. });
                        self.w.push(if select { 3 } else { 4 });
                        self.w.push(u64::from(peer.0));
                        self.w.push(branches.len() as u64);
                        for b in branches {
                            self.s(&b.label);
                            self.s(&b.payload);
                        }
                    }
                },
            }
        }
        self.w[at] = (self.w.len() - at - 1) as u64;
    }
}

// ---------------------------------------------------------------- decoding

/// The result words of one call, with the symbol table they refer to.
struct Out<'a> {
    w: &'a [u64],
    pos: usize,
    off: &'a [u64],
    len: &'a [u64],
    txt: &'a [u8],
}

impl<'a> Out<'a> {
    fn new(c: &'a binding::CRef<'a>) -> Self {
        Self {
            w: c.out().as_slice(),
            pos: 0,
            off: c.sy_off().as_slice(),
            len: c.sy_len().as_slice(),
            txt: c.sy_txt().as_slice(),
        }
    }

    fn word(&mut self) -> u64 {
        let x = self.w.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        x
    }

    fn usize(&mut self) -> usize {
        self.word() as usize
    }

    fn role(&mut self) -> RoleId {
        RoleId(self.word() as u16)
    }

    fn text(&self, y: u64) -> String {
        let y = y as usize;
        let (Some(&o), Some(&n)) = (self.off.get(y), self.len.get(y)) else {
            return String::new();
        };
        let bytes = &self.txt[o as usize..(o + n) as usize];
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn string(&mut self) -> String {
        let y = self.word();
        self.text(y)
    }

    fn strings(&mut self, n: usize) -> Vec<String> {
        (0..n).map(|_| self.string()).collect()
    }

    /// A global tree of the parser's output, up to the end of the words.
    fn global(&mut self) -> GlobalChor {
        let mut vs: Vec<GlobalChor> = Vec::new();
        while self.pos < self.w.len() {
            let g = match self.word() {
                5 => GlobalChor::End,
                4 => GlobalChor::Var(self.string()),
                3 => {
                    let var = self.string();
                    let body = Box::new(vs.pop().unwrap_or(GlobalChor::End));
                    GlobalChor::Rec { var, body }
                }
                2 => {
                    let role = self.role();
                    let action = self.string();
                    let next = Box::new(vs.pop().unwrap_or(GlobalChor::End));
                    GlobalChor::Local { role, action, next }
                }
                _ => {
                    let from = self.role();
                    let to = self.role();
                    let n = self.usize();
                    let nexts = vs.split_off(vs.len().saturating_sub(n));
                    let branches = nexts
                        .into_iter()
                        .map(|next| GBranch {
                            label: self.string(),
                            payload: self.string(),
                            next,
                        })
                        .collect();
                    GlobalChor::Comm { from, to, branches }
                }
            };
            vs.push(g);
        }
        vs.pop().unwrap_or(GlobalChor::End)
    }

    /// A local type of `len` words.
    fn local(&mut self, len: usize) -> LocalType {
        let end = self.pos + len;
        let mut vs: Vec<LocalType> = Vec::new();
        while self.pos < end {
            let tag = self.word();
            let l = match tag {
                8 => LocalType::End,
                7 => LocalType::Var(self.string()),
                6 => LocalType::Rec {
                    var: self.string(),
                    body: Box::new(vs.pop().unwrap_or(LocalType::End)),
                },
                5 => LocalType::Action {
                    name: self.string(),
                    next: Box::new(vs.pop().unwrap_or(LocalType::End)),
                },
                1 | 2 => {
                    let peer = self.role();
                    let label = self.string();
                    let payload = self.string();
                    let next = Box::new(vs.pop().unwrap_or(LocalType::End));
                    if tag == 1 {
                        LocalType::Send {
                            peer,
                            label,
                            payload,
                            next,
                        }
                    } else {
                        LocalType::Recv {
                            peer,
                            label,
                            payload,
                            next,
                        }
                    }
                }
                _ => {
                    let peer = self.role();
                    let n = self.usize();
                    let nexts = vs.split_off(vs.len().saturating_sub(n));
                    let branches = nexts
                        .into_iter()
                        .map(|next| LBranch {
                            label: self.string(),
                            payload: self.string(),
                            next,
                        })
                        .collect();
                    if tag == 3 {
                        LocalType::Select { peer, branches }
                    } else {
                        LocalType::Offer { peer, branches }
                    }
                }
            };
            vs.push(l);
        }
        vs.pop().unwrap_or(LocalType::End)
    }

    fn well_formed(&mut self) -> WellFormedError {
        match self.word() {
            0 => WellFormedError::NoRoles,
            1 => WellFormedError::TooManyRoles {
                count: self.usize(),
            },
            2 => WellFormedError::DuplicateRole {
                name: self.string(),
            },
            3 => WellFormedError::BadIdentifier {
                ident: self.string(),
            },
            4 => WellFormedError::UnknownRole { role: self.role() },
            5 => WellFormedError::SelfCommunication {
                role: self.string(),
            },
            6 => WellFormedError::EmptyChoice {
                from: self.string(),
                to: self.string(),
            },
            7 => WellFormedError::DuplicateLabel {
                from: self.string(),
                to: self.string(),
                label: self.string(),
            },
            8 => WellFormedError::ShadowedRecursionVariable { var: self.string() },
            9 => WellFormedError::UnboundRecursionVariable { var: self.string() },
            10 => WellFormedError::UnguardedRecursion { var: self.string() },
            11 => WellFormedError::UnusedRole {
                name: self.string(),
            },
            12 => WellFormedError::TooLarge {
                nodes: self.usize(),
            },
            _ => WellFormedError::TooDeep {
                depth: self.usize(),
            },
        }
    }

    fn projection(&mut self) -> ProjectionError {
        if self.word() == 1 {
            let role = self.string();
            let decider = self.string();
            let notified = self.string();
            let n = self.usize();
            let path = self.strings(n);
            ProjectionError::Unmergeable(Box::new(Unmergeable {
                role,
                decider,
                notified,
                path,
                left_label: self.string(),
                right_label: self.string(),
                left: self.string(),
                right: self.string(),
            }))
        } else {
            ProjectionError::PayloadMismatch(Box::new(PayloadMismatch {
                role: self.string(),
                peer: self.string(),
                label: self.string(),
                left: self.string(),
                right: self.string(),
            }))
        }
    }
}

// ---------------------------------------------------------------- entry points

/// The texts of `ParseErrorKind::UnexpectedToken::expected`, by the index
/// the library reports (`vesper/src/parse.ves`).
const EXPECTED: [&str; 16] = [
    "role name",
    "type name",
    "`,` or `]`",
    "message label",
    "`(`",
    "`)`",
    "`}`",
    "`{`",
    "recursion variable",
    "`;`",
    "local action name",
    "choice arm or `}`",
    "`:` or `{`",
    "`->` or `:`",
    "`roles`",
    "`,` or `;`",
];

fn expected(i: u64) -> &'static str {
    EXPECTED.get(i as usize).copied().unwrap_or("token")
}

pub(crate) fn parse_choreography(src: &str) -> Result<Choreography, ParseError> {
    let c = binding::parse(src);
    let r = c.as_ref();
    let mut o = Out::new(&r);
    if o.word() == 0 {
        let n = o.usize();
        let roles = o.strings(n);
        let body = o.global();
        return Ok(Choreography::from_checked_parts(roles, body));
    }
    let line = o.word() as u32;
    let col = o.word() as u32;
    let kind = match o.word() {
        0 => ParseErrorKind::TooLong { bytes: o.usize() },
        1 => ParseErrorKind::UnexpectedChar(char::from_u32(o.word() as u32).unwrap_or('\u{fffd}')),
        2 => {
            let e = expected(o.word());
            ParseErrorKind::UnexpectedToken {
                expected: e,
                found: o.string(),
            }
        }
        3 => ParseErrorKind::UnexpectedEof {
            expected: expected(o.word()),
        },
        4 => ParseErrorKind::UnknownRole(o.string()),
        5 => ParseErrorKind::IdentifierTooLong,
        6 => ParseErrorKind::Unreachable,
        7 => ParseErrorKind::TooDeep,
        8 => ParseErrorKind::TooLarge,
        _ => ParseErrorKind::Invalid(o.well_formed()),
    };
    Err(ParseError { line, col, kind })
}

pub(crate) fn validate(chor: &Choreography) -> Result<(), WellFormedError> {
    let mut e = Enc::default();
    e.chor(chor);
    let c = binding::check_chor(&e.w, &e.t);
    let r = c.as_ref();
    let mut o = Out::new(&r);
    if o.word() == 0 {
        Ok(())
    } else {
        Err(o.well_formed())
    }
}

/// Projects one role (`Some`) or every role (`None`).
pub(crate) fn project(
    chor: &Choreography,
    role: Option<RoleId>,
) -> Result<Vec<LocalType>, ProjectionError> {
    let mut e = Enc::default();
    e.chor(chor);
    let c = binding::project(&e.w, &e.t, role.map_or(u64::MAX, |r| u64::from(r.0)));
    let r = c.as_ref();
    let mut o = Out::new(&r);
    if o.word() != 0 {
        return Err(o.projection());
    }
    let n = o.usize();
    Ok((0..n)
        .map(|_| {
            let len = o.usize();
            o.local(len)
        })
        .collect())
}

pub(crate) fn are_dual(a: RoleId, s_a: &LocalType, b: RoleId, s_b: &LocalType) -> bool {
    let mut e = Enc::default();
    e.local(s_a);
    e.local(s_b);
    binding::dual(&e.w, &e.t, u64::from(a.0), u64::from(b.0))
}

/// `explore` when `locals` is `None`, else `explore_locals`.
pub(crate) fn explore(
    chor: &Choreography,
    locals: Option<&[LocalType]>,
    limits: ExploreLimits,
) -> Result<ExploreReport, ExploreError> {
    let mut e = Enc::default();
    let n = match locals {
        None => {
            e.chor(chor);
            u64::MAX
        }
        Some(ls) => {
            e.roles(chor.roles());
            for l in ls {
                e.local(l);
            }
            ls.len() as u64
        }
    };
    let c = binding::explore(
        &e.w,
        &e.t,
        n,
        limits.max_states as u64,
        limits.max_queue as u64,
    );
    let r = c.as_ref();
    let mut o = Out::new(&r);
    if o.word() == 0 {
        return Ok(ExploreReport {
            states: o.usize(),
            completed: o.usize(),
            queue_bound_hit: o.word() != 0,
        });
    }
    Err(match o.word() {
        1 => ExploreError::Projection(o.projection()),
        2 => {
            let n = o.usize();
            ExploreError::Deadlock {
                waiting: (0..n).map(|_| (o.string(), o.string())).collect(),
            }
        }
        3 => {
            let role = o.string();
            let peer = o.string();
            let n = o.usize();
            let expected = o.strings(n);
            ExploreError::UnexpectedMessage {
                role,
                peer,
                expected,
                found: o.string(),
            }
        }
        4 => {
            let n = o.usize();
            ExploreError::Orphans {
                messages: (0..n)
                    .map(|_| (o.string(), o.string(), o.string()))
                    .collect(),
            }
        }
        _ => ExploreError::StateBudgetExceeded { states: o.usize() },
    })
}
