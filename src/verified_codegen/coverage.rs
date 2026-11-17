//! Coverage of a whole checked program by the verified backend: every
//! refusal of every function, not only the first one [`super::compile`]
//! reports. The self-hosted front end's inventory
//! (`tests/verified_codegen_front_end.rs`) is built with this.
//!
//! Each function is planned on its own signature and lowered in coverage
//! mode, which records a refusal and keeps lowering past it. A function
//! *compiles* when it has no refusal of its own and every function it calls
//! compiles. Calls to a refused function are listed as such, not as a
//! refusal of the caller.
use super::{Unsupported, VerifiedError, lower};
use crate::check::World;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// One function's coverage.
#[derive(Clone, Debug)]
pub struct FunctionCoverage {
    pub name: String,
    /// Refusals of this function itself (signature and body), in order.
    pub refusals: Vec<VerifiedError>,
    /// Direct callees that do not compile.
    pub refused_callees: BTreeSet<String>,
    /// Slot-IR rows when the body lowered without refusal.
    pub rows: usize,
}

/// The coverage of a set of functions and everything they call.
#[derive(Clone, Debug)]
pub struct Coverage {
    /// Sorted by name.
    pub functions: Vec<FunctionCoverage>,
}

impl Coverage {
    pub fn get(&self, name: &str) -> Option<&FunctionCoverage> {
        self.functions
            .binary_search_by(|f| f.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.functions[i])
    }
    /// Whether `name` and everything it calls compiles.
    pub fn compiles(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|f| f.refusals.is_empty() && f.refused_callees.is_empty())
    }
    /// Every refusal, in function order.
    pub fn refusals(&self) -> impl Iterator<Item = &VerifiedError> {
        self.functions.iter().flat_map(|f| &f.refusals)
    }
    /// Refusal counts per construct (`Limit`/`Invalid` errors as such).
    pub fn by_construct(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for e in self.refusals() {
            *out.entry(construct_name(e)).or_default() += 1;
        }
        out
    }
    /// Refusal counts per `(construct, detail)`.
    pub fn by_detail(&self) -> BTreeMap<(String, String), usize> {
        let mut out = BTreeMap::new();
        for e in self.refusals() {
            *out.entry((construct_name(e), detail(e))).or_default() += 1;
        }
        out
    }
    /// A deterministic text listing: a summary, then one entry per function.
    pub fn listing(&self) -> String {
        let total = self.functions.len();
        let own = self
            .functions
            .iter()
            .filter(|f| f.refusals.is_empty())
            .count();
        let whole = self
            .functions
            .iter()
            .filter(|f| self.compiles(&f.name))
            .count();
        let mut out = String::new();
        let _ = writeln!(out, "functions: {total}");
        let _ = writeln!(out, "without own refusal: {own}");
        let _ = writeln!(out, "compiling with all callees: {whole}");
        let _ = writeln!(out, "refusals: {}", self.refusals().count());
        for (c, n) in self.by_construct() {
            let _ = writeln!(out, "  {c}: {n}");
        }
        let _ = writeln!(out, "by detail:");
        let mut details: Vec<_> = self.by_detail().into_iter().collect();
        details.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for ((c, d), n) in details {
            let _ = writeln!(out, "  {n:>4} {c}: {d}");
        }
        let _ = writeln!(out);
        for f in &self.functions {
            if f.refusals.is_empty() && f.refused_callees.is_empty() {
                let _ = writeln!(out, "{}: ok, {} rows", f.name, f.rows);
                continue;
            }
            let _ = writeln!(out, "{}:", f.name);
            for e in &f.refusals {
                let _ = writeln!(out, "  {}: {}", construct_name(e), detail(e));
            }
            if !f.refused_callees.is_empty() {
                let _ = writeln!(
                    out,
                    "  calls refused: {}",
                    f.refused_callees
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        out
    }
}

fn construct_name(e: &VerifiedError) -> String {
    match e {
        VerifiedError::Unsupported { construct, .. } => format!("{construct:?}"),
        VerifiedError::Limit(_) => "Limit".into(),
        VerifiedError::Invalid(_) => "Invalid".into(),
        VerifiedError::Entry(_) => "Entry".into(),
        VerifiedError::Target(_) => "Target".into(),
    }
}

fn detail(e: &VerifiedError) -> String {
    match e {
        VerifiedError::Unsupported { detail, .. } => {
            // Type renderings of region capabilities can be kilobytes long.
            if detail.chars().count() > 120 {
                format!("{}...", detail.chars().take(117).collect::<String>())
            } else {
                detail.clone()
            }
        }
        VerifiedError::Limit(d)
        | VerifiedError::Invalid(d)
        | VerifiedError::Entry(d)
        | VerifiedError::Target(d) => d.clone(),
    }
}

/// The user effects in the row of the checked function `name`.
fn program_effects(world: &World, name: &str) -> Vec<String> {
    world
        .functions
        .get(name)
        .map(|f| {
            f.effects
                .instances
                .iter()
                .filter(|e| super::evidence::is_user_effect(e))
                .map(|e| e.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The detail `lower` gives a call to a function outside the plan.
const UNPLANNED: &str = "call to unplanned function ";

/// Cover `roots` (every function with a body when `None`) and everything
/// they call, including the nested functions of `handle` expressions.
pub fn coverage(
    world: &World,
    roots: Option<&BTreeSet<String>>,
    optimize: bool,
) -> Result<Coverage, VerifiedError> {
    let mut program = crate::ir::lower_world(world);
    if optimize {
        crate::ir_opt::optimize(world, &mut program).map_err(VerifiedError::Invalid)?;
    }
    let roots: Vec<String> = match roots {
        Some(r) => r.iter().cloned().collect(),
        None => program
            .functions
            .keys()
            .filter(|n| !n.starts_with('$'))
            .cloned()
            .collect(),
    };
    let mut refusals: BTreeMap<String, Vec<VerifiedError>> = BTreeMap::new();
    // Monomorphize from every non-generic root; instances reached from
    // several roots are the same copy.
    let mut mono = crate::ir::Program::default();
    let mut entries = vec![];
    // A function whose row has a user effect is no entry: only instances of
    // it specialized for a handler (`evidence.rs`) are planned.
    let mut polymorphic = BTreeSet::new();
    for root in &roots {
        if world
            .functions
            .get(root)
            .is_some_and(|f| !f.generics.is_empty())
        {
            continue;
        }
        if program.functions.get(root).is_some_and(|f| {
            f.effects
                .instances
                .iter()
                .any(super::evidence::is_user_effect)
        }) {
            polymorphic.insert(root.clone());
            continue;
        }
        match super::mono::monomorphize(world, &program, root, usize::MAX) {
            Ok(p) => {
                mono.functions.extend(p.functions);
                entries.push(root.clone());
            }
            Err(e) => refusals.entry(root.clone()).or_default().push(e),
        }
    }
    let mut program = mono;
    let mut handler_refusals = BTreeMap::new();
    let sites =
        super::evidence::rewrite(world, &mut program, &entries, Some(&mut handler_refusals))?;
    let plan = lower::plan_with(
        world,
        &program,
        &entries,
        usize::MAX,
        &sites,
        Some(&mut refusals),
    )?;
    // A handler the rewrite refused is also refused by the planner, with
    // less detail.
    for (name, errors) in handler_refusals {
        let own = refusals.entry(name).or_default();
        own.retain(|e| {
            !matches!(e, VerifiedError::Unsupported { detail, .. } if detail.contains("(only Fail handlers)"))
        });
        own.extend(errors);
    }
    // Effect-polymorphic functions no handler instantiates.
    for root in &polymorphic {
        let instanced = program.functions.keys().any(|n| {
            n.strip_prefix(root.as_str())
                .is_some_and(|r| r.starts_with('<'))
        });
        if !instanced {
            let effects: Vec<String> = program_effects(world, root);
            refusals
                .entry(root.clone())
                .or_default()
                .push(VerifiedError::unsupported(
                    Unsupported::Effect,
                    root,
                    format!(
                        "effect {} has no handler in the program (only its host handles it)",
                        effects.join(", ")
                    ),
                ));
        }
    }
    // Everything the plan reached or refused.
    let mut names: BTreeSet<String> = refusals.keys().cloned().collect();
    names.extend(plan.functions.iter().map(|f| f.name.clone()));
    let mut rows = BTreeMap::new();
    let mut next_label = plan
        .functions
        .last()
        .map(|f| f.label + 2)
        .unwrap_or(super::machine::FIRST_FREE_LABEL);
    for index in 0..plan.functions.len() {
        let name = plan.functions[index].name.clone();
        let mut found = vec![];
        let lowered =
            lower::function_refusals(world, &program, &plan, index, &mut next_label, &mut found);
        found.retain(|e| {
            !matches!(e, VerifiedError::Unsupported {
                construct: Unsupported::ExternalFunction,
                detail,
                ..
            } if detail.starts_with(UNPLANNED))
                && !matches!(e, VerifiedError::Invalid(d) if d.starts_with("unplanned "))
        });
        let own = refusals.entry(name.clone()).or_default();
        // A broken invariant after a planning refusal is its consequence.
        if !own.is_empty() {
            found.retain(|e| !matches!(e, VerifiedError::Invalid(_)));
        }
        if let (Some(l), true) = (lowered, found.is_empty() && own.is_empty()) {
            rows.insert(name, l.rows.len());
        }
        own.extend(found);
    }
    // The functions each one depends on: direct calls and the nested
    // functions of its `handle` expressions.
    let deps = |name: &str| -> Vec<String> {
        let Some(f) = program.functions.get(name) else {
            return vec![];
        };
        let mut out: Vec<String> = lower::callees(f).cloned().collect();
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let crate::ir::Op::Handle { expr, .. } = &i.op
                && let Some(h) =
                    plan.handlers
                        .get(&(name.to_owned(), expr.span.start, expr.span.end))
            {
                out.extend([h.body.clone(), h.ret.clone(), h.raise.clone()]);
            }
        }
        out.retain(|c| c != name);
        out
    };
    let mut broken: BTreeSet<String> = refusals
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    loop {
        let more: Vec<String> = names
            .iter()
            .filter(|n| !broken.contains(*n))
            .filter(|n| deps(n).iter().any(|c| broken.contains(c)))
            .cloned()
            .collect();
        if more.is_empty() {
            break;
        }
        broken.extend(more);
    }
    let functions = names
        .iter()
        .map(|name| FunctionCoverage {
            name: name.clone(),
            refusals: refusals.remove(name).unwrap_or_default(),
            refused_callees: deps(name)
                .into_iter()
                .filter(|c| broken.contains(c))
                .collect(),
            rows: rows.get(name).copied().unwrap_or(0),
        })
        .collect();
    Ok(Coverage { functions })
}
