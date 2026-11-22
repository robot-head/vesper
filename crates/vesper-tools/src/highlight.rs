//! `vesper highlight` (design §43): Chroma semantic highlighting.
//!
//! The library crate `vesper-highlight` never depends on this package, so
//! Tier 1 facts are gathered here, read-only, from the parser's AST and the
//! checker's existing outputs (`World::types`, `World::mode`,
//! `World::effects`, `World::functions`) and handed over as a
//! [`SemanticFacts`] DTO. A snippet that does not parse or check is
//! highlighted at Tier 0 with a typed [`FallbackReason`].
use std::fs;

mod facts;
use vesper::{
    ast::{
        Expr, ExprKind, Function, Item, Mode as ValueMode, Module, Pass, Pattern, PatternKind,
        Statement, Type, substitute,
    },
    check::World,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, CheckedSource, check_source},
};
use vesper_highlight::{
    ChromaPalette, FallbackReason, Highlight, SemanticFacts, ansi, css, highlight_tier1, html,
    json, tier0_fallback,
};

const HELP: &str = "Usage: vesper highlight [--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1] [--no-line-numbers] [-o PATH] FILE\n       vesper highlight --css [-o PATH]\n\nChroma semantic highlighting (design §43). Tier 0 classifies the lossless CST of any snippet,\ncomplete or not. Tier 1 (the default) also marks live linear bindings, mutated versus read-only\nFieldTree leaves and linear versus abortable effects when the snippet checks; otherwise it falls\nback to Tier 0 and reports why (in --json, and on stderr).\n\n  --html   Semantic HTML styled by vesper-chroma.css custom properties (copy-exact text)\n  --ansi   24-bit terminal colours (default)\n  --json   The 16-byte ChromaTokenSpanWire records (schema vesper.chroma.spans/1)\n  --css    The vesper-chroma.css sheet with both themes\n";

/// Largest input file (the highlighter's default byte budget).
const MAX_BYTES: usize = vesper_highlight::DEFAULT_MAX_SOURCE_BYTES;
/// Tier 1 analysis budget: the guarded front end's hard maxima.
const ANALYSIS_LIMITS: AnalysisLimits = AnalysisLimits {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Html,
    Ansi,
    Json,
    Css,
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut format = None;
    let mut theme = "dusk";
    let mut tier = 1u8;
    let mut line_numbers = true;
    let mut output = None;
    let mut input = None;
    let mut it = args.into_iter();
    let set = |f: &mut Option<Format>, v: Format| {
        if f.replace(v).is_some_and(|old| old != v) {
            Err("choose one of --html, --ansi, --json, --css".to_string())
        } else {
            Ok(())
        }
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--html" => set(&mut format, Format::Html)?,
            "--ansi" => set(&mut format, Format::Ansi)?,
            "--json" => set(&mut format, Format::Json)?,
            "--css" => set(&mut format, Format::Css)?,
            "--no-line-numbers" => line_numbers = false,
            "--theme" => {
                theme = match it.next().as_deref() {
                    Some("dusk" | "vesper-dusk") => "dusk",
                    Some("dawn" | "vesper-dawn") => "dawn",
                    _ => return Err("--theme takes dusk or dawn".into()),
                }
            }
            "--tier" => {
                tier = match it.next().as_deref() {
                    Some("0") => 0,
                    Some("1") => 1,
                    _ => return Err("--tier takes 0 or 1".into()),
                }
            }
            "-o" => output = Some(it.next().ok_or("-o takes a path")?),
            a if a.starts_with('-') => return Err(format!("unknown highlight option {a}")),
            _ if input.is_some() => return Err("highlight takes one FILE".into()),
            _ => input = Some(arg),
        }
    }
    let format = format.unwrap_or(Format::Ansi);
    if format == Format::Css {
        if input.is_some() {
            return Err("--css takes no FILE".into());
        }
        return write(output, css::stylesheet().as_bytes());
    }
    let input = input.ok_or("missing source file; see vesper highlight --help")?;
    let meta = fs::metadata(&input).map_err(|e| format!("{input}: {e}"))?;
    if meta.len() > MAX_BYTES as u64 {
        return Err(format!(
            "{input}: highlight input exceeds {MAX_BYTES} bytes"
        ));
    }
    let bytes = fs::read(&input).map_err(|e| format!("{input}: {e}"))?;
    let source = String::from_utf8(bytes).map_err(|_| format!("{input}: not UTF-8"))?;
    let highlight = highlight_source(&source, tier).map_err(|e| format!("{input}: {e}"))?;
    if let Some(reason) = &highlight.fallback
        && tier == 1
    {
        eprintln!(
            "vesper highlight: Tier 1 unavailable ({}), using Tier 0",
            describe(reason)
        );
    }
    let palette = ChromaPalette::by_name(theme).ok_or("unknown theme")?;
    let text = match format {
        Format::Html => html::render(
            &source,
            &highlight,
            &html::HtmlOptions {
                theme: Some(theme),
                line_numbers,
            },
        ),
        Format::Ansi => ansi::render(&source, &highlight, &palette),
        Format::Json => {
            json::render(&highlight, palette.name.as_str()).map_err(|e| e.to_string())?
        }
        Format::Css => unreachable!("handled above"),
    };
    write(output, text.as_bytes())
}

