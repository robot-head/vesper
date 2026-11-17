//! `vesper build --lib` flags (port P0 lane 6, #878). The library itself is
//! [`vesper::native::export`].
use std::path::PathBuf;
use vesper::native::export::{self, BindingOptions, LibraryOptions};

#[derive(Default)]
pub struct LibFlags {
    pub enabled: bool,
    name: Option<String>,
    interface: Option<PathBuf>,
    rust_binding: Option<PathBuf>,
    rust_types: Option<String>,
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
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn any_option(&self) -> bool {
        self.name.is_some()
            || self.interface.is_some()
            || self.rust_binding.is_some()
            || self.rust_types.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build(
        self,
        command: &str,
        world: &vesper::check::World,
        source: &str,
        input: &str,
        output: Option<PathBuf>,
        target: Option<&str>,
        optimize: bool,
        json: bool,
    ) -> Result<(), String> {
        if command != "build" || !self.enabled {
            return Err("--lib-name, --interface and --rust-binding require build --lib".into());
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
        let options = LibraryOptions {
            library: &name,
            target,
            optimize,
        };
        let binding = BindingOptions {
            types_path: self.rust_types,
        };
        let summary = export::build_command(
            world,
            source,
            &options,
            &output,
            self.interface.as_deref(),
            self.rust_binding.as_deref().map(|p| (p, &binding)),
        )?;
        if !json {
            println!("{summary}");
        }
        Ok(())
    }
}
