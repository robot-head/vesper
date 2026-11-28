//! `vesper highlight` (design §43): Chroma semantic highlighting.
//!
//! The library crate `vesper-highlight` never depends on the compiler, so
//! the shipped `vesper` gathers the Tier 1 facts, read-only, from the
//! parser's AST and the checker's outputs (`vesper tool-facts chroma`, #2670)
//! and they are handed over as a [`SemanticFacts`] DTO. A snippet that does
//! not parse or check is highlighted at Tier 0 with a typed
//! [`FallbackReason`].
use crate::compiler;
use serde::Deserialize;
use std::{fs, ops::Range};
use vesper_highlight::{
    ChromaPalette, EffectFact, FallbackReason, FieldLeafFact, Highlight, HoverEntry,
    LinearBindingFact, SemanticFacts, ansi, css, highlight_tier1, html, json, tier0_fallback,
};

const HELP: &str = "Usage: vesper highlight [--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1] [--no-line-numbers] [-o PATH] FILE\n       vesper highlight --css [-o PATH]\n\nChroma semantic highlighting (design §43). Tier 0 classifies the lossless CST of any snippet,\ncomplete or not. Tier 1 (the default) also marks live linear bindings, mutated versus read-only\nFieldTree leaves and linear versus abortable effects when the snippet checks; otherwise it falls\nback to Tier 0 and reports why (in --json, and on stderr).\n\n  --html   Semantic HTML styled by vesper-chroma.css custom properties (copy-exact text)\n  --ansi   24-bit terminal colours (default)\n  --json   The 16-byte ChromaTokenSpanWire records (schema vesper.chroma.spans/1)\n  --css    The vesper-chroma.css sheet with both themes\n";

/// Largest input file (the highlighter's default byte budget).
const MAX_BYTES: usize = vesper_highlight::DEFAULT_MAX_SOURCE_BYTES;
/// The `vesper tool-facts` kind with Chroma's Tier 1 facts.
const CHROMA_FACTS: &str = "chroma";

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
    tier1(source, facts(source))
}

/// Tier 1 from `facts`, else Tier 0 with the reason.
pub fn tier1(
    source: &str,
    facts: Result<SemanticFacts, FallbackReason>,
) -> Result<Highlight, vesper_highlight::ChromaError> {
    match facts {
        Ok(facts) => highlight_tier1(source, &facts),
        Err(reason) => tier0_fallback(source, reason),
    }
}

/// The reason a source past the highlighter's budget gets no Tier 1 facts.
pub fn over_budget(source: &str) -> Option<FallbackReason> {
    (source.len() > MAX_BYTES).then(|| FallbackReason::Syntax {
        code: "E0003".into(),
        message: "source exceeds the highlighter budget".into(),
        span: 0..0,
    })
}

/// Asks the shipped compiler for the Tier 1 facts.
pub fn facts(source: &str) -> Result<SemanticFacts, FallbackReason> {
    if let Some(reason) = over_budget(source) {
        return Err(reason);
    }
    let answer = compiler::facts_about(CHROMA_FACTS, source, &[]).map_err(budget)?;
    facts_from_answer(&answer)
}

/// The Tier 0 reason for a compiler answer that could not be used.
fn budget(message: String) -> FallbackReason {
    FallbackReason::Budget {
        message: message.into(),
    }
}

/// The Tier 1 facts in a `chroma` answer (`facts`), or why there are none
/// (`fallback`).
pub fn facts_from_answer(answer: &serde_json::Value) -> Result<SemanticFacts, FallbackReason> {
    if let Some(reason) = answer.get("fallback") {
        let reason: WireFallback = serde_json::from_value(reason.clone())
            .map_err(|e| budget(format!("chroma facts: {e}")))?;
        return Err(reason.into());
    }
    let facts: WireFacts = answer
        .get("facts")
        .cloned()
        .ok_or_else(|| "chroma facts: no facts".to_string())
        .and_then(|facts| serde_json::from_value(facts).map_err(|e| format!("chroma facts: {e}")))
        .map_err(budget)?;
    Ok(facts.into())
}

/// The fallback `kind` the compiler sends for a snippet that does not parse
/// (`FALLBACK_SYNTAX` in `src/tool_facts.rs`).
const FALLBACK_SYNTAX: &str = "syntax";
/// The fallback `kind` for a snippet that does not check.
const FALLBACK_CHECK: &str = "check";

/// Why Tier 1 is unavailable, as the compiler sends it.
#[derive(Deserialize)]
struct WireFallback {
    kind: String,
    code: String,
    message: String,
    span: Option<Range<usize>>,
}

impl From<WireFallback> for FallbackReason {
    fn from(w: WireFallback) -> Self {
        let span = w.span.unwrap_or(0..0);
        match w.kind.as_str() {
            FALLBACK_SYNTAX => FallbackReason::Syntax {
                code: w.code.into(),
                message: w.message.into(),
                span,
            },
            FALLBACK_CHECK => FallbackReason::Check {
                code: w.code.into(),
                message: w.message.into(),
                span,
            },
            _ => FallbackReason::Budget {
                message: w.message.into(),
            },
        }
    }
}

/// [`SemanticFacts`] as the compiler sends them (`src/tool_facts/chroma.rs`).
#[derive(Deserialize)]
struct WireFacts {
    linear_bindings: Vec<WireLinearBinding>,
    field_leaves: Vec<WireFieldLeaf>,
    effects: Vec<WireEffect>,
    hovers: Vec<WireHover>,
}

#[derive(Deserialize)]
struct WireLinearBinding {
    name: String,
    mint: Range<usize>,
    occurrences: Vec<Range<usize>>,
    discharge: Option<Range<usize>>,
    hover: Option<u32>,
}

#[derive(Deserialize)]
struct WireFieldLeaf {
    span: Range<usize>,
    mutated: bool,
}

#[derive(Deserialize)]
struct WireEffect {
    name: String,
    linear: bool,
}

#[derive(Deserialize)]
struct WireHover {
    span: Range<usize>,
    text: String,
}

impl From<WireFacts> for SemanticFacts {
    fn from(w: WireFacts) -> Self {
        SemanticFacts {
            linear_bindings: w
                .linear_bindings
                .into_iter()
                .map(|b| LinearBindingFact {
                    name: b.name.into(),
                    mint: b.mint,
                    occurrences: b.occurrences,
                    discharge: b.discharge,
                    hover: b.hover,
                })
                .collect(),
            field_leaves: w
                .field_leaves
                .into_iter()
                .map(|l| FieldLeafFact {
                    span: l.span,
                    mutated: l.mutated,
                })
                .collect(),
            effects: w
                .effects
                .into_iter()
                .map(|e| EffectFact {
                    name: e.name.into(),
                    linear: e.linear,
                })
                .collect(),
            hovers: w
                .hovers
                .into_iter()
                .map(|h| HoverEntry {
                    span: h.span,
                    text: h.text.into(),
                })
                .collect(),
        }
    }
}
