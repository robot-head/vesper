//! Isolated, actual compiler/evaluator/JIT execution; no reference arithmetic.
#![forbid(unsafe_code)]

use super::{
    Case, DiagnosticPhase, FRAME_CAP, FailureKind, HarnessError, HarnessLimits, Intent, Kind, Node,
    Outcome, RustAnswer, WorkerRequest, WorkerResponse, corpus, protocol,
};
use std::io;
use vesper::{
    ast::Diagnostic,
    check, native, optimize, parser,
    runtime::{Runtime, Value},
};

fn invariant(detail: impl Into<String>) -> HarnessError {
    HarnessError::new(FailureKind::Invariant, detail)
}

fn diagnostic(
    phase: DiagnosticPhase,
    diagnostics: &[Diagnostic],
) -> Result<RustAnswer, HarnessError> {
    if diagnostics.is_empty()
        || diagnostics.len() > 16
        || diagnostics
            .iter()
            .any(|d| d.code.is_empty() || d.code.len() > 16 || !d.code.is_ascii())
    {
        return Err(invariant("compiler diagnostic codes exceed worker profile"));
    }
    Ok(RustAnswer::Diagnostic {
        phase,
        codes: diagnostics.iter().map(|d| d.code.clone()).collect(),
    })
}

/// The only unbound-source exception is an actual parser-range negative. No
/// parsed AST exists for these literals, so the parent compares the observed
/// parser diagnostic with the independently obtained Lean verdict.
fn parser_range_negative(case: &Case) -> bool {
    case.intent == Intent::StaticNegative
        && case
            .program
            .locals
            .iter()
            .chain(std::iter::once(&case.program.result))
            .flatten()
            .any(|n| matches!(n, Node::Int(n) if n.unsigned_abs() > u128::from(u64::MAX)))
}

fn decode(value: Value, kind: Kind) -> Result<Outcome, HarnessError> {
    let Value::Tuple(parts) = value else {
        return Err(invariant("backend result is not a tagged tuple"));
    };
    let [Value::Bool(overflow), value] = parts.as_slice() else {
        return Err(invariant("backend result must be exactly (bool, K)"));
    };
    let n = match (kind, value) {
        (Kind::U8, Value::UInt(n, 8))
        | (Kind::U32, Value::UInt(n, 32))
        | (Kind::U64, Value::UInt(n, 64)) => i128::from(*n),
        (Kind::I64, Value::Int(n)) => i128::from(*n),
        _ => return Err(invariant("backend tagged payload has the wrong value kind")),
    };
    if !kind.fits(n) {
        return Err(invariant(
            "backend tagged payload is outside its declared kind",
        ));
    }
    if *overflow {
        if n != 0 {
            return Err(invariant("overflow result must carry same-kind zero"));
        }
        Ok(Outcome::Overflow)
    } else {
        Ok(Outcome::Value(n))
    }
}

fn execute_mode(
    world: &check::World,
    kind: Kind,
    evaluator_fuel: u64,
    ir_optimized: bool,
) -> Result<(Outcome, Outcome), HarnessError> {
    let mut runtime = Runtime::new(world);
    runtime.optimize = ir_optimized;
    runtime.fuel = evaluator_fuel;
    let evaluated = runtime.run("main", Vec::new()).map_err(|e| {
        HarnessError::new(
            if e == "execution fuel exhausted" {
                FailureKind::Fuel
            } else {
                FailureKind::Invariant
            },
            e,
        )
    })?;
    let evaluated = decode(evaluated, kind)?;
    // Each invocation genuinely compiles, finalizes and executes a fresh JIT.
    // The worker process lifetime bounds retained executable allocations.
    let native = native::jit_with_options(world, "main", ir_optimized)
        .map_err(|e| HarnessError::new(FailureKind::Invariant, e))?;
    Ok((evaluated, decode(native, kind)?))
}

