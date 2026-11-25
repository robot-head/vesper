//! The front-end steps (#1893): parse, syntax reports, check, the HIR and
//! interface emitters and `build --lib`.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

impl Session {
    pub(super) fn front_step(&mut self, step: Step) -> Result<i64, String> {
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
                crate::write(o.output(), format!("{syntax:#?}\n").as_bytes())?;
                Ok(0)
            }
            Step::ReportSyntax => {
                let parsed = self.parsed()?;
                crate::compiler_diagnostics(
                    o.input(),
                    &self.original,
                    &parsed.diagnostics,
                    &self.source_map,
                    o.json,
                );
                Ok(0)
            }
            Step::Check => {
                let checked = check::check(&self.parsed()?.module);
                let lib = o.lib_flags()?;
                // A library with `.vsys` imports checks only through the
                // Bridge lowering, which the export path runs.
                let sugar_rejection = checked
                    .diagnostics
                    .iter()
                    .any(|d| matches!(d.code.as_str(), "E0143" | "E0144"));
                let n = if !checked.diagnostics.is_empty()
                    && (sugar_rejection
                        || !lib.needs_module(&self.source, !self.package_vsys().is_empty()))
                {
                    match &self.loaded {
                        Some(p) => {
                            let localized: Vec<_> = checked
                                .diagnostics
                                .iter()
                                .map(|d| p.sources.localize(d))
                                .collect();
                            crate::package_diagnostics(p, &localized, o.json);
                        }
                        None => crate::compiler_diagnostics(
                            o.input(),
                            &self.original,
                            &checked.diagnostics,
                            &self.source_map,
                            o.json,
                        ),
                    }
                    count(checked.diagnostics.len())
                } else {
                    0
                };
                self.checked = Some(checked);
                Ok(n)
            }
            Step::LibBuild => {
                self.lib_build()?;
                Ok(0)
            }
            Step::EmitInterface => {
                // A package directory exports under its manifest name by
                // default.
                let package = o
                    .slot(PACKAGE)
                    .map(str::to_owned)
                    .or_else(|| self.loaded.as_ref().map(|p| p.package.clone()))
                    .ok_or("interface export requires --package @org/name or std/name")?;
                let interface = vesper::interface::export(&self.checked()?.world, &package)?;
                let bytes = serde_json::to_vec_pretty(&interface).map_err(|e| e.to_string())?;
                if let Some(path) = o.output() {
                    vesper::atomic_file::atomic_write(&path, &bytes)?;
                } else {
                    crate::write(None, &bytes)?;
                }
                Ok(0)
            }
            Step::EmitHir => {
                crate::write(
                    o.output(),
                    serde_json::to_string_pretty(&self.parsed()?.module)
                        .map_err(|e| e.to_string())?
                        .as_bytes(),
                )?;
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
