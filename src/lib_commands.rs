//! `vesper build --lib` flags (port P0 lane 6, #878; `--instance`, export
//! ABI v2, #898). The library itself is [`vesper::native::export`].
use std::path::PathBuf;
use vesper::native::export::{self, BindingOptions, Instance, LibraryOptions};

#[derive(Default)]
pub struct LibFlags {
    pub enabled: bool,
    name: Option<String>,
    interface: Option<PathBuf>,
    rust_binding: Option<PathBuf>,
    rust_types: Option<String>,
    instances: Vec<String>,
}

impl LibFlags {
    /// Consume `a` (and its value) if it is a library flag.
    pub fn flag(
        &mut self,
        a: &str,
        args: &mut impl Iterator<Item = String>,
    ) -> Result<bool, String> {
        let mut value = |what: &str| args.next().ok_or(format!("{a} requires {what}"));
        match a {
            "--lib" => self.enabled = true,
            "--lib-name" => self.name = Some(value("a library name")?),
            "--interface" => self.interface = Some(PathBuf::from(value("a path")?)),
            "--rust-binding" => self.rust_binding = Some(PathBuf::from(value("a path")?)),
            "--rust-types" => self.rust_types = Some(value("a Rust module path")?),
            "--instance" => self.instances.push(value("an instance such as f[u64]")?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn any_option(&self) -> bool {
        self.name.is_some()
            || self.interface.is_some()
            || self.rust_binding.is_some()
            || self.rust_types.is_some()
            || !self.instances.is_empty()
    }

    /// Whether `source` must be compiled by [`LibFlags::build_module`]: its
    /// `.vsys` imports check only through the Bridge lowering.
    pub fn needs_module(&self, source: &str) -> bool {
        (self.enabled || self.any_option()) && source.contains("// @export vsys")
    }

    /// `build --lib`: the module is compiled by the export path, which adds
    /// generic-instance wrappers (`// @export instance`, `--instance`) and
    /// checks `.vsys` imports (`// @export vsys`) through the Bridge
    /// lowering.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        self,
        command: &str,
        module: vesper::ast::Module,
        public: &std::collections::BTreeSet<String>,
        input: &str,
        source: &str,
        output: Option<PathBuf>,
        target: Option<&str>,
        optimize: bool,
        json: bool,
        exclusive: bool,
    ) -> Result<(), String> {
        let job = self.job(command, input, output, target, optimize, exclusive)?;
        let library = export::compile_module(
            input,
            module,
            source,
            &job.options(),
            &job.instances,
            Some(public),
        )
        .map_err(|e| e.to_string())?;
        job.finish(&library, json)
    }

    fn job<'t>(
        self,
        command: &str,
        input: &str,
        output: Option<PathBuf>,
        target: Option<&'t str>,
        optimize: bool,
        exclusive: bool,
    ) -> Result<Job<'t>, String> {
        if command != "build" || !self.enabled {
            return Err(
                "--lib-name, --interface, --rust-binding and --instance require build --lib".into(),
            );
        }
        if exclusive {
            return Err(
                "--verify, --verify-policy, --tee and --codegen do not apply to build --lib yet"
                    .into(),
            );
        }
        if self.rust_types.is_some() && self.rust_binding.is_none() {
            return Err("--rust-types requires --rust-binding".into());
        }
        let output = output.ok_or("build --lib requires -o <object path>")?;
        let name = match self.name {
            Some(n) => n,
            None => std::path::Path::new(input)
                .file_stem()
                .map(|s| s.to_string_lossy().replace('-', "_"))
                .unwrap_or_default(),
        };
        let instances = self
            .instances
            .iter()
            .map(|i| export::directives::parse_instance(i).map_err(|e| e.to_string()))
            .collect::<Result<Vec<Instance>, _>>()?;
        Ok(Job {
            name,
            target,
            optimize,
            output,
            interface: self.interface,
            rust_binding: self.rust_binding,
            binding: BindingOptions {
                types_path: self.rust_types,
            },
            instances,
        })
    }
}

struct Job<'t> {
    name: String,
    target: Option<&'t str>,
    optimize: bool,
    output: PathBuf,
    interface: Option<PathBuf>,
    rust_binding: Option<PathBuf>,
    binding: BindingOptions,
    instances: Vec<Instance>,
}

impl Job<'_> {
    fn options(&self) -> LibraryOptions<'_> {
        LibraryOptions {
            library: &self.name,
            target: self.target,
            optimize: self.optimize,
        }
    }

    fn finish(&self, library: &export::Library, json: bool) -> Result<(), String> {
        let summary = export::write_library(
            library,
            &self.output,
            self.interface.as_deref(),
            self.rust_binding.as_deref().map(|p| (p, &self.binding)),
        )?;
        if !json {
            println!("{summary}");
        }
        Ok(())
    }
}
