//! May-use footprint analysis over checked function bodies (§40.1 item 1b).
//!
//! [`crate::heatmap`] shows each parameter's *declared* footprint: the pass
//! mode and projection the checker enforces. This module walks the body of
//! every documented function and records which FieldTree paths of each
//! parameter the body may actually touch, and how:
//!
//! - an assignment target (`p.a.b = ...`, `p.a += ...`) is a `write`;
//! - an argument passed `inout` is a `write`, `in` a `read`, `sink` a
//!   `consume` (of a `sink` parameter) or a copy-out `read`;
//! - a method receiver (`p.a.m(...)`) gets the strongest access the
//!   parameter's own pass allows, because the receiver pass is not known here;
//! - every other occurrence (`p.a` in a value position) is a `read`, or a
//!   `consume` when a `sink` parameter is used whole.
//!
//! The analysis is a sound over-approximation of the body's uses on the
//! surface syntax: every path the body can reach through the parameter's name
//! is reported (closures and handler clauses included), shadowed names are
//! not the parameter, and an unknown callee pass counts as the strongest pass.
//! It is not an alias analysis: a value copied out of a parameter and then
//! used is the read that copied it, which is exactly the parameter-level
//! footprint a caller needs. A declared leaf that no use reaches is
//! *declared but unused*, i.e. the projection is broader than the body needs.
//!
//! The same walk infers the effects a body may perform: operations called as
//! `Effect::op`, the declared rows of called documented functions, `?` and
//! checked arithmetic (`Fail`), minus the effects handled inside the body. A call whose effects
//! are not known (a method, a closure value, an undocumented callee) makes
//! the inferred row incomplete; only a complete row reports declared effects
//! the body never performs. The declared row stays the checked contract.
use crate::{
    FunctionDoc, Pass,
    heatmap::{Access, Heatmaps},
    visual::escape,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use vesper_core::ast::{Expr, ExprKind, Function, Pattern, PatternKind, Statement};

const MAX_DEPTH: usize = 256;
const MAX_NODES: usize = 1 << 20;
const MAX_PATHS: usize = 1024;
/// Operators whose checked arithmetic may raise `Fail[ArithOverflow]`
/// (counted for every operand type: a may-perform over-approximation).
const CHECKED: [&str; 7] = ["+", "-", "*", "/", "%", "<<", ">>"];

/// One path of a parameter the body may use. `path` is dotted; empty is the
/// whole value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathUse {
    pub path: String,
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamMayUse {
    pub parameter: String,
    pub pass: Pass,
    /// The declared selector as dotted paths; empty means the whole value.
    pub declared: Vec<String>,
    /// Paths the body may use, sorted, each with its strongest access.
    pub uses: Vec<PathUse>,
    /// Declared projection paths no use reaches (empty for a whole-value
    /// selector, which has no finer declared path).
    pub unused_declared: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionMayUse {
    pub function: String,
    pub parameters: Vec<ParamMayUse>,
    /// Effect names the body may perform, sorted.
    pub performed_effects: Vec<String>,
    /// Whether every call's effects were known.
    pub effects_complete: bool,
    /// Declared effect names the body never performs (only when complete).
    pub declared_unperformed: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MayUseReport {
    pub functions: Vec<FunctionMayUse>,
    /// Documented functions without an analysis, with the reason.
    pub unsupported: Vec<(String, String)>,
}

#[derive(Clone, Copy)]
enum Ctx {
    Value,
    Write,
    Arg(Pass),
    Receiver,
}

struct Walk<'a> {
    params: BTreeMap<&'a str, Pass>,
    /// Local bindings, innermost last. The flag is true for a binding whose
    /// calls are already counted: a closure literal bound by `let` (its body
    /// is walked where it is written) or a handler clause's continuation
    /// (resuming runs the rest of the handled body, walked already).
    scopes: Vec<BTreeMap<String, bool>>,
    uses: BTreeMap<(String, Vec<String>), Access>,
    passes: &'a BTreeMap<String, Vec<Pass>>,
    rows: &'a BTreeMap<String, Vec<String>>,
    effects: &'a BTreeSet<String>,
    performed: BTreeSet<String>,
    complete: bool,
    nodes: usize,
}

fn rank(a: Access) -> u8 {
    match a {
        Access::Free => 0,
        Access::Read => 1,
        Access::Write => 2,
        Access::Consume => 3,
    }
}

fn bind(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| bind(p, out)),
        PatternKind::Constructor { fields, .. } => fields.iter().for_each(|(_, p)| bind(p, out)),
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

/// `p.a.b` or `p.a[i].b` (the index truncates the path).
fn place(e: &Expr) -> Option<(&str, Vec<String>)> {
    match &e.kind {
        ExprKind::Name(n) => Some((n.as_str(), vec![])),
        ExprKind::Field(inner, f) => {
            let (root, mut path) = place(inner)?;
            path.push(f.clone());
            Some((root, path))
        }
        ExprKind::Index(inner, _) => place(inner),
        _ => None,
    }
}

fn constructor_like(name: &str) -> bool {
    name.rsplit("::")
        .next()
        .and_then(|s| s.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

impl<'a> Walk<'a> {
    fn live(&self, name: &str) -> Option<Pass> {
        if self.scopes.iter().any(|s| s.contains_key(name)) {
            return None;
        }
        self.params.get(name).copied()
    }
    fn record(&mut self, root: &str, path: Vec<String>, access: Access) -> Result<(), String> {
        let key = (root.to_string(), path);
        let entry = self.uses.entry(key).or_insert(Access::Free);
        if rank(access) > rank(*entry) {
            *entry = access;
        }
        if self.uses.len() > MAX_PATHS {
            return Err("may-use path budget exceeded".into());
        }
        Ok(())
    }
    fn indices(&mut self, e: &Expr, depth: usize) -> Result<(), String> {
        match &e.kind {
            ExprKind::Field(inner, _) => self.indices(inner, depth + 1),
            ExprKind::Index(inner, i) => {
                self.expr(i, Ctx::Value, depth + 1)?;
                self.indices(inner, depth + 1)
            }
            _ => Ok(()),
        }
    }
    fn scoped<T>(
        &mut self,
        names: BTreeSet<String>,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.scopes
            .push(names.into_iter().map(|n| (n, false)).collect());
        let r = f(self);
        self.scopes.pop();
        r
    }
    /// The innermost local binding of `name`, with its counted flag.
    fn local(&self, name: &str) -> Option<bool> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }
    fn call_effects(&mut self, callee: &Expr) {
        match &callee.kind {
            // A local closure literal or continuation: its effects are counted
            // where its body is written. Any other local (a function value
            // such as `let cb = record;`, a closure parameter, a pattern
            // binding) may hold any callee, so the row is incomplete.
            ExprKind::Name(n) if self.local(n).is_some() => {
                if self.local(n) != Some(true) {
                    self.complete = false;
                }
            }
            ExprKind::Name(n) if self.live(n).is_none() => {
                if let Some(row) = self.rows.get(n) {
                    self.performed.extend(row.iter().cloned());
                } else if let Some((e, _)) = n.split_once("::")
                    && self.effects.contains(e)
                {
                    self.performed.insert(e.to_string());
                } else if n.starts_with("wrapping_") || constructor_like(n) {
                    // Intrinsic arithmetic and constructors perform nothing.
                } else {
                    self.complete = false;
                }
            }
            _ => self.complete = false,
        }
    }
    fn expr(&mut self, e: &Expr, ctx: Ctx, depth: usize) -> Result<(), String> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err("may-use body budget exceeded".into());
        }
        if let Some((root, path)) = place(e)
            && let Some(pass) = self.live(root)
        {
            let whole = path.is_empty();
            let access = match ctx {
                Ctx::Value if pass == Pass::Sink && whole => Access::Consume,
                Ctx::Value | Ctx::Arg(Pass::In) => Access::Read,
                Ctx::Write | Ctx::Arg(Pass::Inout) => Access::Write,
                Ctx::Arg(Pass::Sink) if pass == Pass::Sink => Access::Consume,
                Ctx::Arg(Pass::Sink) => Access::Read,
                Ctx::Receiver => match pass {
                    Pass::In => Access::Read,
                    Pass::Inout => Access::Write,
                    Pass::Sink => Access::Consume,
                },
            };
            let root = root.to_string();
            self.record(&root, path, access)?;
            return self.indices(e, depth);
        }
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(items) => {
                for i in items {
                    self.expr(i, Ctx::Value, depth + 1)?;
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v, Ctx::Value, depth + 1)?;
                }
            }
            ExprKind::Block { statements, tail } => {
                self.scoped(BTreeSet::new(), |w| {
                    for s in statements {
                        match s {
                            Statement::Let { pat, value, .. } => {
                                w.expr(value, Ctx::Value, depth + 1)?;
                                let mut names = BTreeSet::new();
                                bind(pat, &mut names);
                                let counted = matches!(pat.kind, PatternKind::Bind(_))
                                    && matches!(value.kind, ExprKind::Closure { .. });
                                w.scopes
                                    .last_mut()
                                    .expect("scope")
                                    .extend(names.into_iter().map(|n| (n, counted)));
                            }
                            Statement::Expr(x) => w.expr(x, Ctx::Value, depth + 1)?,
                        }
                    }
                    if let Some(t) = tail {
                        w.expr(t, Ctx::Value, depth + 1)?;
                    }
                    Ok(())
                })?;
            }
            ExprKind::Call { callee, args, .. } => {
                self.call_effects(callee);
                let declared = match &callee.kind {
                    ExprKind::Name(n) if self.live(n).is_none() => self.passes.get(n),
                    _ => None,
                };
                match &callee.kind {
                    ExprKind::Field(receiver, _) => {
                        self.expr(receiver, Ctx::Receiver, depth + 1)?
                    }
                    _ => self.expr(callee, Ctx::Value, depth + 1)?,
                }
                for (i, a) in args.iter().enumerate() {
                    let ctx = if a.explicit {
                        Ctx::Arg(a.pass)
                    } else if let Some(p) = declared.and_then(|d| d.get(i)) {
                        Ctx::Arg(*p)
                    } else {
                        Ctx::Receiver
                    };
                    self.expr(&a.expr, ctx, depth + 1)?;
                }
            }
            ExprKind::Field(inner, _) => self.expr(inner, Ctx::Value, depth + 1)?,
            ExprKind::Index(inner, i) => {
                self.expr(inner, Ctx::Value, depth + 1)?;
                self.expr(i, Ctx::Value, depth + 1)?;
            }
            ExprKind::Try(inner) => {
                self.performed.insert("Fail".into());
                self.expr(inner, Ctx::Value, depth + 1)?;
            }
            ExprKind::Unary(op, inner) => {
                if op == "-" {
                    self.performed.insert("Fail".into());
                }
                self.expr(inner, Ctx::Value, depth + 1)?
            }
            ExprKind::Binary(op, a, b) => {
                if CHECKED.contains(&op.as_str()) {
                    self.performed.insert("Fail".into());
                }
                self.expr(a, Ctx::Value, depth + 1)?;
                self.expr(b, Ctx::Value, depth + 1)?;
            }
            ExprKind::Assign(op, target, value) => {
                if op != "=" {
                    self.performed.insert("Fail".into());
                }
                self.expr(value, Ctx::Value, depth + 1)?;
                // Compound assignment reads too; a write dominates the read.
                self.expr(target, Ctx::Write, depth + 1)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, Ctx::Value, depth + 1)?;
                self.expr(then, Ctx::Value, depth + 1)?;
                if let Some(o) = otherwise {
                    self.expr(o, Ctx::Value, depth + 1)?;
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, Ctx::Value, depth + 1)?;
                for (pat, body) in arms {
                    let mut names = BTreeSet::new();
                    bind(pat, &mut names);
                    self.scoped(names, |w| w.expr(body, Ctx::Value, depth + 1))?;
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let names = params.iter().map(|p| p.name.clone()).collect();
                self.scoped(names, |w| w.expr(body, Ctx::Value, depth + 1))?;
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let mut state_names = BTreeSet::new();
                if let Some((pat, init)) = state {
                    self.expr(init, Ctx::Value, depth + 1)?;
                    bind(pat, &mut state_names);
                }
                let outer = std::mem::take(&mut self.performed);
                self.expr(body, Ctx::Value, depth + 1)?;
                let mut inner = std::mem::replace(&mut self.performed, outer);
                inner.remove(&effect.name);
                self.performed.extend(inner);
                for (i, clause) in std::iter::once(&**ret).chain(ops).enumerate() {
                    let mut names = state_names.clone();
                    for p in &clause.pats {
                        bind(p, &mut names);
                    }
                    // An operation clause's last pattern is its continuation.
                    let resume = match clause.pats.last().map(|p| &p.kind) {
                        Some(PatternKind::Bind(k)) if i > 0 => Some(k.clone()),
                        _ => None,
                    };
                    self.scoped(names, |w| {
                        if let Some(k) = resume {
                            w.scopes.last_mut().expect("scope").insert(k, true);
                        }
                        w.expr(&clause.body, Ctx::Value, depth + 1)
                    })?;
                }
            }
        }
        Ok(())
    }
}

