//! `vesper tool-facts <kind> --source <file> --out <path> [options]`: what the tool binaries in
//! crates/vesper-tools ask the compiler, over a process boundary (#2670, part
//! of #1397).
//!
//! The tools used to link this library for checking, interfaces and runs.
//! Now each one names its source with `--source` and reads one JSON document
//! back from `--out`, so the tools no longer hold the Rust compiler library
//! and the compiler that answers is whichever `vesper` ships (Stage 2
//! included). The standard streams stay with the program being run (`top`
//! runs the module, which may read and print), which is why the facts go to a
//! file.
//!
//! The command is not in `--help`: its only callers are the tools.
//!
//! Kinds:
//! - `publish --package <name>`: the parse and check error counts, every
//!   function's effect instances, and the interface JSON the release records.
//! - `top --entry <fn> --fuel <steps>`: run the module with the linear
//!   resource trace and report the effect stack and live obligations.
//! - `bench --entry <fn> [--steps N] [--asm]`: the checked scalar kernel as
//!   machine code and relocations for `vesper bench` to load, with the
//!   evaluator's step count and the CLIF and machine listing when asked for
//!   (src/tool_facts_bench.rs).
//! - `fuzz-serve --source <file> --fuel <steps>`: not one answer but a
//!   request loop over the standard streams for a whole `vesper fuzz` or
//!   `vesper verify --refute` campaign (src/tool_facts_fuzz.rs).
//! - `chroma`: Chroma's Tier 1 semantic facts (`vesper highlight`), or why
//!   the snippet falls back to Tier 0.
//! - `shot [--doctest <file>] [--span <start>..<end> ...]`: what
//!   `vesper shot` needs, in one call: with `--doctest`, the sandbox verdict
//!   of the generated doc comment (`--verify`); then the `chroma` facts and
//!   the checker's answer for each `//^?` callout span, in order (`null`
//!   when it has none).
mod chroma;

use std::{io::Read, ops::Range, path::PathBuf, time::Instant};

use serde_json::{Value, json};
use vesper::{
    ast::Mode,
    check,
    doctest::{self, DocTestLimits},
    parser,
    runtime::{
        Runtime,
        resource_trace::{ResourceTraceConfig, ResourceTraceLimits},
    },
    tooling::frontend::{AnalysisFailure, AnalysisLimits, check_source},
};

/// The command name (hidden from `--help`).
pub const COMMAND: &str = "tool-facts";

/// The schema every facts document carries.
pub const SCHEMA: &str = "vesper.tool-facts/1";

/// The kind `vesper publish` asks for.
const PUBLISH: &str = "publish";
/// The kind `vesper top SOURCE.ves` asks for.
const TOP: &str = "top";
/// The kind `vesper highlight` asks for.
const CHROMA: &str = "chroma";
/// The kind `vesper shot` asks for: doctest, chroma and callouts in one call.
const SHOT: &str = "shot";

/// Tier 1 analysis budget: the guarded front end's hard maxima.
const CHROMA_LIMITS: AnalysisLimits = AnalysisLimits {
    max_source_bytes: 262_144,
    max_ast_nodes: 8192,
    max_type_nodes: 32768,
    max_depth: 64,
    max_identifier_bytes: 1024,
    max_printed_type_bytes: 8192,
    max_diagnostics: 256,
    max_diagnostic_bytes: 262_144,
    max_metadata_bytes: 2_097_152,
};

/// The longest hover summary of a diagnostic, in chars (the renderer has its
/// own bound too).
const MAX_HOVER_DIAGNOSTIC_CHARS: usize = 600;

