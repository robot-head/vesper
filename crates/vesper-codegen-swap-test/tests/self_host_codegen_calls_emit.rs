//! The `call` family's emission (self-hosting SH4 lane 7, #1281):
//! `call_hook` of packages/vesper-codegen-calls/emit emits the call
//! instruction `Codegen::call_function_into` emits, through the shim's lent
//! handles.
//!
//! - `call_emission_matches_cranelift`: the same caller built twice, once
//!   with Rust's `ins().call` / `ins().call_indirect` and once through the
//!   hook, over direct calls of every argument count up to 8, register
//!   results (two words), the status ABI (a result and a status word) and
//!   `f32`/`f64`/`i128` lanes; the CLIF text must be equal.
//! - `the_call_swap_matches_the_rust_lowering`: installed as the `call`
//!   hook over the corpus, it emits every call `call_function_into` makes
//!   and declines the graph walk's `INSTRUCTION` requests; every
//!   function's CLIF text, every object and every sampled JIT run equals
//!   the Rust build's, optimize on and off. `CODEGEN_CALLS_STEP=n` compares
//!   every n-th world (default 1).
//! - `the_call_swap_matches_on_test_sources`: the same over the programs
//!   of the feature tests (`dyn` slots, the status ABI, register results),
//!   every entry `main` or the first function with no parameters.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/codegen_calls_sources.rs"]
mod codegen_calls_sources;

use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;
use vesper_cranelift_sys::cranelift_codegen::ir::{self, AbiParam, InstBuilder, Signature, types};
use vesper_cranelift_sys::cranelift_codegen::isa::CallConv;
use vesper_cranelift_sys::cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_cranelift_sys::cranelift_module::{Linkage, Module};

/// The ops of `call_hook` (packages/vesper-codegen-calls/emit).
const CALL: u32 = 1;
const CALL_INDIRECT: u32 = 2;

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect, or a test that first calls it under
    // `with_hooks` would wait on its own `OnceLock`.
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked =
                foreign_package(&check_harness::repo(), "packages/vesper-codegen-calls/emit");
            hook_function(&checked, "call_hook")
        })
    })
}

/// Calls the hook emitted indirectly (`dyn` slots).
static INDIRECT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn call(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    let handled = vesper_hook(lent, hook())?;
    if handled && lent.op == CALL_INDIRECT {
        INDIRECT.fetch_add(1, Ordering::Relaxed);
    }
    Ok(handled)
}

fn big_stack(f: impl FnOnce() + Send) {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        call: Some(call),
        ..CodegenHooks::default()
    }
}

/// The entry of a test-source world: `main`, else the first function with
/// no parameters and no generics, by name.
fn source_entry(w: &vesper::check::World) -> Option<String> {
    if w.functions.contains_key("main") {
        return Some("main".into());
    }
    w.functions
        .iter()
        .find(|(n, f)| !n.contains('$') && f.params.is_empty() && f.generics.is_empty())
        .map(|(n, _)| n.clone())
}

#[test]
fn the_call_swap_matches_on_test_sources() {
    big_stack(|| {
        hook();
        let handled = HANDLED.load(Ordering::Relaxed);
        let indirect = INDIRECT.load(Ordering::Relaxed);
        let mut total = Diff::default();
        for (name, w) in codegen_calls_sources::source_worlds() {
            let Some(entry) = source_entry(&w) else {
                continue;
            };
            for optimize in [true, false] {
                total.add(compare(&name, &w, &entry, hooks(), optimize, false));
            }
        }
        let handled = HANDLED.load(Ordering::Relaxed) - handled;
        let indirect = INDIRECT.load(Ordering::Relaxed) - indirect;
        eprintln!(
            "call swap on test sources: {}; {handled} calls emitted from Vesper ({indirect} indirect)",
            total.summary()
        );
        assert!(total.worlds > 20, "{}", total.summary());
        assert!(indirect > 0, "no indirect call was emitted");
        assert!(total.is_clean(), "{}", total.summary());
    })
}

fn jit_module() -> vesper_cranelift_sys::cranelift_jit::JITModule {
    use vesper_cranelift_sys::cranelift_codegen::settings::{self, Configurable};
    use vesper_cranelift_sys::{cranelift_jit, cranelift_native};
    let mut flags = settings::builder();
    flags.set("enable_llvm_abi_extensions", "true").unwrap();
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    cranelift_jit::JITModule::new(cranelift_jit::JITBuilder::with_isa(
        isa,
        vesper_cranelift_sys::cranelift_module::default_libcall_names(),
    ))
}

/// How the caller emits its call.
#[derive(Clone, Copy, PartialEq)]
enum Via {
    Rust,
    Hook,
}

/// A callee signature: parameter and result lanes.
struct Shape {
    params: Vec<ir::Type>,
    returns: Vec<ir::Type>,
}

fn signature(cc: CallConv, s: &Shape) -> Signature {
    let mut sig = Signature::new(cc);
    sig.params
        .extend(s.params.iter().map(|t| AbiParam::new(*t)));
    sig.returns
        .extend(s.returns.iter().map(|t| AbiParam::new(*t)));
    sig
}

