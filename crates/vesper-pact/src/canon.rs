//! Canonical goals: inference variables renumbered `?0, ?1, ...` in order of
//! first occurrence so that alpha-equivalent queries share one table entry.

use crate::infer::InferCtxt;
use crate::program::{Env, PlaceholderDecl};
use crate::term::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalGoal {
    /// Kinds of the canonical existential variables `?0..?n`.
    pub binders: Vec<ParamKind>,
    pub goal: Goal,
}

/// Renames free inference variables (after resolution through a context) in
/// first-occurrence order across every term it is applied to.
pub(crate) struct Canonicalizer<'a> {
    cx: Option<&'a InferCtxt>,
    map: HashMap<u32, u32>,
    pub binders: Vec<ParamKind>,
    /// For each canonical binder, the original inference variable.
    pub origins: Vec<u32>,
}

impl<'a> Canonicalizer<'a> {
    pub fn new(cx: Option<&'a InferCtxt>) -> Self {
        Canonicalizer {
            cx,
            map: HashMap::new(),
            binders: vec![],
            origins: vec![],
        }
    }
}

impl VarFolder for Canonicalizer<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        match var {
            Var::Infer(i) => {
                let next = self.binders.len() as u32;
                let j = *self.map.entry(i).or_insert_with(|| {
                    self.binders.push(kind);
                    self.origins.push(i);
                    next
                });
                debug_assert!(self.cx.is_none_or(|cx| cx.kind_of(i) == kind));
                GenericArg::var(kind, Var::Infer(j))
            }
            other => GenericArg::var(kind, other),
        }
    }
}

/// Canonicalizes a goal under an inference context. Returns the canonical
/// goal and, per canonical binder, the context variable it stands for.
pub(crate) fn canonicalize_goal(
    cx: &InferCtxt,
    g: &Goal,
) -> Result<(CanonicalGoal, Vec<u32>), DuplicateLabel> {
    let resolved = cx.resolve_goal(g)?;
    let mut c = Canonicalizer::new(Some(cx));
    let goal = fold_goal(&resolved, &mut c)?;
    Ok((
        CanonicalGoal {
            binders: c.binders,
            goal,
        },
        c.origins,
    ))
}

/// Canonicalizes rigid placeholders of a top-level query: placeholders are
/// renumbered in first-occurrence order (goal first, then assumptions), and
/// unused placeholders and empty model scopes are dropped. Returns the new
/// environment and goal, and for each new placeholder its original index.
pub(crate) fn canonicalize_placeholders(
    env: &Env,
    goal: &Goal,
) -> Result<(Env, Goal, Vec<u32>), DuplicateLabel> {
    let mut order: Vec<u32> = Vec::new();
    let mut note = |v: Var, _k: ParamKind| {
        if let Var::Placeholder(i) = v
            && !order.contains(&i)
        {
            order.push(i);
        }
    };
    visit_goal_vars(goal, &mut note);
    for a in &env.assumptions {
        visit_goal_vars(a, &mut note);
    }
    let rename = |order: &[u32]| {
        let order = order.to_vec();
        move |v: Var, k: ParamKind| match v {
            Var::Placeholder(i) => {
                let j = order
                    .iter()
                    .position(|&o| o == i)
                    .expect("placeholder collected") as u32;
                GenericArg::var(k, Var::Placeholder(j))
            }
            other => GenericArg::var(k, other),
        }
    };
    let goal = fold_goal(goal, &mut rename(&order))?;
    let assumptions = env
        .assumptions
        .iter()
        .map(|a| fold_goal(a, &mut rename(&order)))
        .collect::<Result<Vec<_>, _>>()?;
    let placeholders: Vec<PlaceholderDecl> = order
        .iter()
        .map(|&i| env.placeholders[i as usize].clone())
        .collect();
    let model_scopes = env
        .model_scopes
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect();
    Ok((
        Env {
            placeholders,
            assumptions,
            model_scopes,
        },
        goal,
        order,
    ))
}

/// The table key a top-level query maps to: its placeholder-canonical
/// environment and canonical goal. Alpha-equivalent queries (renamed
/// inference variables or placeholders, unused placeholders) share a key.
pub fn canonicalize_query(
    query: &crate::program::QueryDto,
) -> Result<(Env, CanonicalGoal), DuplicateLabel> {
    let (env, goal, _) = canonicalize_placeholders(&query.env, &query.goal)?;
    let mut c = Canonicalizer::new(None);
    let goal = fold_goal(&goal, &mut c)?;
    Ok((
        env,
        CanonicalGoal {
            binders: c.binders,
            goal,
        },
    ))
}
