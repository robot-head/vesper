//! `vesper shot`: render a Vesper example as a vector SVG or a 1x-4x PNG code
//! card (§43.1 item 3, §43.2, §43.5), optionally after checking and running it
//! in the Codex doctest sandbox (§40.1), with outputs cached in the
//! content-addressed store keyed by BLAKE3(source partition, theme, config).

use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;
use vesper::doctest::{self, DocTestLimits};
use vesper_highlight::shot::{
    self, RasterOptions, SceneOptions, ShotFont, ShotLimits, ShotOutputFormat, ShotTheme,
    SvgOptions,
};
use vesper_highlight::{Highlight, HoverEntry};
use vesper_loom_cas::{DerivationInputs, Domain, Store, blob_id};

/// The bundled card font: JetBrains Mono 2.304 Regular, SIL OFL 1.1
/// (assets/fonts/jetbrains-mono/OFL.txt).
pub const CARD_FONT: &[u8] =
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");

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

/// Tier 0/Tier 1 highlighting of the displayed text (lane 1's highlighter).
/// Tier 1 when the snippet checks, else Tier 0 (with lane 1's fallback
/// reason); the hover table comes along for `//^?` queries.
fn highlight(display: &str) -> Result<Highlight, String> {
    crate::highlight_commands::highlight_source(display, 1).map_err(|e| format!("highlight: {e:?}"))
}

/// Checks the displayed example and runs it in the doctest sandbox: the
/// example becomes one declaration fence (entry `main`) of a generated doc
/// comment, so it goes through the same checker, shared JIT module and
/// evaluator fallback as `vesper doctest`.
pub fn verify_example(display: &str) -> Result<(), String> {
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
    let report =
        doctest::run(&wrapped, DocTestLimits::default()).map_err(|e| format!("--verify: {e}"))?;
    if report.passed && report.examples.len() == 1 {
        return Ok(());
    }
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
    Err(format!(
        "--verify: the example failed; not rendering it\n  {}",
        reasons.join("\n  ")
    ))
}

/// Answers a `//^?` query from the checker: the type and type mode of the
/// expression or parameter at `start` (the same facts Lens hover shows).
/// `None` when the snippet does not check or has no type there.
fn checker_hover<'a>(
    display: &'a str,
    table: &'a [HoverEntry],
) -> impl FnMut(&str, usize, usize) -> Option<String> + 'a {
    let analysis = vesper::tooling::frontend::check_source(display, Default::default());
    let index = vesper_span::LineIndex::new(display.into(), Default::default()).ok();
    move |_token, start, end| {
        // Tier 1's hover table first (it covers binding sites), then Lens hover.
        if let Some(h) = table.iter().find(|h| h.span == (start..end)) {
            return Some(h.text.to_string());
        }
        let checked = match &analysis {
            Ok(checked) => checked,
            Err(vesper::tooling::frontend::AnalysisFailure::Semantic { diagnostics }) => {
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
                // The renderer has its own character bound. Normalize control
                // characters and bound the summary without splitting UTF-8.
                return Some(
                    text.chars()
                        .take(600)
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
        let pos =
            |v: &serde_json::Value| Some((v.get("line")?.as_u64()?, v.get("character")?.as_u64()?));
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
    }
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
    if args.verify {
        verify_example(&doc.display)?;
    }
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
    let Highlight { spans, hovers, .. } = highlight(&doc.display)?;
    let pins = shot::resolve_callouts(&doc, &spans, checker_hover(&doc.display, &hovers))
        .map_err(|e| e.to_string())?;
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
