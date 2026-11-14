//! Stage-5 verifier hooks over the Cranelift functions `src/native.rs` emits
//! (integration B, issue #136).
//!
//! The hook point is [`crate::native::object_with_observer`]: it hands every
//! emitted function to a callback as a [`crate::native::EmittedFunction`]
//! (the finalized IR, cloned before machine-code generation, named
//! `u0:<func_id>`), and with no observer it is exactly
//! `object_with_options`, so programs compile to identical code when no
//! verifier flag is given. This module runs the wave-five backend verifiers
//! on that set:
//!
//! | flag | verifier | crate |
//! | --- | --- | --- |
//! | `const-time` | constant-time SSA taint | `vesper_vault::verify_constant_time` |
//! | `stack` | worst-case stack depth, shared report | `vesper_silicon::stack` via [`crate::stack_bound`] |
//! | `realtime` | transitive realtime confinement | `vesper_resonance::{clif, confine}` |
//! | `detfloat` | deterministic floating point | `vesper_kinetic::detfloat::audit` |
//!
//! Annotations the source language cannot express yet (secret parameters,
//! stack budgets, realtime roots, allowed float calls) come from a JSON
//! policy file ([`NativeVerifierPolicy`], schema
//! `vesper.native-verifiers/1`). The source capability effects
//! `eff [ConstTime]`, `eff [Realtime]` and `eff [DetFloat]` (lane 15) imply
//! the same policies: [`policy_from_capabilities`] derives them from
//! [`crate::capability_effects::capability_policies`] and the emitted set, and
//! [`verify_native_with_capabilities`] runs the verifiers those effects imply
//! (lane 24, issue #266). `vesper build` and `vesper emit --emit
//! object|clif` call it, so a capability program is checked at the backend
//! without any flag.
//!
//! Findings name source functions (instance keys) and carry the source span
//! of the function. When a verifier names an instruction, the finding also
//! carries the source byte offset the backend attached to that Cranelift
//! instruction ([`crate::native::source_offset`], lane 23, #346), and
//! [`render`] points at it instead of at the function.
use crate::{check::World, native::EmittedFunction, stack_bound};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

pub const POLICY_SCHEMA: &str = "vesper.native-verifiers/1";

/// One backend verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NativeVerifier {
    ConstTime,
    Stack,
    Realtime,
    DetFloat,
}

impl NativeVerifier {
    pub const ALL: [NativeVerifier; 4] = [
        NativeVerifier::ConstTime,
        NativeVerifier::Stack,
        NativeVerifier::Realtime,
        NativeVerifier::DetFloat,
    ];
    /// Name used in `--verify=<list>` and in findings.
    pub fn name(self) -> &'static str {
        match self {
            NativeVerifier::ConstTime => "const-time",
            NativeVerifier::Stack => "stack",
            NativeVerifier::Realtime => "realtime",
            NativeVerifier::DetFloat => "detfloat",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }
    /// The single-verifier flag (`--verify-const-time`, ...).
    pub fn from_flag(flag: &str) -> Option<Self> {
        flag.strip_prefix("--verify-").and_then(Self::from_name)
    }
}

/// Parse `const-time,stack` (or `all`). Empty or unknown entries are errors.
pub fn parse_verifier_list(list: &str) -> Result<BTreeSet<NativeVerifier>, String> {
    let mut out = BTreeSet::new();
    for item in list.split(',') {
        let item = item.trim();
        if item == "all" {
            out.extend(NativeVerifier::ALL);
        } else if let Some(v) = NativeVerifier::from_name(item) {
            out.insert(v);
        } else {
            return Err(format!(
                "unknown verifier '{item}'; expected const-time, stack, realtime, detfloat or all"
            ));
        }
    }
    Ok(out)
}