/// Execute the actual parsed, source-bound program in all eight settings.
/// Neither this API nor the worker protocol receives an expected Lean answer.
pub fn execute_case(case: &Case, evaluator_fuel: u64) -> Result<RustAnswer, HarnessError> {
    if evaluator_fuel == 0 || evaluator_fuel > HarnessLimits::HARD.evaluator_fuel {
        return Err(HarnessError::new(
            FailureKind::InvalidConfig,
            "worker evaluator fuel is outside the finite profile",
        ));
    }
    case.program.validate()?;
    let source = corpus::render_case(case)?;
    if source.len() > HarnessLimits::HARD.source_bytes {
        return Err(HarnessError::new(
            FailureKind::UnsupportedProfile,
            "rendered source exceeds the 16 KiB compiler input cap",
        ));
    }
    let parsed = parser::parse(&source);
    if !parsed.diagnostics.is_empty() {
        if !parser_range_negative(case)
            || parsed.diagnostics.len() != 1
            || parsed.diagnostics[0].code != "E0002"
        {
            return Err(invariant("unexpected parser rejection of rendered source"));
        }
        return diagnostic(DiagnosticPhase::Parse, &parsed.diagnostics);
    }
    corpus::validate_rendered(case, &source, &parsed.module)?;
    let original = check::check(&parsed.module);
    if !original.diagnostics.is_empty() {
        return diagnostic(DiagnosticPhase::Check, &original.diagnostics);
    }
    let mut optimized_module = parsed.module.clone();
    optimize::optimize(&mut optimized_module, &original.expression_types);
    let optimized = check::check(&optimized_module);
    if !optimized.diagnostics.is_empty() {
        return Err(invariant("AST-optimized source failed the real checker"));
    }
    let kind = case.program.kind;
    let (e0, j0) = execute_mode(&original.world, kind, evaluator_fuel, false)?;
    let (e1, j1) = execute_mode(&original.world, kind, evaluator_fuel, true)?;
    let (e2, j2) = execute_mode(&optimized.world, kind, evaluator_fuel, false)?;
    let (e3, j3) = execute_mode(&optimized.world, kind, evaluator_fuel, true)?;
    Ok(RustAnswer::Outcomes {
        evaluator: Box::new([e0, e1, e2, e3]),
        jit: Box::new([j0, j1, j2, j3]),
    })
}

/// Bounded private IPC. EOF alone retires a worker normally. A 129th frame is
/// refused, including when earlier frames were static-negative programs.
pub fn worker_main() -> Result<(), HarnessError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut handled = 0u64;
    while let Some(bytes) = protocol::read_frame(&mut input, FRAME_CAP)? {
        if handled >= u64::from(HarnessLimits::HARD.worker_cases) {
            return Err(HarnessError::new(
                FailureKind::Protocol,
                "worker hard case retirement limit",
            ));
        }
        let request: WorkerRequest = serde_json::from_slice(&bytes).map_err(|_| {
            HarnessError::new(FailureKind::Protocol, "invalid private worker request")
        })?;
        if request.worker_seq != handled {
            return Err(HarnessError::new(
                FailureKind::Protocol,
                "private worker sequence mismatch",
            ));
        }
        // execute_case performs all structural/fuel admission before compiler
        // entry; unsupported data produce bounded errors, never backend answers.
        let response = WorkerResponse {
            worker_seq: request.worker_seq,
            case_id: request.case.id,
            answer: execute_case(&request.case, request.evaluator_fuel),
        };
        let bytes = serde_json::to_vec(&response).map_err(|_| {
            HarnessError::new(
                FailureKind::Protocol,
                "cannot serialize private worker reply",
            )
        })?;
        protocol::write_frame(&mut output, &bytes, FRAME_CAP)?;
        handled = handled.checked_add(1).ok_or_else(|| {
            HarnessError::new(FailureKind::Protocol, "private worker sequence exhausted")
        })?;
    }
    Ok(())
}
