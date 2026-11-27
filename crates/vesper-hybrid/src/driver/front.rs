//! The front-end steps (#1893): parse, syntax reports, check, the HIR and
//! interface emitters and `build --lib`.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

/// Tag byte for `self.text` when `ReportSyntax` / `CheckReport` hands JSON
/// diagnostics to `@vesper/driver::diagnostics_json` (single file).
const DIAG_JSON_SINGLE: u8 = 0;
/// Tag byte for `self.text` when `CheckReport` hands JSON diagnostics to
/// `@vesper/driver::package_diagnostics_json` (package).
const DIAG_JSON_PACKAGE: u8 = 1;
/// Tag byte for `self.text` when `ReportSyntax` / `CheckReport` hands stderr
/// diagnostic chunks to `modules/driver/src/lib.ves` (`emit_diagnostics`).
const DIAG_TEXT_STREAM: u8 = 2;

/// Chunk tags inside `DIAG_TEXT_STREAM`.
const DIAG_CHUNK_LITERAL: u8 = 0;
const DIAG_CHUNK_RAW: u8 = 1;
const DIAG_CHUNK_MANIFEST: u8 = 2;
const DIAG_CHUNK_UNLOCATED: u8 = 3;

fn push_len_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn render_terminal_diagnostics(
    file: &str,
    source: &str,
    ds: &[vesper::ast::Diagnostic],
) -> Result<Vec<u8>, String> {
    if source.len() > vesper_span::Limits::HARD.max_source_bytes {
        return Err("source exceeds the 1 MiB diagnostic display profile".into());
    }
    let index =
        vesper_span::LineIndex::new(std::sync::Arc::from(source), vesper_span::Limits::HARD)
            .map_err(|error| error.to_string())?;
    vesper_diagnostics::prepare(&index, ds, vesper_diagnostics::Limits::HARD)
        .and_then(|prepared| prepared.terminal(file))
        .map_err(|error| error.to_string())
}

fn push_single_text_chunk(
    out: &mut Vec<u8>,
    file: &str,
    source: &str,
    ds: &[vesper::ast::Diagnostic],
) {
    match render_terminal_diagnostics(file, source, ds) {
        Ok(bytes) => {
            out.push(DIAG_CHUNK_LITERAL);
            push_len_bytes(out, &bytes);
        }
        Err(reason) => {
            out.push(DIAG_CHUNK_RAW);
            push_len_bytes(out, reason.as_bytes());
            let mut stream = Vec::new();
            stream.extend_from_slice(&(ds.len() as u64).to_le_bytes());
            for d in ds {
                push_diag_fields(&mut stream, d);
            }
            push_len_bytes(out, &stream);
        }
    }
}

fn encode_diagnostics_text_stream(
    file: &str,
    source: &str,
    ds: &[vesper::ast::Diagnostic],
    map: &[vesper::forge_frontend::SourceMapSegment],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(DIAG_TEXT_STREAM);
    out.extend_from_slice(&1u64.to_le_bytes());
    if map.is_empty() {
        push_single_text_chunk(&mut out, file, source, ds);
    } else {
        let mapped: Vec<_> = ds
            .iter()
            .cloned()
            .map(|d| vesper::forge_frontend::original_diagnostic(map, d))
            .collect();
        push_single_text_chunk(&mut out, file, source, &mapped);
    }
    out
}

fn encode_package_diagnostics_text_stream(
    package: &vesper::packages::Assembled,
    ds: &[vesper::packages::PackageDiagnostic],
) -> Vec<u8> {
    let files = &package.sources.files;
    let total = package.manifest_errors.len() + ds.len();
    let mut out = Vec::new();
    out.push(DIAG_TEXT_STREAM);
    out.extend_from_slice(&(total as u64).to_le_bytes());
    for (file, d) in &package.manifest_errors {
        out.push(DIAG_CHUNK_MANIFEST);
        push_len_bytes(&mut out, file.as_bytes());
        push_len_bytes(&mut out, d.code.as_bytes());
        push_len_bytes(&mut out, d.message.as_bytes());
    }
    for d in ds {
        match d.file {
            Some(f) => push_single_text_chunk(
                &mut out,
                &files[f].display,
                &files[f].text,
                std::slice::from_ref(&d.diagnostic),
            ),
            None => {
                out.push(DIAG_CHUNK_UNLOCATED);
                push_len_bytes(&mut out, d.diagnostic.code.as_bytes());
                push_len_bytes(&mut out, d.diagnostic.message.as_bytes());
            }
        }
    }
    out
}

