//! The front-end steps (#1893): parse, syntax reports, check, the HIR and
//! interface emitters and `build --lib`.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

impl Session {
    pub(super) fn front_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
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
            // The interface JSON (`arg`) to `-o`, written atomically, or
            // to stdout.
            Step::InterfaceWrite => {
                if let Some(path) = o.output() {
                    vesper::atomic_file::atomic_write(&path, arg)?;
                } else {
                    crate::write(None, arg)?;
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
    fn report_check(&self) -> Result<i64, String> {
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
        Ok(count(checked.diagnostics.len()))
    }
}
