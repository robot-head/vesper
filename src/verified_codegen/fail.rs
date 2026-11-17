//! Milestone 7: abort-only `Fail[E]`, `?` and `handle Fail` as structured
//! early exits.
//!
//! Every error type `E` that the planned program raises or handles gets a
//! program-wide code (1, 2, ...; 0 is "no failure"). A function that may
//! raise to its caller uses the *status protocol*: word 0 of its incoming
//! result area is the status, the words after it hold either the result
//! (status 0) or the raised error's payload (status = its code). Callers test
//! the status after every call and either run the matching handler clause or
//! pass the status and payload on to their own caller. The entry never uses
//! the protocol: an `ArithOverflow` or `RuntimeError` that reaches it aborts
//! the process as before, and any other error type reaching it is rejected.
//!
//! `ArithOverflow` and `RuntimeError` are also raised implicitly by checked
//! arithmetic. They join the protocol only when the program handles or
//! explicitly raises them; otherwise they stay process aborts, so programs
//! without handlers compile exactly as before. A function that catches them
//! sets a trap scope ([`super::Lir::Scope`]) so the checked instructions jump
//! to its own raise stubs instead of the process exits.
//!
//! Only non-resuming handlers are compiled: the `raise` clause's
//! continuation must be unused (it is erased from the clause's parameters).
//! Stateful handlers, labeled `Fail` instances and every other effect are
//! rejected with a typed error.
use super::lower::{Shape, shape};
use super::machine::TrapKind;
use super::{Unsupported, VerifiedError};
use crate::ast::{ExprKind, PatternKind, Type};
use crate::check::World;
use crate::ir::{FunctionIr, Op, Program, ValueId, nested_name};
use std::collections::{BTreeMap, BTreeSet};

/// The callee of an explicit raise.
pub const RAISE: &str = "Fail::raise";
const OVERFLOW: &str = "ArithOverflow";
const RUNTIME: &str = "RuntimeError";

/// The program-wide failure codes.
#[derive(Default)]
pub struct Fails {
    /// By error type (rendered after literal defaulting): code, type, words.
    by_type: BTreeMap<String, (u64, Type, Shape)>,
}
impl Fails {
    pub fn code(&self, ty: &Type) -> Option<u64> {
        self.by_type.get(&key(ty)).map(|(c, _, _)| *c)
    }
    /// The code of an implicit failure kind, when it uses the protocol.
    pub fn implicit(&self, kind: TrapKind) -> Option<u64> {
        match kind {
            TrapKind::Overflow => self.by_type.get(OVERFLOW).map(|(c, _, _)| *c),
            TrapKind::DivisionByZero => self.by_type.get(RUNTIME).map(|(c, _, _)| *c),
            // Region exhaustion is not a `Fail` effect: no handler catches it.
            TrapKind::Unreachable | TrapKind::Region => None,
        }
    }
    /// The process abort a code stands for at the entry, if any.
    pub fn trap_kind(&self, code: u64) -> Option<TrapKind> {
        [TrapKind::Overflow, TrapKind::DivisionByZero]
            .into_iter()
            .find(|k| self.implicit(*k) == Some(code))
    }
    pub fn payload(&self, code: u64) -> Option<(&Type, &Shape)> {
        self.by_type
            .values()
            .find(|(c, _, _)| *c == code)
            .map(|(_, t, s)| (t, s))
    }
    /// The widest payload among `codes`.
    pub fn payload_words(&self, codes: &BTreeSet<u64>) -> u32 {
        codes
            .iter()
            .filter_map(|c| self.payload(*c))
            .map(|(_, s)| s.len())
            .max()
            .unwrap_or(0)
    }
    pub fn name(&self, code: u64) -> String {
        self.payload(code)
            .map(|(t, _)| t.to_string())
            .unwrap_or_else(|| format!("code {code}"))
    }
}

fn key(ty: &Type) -> String {
    crate::numeric::default_type(ty).to_string()
}

