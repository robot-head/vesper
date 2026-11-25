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
            // The checked world's declarations for the Vesper interface
            // export (`interface_json` of modules/driver), as the session's
            // text.
            Step::EmitInterface => {
                // A package directory exports under its manifest name by
                // default.
                let package = o
                    .slot(PACKAGE)
                    .map(str::to_owned)
                    .or_else(|| self.loaded.as_ref().map(|p| p.package.clone()))
                    .ok_or("interface export requires --package @org/name or std/name")?;
                let package = vesper_loom_calculus::PackageName::parse(&package)?;
                self.text = interface_words::encode(&self.checked()?.world, package.as_str());
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

/// The checked world's declarations the Vesper interface export reads
/// (`interface.ves` of modules/driver, #1893), as a byte stream: LEB128
/// numbers, and texts as their length and bytes.
///
/// ```text
/// package
/// index:  n adts (key, generic count, nominal mode)
///         n effects (key, generic count, linear)
/// adts:   key, G, is_enum, n variants (name, named, n fields (name, T))
/// effects: key, linear, G, n ops (name, local, F)
/// traits: n names
/// conformances: n (has_model, model, path, trait, self type)
/// functions: n not generated (key, G, F)
/// G: n (name, row, bound)
/// F: n params (pass, n projections (n parts), T), T result, E
/// E: n instances (labeled, name, n args T), has_tail, tail
/// T: 0 named (name, n T) | 1 tuple (n T) | 2 function (n (pass, T), T, E,
///    mode) | 3 row | 4 unresolved
/// ```
///
/// Modes are 0 copy, 1 affine, 2 linear; passes 0 in, 1 inout, 2 sink.
mod interface_words {
    use std::collections::BTreeMap;
    use vesper::{ast, check::World};

    #[derive(Default)]
    pub(super) struct Out(pub(super) Vec<u8>);

    impl Out {
        fn n(&mut self, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    self.0.push(b);
                    return;
                }
                self.0.push(b | 0x80);
            }
        }
        fn len(&mut self, n: usize) {
            self.n(n as u64);
        }
        fn b(&mut self, v: bool) {
            self.0.push(u8::from(v));
        }
        fn s(&mut self, s: &str) {
            self.len(s.len());
            self.0.extend_from_slice(s.as_bytes());
        }
        fn mode(&mut self, m: ast::Mode) {
            self.n(m as u64);
        }
        fn pass(&mut self, p: ast::Pass) {
            self.n(p as u64);
        }
        fn generics(&mut self, gs: &[ast::Generic]) {
            self.len(gs.len());
            for g in gs {
                self.s(&g.name);
                self.b(g.row);
                self.mode(g.bound);
            }
        }
        fn effects(&mut self, e: &ast::Effects) {
            self.len(e.instances.len());
            for i in &e.instances {
                self.b(i.label.is_some());
                self.s(&i.name);
                self.len(i.args.len());
                for t in &i.args {
                    self.ty(t);
                }
            }
            self.b(e.tail.is_some());
            self.s(e.tail.as_deref().unwrap_or(""));
        }
        fn ty(&mut self, t: &ast::Type) {
            match t {
                ast::Type::Named(n, args) => {
                    self.n(0);
                    self.s(n);
                    self.len(args.len());
                    for a in args {
                        self.ty(a);
                    }
                }
                ast::Type::Tuple(ts) => {
                    self.n(1);
                    self.len(ts.len());
                    for a in ts {
                        self.ty(a);
                    }
                }
                ast::Type::Function {
                    params,
                    result,
                    effects,
                    mode,
                } => {
                    self.n(2);
                    self.len(params.len());
                    for (p, t) in params {
                        self.pass(*p);
                        self.ty(t);
                    }
                    self.ty(result);
                    self.effects(effects);
                    self.mode(*mode);
                }
                ast::Type::Row(_) => self.n(3),
                ast::Type::Infer | ast::Type::IntLiteral(_) => self.n(4),
            }
        }
        fn function(&mut self, f: &ast::Function) {
            self.len(f.params.len());
            for p in &f.params {
                self.pass(p.pass);
                self.len(p.projections.len());
                for path in &p.projections {
                    self.len(path.len());
                    for part in path {
                        self.s(part);
                    }
                }
                self.ty(&p.ty);
            }
            self.ty(&f.result);
            self.effects(&f.effects);
        }
    }

    // `Exporter::nominal_mode` of src/interface.rs.
    fn nominal_mode(world: &World, adt: &ast::Adt) -> ast::Mode {
        let bounds = adt
            .generics
            .iter()
            .map(|g| (g.name.clone(), ast::Mode::Copy))
            .collect::<BTreeMap<_, _>>();
        let ty = ast::Type::Named(
            adt.name.clone(),
            adt.generics
                .iter()
                .map(|g| ast::Type::named(&g.name))
                .collect(),
        );
        world.mode(&ty, &bounds)
    }

    pub(super) fn encode(world: &World, package: &str) -> Vec<u8> {
        let mut o = Out::default();
        o.s(package);
        o.len(world.adts.len());
        for (name, adt) in &world.adts {
            o.s(name);
            o.len(adt.generics.len());
            o.mode(nominal_mode(world, adt));
        }
        o.len(world.effects.len());
        for (name, e) in &world.effects {
            o.s(name);
            o.len(e.generics.len());
            o.b(e.linear);
        }
        for (name, adt) in &world.adts {
            o.s(name);
            o.generics(&adt.generics);
            o.b(adt.is_enum);
            o.len(adt.variants.len());
            for v in &adt.variants {
                o.s(&v.name);
                o.b(v.named);
                o.len(v.fields.len());
                for f in &v.fields {
                    o.s(&f.name);
                    o.ty(&f.ty);
                }
            }
        }
        for (name, e) in &world.effects {
            o.s(name);
            o.b(e.linear);
            o.generics(&e.generics);
            o.len(e.ops.len());
            for op in &e.ops {
                o.s(&op.name);
                o.b(!op.generics.is_empty()
                    || op.body.is_some()
                    || !op.effects.instances.is_empty()
                    || op.effects.tail.is_some());
                o.function(op);
            }
        }
        let traits = &world.elaboration.traits;
        o.len(traits.len());
        for t in traits {
            o.s(t);
        }
        let conformances = &world.elaboration.conformances;
        o.len(conformances.len());
        for c in conformances {
            o.b(c.model.is_some());
            o.s(c.model.as_deref().unwrap_or(""));
            o.s(&c.path);
            o.s(&c.trait_name);
            o.s(&c.self_ty);
        }
        let generated = &world.elaboration.generated;
        let functions: Vec<_> = world
            .functions
            .iter()
            .filter(|(name, _)| !generated.contains_key(*name))
            .collect();
        o.len(functions.len());
        for (name, f) in functions {
            o.s(name);
            o.generics(&f.generics);
            o.function(f);
        }
        o.0
    }
}