fn push_diag_fields(out: &mut Vec<u8>, d: &vesper::ast::Diagnostic) {
    let push_u64 = |buf: &mut Vec<u8>, v: u64| buf.extend_from_slice(&v.to_le_bytes());
    let push_str = |buf: &mut Vec<u8>, s: &str| {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    };
    push_str(out, &d.code);
    push_str(out, &d.message);
    push_u64(out, d.span.start as u64);
    push_u64(out, d.span.end as u64);
    push_u64(out, d.related.len() as u64);
    for r in &d.related {
        push_u64(out, r.span.start as u64);
        push_u64(out, r.span.end as u64);
        push_str(out, &r.message);
    }
    match &d.help {
        Some(h) => {
            push_u64(out, 1);
            push_str(out, h);
        }
        None => push_u64(out, 0),
    }
}

const DIAG_MAP_KIND_SOURCE: u64 = 0;
const DIAG_MAP_KIND_GENERATED: u64 = 1;

fn encode_diagnostics_json_stream(
    ds: &[vesper::ast::Diagnostic],
    map: &[vesper::forge_frontend::SourceMapSegment],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(DIAG_JSON_SINGLE);
    out.extend_from_slice(&(map.len() as u64).to_le_bytes());
    for s in map {
        out.extend_from_slice(&(s.expanded.start as u64).to_le_bytes());
        out.extend_from_slice(&(s.expanded.end as u64).to_le_bytes());
        out.extend_from_slice(&(s.original.start as u64).to_le_bytes());
        out.extend_from_slice(&(s.original.end as u64).to_le_bytes());
        let kind = match s.kind {
            vesper::forge_frontend::MapKind::Source => DIAG_MAP_KIND_SOURCE,
            vesper::forge_frontend::MapKind::Generated => DIAG_MAP_KIND_GENERATED,
        };
        out.extend_from_slice(&kind.to_le_bytes());
    }
    out.extend_from_slice(&(ds.len() as u64).to_le_bytes());
    for d in ds {
        push_diag_fields(&mut out, d);
    }
    out
}

fn encode_package_diagnostics_json_stream(
    package: &vesper::packages::Assembled,
    ds: &[vesper::packages::PackageDiagnostic],
) -> Vec<u8> {
    let files = &package.sources.files;
    let total = package.manifest_errors.len() + ds.len();
    let mut out = Vec::new();
    out.push(DIAG_JSON_PACKAGE);
    out.extend_from_slice(&(total as u64).to_le_bytes());
    for (file, d) in &package.manifest_errors {
        out.extend_from_slice(&(file.len() as u64).to_le_bytes());
        out.extend_from_slice(file.as_bytes());
        push_diag_fields(&mut out, d);
    }
    for d in ds {
        let file = d.file.map_or("Loom.toml", |f| files[f].display.as_str());
        out.extend_from_slice(&(file.len() as u64).to_le_bytes());
        out.extend_from_slice(file.as_bytes());
        push_diag_fields(&mut out, &d.diagnostic);
    }
    out
}

