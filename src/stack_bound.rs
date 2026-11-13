//! One worst-case stack-bound report over the wave-five `CallGraph` shape,
//! shared by the Silicon (§26.5, Cranelift) and eBPF (§27.1, BPF-to-BPF)
//! analyzers.
//!
//! Both crates define the contract's `CallGraph { functions, calls }` with
//! `FnId(u32)` and per-function frame bytes, but as distinct types, and
//! neither may depend on the other. This adapter (integration B, issue #136)
//! converts either graph into [`StackGraph`] and analyses it with one
//! algorithm: Silicon's [`vesper_silicon::stack::analyze_stack`], which
//! rejects recursion, unresolved indirect calls and unbounded external calls
//! and reports each root's worst path. For graphs that came from the eBPF
//! verifier the kernel's own rules are checked as well (512 bytes combined,
//! at most 8 frames) with [`vesper_ebpf::callgraph::check_stack`], and the two
//! worst-case numbers are compared: a disagreement is a finding, never
//! silently resolved in either direction.
use std::collections::BTreeMap;

/// Which analyzer produced a graph (and which extra rules apply).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphOrigin {
    /// Frames measured from Cranelift prologues by `vesper-silicon`.
    Cranelift,
    /// Frames from the eBPF verifier (already rounded to 32 bytes).
    Ebpf,
}

/// A function in the unified graph. Ids are dense: `functions[i].id == i`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFn {
    pub id: u32,
    pub name: String,
    pub frame_bytes: u32,
}

/// A call whose callee is not a function of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackExternal {
    pub caller: u32,
    pub symbol: String,
    pub inst: Option<u32>,
}

/// The contract's call graph, independent of the producing crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackGraph {
    pub origin: GraphOrigin,
    pub functions: Vec<StackFn>,
    pub calls: Vec<(u32, u32)>,
    /// Indirect call sites `(caller, instruction index if known)`.
    pub indirect: Vec<(u32, Option<u32>)>,
    pub external: Vec<StackExternal>,
}

/// A root and its budget (`@stack_limit(bytes)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackBudget {
    pub function: String,
    pub budget_bytes: u32,
}

/// Root-independent policy inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackBoundPolicy {
    pub roots: Vec<StackBudget>,
    /// External symbol -> declared worst-case bytes including its callees.
    pub external_bounds: BTreeMap<String, u32>,
    /// Caller -> complete target set of its indirect calls.
    pub indirect_targets: BTreeMap<String, Vec<String>>,
}

/// One analysed root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackRootBound {
    pub root: String,
    pub budget_bytes: u32,
    /// `None` when the depth is unbounded.
    pub worst_case_bytes: Option<u64>,
    pub worst_path: Vec<String>,
    pub within_budget: bool,
}

/// A rule violation, in the contract's finding shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFinding {
    pub function: String,
    pub inst: Option<u32>,
    pub rule: String,
    pub message: String,
}

/// The single stack-bound report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackBoundReport {
    pub origin: GraphOrigin,
    pub roots: Vec<StackRootBound>,
    pub findings: Vec<StackFinding>,
}

impl StackBoundReport {
    pub fn accepted(&self) -> bool {
        self.findings.is_empty() && self.roots.iter().all(|r| r.within_budget)
    }
}

/// Rule for a disagreement between the two analyzers on an eBPF graph.
pub const RULE_DISAGREEMENT: &str = "stack.analyzers_disagree";
/// Rule for the kernel's eBPF limits (combined bytes, frames, recursion).
pub const RULE_EBPF_KERNEL: &str = "ebpf.stack.kernel_limit";

impl From<&vesper_silicon::stack::CallGraph> for StackGraph {
    fn from(g: &vesper_silicon::stack::CallGraph) -> Self {
        Self {
            origin: GraphOrigin::Cranelift,
            functions: g
                .functions
                .iter()
                .map(|f| StackFn {
                    id: f.id.0,
                    name: f.name.clone(),
                    frame_bytes: f.frame_bytes,
                })
                .collect(),
            calls: g.calls.iter().map(|(a, b)| (a.0, b.0)).collect(),
            indirect: g
                .indirect_calls
                .iter()
                .map(|c| (c.caller.0, Some(c.inst)))
                .collect(),
            external: g
                .external_calls
                .iter()
                .map(|c| StackExternal {
                    caller: c.caller.0,
                    symbol: c.symbol.clone(),
                    inst: c.inst,
                })
                .collect(),
        }
    }
}

impl From<&vesper_ebpf::CallGraph> for StackGraph {
    fn from(g: &vesper_ebpf::CallGraph) -> Self {
        Self {
            origin: GraphOrigin::Ebpf,
            functions: g
                .functions
                .iter()
                .map(|f| StackFn {
                    id: f.id.0,
                    name: f.name.clone(),
                    frame_bytes: f.frame_bytes,
                })
                .collect(),
            calls: g.calls.iter().map(|(a, b)| (a.0, b.0)).collect(),
            indirect: g.indirect.iter().map(|c| (c.0, None)).collect(),
            external: Vec::new(),
        }
    }
}

impl StackGraph {
    fn validate(&self) -> Result<(), String> {
        for (i, f) in self.functions.iter().enumerate() {
            if f.id as usize != i {
                return Err(format!(
                    "stack graph ids must be dense: function `{}` has id {} at position {i}",
                    f.name, f.id
                ));
            }
        }
        let n = self.functions.len() as u32;
        let bad = self
            .calls
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .chain(self.indirect.iter().map(|(c, _)| *c))
            .chain(self.external.iter().map(|e| e.caller))
            .find(|id| *id >= n);
        match bad {
            Some(id) => Err(format!("stack graph names unknown function id {id}")),
            None => Ok(()),
        }
    }