/// The largest source read: the larger of the tools' own bounds (`vesper top`
/// reads up to 4 MiB; `vesper publish` less), which they check first.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut it = args.into_iter();
    let kind = it.next().ok_or("tool-facts requires a kind")?;
    // One process for a whole campaign, answering over its standard streams.
    if kind == crate::tool_facts_fuzz::KIND {
        return crate::tool_facts_fuzz::serve(it.collect());
    }
    let mut out = None;
    let mut source_path = None;
    let mut package = None;
    let mut entry = None;
    let mut fuel = None;
    let mut steps = None;
    let mut asm = false;
    let mut spans = vec![];
    let mut doctest_path = None;
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} requires a value"));
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--source" => source_path = Some(PathBuf::from(value()?)),
            "--package" => package = Some(value()?),
            "--entry" => entry = Some(value()?),
            "--fuel" => {
                fuel = Some(
                    value()?
                        .parse::<u64>()
                        .map_err(|e| format!("--fuel: {e}"))?,
                )
            }
            "--steps" => {
                steps = Some(
                    value()?
                        .parse::<usize>()
                        .map_err(|e| format!("--steps: {e}"))?,
                )
            }
            "--asm" => asm = true,
            "--doctest" => doctest_path = Some(PathBuf::from(value()?)),
            "--span" => {
                let text = value()?;
                let (start, end) = text
                    .split_once("..")
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                    .ok_or_else(|| format!("--span takes <start>..<end>, not '{text}'"))?;
                spans.push(start..end);
            }
            _ => return Err(format!("unknown tool-facts option '{arg}'")),
        }
    }
    let out = out.ok_or("tool-facts requires --out <path>")?;
    let source_path = source_path.ok_or("tool-facts requires --source <path>")?;
    let source = read_source(&source_path)?;
    let facts = match kind.as_str() {
        PUBLISH => publish(&source, &package.ok_or("publish facts require --package")?),
        TOP => top(
            &source,
            &entry.ok_or("top facts require --entry")?,
            fuel.ok_or("top facts require --fuel")?,
        ),
        crate::tool_facts_bench::KIND => crate::tool_facts_bench::bench(
            &source,
            &entry.ok_or("bench facts require --entry")?,
            steps,
            asm,
        ),
        CHROMA => Ok(chroma_facts(&source)),
        SHOT => shot(&source, doctest_path.as_deref(), &spans),
        _ => return Err(format!("unknown tool-facts kind '{kind}'")),
    };
    let mut document = json!({ "schema": SCHEMA, "kind": kind });
    let fields = document.as_object_mut().expect("an object");
    match facts {
        Ok(Value::Object(map)) => fields.extend(map),
        Ok(other) => {
            fields.insert("facts".into(), other);
        }
        Err(error) => {
            fields.insert("error".into(), Value::String(error));
        }
    }
    let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
    std::fs::write(&out, bytes).map_err(|e| format!("{}: {e}", out.display()))
}

/// A source file, bounded by [`MAX_SOURCE_BYTES`].
fn read_source(path: &std::path::Path) -> Result<String, String> {
    let mut source = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_SOURCE_BYTES + 1).read_to_string(&mut source))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if source.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "{}: source exceeds {MAX_SOURCE_BYTES} bytes",
            path.display()
        ));
    }
    Ok(source)
}

/// The capability text of one effect instance (`Name` or `Name[args]`), as
/// the package's `capability_ceiling` spells it.
fn capability(effect: &vesper::ast::Effect) -> String {
    if effect.args.is_empty() {
        effect.name.clone()
    } else {
        let args = effect
            .args
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}[{args}]", effect.name)
    }
}

/// The parse error count when the source does not parse.
fn syntax_failure(count: usize) -> Value {
    json!({ "syntax_errors": count })
}

/// The check error count when the source does not check.
fn semantic_failure(count: usize) -> Value {
    json!({ "semantic_errors": count })
}

fn publish(source: &str, package: &str) -> Result<Value, String> {
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Ok(syntax_failure(parsed.diagnostics.len()));
    }
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Ok(semantic_failure(checked.diagnostics.len()));
    }
    let effects: Vec<Value> = checked
        .world
        .functions
        .iter()
        .flat_map(|(function, f)| {
            f.effects.instances.iter().map(move |effect| {
                json!({
                    "function": function,
                    "name": effect.name,
                    "capability": capability(effect),
                })
            })
        })
        .collect();
    // The interface's exact bytes: the release hashes them.
    let interface = vesper::interface::export(&checked.world, package)
        .and_then(|interface| serde_json::to_string_pretty(&interface).map_err(|e| e.to_string()));
    Ok(match interface {
        Ok(text) => json!({ "effects": effects, "interface": text }),
        Err(error) => json!({ "effects": effects, "interface_error": error }),
    })
}