/// A resolved non-resuming `handle Fail[E] in body with { return p => ..,
/// raise(e, k) => .. }`: the nested IR functions and the names their
/// patterns bind.
#[derive(Clone, Debug)]
pub struct Handler {
    pub error: Type,
    pub body: String,
    pub ret: String,
    /// The name the `return` pattern binds (its last parameter).
    pub ret_bind: Option<String>,
    pub raise: String,
    /// The name the error pattern binds.
    pub raise_bind: Option<String>,
    /// The continuation's name; it must be unused and is erased.
    pub resume: Option<String>,
}

fn bound(p: &crate::ast::Pattern) -> Result<Option<String>, ()> {
    match &p.kind {
        PatternKind::Bind(n) => Ok(Some(n.clone())),
        PatternKind::Wild => Ok(None),
        _ => Err(()),
    }
}

/// Resolve the handler of an `Op::Handle` in `site`.
pub fn handler(
    program: &Program,
    site: &str,
    state: Option<ValueId>,
    expr: &crate::ast::Expr,
) -> Result<Handler, VerifiedError> {
    let unsupported =
        |detail: String| VerifiedError::unsupported(Unsupported::Effect, site, detail);
    let ExprKind::Handle {
        effect,
        state: st,
        body,
        ret,
        ops,
    } = &expr.kind
    else {
        return Err(VerifiedError::Invalid(
            "handle instruction without a handler".into(),
        ));
    };
    if effect.name != "Fail" {
        return Err(unsupported(format!(
            "handler for {} (only Fail handlers)",
            effect.name
        )));
    }
    if effect.label.is_some() || effect.args.len() != 1 {
        return Err(unsupported(
            "labeled or ill-formed Fail handler (only `handle Fail[E]`)".into(),
        ));
    }
    if st.is_some() || state.is_some() {
        return Err(unsupported("stateful Fail handler".into()));
    }
    let [clause] = ops.as_slice() else {
        return Err(VerifiedError::Invalid(
            "a Fail handler has exactly one raise clause".into(),
        ));
    };
    let [error_pattern, resume_pattern] = clause.pats.as_slice() else {
        return Err(VerifiedError::Invalid("raise clause arity".into()));
    };
    let ret_pattern = ret
        .pats
        .first()
        .ok_or_else(|| VerifiedError::Invalid("return clause without a pattern".into()))?;
    let destructuring = || {
        VerifiedError::unsupported(
            Unsupported::Match,
            site,
            "a handler clause pattern other than a name or `_`",
        )
    };
    let handler = Handler {
        error: crate::numeric::default_type(&effect.args[0]),
        body: super::mono::nested_instance(site, &nested_name("handler_body", &body.span)),
        ret: super::mono::nested_instance(site, &nested_name("handler_clause", &ret.body.span)),
        ret_bind: bound(ret_pattern).map_err(|_| destructuring())?,
        raise: super::mono::nested_instance(
            site,
            &nested_name("handler_clause", &clause.body.span),
        ),
        raise_bind: bound(error_pattern).map_err(|_| destructuring())?,
        resume: bound(resume_pattern).map_err(|_| destructuring())?,
    };
    for f in [&handler.body, &handler.ret, &handler.raise] {
        if !program.functions.contains_key(f) {
            return Err(VerifiedError::Invalid(format!(
                "handler function {f} was not lowered"
            )));
        }
    }
    if let Some(k) = &handler.resume
        && reads(&program.functions[&handler.raise], k)
    {
        return Err(unsupported(format!(
            "raise clause uses its continuation '{k}' (only non-resuming Fail handlers)"
        )));
    }
    Ok(handler)
}

/// Whether `f` reads the local `name` (directly, or by capturing it in a
/// nested closure or handler).
fn reads(f: &FunctionIr, name: &str) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| match &i.op {
            Op::Read { root, .. } => root == name,
            // The alias map holds every name in scope; only the free names
            // of the nested expression are actually captured.
            Op::Closure { expr, aliases, .. } | Op::Handle { expr, aliases, .. } => {
                crate::check::free_names(expr)
                    .iter()
                    .any(|n| aliases.get(n).is_some_and(|v| v == name))
            }
            _ => false,
        })
}

