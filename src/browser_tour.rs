use crate::ast::Diagnostic;
use crate::check::World;
use crate::runtime::{Runtime, Value};
use crate::tooling::frontend::{AnalysisLimits, check_source};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, fmt::Write as _};

const MAX_SOURCE: usize = 16 * 1024;
const MAX_REQUEST: usize = 6 * MAX_SOURCE + 256;
const FUEL: u64 = 100_000;

thread_local! {
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// JavaScript writes into this allocation before calling execute. A zero
// pointer rejects an oversized request without growing Wasm memory.
#[unsafe(no_mangle)]
pub extern "C" fn vesper_tour_input(len: usize) -> *mut u8 {
    if len > MAX_REQUEST {
        return std::ptr::null_mut();
    }
    INPUT.with_borrow_mut(|input| {
        input.resize(len, 0);
        input.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_tour_execute() -> *const u8 {
    let response = INPUT.with_borrow(|input| match serde_json::from_slice::<Request>(input) {
        Ok(request) => execute(request),
        Err(_) => Response::error("Invalid Vesper tour request."),
    });
    OUTPUT.with_borrow_mut(|output| {
        *output = serde_json::to_vec(&response).expect("serializing strings and a boolean");
        output.as_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_tour_output_len() -> usize {
    OUTPUT.with_borrow(Vec::len)
}

#[derive(Deserialize)]
struct Request {
    source: String,
    mode: Mode,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Run,
    Check,
    Test,
    TestCase,
    Macro,
    Expand,
    Format,
    Highlight,
}

#[derive(Serialize)]
struct Response {
    ok: bool,
    output: String,
}

impl Response {
    fn success(output: impl Into<String>) -> Self {
        Self {
            ok: true,
            output: output.into(),
        }
    }

    fn error(output: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: output.into(),
        }
    }
}

fn diagnostics(source: &str, errors: &[Diagnostic], fallback: impl ToString) -> Response {
    if errors.is_empty() {
        return Response::error(fallback.to_string());
    }
    let mut output = String::new();
    for error in errors {
        let (line, column) = crate::test_discovery::location(source, error.span.start);
        let _ = writeln!(output, "{line}:{column}  {}  {}", error.code, error.message);
    }
    Response::error(output)
}

fn limits() -> AnalysisLimits {
    AnalysisLimits {
        max_source_bytes: MAX_SOURCE,
        ..AnalysisLimits::default()
    }
}

fn execute(Request { source, mode }: Request) -> Response {
    if source.len() > MAX_SOURCE {
        return Response::error("This example exceeds the 16 KiB source limit.");
    }
    match mode {
        Mode::Run | Mode::Check => match check_source(&source, limits()) {
            Ok(_) if matches!(mode, Mode::Check) => {
                Response::success("Source checks successfully.")
            }
            Ok(checked) => evaluate_entry(checked.world()),
            Err(error) => diagnostics(&source, error.diagnostics(), &error),
        },
        Mode::Macro | Mode::Expand => {
            use crate::forge_frontend::{CompileConfig, CompileLimits, expand_source};
            match expand_source(&source, &CompileConfig::default(), CompileLimits::default()) {
                Ok(expanded) if matches!(mode, Mode::Expand) => {
                    Response::success(expanded.expanded_source())
                }
                Ok(expanded) => evaluate_entry(expanded.checked().world()),
                Err(error) => diagnostics(&source, error.diagnostics(), &error),
            }
        }
        Mode::Highlight => {
            use vesper_highlight::{FallbackReason, html, tier0_fallback};
            match tier0_fallback(&source, FallbackReason::NotRequested) {
                Ok(highlight) => Response::success(html::render(
                    &source,
                    &highlight,
                    &html::HtmlOptions {
                        theme: None,
                        line_numbers: false,
                    },
                )),
                Err(error) => Response::error(error.to_string()),
            }
        }
        Mode::Format => match vesper_fmt::format_source(&source, vesper_fmt::Limits::default()) {
            Ok(result) => Response::success(result.output),
            Err(error) => Response::error(error.to_string()),
        },
        Mode::Test | Mode::TestCase => tests(&source, matches!(mode, Mode::TestCase)),
    }
}

fn evaluate_entry(world: &World) -> Response {
    let Some(entry) = world.functions.get("main") else {
        return Response::error("Missing entry function 'main'.");
    };
    if !entry.params.is_empty() {
        return Response::error("The entry function must take no arguments.");
    }
    if !entry.effects.instances.is_empty() || entry.effects.tail.is_some() {
        return Response::error("The entry function must handle all effects.");
    }
    let mut runtime = Runtime::new(world);
    runtime.fuel = FUEL;
    match runtime.run("main", Vec::new()) {
        Ok(value) => Response::success(value.to_string()),
        Err(error) => Response::error(error),
    }
}

fn tests(source: &str, one_case: bool) -> Response {
    use crate::test_discovery::{case_rows, discover, finish_case_wrappers, location};
    use vesper_test::TestMode;

    if let Err(error) = vesper_doc::source_preflight(source, vesper_doc::Limits::default()) {
        return Response::error(error);
    }
    let parsed = crate::parser::parse_for_tests(source);
    if !parsed.diagnostics.is_empty() {
        return diagnostics(source, &parsed.diagnostics, "Invalid test source.");
    }
    let cases = match case_rows(source, &parsed.module) {
        Ok(cases) => cases,
        Err(error) => return Response::error(error),
    };
    let expanded = cases.expanded.as_deref().unwrap_or(source);
    if let Err(error) = vesper_doc::source_preflight(expanded, vesper_doc::Limits::default()) {
        return Response::error(error);
    }
    let parsed = if cases.expanded.is_some() {
        let mut lowered = crate::parser::parse_for_tests(expanded);
        if !lowered.diagnostics.is_empty() {
            return diagnostics(expanded, &lowered.diagnostics, "Invalid test source.");
        }
        if let Err(error) =
            finish_case_wrappers(source, &parsed.module, &mut lowered.module, &cases.rows)
        {
            return Response::error(error);
        }
        lowered
    } else {
        parsed
    };
    let checked = crate::check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return diagnostics(expanded, &checked.diagnostics, "Invalid test source.");
    }
    let tests = match discover(source, &parsed.module.items, &cases.rows) {
        Ok(tests) => tests,
        Err(error) => return Response::error(error),
    };
    let mut runtime = Runtime::new(&checked.world);
    let (mut passed, mut failed) = (0, 0);
    let mut output = String::new();
    for test in tests {
        if test.plan.ignore || (one_case && test.name != "test_reserve::over") {
            continue;
        }
        let entry = match &parsed.module.items[test.index] {
            crate::ast::Item::Function(function) => &function.name,
            _ => return Response::error("Test discovery returned an invalid function."),
        };
        let result = if test.plan.primary_mode == TestMode::Unit {
            runtime.fuel = test.plan.fuel_limit.unwrap_or(FUEL).min(FUEL);
            match runtime.run(entry, Vec::new()) {
                Ok(Value::Bool(false)) => Err("assertion failed: test returned false".into()),
                Ok(
                    ref value @ Value::Data {
                        ref constructor, ..
                    },
                ) if constructor == "Err" => Err(format!("test returned {value}")),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        } else {
            Err(format!(
                "The tour runner supports unit tests and table rows. This test requires {}.",
                test.plan.primary_mode
            ))
        };
        let (line, column) = location(source, test.span.start);
        match result {
            Ok(()) => {
                passed += 1;
                let _ = writeln!(output, "{line}:{column}: PASS {} (ok)", test.name);
            }
            Err(error) => {
                failed += 1;
                let _ = writeln!(output, "{line}:{column}: FAIL {} ({error})", test.name);
            }
        }
    }
    if passed + failed == 0 {
        return Response::error("No matching tests.");
    }
    let _ = write!(output, "tests: {passed} passed, {failed} failed");
    Response {
        ok: failed == 0,
        output,
    }
}
