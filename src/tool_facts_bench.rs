//! `vesper tool-facts bench`: a checked finite scalar kernel, compiled for
//! `vesper bench` to load into its own process (#2670, part of #1397).
//!
//! `vesper bench` used to link this library to check the kernel and JIT it.
//! Now it asks for the kernel's native batch as machine code and relocations
//! ([`native::batch::compile_code`]) and loads them into its own JIT module,
//! so the measured windows still call native code in the bench process. The
//! answer also carries what bench reports beside the timings: the evaluator's
//! step count (`--steps`, for `--deterministic`) and the CLIF and machine
//! listing (`--asm`, for `--asm` and `--expect-vectorized`). Each of those is
//! its own `{ "ok": .. }` or `{ "error": .. }`, so bench raises it where it
//! raised it before.
use serde_json::{Value as Json, json};
use vesper::{
    ast::{ExprKind, Item, Literal, Module, PatternKind, Statement, Type},
    check,
    lexer::{self, TokenKind as K},
    native::{
        self,
        batch::{BatchCode, BatchLimits, BatchResult},
    },
    parser,
};

/// The kind `vesper bench` asks for.
pub const KIND: &str = "bench";

/// The largest kernel source `vesper bench` accepts.
const MAX_SOURCE_BYTES: u64 = 256 * 1024;

/// The evaluator step budget of one `--deterministic` call.
const STEP_FUEL: u64 = 50_000_000;

/// The fewest and most `--steps` repetitions bench asks for.
const MIN_STEP_REPETITIONS: usize = 1;
const MAX_STEP_REPETITIONS: usize = 64;

/// The kernel `entry` of `source`, compiled, with `steps` evaluator
/// repetitions and the `asm` listing when asked for.
pub fn bench(source: &str, entry: &str, steps: Option<usize>, asm: bool) -> Result<Json, String> {
    if source.len() as u64 > MAX_SOURCE_BYTES {
        return Err("bench source exceeds 256 KiB".into());
    }
    source_guard(source)?;
    let parsed = parser::parse(source);
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!(
            "{} at {:?}: {}",
            error.code, error.span, error.message
        ));
    }
    scalar_source(&parsed.module)?;
    let checked = check::check(&parsed.module);
    if let Some(error) = checked.diagnostics.first() {
        return Err(format!(
            "{} at {:?}: {}",
            error.code, error.span, error.message
        ));
    }
    let world = checked.world;
    let code = native::batch::compile_code(&world, &[entry], BatchLimits::default())?;
    let mut answer = json!({ "batch": batch_json(&code)? });
    if let Some(repetitions) = steps {
        if !(MIN_STEP_REPETITIONS..=MAX_STEP_REPETITIONS).contains(&repetitions) {
            return Err("bench step repetitions out of range".into());
        }
        answer["steps"] = outcome(deterministic_steps(&world, entry, repetitions));
    }
    if asm {
        answer["asm"] = outcome(asm_listing(&world, entry));
    }
    Ok(answer)
}

/// A nested result: `{ "ok": value }` or `{ "error": message }`.
fn outcome(result: Result<Json, String>) -> Json {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error }),
    }
}