fn top(source: &str, entry: &str, fuel: u64) -> Result<Value, String> {
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Ok(syntax_failure(parsed.diagnostics.len()));
    }
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Ok(semantic_failure(checked.diagnostics.len()));
    }
    let f = checked
        .world
        .functions
        .get(entry)
        .ok_or_else(|| format!("missing entry function '{entry}'"))?;
    if !f.params.is_empty() {
        return Err("entry function must take no arguments".into());
    }
    if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
        return Err("entry function must handle all effects".into());
    }
    let linear_types: Vec<String> = checked
        .world
        .adts
        .values()
        .filter(|adt| {
            adt.mode == Mode::Linear
                && !adt.is_enum
                && adt.generics.is_empty()
                && adt.variants.len() == 1
                && adt.variants[0].name == adt.name
                && adt.variants[0].fields.is_empty()
        })
        .map(|adt| adt.name.clone())
        .take(ResourceTraceLimits::HARD.max_selected_types)
        .collect();
    let started = Instant::now();
    let mut runtime = Runtime::new(&checked.world);
    runtime.fuel = fuel;
    let observer = if linear_types.is_empty() {
        None
    } else {
        let mut limits = ResourceTraceLimits::HARD;
        limits.max_selected_types = linear_types.len();
        let total_name_bytes: usize = linear_types.iter().map(String::len).sum();
        if total_name_bytes <= ResourceTraceLimits::HARD.max_selected_name_bytes {
            limits.max_selected_name_bytes = total_name_bytes.max(1);
        }
        let selected_refs: Vec<&str> = linear_types.iter().map(String::as_str).collect();
        let config = ResourceTraceConfig::new(&checked.world, &selected_refs, limits)
            .map_err(|e| e.to_string())?
            .with_best_effort(true);
        Some(
            runtime
                .observe_resources(config)
                .map_err(|e| e.to_string())?,
        )
    };
    let _val = runtime.run(entry, vec![])?;
    let elapsed_ns = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    let effect_stack = runtime.active_effects();
    let mut obligations = Vec::new();
    let mut observer_truncated = false;
    if let Some(observer) = observer {
        let report = observer.snapshot().map_err(|e| e.to_string())?;
        observer_truncated = report.truncated();
        let selected = report.selected_types();
        for live in report.live() {
            obligations.push(json!({
                "serial": live.identity().serial(),
                "kind": selected.get(live.kind()).cloned(),
            }));
        }
    }
    Ok(json!({
        "elapsed_ns": elapsed_ns,
        "effect_stack": effect_stack,
        "observer_truncated": observer_truncated,
        "obligations": obligations,
    }))
}

/// The `kind` of a fallback for a snippet that does not parse.
const FALLBACK_SYNTAX: &str = "syntax";
/// The `kind` of a fallback for a snippet that does not check.
const FALLBACK_CHECK: &str = "check";
/// The `kind` of a fallback for an analysis past its budget.
const FALLBACK_BUDGET: &str = "budget";

/// Why Tier 1 is unavailable, as `vesper_highlight::FallbackReason` spells
/// it: `kind` is one of the `FALLBACK_*` tags.
fn fallback(kind: &str, code: &str, message: &str, span: Option<Range<usize>>) -> Value {
    json!({ "fallback": { "kind": kind, "code": code, "message": message, "span": span } })
}

/// The `budget` fallback.
fn budget(message: &str) -> Value {
    fallback(FALLBACK_BUDGET, "", message, None)
}

fn chroma_facts(source: &str) -> Value {
    // The guarded front end bounds AST size, nesting and type growth, so a
    // small input such as repeated `let x = (x, x);` cannot exhaust memory.
    match check_source(source, CHROMA_LIMITS) {
        Ok(checked) => json!({ "facts": chroma::collect_facts(source, &checked) }),
        Err(AnalysisFailure::Parse { diagnostics }) => diagnostics.first().map_or_else(
            || budget("parse failed"),
            |d| {
                fallback(
                    FALLBACK_SYNTAX,
                    d.code.as_str(),
                    d.message.as_str(),
                    Some(d.span.clone()),
                )
            },
        ),
        Err(AnalysisFailure::Semantic { diagnostics }) => diagnostics.first().map_or_else(
            || budget("check failed"),
            |d| {
                fallback(
                    FALLBACK_CHECK,
                    d.code.as_str(),
                    d.message.as_str(),
                    Some(d.span.clone()),
                )
            },
        ),
        Err(AnalysisFailure::Resource { reason } | AnalysisFailure::Unsupported { reason }) => {
            budget(reason.as_str())
        }
    }
}

