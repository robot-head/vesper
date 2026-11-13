//! §26.5 worst-case stack-depth analysis over real Cranelift functions.
//!
//! [`Program::compile`] compiles every function with `cranelift-codegen`
//! 0.125 for x86_64, AArch64 or riscv64, measures each activation's stack bytes from
//! the emitted prologue (see [`crate::prologue`]), cross-checks that number
//! against Cranelift's own frame layout, and builds the wave-five
//! [`CallGraph`] from the IR's `call`, `call_indirect`, `return_call` and
//! `return_call_indirect` instructions plus libcall relocations in the
//! machine code. [`Program::check_stack`] then rejects recursion (any
//! strongly connected component reachable from a root), indirect calls whose
//! targets the policy does not list, and external calls without a declared
//! frame bound, and checks each root's `@stack_limit(bytes)` budget, reporting
//! the worst path.
//!
//! Tail calls (`return_call*`) are counted as ordinary calls, which
//! over-approximates the depth: the analysis is conservative, not exact, for
//! them. Stack probes are disabled when compiling (MMU-less targets have no
//! guard page to probe), and dynamic `alloca`-style growth does not exist in
//! Cranelift IR, so the prologue is the whole SP decrement of an activation.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cranelift_codegen::Context;
use cranelift_codegen::FinalizedRelocTarget;
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::{ExternalName, Function, Inst, InstructionData, UserFuncName};
use cranelift_codegen::isa::{self, TargetIsa};
use cranelift_codegen::settings::{self, Configurable};

use crate::prologue::{self, MeasuredPrologue};
use crate::{Arch, VerifierFinding};

/// Index of a function in a [`CallGraph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

/// One defined function and its measured activation size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnNode {
    pub id: FnId,
    pub name: String,
    /// Stack bytes one activation occupies, read from the machine code.
    pub frame_bytes: u32,
}

/// An indirect call site (`call_indirect` / `return_call_indirect`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndirectCall {
    pub caller: FnId,
    pub inst: u32,
}

/// A call to a symbol the program does not define (including libcalls that
/// Cranelift inserted during lowering, named `libcall:<Name>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalCall {
    pub caller: FnId,
    pub symbol: String,
    /// IR instruction, when the call is visible in the IR (libcalls are not).
    pub inst: Option<u32>,
}

/// The wave-five call-graph contract: defined functions with per-function
/// frame bytes, resolved direct edges, and the two unresolved edge kinds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallGraph {
    pub functions: Vec<FnNode>,
    /// Direct `(caller, callee)` edges, deduplicated, in first-seen order.
    pub calls: Vec<(FnId, FnId)>,
    pub indirect_calls: Vec<IndirectCall>,
    pub external_calls: Vec<ExternalCall>,
}

impl CallGraph {
    pub fn id_of(&self, name: &str) -> Option<FnId> {
        self.functions.iter().find(|f| f.name == name).map(|f| f.id)
    }

    pub fn name(&self, id: FnId) -> &str {
        &self.functions[id.0 as usize].name
    }
}

/// How the frame of one function was measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameMeasurement {
    pub name: String,
    pub prologue: MeasuredPrologue,
    /// `CompiledCode::frame_size` (clobbers + fixed slots, no setup area and
    /// no outgoing-argument area).
    pub cranelift_frame_size: u32,
    pub code_bytes: usize,
}

/// Compilation or measurement failure. These are typed rejections; nothing
/// here is reported as a successful analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackError {
    UnsupportedIsa(Arch),
    IsaSetup(String),
    UnnamedFunction,
    DuplicateFunction(String),
    Compile {
        function: String,
        message: String,
    },
    /// The decoded prologue is smaller than Cranelift's own frame layout, so
    /// the decoder missed an SP adjustment; the frame is not trusted.
    FrameMeasurementMismatch {
        function: String,
        measured: u32,
        layout_minimum: u32,
    },
    Overflow {
        function: String,
    },
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for StackError {}

fn testcase(display: &str) -> String {
    display.strip_prefix('%').unwrap_or(display).to_string()
}

/// Canonical symbol for a function name or call target.
pub fn symbol_of_user_func(name: &UserFuncName) -> String {
    match name {
        UserFuncName::User(u) => format!("u{}:{}", u.namespace, u.index),
        UserFuncName::Testcase(t) => testcase(&t.to_string()),
    }
}