/// Constant-time policy of one source function.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConstTimeEntry {
    /// Secret parameters by source name. Scalars passed by value are secret
    /// values; `inout` scalars and aggregates are pointers to secret memory.
    #[serde(default)]
    pub secret: Vec<String>,
    /// Callees (source functions or imported symbols) whose results are
    /// declassified.
    #[serde(default)]
    pub declassify: Vec<String>,
    /// Callees trusted to be constant time.
    #[serde(default)]
    pub constant_time_callees: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StackSection {
    /// Root function -> `@stack_limit(bytes)` budget.
    #[serde(default)]
    pub roots: BTreeMap<String, u32>,
    /// Imported symbol (or `libcall:<Name>`) -> declared worst-case bytes.
    #[serde(default)]
    pub external_bounds: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RealtimeSymbol {
    #[serde(default)]
    pub effects: Vec<String>,
    #[serde(default)]
    pub frame_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RealtimeSection {
    /// Root function -> optional stack budget in bytes.
    #[serde(default)]
    pub roots: BTreeMap<String, Option<u64>>,
    /// Effects roots may reach; defaults to Resonance's realtime set.
    #[serde(default)]
    pub allowed_effects: Option<Vec<String>>,
    /// Imported symbols and their effects. An import that is not listed is
    /// an unknown callee, which the verifier rejects when reachable.
    #[serde(default)]
    pub symbols: BTreeMap<String, RealtimeSymbol>,
    /// Declared direct effects of source functions.
    #[serde(default)]
    pub effects: BTreeMap<String, Vec<String>>,
    /// Attested total loop bounds of source functions.
    #[serde(default)]
    pub loop_bounds: BTreeMap<String, u64>,
    #[serde(default = "default_true")]
    pub reject_unbounded_loops: bool,
    #[serde(default)]
    pub max_loop_iterations: Option<u64>,
}

impl Default for RealtimeSection {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            allowed_effects: None,
            symbols: BTreeMap::new(),
            effects: BTreeMap::new(),
            loop_bounds: BTreeMap::new(),
            reject_unbounded_loops: true,
            max_loop_iterations: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DetFloatSection {
    /// Source functions to audit; every emitted function when absent.
    #[serde(default)]
    pub functions: Option<Vec<String>>,
    #[serde(default)]
    pub allow_fma: bool,
    /// Callees with floats in their signature that are allowed (for
    /// example a correctly rounded `sin`).
    #[serde(default)]
    pub allowed_float_calls: Vec<String>,
    /// `pairwise` (default) or `sequential`.
    #[serde(default)]
    pub reduction_order: Option<String>,
}

/// Policy file contents. Every section is optional; a verifier whose
/// section names nothing to check reports that, rather than passing
/// silently.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeVerifierPolicy {
    pub schema: String,
    #[serde(default)]
    pub const_time: BTreeMap<String, ConstTimeEntry>,
    #[serde(default)]
    pub stack: StackSection,
    #[serde(default)]
    pub realtime: RealtimeSection,
    #[serde(default)]
    pub det_float: DetFloatSection,
}

impl NativeVerifierPolicy {
    pub fn parse(text: &str) -> Result<Self, String> {
        let policy: Self =
            serde_json::from_str(text).map_err(|e| format!("verifier policy: {e}"))?;
        if policy.schema != POLICY_SCHEMA {
            return Err(format!(
                "verifier policy: schema must be \"{POLICY_SCHEMA}\", found \"{}\"",
                policy.schema
            ));
        }
        Ok(policy)
    }
}

/// A finding mapped back to source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeFinding {
    pub verifier: NativeVerifier,
    /// Source function (instance key) the finding is about.
    pub function: String,
    /// Cranelift instruction index, when the verifier reports one.
    pub inst: Option<u32>,
    pub rule: String,
    pub message: String,
    /// Byte span of the source function, when it is a source function.
    pub span: Option<(usize, usize)>,
    /// Source byte offset of the instruction `inst` names, when the backend
    /// attached one: the start of the Place-IR instruction or source
    /// expression the Cranelift instruction was lowered from.
    pub location: Option<usize>,
}

/// The Cranelift instruction a verifier's index names. Vault and the stack
/// analysis report entity indices (`Inst::as_u32`); Resonance and Kinetic
/// report positions in layout order.
fn verifier_inst(
    verifier: NativeVerifier,
    function: &cranelift_codegen::ir::Function,
    index: u32,
) -> Option<cranelift_codegen::ir::Inst> {
    match verifier {
        NativeVerifier::ConstTime | NativeVerifier::Stack => {
            Some(cranelift_codegen::ir::Inst::from_u32(index))
        }
        NativeVerifier::Realtime | NativeVerifier::DetFloat => function
            .layout
            .blocks()
            .flat_map(|b| function.layout.block_insts(b))
            .nth(index as usize),
    }
}

/// Result of [`verify_emitted`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeVerifierReport {
    pub verifiers: BTreeSet<NativeVerifier>,
    pub functions: usize,
    pub findings: Vec<NativeFinding>,
    /// The shared stack-bound report when `stack` ran.
    pub stack: Option<stack_bound::StackBoundReport>,
    /// Notes such as "no const_time entries: nothing to check".
    pub notes: Vec<String>,
}

