//! `vesper shot`: render a Vesper example as a vector SVG or a 1x-4x PNG code
//! card (§43.1 item 3, §43.2, §43.5), optionally after checking and running it
//! in the Codex doctest sandbox (§40.1), with outputs cached in the
//! content-addressed store keyed by BLAKE3(source partition, theme, config).

use crate::compiler;
use serde_json::Value;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{collections::BTreeMap, ops::Range};
use vesper_highlight::shot::{
    self, RasterOptions, SceneOptions, ShotFont, ShotLimits, ShotOutputFormat, ShotTheme,
    SvgOptions,
};
use vesper_highlight::{Highlight, HighlightSpan, highlight_tier0};
use vesper_loom_cas::{DerivationInputs, Domain, Store, blob_id};

/// The `vesper tool-facts` kind with the `--verify` verdict, the Tier 1
/// facts and the callout answers.
const SHOT_FACTS: &str = "shot";
/// What the first callout pass answers while it collects the queries.
const PLACEHOLDER_ANSWER: &str = "?";

/// The bundled card font: JetBrains Mono 2.304 Regular, SIL OFL 1.1
/// (assets/fonts/jetbrains-mono/OFL.txt).
pub const CARD_FONT: &[u8] =
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");

const HELP: &str = "Usage:
  vesper shot <file> -o <out.svg|out.png> [--theme dusk|dawn] [--dpi 1-4]
              [--title <text>] [--verify] [--embed-font]
              [--store <dir> | --no-cache] [--json]

Renders a code card. The file may start with an @shot(title = '..', theme = '..',
dpi = N) directive and may carry Twoslash-style callouts: a `//  ^?` line under
a token shows its inferred type and mode or an unambiguous checker diagnostic,
and `//^!` attaches a note. Both are read by the renderer from the text; they
are not compiled.

--verify checks and runs the example in the doctest sandbox first and refuses
to render one that fails. Outputs are cached in the content-addressed store
(default ~/.vesper/store) under BLAKE3(source partition, theme, config).
Command-line flags override the @shot directive.";

struct Args {
    input: PathBuf,
    output: PathBuf,
    theme: Option<ShotTheme>,
    dpi: Option<u32>,
    title: Option<String>,
    verify: bool,
    embed_font: bool,
    store: Option<PathBuf>,
    no_cache: bool,
    json: bool,
}

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut parsed = Args {
        input: PathBuf::new(),
        output: PathBuf::new(),
        theme: None,
        dpi: None,
        title: None,
        verify: false,
        embed_font: false,
        store: None,
        no_cache: false,
        json: false,
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(HELP.into()),
            "-o" | "--output" => {
                if output.replace(PathBuf::from(value("-o")?)).is_some() {
                    return Err("-o may only be given once".into());
                }
            }
            "--theme" => {
                let name = value("--theme")?;
                parsed.theme = Some(
                    ShotTheme::parse(&name)
                        .ok_or_else(|| format!("unknown theme `{name}` (dusk or dawn)"))?,
                );
            }
            "--dpi" => {
                let n = value("--dpi")?;
                match n.parse::<u32>() {
                    Ok(d @ 1..=4) => parsed.dpi = Some(d),
                    _ => return Err("--dpi must be 1, 2, 3 or 4".into()),
                }
            }
            "--title" => parsed.title = Some(value("--title")?),
            "--store" => parsed.store = Some(PathBuf::from(value("--store")?)),
            "--verify" => parsed.verify = true,
            "--embed-font" => parsed.embed_font = true,
            "--no-cache" => parsed.no_cache = true,
            "--json" => parsed.json = true,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag `{flag}`\n\n{HELP}"));
            }
            _ => {
                if input.replace(PathBuf::from(&arg)).is_some() {
                    return Err("shot takes exactly one input file".into());
                }
            }
        }
    }
    if parsed.no_cache && parsed.store.is_some() {
        return Err("--store and --no-cache are exclusive".into());
    }
    parsed.input = input.ok_or_else(|| format!("shot requires an input file\n\n{HELP}"))?;
    parsed.output = output.ok_or("shot requires -o <out.svg|out.png>")?;
    Ok(parsed)
}

fn default_store() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("VESPER_STORE") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(Path::new(&home).join(".vesper").join("store"))
}

/// The displayed example as one declaration fence (entry `main`) of a
/// generated doc comment, so `--verify` runs it through the same checker,
/// shared JIT module and evaluator fallback as `vesper doctest`.
fn wrap_example(display: &str) -> Result<String, String> {
    let mut wrapped = String::with_capacity(display.len() + 64);
    wrapped.push_str("//! ```vesper\n");
    for line in display.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            return Err("--verify: a code fence inside the example".into());
        }
        wrapped.push_str("//!");
        if !line.is_empty() {
            wrapped.push(' ');
            wrapped.push_str(line);
        }
        wrapped.push('\n');
    }
    wrapped.push_str("//! ```\n");
    Ok(wrapped)
}