fn symbol_of_external(func: &Function, name: &ExternalName) -> String {
    match name {
        ExternalName::User(r) => {
            let u = &func.params.user_named_funcs()[*r];
            format!("u{}:{}", u.namespace, u.index)
        }
        ExternalName::TestCase(t) => testcase(&t.to_string()),
        ExternalName::LibCall(lc) => format!("libcall:{lc}"),
        ExternalName::KnownSymbol(k) => format!("known:{k}"),
    }
}

/// A call-like instruction found in a function body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallSite {
    Direct {
        inst: u32,
        symbol: String,
        tail: bool,
    },
    Indirect {
        inst: u32,
        tail: bool,
    },
}

/// Every call-like instruction of `func`, in layout order.
pub fn call_sites(func: &Function) -> Vec<CallSite> {
    let mut out = Vec::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            if let Some(site) = call_site(func, inst) {
                out.push(site);
            }
        }
    }
    out
}

pub(crate) fn call_site(func: &Function, inst: Inst) -> Option<CallSite> {
    use cranelift_codegen::ir::Opcode;
    let index = inst.as_u32();
    match &func.dfg.insts[inst] {
        InstructionData::Call {
            opcode, func_ref, ..
        } => Some(CallSite::Direct {
            inst: index,
            symbol: symbol_of_external(func, &func.dfg.ext_funcs[*func_ref].name),
            tail: *opcode == Opcode::ReturnCall,
        }),
        InstructionData::CallIndirect { opcode, .. } => Some(CallSite::Indirect {
            inst: index,
            tail: *opcode == Opcode::ReturnCallIndirect,
        }),
        InstructionData::TryCall { func_ref, .. } => Some(CallSite::Direct {
            inst: index,
            symbol: symbol_of_external(func, &func.dfg.ext_funcs[*func_ref].name),
            tail: false,
        }),
        InstructionData::TryCallIndirect { .. } => Some(CallSite::Indirect {
            inst: index,
            tail: false,
        }),
        _ => None,
    }
}

/// A set of Cranelift functions compiled for one ISA, with measured frames.
pub struct Program {
    arch: Arch,
    functions: Vec<Function>,
    measurements: Vec<FrameMeasurement>,
    graph: CallGraph,
}

/// Builds the Cranelift ISA used for analysis.
pub fn target_isa(arch: Arch) -> Result<std::sync::Arc<dyn TargetIsa>, StackError> {
    target_isa_with(arch, &[])
}

/// Builds the analysis ISA with extra ISA-specific settings, for example
/// `[("has_zca", "true"), ("has_zcd", "true")]` for riscv64 code that uses the
/// compressed encodings. An unknown setting is [`StackError::IsaSetup`].
pub fn target_isa_with(
    arch: Arch,
    isa_flags: &[(&str, &str)],
) -> Result<std::sync::Arc<dyn TargetIsa>, StackError> {
    let name = match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::Riscv64 => "riscv64",
    };
    let mut flags = settings::builder();
    for (k, v) in [
        ("opt_level", "speed"),
        ("enable_probestack", "false"),
        ("unwind_info", "true"),
        ("preserve_frame_pointers", "true"),
    ] {
        flags
            .set(k, v)
            .map_err(|e| StackError::IsaSetup(format!("{k}={v}: {e}")))?;
    }
    let mut builder = isa::lookup_by_name(name).map_err(|e| StackError::IsaSetup(e.to_string()))?;
    for (k, v) in isa_flags {
        builder
            .set(k, v)
            .map_err(|e| StackError::IsaSetup(format!("{k}={v}: {e}")))?;
    }
    builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| StackError::IsaSetup(e.to_string()))
}

impl Program {
    /// Compiles and measures every function. Function names (`User` or
    /// `Testcase`) must be unique; callees are resolved by the same names.
    pub fn compile(arch: Arch, functions: Vec<Function>) -> Result<Self, StackError> {
        Self::compile_with_isa_flags(arch, &[], functions)
    }

