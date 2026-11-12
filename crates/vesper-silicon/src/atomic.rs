//! Sleep-in-atomic, ISR-suspension and lock-order checks over Cranelift
//! functions and their [`CallGraph`] (§26.1 item 3).
//!
//! Atomic context is: anywhere between a guard-acquire call and its matching
//! guard-release call, anywhere a spinlock is held, and every function
//! reachable from an `@isr` root. In atomic context the checker rejects a
//! call to a suspending or allocating symbol, and a call to any function
//! that can transitively reach one (the finding names the path). It also
//! rejects taking an ISR-shared spinlock outside ISR context without an
//! interrupt guard, unbalanced guard/lock state at joins and returns, and
//! cycles in the lock-acquisition order (including order induced through
//! callees).
//!
//! The analysis is intraprocedural for guard and lock *state* (each function
//! must release what it takes before returning, the IR-level counterpart of
//! the source rule that a linear `IrqGuard` cannot escape its scope) and
//! interprocedural for reachability. Symbols are the canonical names from
//! [`crate::stack::symbol_of_user_func`].

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::{Block, Function};

use crate::VerifierFinding;
use crate::stack::{CallGraph, CallSite, Program, call_site, tarjan};

/// One spinlock, identified by its acquire/release symbols.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockSpec {
    pub name: String,
    pub acquire: String,
    pub release: String,
    /// Also taken by an ISR: thread context must mask interrupts first.
    pub isr_shared: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AtomicPolicy {
    /// `CpuIrq::disable_local_save` symbols.
    pub guard_acquire: Vec<String>,
    /// `CpuIrq::restore_local` symbols.
    pub guard_release: Vec<String>,
    pub locks: Vec<LockSpec>,
    /// Symbols that suspend the current fiber (Async, DmaWait, Storage).
    pub suspending: Vec<String>,
    /// Symbols that allocate (Alloc).
    pub allocating: Vec<String>,
    /// `@isr` entry points.
    pub isr_roots: Vec<String>,
    /// Caller -> complete indirect target set; absent = unresolved.
    pub indirect_targets: BTreeMap<String, Vec<String>>,
}

pub const RULE_SLEEP_IN_ATOMIC: &str = "silicon.irq.sleep_in_atomic";
pub const RULE_ALLOC_IN_ATOMIC: &str = "silicon.irq.alloc_in_atomic";
pub const RULE_SUSPEND_IN_ISR: &str = "silicon.irq.suspend_in_isr";
pub const RULE_ALLOC_IN_ISR: &str = "silicon.irq.alloc_in_isr";
pub const RULE_UNRESOLVED_IN_ATOMIC: &str = "silicon.irq.unresolved_call_in_atomic";
pub const RULE_LOCK_WITHOUT_GUARD: &str = "silicon.irq.lock_without_guard";
pub const RULE_RECURSIVE_LOCK: &str = "silicon.irq.recursive_lock";
pub const RULE_UNHELD_RELEASE: &str = "silicon.irq.release_not_held";
pub const RULE_UNBALANCED_JOIN: &str = "silicon.irq.unbalanced_join";
pub const RULE_ESCAPES: &str = "silicon.irq.guard_or_lock_escapes";
pub const RULE_LOCK_ORDER: &str = "silicon.irq.lock_order_cycle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bad {
    Suspends,
    Allocates,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct State {
    guards: u32,
    held: Vec<usize>,
}

impl State {
    fn atomic(&self) -> bool {
        self.guards > 0 || !self.held.is_empty()
    }
}

struct Ctx<'a> {
    graph: &'a CallGraph,
    policy: &'a AtomicPolicy,
    ids: HashMap<&'a str, usize>,
    /// Nearest forbidden effect reachable from each function, with path.
    bad: Vec<Option<(Bad, Vec<String>)>>,
    /// Locks each function may acquire, transitively.
    locks_of: Vec<BTreeSet<usize>>,
}

