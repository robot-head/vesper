//! Transitive `@realtime` effect confinement (design §32.1 item 1, §32.5).
//!
//! The source compiler cannot yet express `@realtime` roots, so roots and
//! their budgets come from an explicit [`RealtimePolicy`]. Each function in
//! the [`CallGraph`] carries its *own* declared effects (what its body does
//! directly, e.g. a call into the allocator is `Alloc`); the verifier then
//! computes everything reachable from each root, through direct calls and
//! through indirect calls whose targets the policy lists, and reports every
//! reachable violation with the call path that reaches it.
//!
//! The section's sketched `verify_realtime_effect_row` compares one effect
//! row against a name list. That is not confinement: a helper two calls deep
//! that allocates passes it. Here confinement is reachability.

use crate::VerifierFinding;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// Function identifier: an index into [`CallGraph::functions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

/// An effect a function body performs directly.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// The stateless realtime capability itself.
    Realtime,
    /// Flush-to-zero floating-point scope.
    ConstDenorm,
    /// The realtime bus (`poll_param_events`, wait-free retirement, DMA commit).
    RtBus,
    /// ISA-profile SIMD instructions.
    Isa,
    /// Memory-mapped register access.
    Mmio,
    /// Wait-free atomic loads/stores/RMW on preallocated memory.
    WaitFreeAtomic,
    /// Heap allocation (`malloc`, `GlobalAlloc::alloc`, growth of a collection).
    Alloc,
    /// Heap deallocation (destructors that free memory).
    Dealloc,
    /// Acquiring a lock that can block (mutex, rwlock, condvar wait).
    BlockingLock,
    /// A syscall that can block or page-fault (read, write, futex wait, mmap).
    BlockingSyscall,
    /// Suspension (`Async`, `Scope`).
    Async,
    /// Network I/O.
    Net,
    /// Storage I/O.
    Storage,
    /// Unwinding or panicking.
    Unwind,
    /// A foreign call whose behaviour is not described.
    Ffi,
    /// Any other named effect (rejected unless the policy allows it).
    Other(String),
}

impl Effect {
    /// The effects a realtime root may reach by default.
    pub fn default_allowed() -> BTreeSet<Effect> {
        [
            Effect::Realtime,
            Effect::ConstDenorm,
            Effect::RtBus,
            Effect::Isa,
            Effect::Mmio,
            Effect::WaitFreeAtomic,
        ]
        .into_iter()
        .collect()
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::Other(name) => f.write_str(name),
            other => write!(f, "{other:?}"),
        }
    }
}

/// What the analysis knows about a function's loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopBound {
    /// The body has no loops.
    NoLoops,
    /// Every loop runs at most this many iterations in total (attested by the
    /// policy or by the producer of the graph).
    Bounded(u64),
    /// The body has a loop with no known bound.
    Unbounded,
}

/// One function in the call graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnNode {
    /// Symbol name, used in findings.
    pub name: String,
    /// `false` for a symbol with no body and no effect declaration: reaching
    /// it from a realtime root is a finding.
    pub known: bool,
    /// Effects the body performs directly.
    pub effects: Vec<Effect>,
    /// Bytes of stack this function's own frame uses.
    pub frame_bytes: u64,
    /// Loop information.
    pub loops: LoopBound,
    /// Optional per-callee instruction index of the call (Cranelift input).
    pub call_sites: Vec<(FnId, u32)>,
}

impl FnNode {
    /// A known function with the given direct effects and frame size.
    pub fn new(name: impl Into<String>, effects: Vec<Effect>, frame_bytes: u64) -> Self {
        FnNode {
            name: name.into(),
            known: true,
            effects,
            frame_bytes,
            loops: LoopBound::NoLoops,
            call_sites: Vec::new(),
        }
    }

    /// A symbol nothing is known about.
    pub fn unknown(name: impl Into<String>) -> Self {
        FnNode {
            known: false,
            ..FnNode::new(name, Vec::new(), 0)
        }
    }

    /// Builder: set the loop information.
    pub fn with_loops(mut self, loops: LoopBound) -> Self {
        self.loops = loops;
        self
    }
}

/// An indirect call site (`call_indirect`, function pointer, vtable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndirectCall {
    /// The calling function.
    pub caller: FnId,
    /// Site number inside the caller (the Cranelift instruction index for
    /// Cranelift input).
    pub site: u32,
}

/// Call graph (wave-five shared vocabulary). Indirect calls are an explicit
/// edge kind the analysis rejects unless the policy lists their targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallGraph {
    /// Functions, indexed by [`FnId`].
    pub functions: Vec<FnNode>,
    /// Direct call edges `(caller, callee)`.
    pub calls: Vec<(FnId, FnId)>,
    /// Indirect call sites.
    pub indirect_calls: Vec<IndirectCall>,
}

impl CallGraph {
    /// Add a function and return its id.
    pub fn add(&mut self, node: FnNode) -> Result<FnId, GraphError> {
        let id = u32::try_from(self.functions.len()).map_err(|_| GraphError::TooManyFunctions)?;
        self.functions.push(node);
        Ok(FnId(id))
    }

    /// Add a direct call edge.
    pub fn call(&mut self, caller: FnId, callee: FnId) {
        self.calls.push((caller, callee));
    }

    /// Look a function up by name.
    pub fn by_name(&self, name: &str) -> Option<FnId> {
        self.functions
            .iter()
            .position(|f| f.name == name)
            .and_then(|i| u32::try_from(i).ok())
            .map(FnId)
    }