    /// [`Program::compile`] with extra ISA-specific settings (see
    /// [`target_isa_with`]).
    pub fn compile_with_isa_flags(
        arch: Arch,
        isa_flags: &[(&str, &str)],
        functions: Vec<Function>,
    ) -> Result<Self, StackError> {
        let isa = target_isa_with(arch, isa_flags)?;
        let mut ids: HashMap<String, FnId> = HashMap::new();
        for (i, f) in functions.iter().enumerate() {
            let name = symbol_of_user_func(&f.name);
            if name.is_empty() {
                return Err(StackError::UnnamedFunction);
            }
            let id = FnId(u32::try_from(i).map_err(|_| StackError::Overflow {
                function: name.clone(),
            })?);
            if ids.insert(name.clone(), id).is_some() {
                return Err(StackError::DuplicateFunction(name));
            }
        }
        let mut graph = CallGraph::default();
        let mut measurements = Vec::with_capacity(functions.len());
        let mut seen_edges = BTreeSet::new();
        for (i, func) in functions.iter().enumerate() {
            let id = FnId(i as u32);
            let name = symbol_of_user_func(&func.name);
            let mut ctx = Context::for_function(func.clone());
            let compiled = ctx
                .compile(&*isa, &mut ControlPlane::default())
                .map_err(|e| StackError::Compile {
                    function: name.clone(),
                    message: format!("{:?}", e.inner),
                })?;
            let code = compiled.code_buffer();
            let measured = match arch {
                Arch::X86_64 => prologue::measure_x86_64(code),
                Arch::Aarch64 => prologue::measure_aarch64(code),
                Arch::Riscv64 => prologue::measure_riscv64(code),
            };
            let total = measured.total().ok_or_else(|| StackError::Overflow {
                function: name.clone(),
            })?;
            // The layout minimum: Cranelift's frame_size plus the frame record
            // when one was pushed, plus the x86 return address.
            let layout_minimum = compiled
                .frame_size
                .checked_add(measured.frame_record_bytes)
                .and_then(|v| v.checked_add(measured.call_push_bytes))
                .ok_or_else(|| StackError::Overflow {
                    function: name.clone(),
                })?;
            if total < layout_minimum {
                return Err(StackError::FrameMeasurementMismatch {
                    function: name,
                    measured: total,
                    layout_minimum,
                });
            }
            // Libcalls inserted by lowering are only visible as relocations.
            let mut libcalls = BTreeSet::new();
            for reloc in compiled.buffer.relocs() {
                if let FinalizedRelocTarget::ExternalName(ExternalName::LibCall(lc)) = &reloc.target
                {
                    libcalls.insert(format!("libcall:{lc}"));
                }
            }
            measurements.push(FrameMeasurement {
                name: name.clone(),
                prologue: measured,
                cranelift_frame_size: compiled.frame_size,
                code_bytes: code.len(),
            });
            graph.functions.push(FnNode {
                id,
                name,
                frame_bytes: total,
            });
            for site in call_sites(func) {
                match site {
                    CallSite::Direct { inst, symbol, .. } => match ids.get(&symbol) {
                        Some(&callee) => {
                            if seen_edges.insert((id, callee)) {
                                graph.calls.push((id, callee));
                            }
                        }
                        None => graph.external_calls.push(ExternalCall {
                            caller: id,
                            symbol,
                            inst: Some(inst),
                        }),
                    },
                    CallSite::Indirect { inst, .. } => {
                        graph.indirect_calls.push(IndirectCall { caller: id, inst })
                    }
                }
            }
            for symbol in libcalls {
                graph.external_calls.push(ExternalCall {
                    caller: id,
                    symbol,
                    inst: None,
                });
            }
        }
        Ok(Self {
            arch,
            functions,
            measurements,
            graph,
        })
    }

    pub fn arch(&self) -> Arch {
        self.arch
    }

    pub fn graph(&self) -> &CallGraph {
        &self.graph
    }

    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn measurements(&self) -> &[FrameMeasurement] {
        &self.measurements
    }

    /// Runs the stack-budget analysis for every root in `policy`.
    pub fn check_stack(&self, policy: &StackPolicy) -> StackReport {
        analyze_stack(&self.graph, policy)
    }
}

/// One `@stack_limit(bytes)` root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackRoot {
    pub function: String,
    pub budget_bytes: u32,
}