/// Runs every atomic-context check on `program`.
pub fn check_atomic(program: &Program, policy: &AtomicPolicy) -> Vec<VerifierFinding> {
    let graph = program.graph();
    let funcs = program.functions();
    let ids: HashMap<&str, usize> = graph
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    let n = funcs.len();

    // Successors of each function, with the symbol of every direct callee,
    // including symbols the program does not define.
    let mut callees: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut unresolved = vec![false; n];
    for (i, f) in funcs.iter().enumerate() {
        for site in crate::stack::call_sites(f) {
            match site {
                CallSite::Direct { symbol, .. } => callees[i].push(symbol),
                CallSite::Indirect { .. } => {
                    match policy.indirect_targets.get(&graph.functions[i].name) {
                        Some(ts) => callees[i].extend(ts.iter().cloned()),
                        None => unresolved[i] = true,
                    }
                }
            }
        }
    }
    for ext in &graph.external_calls {
        if ext.inst.is_none() {
            callees[ext.caller.0 as usize].push(ext.symbol.clone());
        }
    }

    let sym_bad = |s: &str| -> Option<Bad> {
        if policy.suspending.iter().any(|x| x == s) {
            Some(Bad::Suspends)
        } else if policy.allocating.iter().any(|x| x == s) {
            Some(Bad::Allocates)
        } else {
            None
        }
    };

    // Nearest forbidden sink per function (BFS, so the path is shortest).
    let mut bad = vec![None; n];
    for (start, slot) in bad.iter_mut().enumerate() {
        let mut prev: Vec<Option<usize>> = vec![None; n];
        let mut seen = vec![false; n];
        seen[start] = true;
        let mut q = VecDeque::from([start]);
        let mut found = None;
        'bfs: while let Some(v) = q.pop_front() {
            if unresolved[v] {
                found = Some((Bad::Unresolved, v, None));
                break;
            }
            for s in &callees[v] {
                if let Some(b) = sym_bad(s) {
                    found = Some((b, v, Some(s.clone())));
                    break 'bfs;
                }
                if let Some(&w) = ids.get(s.as_str())
                    && !seen[w]
                {
                    seen[w] = true;
                    prev[w] = Some(v);
                    q.push_back(w);
                }
            }
        }
        if let Some((kind, at, sink)) = found {
            let mut path = vec![graph.functions[at].name.clone()];
            let mut cur = at;
            while let Some(p) = prev[cur] {
                path.push(graph.functions[p].name.clone());
                cur = p;
            }
            path.reverse();
            if let Some(s) = sink {
                path.push(s);
            } else {
                path.push("<indirect call>".to_string());
            }
            *slot = Some((kind, path));
        }
    }

    // Transitive lock sets (fixpoint).
    let lock_index = |s: &str| policy.locks.iter().position(|l| l.acquire == s);
    let mut locks_of: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for i in 0..n {
        for s in &callees[i] {
            if let Some(l) = lock_index(s) {
                locks_of[i].insert(l);
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n {
            let mut add = BTreeSet::new();
            for s in &callees[i] {
                if let Some(&w) = ids.get(s.as_str()) {
                    add.extend(locks_of[w].iter().copied());
                }
            }
            let before = locks_of[i].len();
            locks_of[i].extend(add);
            changed |= locks_of[i].len() != before;
        }
    }

    let ctx = Ctx {
        graph,
        policy,
        ids,
        bad,
        locks_of,
    };
    let mut findings = Vec::new();

    // ISR reachability.
    let mut in_isr = vec![false; n];
    for root in &policy.isr_roots {
        let Some(&r) = ctx.ids.get(root.as_str()) else {
            findings.push(VerifierFinding {
                function: root.clone(),
                inst: None,
                rule: RULE_SUSPEND_IN_ISR,
                message: "@isr root is not a function of this program".to_string(),
            });
            continue;
        };
        let mut q = VecDeque::from([r]);
        in_isr[r] = true;
        while let Some(v) = q.pop_front() {
            for s in &callees[v] {
                if let Some(&w) = ctx.ids.get(s.as_str())
                    && !in_isr[w]
                {
                    in_isr[w] = true;
                    q.push_back(w);
                }
            }
        }
        if let Some((kind, path)) = &ctx.bad[r] {
            let rule = match kind {
                Bad::Suspends => RULE_SUSPEND_IN_ISR,
                Bad::Allocates => RULE_ALLOC_IN_ISR,
                Bad::Unresolved => RULE_UNRESOLVED_IN_ATOMIC,
            };
            findings.push(VerifierFinding {
                function: root.clone(),
                inst: None,
                rule,
                message: format!(
                    "@isr handler reaches a forbidden effect: {}",
                    path.join(" -> ")
                ),
            });
        }
    }

    // Per-function guard/lock dataflow.
    let mut order: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (i, f) in funcs.iter().enumerate() {
        check_function(&ctx, i, f, in_isr[i], &mut order, &mut findings);
    }

    // Lock-order cycles.
    let nl = policy.locks.len();
    let mut adj = vec![Vec::new(); nl];
    for &(a, b) in &order {
        adj[a].push(b);
    }
    for scc in tarjan(nl, &adj) {
        if scc.len() > 1 || adj[scc[0]].contains(&scc[0]) {
            let names: Vec<&str> = scc.iter().map(|&l| policy.locks[l].name.as_str()).collect();
            findings.push(VerifierFinding {
                function: String::new(),
                inst: None,
                rule: RULE_LOCK_ORDER,
                message: format!(
                    "locks acquired in inconsistent order: {{{}}}",
                    names.join(", ")
                ),
            });
        }
    }
    findings
}

fn check_function(
    ctx: &Ctx<'_>,
    index: usize,
    func: &Function,
    in_isr: bool,
    order: &mut BTreeSet<(usize, usize)>,
    findings: &mut Vec<VerifierFinding>,
) {
    let name = ctx.graph.functions[index].name.clone();
    let p = ctx.policy;
    let cfg = ControlFlowGraph::with_function(func);
    let Some(entry) = func.layout.entry_block() else {
        return;
    };
    let mut entry_state: HashMap<Block, State> = HashMap::new();
    entry_state.insert(entry, State::default());
    let mut work = VecDeque::from([entry]);
    let mut reported_join: BTreeSet<Block> = BTreeSet::new();
    let mut finding = |inst: Option<u32>, rule: &'static str, message: String| {
        findings.push(VerifierFinding {
            function: name.clone(),
            inst,
            rule,
            message,
        });
    };
    let mut visited: BTreeSet<Block> = BTreeSet::new();
    while let Some(block) = work.pop_front() {
        if !visited.insert(block) {
            continue;
        }
        let mut st = entry_state[&block].clone();
        for inst in func.layout.block_insts(block) {
            let idx = Some(inst.as_u32());
            if func.dfg.insts[inst].opcode().is_return() && st.atomic() {
                finding(
                    idx,
                    RULE_ESCAPES,
                    format!(
                        "returns with {} interrupt guard(s) and {} lock(s) still held",
                        st.guards,
                        st.held.len()
                    ),
                );
            }
            let Some(site) = call_site(func, inst) else {
                continue;
            };
            let symbol = match site {
                CallSite::Indirect { .. } => {
                    if st.atomic() && !p.indirect_targets.contains_key(&name) {
                        finding(
                            idx,
                            RULE_UNRESOLVED_IN_ATOMIC,
                            "indirect call with unknown targets in atomic context".to_string(),
                        );
                    }
                    if let Some(ts) = p.indirect_targets.get(&name) {
                        for t in ts {
                            callee_in_atomic(ctx, &st, t, idx, order, &mut finding);
                        }
                    }
                    continue;
                }
                CallSite::Direct { symbol, .. } => symbol,
            };
            if p.guard_acquire.contains(&symbol) {
                st.guards += 1;
            } else if p.guard_release.contains(&symbol) {
                if st.guards == 0 {
                    finding(
                        idx,
                        RULE_UNHELD_RELEASE,
                        format!("`{symbol}` without a held interrupt guard"),
                    );
                } else {
                    st.guards -= 1;
                }
            } else if let Some(l) = p.locks.iter().position(|l| l.acquire == symbol) {
                let spec = &p.locks[l];
                if spec.isr_shared && st.guards == 0 && !in_isr {
                    finding(
                        idx,
                        RULE_LOCK_WITHOUT_GUARD,
                        format!("ISR-shared lock `{}` taken without an IrqGuard", spec.name),
                    );
                }
                if st.held.contains(&l) {
                    finding(
                        idx,
                        RULE_RECURSIVE_LOCK,
                        format!("lock `{}` already held", spec.name),
                    );
                }
                for &h in &st.held {
                    order.insert((h, l));
                }
                st.held.push(l);
            } else if let Some(l) = p.locks.iter().position(|l| l.release == symbol) {
                match st.held.iter().rposition(|&h| h == l) {
                    Some(pos) => {
                        st.held.remove(pos);
                    }
                    None => finding(
                        idx,
                        RULE_UNHELD_RELEASE,
                        format!("lock `{}` released but not held", p.locks[l].name),
                    ),
                }
            } else if st.atomic() || in_isr {
                callee_in_atomic(ctx, &st, &symbol, idx, order, &mut finding);
            }
        }
        for succ in cfg.succ_iter(block) {
            match entry_state.get(&succ) {
                None => {
                    entry_state.insert(succ, st.clone());
                    work.push_back(succ);
                }
                Some(prev) if *prev != st => {
                    if reported_join.insert(succ) {
                        finding(
                            None,
                            RULE_UNBALANCED_JOIN,
                            format!("predecessors of {succ} disagree on held guards/locks"),
                        );
                    }
                }
                Some(_) => {}
            }
        }
    }
}

