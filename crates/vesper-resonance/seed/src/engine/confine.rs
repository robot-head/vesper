//! The frozen Rust realtime confinement verifier
//! (`confine::verify_realtime`), which the Vesper port
//! (`vesper/src/confine.ves`) replaced in the package `vesper-resonance`
//! (SH6 lane 9, #1404).

use crate::VerifierFinding;
use crate::confine::{
    CallGraph, FnId, FnNode, GraphError, LoopBound, RealtimePolicy, RealtimeRoot, rules,
};
use std::collections::{BTreeSet, VecDeque};

struct Adjacency {
    /// Successors per function: (callee, instruction index of the call).
    succ: Vec<Vec<(FnId, Option<u32>)>>,
    /// Unresolved indirect sites per function.
    unresolved: Vec<Vec<u32>>,
}

fn node(graph: &CallGraph, id: FnId) -> &FnNode {
    &graph.functions[id.0 as usize]
}

fn adjacency(graph: &CallGraph, policy: &RealtimePolicy) -> Result<Adjacency, GraphError> {
    let n = graph.functions.len();
    let check = |id: FnId| {
        if (id.0 as usize) < n {
            Ok(id)
        } else {
            Err(GraphError::UnknownFnId(id))
        }
    };
    let mut succ: Vec<Vec<(FnId, Option<u32>)>> = vec![Vec::new(); n];
    let mut unresolved: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(caller, callee) in &graph.calls {
        check(caller)?;
        check(callee)?;
        let inst = node(graph, caller)
            .call_sites
            .iter()
            .find(|(c, _)| *c == callee)
            .map(|&(_, i)| i);
        succ[caller.0 as usize].push((callee, inst));
    }
    let mut sites = BTreeSet::new();
    for ic in &graph.indirect_calls {
        check(ic.caller)?;
        sites.insert((ic.caller, ic.site));
        match policy.indirect_targets.get(&(ic.caller, ic.site)) {
            // An empty target list resolves nothing: still unresolved.
            Some(targets) if targets.is_empty() => {
                unresolved[ic.caller.0 as usize].push(ic.site);
            }
            Some(targets) => {
                for &t in targets {
                    check(t)?;
                    succ[ic.caller.0 as usize].push((t, Some(ic.site)));
                }
            }
            None => unresolved[ic.caller.0 as usize].push(ic.site),
        }
    }
    for &(caller, site) in policy.indirect_targets.keys() {
        check(caller)?;
        if !sites.contains(&(caller, site)) {
            return Err(GraphError::UnknownIndirectSite { caller, site });
        }
    }
    for root in &policy.roots {
        check(root.function)?;
    }
    Ok(Adjacency { succ, unresolved })
}

/// Verify every realtime root of `policy` against `graph`.
pub(crate) fn verify_realtime(
    graph: &CallGraph,
    policy: &RealtimePolicy,
) -> Result<Vec<VerifierFinding>, GraphError> {
    let adj = adjacency(graph, policy)?;
    let mut findings = Vec::new();
    for root in &policy.roots {
        verify_root(graph, policy, &adj, root, &mut findings);
    }
    Ok(findings)
}