impl NativeVerifierReport {
    pub fn accepted(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Compile `entry` like `vesper emit --emit object` and collect the emitted
/// functions. Returns the object bytes, CLIF text and the functions.
pub fn emit_and_collect(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
) -> Result<(Vec<u8>, String, Vec<EmittedFunction>), String> {
    let mut functions = Vec::new();
    let mut observer = |f: EmittedFunction| functions.push(f);
    let (bytes, clif) =
        crate::native::object_with_observer(world, entry, target, optimize, Some(&mut observer))?;
    Ok((bytes, clif, functions))
}

/// Names of the functions in the emitted set, both directions.
struct Names {
    /// func_id -> name (instance key for defined functions, symbol for imports).
    by_id: BTreeMap<u32, String>,
    /// name -> func_id.
    by_name: BTreeMap<String, u32>,
    defined: BTreeSet<u32>,
}

impl Names {
    fn new(functions: &[EmittedFunction]) -> Self {
        let mut by_id = BTreeMap::new();
        let mut defined = BTreeSet::new();
        for f in functions {
            by_id.insert(f.func_id, f.key.clone());
            defined.insert(f.func_id);
        }
        for f in functions {
            for (id, symbol) in &f.callees {
                by_id.entry(*id).or_insert_with(|| symbol.clone());
            }
        }
        // The entry keeps its source name as its symbol; other defined
        // functions are also reachable by their symbol.
        let mut by_name: BTreeMap<String, u32> =
            by_id.iter().map(|(id, n)| (n.clone(), *id)).collect();
        for f in functions {
            by_name.entry(f.symbol.clone()).or_insert(f.func_id);
        }
        Self {
            by_id,
            by_name,
            defined,
        }
    }
    fn user(&self, name: &str) -> Option<String> {
        self.by_name.get(name).map(|id| format!("u0:{id}"))
    }
    /// Map a verifier's function name (`u0:N`, a key, or a symbol) to a key.
    fn source(&self, name: &str) -> String {
        name.strip_prefix("u0:")
            .and_then(|i| i.parse::<u32>().ok())
            .and_then(|i| self.by_id.get(&i))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }
}

fn resolve_list(names: &Names, list: &[String], what: &str) -> Result<Vec<String>, String> {
    list.iter()
        .map(|n| {
            names
                .user(n)
                .ok_or_else(|| format!("verifier policy: {what} names unknown function '{n}'"))
        })
        .collect()
}

/// Compile every emitted function with exactly the ISA and flags object
/// emission uses ([`crate::native::object_isa`]), measure each activation's
/// stack bytes from the emitted prologue with Silicon's decoders, cross-check
/// against Cranelift's frame layout, and build the contract call graph
/// (direct calls by `u0:N` name, imports as external calls, libcalls from
/// relocations). This measures the code `vesper build` emits, not a
/// re-compilation under Silicon's own analysis flags.
fn measure_emitted(
    world: &World,
    functions: &[EmittedFunction],
    target: Option<&str>,
) -> Result<vesper_silicon::stack::CallGraph, String> {
    use cranelift_codegen::{
        Context, FinalizedRelocTarget, control::ControlPlane, ir::ExternalName,
    };
    use vesper_silicon::stack::{CallGraph, CallSite, ExternalCall, FnId, FnNode, IndirectCall};
    let isa = crate::native::object_isa_for(world, target)?;
    let x86 = match isa.triple().architecture {
        target_lexicon::Architecture::X86_64 => true,
        target_lexicon::Architecture::Aarch64(_) => false,
        other => {
            return Err(format!(
                "stack verifier supports x86_64 and aarch64, not {other}"
            ));
        }
    };
    let ids: BTreeMap<String, u32> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (format!("u0:{}", f.func_id), i as u32))
        .collect();
    let mut graph = CallGraph::default();
    let mut edges = BTreeSet::new();
    for (i, f) in functions.iter().enumerate() {
        let id = FnId(i as u32);
        let name = format!("u0:{}", f.func_id);
        let mut ctx = Context::for_function(f.function.clone());
        let compiled = ctx
            .compile(&*isa, &mut ControlPlane::default())
            .map_err(|e| format!("stack verifier: compiling '{}': {:?}", f.key, e.inner))?;
        let code = compiled.code_buffer();
        let measured = if x86 {
            vesper_silicon::prologue::measure_x86_64(code)
        } else {
            vesper_silicon::prologue::measure_aarch64(code)
        };
        let total = measured
            .total()
            .ok_or_else(|| format!("stack verifier: frame of '{}' overflows", f.key))?;
        let minimum = compiled
            .frame_size
            .checked_add(measured.frame_record_bytes)
            .and_then(|v| v.checked_add(measured.call_push_bytes))
            .ok_or_else(|| format!("stack verifier: frame of '{}' overflows", f.key))?;
        if total < minimum {
            return Err(format!(
                "stack verifier: measured {total} stack bytes for '{}' but its frame layout needs at least {minimum}",
                f.key
            ));
        }
        let mut libcalls = BTreeSet::new();
        for reloc in compiled.buffer.relocs() {
            if let FinalizedRelocTarget::ExternalName(ExternalName::LibCall(lc)) = &reloc.target {
                libcalls.insert(format!("libcall:{lc}"));
            }
        }
        graph.functions.push(FnNode {
            id,
            name,
            frame_bytes: total,
        });
        for site in vesper_silicon::stack::call_sites(&f.function) {
            match site {
                CallSite::Direct { inst, symbol, .. } => match ids.get(&symbol) {
                    Some(&callee) => {
                        if edges.insert((i as u32, callee)) {
                            graph.calls.push((id, FnId(callee)));
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
    Ok(graph)
}

fn effect(name: &str) -> vesper_resonance::confine::Effect {
    use vesper_resonance::confine::Effect as E;
    match name {
        "Realtime" => E::Realtime,
        "ConstDenorm" => E::ConstDenorm,
        "RtBus" => E::RtBus,
        "Isa" => E::Isa,
        "Mmio" => E::Mmio,
        "WaitFreeAtomic" => E::WaitFreeAtomic,
        "Alloc" => E::Alloc,
        "Dealloc" => E::Dealloc,
        "BlockingLock" => E::BlockingLock,
        "BlockingSyscall" => E::BlockingSyscall,
        "Async" => E::Async,
        "Net" => E::Net,
        "Storage" => E::Storage,
        "Unwind" => E::Unwind,
        "Ffi" => E::Ffi,
        other => E::Other(other.to_string()),
    }
}

/// Run `verifiers` over the emitted functions.
pub fn verify_emitted(
    world: &World,
    functions: &[EmittedFunction],
    target: Option<&str>,
    verifiers: &BTreeSet<NativeVerifier>,
    policy: &NativeVerifierPolicy,
) -> Result<NativeVerifierReport, String> {
    let names = Names::new(functions);
    let mut report = NativeVerifierReport {
        verifiers: verifiers.clone(),
        functions: functions.len(),
        findings: Vec::new(),
        stack: None,
        notes: Vec::new(),
    };
    let by_key: BTreeMap<&str, &EmittedFunction> =
        functions.iter().map(|f| (f.key.as_str(), f)).collect();
    let span_of = |key: &str| {
        let source = by_key.get(key).map_or(key, |f| f.source.as_str());
        world
            .functions
            .get(source)
            .map(|f| (f.span.start, f.span.end))
    };
    let location_of = |verifier: NativeVerifier, key: &str, inst: Option<u32>| {
        let emitted = by_key.get(key)?;
        let inst = verifier_inst(verifier, &emitted.function, inst?)?;
        crate::native::source_offset(&emitted.function, inst)
    };
    let push = |report: &mut NativeVerifierReport,
                verifier: NativeVerifier,
                function: &str,
                inst: Option<u32>,
                rule: &str,
                message: String| {
        let function = names.source(function);
        let span = span_of(&function);
        let location = location_of(verifier, &function, inst);
        report.findings.push(NativeFinding {
            verifier,
            function,
            inst,
            rule: rule.to_string(),
            message,
            span,
            location,
        });
    };

    if verifiers.contains(&NativeVerifier::ConstTime) {
        if policy.const_time.is_empty() {
            report.notes.push(
                "const-time: the policy has no const_time entries; nothing was checked".into(),
            );
        }
        for (name, entry) in &policy.const_time {
            let emitted = by_key.get(name.as_str()).ok_or_else(|| {
                format!("verifier policy: const_time names '{name}', which was not emitted")
            })?;
            // The instance's concrete parameter types decide which
            // parameters are passed by pointer (generic instances included).
            let source = emitted.instance(world)?;
            let sret = usize::from(crate::layout::Layout::of(world, &source.result)?.aggregate());
            let mut ct = vesper_vault::CtPolicy::default();
            for secret in &entry.secret {
                let (index, param) = source
                    .params
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.name == *secret)
                    .ok_or_else(|| {
                        format!("verifier policy: '{name}' has no parameter '{secret}'")
                    })?;
                let pointer = param.pass == crate::ast::Pass::Inout
                    || crate::layout::Layout::of(world, &param.ty)?.aggregate();
                if pointer {
                    ct.secret_pointer_params.push(index + sret);
                } else {
                    ct.secret_params.push(index + sret);
                }
            }
            ct.declassify
                .extend(resolve_list(&names, &entry.declassify, "declassify")?);
            ct.constant_time_callees.extend(resolve_list(
                &names,
                &entry.constant_time_callees,
                "constant_time_callees",
            )?);
            for f in vesper_vault::verify_constant_time(&emitted.function, &ct) {
                push(
                    &mut report,
                    NativeVerifier::ConstTime,
                    &f.function,
                    f.inst,
                    f.rule,
                    f.message,
                );
            }
        }
    }

    if verifiers.contains(&NativeVerifier::Stack) {
        if policy.stack.roots.is_empty() {
            report
                .notes
                .push("stack: the policy has no stack roots; only recursion and unbounded calls are reported".into());
        }
        let measured = measure_emitted(world, functions, target)?;
        let mut graph = stack_bound::StackGraph::from(&measured);
        for f in &mut graph.functions {
            f.name = names.source(&f.name);
        }
        for e in &mut graph.external {
            e.symbol = names.source(&e.symbol);
        }
        for root in policy.stack.roots.keys() {
            if !by_key.contains_key(root.as_str()) {
                return Err(format!(
                    "verifier policy: stack root '{root}' was not emitted"
                ));
            }
        }
        let bound = stack_bound::stack_bound_report(
            &graph,
            &stack_bound::StackBoundPolicy {
                roots: policy
                    .stack
                    .roots
                    .iter()
                    .map(|(function, budget)| stack_bound::StackBudget {
                        function: function.clone(),
                        budget_bytes: *budget,
                    })
                    .collect(),
                external_bounds: policy.stack.external_bounds.clone(),
                indirect_targets: BTreeMap::new(),
            },
        )?;
        for f in &bound.findings {
            push(
                &mut report,
                NativeVerifier::Stack,
                &f.function,
                f.inst,
                &f.rule,
                f.message.clone(),
            );
        }
        report.stack = Some(bound);
    }

    if verifiers.contains(&NativeVerifier::Realtime) {
        use vesper_resonance::{
            clif::{SymbolEffects, SymbolInfo, call_graph_from_clif},
            confine::{RealtimePolicy, RealtimeRoot, verify_realtime},
        };
        if policy.realtime.roots.is_empty() {
            report
                .notes
                .push("realtime: the policy has no realtime roots; nothing was checked".into());
        }
        let mut table = SymbolEffects::default();
        for (id, name) in &names.by_id {
            table.user_names.insert((0, *id), name.clone());
        }
        for (symbol, info) in &policy.realtime.symbols {
            if names
                .by_name
                .get(symbol)
                .is_some_and(|id| names.defined.contains(id))
            {
                return Err(format!(
                    "verifier policy: realtime symbol '{symbol}' is a source function; use realtime.effects"
                ));
            }
            table.symbols.insert(
                symbol.clone(),
                SymbolInfo {
                    effects: info.effects.iter().map(|e| effect(e)).collect(),
                    frame_bytes: info.frame_bytes,
                },
            );
        }
        for (function, effects) in &policy.realtime.effects {
            table.defined_effects.insert(
                function.clone(),
                effects.iter().map(|e| effect(e)).collect(),
            );
        }
        table.loop_bounds = policy.realtime.loop_bounds.clone();
        let input: Vec<(String, cranelift_codegen::ir::Function)> = functions
            .iter()
            .map(|f| (f.key.clone(), f.function.clone()))
            .collect();
        let graph =
            call_graph_from_clif(&input, &table).map_err(|e| format!("realtime verifier: {e}"))?;
        let mut roots = Vec::new();
        for (root, budget) in &policy.realtime.roots {
            let function = graph.by_name(root).ok_or_else(|| {
                format!("verifier policy: realtime root '{root}' was not emitted")
            })?;
            roots.push(RealtimeRoot {
                function,
                stack_budget_bytes: *budget,
            });
        }
        let mut rt = RealtimePolicy::new(roots);
        if let Some(allowed) = &policy.realtime.allowed_effects {
            rt.allowed_effects = allowed.iter().map(|e| effect(e)).collect();
        }
        rt.reject_unbounded_loops = policy.realtime.reject_unbounded_loops;
        rt.max_loop_iterations = policy.realtime.max_loop_iterations;
        for f in verify_realtime(&graph, &rt).map_err(|e| format!("realtime verifier: {e}"))? {
            push(
                &mut report,
                NativeVerifier::Realtime,
                &f.function,
                f.inst,
                f.rule,
                f.message,
            );
        }
    }

    if verifiers.contains(&NativeVerifier::DetFloat) {
        let mut det = vesper_kinetic::DetFloatPolicy {
            // Object emission enables Cranelift's NaN canonicalisation for
            // programs with DetFloat functions (`native::object_isa_for`).
            codegen_canonicalizes_nans: crate::native::needs_nan_canonicalization(world),
            allow_fma: policy.det_float.allow_fma,
            allowed_float_calls: policy.det_float.allowed_float_calls.clone(),
            user_names: names
                .by_id
                .iter()
                .map(|(id, n)| ((0, *id), n.clone()))
                .collect(),
            ..Default::default()
        };
        det.reduction_order = match policy.det_float.reduction_order.as_deref() {
            None | Some("pairwise") => vesper_kinetic::ReductionOrder::PairwiseTree,
            Some("sequential") => vesper_kinetic::ReductionOrder::Sequential,
            Some(other) => {
                return Err(format!(
                    "verifier policy: det_float.reduction_order must be pairwise or sequential, not '{other}'"
                ));
            }
        };
        let selected: Vec<&EmittedFunction> = match &policy.det_float.functions {
            None => functions.iter().collect(),
            Some(list) => list
                .iter()
                .map(|n| {
                    by_key.get(n.as_str()).copied().ok_or_else(|| {
                        format!("verifier policy: det_float names '{n}', which was not emitted")
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        // Calls between audited emitted functions are checked at the callee.
        for f in &selected {
            det.allowed_float_calls.push(f.key.clone());
        }
        for f in selected {
            let findings = vesper_kinetic::audit(&f.function, &det)
                .map_err(|e| format!("detfloat verifier on '{}': {e}", f.key))?;
            for finding in findings {
                push(
                    &mut report,
                    NativeVerifier::DetFloat,
                    &f.key,
                    finding.inst,
                    finding.rule,
                    finding.message,
                );
            }
        }
    }
    Ok(report)
}

/// Compile and verify in one call.
pub fn verify_native(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    verifiers: &BTreeSet<NativeVerifier>,
    policy: &NativeVerifierPolicy,
) -> Result<(Vec<u8>, String, NativeVerifierReport), String> {
    let (bytes, clif, functions) = emit_and_collect(world, entry, target, optimize)?;
    let report = verify_emitted(world, &functions, target, verifiers, policy)?;
    Ok((bytes, clif, report))
}

/// Render findings with `file:line:col` locations.
pub fn render(report: &NativeVerifierReport, file: &str, source: &str) -> String {
    let mut out = String::new();
    for note in &report.notes {
        out.push_str(&format!("note: {note}\n"));
    }
    if let Some(stack) = &report.stack {
        out.push_str(&stack_bound::render(stack));
    }
    for f in &report.findings {
        let at = |offset: usize| {
            let start = offset.min(source.len());
            let start = (0..=start)
                .rev()
                .find(|&i| source.is_char_boundary(i))
                .unwrap_or(0);
            let before = &source[..start];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("{file}:{line}:{column}: ")
        };
        let location = if let Some(offset) = f.location {
            at(offset)
        } else {
            f.span.map_or_else(String::new, |(start, end)| {
                // Function spans include leading comments; point at the item.
                let mut start = start.min(source.len());
                let end = end.min(source.len()).max(start);
                while let Some(rest) = source.get(start..end) {
                    let trimmed = rest.trim_start();
                    let skipped = rest.len() - trimmed.len();
                    if trimmed.starts_with("//") {
                        start += skipped + trimmed.find('\n').map_or(trimmed.len(), |n| n + 1);
                    } else {
                        start += skipped;
                        break;
                    }
                }
                at(start)
            })
        };
        let inst = f.inst.map_or_else(String::new, |i| format!(" (inst {i})"));
        out.push_str(&format!(
            "{location}error[{}/{}]: {}{inst}: {}\n",
            f.verifier.name(),
            f.rule,
            f.function,
            f.message
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Policies from source capability effects (lane 24, issue #266).

/// The verifiers the capability effects of `world` imply: `const-time` for
/// `eff [ConstTime]`, `realtime` for `eff [Realtime]` and `detfloat` for
/// `eff [DetFloat]`. Empty for a program without capability effects.
pub fn capability_verifiers(world: &World) -> BTreeSet<NativeVerifier> {
    use crate::capability_effects::capabilities;
    let mut out = BTreeSet::new();
    for f in world.functions.values() {
        let mut caps = capabilities(&f.effects);
        caps.extend(crate::native::closure_capabilities(f));
        out.extend(caps.into_iter().map(cap_verifier));
    }
    out
}

fn cap_verifier(c: crate::capability_effects::Capability) -> NativeVerifier {
    use crate::capability_effects::Capability;
    match c {
        Capability::ConstTime => NativeVerifier::ConstTime,
        Capability::Realtime => NativeVerifier::Realtime,
        Capability::DetFloat => NativeVerifier::DetFloat,
    }
}

/// The source facts for `world`'s capability effects.
fn source_capabilities(
    world: &World,
) -> Result<crate::capability_effects::CapabilityPolicies, String> {
    // `capability_policies` reads only the world of an accepted program; the
    // backend is only reached for programs that checked.
    let checked = crate::check::Checked {
        world: world.clone(),
        diagnostics: Vec::new(),
        expression_types: Default::default(),
    };
    crate::capability_effects::capability_policies(&checked).map_err(|e| e.to_string())
}

/// Emitted instance keys of source function `name` (one for a non-generic
/// function, one per instance for a generic one).
fn instances_of<'a>(functions: &'a [EmittedFunction], name: &str) -> Vec<&'a EmittedFunction> {
    functions.iter().filter(|f| f.source == name).collect()
}

/// Why a capability function has no Cranelift function of its own.
fn not_emitted(world: &World, name: &str) -> String {
    let inlined = world.functions.get(name).is_some_and(|f| {
        f.effects.tail.is_some()
            || f.effects
                .instances
                .iter()
                .any(|e| !crate::capability_effects::is_capability_effect(&e.name))
            || f.params
                .iter()
                .any(|p| matches!(p.ty, crate::ast::Type::Function { .. }))
            || matches!(f.result, crate::ast::Type::Function { .. })
    });
    if inlined {
        format!(
            "'{name}' is specialized into its callers (its row has non-capability effects other than Fail, or it returns a function value, or a function-value argument is not a top-level function); its code is checked only inside callers that carry the same capability"
        )
    } else {
        format!("'{name}' is not reachable from the entry and was not compiled")
    }
}

/// Derive a [`NativeVerifierPolicy`] from the source capability effects of
/// `world` for the emitted set `functions`, on top of `base` (a policy file,
/// or an empty policy). Entries in `base` win: a function the file already
/// names keeps the file's entry.
///
/// - `eff [ConstTime]`: one `const_time` entry per emitted function, with its
///   secret-typed parameters (`Secret[T]`, `SecU64`, ...) as `secret`, its
///   declassification-gate callees as `declassify` and its other (ConstTime)
///   callees as `constant_time_callees`. A generic ConstTime function gets
///   one entry per emitted instance, checked with that instance's concrete
///   parameter layouts.
/// - `eff [Realtime]`: every emitted realtime function is a realtime root
///   (the source roots are a subset, and a root that was specialized into
///   its caller has no function to start from), with its `Realtime` effect;
///   unbounded loops are rejected, since the source has no loops.
/// - `eff [DetFloat]`: `det_float.functions` lists the emitted DetFloat
///   functions; their DetFloat callees are allowed float calls; no FMA.
///
/// Returns the policy, notes about capability functions that could not be
/// checked on their own, and findings for capability functions the backend
/// cannot check at all (ConstTime closures in non-ConstTime functions).
pub fn policy_from_capabilities(
    world: &World,
    functions: &[EmittedFunction],
    base: &NativeVerifierPolicy,
) -> Result<CapabilityPolicy, String> {
    derive_policy(world, functions, base, false)
}

/// [`policy_from_capabilities`]; with `explicit_det_float_all`, an absent
/// `det_float.functions` in `base` stays absent (audit every emitted
/// function), because `--verify=detfloat` was asked for explicitly.
fn derive_policy(
    world: &World,
    functions: &[EmittedFunction],
    base: &NativeVerifierPolicy,
    explicit_det_float_all: bool,
) -> Result<CapabilityPolicy, String> {
    let explicit_det_float_all = explicit_det_float_all && base.det_float.functions.is_none();
    let caps = source_capabilities(world)?;
    let mut policy = base.clone();
    let mut notes = Vec::new();
    let mut findings = Vec::new();
    // Source callee names -> the emitted instance keys of those functions
    // (a callee that was specialized into its caller has none).
    let keep_emitted = |list: &[String]| -> Vec<String> {
        list.iter()
            .flat_map(|n| instances_of(functions, n))
            .map(|f| f.key.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };

    for ct in &caps.const_time {
        let source = world
            .functions
            .get(&ct.function)
            .ok_or_else(|| format!("capability policy names unknown function '{}'", ct.function))?;
        let instances = instances_of(functions, &ct.function);
        if instances.is_empty() {
            notes.push(format!("const-time: {}", not_emitted(world, &ct.function)));
            continue;
        }
        let secret = ct
            .secret_params
            .iter()
            .map(|&i| {
                source.params.get(i).map(|p| p.name.clone()).ok_or_else(|| {
                    format!("capability policy: '{}' has no parameter {i}", ct.function)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // One entry per emitted instance: a generic ConstTime function is
        // checked at each instance, whose concrete layouts the backend
        // exposes through `EmittedFunction::instance`.
        for f in instances {
            if policy.const_time.contains_key(&f.key)
                || (f.key != ct.function && policy.const_time.contains_key(&ct.function))
            {
                continue;
            }
            policy.const_time.insert(
                f.key.clone(),
                ConstTimeEntry {
                    secret: secret.clone(),
                    declassify: keep_emitted(&ct.declassify_callees),
                    constant_time_callees: keep_emitted(&ct.constant_time_callees),
                },
            );
        }
    }

    let rt_effect = crate::capability_effects::Capability::Realtime.name();
    for rt in &caps.realtime.functions {
        let instances = instances_of(functions, &rt.function);
        if instances.is_empty() {
            notes.push(format!("realtime: {}", not_emitted(world, &rt.function)));
            continue;
        }
        for f in instances {
            policy
                .realtime
                .roots
                .entry(f.key.clone())
                .or_insert(rt.stack_budget_bytes);
            // Only Realtime is a runtime-relevant effect here: ConstTime and
            // DetFloat constrain the body and are not Resonance effects.
            policy
                .realtime
                .effects
                .entry(f.key.clone())
                .or_insert_with(|| vec![rt_effect.to_string()]);
        }
    }
    if caps.realtime.reject_unbounded_loops {
        policy.realtime.reject_unbounded_loops = true;
    }

    let mut det: Vec<String> = Vec::new();
    for d in &caps.det_float {
        let instances = instances_of(functions, &d.function);
        if instances.is_empty() {
            notes.push(format!("detfloat: {}", not_emitted(world, &d.function)));
            continue;
        }
        det.extend(instances.iter().map(|f| f.key.clone()));
        for callee in keep_emitted(&d.allowed_float_calls) {
            if !policy.det_float.allowed_float_calls.contains(&callee) {
                policy.det_float.allowed_float_calls.push(callee);
            }
        }
        policy.det_float.allow_fma |= d.allow_fma;
    }
    // A capability closure is specialized into the function that contains
    // it, so its code can only be checked as part of that function. When the
    // container does not carry the capability itself, check the whole
    // container under it (stricter than the closure alone, never weaker), or
    // reject it where that is not possible (ConstTime needs the closure's
    // secret parameters, which the container's signature does not have).
    {
        use crate::capability_effects::{Capability, capabilities};
        for f in world.functions.values() {
            let own = capabilities(&f.effects);
            for cap in crate::native::closure_capabilities(f) {
                if own.contains(&cap) {
                    continue;
                }
                let instances = instances_of(functions, &f.name);
                if instances.is_empty() {
                    notes.push(format!(
                        "{}: a closure in {}",
                        cap_verifier(cap).name(),
                        not_emitted(world, &f.name)
                    ));
                    continue;
                }
                match cap {
                    Capability::ConstTime => findings.push(NativeFinding {
                        verifier: NativeVerifier::ConstTime,
                        function: f.name.clone(),
                        inst: None,
                        rule: "capability.closure-unsupported".into(),
                        message: "an eff [ConstTime] closure is specialized into this function, which is not ConstTime; the backend cannot tell which of its values are secret. Move the closure body into a named ConstTime function".into(),
                        span: Some((f.span.start, f.span.end)),
                        location: None,
                    }),
                    Capability::Realtime => {
                        notes.push(format!(
                            "realtime: '{}' contains an eff [Realtime] closure and is checked as a realtime root as a whole",
                            f.name
                        ));
                        for i in instances {
                            policy.realtime.roots.entry(i.key.clone()).or_insert(None);
                            policy
                                .realtime
                                .effects
                                .entry(i.key.clone())
                                .or_insert_with(|| vec![rt_effect.to_string()]);
                        }
                    }
                    Capability::DetFloat => {
                        notes.push(format!(
                            "detfloat: '{}' contains an eff [DetFloat] closure and is audited as a whole",
                            f.name
                        ));
                        det.extend(instances.iter().map(|i| i.key.clone()));
                    }
                }
            }
        }
    }
    if !det.is_empty() && !explicit_det_float_all {
        let list = policy.det_float.functions.get_or_insert_with(Vec::new);
        for f in det {
            if !list.contains(&f) {
                list.push(f);
            }
        }
    }
    Ok(CapabilityPolicy {
        policy,
        notes,
        findings,
    })
}

/// Result of [`policy_from_capabilities`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityPolicy {
    pub policy: NativeVerifierPolicy,
    pub notes: Vec<String>,
    pub findings: Vec<NativeFinding>,
}

/// Compile `entry` and run `requested` plus the verifiers the program's
/// capability effects imply ([`capability_verifiers`]), with the policy from
/// [`policy_from_capabilities`] layered under `base`. With no requested
/// verifier and no capability effect this runs nothing and is exactly
/// [`crate::native::object_with_options`]; the report's `verifiers` is
/// then empty.
pub fn verify_native_with_capabilities(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    requested: &BTreeSet<NativeVerifier>,
    base: &NativeVerifierPolicy,
) -> Result<(Vec<u8>, String, NativeVerifierReport), String> {
    let mut verifiers = requested.clone();
    verifiers.extend(capability_verifiers(world));
    if verifiers.is_empty() {
        let (bytes, clif) = crate::native::object_with_options(world, entry, target, optimize)?;
        let report = NativeVerifierReport {
            verifiers,
            functions: 0,
            findings: Vec::new(),
            stack: None,
            notes: Vec::new(),
        };
        return Ok((bytes, clif, report));
    }
    let (bytes, clif, functions) = emit_and_collect(world, entry, target, optimize)?;
    let derived = derive_policy(
        world,
        &functions,
        base,
        requested.contains(&NativeVerifier::DetFloat),
    )?;
    let mut report = verify_emitted(world, &functions, target, &verifiers, &derived.policy)?;
    report.notes.extend(derived.notes);
    report.findings.splice(0..0, derived.findings);
    Ok((bytes, clif, report))
}