fn dotted(paths: &[Vec<String>]) -> Vec<String> {
    paths.iter().map(|p| p.join(".")).collect()
}

/// Analyse the bodies of the documented functions. `bodies` are the checked
/// declarations (by name); `functions` and `operations` supply declared
/// parameter passes and effect rows of callees; `effects` are the declared
/// effect names.
pub fn may_use(
    bodies: &[&Function],
    functions: &[FunctionDoc],
    operations: &[(String, FunctionDoc)],
    effects: &BTreeSet<String>,
) -> Result<MayUseReport, String> {
    if bodies.len() > 65_536 {
        return Err("may-use function budget exceeded".into());
    }
    let mut passes: BTreeMap<String, Vec<Pass>> = BTreeMap::new();
    let mut rows: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in functions {
        passes.insert(
            f.name.clone(),
            f.parameters.iter().map(|p| p.pass).collect(),
        );
        rows.insert(
            f.name.clone(),
            f.effects.instances.iter().map(|e| e.name.clone()).collect(),
        );
    }
    for (effect, op) in operations {
        let name = format!("{effect}::{}", op.name);
        passes.insert(name, op.parameters.iter().map(|p| p.pass).collect());
    }
    let by_name: BTreeMap<&str, &Function> = bodies.iter().map(|f| (f.name.as_str(), *f)).collect();
    let mut report = MayUseReport::default();
    for doc in functions {
        let Some(f) = by_name.get(doc.name.as_str()) else {
            report
                .unsupported
                .push((doc.name.clone(), "no checked declaration".into()));
            continue;
        };
        let Some(body) = &f.body else {
            report
                .unsupported
                .push((doc.name.clone(), "declaration has no body".into()));
            continue;
        };
        if doc.effects.tail.is_some() {
            report
                .unsupported
                .push((doc.name.clone(), "open effect row".into()));
            continue;
        }
        let mut walk = Walk {
            params: f.params.iter().map(|p| (p.name.as_str(), p.pass)).collect(),
            scopes: vec![],
            uses: BTreeMap::new(),
            passes: &passes,
            rows: &rows,
            effects,
            performed: BTreeSet::new(),
            complete: true,
            nodes: 0,
        };
        walk.expr(body, Ctx::Value, 0)
            .map_err(|e| format!("{}: {e}", doc.name))?;
        let mut parameters = Vec::new();
        for p in &f.params {
            let uses: Vec<(Vec<String>, Access)> = walk
                .uses
                .iter()
                .filter(|((root, _), _)| *root == p.name)
                .map(|((_, path), a)| (path.clone(), *a))
                .collect();
            let unused_declared = p
                .projections
                .iter()
                .filter(|d| {
                    !uses
                        .iter()
                        .any(|(u, _)| u.starts_with(d) || d.starts_with(u))
                })
                .map(|d| d.join("."))
                .collect();
            parameters.push(ParamMayUse {
                parameter: p.name.clone(),
                pass: p.pass,
                declared: dotted(&p.projections),
                uses: uses
                    .into_iter()
                    .map(|(path, access)| PathUse {
                        path: path.join("."),
                        access,
                    })
                    .collect(),
                unused_declared,
            });
        }
        let declared: BTreeSet<String> = doc
            .effects
            .instances
            .iter()
            .map(|e| e.name.clone())
            .collect();
        let declared_unperformed = if walk.complete {
            declared.difference(&walk.performed).cloned().collect()
        } else {
            vec![]
        };
        report.functions.push(FunctionMayUse {
            function: doc.name.clone(),
            parameters,
            performed_effects: walk.performed.into_iter().collect(),
            effects_complete: walk.complete,
            declared_unperformed,
        });
    }
    report.functions.sort_by(|a, b| a.function.cmp(&b.function));
    report.unsupported.sort();
    Ok(report)
}

