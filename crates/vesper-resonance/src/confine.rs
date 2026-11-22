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
#[cfg(vesper_port)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
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

/// Verify every realtime root of `policy` against `graph`.
///
/// Returns one finding per (root, reachable violation). An empty vector means
/// every root is confined. Malformed graphs are an error, never success.
///
/// The seed runs the Rust verifier (`seed/src/engine/confine.rs`); this
/// package calls `vesper/src/confine.ves`.
pub fn verify_realtime(
    graph: &CallGraph,
    policy: &RealtimePolicy,
) -> Result<Vec<VerifierFinding>, GraphError> {
    #[cfg(vesper_port)]
    {
        // Number the distinct effects via a hash index; keep `effects` for
        // decoding findings.
        let mut effects: Vec<&Effect> = Vec::new();
        let mut effect_ids: HashMap<&Effect, u64> = HashMap::new();
        fn intern<'a>(
            effects: &mut Vec<&'a Effect>,
            effect_ids: &mut HashMap<&'a Effect, u64>,
            e: &'a Effect,
        ) -> u64 {
            *effect_ids.entry(e).or_insert_with(|| {
                let id = effects.len() as u64;
                effects.push(e);
                id
            })
        }
        let mut fns = Vec::with_capacity(8 * graph.functions.len());
        let mut pool = Vec::new();
        for f in &graph.functions {
            let (loops, bound) = match f.loops {
                LoopBound::NoLoops => (0, 0),
                LoopBound::Bounded(b) => (1, b),
                LoopBound::Unbounded => (2, 0),
            };
            let eff_first = pool.len() as u64;
            for e in &f.effects {
                let c = intern(&mut effects, &mut effect_ids, e);
                pool.push(c);
            }
            let site_first = pool.len() as u64;
            for &(callee, inst) in &f.call_sites {
                pool.extend_from_slice(&[u64::from(callee.0), u64::from(inst)]);
            }
            fns.extend_from_slice(&[
                u64::from(f.known),
                f.frame_bytes,
                loops,
                bound,
                eff_first,
                f.effects.len() as u64,
                site_first,
                f.call_sites.len() as u64,
            ]);
        }
        let calls: Vec<u64> = graph
            .calls
            .iter()
            .flat_map(|&(a, b)| [u64::from(a.0), u64::from(b.0)])
            .collect();
        let indirect: Vec<u64> = graph
            .indirect_calls
            .iter()
            .flat_map(|ic| [u64::from(ic.caller.0), u64::from(ic.site)])
            .collect();
        let mut entries = Vec::with_capacity(4 * policy.indirect_targets.len());
        for (&(caller, site), targets) in &policy.indirect_targets {
            entries.extend_from_slice(&[
                u64::from(caller.0),
                u64::from(site),
                pool.len() as u64,
                targets.len() as u64,
            ]);
            pool.extend(targets.iter().map(|t| u64::from(t.0)));
        }
        let roots: Vec<u64> = policy
            .roots
            .iter()
            .flat_map(|r| {
                [
                    u64::from(r.function.0),
                    u64::from(r.stack_budget_bytes.is_some()),
                    r.stack_budget_bytes.unwrap_or(0),
                ]
            })
            .collect();
        let allowed: Vec<u64> = policy
            .allowed_effects
            .iter()
            .map(|e| intern(&mut effects, &mut effect_ids, e))
            .collect();
        let w = crate::port::verify_realtime(
            &fns,
            &pool,
            &calls,
            &indirect,
            &entries,
            &roots,
            &allowed,
            policy.reject_unbounded_loops,
            policy.max_loop_iterations.is_some(),
            policy.max_loop_iterations.unwrap_or(0),
        );
        match w[0] {
            0 => {}
            1 => return Err(GraphError::UnknownFnId(FnId(w[1] as u32))),
            2 => {
                return Err(GraphError::UnknownIndirectSite {
                    caller: FnId(w[1] as u32),
                    site: w[2] as u32,
                });
            }
            status => {
                panic!("the Vesper port of vesper-resonance returned verifier status {status}")
            }
        }
        let name = |f: u64| graph.functions[f as usize].name.as_str();
        let mut findings = Vec::with_capacity(w[1] as usize);
        let mut k = 2;
        for _ in 0..w[1] {
            let f = &w[k..k + 8];
            let path_len = f[7] as usize;
            let path = w[k + 8..k + 8 + path_len]
                .iter()
                .map(|&p| name(p))
                .collect::<Vec<_>>()
                .join(" -> ");
            k += 8 + path_len;
            let (rule, what) = match f[3] {
                0 => (
                    rules::FORBIDDEN_EFFECT,
                    format!(
                        "forbidden effect `{}` in `{}`",
                        effects[f[5] as usize],
                        name(f[4])
                    ),
                ),
                1 => (
                    rules::UNKNOWN_CALLEE,
                    format!("unknown callee `{}` with no effect declaration", name(f[4])),
                ),
                2 => (
                    rules::UNRESOLVED_INDIRECT,
                    format!(
                        "indirect call site {} in `{}` with no policy-listed targets",
                        f[5],
                        name(f[4])
                    ),
                ),
                3 => (
                    rules::UNBOUNDED_LOOP,
                    format!("a loop with no known bound in `{}`", name(f[4])),
                ),
                4 => (
                    rules::LOOP_BOUND,
                    format!(
                        "a loop bounded by {} > policy maximum {} in `{}`",
                        f[5],
                        f[6],
                        name(f[4])
                    ),
                ),
                5 => (
                    rules::RECURSION,
                    format!("recursion through `{}` (unbounded stack depth)", name(f[4])),
                ),
                6 => (
                    rules::STACK_BUDGET,
                    format!("worst-case stack depth {} bytes over budget {}", f[5], f[6]),
                ),
                7 => (
                    rules::STACK_BUDGET,
                    "a worst-case stack depth that overflows u64".to_string(),
                ),
                rule => panic!("the Vesper port of vesper-resonance returned rule {rule}"),
            };
            let root_name = name(f[0]);
            findings.push(VerifierFinding {
                function: root_name.to_string(),
                inst: (f[1] == 1).then_some(f[2] as u32),
                rule,
                message: format!("@realtime root `{root_name}` reaches {what} via {path}"),
            });
        }
        Ok(findings)
    }
    #[cfg(not(vesper_port))]
    crate::engine::confine::verify_realtime(graph, policy)
}
