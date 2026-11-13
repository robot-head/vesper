//! Build a realtime [`CallGraph`] from Cranelift IR (design §32.5 on emitted code).
//!
//! Input is a set of named `cranelift_codegen::ir::Function`s plus a
//! [`SymbolEffects`] table describing external symbols (the allocator, a
//! mutex, the realtime bus, ...). For each function the builder records:
//!
//! * a direct edge for every `call`, `return_call` and `try_call`, resolving
//!   the callee's external name to a defined function or to a table entry;
//!   a callee in neither becomes an *unknown* node, which the verifier
//!   rejects when reachable;
//! * an [`IndirectCall`] for every `call_indirect`, `return_call_indirect`
//!   and `try_call_indirect`, keyed by the instruction index;
//! * loops: natural loops from Cranelift's own loop analysis plus any other
//!   CFG cycle (irreducible control flow) found by DFS, reported as
//!   [`LoopBound::Unbounded`] unless the policy gives a bound for that
//!   function;
//! * the frame size as the sum of the function's sized stack slots. This is
//!   the IR-visible lower bound; spill slots and the ABI frame added by
//!   register allocation are not visible in IR and are not counted.
//!
//! Instruction indices are positions in layout order, counting from 0.

use crate::confine::{CallGraph, Effect, FnId, FnNode, GraphError, IndirectCall, LoopBound};
use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::instructions::CallInfo;
use cranelift_codegen::ir::{Block, ExternalName, Function};
use cranelift_codegen::loop_analysis::LoopAnalysis;
use std::collections::BTreeMap;
use std::fmt;

/// What is known about an external symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolInfo {
    /// Effects of calling it.
    pub effects: Vec<Effect>,
    /// Stack bytes the callee uses.
    pub frame_bytes: u64,
}

/// Symbol-effect table and name resolution for Cranelift input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SymbolEffects {
    /// External symbols by name.
    pub symbols: BTreeMap<String, SymbolInfo>,
    /// Names of `ExternalName::User { namespace, index }` references.
    pub user_names: BTreeMap<(u32, u32), String>,
    /// Declared direct effects of *defined* functions (their effect rows).
    pub defined_effects: BTreeMap<String, Vec<Effect>>,
    /// Attested total loop iteration bounds for defined functions.
    pub loop_bounds: BTreeMap<String, u64>,
}

impl SymbolEffects {
    /// Describe an external symbol.
    pub fn symbol(mut self, name: &str, effects: Vec<Effect>, frame_bytes: u64) -> Self {
        self.symbols.insert(
            name.to_string(),
            SymbolInfo {
                effects,
                frame_bytes,
            },
        );
        self
    }

    /// Name a user external name.
    pub fn user_name(mut self, namespace: u32, index: u32, name: &str) -> Self {
        self.user_names.insert((namespace, index), name.to_string());
        self
    }
}

/// Malformed Cranelift input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClifError {
    /// Two input functions share a name.
    DuplicateFunction(String),
    /// A `User` external name has no entry in [`SymbolEffects::user_names`].
    UnnamedUserSymbol {
        /// Function containing the call.
        function: String,
        /// Namespace of the unresolved name.
        namespace: u32,
        /// Index of the unresolved name.
        index: u32,
    },
    /// The external name of a call is not a function symbol.
    UnsupportedCallee {
        /// Function containing the call.
        function: String,
        /// Debug rendering of the name.
        callee: String,
    },
    /// Frame size overflowed u64.
    FrameOverflow(String),
    /// Graph construction failed.
    Graph(GraphError),
    /// Instruction count exceeds u32.
    TooManyInstructions(String),
}

impl fmt::Display for ClifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClifError::DuplicateFunction(n) => write!(f, "duplicate function `{n}`"),
            ClifError::UnnamedUserSymbol {
                function,
                namespace,
                index,
            } => write!(
                f,
                "`{function}` calls unnamed user symbol u{namespace}:{index}"
            ),
            ClifError::UnsupportedCallee { function, callee } => {
                write!(f, "`{function}` calls unsupported external name {callee}")
            }
            ClifError::FrameOverflow(n) => write!(f, "stack slots of `{n}` overflow u64"),
            ClifError::Graph(e) => write!(f, "{e}"),
            ClifError::TooManyInstructions(n) => {
                write!(f, "`{n}` has more than u32::MAX instructions")
            }
        }
    }
}

impl std::error::Error for ClifError {}

impl From<GraphError> for ClifError {
    fn from(e: GraphError) -> Self {
        ClifError::Graph(e)
    }
}

