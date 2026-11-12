//! Conservative lint engine over trusted compiler-produced typed facts.
//!
//! The facade must parse/check/lower actual source before constructing these
//! DTOs. They are inspection data, not certificates accepted from plugins or
//! untrusted JSON. This leaf has no dependency on the compiler facade.
#![forbid(unsafe_code)]
pub mod policy;

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use vesper_core::ast::{Effect, Effects, Function, Param, Pass, Span, Type, substitute};

#[derive(Clone, Debug)]
pub struct CheckedProgram {
    pub functions: BTreeMap<String, FunctionFacts>,
}
#[derive(Clone, Debug)]
pub struct FunctionFacts {
    pub declaration: Function,
    pub actions: Vec<Action>,
    pub parameters: Vec<ParameterFacts>,
    pub source_owned: bool,
}
#[derive(Clone, Debug)]
pub struct ParameterFacts {
    pub parameter: Param,
    pub struct_fields: Vec<String>,
}
#[derive(Clone, Debug)]
pub enum Action {
    /// Parent effect of a resolved operation, with checker-inferred type args.
    Perform(Effect),
    /// Resolved direct call and the actual generic substitution at its span.
    Call {
        callee: String,
        substitutions: BTreeMap<String, Type>,
    },
    /// Effective checked selector, including the callee's projection selectors.
    Access {
        root: String,
        path: Vec<String>,
        pass: Pass,
        moves_noncopy: bool,
    },
    /// Unknown indirect/closure/handler/open-row evidence prevents inference.
    Opaque(String),
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub functions: usize,
    pub actions: usize,
    pub work: usize,
    pub call_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            functions: 256,
            actions: 16384,
            work: 1_000_000,
            call_depth: 64,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LintError(pub &'static str);
impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for LintError {}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingKind {
    UnusedEffectCapability {
        effect: Effect,
        effect_index: usize,
    },
    OverbroadInoutProjection {
        parameter: String,
        mutable_paths: Vec<Vec<String>>,
        read_paths: Vec<Vec<String>>,
    },
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub code: String,
    pub function: String,
    pub span: Span,
    pub message: String,
    pub detail: FindingKind,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Skipped {
    pub function: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct LintReport {
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
    pub work: usize,
}
struct Budget {
    remaining: usize,
    used: usize,
    depth: usize,
}
impl Budget {
    fn tick(&mut self) -> Result<(), LintError> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(LintError("lint work limit"))?;
        self.used += 1;
        Ok(())
    }
}

/// Analyze only actual resolved facts; all comparisons preserve full labels and
/// structural type arguments. No stringified type/name-set authority is used.
pub fn analyze(program: &CheckedProgram, limits: Limits) -> Result<LintReport, LintError> {
    if limits.functions == 0
        || limits.functions > 256
        || limits.actions == 0
        || limits.actions > 16384
        || limits.work == 0
        || limits.work > 1_000_000
        || limits.call_depth == 0
        || limits.call_depth > 64
    {
        return Err(LintError("invalid lint limits"));
    }
    if program.functions.len() > limits.functions {
        return Err(LintError("function limit"));
    }
    let mut count = 0usize;
    for f in program.functions.values() {
        count = count
            .checked_add(f.actions.len())
            .ok_or(LintError("action count overflow"))?;
        if count > limits.actions
            || f.declaration.effects.instances.len() > 64
            || f.parameters.len() > 128
        {
            return Err(LintError("fact limit"));
        }
        if f.declaration.params.len() > 128 || f.declaration.generics.len() > 64 {
            return Err(LintError("signature size limit"));
        }
        validate_type(&f.declaration.result)?;
        for parameter in &f.declaration.params {
            validate_type(&parameter.ty)?;
        }
        for parameter in &f.parameters {
            validate_type(&parameter.parameter.ty)?;
            if parameter.struct_fields.len() > 128 {
                return Err(LintError("struct field limit"));
            }
        }
        for effect in &f.declaration.effects.instances {
            validate_effect(effect)?;
        }
        for action in &f.actions {
            match action {
                Action::Perform(effect) => validate_effect(effect)?,
                Action::Call { substitutions, .. } => {
                    if substitutions.len() > 64 {
                        return Err(LintError("substitution limit"));
                    }
                    for ty in substitutions.values() {
                        validate_type(ty)?;
                    }
                }
                Action::Access { path, .. } => {
                    if path.len() > 32 {
                        return Err(LintError("projection depth limit"));
                    }
                }
                Action::Opaque(_) => (),
            }
        }
    }
    let mut budget = Budget {
        remaining: limits.work,
        used: 0,
        depth: limits.call_depth,
    };
    let mut report = LintReport {
        findings: vec![],
        skipped: vec![],
        work: 0,
    };
    for (name, facts) in &program.functions {
        if !facts.source_owned || facts.declaration.body.is_none() {
            continue;
        }
        let mut seen = BTreeSet::new();
        match required(program, name, &mut seen, &mut budget)? {
            Ok(used) => {
                for (index, effect) in facts.declaration.effects.instances.iter().enumerate() {
                    if !used.contains(effect) {
                        report.findings.push(Finding {
                            code: "L0102".into(),
                            function: name.clone(),
                            span: facts.declaration.span.clone(),
                            message: format!("unused effect capability {}", display_effect(effect)),
                            detail: FindingKind::UnusedEffectCapability {
                                effect: effect.clone(),
                                effect_index: index,
                            },
                        });
                    }
                }
            }
            Err(reason) => report.skipped.push(Skipped {
                function: name.clone(),
                reason,
            }),
        }
        projection_findings(name, facts, &mut report.findings, &mut budget)?;
    }
    report.work = budget.used;
    Ok(report)
}
fn validate_type(ty: &Type) -> Result<(), LintError> {
    let mut stack = vec![(ty, 0usize)];
    let mut nodes = 0usize;
    while let Some((ty, depth)) = stack.pop() {
        nodes += 1;
        if depth > 32 || nodes > 512 {
            return Err(LintError("type complexity limit"));
        }
        match ty {
            Type::Named(_, args) | Type::Tuple(args) => {
                if args.len() > 512 {
                    return Err(LintError("type arity limit"));
                }
                for ty in args {
                    stack.push((ty, depth + 1));
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                if params.len() > 128 || effects.instances.len() > 64 {
                    return Err(LintError("arrow shape limit"));
                }
                for (_, t) in params {
                    stack.push((t, depth + 1));
                }
                stack.push((result, depth + 1));
                for e in &effects.instances {
                    if e.args.len() > 64 {
                        return Err(LintError("nested effect arity limit"));
                    }
                    for t in &e.args {
                        stack.push((t, depth + 1));
                    }
                }
            }
            Type::Row(effects) => {
                if effects.instances.len() > 64 {
                    return Err(LintError("row shape limit"));
                }
                for e in &effects.instances {
                    if e.args.len() > 64 {
                        return Err(LintError("nested effect arity limit"));
                    }
                    for t in &e.args {
                        stack.push((t, depth + 1));
                    }
                }
            }
            _ => (),
        }
    }
    Ok(())
}
fn validate_effect(effect: &Effect) -> Result<(), LintError> {
    if effect.args.len() > 64 {
        return Err(LintError("effect arity limit"));
    }
    for ty in &effect.args {
        validate_type(ty)?;
    }
    Ok(())
}
fn opaque_type(ty: &Type) -> bool {
    match ty {
        Type::Infer | Type::IntLiteral(_) | Type::Function { .. } | Type::Row(_) => true,
        Type::Named(_, args) | Type::Tuple(args) => args.iter().any(opaque_type),
    }
}
fn resolve(row: &Effects, request: &Effect) -> Option<Effect> {
    row.instances
        .iter()
        .rev()
        .find(|e| {
            e.name == request.name
                && e.args == request.args
                && request
                    .label
                    .as_ref()
                    .is_none_or(|label| e.label.as_ref() == Some(label))
        })
        .cloned()
}
fn required(
    program: &CheckedProgram,
    name: &str,
    seen: &mut BTreeSet<String>,
    budget: &mut Budget,
) -> Result<Result<Vec<Effect>, String>, LintError> {
    budget.tick()?;
    let Some(f) = program.functions.get(name) else {
        return Ok(Err("unresolved direct callee".into()));
    };
    if f.declaration.effects.tail.is_some() || f.declaration.generics.iter().any(|g| g.row) {
        return Ok(Err("open or polymorphic effect row".into()));
    }
    if f.declaration.params.iter().any(|p| opaque_type(&p.ty)) || opaque_type(&f.declaration.result)
    {
        return Ok(Err("higher-order or unresolved signature".into()));
    }
    if f.declaration
        .effects
        .instances
        .iter()
        .flat_map(|e| &e.args)
        .any(opaque_type)
    {
        return Ok(Err("unresolved effect instance".into()));
    }
    if !f.source_owned || f.declaration.body.is_none() {
        return Ok(Ok(f.declaration.effects.instances.clone()));
    }
    if seen.len() >= budget.depth || !seen.insert(name.into()) {
        return Ok(Err("recursive or deep call graph".into()));
    }
    let mut out = Vec::new();
    let result = (|| {
        for action in &f.actions {
            budget.tick()?;
            let requested = match action {
                Action::Perform(effect) => vec![effect.clone()],
                Action::Call {
                    callee,
                    substitutions,
                } => {
                    let Some(target) = program.functions.get(callee) else {
                        return Ok(Err("unresolved direct call".into()));
                    };
                    if target
                        .declaration
                        .generics
                        .iter()
                        .any(|g| !substitutions.contains_key(&g.name))
                        || substitutions.values().any(opaque_type)
                    {
                        return Ok(Err("unresolved generic call substitution".into()));
                    }
                    match required(program, callee, seen, budget)? {
                        Ok(effects) => effects
                            .into_iter()
                            .map(|mut e| {
                                e.args = e
                                    .args
                                    .iter()
                                    .map(|t| substitute(t, substitutions))
                                    .collect();
                                e
                            })
                            .collect(),
                        Err(reason) => return Ok(Err(reason)),
                    }
                }
                Action::Opaque(reason) => return Ok(Err(reason.clone())),
                Action::Access { .. } => continue,
            };
            for request in requested {
                let Some(effect) = resolve(&f.declaration.effects, &request) else {
                    return Ok(Err(
                        "effect instance could not be mapped to caller capability".into(),
                    ));
                };
                if !out.contains(&effect) {
                    out.push(effect);
                }
            }
        }
        Ok(Ok(out))
    })();
    seen.remove(name);
    result
}
fn projection_findings(
    name: &str,
    f: &FunctionFacts,
    out: &mut Vec<Finding>,
    budget: &mut Budget,
) -> Result<(), LintError> {
    if f.actions.iter().any(|a| matches!(a, Action::Opaque(_))) {
        return Ok(());
    }
    for parameter in &f.parameters {
        let p = &parameter.parameter;
        if p.pass != Pass::Inout || !p.projections.is_empty() || parameter.struct_fields.is_empty()
        {
            continue;
        }
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        let mut blocked = false;
        for action in &f.actions {
            budget.tick()?;
            if let Action::Access {
                root,
                path,
                pass,
                moves_noncopy,
            } = action
            {
                if root != &p.name {
                    continue;
                }
                if path.is_empty() || *moves_noncopy || !parameter.struct_fields.contains(&path[0])
                {
                    blocked = true;
                    break;
                }
                let target = if *pass == Pass::Inout {
                    &mut writes
                } else {
                    &mut reads
                };
                if !target.contains(path) {
                    target.push(path.clone());
                }
            }
        }
        if blocked || writes.is_empty() {
            continue;
        }
        minimize(&mut writes);
        minimize(&mut reads);
        reads.retain(|read| !writes.iter().any(|write| read.starts_with(write)));
        if parameter
            .struct_fields
            .iter()
            .all(|field| writes.iter().any(|path| path == &vec![field.clone()]))
        {
            continue;
        }
        out.push(Finding {
            code: "L0101".into(), function: name.into(), span: p.span.clone(),
            message: format!("inout {} mutates {{{}}} and reads {{{}}}; narrower signature needs caller review (diagnostic only)", p.name, display_paths(&writes), display_paths(&reads)),
            detail: FindingKind::OverbroadInoutProjection { parameter: p.name.clone(), mutable_paths: writes, read_paths: reads },
        });
    }
    Ok(())
}
fn display_effect(effect: &Effect) -> String {
    let label = effect
        .label
        .as_ref()
        .map_or(String::new(), |label| format!("{label}: "));
    let args = if effect.args.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            effect
                .args
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{label}{}{args}", effect.name)
}
fn display_paths(paths: &[Vec<String>]) -> String {
    paths
        .iter()
        .map(|p| p.join("."))
        .collect::<Vec<_>>()
        .join(", ")
}

fn minimize(paths: &mut Vec<Vec<String>>) {
    paths.sort();
    let mut out: Vec<Vec<String>> = vec![];
    for path in paths.drain(..) {
        if !out.iter().any(|p| path.starts_with(p)) {
            out.push(path);
        }
    }
    *paths = out;
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceEdit {
    pub span: Span,
    pub expected: String,
    pub replacement: String,
}
/// Validate nonoverlap, exact original bytes, UTF-8 boundaries and output size.
/// Compiler rechecking and publication are mandatory facade responsibilities.
pub fn apply_edits(
    source: &str,
    edits: &[SourceEdit],
    max_bytes: usize,
) -> Result<String, LintError> {
    if edits.len() > 8192 || source.len() > max_bytes || max_bytes > 262144 {
        return Err(LintError("source/edit byte limit"));
    }
    let mut ordered: Vec<_> = edits.iter().collect();
    ordered.sort_by_key(|e| (e.span.start, e.span.end));
    let mut end = 0;
    let mut bytes = source.len();
    for e in &ordered {
        if e.span.start < end
            || e.span.start >= e.span.end
            || source.get(e.span.clone()) != Some(e.expected.as_str())
        {
            return Err(LintError("overlapping or stale source edit"));
        }
        end = e.span.end;
        bytes = bytes
            .checked_sub(e.span.len())
            .and_then(|n| n.checked_add(e.replacement.len()))
            .ok_or(LintError("edit size overflow"))?;
        if bytes > max_bytes {
            return Err(LintError("candidate byte limit"));
        }
    }
    let mut output = String::with_capacity(bytes);
    let mut cursor = 0;
    for e in ordered {
        output.push_str(&source[cursor..e.span.start]);
        output.push_str(&e.replacement);
        cursor = e.span.end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}