/// Annotations the source language cannot express yet, passed explicitly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackPolicy {
    pub roots: Vec<StackRoot>,
    /// Caller symbol -> the complete set of targets its indirect calls may
    /// reach. A caller absent from this map has unresolved indirect calls.
    pub indirect_targets: BTreeMap<String, Vec<String>>,
    /// External symbol (including `libcall:<Name>`) -> declared worst-case
    /// stack bytes of that callee, including everything it calls.
    pub external_bounds: BTreeMap<String, u32>,
}

/// Result for one root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootResult {
    pub root: String,
    pub budget_bytes: u32,
    /// `None` when the depth is unbounded (recursion, unresolved calls).
    pub worst_case_bytes: Option<u64>,
    /// Functions on a deepest path, root first; a bounded external callee
    /// appears by its symbol.
    pub worst_path: Vec<String>,
    pub within_budget: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackReport {
    pub roots: Vec<RootResult>,
    pub findings: Vec<VerifierFinding>,
}

pub const RULE_RECURSION: &str = "silicon.stack.recursion";
pub const RULE_INDIRECT: &str = "silicon.stack.unresolved_indirect_call";
pub const RULE_EXTERNAL: &str = "silicon.stack.unbounded_external_call";
pub const RULE_BUDGET: &str = "silicon.stack.budget_exceeded";
pub const RULE_UNKNOWN_ROOT: &str = "silicon.stack.unknown_root";
pub const RULE_UNKNOWN_TARGET: &str = "silicon.stack.unknown_indirect_target";

/// Successor of a function in the analysis graph.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Succ {
    Fn(FnId),
    Bounded(String, u32),
}

/// Tarjan's algorithm, iterative. Returns SCCs in reverse topological order
/// (every SCC appears after all SCCs it calls into).
pub(crate) fn tarjan(n: usize, adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut sccs = Vec::new();
    let mut next = 0usize;
    for start in 0..n {
        if index[start] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&mut (v, ref mut child)) = work.last_mut() {
            if *child == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(*child) {
                *child += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                scc.sort_unstable();
                sccs.push(scc);
            }
        }
    }
    sccs
}