/// Build the call graph of `functions` (name, body).
pub fn call_graph_from_clif(
    functions: &[(String, Function)],
    table: &SymbolEffects,
) -> Result<CallGraph, ClifError> {
    let mut graph = CallGraph::default();
    let mut ids: BTreeMap<String, FnId> = BTreeMap::new();
    for (name, func) in functions {
        if ids.contains_key(name) {
            return Err(ClifError::DuplicateFunction(name.clone()));
        }
        let mut frame: u64 = 0;
        for (_, slot) in func.sized_stack_slots.iter() {
            frame = frame
                .checked_add(u64::from(slot.size))
                .ok_or_else(|| ClifError::FrameOverflow(name.clone()))?;
        }
        let loops = if has_loops(func) {
            match table.loop_bounds.get(name) {
                Some(&b) => LoopBound::Bounded(b),
                None => LoopBound::Unbounded,
            }
        } else {
            LoopBound::NoLoops
        };
        let effects = table.defined_effects.get(name).cloned().unwrap_or_default();
        let id = graph.add(FnNode::new(name.clone(), effects, frame).with_loops(loops))?;
        ids.insert(name.clone(), id);
    }

    for (name, func) in functions {
        let caller = ids[name];
        let mut index: u32 = 0;
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                let data = &func.dfg.insts[inst];
                if data.opcode().is_call() {
                    match data.analyze_call(&func.dfg.value_lists, &func.dfg.exception_tables) {
                        CallInfo::Direct(fref, _) | CallInfo::DirectWithSig(fref, _, _) => {
                            let callee_name =
                                callee_symbol(name, func, &func.dfg.ext_funcs[fref].name, table)?;
                            let callee = match ids.get(&callee_name) {
                                Some(&id) => id,
                                None => {
                                    let node = match table.symbols.get(&callee_name) {
                                        Some(info) => FnNode::new(
                                            callee_name.clone(),
                                            info.effects.clone(),
                                            info.frame_bytes,
                                        ),
                                        None => FnNode::unknown(callee_name.clone()),
                                    };
                                    let id = graph.add(node)?;
                                    ids.insert(callee_name, id);
                                    id
                                }
                            };
                            graph.call(caller, callee);
                            graph.functions[caller.0 as usize]
                                .call_sites
                                .push((callee, index));
                        }
                        CallInfo::Indirect(_, _) => graph.indirect_calls.push(IndirectCall {
                            caller,
                            site: index,
                        }),
                        CallInfo::NotACall => {}
                    }
                }
                index = index
                    .checked_add(1)
                    .ok_or_else(|| ClifError::TooManyInstructions(name.clone()))?;
            }
        }
    }
    Ok(graph)
}

fn callee_symbol(
    caller: &str,
    func: &Function,
    name: &ExternalName,
    table: &SymbolEffects,
) -> Result<String, ClifError> {
    match name {
        ExternalName::User(r) => {
            let u = &func.params.user_named_funcs()[*r];
            table
                .user_names
                .get(&(u.namespace, u.index))
                .cloned()
                .ok_or(ClifError::UnnamedUserSymbol {
                    function: caller.to_string(),
                    namespace: u.namespace,
                    index: u.index,
                })
        }
        ExternalName::TestCase(t) => Ok(t.to_string()),
        // Runtime library calls are looked up by their Cranelift name
        // (e.g. "Memcpy", "CeilF32"); none is trusted by default.
        ExternalName::LibCall(l) => Ok(l.to_string()),
        other => Err(ClifError::UnsupportedCallee {
            function: caller.to_string(),
            callee: format!("{other:?}"),
        }),
    }
}

/// `true` if the reachable CFG has any cycle. Natural loops come from
/// Cranelift's `LoopAnalysis`; an iterative DFS additionally catches
/// irreducible cycles (no header dominating the cycle), which natural-loop
/// analysis does not report.
fn has_loops(func: &Function) -> bool {
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut la = LoopAnalysis::new();
    la.compute(func, &cfg, &domtree);
    if la.loops().next().is_some() {
        return true;
    }
    let Some(entry) = func.layout.entry_block() else {
        return false;
    };
    // 0 = unvisited, 1 = on the DFS stack, 2 = finished.
    let mut state: BTreeMap<Block, u8> = BTreeMap::new();
    let mut stack: Vec<(Block, Vec<Block>)> = vec![(entry, cfg.succ_iter(entry).collect())];
    state.insert(entry, 1);
    while let Some((block, succs)) = stack.last_mut() {
        match succs.pop() {
            Some(next) => match state.get(&next).copied().unwrap_or(0) {
                0 => {
                    state.insert(next, 1);
                    let s = cfg.succ_iter(next).collect();
                    stack.push((next, s));
                }
                1 => return true,
                _ => {}
            },
            None => {
                state.insert(*block, 2);
                stack.pop();
            }
        }
    }
    false
}