fn callee_in_atomic(
    ctx: &Ctx<'_>,
    st: &State,
    symbol: &str,
    idx: Option<u32>,
    order: &mut BTreeSet<(usize, usize)>,
    finding: &mut impl FnMut(Option<u32>, &'static str, String),
) {
    let p = ctx.policy;
    let direct = if p.suspending.iter().any(|s| s == symbol) {
        Some((Bad::Suspends, vec![symbol.to_string()]))
    } else if p.allocating.iter().any(|s| s == symbol) {
        Some((Bad::Allocates, vec![symbol.to_string()]))
    } else {
        ctx.ids.get(symbol).and_then(|&w| ctx.bad[w].clone())
    };
    if let Some(&w) = ctx.ids.get(symbol) {
        for &h in &st.held {
            for &l in &ctx.locks_of[w] {
                order.insert((h, l));
            }
        }
    }
    if let Some((kind, path)) = direct {
        let rule = match kind {
            Bad::Suspends => RULE_SLEEP_IN_ATOMIC,
            Bad::Allocates => RULE_ALLOC_IN_ATOMIC,
            Bad::Unresolved => RULE_UNRESOLVED_IN_ATOMIC,
        };
        finding(
            idx,
            rule,
            format!(
                "call in atomic context reaches a forbidden effect: {}",
                path.join(" -> ")
            ),
        );
    }
}
