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
use std::{io::Read, path::PathBuf, time::Instant};

use serde_json::{Value, json};
use vesper::{
    ast::Mode,
    check, parser,
    runtime::{
        Runtime,
        resource_trace::{ResourceTraceConfig, ResourceTraceLimits},
    },
};

/// The command name (hidden from `--help`).
pub const COMMAND: &str = "tool-facts";

/// The schema every facts document carries.
pub const SCHEMA: &str = "vesper.tool-facts/1";

/// The kind `vesper publish` asks for.
const PUBLISH: &str = "publish";
/// The kind `vesper top SOURCE.ves` asks for.
const TOP: &str = "top";

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
            _ => return Err(format!("unknown tool-facts option '{arg}'")),
        }
    }
    let out = out.ok_or("tool-facts requires --out <path>")?;
    let source_path = source_path.ok_or("tool-facts requires --source <path>")?;
    let mut source = String::new();
    std::fs::File::open(&source_path)
        .and_then(|file| file.take(MAX_SOURCE_BYTES + 1).read_to_string(&mut source))
        .map_err(|e| format!("{}: {e}", source_path.display()))?;
    if source.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "{}: source exceeds {MAX_SOURCE_BYTES} bytes",
            source_path.display()
        ));
    }
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
