//! Checks an image must pass before it is linked: no floating point on
//! riscv64imac, object code equal to the measured code, Silicon's stack
//! analysis and Silicon's interrupt-mask (atomic-context) rules.

use std::collections::BTreeMap;

use cranelift_codegen::control::ControlPlane;
use object::{self, Object, ObjectSection, ObjectSymbol, SymbolSection};
use vesper_silicon::atomic::{AtomicPolicy, check_atomic};

use vesper_silicon::stack::{Program, StackPolicy, StackRoot};

use super::runtime::Runtime;
use super::{FreestandingError, FreestandingTarget, StackRootSummary};
use crate::native::EmittedFunction;

/// Symbols that suspend the current fiber (Pulse / fiber ABI v2).
pub const SUSPENDING_SYMBOLS: [&str; 2] = ["vesper_fiber_park", "vesper_proto_pulse_read"];
/// Symbols that allocate.
pub const ALLOCATING_SYMBOLS: [&str; 6] = [
    "malloc",
    "calloc",
    "realloc",
    "free",
    "__rust_alloc",
    "vesper_alloc",
];

/// riscv64imac has no F or D: reject any floating-point or vector value.
pub fn reject_floating_point(functions: &[EmittedFunction]) -> Result<(), FreestandingError> {
    for f in functions {
        let dfg = &f.function.dfg;
        let uses_float = f
            .function
            .signature
            .params
            .iter()
            .chain(&f.function.signature.returns)
            .any(|p| p.value_type.is_float() || p.value_type.is_vector())
            || dfg.values().any(|v| {
                let t = dfg.value_type(v);
                t.is_float() || t.is_vector()
            });
        if uses_float {
            return Err(FreestandingError::UnsupportedOperation {
                function: f.source.clone(),
                what: "floating point (riscv64imac has no F or D extension)".into(),
            });
        }
    }
    Ok(())
}

/// The bytes of `symbol` in `object` must equal `code`.
pub fn compare_code(
    object_bytes: &[u8],
    symbol: &str,
    code: &[u8],
) -> Result<(), FreestandingError> {
    let file =
        object::File::parse(object_bytes).map_err(|e| FreestandingError::Object(e.to_string()))?;
    let mismatch = || FreestandingError::CodeMismatch {
        function: symbol.into(),
    };
    let sym = file
        .symbols()
        .find(|s| s.name() == Ok(symbol))
        .ok_or_else(mismatch)?;
    let SymbolSection::Section(index) = sym.section() else {
        return Err(mismatch());
    };
    let section = file
        .section_by_index(index)
        .map_err(|e| FreestandingError::Object(e.to_string()))?;
    let data = section
        .data()
        .map_err(|e| FreestandingError::Object(e.to_string()))?;
    let start = usize::try_from(sym.address()).map_err(|_| mismatch())?;
    let bytes = data.get(start..start + code.len()).ok_or_else(mismatch)?;
    if bytes != code || sym.size() != code.len() as u64 {
        return Err(mismatch());
    }
    Ok(())
}

pub struct Analysed {
    pub roots: Vec<StackRootSummary>,
    pub frames: BTreeMap<String, u32>,
}