fn verify_root(
    graph: &CallGraph,
    policy: &RealtimePolicy,
    adj: &Adjacency,
    root: &RealtimeRoot,
    findings: &mut Vec<VerifierFinding>,
) {
    let n = graph.functions.len();
    // BFS for shortest call paths; `parent[v] = (u, inst of call u->v)`.
    let mut parent: Vec<Option<(FnId, Option<u32>)>> = vec![None; n];
    let mut seen = vec![false; n];
    let mut order = Vec::new();
    let mut queue = VecDeque::new();
    seen[root.function.0 as usize] = true;
    queue.push_back(root.function);
    while let Some(u) = queue.pop_front() {
        order.push(u);
        for &(v, inst) in &adj.succ[u.0 as usize] {
            if !seen[v.0 as usize] {
                seen[v.0 as usize] = true;
                parent[v.0 as usize] = Some((u, inst));
                queue.push_back(v);
            }
        }
    }
    let root_name = &node(graph, root.function).name;
    let path_to = |v: FnId| -> (String, Option<u32>) {
        let mut names = vec![node(graph, v).name.as_str()];
        let mut cur = v;
        let mut first_inst = None;
        while let Some((p, inst)) = parent[cur.0 as usize] {
            names.push(node(graph, p).name.as_str());
            if p == root.function {
                first_inst = inst;
            }
            cur = p;
        }
        names.reverse();
        (names.join(" -> "), first_inst)
    };
    // Findings name the rejected root; `inst` is the root's call instruction
    // that starts the offending path (None for the root's own body).
    let push = |findings: &mut Vec<VerifierFinding>, v: FnId, rule, what: String| {
        let (path, root_inst) = path_to(v);
        findings.push(VerifierFinding {
            function: root_name.clone(),
            inst: root_inst,
            rule,
            message: format!("@realtime root `{root_name}` reaches {what} via {path}"),
        });
    };

    for &v in &order {
        let node_ref = node(graph, v);
        if !node_ref.known {
            push(
                findings,
                v,
                rules::UNKNOWN_CALLEE,
                format!(
                    "unknown callee `{}` with no effect declaration",
                    node_ref.name
                ),
            );
            continue;
        }
        let mut reported = BTreeSet::new();
        for eff in &node_ref.effects {
            if !policy.allowed_effects.contains(eff) && reported.insert(eff) {
                push(
                    findings,
                    v,
                    rules::FORBIDDEN_EFFECT,
                    format!("forbidden effect `{eff}` in `{}`", node_ref.name),
                );
            }
        }
        for &site in &adj.unresolved[v.0 as usize] {
            push(
                findings,
                v,
                rules::UNRESOLVED_INDIRECT,
                format!(
                    "indirect call site {site} in `{}` with no policy-listed targets",
                    node_ref.name
                ),
            );
        }
        match node_ref.loops {
            LoopBound::NoLoops => {}
            LoopBound::Unbounded if policy.reject_unbounded_loops => push(
                findings,
                v,
                rules::UNBOUNDED_LOOP,
                format!("a loop with no known bound in `{}`", node_ref.name),
            ),
            LoopBound::Unbounded => {}
            LoopBound::Bounded(b) => {
                if let Some(max) = policy.max_loop_iterations
                    && b > max
                {
                    push(
                        findings,
                        v,
                        rules::LOOP_BOUND,
                        format!(
                            "a loop bounded by {b} > policy maximum {max} in `{}`",
                            node_ref.name
                        ),
                    );
                }
            }
        }
    }

    // Worst-case stack depth over the reachable subgraph (iterative DFS,
    // recursion detection, checked sums).
    match worst_case_stack(graph, adj, root.function, &seen) {
        Ok(depth) => {
            if let Some(budget) = root.stack_budget_bytes
                && depth > budget
            {
                push(
                    findings,
                    root.function,
                    rules::STACK_BUDGET,
                    format!("worst-case stack depth {depth} bytes over budget {budget}"),
                );
            }
        }
        Err(StackError::Recursion(v)) => push(
            findings,
            v,
            rules::RECURSION,
            format!(
                "recursion through `{}` (unbounded stack depth)",
                node(graph, v).name
            ),
        ),
        Err(StackError::Overflow) => push(
            findings,
            root.function,
            rules::STACK_BUDGET,
            "a worst-case stack depth that overflows u64".to_string(),
        ),
    }
}

enum StackError {
    Recursion(FnId),
    Overflow,
}

/// Longest frame-byte path from `root` (inclusive), or the recursion found.
fn worst_case_stack(
    graph: &CallGraph,
    adj: &Adjacency,
    root: FnId,
    reachable: &[bool],
) -> Result<u64, StackError> {
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    let n = graph.functions.len();
    let mut color = vec![WHITE; n];
    let mut depth = vec![0u64; n];
    // Stack of (node, next successor index).
    let mut stack: Vec<(FnId, usize)> = vec![(root, 0)];
    color[root.0 as usize] = GREY;
    while let Some(&mut (u, ref mut next)) = stack.last_mut() {
        let succ = &adj.succ[u.0 as usize];
        if *next < succ.len() {
            let (v, _) = succ[*next];
            *next += 1;
            debug_assert!(reachable[v.0 as usize]);
            match color[v.0 as usize] {
                WHITE => {
                    color[v.0 as usize] = GREY;
                    stack.push((v, 0));
                }
                GREY => return Err(StackError::Recursion(v)),
                _ => {}
            }
        } else {
            let deepest_callee = succ
                .iter()
                .map(|&(v, _)| depth[v.0 as usize])
                .max()
                .unwrap_or(0);
            depth[u.0 as usize] = node(graph, u)
                .frame_bytes
                .checked_add(deepest_callee)
                .ok_or(StackError::Overflow)?;
            color[u.0 as usize] = BLACK;
            stack.pop();
        }
    }
    Ok(depth[root.0 as usize])
}