impl Session {
    pub(super) fn front_step(&mut self, step: Step, _arg: &[u8]) -> Result<i64, String> {
        let o = &self.o;
        match step {
            Step::Parse => {
                let parsed = match &self.loaded {
                    Some(p) => {
                        self.source = self.original.clone();
                        parser::Parse {
                            module: p.module.clone(),
                            green: parser::parse("").green,
                            diagnostics: vec![],
                        }
                    }
                    // `--emit cst` parses in its own pass.
                    None if o.command == "emit" && o.format() == "cst" => return Ok(0),
                    None => {
                        let (source, parsed, map) = vesper::forge_frontend::parse_compiler_input(
                            &self.original,
                            o.include_tests,
                        )
                        .map_err(|diagnostic| {
                            crate::diagnostics(
                                o.input(),
                                &self.original,
                                std::slice::from_ref(&diagnostic),
                                o.json,
                            );
                            format!("{}: {}", diagnostic.code, diagnostic.message)
                        })?;
                        self.source = source.into_owned();
                        self.source_map = map;
                        parsed
                    }
                };
                let n = count(parsed.diagnostics.len());
                self.parsed = Some(parsed);
                Ok(n)
            }
            Step::EmitCst => {
                let original = &self.original;
                let core = parser::parse(original);
                let syntax = if core.diagnostics.is_empty() {
                    core.syntax()
                } else if let Ok(surface) =
                    parser::parse_surface_with_limits(original, Default::default())
                    && surface.diagnostics.is_empty()
                {
                    surface.syntax()
                } else {
                    crate::diagnostics(o.input(), original, &core.diagnostics, o.json);
                    core.syntax()
                };
                self.text = format!("{syntax:#?}\n").into_bytes();
                Ok(0)
            }
            Step::ReportSyntax => {
                let ds = &self.parsed()?.diagnostics;
                self.text = if o.json {
                    encode_diagnostics_json_stream(ds, &self.source_map)
                } else {
                    encode_diagnostics_text_stream(o.input(), &self.original, ds, &self.source_map)
                };
                Ok(0)
            }
            Step::Check => {
                let checked = check::check(&self.parsed()?.module);
                self.checked = Some(checked);
                self.report_check()
            }
            // `check::check`'s parts, sequenced by `check` in
            // modules/driver/src/lib.ves (#1900).
            Step::CheckUntyped => {
                let rewrite = vesper::arrays::untyped_members(self.check_module()?);
                Ok(match rewrite {
                    Some((module, diagnostics)) => {
                        self.checking.rewritten = Some(module);
                        self.checking.pending.push(diagnostics);
                        1
                    }
                    None => 0,
                })
            }
            Step::CheckTensor => {
                let checked = vesper::tensor_types::check(self.check_module()?);
                let found = checked.is_some();
                if found {
                    self.checked = checked;
                }
                Ok(i64::from(found))
            }
            Step::CheckFrontend => {
                let beacon = vesper::trace_effects::with_prelude(self.check_module()?);
                let mut checked = vesper::pact_frontend::check_module(beacon.module());
                beacon.check(&mut checked);
                self.checked = Some(checked);
                Ok(0)
            }
            Step::CheckUses => {
                // `use` imports resolve only through the package loader.
                let uses: Vec<_> = self
                    .check_module()?
                    .uses
                    .iter()
                    .map(|u| u.span.clone())
                    .collect();
                let checked = self.checked.as_mut().ok_or("driver: no check yet")?;
                let n = uses.len();
                for span in uses {
                    checked.diagnostics.push(
                        vesper::ast::Diagnostic::new(
                            check::USE_OUTSIDE_PACKAGE,
                            "`use` imports need a package; check the package directory instead of one file",
                            span,
                        )
                        .with_help("a package is a directory with a Loom.toml and src/**/*.ves"),
                    );
                }
                Ok(count(n))
            }
            Step::CheckFinish => {
                let checking = std::mem::take(&mut self.checking);
                let checked = self.checked.as_mut().ok_or("driver: no check yet")?;
                for diagnostics in checking.pending.into_iter().rev() {
                    checked.diagnostics.extend(diagnostics);
                }
                Ok(0)
            }
            Step::CheckReport => self.report_check(),
            Step::LibBuild => {
                self.lib_build()?;
                Ok(0)
            }
            // The interface's package name (validated) for the Vesper
            // interface export (`interface_json` of modules/driver), as
            // the session's text.
            Step::EmitInterface => {
                // A package directory exports under its manifest name by
                // default.
                let package = o
                    .slot(PACKAGE)
                    .map(str::to_owned)
                    .or_else(|| self.loaded.as_ref().map(|p| p.package.clone()))
                    .ok_or("interface export requires --package @org/name or std/name")?;
                let package = vesper_loom_calculus::PackageName::parse(&package)?;
                // The declarations themselves are the world the driver
                // holds (`WorldDecls`, driver/world.rs, #2024).
                self.text = package.as_str().as_bytes().to_vec();
                Ok(0)
            }
            Step::EmitHir => {
                self.text = serde_json::to_string_pretty(&self.parsed()?.module)
                    .map_err(|e| e.to_string())?
                    .into_bytes();
                Ok(0)
            }
            _ => Err(format!("driver: step {} is not a front step", step as u32)),
        }
    }