/// Silicon's stack analysis and interrupt-mask rules over the user and
/// runtime functions of one image.
pub fn analyse(
    target: FreestandingTarget,
    emitted: &[EmittedFunction],
    code_sizes: &BTreeMap<String, usize>,
    runtime: &Runtime,
    stack_bytes: u32,
    exit_stack_bytes: u32,
) -> Result<Analysed, FreestandingError> {
    let isa = target.isa()?;
    let mut code_sizes = code_sizes.clone();
    // Silicon names -> object symbols, for reports.
    let mut symbol_of: BTreeMap<String, String> = BTreeMap::new();
    let mut functions = Vec::new();
    for f in emitted {
        let name = format!("u0:{}", f.func_id);
        symbol_of.insert(name, f.symbol.clone());
        for (id, callee) in &f.callees {
            symbol_of
                .entry(format!("u0:{id}"))
                .or_insert_with(|| callee.clone());
        }
        functions.push(f.function.clone());
    }
    for (symbol, func) in &runtime.functions {
        // The runtime object must hold exactly the code Silicon measures.
        let mut ctx = cranelift_codegen::Context::for_function(func.clone());
        let compiled = ctx
            .compile(&*isa, &mut ControlPlane::default())
            .map_err(|e| FreestandingError::Codegen(format!("{symbol}: {:?}", e.inner)))?;
        compare_code(&runtime.object, symbol, compiled.code_buffer())?;
        code_sizes.insert(symbol.clone(), compiled.code_buffer().len());
        symbol_of.insert(
            vesper_silicon::stack::symbol_of_user_func(&func.name),
            symbol.clone(),
        );
        functions.push(func.clone());
    }
    symbol_of.insert(runtime.exit.clone(), super::stub::EXIT_SYMBOL.into());
    let program = Program::compile_with_isa_flags(target.arch(), target.isa_flags(), functions)
        .map_err(|e| FreestandingError::Codegen(format!("stack analysis: {e}")))?;
    // Silicon compiled the same functions with the same ISA: sizes agree.
    for m in program.measurements() {
        let symbol = symbol_of
            .get(&m.name)
            .cloned()
            .unwrap_or_else(|| m.name.clone());
        let expected = code_sizes.get(&symbol).copied();
        if expected != Some(m.code_bytes) {
            return Err(FreestandingError::CodeMismatch { function: symbol });
        }
    }
    let mut external_bounds = BTreeMap::new();
    external_bounds.insert(runtime.exit.clone(), exit_stack_bytes);
    let policy = StackPolicy {
        roots: vec![
            StackRoot {
                function: runtime.main.clone(),
                budget_bytes: stack_bytes,
            },
            StackRoot {
                function: runtime.trap.clone(),
                budget_bytes: stack_bytes,
            },
        ],
        indirect_targets: BTreeMap::new(),
        external_bounds,
    };
    let report = program.check_stack(&policy);
    let rename = |findings: Vec<vesper_silicon::VerifierFinding>| {
        findings
            .into_iter()
            .map(|mut f| {
                if let Some(s) = symbol_of.get(&f.function) {
                    f.function = s.clone();
                }
                f
            })
            .collect::<Vec<_>>()
    };
    if !report.findings.is_empty() {
        return Err(FreestandingError::Stack(rename(report.findings)));
    }
    let mut roots = Vec::new();
    for r in &report.roots {
        let worst = r.worst_case_bytes.ok_or_else(|| {
            FreestandingError::Stack(vec![vesper_silicon::VerifierFinding {
                function: symbol_of.get(&r.root).cloned().unwrap_or_default(),
                inst: None,
                rule: vesper_silicon::stack::RULE_BUDGET,
                message: "unbounded stack depth".into(),
            }])
        })?;
        roots.push(StackRootSummary {
            root: symbol_of
                .get(&r.root)
                .cloned()
                .unwrap_or_else(|| r.root.clone()),
            worst_case_bytes: worst,
            worst_path: r
                .worst_path
                .iter()
                .map(|n| symbol_of.get(n).cloned().unwrap_or_else(|| n.clone()))
                .collect(),
            budget_bytes: r.budget_bytes,
        });
    }
    let findings = interrupt_rules(&program, &runtime.main, &runtime.trap, &symbol_of);
    if !findings.is_empty() {
        return Err(FreestandingError::Interrupt(rename(findings)));
    }
    let mut frames = BTreeMap::new();
    for node in &program.graph().functions {
        let symbol = symbol_of
            .get(&node.name)
            .cloned()
            .unwrap_or_else(|| node.name.clone());
        frames.insert(symbol, node.frame_bytes);
    }
    Ok(Analysed { roots, frames })
}

/// Silicon's interrupt-mask rules for an image. The whole image runs with
/// local interrupts masked (the stub masks them and nothing unmasks), so
/// every function reachable from `main` or `trap` is atomic context: both are
/// ISR roots for [`check_atomic`], which then rejects any path to a
/// suspending or allocating symbol.
pub fn interrupt_rules(
    program: &Program,
    main: &str,
    trap: &str,
    symbol_of: &BTreeMap<String, String>,
) -> Vec<vesper_silicon::VerifierFinding> {
    let by_symbol = |list: &[&str]| -> Vec<String> {
        symbol_of
            .iter()
            .filter(|(_, s)| list.contains(&s.as_str()))
            .map(|(n, _)| n.clone())
            .collect()
    };
    let atomic = AtomicPolicy {
        isr_roots: vec![main.to_string(), trap.to_string()],
        suspending: by_symbol(&SUSPENDING_SYMBOLS),
        allocating: by_symbol(&ALLOCATING_SYMBOLS),
        ..AtomicPolicy::default()
    };
    check_atomic(program, &atomic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{
        AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, Signature, UserExternalName,
        UserFuncName, types::I64,
    };
    use cranelift_codegen::isa::CallConv;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    /// `u1:0` calls the external `u0:7`.
    fn caller() -> Function {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.returns.push(AbiParam::new(I64));
        let mut f = Function::with_name_signature(
            UserFuncName::user(1, 0),
            Signature::new(CallConv::SystemV),
        );
        let mut ctx = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut f, &mut ctx);
        let block = b.create_block();
        b.switch_to_block(block);
        let sig_ref = b.import_signature(sig);
        let name = b
            .func
            .declare_imported_user_function(UserExternalName::new(0, 7));
        let callee = b.import_function(ExtFuncData {
            name: ExternalName::user(name),
            signature: sig_ref,
            colocated: false,
        });
        b.ins().call(callee, &[]);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
        f
    }

    fn findings(symbol: &str) -> Vec<vesper_silicon::VerifierFinding> {
        let target = FreestandingTarget::Riscv64Imac;
        let program =
            Program::compile_with_isa_flags(target.arch(), target.isa_flags(), vec![caller()])
                .unwrap();
        let mut symbols = BTreeMap::new();
        symbols.insert("u0:7".to_string(), symbol.to_string());
        interrupt_rules(&program, "u1:0", "u1:0", &symbols)
    }

    #[test]
    fn masked_image_may_not_suspend_or_allocate() {
        let suspend = findings("vesper_fiber_park");
        assert!(
            suspend
                .iter()
                .any(|f| f.rule == vesper_silicon::atomic::RULE_SUSPEND_IN_ISR),
            "{suspend:?}"
        );
        let alloc = findings("malloc");
        assert!(
            alloc
                .iter()
                .any(|f| f.rule == vesper_silicon::atomic::RULE_ALLOC_IN_ISR),
            "{alloc:?}"
        );
        assert!(findings("vesper_board_exit").is_empty());
    }
}
