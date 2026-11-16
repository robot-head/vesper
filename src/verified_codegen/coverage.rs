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

/// The detail `lower` gives a call to a function outside the plan.
const UNPLANNED: &str = "call to unplanned function ";

/// Cover `roots` (every function with a body when `None`) and everything
/// they call.
pub fn coverage(
    world: &World,
    roots: Option<&BTreeSet<String>>,
    optimize: bool,
) -> Result<Coverage, VerifiedError> {
    let mut program = crate::ir::lower_world(world);
    if optimize {
        crate::ir_opt::optimize(world, &mut program).map_err(VerifiedError::Invalid)?;
    }
    let mut names: BTreeSet<String> = match roots {
        Some(r) => r.clone(),
        None => program.functions.keys().cloned().collect(),
    };
    let mut queue: Vec<String> = names.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        if let Some(f) = program.functions.get(&name) {
            for c in lower::callees(f) {
                if names.insert(c.clone()) {
                    queue.push(c.clone());
                }
            }
        }
    }
    let mut refusals: BTreeMap<String, Vec<VerifiedError>> = BTreeMap::new();
    let mut plan = lower::Plan {
        functions: vec![],
        index: BTreeMap::new(),
    };
    let mut label = super::machine::FIRST_FREE_LABEL;
    for name in &names {
        let mut own = lower::signature_refusals(world, &program, name);
        // A function refused only for its effect row is still lowered, so
        // its body's refusals are listed too.
        let effects_only = own.iter().all(|e| {
            matches!(
                e,
                VerifiedError::Unsupported {
                    construct: Unsupported::Effect,
                    ..
                }
            )
        });
        if effects_only {
            match lower::planned_with(world, &program, name, label, false) {
                Ok(p) => {
                    plan.index.insert(name.clone(), plan.functions.len());
                    plan.functions.push(p);
                    label += 2;
                }
                Err(e) => own.push(e),
            }
        }
        refusals.insert(name.clone(), own);
    }
    let mut rows = BTreeMap::new();
    let mut next_label = label;
    for index in 0..plan.functions.len() {
        let name = plan.functions[index].name.clone();
        let body =
            lower::function_refusals(world, &program, &plan, index, &mut next_label, usize::MAX);
        let own = refusals.get_mut(&name).expect("planned function");
        match body {
            Ok((lowered, found)) => {
                let found: Vec<_> = found
                    .into_iter()
                    .filter(|e| {
                        !matches!(e, VerifiedError::Unsupported {
                            construct: Unsupported::ExternalFunction,
                            detail,
                            ..
                        } if detail.starts_with(UNPLANNED))
                    })
                    .collect();
                if found.is_empty() {
                    rows.insert(name, lowered.rows.len());
                }
                own.extend(found);
            }
            Err(e) => own.push(e),
        }
    }
    // A function compiles when it and all its callees have no refusal.
    let mut broken: BTreeSet<String> = refusals
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    loop {
        let more: Vec<String> = names
            .iter()
            .filter(|n| !broken.contains(*n))
            .filter(|n| {
                program
                    .functions
                    .get(*n)
                    .is_some_and(|f| lower::callees(f).any(|c| broken.contains(c)))
            })
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
            refused_callees: program
                .functions
                .get(name)
                .map(|f| {
                    lower::callees(f)
                        .filter(|c| *c != name && broken.contains(*c))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            rows: rows.get(name).copied().unwrap_or(0),
        })
        .collect();
    Ok(Coverage { functions })
}
