//! Isolated, actual compiler/evaluator/JIT execution; no reference arithmetic.
//!
//! The system under test is the `vesper` command line (`compiler.rs`): the
//! worker renders the case, has the compiler parse and check it (`emit
//! --emit hir --no-opt --json`), binds the parsed module to the case, then
//! runs it on the evaluator (`run`) and the JIT (`jit`) in all four
//! optimizer settings.
#![forbid(unsafe_code)]

use super::compiler::{self, Compiler, Executed, Front, Scratch};
use super::{
    Case, DiagnosticPhase, FRAME_CAP, FailureKind, HarnessError, HarnessLimits, Intent, Kind, Node,
    Outcome, RustAnswer, WorkerRequest, WorkerResponse, corpus, protocol,
};
use std::io;

fn invariant(detail: impl Into<String>) -> HarnessError {
    HarnessError::new(FailureKind::Invariant, detail)
}

fn diagnostic(phase: DiagnosticPhase, codes: Vec<String>) -> Result<RustAnswer, HarnessError> {
    if codes.is_empty()
        || codes.len() > 16
        || codes
            .iter()
            .any(|c| c.is_empty() || c.len() > 16 || !c.is_ascii())
    {
        return Err(invariant("compiler diagnostic codes exceed worker profile"));
    }
    Ok(RustAnswer::Diagnostic { phase, codes })
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

#[derive(Clone, Copy)]
enum Backend {
    Evaluator,
    Native,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Evaluator => "evaluator",
            Self::Native => "native",
        }
    }
}

fn mode_error(
    error: HarnessError,
    backend: Backend,
    ast_optimized: bool,
    ir_optimized: bool,
) -> HarnessError {
    HarnessError::new(
        error.kind,
        format!(
            "{} AST optimized={ast_optimized}, IR optimized={ir_optimized}: {}",
            backend.name(),
            error.detail
        ),
    )
}

/// The CLI prints `main`'s `(bool, K)` result as `(<bool>, <integer>)`.
/// Anything else, a payload outside `K`, or an overflow tag with a nonzero
/// payload is an invariant failure, never an answer.
fn decode(printed: &str, kind: Kind) -> Result<Outcome, HarnessError> {
    let bounded: String = printed.chars().take(128).collect();
    let shape = || {
        invariant(format!(
            "backend result is not a (bool, K) tuple: {bounded}"
        ))
    };
    let inner = printed
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(shape)?;
    let (tag, value) = inner.split_once(", ").ok_or_else(shape)?;
    let overflow = match tag {
        "true" => true,
        "false" => false,
        _ => return Err(shape()),
    };
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty()
        || digits.len() > 39
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
        || value == "-0"
    {
        return Err(shape());
    }
    let n: i128 = value.parse().map_err(|_| shape())?;
    if !kind.fits(n) {
        return Err(invariant(format!(
            "backend tagged payload is outside its declared kind: {bounded}"
        )));
    }
    if overflow {
        if n != 0 {
            return Err(invariant(format!(
                "overflow result must carry same-kind zero: {bounded}"
            )));
        }
        Ok(Outcome::Overflow)
    } else {
        Ok(Outcome::Value(n))
    }
}

struct Program<'a> {
    compiler: &'a Compiler,
    scratch: &'a Scratch,
    kind: Kind,
    fuel: u64,
}

impl Program<'_> {
    fn execute(
        &self,
        backend: Backend,
        ast_optimized: bool,
        ir_optimized: bool,
    ) -> Result<Outcome, HarnessError> {
        let fail = |e| mode_error(e, backend, ast_optimized, ir_optimized);
        let executed = compiler::execute(
            self.compiler,
            &self.scratch.0,
            CASE_FILE,
            matches!(backend, Backend::Native),
            ast_optimized,
            ir_optimized,
            self.fuel,
        )
        .map_err(fail)?;
        match executed {
            Executed::Printed(text) => decode(&text, self.kind).map_err(fail),
            Executed::FuelExhausted if matches!(backend, Backend::Evaluator) => Err(fail(
                HarnessError::new(FailureKind::Fuel, "execution fuel exhausted"),
            )),
            Executed::FuelExhausted => Err(fail(invariant("JIT reported fuel exhaustion"))),
            Executed::Failed(line) => Err(fail(invariant(line))),
        }
    }

    fn mode(
        &self,
        ast_optimized: bool,
        ir_optimized: bool,
    ) -> Result<(Outcome, Outcome), HarnessError> {
        Ok((
            self.execute(Backend::Evaluator, ast_optimized, ir_optimized)?,
            // Each invocation genuinely compiles, finalizes and executes a
            // fresh JIT in its own process.
            self.execute(Backend::Native, ast_optimized, ir_optimized)?,
        ))
    }
}