fn result_json(result: BatchResult) -> Json {
    match result {
        BatchResult::Signed(bits) => json!({ "signed": bits }),
        BatchResult::Unsigned(bits) => json!({ "unsigned": bits }),
        BatchResult::F64 => json!("f64"),
        BatchResult::F32 => json!("f32"),
        BatchResult::Bool => json!("bool"),
        BatchResult::Unit => json!("unit"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn reloc_target_json(target: &cranelift_module::ModuleRelocTarget) -> Result<Json, String> {
    use cranelift_module::ModuleRelocTarget as T;
    match target {
        T::User { namespace, index } => Ok(json!({ "user": [namespace, index] })),
        T::LibCall(libcall) => Ok(json!({ "libcall": libcall.to_string() })),
        T::FunctionOffset(func, offset) => {
            Ok(json!({ "function_offset": [func.as_u32(), offset] }))
        }
        T::KnownSymbol(symbol) => Err(format!(
            "native batch relocation to known symbol {symbol:?} is unsupported"
        )),
    }
}

/// The batch as `vesper bench` loads it: declarations by function id (so a
/// fresh module gives each the same id), each defined function's code and
/// relocations (kinds and linkages by their Cranelift names), the entries
/// and the batch statistics.
fn batch_json(code: &BatchCode) -> Result<Json, String> {
    let declarations: Vec<Json> = code
        .declarations
        .iter()
        .map(|(symbol, linkage)| json!({ "symbol": symbol, "linkage": format!("{linkage:?}") }))
        .collect();
    let functions = code
        .functions
        .iter()
        .map(|f| {
            let relocs = f
                .relocs
                .iter()
                .map(|r| {
                    Ok(json!({
                        "offset": r.offset,
                        "kind": format!("{:?}", r.kind),
                        "target": reloc_target_json(&r.name)?,
                        "addend": r.addend,
                    }))
                })
                .collect::<Result<Vec<Json>, String>>()?;
            Ok(json!({
                "func_id": f.func_id,
                "alignment": f.alignment,
                "code": hex(&f.code),
                "relocs": relocs,
            }))
        })
        .collect::<Result<Vec<Json>, String>>()?;
    let entries: Vec<Json> = code
        .entries
        .iter()
        .map(|(name, func_id, result)| {
            json!({ "name": name, "func_id": func_id, "result": result_json(*result) })
        })
        .collect();
    let stats = code.stats;
    Ok(json!({
        "declarations": declarations,
        "functions": functions,
        "entries": entries,
        "stats": {
            "modules": stats.module_count,
            "compilations": stats.compilation_count,
            "defined_functions": stats.defined_functions,
            "entries": stats.entry_count,
            "max_entry_work": stats.max_entry_work,
        },
    }))
}

/// Evaluator steps for one call of `entry`, with its result.
fn evaluator_steps(world: &check::World, entry: &str) -> Result<(u64, String), String> {
    let mut runtime = vesper::runtime::Runtime::new(world);
    // The benchmark JIT compiles unoptimized IR; count the same program.
    runtime.optimize = false;
    runtime.fuel = STEP_FUEL;
    let value = runtime.run(entry, Vec::new())?;
    let steps = STEP_FUEL - runtime.fuel;
    if steps == 0 {
        return Err("evaluator reported zero steps".into());
    }
    Ok((steps, format!("{value:?}")))
}

/// The evaluator's steps and result, the same in every one of
/// `repetitions` calls.
fn deterministic_steps(
    world: &check::World,
    entry: &str,
    repetitions: usize,
) -> Result<Json, String> {
    let (steps, value) = evaluator_steps(world, entry)?;
    for _ in 1..repetitions {
        if evaluator_steps(world, entry)? != (steps, value.clone()) {
            return Err("evaluator step count is not deterministic".into());
        }
    }
    Ok(json!({ "steps": steps, "value": value }))
}

/// Machine-listing lines that name a vector register. On x86-64 scalar
/// floats also live in xmm registers, so this is a listing aid, not the
/// vectorization decision (which uses CLIF value types).
fn vector_register_line(line: &str) -> bool {
    const AARCH64: [&str; 8] = [".16b", ".8b", ".8h", ".4h", ".4s", ".2s", ".2d", ".1d"];
    line.contains("%xmm")
        || line.contains("%ymm")
        || line.contains("%zmm")
        || AARCH64.iter().any(|a| line.contains(a))
        || line.contains(" z0") // SVE
}

/// `vesper bench --asm`: every compiled function's CLIF and machine code,
/// recompiled with the benchmark JIT's ISA flags. `data` is the report's
/// `asm` object, `text` the human listing.
fn asm_listing(world: &check::World, entry: &str) -> Result<Json, String> {
    let mut functions = Vec::new();
    native::object_with_observer(world, entry, None, false, Some(&mut |f| functions.push(f)))?;
    if functions.is_empty() {
        return Err("no compiled functions to list".into());
    }
    let isa = native::batch::batch_isa(world)?;
    let mut text = String::new();
    let mut entries = Vec::new();
    let mut total_vector = 0usize;
    for f in functions {
        let func = &f.function;
        let mut vector = 0usize;
        let mut instructions = 0usize;
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                instructions += 1;
                let dfg = &func.dfg;
                if dfg.ctrl_typevar(inst).is_vector()
                    || dfg
                        .inst_results(inst)
                        .iter()
                        .any(|v| dfg.value_type(*v).is_vector())
                    || dfg
                        .inst_args(inst)
                        .iter()
                        .any(|v| dfg.value_type(*v).is_vector())
                {
                    vector += 1;
                }
            }
        }
        total_vector += vector;
        let clif = func.display().to_string();
        let mut ctx = cranelift_codegen::Context::for_function(func.clone());
        ctx.set_disasm(true);
        let compiled = ctx
            .compile(
                &*isa,
                &mut cranelift_codegen::control::ControlPlane::default(),
            )
            .map_err(|e| format!("{}: {:?}", f.symbol, e.inner))?;
        let machine = compiled
            .vcode
            .clone()
            .ok_or("Cranelift produced no machine listing")?;
        let bytes = compiled.code_buffer();
        let vector_lines = machine.lines().filter(|l| vector_register_line(l)).count();
        text.push_str(&format!(
            ";; {} ({}): {} CLIF instructions, {} vector-typed; {} code bytes\n{clif}\n;; machine code ({})\n{machine}\n",
            f.symbol,
            f.source,
            instructions,
            vector,
            bytes.len(),
            isa.triple(),
        ));
        entries.push(
            json!({"symbol":f.symbol,"source":f.source,"clif":clif,"machine":machine,
            "clif_instructions":instructions,"vector_clif_instructions":vector,
            "machine_vector_register_lines":vector_lines,"code_bytes":bytes.len(),
            "code_blake3":blake3::hash(bytes).to_hex().to_string()}),
        );
    }
    Ok(json!({
        "data": {"target":isa.triple().to_string(),"functions":entries,
            "scope":"the benchmark's unoptimized IR recompiled with the benchmark JIT's ISA flags; code bytes are this recompilation's, not read from JIT memory"},
        "text": text,
        "vector_instructions": total_vector,
    }))
}

fn source_guard(source: &str) -> Result<(), String> {
    let (tokens, errors) = lexer::lex(source);
    if !errors.is_empty() {
        return Err("bench source has lexical diagnostics".into());
    }
    if tokens.len() > 16_384 {
        return Err("bench source token limit".into());
    }
    let (mut depth, mut functions, mut conditionals, mut segment) =
        (0usize, 0usize, 0usize, 0usize);
    for token in tokens {
        if token.kind.trivia() || token.kind == K::Eof {
            continue;
        }
        match token.kind {
            K::LBrace | K::LBracket | K::LParen => {
                depth += 1;
                if depth > 32 {
                    return Err("bench source nesting limit".into());
                }
            }
            K::RBrace | K::RBracket | K::RParen => depth = depth.saturating_sub(1),
            _ => {}
        }
        functions += usize::from(token.kind == K::Fn);
        conditionals += usize::from(token.kind == K::If);
        if functions > 128 || conditionals > 64 {
            return Err("bench source function/conditional limit".into());
        }
        if matches!(token.kind, K::Semi | K::LBrace | K::RBrace) {
            segment = 0;
        } else {
            segment += 1;
            if segment > 256 {
                return Err("bench source expression/signature limit".into());
            }
        }
    }
    Ok(())
}
fn primitive(ty: &Type) -> bool {
    *ty == Type::unit()
        || matches!(
            ty,
            Type::Named(name, args)
                if args.is_empty()
                    && matches!(name.as_str(), "i64" | "u8" | "u32" | "u64" | "f64" | "bool" | "Unit")
        )
}
fn scalar_source(module: &Module) -> Result<(), String> {
    if !module.model_uses.is_empty() || !module.trait_bounds.is_empty() {
        return Err("bench source models/trait constraints are unsupported".into());
    }
    let mut bodies = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else {
            return Err(
                "bench currently supports ordinary scalar function declarations only".into(),
            );
        };
        if !function.generics.is_empty()
            || function.body.is_none()
            || !function.effects.instances.is_empty()
            || function.effects.tail.is_some()
            || !primitive(&function.result)
            || function.params.iter().any(|p| !primitive(&p.ty))
        {
            return Err("bench requires nongeneric pure primitive-scalar signatures".into());
        }
        names.insert(function.name.as_str());
        bodies.push((function.body.as_ref().expect("body checked"), 1usize));
    }
    // The initial kernel profile excludes inferred composite growth before the
    // unchanged checker. Byte/depth limits alone cannot bound repeated (x,x).
    const MAX_NODES: usize = 4096;
    let mut visited = 0usize;
    while let Some((expr, depth)) = bodies.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > 32 {
            return Err("bench scalar AST volume/depth limit".into());
        }
        let mut add = |child| -> Result<(), String> {
            if visited + bodies.len() >= MAX_NODES {
                return Err("bench scalar AST volume limit".into());
            }
            bodies.push((child, depth + 1));
            Ok(())
        };
        match &expr.kind {
            ExprKind::Literal(Literal::String(_)) => {
                return Err("bench string/aggregate expressions are unsupported".into());
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(values) if values.is_empty() => {}
            ExprKind::Unary(_, value) => add(value)?,
            ExprKind::Binary(_, left, right) | ExprKind::Assign(_, left, right) => {
                add(left)?;
                add(right)?;
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return Err("bench indirect/method calls are unsupported".into());
                };
                if !types.is_empty()
                    || (!names.contains(name.as_str())
                        && vesper::numeric::wrapping_intrinsic(name).is_none())
                {
                    return Err("bench generic/constructor/unknown calls are unsupported".into());
                }
                add(callee)?;
                for argument in args {
                    add(&argument.expr)?;
                }
            }
            ExprKind::Block { statements, tail } => {
                for statement in statements {
                    match statement {
                        Statement::Let { pat, ty, value, .. } => {
                            if !matches!(pat.kind, PatternKind::Bind(_) | PatternKind::Wild)
                                || (*ty != Type::Infer && !primitive(ty))
                            {
                                return Err(
                                    "bench composite patterns/annotations are unsupported".into()
                                );
                            }
                            add(value)?;
                        }
                        Statement::Expr(value) => add(value)?,
                    }
                }
                if let Some(tail) = tail {
                    add(tail)?;
                }
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                add(condition)?;
                add(then)?;
                if let Some(otherwise) = otherwise {
                    add(otherwise)?;
                }
            }
            _ => return Err("bench composite/closure/handler expressions are unsupported".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inferred_growth_is_rejected_without_calling_the_checker() {
        let mut source = String::from("fn main(){let x=0;");
        for _ in 0..25 {
            source.push_str("let x=(x,x);");
        }
        source.push('}');
        let parsed = parser::parse(&source);
        assert!(parsed.diagnostics.is_empty());
        assert!(scalar_source(&parsed.module).is_err());
        let generic = parser::parse("fn dup[T:mod copy](in x:T)->(T,T){(x,x)} fn main(){dup(0);}");
        assert!(generic.diagnostics.is_empty());
        assert!(scalar_source(&generic.module).is_err());
    }

    #[test]
    fn vector_registers_are_recognized_on_both_hosts() {
        assert!(vector_register_line("  addps %xmm1, %xmm0"));
        assert!(vector_register_line("  add v0.4s, v1.4s, v2.4s"));
        assert!(!vector_register_line("  addq %rsi, %rax"));
    }
}