    /// `build --lib` (and any library option): the export path, as in
    /// `cli()`.
    fn lib_build(&mut self) -> Result<(), String> {
        let o = &self.o;
        let parsed = self.parsed.take().ok_or("driver: no parse yet")?;
        let loaded = &self.loaded;
        let package_vsys = loaded.as_ref().map_or(&[][..], |p| &p.vsys[..]);
        let lib = o.lib_flags()?;
        // Exports are the root module's `pub` functions. Items of package
        // submodules and dependencies are renamed and stay internal.
        let public = parsed
            .module
            .public_items
            .iter()
            .filter(|n| {
                loaded
                    .as_ref()
                    .is_none_or(|p| !p.sources.qualified.contains_key(*n))
            })
            .cloned()
            .collect();
        let directives_source = match loaded {
            Some(p) => crate::export_directives_source(p)?,
            None => self.source.clone(),
        };
        let foreign = loaded.is_some() && lib.needs_module(&self.source, !package_vsys.is_empty());
        let module = foreign.then(|| parsed.module.clone());
        let built = lib.build(
            o.command,
            parsed.module,
            &public,
            o.input(),
            &directives_source,
            package_vsys,
            o.output(),
            o.target(),
            o.optimize,
            o.json,
            o.exclusive(),
        );
        // A package that imports `.vsys` libraries is checked by the
        // export path; on failure, check it again through the Bridge
        // lowering to report each diagnostic at its own file and line.
        if built.is_err()
            && let (Some(p), Some(module)) = (loaded, module)
            && let Some(ds) =
                crate::foreign_package_diagnostics(p, &module, &directives_source, o.target())
        {
            crate::package_diagnostics(p, &ds, o.json);
            return Err(format!("{} semantic error(s)", ds.len()));
        }
        built
    }
}

impl Session {
    /// The module `check::check`'s next part checks: the latest rewrite, or
    /// the parse's.
    fn check_module(&self) -> Result<&vesper::ast::Module, String> {
        match &self.checking.rewritten {
            Some(m) => Ok(m),
            None => Ok(&self.parsed()?.module),
        }
    }

    /// Prints the check's diagnostics unless the export path reports them
    /// (a library whose `.vsys` imports check only through the Bridge
    /// lowering); their count when printed, else 0.
    fn report_check(&mut self) -> Result<i64, String> {
        let o = &self.o;
        let checked = self.checked()?;
        let lib = o.lib_flags()?;
        let sugar_rejection = checked
            .diagnostics
            .iter()
            .any(|d| matches!(d.code.as_str(), "E0143" | "E0144"));
        if checked.diagnostics.is_empty()
            || !(sugar_rejection
                || !lib.needs_module(&self.source, !self.package_vsys().is_empty()))
        {
            return Ok(0);
        }
        let n = count(checked.diagnostics.len());
        let bytes = match &self.loaded {
            Some(p) => {
                let localized: Vec<_> = checked
                    .diagnostics
                    .iter()
                    .map(|d| p.sources.localize(d))
                    .collect();
                if o.json {
                    encode_package_diagnostics_json_stream(p, &localized)
                } else {
                    encode_package_diagnostics_text_stream(p, &localized)
                }
            }
            None => {
                if o.json {
                    encode_diagnostics_json_stream(&checked.diagnostics, &self.source_map)
                } else {
                    encode_diagnostics_text_stream(
                        o.input(),
                        &self.original,
                        &checked.diagnostics,
                        &self.source_map,
                    )
                }
            }
        };
        self.text = bytes;
        Ok(n)
    }
}