/// Analyses a call graph against a policy. Works on any [`CallGraph`], so
/// integration can feed graphs built elsewhere.
pub fn analyze_stack(graph: &CallGraph, policy: &StackPolicy) -> StackReport {
    let n = graph.functions.len();
    let mut report = StackReport::default();
    let by_name: HashMap<&str, FnId> = graph
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f.id))
        .collect();

    let mut succ: Vec<BTreeSet<Succ>> = vec![BTreeSet::new(); n];
    // `true` when the function itself has an unresolved edge.
    let mut unbounded_here = vec![false; n];
    for &(a, b) in &graph.calls {
        succ[a.0 as usize].insert(Succ::Fn(b));
    }
    for ext in &graph.external_calls {
        let c = ext.caller.0 as usize;
        match policy.external_bounds.get(&ext.symbol) {
            Some(&bytes) => {
                succ[c].insert(Succ::Bounded(ext.symbol.clone(), bytes));
            }
            None => {
                unbounded_here[c] = true;
                report.findings.push(VerifierFinding {
                    function: graph.name(ext.caller).to_string(),
                    inst: ext.inst,
                    rule: RULE_EXTERNAL,
                    message: format!(
                        "call to `{}` has no declared stack bound; list it in StackPolicy::external_bounds",
                        ext.symbol
                    ),
                });
            }
        }
    }
    for site in &graph.indirect_calls {
        let c = site.caller.0 as usize;
        let caller = graph.name(site.caller);
        match policy.indirect_targets.get(caller) {
            None => {
                unbounded_here[c] = true;
                report.findings.push(VerifierFinding {
                    function: caller.to_string(),
                    inst: Some(site.inst),
                    rule: RULE_INDIRECT,
                    message: "indirect call without a policy-listed target set".to_string(),
                });
            }
            Some(targets) => {
                for t in targets {
                    if let Some(&id) = by_name.get(t.as_str()) {
                        succ[c].insert(Succ::Fn(id));
                    } else if let Some(&bytes) = policy.external_bounds.get(t) {
                        succ[c].insert(Succ::Bounded(t.clone(), bytes));
                    } else {
                        unbounded_here[c] = true;
                        report.findings.push(VerifierFinding {
                            function: caller.to_string(),
                            inst: Some(site.inst),
                            rule: RULE_UNKNOWN_TARGET,
                            message: format!(
                                "indirect target `{t}` is neither defined nor bounded"
                            ),
                        });
                    }
                }
            }
        }
    }

    let adj: Vec<Vec<usize>> = succ
        .iter()
        .map(|s| {
            s.iter()
                .filter_map(|x| match x {
                    Succ::Fn(id) => Some(id.0 as usize),
                    Succ::Bounded(..) => None,
                })
                .collect()
        })
        .collect();
    let sccs = tarjan(n, &adj);
    let mut cyclic = vec![false; n];
    for scc in &sccs {
        let is_cycle = scc.len() > 1 || adj[scc[0]].contains(&scc[0]);
        if is_cycle {
            let names: Vec<&str> = scc
                .iter()
                .map(|&i| graph.functions[i].name.as_str())
                .collect();
            for &i in scc {
                cyclic[i] = true;
            }
            report.findings.push(VerifierFinding {
                function: names[0].to_string(),
                inst: None,
                rule: RULE_RECURSION,
                message: format!("recursive call cycle among {{{}}}", names.join(", ")),
            });
        }
    }

    // Longest path in reverse topological order. `None` = unbounded.
    let mut depth: Vec<Option<u64>> = vec![None; n];
    let mut best: Vec<Option<Succ>> = vec![None; n];
    for scc in &sccs {
        for &v in scc {
            if cyclic[v] || unbounded_here[v] {
                continue;
            }
            let mut deepest: u64 = 0;
            let mut pick = None;
            let mut ok = true;
            for s in &succ[v] {
                let d = match s {
                    Succ::Fn(id) => match depth[id.0 as usize] {
                        Some(d) => d,
                        None => {
                            ok = false;
                            break;
                        }
                    },
                    Succ::Bounded(_, b) => u64::from(*b),
                };
                if pick.is_none() || d > deepest {
                    deepest = d;
                    pick = Some(s.clone());
                }
            }
            if ok {
                // u32 frames over at most u32::MAX functions cannot overflow u64.
                depth[v] = Some(u64::from(graph.functions[v].frame_bytes) + deepest);
                best[v] = pick;
            }
        }
    }

    for root in &policy.roots {
        let Some(&id) = by_name.get(root.function.as_str()) else {
            report.findings.push(VerifierFinding {
                function: root.function.clone(),
                inst: None,
                rule: RULE_UNKNOWN_ROOT,
                message: "stack root is not a function of this program".to_string(),
            });
            report.roots.push(RootResult {
                root: root.function.clone(),
                budget_bytes: root.budget_bytes,
                worst_case_bytes: None,
                worst_path: Vec::new(),
                within_budget: false,
            });
            continue;
        };
        let worst = depth[id.0 as usize];
        let mut path = Vec::new();
        if worst.is_some() {
            let mut cur = Some(Succ::Fn(id));
            while let Some(s) = cur {
                match s {
                    Succ::Fn(f) => {
                        path.push(graph.name(f).to_string());
                        cur = best[f.0 as usize].clone();
                    }
                    Succ::Bounded(sym, _) => {
                        path.push(sym);
                        cur = None;
                    }
                }
            }
        }
        let within = worst.is_some_and(|w| w <= u64::from(root.budget_bytes));
        match worst {
            Some(w) if !within => report.findings.push(VerifierFinding {
                function: root.function.clone(),
                inst: None,
                rule: RULE_BUDGET,
                message: format!(
                    "worst-case stack {w} bytes exceeds @stack_limit({}) along {}",
                    root.budget_bytes,
                    path.join(" -> ")
                ),
            }),
            None => report.findings.push(VerifierFinding {
                function: root.function.clone(),
                inst: None,
                rule: RULE_BUDGET,
                message:
                    "worst-case stack depth is unbounded (recursion or unresolved call reachable)"
                        .to_string(),
            }),
            _ => {}
        }
        report.roots.push(RootResult {
            root: root.function.clone(),
            budget_bytes: root.budget_bytes,
            worst_case_bytes: worst,
            worst_path: path,
            within_budget: within,
        });
    }
    report
}