    fn to_silicon(&self) -> vesper_silicon::stack::CallGraph {
        use vesper_silicon::stack::{CallGraph, ExternalCall, FnId, FnNode, IndirectCall};
        CallGraph {
            functions: self
                .functions
                .iter()
                .map(|f| FnNode {
                    id: FnId(f.id),
                    name: f.name.clone(),
                    frame_bytes: f.frame_bytes,
                })
                .collect(),
            calls: self
                .calls
                .iter()
                .map(|(a, b)| (FnId(*a), FnId(*b)))
                .collect(),
            indirect_calls: self
                .indirect
                .iter()
                .map(|(c, inst)| IndirectCall {
                    caller: FnId(*c),
                    inst: inst.unwrap_or(0),
                })
                .collect(),
            external_calls: self
                .external
                .iter()
                .map(|e| ExternalCall {
                    caller: FnId(e.caller),
                    symbol: e.symbol.clone(),
                    inst: e.inst,
                })
                .collect(),
        }
    }

    fn to_ebpf(&self) -> vesper_ebpf::CallGraph {
        use vesper_ebpf::{CallGraph, FnId, FnNode};
        CallGraph {
            functions: self
                .functions
                .iter()
                .map(|f| FnNode {
                    id: FnId(f.id),
                    name: f.name.clone(),
                    frame_bytes: f.frame_bytes,
                })
                .collect(),
            calls: self
                .calls
                .iter()
                .map(|(a, b)| (FnId(*a), FnId(*b)))
                .collect(),
            indirect: self.indirect.iter().map(|(c, _)| FnId(*c)).collect(),
        }
    }

    fn name(&self, id: u32) -> &str {
        self.functions
            .get(id as usize)
            .map_or("<unknown>", |f| f.name.as_str())
    }
}

/// Analyse `graph` against `policy` with the shared algorithm, plus the
/// kernel's rules for eBPF graphs. A malformed graph is an error.
pub fn stack_bound_report(
    graph: &StackGraph,
    policy: &StackBoundPolicy,
) -> Result<StackBoundReport, String> {
    graph.validate()?;
    let silicon_policy = vesper_silicon::stack::StackPolicy {
        roots: policy
            .roots
            .iter()
            .map(|r| vesper_silicon::stack::StackRoot {
                function: r.function.clone(),
                budget_bytes: r.budget_bytes,
            })
            .collect(),
        indirect_targets: policy.indirect_targets.clone(),
        external_bounds: policy.external_bounds.clone(),
    };
    let report = vesper_silicon::stack::analyze_stack(&graph.to_silicon(), &silicon_policy);
    let mut out = StackBoundReport {
        origin: graph.origin,
        roots: report
            .roots
            .iter()
            .map(|r| StackRootBound {
                root: r.root.clone(),
                budget_bytes: r.budget_bytes,
                worst_case_bytes: r.worst_case_bytes,
                worst_path: r.worst_path.clone(),
                within_budget: r.within_budget,
            })
            .collect(),
        findings: report
            .findings
            .iter()
            .map(|f| StackFinding {
                function: f.function.clone(),
                inst: f.inst,
                rule: f.rule.to_string(),
                message: f.message.clone(),
            })
            .collect(),
    };
    if graph.origin == GraphOrigin::Ebpf {
        let ebpf = graph.to_ebpf();
        for root in &out.roots {
            let Some(id) = graph.functions.iter().find(|f| f.name == root.root) else {
                continue;
            };
            match vesper_ebpf::callgraph::check_stack(
                &ebpf,
                vesper_ebpf::FnId(id.id),
                vesper_ebpf::BPF_MAX_STACK_BYTES,
                vesper_ebpf::verifier::MAX_CALL_FRAMES,
            ) {
                Ok(bytes) => {
                    if root.worst_case_bytes != Some(u64::from(bytes)) {
                        out.findings.push(StackFinding {
                            function: root.root.clone(),
                            inst: None,
                            rule: RULE_DISAGREEMENT.into(),
                            message: format!(
                                "the shared analysis gives {:?} bytes but the eBPF analysis gives {bytes}",
                                root.worst_case_bytes
                            ),
                        });
                    }
                }
                Err(error) => {
                    let message = match &error {
                        vesper_ebpf::callgraph::StackError::Exceeded { bytes, path } => format!(
                            "{bytes} bytes exceed the kernel's {} byte limit along {}",
                            vesper_ebpf::BPF_MAX_STACK_BYTES,
                            path.iter()
                                .map(|id| graph.name(id.0))
                                .collect::<Vec<_>>()
                                .join(" -> ")
                        ),
                        vesper_ebpf::callgraph::StackError::TooDeep { path } => format!(
                            "more than {} call frames along {}",
                            vesper_ebpf::verifier::MAX_CALL_FRAMES,
                            path.iter()
                                .map(|id| graph.name(id.0))
                                .collect::<Vec<_>>()
                                .join(" -> ")
                        ),
                        other => other.to_string(),
                    };
                    out.findings.push(StackFinding {
                        function: root.root.clone(),
                        inst: None,
                        rule: RULE_EBPF_KERNEL.into(),
                        message,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Text rendering shared by `vesper ebpf check` and `--verify-stack`.
pub fn render(report: &StackBoundReport) -> String {
    let mut out = String::new();
    for root in &report.roots {
        let worst = root
            .worst_case_bytes
            .map_or_else(|| "unbounded".to_string(), |b| format!("{b} bytes"));
        out.push_str(&format!(
            "stack {}: worst case {worst} of {} byte budget ({}) via {}\n",
            root.root,
            root.budget_bytes,
            if root.within_budget { "ok" } else { "exceeded" },
            if root.worst_path.is_empty() {
                "-".to_string()
            } else {
                root.worst_path.join(" -> ")
            }
        ));
    }
    out
}