const CASE_FILE: &str = "case.ves";

/// Execute the actual parsed, source-bound program in all eight settings.
/// Neither this API nor the worker protocol receives an expected Lean answer.
pub fn execute_case(
    compiler: &Compiler,
    case: &Case,
    evaluator_fuel: u64,
) -> Result<RustAnswer, HarnessError> {
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
    let scratch = Scratch::new("worker")?;
    let module = match compiler::front(compiler, &scratch.0, CASE_FILE, &source)? {
        Front::Module(module) => module,
        Front::Rejected { parse: true, codes } => {
            if !parser_range_negative(case) || codes != ["E0002"] {
                return Err(invariant("unexpected parser rejection of rendered source"));
            }
            return diagnostic(DiagnosticPhase::Parse, codes);
        }
        Front::Rejected {
            parse: false,
            codes,
        } => {
            return diagnostic(DiagnosticPhase::Check, codes);
        }
    };
    corpus::validate_rendered(case, &source, &module)?;
    let program = Program {
        compiler,
        scratch: &scratch,
        kind: case.program.kind,
        fuel: evaluator_fuel,
    };
    // An AST-optimized program that fails the checker again is reported by
    // the CLI ("optimizer validation failed") and fails the mode.
    let (e0, j0) = program.mode(false, false)?;
    let (e1, j1) = program.mode(false, true)?;
    let (e2, j2) = program.mode(true, false)?;
    let (e3, j3) = program.mode(true, true)?;
    Ok(RustAnswer::Outcomes {
        evaluator: Box::new([e0, e1, e2, e3]),
        jit: Box::new([j0, j1, j2, j3]),
    })
}

/// Bounded private IPC. EOF alone retires a worker normally. A 129th frame is
/// refused, including when earlier frames were static-negative programs.
pub fn worker_main(compiler: &Compiler) -> Result<(), HarnessError> {
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
            answer: execute_case(compiler, &request.case, request.evaluator_fuel),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rejects(printed: &str, kind: Kind) {
        assert_eq!(
            decode(printed, kind).unwrap_err().kind,
            FailureKind::Invariant,
            "{printed}"
        );
    }

    #[test]
    fn unsigned_results_never_admit_a_negative_payload() {
        for kind in [Kind::U8, Kind::U64] {
            rejects("(false, -1)", kind);
            rejects("(true, -1)", kind);
        }
    }

    #[test]
    fn payload_must_fit_the_checked_kind() {
        rejects("(false, 256)", Kind::U8);
        rejects("(false, 4294967296)", Kind::U32);
        rejects("(false, 18446744073709551616)", Kind::U64);
        rejects("(false, 9223372036854775808)", Kind::I64);
        rejects("(false, -9223372036854775809)", Kind::I64);
    }

    #[test]
    fn overflow_tag_never_hides_a_nonzero_payload() {
        for kind in [Kind::U8, Kind::U32, Kind::U64, Kind::I64] {
            rejects("(true, 1)", kind);
        }
        rejects("(true, -1)", Kind::I64);
    }

    #[test]
    fn only_the_exact_printed_tuple_is_an_answer() {
        for printed in [
            "",
            "1",
            "(false)",
            "(false, 1, 2)",
            "(0, 1)",
            "(false,1)",
            "( false, 1)",
            "(false, 01)",
            "(false, -0)",
            "(false, +1)",
            "(false, 1.0)",
            "(false, 1) ",
            "(False, 1)",
            "(false, 1u8)",
        ] {
            rejects(printed, Kind::I64);
        }
    }

    #[test]
    fn exact_values_and_overflow_decode() {
        assert_eq!(decode("(false, 0)", Kind::U8).unwrap(), Outcome::Value(0));
        assert_eq!(
            decode("(false, 255)", Kind::U8).unwrap(),
            Outcome::Value(255)
        );
        assert_eq!(decode("(true, 0)", Kind::U8).unwrap(), Outcome::Overflow);
        assert_eq!(
            decode("(false, 18446744073709551615)", Kind::U64).unwrap(),
            Outcome::Value(i128::from(u64::MAX))
        );
        assert_eq!(
            decode("(false, -9223372036854775808)", Kind::I64).unwrap(),
            Outcome::Value(i128::from(i64::MIN))
        );
    }

    #[test]
    fn optimizer_settings_map_to_distinct_flags() {
        let all: Vec<_> = [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .map(|(a, i)| compiler::optimizer_flags(a, i))
            .collect();
        assert_eq!(
            all,
            [&["--no-opt"][..], &["--no-ast-opt"], &["--no-ir-opt"], &[]]
        );
    }
}