fn describe(reason: &FallbackReason) -> String {
    match reason {
        FallbackReason::Syntax { code, message, .. } => format!("syntax error {code}: {message}"),
        FallbackReason::Check { code, message, .. } => format!("check error {code}: {message}"),
        FallbackReason::Budget { message } => format!("analysis budget exceeded: {message}"),
        FallbackReason::NotRequested => "not requested".into(),
    }
}

fn write(output: Option<String>, bytes: &[u8]) -> Result<(), String> {
    match output {
        Some(path) => fs::write(&path, bytes).map_err(|e| format!("{path}: {e}")),
        None => {
            use std::io::Write;
            std::io::stdout()
                .write_all(bytes)
                .map_err(|e| e.to_string())
        }
    }
}

/// Tier 1 when requested and the snippet checks, else Tier 0 with a reason.
pub fn highlight_source(
    source: &str,
    tier: u8,
) -> Result<Highlight, vesper_highlight::ChromaError> {
    if tier == 0 {
        return tier0_fallback(source, FallbackReason::NotRequested);
    }
    match facts(source) {
        Ok(facts) => highlight_tier1(source, &facts),
        Err(reason) => tier0_fallback(source, reason),
    }
}

/// Runs the parser and checker and extracts the Tier 1 facts.
pub fn facts(source: &str) -> Result<SemanticFacts, FallbackReason> {
    if source.len() > MAX_BYTES {
        return Err(FallbackReason::Syntax {
            code: "E0003".into(),
            message: "source exceeds the highlighter budget".into(),
            span: 0..0,
        });
    }
    // The guarded front end bounds AST size, nesting and type growth, so a
    // small input such as repeated `let x = (x, x);` cannot exhaust memory.
    let checked = match check_source(source, ANALYSIS_LIMITS) {
        Ok(c) => c,
        Err(AnalysisFailure::Parse { diagnostics }) => {
            return Err(diagnostics.first().map_or_else(
                || FallbackReason::Budget {
                    message: "parse failed".into(),
                },
                |d| FallbackReason::Syntax {
                    code: d.code.as_str().into(),
                    message: d.message.as_str().into(),
                    span: d.span.clone(),
                },
            ));
        }
        Err(AnalysisFailure::Semantic { diagnostics }) => {
            return Err(diagnostics.first().map_or_else(
                || FallbackReason::Budget {
                    message: "check failed".into(),
                },
                |d| FallbackReason::Check {
                    code: d.code.as_str().into(),
                    message: d.message.as_str().into(),
                    span: d.span.clone(),
                },
            ));
        }
        Err(AnalysisFailure::Resource { reason } | AnalysisFailure::Unsupported { reason }) => {
            return Err(FallbackReason::Budget {
                message: reason.as_str().into(),
            });
        }
    };
    Ok(facts::collect_facts(source, &checked))
}