/// Fill each heatmap row's `may_use` cells: per leaf, the strongest access
/// of any body use that reaches it (a use of a leaf's prefix reaches every
/// leaf below it; a use below an opaque leaf reaches that leaf).
pub fn attach(maps: &mut Heatmaps, report: &MayUseReport) {
    for map in &mut maps.heatmaps {
        let leaves: Vec<Vec<&str>> = map.leaves.iter().map(|l| l.split('.').collect()).collect();
        for row in &mut map.rows {
            let Some(param) = report
                .functions
                .iter()
                .find(|f| f.function == row.function)
                .and_then(|f| f.parameters.iter().find(|p| p.parameter == row.parameter))
            else {
                continue;
            };
            let cells = leaves
                .iter()
                .map(|leaf| {
                    param
                        .uses
                        .iter()
                        .filter(|u| {
                            let path: Vec<&str> = if u.path.is_empty() {
                                vec![]
                            } else {
                                u.path.split('.').collect()
                            };
                            leaf.starts_with(&path) || path.starts_with(leaf)
                        })
                        .map(|u| u.access)
                        .max_by_key(|a| rank(*a))
                        .unwrap_or(Access::Free)
                })
                .collect();
            row.may_use = Some(cells);
        }
    }
}

/// Static HTML: per function, each parameter's may-use paths, the declared
/// paths no use reaches and the inferred effect row.
pub fn render(report: &MayUseReport) -> String {
    let mut out = String::from(
        "<section id=\"codex-may-use\"><h2>May-use footprints</h2><p>Paths each body may touch through its parameters (an over-approximation over the checked body), next to the declared projection. Declared paths no use reaches are broader than the body needs. Inferred effects are the operations and callee rows the body may perform, minus handled effects.</p>",
    );
    if report.functions.is_empty() {
        out.push_str("<p>No documented function body was analysed.</p>");
    }
    for f in &report.functions {
        out.push_str(&format!(
            "<article class=\"may-use\" id=\"may-use-{0}\"><h3>{0}</h3><ul>",
            escape(&f.function)
        ));
        for p in &f.parameters {
            let uses: Vec<String> = p
                .uses
                .iter()
                .map(|u| {
                    format!(
                        "{} {}",
                        u.access.label(),
                        if u.path.is_empty() {
                            p.parameter.clone()
                        } else {
                            format!("{}.{}", p.parameter, u.path)
                        }
                    )
                })
                .collect();
            out.push_str(&format!(
                "<li><code>{}</code>: {}{}</li>",
                escape(&p.parameter),
                if uses.is_empty() {
                    "unused".to_string()
                } else {
                    escape(&uses.join(", "))
                },
                if p.unused_declared.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; <strong>declared but unused</strong>: {}",
                        escape(&p.unused_declared.join(", "))
                    )
                }
            ));
        }
        out.push_str(&format!(
            "</ul><p>Inferred effects: <code>eff [{}]</code>{}{}</p></article>",
            escape(&f.performed_effects.join(", ")),
            if f.effects_complete {
                ""
            } else {
                " (incomplete: a callee's effects are not known)"
            },
            if f.declared_unperformed.is_empty() {
                String::new()
            } else {
                format!(
                    "; declared but never performed: {}",
                    escape(&f.declared_unperformed.join(", "))
                )
            }
        ));
    }
    for (name, reason) in &report.unsupported {
        out.push_str(&format!(
            "<p>No may-use analysis for {}: {}</p>",
            escape(name),
            escape(reason)
        ));
    }
    out.push_str("</section>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effects, Mode, Parameter, Selector, Type};
    use vesper_core::ast::{Item, Module};

    fn module(source: &str) -> Module {
        let parsed = vesper_syntax::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        parsed.module
    }
    fn doc(f: &Function) -> FunctionDoc {
        FunctionDoc {
            name: f.name.clone(),
            docs: String::new(),
            span: f.span.clone(),
            generics: f.generics.clone(),
            parameters: f
                .params
                .iter()
                .map(|p| Parameter {
                    name: p.name.clone(),
                    pass: p.pass,
                    mode: Mode::Copy,
                    ty: p.ty.clone(),
                    selector: Selector::from_paths(&p.projections),
                })
                .collect(),
            result: f.result.clone(),
            declared_result: f.result.clone(),
            result_mode: Mode::Copy,
            effect_resumptions: f
                .effects
                .instances
                .iter()
                .map(|_| Some(Mode::Affine))
                .collect(),
            effects: f.effects.clone(),
            unsupported: vec![],
        }
    }
    fn analyse(source: &str) -> MayUseReport {
        let m = module(source);
        let functions: Vec<&Function> = m
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Function(f) => Some(&**f),
                _ => None,
            })
            .collect();
        let docs: Vec<FunctionDoc> = functions.iter().map(|f| doc(f)).collect();
        let effects: BTreeSet<String> = m
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Effect(e) => Some(e.name.clone()),
                _ => None,
            })
            .collect();
        may_use(&functions, &docs, &[], &effects).unwrap()
    }
    fn uses(r: &MayUseReport, f: &str, p: &str) -> Vec<(String, Access)> {
        r.functions
            .iter()
            .find(|x| x.function == f)
            .unwrap()
            .parameters
            .iter()
            .find(|x| x.parameter == p)
            .unwrap()
            .uses
            .iter()
            .map(|u| (u.path.clone(), u.access))
            .collect()
    }

    const S: &str = "copy struct S {\n    a: u64,\n    b: u64,\n    c: u64\n}\neffect E {\n    fn op(x: u64) -> u64;\n}\n";

    #[test]
    fn writes_reads_and_shadowing() {
        let r = analyse(&format!(
            "{S}fn f(inout s.{{a, b, c}}: S) {{\n    s.a = 1;\n    s.b += s.c;\n    let s = 3;\n    s;\n}}\nfn g(in s: S) -> u64 {{\n    match 1 {{\n        s => s\n    }}\n}}\nfn h(in s: S) -> u64 {{\n    let k = |s: u64| -> u64 {{ s }};\n    s.c\n}}\n"
        ));
        assert_eq!(
            uses(&r, "f", "s"),
            [
                ("a".into(), Access::Write),
                ("b".into(), Access::Write),
                ("c".into(), Access::Read)
            ]
        );
        // The match arm and closure parameter shadow `s`.
        assert!(uses(&r, "g", "s").is_empty());
        assert_eq!(uses(&r, "h", "s"), [("c".into(), Access::Read)]);
        let f = r.functions.iter().find(|x| x.function == "f").unwrap();
        // Compound assignment is checked arithmetic.
        assert_eq!(f.performed_effects, ["Fail"]);
    }

    #[test]
    fn call_passes_and_unknown_receivers() {
        let r = analyse(&format!(
            "{S}fn take(inout x: u64) {{\n}}\nfn f(inout s.{{a, b}}: S) {{\n    take(inout s.a);\n    s.b.frob();\n}}\nfn g(sink s: S) -> S {{\n    s\n}}\n"
        ));
        assert_eq!(
            uses(&r, "f", "s"),
            [("a".into(), Access::Write), ("b".into(), Access::Write)]
        );
        let f = r.functions.iter().find(|x| x.function == "f").unwrap();
        assert!(!f.effects_complete, "a method call's effects are unknown");
        assert!(f.declared_unperformed.is_empty());
        assert_eq!(uses(&r, "g", "s"), [("".into(), Access::Consume)]);
    }

    #[test]
    fn handled_effects_are_not_performed() {
        let r = analyse(&format!(
            "{S}fn f() -> u64 eff [E] {{\n    let x = E::op(1);\n    handle E in {{\n        E::op(2)\n    }} with {{\n        return v => v,\n        op(x, resume) => resume(x),\n    }}\n}}\nfn g() -> u64 eff [E] {{\n    handle E in {{\n        E::op(2)\n    }} with {{\n        return v => v,\n        op(x, resume) => resume(x),\n    }}\n}}\n"
        ));
        let f = r.functions.iter().find(|x| x.function == "f").unwrap();
        assert_eq!(f.performed_effects, ["E"]);
        let g = r.functions.iter().find(|x| x.function == "g").unwrap();
        assert!(g.performed_effects.is_empty());
        assert_eq!(g.declared_unperformed, ["E"]);
    }

    #[test]
    fn calls_through_function_values_are_incomplete() {
        let r = analyse(&format!(
            "{S}fn record() -> u64 eff [E] {{\n    E::op(1)\n}}\nfn alias() -> u64 eff [E] {{\n    let cb = record;\n    cb()\n}}\nfn literal() -> u64 eff [E] {{\n    let k = || -> u64 eff [E] {{ E::op(2) }};\n    k()\n}}\nfn shadowed() -> u64 eff [E] {{\n    let k = || -> u64 eff [E] {{ E::op(2) }};\n    let k = record;\n    k()\n}}\n"
        ));
        let f = |n: &str| r.functions.iter().find(|x| x.function == n).unwrap();
        // `cb` holds a function value: its effects are not known locally.
        assert!(!f("alias").effects_complete);
        assert!(f("alias").declared_unperformed.is_empty());
        // A closure literal's body is walked where it is written.
        assert!(f("literal").effects_complete);
        assert_eq!(f("literal").performed_effects, ["E"]);
        // The innermost binding decides.
        assert!(!f("shadowed").effects_complete);
    }

    #[test]
    fn attach_fills_heatmap_cells() {
        let mut maps = Heatmaps {
            heatmaps: vec![crate::heatmap::Heatmap {
                ty: "S".into(),
                leaves: vec!["a".into(), "b".into()],
                rows: vec![crate::heatmap::FootprintRow {
                    function: "f".into(),
                    parameter: "s".into(),
                    pass: Pass::Inout,
                    selector: vec![],
                    cells: vec![Access::Write, Access::Write],
                    free: vec![],
                    may_use: None,
                }],
                writers: vec![1, 1],
                readers: vec![0, 0],
                consumers: vec![0, 0],
                opaque: vec![],
            }],
            unsupported: vec![],
        };
        let report = MayUseReport {
            functions: vec![FunctionMayUse {
                function: "f".into(),
                parameters: vec![ParamMayUse {
                    parameter: "s".into(),
                    pass: Pass::Inout,
                    declared: vec![],
                    uses: vec![PathUse {
                        path: "b".into(),
                        access: Access::Read,
                    }],
                    unused_declared: vec![],
                }],
                performed_effects: vec![],
                effects_complete: true,
                declared_unperformed: vec![],
            }],
            unsupported: vec![],
        };
        attach(&mut maps, &report);
        assert_eq!(
            maps.heatmaps[0].rows[0].may_use,
            Some(vec![Access::Free, Access::Read])
        );
        let _ = (Effects::default(), Type::named("S"));
    }
}