/// `--verify`'s verdict in a `shot` answer: the example must pass.
fn verdict(answer: &Value) -> Result<(), String> {
    if let Some(error) = answer.get("doctest_error") {
        return Err(format!("--verify: {}", error.as_str().unwrap_or_default()));
    }
    let report = answer
        .get("doctest")
        .ok_or("shot facts: no doctest verdict")?;
    if report.get("passed").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let reasons: Vec<&str> = report
        .get("reasons")
        .and_then(Value::as_array)
        .map(|reasons| reasons.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Err(format!(
        "--verify: the example failed; not rendering it\n  {}",
        reasons.join("\n  ")
    ))
}

/// The spans the callouts of `doc` ask about. Tier 1 keeps Tier 0's token
/// boundaries, so Tier 0's spans locate them before any facts exist. Every
/// query is answered with a placeholder, so that one without an answer does
/// not stop the pass before the later ones are seen.
fn callout_spans(
    doc: &shot::ShotDocument,
    spans: &[HighlightSpan],
) -> Result<Vec<Range<usize>>, String> {
    let mut asked = vec![];
    shot::resolve_callouts(doc, spans, |_token, start, end| {
        asked.push(start..end);
        Some(PLACEHOLDER_ANSWER.to_owned())
    })
    .map_err(|e| e.to_string())?;
    Ok(asked)
}

/// Asks the shipped compiler, in one call, for everything the card needs
/// from the checker (#2670): the `--verify` verdict, Chroma's Tier 1 facts
/// and an answer for each callout span.
fn ask(doc: &shot::ShotDocument, verify: bool, asked: &[Range<usize>]) -> Result<Value, String> {
    let wrapped = if verify {
        Some(compiler::TempFile::source(&wrap_example(&doc.display)?)?)
    } else {
        None
    };
    let mut options: Vec<String> = vec![];
    if let Some(file) = &wrapped {
        options.push("--doctest".into());
        options.push(file.path().display().to_string());
    }
    for span in asked {
        options.push("--span".into());
        options.push(format!("{}..{}", span.start, span.end));
    }
    let options: Vec<&str> = options.iter().map(String::as_str).collect();
    let answer = compiler::facts_about(SHOT_FACTS, &doc.display, &options);
    if verify {
        // A verify failure is reported as `--verify: ...`, as before.
        let answer = answer.map_err(|e| format!("--verify: {e}"))?;
        verdict(&answer)?;
        return Ok(answer);
    }
    answer
}

/// Tier 0/Tier 1 highlighting of the displayed text (lane 1's highlighter),
/// Tier 1 when the snippet checks, else Tier 0 with lane 1's fallback reason,
/// and the callout pins. A `//^?` query is answered from Tier 1's hover
/// table first (it covers binding sites), then by the checker: the type and
/// type mode of the expression or parameter there (the same facts Lens hover
/// shows). `None` when the snippet does not check or has no type there.
fn highlight_and_pins(
    doc: &shot::ShotDocument,
    verify: bool,
) -> Result<(Highlight, Vec<shot::CalloutPin>), String> {
    // `--verify` reports first, as it ran before highlighting.
    let asked = highlight_tier0(&doc.display)
        .map_err(|e| format!("highlight: {e:?}"))
        .and_then(|tier0| callout_spans(doc, &tier0));
    let asked = match asked {
        Ok(asked) => asked,
        Err(error) => {
            if verify {
                ask(doc, true, &[])?;
            }
            return Err(error);
        }
    };
    let answer = ask(doc, verify, &asked)?;
    let facts = match crate::highlight::over_budget(&doc.display) {
        Some(reason) => Err(reason),
        None => crate::highlight::facts_from_answer(
            answer.get("chroma").ok_or("shot facts: no chroma facts")?,
        ),
    };
    let highlight =
        crate::highlight::tier1(&doc.display, facts).map_err(|e| format!("highlight: {e:?}"))?;
    let replies = answer
        .get("answers")
        .and_then(Value::as_array)
        .ok_or("shot facts: no callout answers")?;
    let answers: BTreeMap<(usize, usize), Option<String>> = asked
        .iter()
        .zip(replies)
        .map(|(span, reply)| ((span.start, span.end), reply.as_str().map(str::to_owned)))
        .collect();
    let table = &highlight.hovers;
    let pins = shot::resolve_callouts(doc, &highlight.spans, |_token, start, end| {
        table
            .iter()
            .find(|h| h.span == (start..end))
            .map(|h| h.text.to_string())
            .or_else(|| answers.get(&(start, end)).cloned().flatten())
    })
    .map_err(|e| e.to_string())?;
    Ok((highlight, pins))
}

fn derivation(key: &shot::ShotCacheKey, png: bool) -> DerivationInputs {
    DerivationInputs::new(Domain::Artifact, "vesper-shot", shot::SHOT_CACHE_SCHEMA)
        .with_option("key", key.to_hex())
        .with_option("format", if png { "png" } else { "svg" })
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let args = parse_args(args)?;
    let format_png = match args.output.extension().and_then(|e| e.to_str()) {
        Some("svg") => false,
        Some("png") => true,
        _ => return Err("the output must end in .svg or .png".into()),
    };
    let limits = ShotLimits::default();
    let meta =
        std::fs::metadata(&args.input).map_err(|e| format!("{}: {e}", args.input.display()))?;
    if meta.len() > limits.max_source_bytes as u64 {
        return Err("shot source exceeds 64 KiB".into());
    }
    let source = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("{}: {e}", args.input.display()))?;
    let doc = shot::parse_shot_source(&source, limits).map_err(|e| e.to_string())?;
    let (Highlight { spans, .. }, pins) = highlight_and_pins(&doc, args.verify)?;
    let theme = args.theme.or(doc.config.theme).unwrap_or(ShotTheme::Dusk);
    let dpi = args.dpi.or(doc.config.dpi).unwrap_or(4);
    let title = args
        .title
        .clone()
        .or_else(|| doc.config.title.clone())
        .unwrap_or_else(|| {
            args.input
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let font = ShotFont::parse(CARD_FONT).map_err(|e| e.to_string())?;
    let out_format = if format_png {
        ShotOutputFormat::Png { dpi_scale: dpi }
    } else {
        ShotOutputFormat::Svg {
            embed_font: args.embed_font,
        }
    };
    let digest = shot::source_digest(&doc.display, &spans);
    let key = shot::shot_cache_key(digest, theme.id(), &title, &pins, out_format, &font);

    let store_dir = if args.no_cache {
        None
    } else {
        args.store.clone().or_else(default_store)
    };
    let store = match &store_dir {
        Some(dir) => {
            Some(Store::open(dir).map_err(|e| format!("shot cache store {}: {e}", dir.display()))?)
        }
        None => None,
    };
    // The derivation names the displayed source object, so the store keeps it.
    let cached = match &store {
        Some(store) => {
            let source = store
                .put_blob(Domain::Source, doc.display.as_bytes())
                .map_err(|e| e.to_string())?;
            let inputs = derivation(&key, format_png).with_input("source", source.id);
            match store
                .lookup_derivation(&inputs)
                .map_err(|e| e.to_string())?
            {
                Some(id) => Some(store.read_blob(&id).map_err(|e| e.to_string())?),
                None => None,
            }
        }
        None => None,
    };
    let started = Instant::now();
    let (bytes, cache, size) = match cached {
        Some(bytes) => (bytes, "hit", None),
        None => {
            let options = SceneOptions {
                dpi_scale: dpi,
                limits,
                ..SceneOptions::default()
            };
            let scene = shot::build_code_card_scene(
                &title,
                &doc.display,
                &spans,
                &pins,
                &theme.palette(),
                &font,
                options,
            )
            .map_err(|e| e.to_string())?;
            let bytes = if format_png {
                shot::render_png(
                    &scene,
                    &font,
                    RasterOptions {
                        dpi_scale: Some(dpi),
                        limits,
                    },
                )
                .map_err(|e| e.to_string())?
            } else {
                shot::render_svg(
                    &scene,
                    &font,
                    SvgOptions {
                        embed_font: args.embed_font,
                    },
                )
                .into_bytes()
            };
            if let Some(store) = &store {
                let object = store
                    .put_blob(Domain::Artifact, &bytes)
                    .map_err(|e| e.to_string())?;
                let inputs = derivation(&key, format_png)
                    .with_input("source", blob_id(Domain::Source, doc.display.as_bytes()));
                store
                    .publish_derivation(&inputs, object.id)
                    .map_err(|e| e.to_string())?;
            }
            let size = (
                scene.width_px * if format_png { dpi } else { 1 },
                scene.height_px * if format_png { dpi } else { 1 },
            );
            (
                bytes,
                if store.is_some() { "miss" } else { "off" },
                Some(size),
            )
        }
    };
    let elapsed = started.elapsed();
    std::fs::write(&args.output, &bytes).map_err(|e| format!("{}: {e}", args.output.display()))?;
    if args.json {
        println!(
            "{}",
            json!({
                "output": args.output.display().to_string(),
                "format": if format_png { "png" } else { "svg" },
                "bytes": bytes.len(),
                "cache": cache,
                "key": key.to_hex(),
                "theme": theme.id(),
                "dpi": if format_png { Some(dpi) } else { None },
                "width": size.map(|s| s.0),
                "height": size.map(|s| s.1),
                "callouts": pins.len(),
                "verified": args.verify,
                "render_us": elapsed.as_micros() as u64,
            })
        );
    } else {
        println!(
            "wrote {} ({} bytes, cache {cache}{})",
            args.output.display(),
            bytes.len(),
            if args.verify { ", verified" } else { "" }
        );
    }
    Ok(())
}