/// The checker's answer for each callout span: the type and type mode of the
/// expression or parameter there (the facts Lens hover shows), or the one
/// diagnostic that starts on it when the snippet does not check.
fn hover(source: &str, spans: &[Range<usize>]) -> Value {
    let analysis = check_source(source, Default::default());
    let index = vesper_span::LineIndex::new(source.into(), Default::default()).ok();
    let answer = |span: &Range<usize>| -> Option<String> {
        let (start, end) = (span.start, span.end);
        let checked = match &analysis {
            Ok(checked) => checked,
            Err(AnalysisFailure::Semantic { diagnostics }) => {
                // The primary span must start on this token; merely enclosing
                // it (for example, a whole-block diagnostic) is not evidence
                // that the diagnostic belongs to this query. Equal-sized
                // matches remain ambiguous, so do not guess.
                let matches = diagnostics.iter().filter(|d| {
                    start <= d.span.start && d.span.start < end && d.span.start < d.span.end
                });
                let minimum = matches.clone().map(|d| d.span.len()).min()?;
                let mut most_specific = matches.filter(|d| d.span.len() == minimum);
                let diagnostic = most_specific.next()?;
                if most_specific.next().is_some() {
                    return None;
                }
                let mut text = format!("{}: {}", diagnostic.code, diagnostic.message);
                for label in &diagnostic.related {
                    text.push_str("; related: ");
                    text.push_str(&label.message);
                }
                // Normalize control characters and bound the summary without
                // splitting UTF-8.
                return Some(
                    text.chars()
                        .take(MAX_HOVER_DIAGNOSTIC_CHARS)
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect(),
                );
            }
            Err(_) => return None,
        };
        let index = index.as_ref()?;
        let at = index.position(start).ok()?;
        let to = index.position(end).ok()?;
        let value =
            vesper::lsp::navigation::hover(checked, index, at, Default::default()).ok()??;
        // Only a fact about exactly this token: hover falls back to the
        // smallest enclosing expression, which would describe something else.
        let range = value.get("range")?;
        let pos = |v: &Value| Some((v.get("line")?.as_u64()?, v.get("character")?.as_u64()?));
        if pos(range.get("start")?)? != (u64::from(at.line), u64::from(at.character))
            || pos(range.get("end")?)? != (u64::from(to.line), u64::from(to.character))
        {
            return None;
        }
        let text = value.pointer("/contents/value")?.as_str()?;
        let ty = text.lines().find_map(|l| l.strip_prefix("type: "))?;
        let mode = text
            .lines()
            .find_map(|l| l.strip_prefix("type mode: "))
            .and_then(|m| m.split_whitespace().next());
        Some(match mode {
            Some(m) => format!("{ty} : mod {m}"),
            None => ty.to_string(),
        })
    };
    json!({ "answers": spans.iter().map(answer).collect::<Vec<_>>() })
}

/// Run the doc comment `source` in the doctest sandbox. It passes when its
/// one example passes; otherwise the reasons are the base diagnostics, then
/// each failed example's message and diagnostics.
fn run_doctest(source: &str) -> Result<Value, String> {
    let report = doctest::run(source, DocTestLimits::default())?;
    let passed = report.passed && report.examples.len() == 1;
    let mut reasons = Vec::new();
    for d in &report.base_diagnostics {
        reasons.push(format!("{}: {}", d.code, d.message));
    }
    for e in &report.examples {
        if !e.passed {
            reasons.push(e.message.clone());
            reasons.extend(
                e.diagnostics
                    .iter()
                    .map(|d| format!("{}: {}", d.code, d.message)),
            );
        }
    }
    Ok(json!({ "passed": passed, "reasons": reasons }))
}

/// `vesper shot`'s answers in one process: the doctest verdict first (a
/// failed or unrunnable example stops there, as `--verify` does), then the
/// Chroma facts and the callout answers.
fn shot(
    source: &str,
    doctest_path: Option<&std::path::Path>,
    spans: &[Range<usize>],
) -> Result<Value, String> {
    let mut answer = serde_json::Map::new();
    if let Some(path) = doctest_path {
        let verdict = match run_doctest(&read_source(path)?) {
            Ok(verdict) => verdict,
            Err(error) => {
                answer.insert("doctest_error".into(), Value::String(error));
                return Ok(Value::Object(answer));
            }
        };
        let passed = verdict.get("passed").and_then(Value::as_bool) == Some(true);
        answer.insert("doctest".into(), verdict);
        if !passed {
            return Ok(Value::Object(answer));
        }
    }
    answer.insert("chroma".into(), chroma_facts(source));
    answer.insert(
        "answers".into(),
        hover(source, spans)
            .get_mut("answers")
            .map(Value::take)
            .unwrap_or_default(),
    );
    Ok(Value::Object(answer))
}