/// What one function raises by itself, before calls: explicit error types
/// (`?`, `Fail::raise`) and whether its checked arithmetic may overflow or
/// divide by zero (conservatively, by operator).
#[derive(Default)]
pub struct Local {
    pub explicit: BTreeSet<String>,
    pub types: BTreeMap<String, Type>,
    pub overflow: bool,
    pub runtime: bool,
}

pub fn local(f: &FunctionIr) -> Result<Local, VerifiedError> {
    let mut types: BTreeMap<ValueId, Type> = f.values.clone();
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        types.insert(i.dest, i.ty.clone());
    }
    let mut out = Local::default();
    let add = |out: &mut Local, ty: &Type| {
        let ty = crate::numeric::default_type(ty);
        out.explicit.insert(key(&ty));
        out.types.insert(key(&ty), ty);
    };
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Try(v) => {
                let ty = types
                    .get(v)
                    .map(crate::numeric::default_type)
                    .ok_or_else(|| VerifiedError::Invalid(format!("untyped value %{v}")))?;
                match &ty {
                    Type::Named(n, args) if n == "Result" && args.len() == 2 => {
                        add(&mut out, &args[1])
                    }
                    _ => return Err(VerifiedError::Invalid(format!("`?` on {ty}"))),
                }
            }
            Op::Call { callee, args, .. } if callee == RAISE => {
                let [arg] = args.as_slice() else {
                    return Err(VerifiedError::Invalid("Fail::raise arity".into()));
                };
                let ty = types.get(&arg.value).ok_or_else(|| {
                    VerifiedError::Invalid(format!("untyped value %{}", arg.value))
                })?;
                add(&mut out, ty);
            }
            Op::Binary(op, ..) => match op.as_str() {
                "+" | "-" | "*" => out.overflow = true,
                "/" | "%" => {
                    out.overflow = true;
                    out.runtime = true;
                }
                _ => {}
            },
            Op::Write { op, .. } => match op.as_str() {
                "+=" | "-=" | "*=" => out.overflow = true,
                "/=" | "%=" => {
                    out.overflow = true;
                    out.runtime = true;
                }
                _ => {}
            },
            Op::Unary(op, _) if op == "-" => out.overflow = true,
            // Out of range raises `RuntimeError::IndexOutOfBounds`.
            Op::Index(..) => out.runtime = true,
            _ => {}
        }
    }
    Ok(out)
}

/// Assign codes to every error type raised or handled anywhere, plus the
/// implicit kinds when they are handled or raised explicitly.
pub fn codes(
    world: &World,
    locals: &[&Local],
    handled: &BTreeMap<String, Type>,
    site: &str,
) -> Result<Fails, VerifiedError> {
    let mut all: BTreeMap<String, Type> = handled.clone();
    for l in locals {
        for (k, t) in &l.types {
            all.insert(k.clone(), t.clone());
        }
    }
    let mut fails = Fails::default();
    for (n, (k, t)) in all.into_iter().enumerate() {
        let s = shape(world, &t, site)?;
        fails.by_type.insert(k, (n as u64 + 1, t, s));
    }
    Ok(fails)
}

/// The codes a function raises by itself.
pub fn local_codes(fails: &Fails, l: &Local) -> BTreeSet<u64> {
    let mut out: BTreeSet<u64> = l
        .explicit
        .iter()
        .filter_map(|k| fails.by_type.get(k).map(|(c, _, _)| *c))
        .collect();
    if l.overflow
        && let Some(c) = fails.implicit(TrapKind::Overflow)
    {
        out.insert(c);
    }
    if l.runtime
        && let Some(c) = fails.implicit(TrapKind::DivisionByZero)
    {
        out.insert(c);
    }
    out
}