/// The CLIF text of a caller that takes the callee's parameters (and, for
/// an indirect call, a target address last) and returns the sum of the
/// call's integer results' low words (or the first result).
fn caller(shape: &Shape, indirect: bool, via: Via) -> Result<String, String> {
    let mut m = jit_module();
    let cc = m.target_config().default_call_conv;
    let callee_sig = signature(cc, shape);
    let callee = m
        .declare_function("callee", Linkage::Import, &callee_sig)
        .map_err(|e| e.to_string())?;
    let mut caller_sig = Signature::new(cc);
    caller_sig
        .params
        .extend(shape.params.iter().map(|t| AbiParam::new(*t)));
    if indirect {
        caller_sig.params.push(AbiParam::new(types::I64));
    }
    caller_sig
        .returns
        .extend(shape.returns.iter().map(|t| AbiParam::new(*t)));
    let mut func = ir::Function::with_name_signature(ir::UserFuncName::user(0, 1), caller_sig);
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut ctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params = b.block_params(entry).to_vec();
    let (values, target) = if indirect {
        let (t, v) = params.split_last().unwrap();
        (v.to_vec(), Some(*t))
    } else {
        (params, None)
    };
    let results: Vec<ir::Value> = match (target, via) {
        (None, Via::Rust) => {
            let fun = m.declare_func_in_func(callee, b.func);
            let c = b.ins().call(fun, &values);
            b.inst_results(c).to_vec()
        }
        (Some(t), Via::Rust) => {
            let sig = b.import_signature(callee_sig.clone());
            let c = b.ins().call_indirect(sig, t, &values);
            b.inst_results(c).to_vec()
        }
        (None, Via::Hook) => {
            let fun = m.declare_func_in_func(callee, b.func);
            let mut args = vec![u64::from(fun.as_u32())];
            args.extend(values.iter().map(|v| codegen_swap::word(*v)));
            lent_call(&mut b, &mut m, CALL, &args)?
        }
        (Some(t), Via::Hook) => {
            let sig = b.import_signature(callee_sig.clone());
            let mut args = vec![u64::from(sig.as_u32()), codegen_swap::word(t)];
            args.extend(values.iter().map(|v| codegen_swap::word(*v)));
            lent_call(&mut b, &mut m, CALL_INDIRECT, &args)?
        }
    };
    b.ins().return_(&results);
    b.finalize();
    Ok(func.display().to_string())
}

/// One request through `call_hook`; its answer as values.
fn lent_call(
    b: &mut FunctionBuilder<'_>,
    m: &mut dyn Module,
    op: u32,
    args: &[u64],
) -> Result<Vec<ir::Value>, String> {
    let f = hook();
    // SAFETY: `f` takes one word, the lent call's address, live for the call.
    let (answer, crossing) =
        vesper_cranelift_sys::lend(b, m, op, args, |call| unsafe { f.call(&[call]) });
    let answer = answer?;
    if let Some(e) = crossing.error {
        return Err(e);
    }
    if crossing.unreturned || answer != 1 {
        return Err(format!("call_hook answered {answer} for op {op}"));
    }
    crossing
        .results
        .iter()
        .map(|w| codegen_swap::value_of(std::slice::from_ref(w)))
        .collect()
}

fn shapes() -> Vec<Shape> {
    let i = types::I64;
    let mut out = vec![];
    // Direct calls of every arity up to `MAX_WORD_FUNCTION_ARGS`, with one
    // result word.
    for n in 0..=8 {
        out.push(Shape {
            params: vec![i; n],
            returns: vec![i],
        });
    }
    // A register result (two words), and the status ABI (result + status).
    out.push(Shape {
        params: vec![i, i],
        returns: vec![i, i],
    });
    out.push(Shape {
        params: vec![i, i, i],
        returns: vec![i, i, i],
    });
    // Float and wide lanes.
    out.push(Shape {
        params: vec![types::F64, types::F32, i],
        returns: vec![types::F64],
    });
    out.push(Shape {
        params: vec![types::I128, i],
        returns: vec![types::I128],
    });
    out
}

#[test]
fn call_emission_matches_cranelift() {
    let mut compared = 0;
    for shape in shapes() {
        for indirect in [false, true] {
            let rust = caller(&shape, indirect, Via::Rust).expect("Rust caller");
            let hooked = caller(&shape, indirect, Via::Hook).expect("hooked caller");
            assert_eq!(
                rust, hooked,
                "{:?} -> {:?} (indirect {indirect})",
                shape.params, shape.returns
            );
            compared += 1;
        }
    }
    assert_eq!(compared, shapes().len() * 2);
}

#[test]
fn the_call_swap_matches_the_rust_lowering() {
    big_stack(call_swap);
}

fn call_swap() {
    let step = std::env::var("CODEGEN_CALLS_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1usize)
        .max(1);
    let hooks = hooks();
    hook();
    let handled = HANDLED.load(Ordering::Relaxed);
    let declined = DECLINED.load(Ordering::Relaxed);
    let mut total = Diff::default();
    for (i, case) in check_harness::corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .step_by(step)
        .enumerate()
    {
        let f = &case.world.functions["main"];
        // Every fourth world whose `main` has no parameters or effects.
        let run = i.is_multiple_of(4)
            && f.params.is_empty()
            && f.effects.instances.is_empty()
            && f.effects.tail.is_none();
        for optimize in [true, false] {
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks,
                optimize,
                run,
            ));
        }
    }
    let handled = HANDLED.load(Ordering::Relaxed) - handled;
    let declined = DECLINED.load(Ordering::Relaxed) - declined;
    eprintln!(
        "call swap: {}; {handled} calls emitted from Vesper, {declined} requests declined",
        total.summary()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(handled > 0, "the call hook never emitted a call");
    assert!(total.is_clean(), "{}", total.summary());
}