    fn node(&self, id: FnId) -> &FnNode {
        &self.functions[id.0 as usize]
    }
}

/// A realtime root and its budgets (the `@realtime(...)` attribute).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeRoot {
    /// The root function.
    pub function: FnId,
    /// `stack_budget_bytes`: maximum worst-case stack depth, if checked.
    pub stack_budget_bytes: Option<u64>,
}

/// Policy for annotations the source language cannot express yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimePolicy {
    /// Realtime roots.
    pub roots: Vec<RealtimeRoot>,
    /// Effects a root may reach. Defaults to [`Effect::default_allowed`].
    pub allowed_effects: BTreeSet<Effect>,
    /// Resolved targets of indirect call sites `(caller, site)`.
    pub indirect_targets: BTreeMap<(FnId, u32), Vec<FnId>>,
    /// Reject reachable loops with no known bound.
    pub reject_unbounded_loops: bool,
    /// Reject reachable loops whose bound exceeds this many iterations.
    pub max_loop_iterations: Option<u64>,
}

impl RealtimePolicy {
    /// Policy with the given roots, default allowed effects, and unbounded
    /// loops rejected.
    pub fn new(roots: Vec<RealtimeRoot>) -> Self {
        RealtimePolicy {
            roots,
            allowed_effects: Effect::default_allowed(),
            indirect_targets: BTreeMap::new(),
            reject_unbounded_loops: true,
            max_loop_iterations: None,
        }
    }

    /// Policy with a single root and no stack budget.
    pub fn root(function: FnId) -> Self {
        Self::new(vec![RealtimeRoot {
            function,
            stack_budget_bytes: None,
        }])
    }
}

/// Malformed verifier input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// An edge, root or indirect target names a function that does not exist.
    UnknownFnId(FnId),
    /// An indirect target was listed for a site that is not in the graph.
    UnknownIndirectSite {
        /// Caller of the listed site.
        caller: FnId,
        /// The listed site.
        site: u32,
    },
    /// More than `u32::MAX` functions.
    TooManyFunctions,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::UnknownFnId(id) => write!(f, "unknown function id {}", id.0),
            GraphError::UnknownIndirectSite { caller, site } => {
                write!(
                    f,
                    "indirect target listed for unknown site {} in fn {}",
                    site, caller.0
                )
            }
            GraphError::TooManyFunctions => {
                f.write_str("call graph has more than u32::MAX functions")
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// Rule identifiers.
pub mod rules {
    /// A reachable function performs an effect the policy does not allow.
    pub const FORBIDDEN_EFFECT: &str = "rt-forbidden-effect";
    /// A reachable symbol has no body and no effect declaration.
    pub const UNKNOWN_CALLEE: &str = "rt-unknown-callee";
    /// A reachable indirect call has no policy-listed targets.
    pub const UNRESOLVED_INDIRECT: &str = "rt-unresolved-indirect-call";
    /// A reachable loop has no bound.
    pub const UNBOUNDED_LOOP: &str = "rt-unbounded-loop";
    /// A reachable loop bound exceeds the policy maximum.
    pub const LOOP_BOUND: &str = "rt-loop-bound";
    /// The reachable subgraph is recursive, so stack depth is unbounded.
    pub const RECURSION: &str = "rt-recursion";
    /// Worst-case stack depth exceeds the root's budget (or overflows u64).
    pub const STACK_BUDGET: &str = "rt-stack-budget";
}

struct Adjacency {
    /// Successors per function: (callee, instruction index of the call).
    succ: Vec<Vec<(FnId, Option<u32>)>>,
    /// Unresolved indirect sites per function.
    unresolved: Vec<Vec<u32>>,
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
        let inst = graph
            .node(caller)
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
///
/// Returns one finding per (root, reachable violation). An empty vector means
/// every root is confined. Malformed graphs are an error, never success.
pub fn verify_realtime(
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
    let root_name = &graph.node(root.function).name;
    let path_to = |v: FnId| -> (String, Option<u32>) {
        let mut names = vec![graph.node(v).name.as_str()];
        let mut cur = v;
        let mut first_inst = None;
        while let Some((p, inst)) = parent[cur.0 as usize] {
            names.push(graph.node(p).name.as_str());
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
        let node = graph.node(v);
        if !node.known {
            push(
                findings,
                v,
                rules::UNKNOWN_CALLEE,
                format!("unknown callee `{}` with no effect declaration", node.name),
            );
            continue;
        }
        let mut reported = BTreeSet::new();
        for eff in &node.effects {
            if !policy.allowed_effects.contains(eff) && reported.insert(eff) {
                push(
                    findings,
                    v,
                    rules::FORBIDDEN_EFFECT,
                    format!("forbidden effect `{eff}` in `{}`", node.name),
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
                    node.name
                ),
            );
        }
        match node.loops {
            LoopBound::NoLoops => {}
            LoopBound::Unbounded if policy.reject_unbounded_loops => push(
                findings,
                v,
                rules::UNBOUNDED_LOOP,
                format!("a loop with no known bound in `{}`", node.name),
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
                            node.name
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
                graph.node(v).name
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
            depth[u.0 as usize] = graph
                .node(u)
                .frame_bytes
                .checked_add(deepest_callee)
                .ok_or(StackError::Overflow)?;
            color[u.0 as usize] = BLACK;
            stack.pop();
        }
    }
    Ok(depth[root.0 as usize])
}
