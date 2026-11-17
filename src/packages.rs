//! Multi-file packages and `use` imports (port wave P0, lane 1; manuscript
//! ch 2.2 `UseDecl` and ch 11.3 path dependencies).
//!
//! A package is a directory with a `Loom.toml`. Its modules are the files
//! `src/**/*.ves`: `src/lib.ves` (or `src/main.ves`) is the root module and
//! `src/a/b.ves` is module `a::b`. A module sees its own top-level items and
//! the items it imports with `use`:
//!
//! ```text
//! use @demo/geometry::shapes::{Point, area as shape_area};
//! use self::util::clamp;      // another file of this package
//! ```
//!
//! Only `pub` items can be imported; everything else is private to its
//! file. Dependencies are `[dependencies]` entries with a `path`.
//!
//! The loader assembles every module of the root package and its dependency
//! closure into one ordinary [`Module`], so the checker and every backend
//! run unchanged:
//! - Items of the root package's root module keep their names, so a
//!   one-file package checks exactly like `vesper check file.ves`.
//! - Every other item gets the identifier-safe qualified name
//!   `{scope}__{name}__{module}__{item}`, each component escaped so the
//!   encoding is injective (`@demo/geometry` module `shapes` item `area` is
//!   `demo__geometry__shapes__area`; `_` and `-` inside a component become
//!   `_1` and `_2`). A struct's constructor is renamed with it; enum
//!   variants and effect operations keep their names, two modules may not
//!   declare the same one ([`AMBIGUOUS`]), and a module may use a variant
//!   only when it declares or imports the enum.
//! - Each file's spans are shifted by its offset in a virtual concatenation
//!   of all sources ([`SourceMap`]), so span-keyed checker tables never
//!   collide and diagnostics map back to the right file.
use crate::ast::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
use vesper_loom_calculus::{Manifest, PackageName};

/// `use` in a single-file module, which has no package to import from.
pub const USE_OUTSIDE_PACKAGE: &str = "E0110";
/// Unknown package, module or item in an import, or an unimported name.
pub const UNKNOWN_IMPORT: &str = "E0111";
/// Import of an item that is not `pub`.
pub const PRIVATE_ITEM: &str = "E0112";
/// One name bound twice in a module, or one variant/operation in two modules.
pub const AMBIGUOUS: &str = "E0113";
/// A dependency cycle between packages.
pub const CYCLE: &str = "E0114";
/// An invalid manifest, dependency or package layout.
pub const INVALID_PACKAGE: &str = "E0115";

/// Walk bounds for one package's `src/` tree.
const MAX_FILES: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_PACKAGES: usize = 256;

/// One source file of an assembled program.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Display path (the CLI input joined with the package-relative path).
    pub display: String,
    pub text: String,
    /// Offset of this file in the virtual concatenation.
    pub base: usize,
}

/// The files of an assembled program, at disjoint offsets.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
    /// Qualified display name of each renamed item (`@demo/geometry::area`).
    pub qualified: BTreeMap<String, String>,
}
impl SourceMap {
    /// Every source concatenated at its base offset (separated by `\n`).
    pub fn combined(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            out.push_str(&f.text);
            out.push('\n');
        }
        out
    }
    /// The file containing a global offset.
    pub fn file_at(&self, offset: usize) -> Option<usize> {
        self.files
            .iter()
            .rposition(|f| f.base <= offset && offset <= f.base + f.text.len())
    }
    /// Map a checker diagnostic (global spans, internal names) to its file:
    /// local spans, labels outside that file dropped, names demangled.
    pub fn localize(&self, d: &Diagnostic) -> PackageDiagnostic {
        let Some(index) = self.file_at(d.span.start) else {
            return PackageDiagnostic {
                file: None,
                diagnostic: self.demangle(d.clone()),
            };
        };
        let file = &self.files[index];
        let local = |s: &Span| -> Option<Span> {
            let end = file.base + file.text.len();
            (s.start >= file.base && s.end <= end).then(|| s.start - file.base..s.end - file.base)
        };
        let mut out = d.clone();
        out.span = local(&d.span).unwrap_or(d.span.start - file.base..d.span.start - file.base);
        out.related = d
            .related
            .iter()
            .filter_map(|l| {
                Some(DiagnosticLabel {
                    span: local(&l.span)?,
                    message: l.message.clone(),
                })
            })
            .collect();
        PackageDiagnostic {
            file: Some(index),
            diagnostic: self.demangle(out),
        }
    }
    fn demangle(&self, mut d: Diagnostic) -> Diagnostic {
        let fix = |s: &str| {
            let mut s = s.to_owned();
            // Longest first, so no qualified name rewrites inside another.
            let mut names: Vec<_> = self.qualified.iter().collect();
            names.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
            for (mangled, shown) in names {
                if s.contains(mangled.as_str()) {
                    s = s.replace(mangled.as_str(), shown);
                }
            }
            s
        };
        d.message = fix(&d.message);
        d.help = d.help.as_deref().map(fix);
        for l in &mut d.related {
            l.message = fix(&l.message);
        }
        d
    }
}

/// A diagnostic in one file (`file` indexes [`SourceMap::files`]), or about
/// a manifest when `file` is `None` and `manifest` names it.
#[derive(Debug, Clone)]
pub struct PackageDiagnostic {
    pub file: Option<usize>,
    pub diagnostic: Diagnostic,
}

/// A loaded package graph assembled into one module.
#[derive(Debug, Clone)]
pub struct Assembled {
    /// The assembled module, ready for `check::check`. Its `uses` are empty.
    pub module: Module,
    pub sources: SourceMap,
    /// Loader diagnostics (syntax errors and import resolution). When
    /// non-empty, `module` must not be checked.
    pub diagnostics: Vec<PackageDiagnostic>,
    /// Manifest-level diagnostics: `(manifest display path, diagnostic)`.
    pub manifest_errors: Vec<(String, Diagnostic)>,
    /// The root package's name.
    pub package: String,
}
impl Assembled {
    pub fn ok(&self) -> bool {
        self.diagnostics.is_empty() && self.manifest_errors.is_empty()
    }
}

struct Package {
    name: PackageName,
    manifest: Manifest,
    /// Display directory (lexical, relative to the CLI input).
    display: PathBuf,
    /// Dependency name -> package index.
    deps: BTreeMap<String, usize>,
    modules: Vec<ModuleSource>,
}
/// (package index, module path).
type ModuleKey = (usize, Vec<String>);
struct ModuleSource {
    /// Module path below the package root (empty for the root module).
    path: Vec<String>,
    file: usize,
    parsed: Module,
}

/// True when `path` is a package directory (it holds a `Loom.toml`).
pub fn is_package(path: &Path) -> bool {
    path.is_dir() && path.join("Loom.toml").is_file()
}

/// Load the package at `dir`, its path-dependency closure, and assemble
/// them into one module. `Err` is an I/O failure; language and manifest
/// errors are reported in [`Assembled`].
pub fn load(dir: &Path) -> Result<Assembled, String> {
    let mut loader = Loader::default();
    let root = loader.package(dir, dir.to_path_buf(), &mut vec![])?;
    let package = root
        .map(|i| loader.packages[i].name.as_str().to_owned())
        .unwrap_or_default();
    if !loader.manifest_errors.is_empty() || root.is_none() {
        return Ok(Assembled {
            module: Module::default(),
            sources: loader.sources,
            diagnostics: loader.diagnostics,
            manifest_errors: loader.manifest_errors,
            package,
        });
    }
    let module = loader.assemble(root.unwrap_or_default());
    Ok(Assembled {
        module,
        sources: loader.sources,
        diagnostics: loader.diagnostics,
        manifest_errors: loader.manifest_errors,
        package,
    })
}

#[derive(Default)]
struct Loader {
    packages: Vec<Package>,
    /// Canonical directory -> package index.
    by_path: BTreeMap<PathBuf, usize>,
    sources: SourceMap,
    diagnostics: Vec<PackageDiagnostic>,
    manifest_errors: Vec<(String, Diagnostic)>,
}

fn manifest_error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message, 0..0)
}

/// `a/./b/../c` -> `a/c`, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if out
                    .components()
                    .next_back()
                    .is_some_and(|l| matches!(l, Component::Normal(_))) =>
            {
                out.pop();
            }
            _ => out.push(c),
        }
    }
    out
}

fn display(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// One name component, escaped so `__` only ever separates components:
/// `_` becomes `_1` and `-` becomes `_2` (`geo-lib` -> `geo_2lib`).
fn escape_component(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '_' => out.push_str("_1"),
            '-' => out.push_str("_2"),
            c => out.push(c),
        }
    }
}
/// The internal name of `item` in module `path` of package `name`: escaped
/// components joined by `__` (`@demo/geometry` `util` `abs` ->
/// `demo__geometry__util__abs`). The encoding is injective.
fn mangled(name: &str, path: &[String], item: &str) -> String {
    let mut out = String::new();
    let (scope, pkg) = name
        .trim_start_matches('@')
        .split_once('/')
        .unwrap_or(("", name));
    for (i, component) in [scope, pkg]
        .into_iter()
        .chain(path.iter().map(String::as_str))
        .chain([item])
        .enumerate()
    {
        if i > 0 {
            out.push_str("__");
        }
        escape_component(component, &mut out);
    }
    out
}

fn is_module_segment(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{}: module tree deeper than {MAX_DEPTH}",
            dir.display()
        ));
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // `metadata` follows symlinks: Bazel runfiles are symlink trees.
        let kind = std::fs::metadata(entry.path())
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if kind.is_dir() {
            walk(&entry.path(), depth + 1, out)?;
        } else if kind.is_file() && name.ends_with(".ves") {
            if out.len() >= MAX_FILES {
                return Err(format!("{}: more than {MAX_FILES} sources", dir.display()));
            }
            out.push(entry.path());
        }
    }
    Ok(())
}

impl Loader {
    /// Load one package (and, recursively, its dependencies). `stack` holds
    /// the canonical directories being loaded, for cycle detection.
    fn package(
        &mut self,
        dir: &Path,
        shown: PathBuf,
        stack: &mut Vec<(PathBuf, String)>,
    ) -> Result<Option<usize>, String> {
        let canonical = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if let Some(i) = stack.iter().position(|(p, _)| *p == canonical) {
            let mut chain: Vec<_> = stack[i..].iter().map(|(_, n)| n.clone()).collect();
            chain.push(stack[i].1.clone());
            self.manifest_errors.push((
                display(&shown.join("Loom.toml")),
                manifest_error(
                    CYCLE,
                    format!("package dependency cycle: {}", chain.join(" -> ")),
                ),
            ));
            return Ok(None);
        }
        if let Some(&i) = self.by_path.get(&canonical) {
            return Ok(Some(i));
        }
        if self.packages.len() >= MAX_PACKAGES {
            return Err(format!("more than {MAX_PACKAGES} packages"));
        }
        let manifest_path = dir.join("Loom.toml");
        let manifest_shown = display(&shown.join("Loom.toml"));
        let text = fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest = match vesper_loom_calculus::parse_manifest(&text) {
            Ok(m) => m,
            Err(errors) => {
                for e in errors {
                    self.manifest_errors.push((
                        manifest_shown.clone(),
                        manifest_error(INVALID_PACKAGE, format!("{}: {}", e.subject, e.message)),
                    ));
                }
                return Ok(None);
            }
        };
        let name = manifest.package.name.clone();
        if let Some(other) = self.packages.iter().find(|p| p.name == name) {
            self.manifest_errors.push((
                manifest_shown,
                manifest_error(
                    INVALID_PACKAGE,
                    format!(
                        "package {} is loaded from two directories ({} and {})",
                        name.as_str(),
                        display(&other.display),
                        display(&shown)
                    ),
                ),
            ));
            return Ok(None);
        }
        // Sources first, so the root package's root module is file 0.
        let mut files = vec![];
        let src = dir.join("src");
        if src.is_dir() {
            walk(&src, 0, &mut files)?;
        }
        let mut modules = vec![];
        let mut roots = vec![];
        for path in files {
            let relative = path.strip_prefix(&src).unwrap_or(&path).with_extension("");
            let mut segments: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let file_shown = display(&normalize(
                &shown
                    .join("src")
                    .join(path.strip_prefix(&src).unwrap_or(&path)),
            ));
            if segments.len() == 1 && matches!(segments[0].as_str(), "lib" | "main") {
                roots.push(file_shown.clone());
                segments.clear();
            }
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let file = self.add_file(file_shown.clone(), text);
            if let Some(bad) = segments.iter().find(|s| !is_module_segment(s)) {
                self.manifest_errors.push((
                    file_shown,
                    manifest_error(
                        INVALID_PACKAGE,
                        format!("module name '{bad}' must be a lower-case identifier"),
                    ),
                ));
                continue;
            }
            let parsed = crate::parser::parse(&self.sources.files[file].text);
            for d in parsed.diagnostics {
                self.diagnostics.push(PackageDiagnostic {
                    file: Some(file),
                    diagnostic: d,
                });
            }
            modules.push(ModuleSource {
                path: segments,
                file,
                parsed: parsed.module,
            });
        }
        if roots.len() > 1 {
            self.manifest_errors.push((
                manifest_shown.clone(),
                manifest_error(
                    INVALID_PACKAGE,
                    format!(
                        "a package has one root module, found {}",
                        roots.join(" and ")
                    ),
                ),
            ));
        }
        let index = self.packages.len();
        self.packages.push(Package {
            name: name.clone(),
            manifest: manifest.clone(),
            display: shown.clone(),
            deps: BTreeMap::new(),
            modules,
        });
        self.by_path.insert(canonical.clone(), index);
        stack.push((canonical, name.as_str().to_owned()));
        // Optional dependencies are enabled only through features, which
        // package checking does not select yet, so they are not loaded.
        for (dep_name, dep) in manifest.dependencies.iter().filter(|(_, d)| !d.optional) {
            let Some(path) = &dep.path else {
                self.manifest_errors.push((
                    manifest_shown.clone(),
                    manifest_error(
                        INVALID_PACKAGE,
                        format!(
                            "dependency {} has no `path`; registry dependencies are not supported yet",
                            dep_name.as_str()
                        ),
                    ),
                ));
                continue;
            };
            let dep_dir = dir.join(path);
            if !dep_dir.join("Loom.toml").is_file() {
                self.manifest_errors.push((
                    manifest_shown.clone(),
                    manifest_error(
                        INVALID_PACKAGE,
                        format!("dependency {}: no Loom.toml in '{path}'", dep_name.as_str()),
                    ),
                ));
                continue;
            }
            let Some(dep_index) = self.package(&dep_dir, normalize(&shown.join(path)), stack)?
            else {
                continue;
            };
            let found = &self.packages[dep_index];
            if found.name != *dep_name {
                self.manifest_errors.push((
                    manifest_shown.clone(),
                    manifest_error(
                        INVALID_PACKAGE,
                        format!(
                            "dependency {} at '{path}' is package {}",
                            dep_name.as_str(),
                            found.name.as_str()
                        ),
                    ),
                ));
                continue;
            }
            if !dep.version.matches(&found.manifest.package.version) {
                self.manifest_errors.push((
                    manifest_shown.clone(),
                    manifest_error(
                        INVALID_PACKAGE,
                        format!(
                            "dependency {} requires version {}, but '{path}' is {}",
                            dep_name.as_str(),
                            dep.version,
                            found.manifest.package.version
                        ),
                    ),
                ));
                continue;
            }
            self.packages[index]
                .deps
                .insert(dep_name.as_str().to_owned(), dep_index);
        }
        stack.pop();
        Ok(Some(index))
    }

    fn add_file(&mut self, display: String, text: String) -> usize {
        let base = self
            .sources
            .files
            .last()
            .map_or(0, |f| f.base + f.text.len() + 1);
        self.sources.files.push(SourceFile {
            display,
            text,
            base,
        });
        self.sources.files.len() - 1
    }

    fn error(&mut self, file: usize, code: &str, message: String, span: Span) {
        self.diagnostics.push(PackageDiagnostic {
            file: Some(file),
            diagnostic: Diagnostic::new(code, message, span),
        });
    }

    fn assemble(&mut self, root: usize) -> Module {
        // Top-level names of every module: (package, module path) -> names.
        let mut tables: BTreeMap<ModuleKey, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
        for (p, package) in self.packages.iter().enumerate() {
            for m in &package.modules {
                let names = m.parsed.items.iter().filter_map(item_name).collect();
                let public = m.parsed.public_items.iter().cloned().collect();
                tables.insert((p, m.path.clone()), (names, public));
            }
        }
        let root_items: BTreeSet<String> = tables
            .get(&(root, vec![]))
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        // Enum variants keep their names; record each one's owning enum so a
        // module may use it only when it declares or imports that enum.
        let mut variants: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (p, package) in self.packages.iter().enumerate() {
            for m in &package.modules {
                for item in &m.parsed.items {
                    if let Item::Adt(a) = item
                        && a.is_enum
                    {
                        let owner = self.mangle_in(&self.packages, root, p, &m.path, &a.name);
                        let shown = self.qualified_in(&self.packages, p, &m.path, &a.name);
                        for v in &a.variants {
                            variants
                                .entry(v.name.clone())
                                .or_insert_with(|| (owner.clone(), shown.clone()));
                        }
                    }
                }
            }
        }
        let mut out = Module::default();
        // Variant and operation names stay global; record their owners.
        let mut globals: BTreeMap<String, (String, usize, Span)> = BTreeMap::new();
        let packages = std::mem::take(&mut self.packages);
        for (p, package) in packages.iter().enumerate() {
            for m in &package.modules {
                let file = m.file;
                let base = self.sources.files[file].base;
                // Names this module binds: its own items, then its imports.
                let mut names: BTreeMap<String, String> = BTreeMap::new();
                let own = &tables[&(p, m.path.clone())].0;
                for n in own {
                    let mangled = self.mangle_in(&packages, root, p, &m.path, n);
                    if mangled != *n {
                        self.sources
                            .qualified
                            .insert(mangled.clone(), self.qualified_in(&packages, p, &m.path, n));
                    }
                    names.insert(n.clone(), mangled);
                }
                for u in &m.parsed.uses {
                    let target_pkg = match &u.package {
                        None => p,
                        Some(dep) => match package.deps.get(dep) {
                            Some(&d) => d,
                            None => {
                                let optional = package
                                    .manifest
                                    .dependencies
                                    .iter()
                                    .any(|(n, d)| n.as_str() == dep && d.optional);
                                self.error(
                                    file,
                                    UNKNOWN_IMPORT,
                                    if optional {
                                        format!(
                                            "{dep} is an optional dependency of {}, and package checking does not enable optional dependencies yet",
                                            package.name.as_str()
                                        )
                                    } else {
                                        format!(
                                            "{dep} is not a dependency of {}; add it to [dependencies] in Loom.toml",
                                            package.name.as_str()
                                        )
                                    },
                                    u.span.clone(),
                                );
                                continue;
                            }
                        },
                    };
                    let shown_pkg = if u.package.is_some() {
                        packages[target_pkg].name.as_str().to_owned()
                    } else {
                        "self".to_owned()
                    };
                    let module_shown = std::iter::once(shown_pkg)
                        .chain(u.path.iter().cloned())
                        .collect::<Vec<_>>()
                        .join("::");
                    let Some((items, public)) = tables.get(&(target_pkg, u.path.clone())) else {
                        self.error(
                            file,
                            UNKNOWN_IMPORT,
                            format!("no module {module_shown}"),
                            u.span.clone(),
                        );
                        continue;
                    };
                    let same_module = target_pkg == p && u.path == m.path;
                    for n in &u.names {
                        if !items.contains(&n.name) {
                            self.error(
                                file,
                                UNKNOWN_IMPORT,
                                format!("module {module_shown} has no item '{}'", n.name),
                                n.span.clone(),
                            );
                            continue;
                        }
                        if !same_module && !public.contains(&n.name) {
                            self.diagnostics.push(PackageDiagnostic {
                                file: Some(file),
                                diagnostic: Diagnostic::new(
                                    PRIVATE_ITEM,
                                    format!("'{}' is private to module {module_shown}", n.name),
                                    n.span.clone(),
                                )
                                .with_help("mark the item `pub` to export it"),
                            });
                            continue;
                        }
                        let target = self.mangle_in(&packages, root, target_pkg, &u.path, &n.name);
                        let local = n.local().to_owned();
                        if own.contains(&local) {
                            self.error(
                                file,
                                AMBIGUOUS,
                                format!("'{local}' is both declared in this module and imported"),
                                n.span.clone(),
                            );
                            continue;
                        }
                        match names.get(&local) {
                            Some(existing) if *existing != target => {
                                self.error(
                                    file,
                                    AMBIGUOUS,
                                    format!("'{local}' is imported twice from different items"),
                                    n.span.clone(),
                                );
                            }
                            _ => {
                                names.insert(local, target);
                            }
                        }
                    }
                }
                let is_root_module = p == root && m.path.is_empty();
                let mut renamer = Renamer {
                    names: &names,
                    hidden: if is_root_module {
                        None
                    } else {
                        Some(&root_items)
                    },
                    variants: &variants,
                    shift: base,
                    values: vec![],
                    types: vec![],
                    diagnostics: vec![],
                };
                for item in &m.parsed.items {
                    let mut item = item.clone();
                    renamer.item(&mut item);
                    for (name, span) in global_names(&item) {
                        if let Some((owner, other_file, _)) = globals.get(&name) {
                            if *other_file != file {
                                renamer.diagnostics.push(Diagnostic::new(
                                    AMBIGUOUS,
                                    format!(
                                        "'{name}' is also declared in {owner}; enum variants, effect operations, traits and models share one namespace"
                                    ),
                                    span.start - base..span.end - base,
                                ));
                            }
                        } else {
                            globals.insert(
                                name,
                                (self.sources.files[file].display.clone(), file, span),
                            );
                        }
                    }
                    out.items.push(item);
                }
                // Trait bounds and `use model` scopes are side tables keyed by
                // owner name and span: rename the owner and shift the spans.
                // Trait and model names stay global, like enum variants.
                // A bound's arguments see its owner's generic parameters.
                let scopes = generic_scopes(&m.parsed.items);
                for b in &m.parsed.trait_bounds {
                    let mut b = b.clone();
                    let mark = renamer.types.len();
                    if let Some(params) = scopes.get(&b.owner) {
                        renamer.types.extend(params.iter().cloned());
                    }
                    b.owner = renamer.owner(&b.owner);
                    renamer.trait_ref(&mut b.trait_ref);
                    renamer.types.truncate(mark);
                    renamer.span(&mut b.span);
                    out.trait_bounds.push(b);
                }
                for u in &m.parsed.model_uses {
                    let mut u = u.clone();
                    renamer.span(&mut u.span);
                    renamer.span(&mut u.scope);
                    out.model_uses.push(u);
                }
                for d in renamer.diagnostics {
                    self.diagnostics.push(PackageDiagnostic {
                        file: Some(file),
                        diagnostic: d,
                    });
                }
                out.public_items.extend(
                    m.parsed
                        .public_items
                        .iter()
                        .filter_map(|n| names.get(n).cloned()),
                );
            }
        }
        self.packages = packages;
        out
    }
    fn mangle_in(
        &self,
        packages: &[Package],
        root: usize,
        pkg: usize,
        path: &[String],
        item: &str,
    ) -> String {
        if pkg == root && path.is_empty() {
            return item.to_owned();
        }
        mangled(packages[pkg].name.as_str(), path, item)
    }
    fn qualified_in(
        &self,
        packages: &[Package],
        pkg: usize,
        path: &[String],
        item: &str,
    ) -> String {
        let mut out = packages[pkg].name.as_str().to_owned();
        for segment in path {
            out.push_str("::");
            out.push_str(segment);
        }
        out.push_str("::");
        out.push_str(item);
        out
    }
}

/// The importable name an item declares.
fn item_name(item: &Item) -> Option<String> {
    match item {
        Item::Adt(a) => Some(a.name.clone()),
        Item::Function(f) => Some(f.name.clone()),
        Item::Effect(e) => Some(e.name.clone()),
        Item::Impl { .. } | Item::Trait(_) | Item::Conformance(_) => None,
    }
}

/// The generic parameters in scope for each trait-bound owner of a module,
/// keyed as the parser writes `TraitBoundSyntax::owner`: a function `f`, an
/// inherent method `Type::m` (the impl's parameters, then the method's) or a
/// type `Type`.
fn generic_scopes(items: &[Item]) -> BTreeMap<String, Vec<String>> {
    let names = |gs: &[Generic]| gs.iter().map(|g| g.name.clone()).collect::<Vec<_>>();
    let mut out = BTreeMap::new();
    for item in items {
        match item {
            Item::Function(f) => {
                out.insert(f.name.clone(), names(&f.generics));
            }
            Item::Adt(a) => {
                out.insert(a.name.clone(), names(&a.generics));
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                for m in methods {
                    let mut params = names(generics);
                    params.extend(names(&m.generics));
                    out.insert(format!("{}::{}", ty.name(), m.name), params);
                }
            }
            Item::Effect(_) | Item::Trait(_) | Item::Conformance(_) => {}
        }
    }
    out
}

/// Enum variants and effect operations: names that stay global.
fn global_names(item: &Item) -> Vec<(String, Span)> {
    match item {
        Item::Adt(a) if a.is_enum => a
            .variants
            .iter()
            .map(|v| (v.name.clone(), a.span.clone()))
            .collect(),
        Item::Effect(e) => e
            .ops
            .iter()
            .map(|o| (o.name.clone(), o.span.clone()))
            .collect(),
        Item::Trait(t) => vec![(t.name.clone(), t.span.clone())],
        Item::Conformance(Conformance {
            kind: ConformanceKind::Model { name, .. },
            span,
            ..
        }) => vec![(name.clone(), span.clone())],
        _ => vec![],
    }
}

/// Renames a module's references to their internal names and shifts its
/// spans to the module's base offset.
struct Renamer<'a> {
    /// Names bound by the module: own items and imports.
    names: &'a BTreeMap<String, String>,
    /// Root-module names this module must not see unimported.
    hidden: Option<&'a BTreeSet<String>>,
    /// Enum variant -> (owning enum's internal name, its display name).
    variants: &'a BTreeMap<String, (String, String)>,
    shift: usize,
    /// Value bindings in scope.
    values: Vec<String>,
    /// Type and row parameters in scope.
    types: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}
impl Renamer<'_> {
    fn span(&self, s: &mut Span) {
        *s = s.start + self.shift..s.end + self.shift;
    }
    /// A free name: imported/own item, else left for the checker. A root
    /// module item reached without an import is reported (local span).
    fn resolve(&mut self, n: &str, span: &Span) -> Option<String> {
        if let Some(t) = self.names.get(n) {
            return Some(t.clone());
        }
        if let Some((owner, shown)) = self.variants.get(n)
            && !self.names.values().any(|t| t == owner)
        {
            self.diagnostics.push(
                Diagnostic::new(
                    UNKNOWN_IMPORT,
                    format!("variant '{n}' belongs to enum {shown}, which is not declared or imported in this module"),
                    span.clone(),
                )
                .with_help("import the enum to use its variants"),
            );
            return None;
        }
        if self.hidden.is_some_and(|h| h.contains(n)) {
            self.diagnostics.push(
                Diagnostic::new(
                    UNKNOWN_IMPORT,
                    format!("'{n}' is not declared or imported in this module"),
                    span.clone(),
                )
                .with_help(format!(
                    "items of other modules need an import such as `use self::{n};`"
                )),
            );
        }
        None
    }
    /// A path name `Head::rest` or plain type/effect name.
    fn type_name(&mut self, n: &mut String, span: &Span) {
        let (head, rest) = match n.split_once("::") {
            Some((h, r)) => (h.to_owned(), Some(r.to_owned())),
            None => (n.clone(), None),
        };
        if self.types.contains(&head) {
            return;
        }
        if let Some(t) = self.resolve(&head, span) {
            *n = match rest {
                Some(r) => format!("{t}::{r}"),
                None => t,
            };
        }
    }
    fn value_name(&mut self, n: &mut String, span: &Span) {
        if n.contains("::") {
            self.type_name(n, span);
        } else if !self.values.contains(n)
            && let Some(t) = self.resolve(n, span)
        {
            *n = t;
        }
    }
    fn ty(&mut self, t: &mut Type, span: &Span) {
        match t {
            Type::Named(n, args) => {
                if args.is_empty() && self.types.contains(n) {
                    return;
                }
                self.type_name(n, span);
                for a in args {
                    self.ty(a, span);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.ty(t, span);
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for (_, p) in params {
                    self.ty(p, span);
                }
                self.ty(result, span);
                self.effects(effects, span);
            }
            Type::Row(e) => self.effects(e, span),
            Type::IntLiteral(_) | Type::Infer => {}
        }
    }
    fn effects(&mut self, e: &mut Effects, span: &Span) {
        for i in &mut e.instances {
            self.effect(i, span);
        }
    }
    fn effect(&mut self, e: &mut Effect, span: &Span) {
        if !self.types.contains(&e.name) {
            self.type_name(&mut e.name, span);
        }
        for a in &mut e.args {
            self.ty(a, span);
        }
    }
    /// Capability labels of an effect row (`eff [c: Counter]`) are value
    /// names for operation receivers (`c.next()`), never item references.
    fn labels(&mut self, e: &Effects) {
        self.values
            .extend(e.instances.iter().filter_map(|i| i.label.clone()));
    }
    fn generics(&mut self, gs: &[Generic]) {
        self.types.extend(gs.iter().map(|g| g.name.clone()));
    }
    fn item(&mut self, item: &mut Item) {
        match item {
            Item::Adt(a) => {
                let span = a.span.clone();
                let old = a.name.clone();
                if let Some(t) = self.names.get(&old) {
                    a.name = t.clone();
                }
                let mark = self.types.len();
                self.generics(&a.generics);
                for v in &mut a.variants {
                    if !a.is_enum && v.name == old {
                        v.name = a.name.clone();
                    }
                    for f in &mut v.fields {
                        self.ty(&mut f.ty, &span);
                    }
                }
                self.types.truncate(mark);
                self.span(&mut a.span);
            }
            Item::Function(f) => {
                if let Some(t) = self.names.get(&f.name) {
                    f.name = t.clone();
                }
                self.function(f, &[]);
            }
            Item::Effect(e) => {
                if let Some(t) = self.names.get(&e.name) {
                    e.name = t.clone();
                }
                let mark = self.types.len();
                self.generics(&e.generics);
                for op in &mut e.ops {
                    self.function(op, &[]);
                }
                self.types.truncate(mark);
                self.span(&mut e.span);
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                let mark = self.types.len();
                self.generics(generics);
                let span = methods.first().map(|m| m.span.clone()).unwrap_or(0..0);
                self.ty(ty, &span);
                let outer = generics.clone();
                for m in methods {
                    self.function(m, &outer);
                }
                self.types.truncate(mark);
            }
            // Trait and model names are global; their signatures, heads and
            // method bodies refer to items like any other code.
            Item::Trait(t) => {
                let mark = self.types.len();
                self.types.push("Self".into());
                self.generics(&t.generics);
                self.types.extend(t.assoc.iter().map(|a| a.name.clone()));
                for m in &mut t.methods {
                    self.function(m, &[]);
                }
                for a in &mut t.assoc {
                    self.span(&mut a.span);
                }
                self.types.truncate(mark);
                self.span(&mut t.span);
            }
            Item::Conformance(c) => {
                let mark = self.types.len();
                self.types.push("Self".into());
                self.generics(&c.generics);
                let span = c.span.clone();
                self.trait_ref(&mut c.trait_ref);
                self.ty(&mut c.self_ty, &span);
                for w in &mut c.where_clauses {
                    self.ty(&mut w.ty, &span);
                    self.trait_ref(&mut w.trait_ref);
                    self.span(&mut w.span);
                }
                for a in &mut c.assoc {
                    self.ty(&mut a.ty, &span);
                    self.span(&mut a.span);
                }
                for m in &mut c.methods {
                    self.function(m, &[]);
                }
                self.types.truncate(mark);
                self.span(&mut c.span);
            }
        }
    }
    /// A trait reference: the trait name is global, its arguments are types.
    fn trait_ref(&mut self, t: &mut TraitRef) {
        let span = t.span.clone();
        for a in &mut t.args {
            self.ty(a, &span);
        }
        self.span(&mut t.span);
    }
    /// The owner of a trait bound: `f`, `Type::method` or `Type`.
    fn owner(&self, owner: &str) -> String {
        let (head, rest) = match owner.split_once("::") {
            Some((h, r)) => (h, Some(r)),
            None => (owner, None),
        };
        let Some(t) = self.names.get(head) else {
            return owner.to_owned();
        };
        match rest {
            Some(r) => format!("{t}::{r}"),
            None => t.clone(),
        }
    }
    fn function(&mut self, f: &mut Function, outer: &[Generic]) {
        let span = f.span.clone();
        let (tmark, vmark) = (self.types.len(), self.values.len());
        self.generics(outer);
        self.generics(&f.generics);
        for p in &mut f.params {
            self.ty(&mut p.ty, &p.span.clone());
            self.values.push(p.name.clone());
            self.span(&mut p.span);
        }
        self.ty(&mut f.result, &span);
        self.effects(&mut f.effects, &span);
        self.labels(&f.effects);
        if let Some(body) = &mut f.body {
            self.expr(body);
        }
        self.types.truncate(tmark);
        self.values.truncate(vmark);
        self.span(&mut f.span);
    }
    /// Rename a pattern and bind its variables (the caller restores scope).
    fn pattern(&mut self, p: &mut Pattern) {
        let span = p.span.clone();
        match &mut p.kind {
            PatternKind::Wild | PatternKind::Literal(_) => {}
            PatternKind::Bind(n) => self.values.push(n.clone()),
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.pattern(p);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                self.type_name(name, &span);
                for (_, p) in fields {
                    self.pattern(p);
                }
            }
        }
        self.span(&mut p.span);
    }
    fn clause(&mut self, c: &mut Clause) {
        let mark = self.values.len();
        for p in &mut c.pats {
            self.pattern(p);
        }
        self.expr(&mut c.body);
        self.values.truncate(mark);
    }
    fn expr(&mut self, e: &mut Expr) {
        let span = e.span.clone();
        match &mut e.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Name(n) => self.value_name(n, &span),
            ExprKind::Tuple(es) => {
                for e in es {
                    self.expr(e);
                }
            }
            ExprKind::Struct { name, fields } => {
                self.type_name(name, &span);
                for (_, e) in fields {
                    self.expr(e);
                }
            }
            ExprKind::Block { statements, tail } => {
                let mark = self.values.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            self.expr(value);
                            self.ty(ty, &pat.span.clone());
                            self.pattern(pat);
                        }
                        Statement::Expr(e) => self.expr(e),
                    }
                }
                if let Some(t) = tail {
                    self.expr(t);
                }
                self.values.truncate(mark);
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                self.expr(callee);
                for t in types {
                    self.ty(t, &span);
                }
                for a in args {
                    self.expr(&mut a.expr);
                }
            }
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => self.expr(e),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition);
                self.expr(then);
                if let Some(o) = otherwise {
                    self.expr(o);
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value);
                for (p, e) in arms {
                    let mark = self.values.len();
                    self.pattern(p);
                    self.expr(e);
                    self.values.truncate(mark);
                }
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                let mark = self.values.len();
                for p in params.iter_mut() {
                    self.ty(&mut p.ty, &p.span.clone());
                    self.values.push(p.name.clone());
                    self.span(&mut p.span);
                }
                self.ty(result, &span);
                self.effects(effects, &span);
                self.labels(effects);
                self.expr(body);
                self.values.truncate(mark);
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                self.effect(effect, &span);
                let mark = self.values.len();
                // A capability label (`handle c: E`) names the handler.
                if let Some(label) = &effect.label {
                    self.values.push(label.clone());
                }
                // The state pattern binds only through the clause patterns;
                // the body is checked in the outer environment.
                if let Some((pat, init)) = state {
                    self.expr(init);
                    let state_mark = self.values.len();
                    self.pattern(pat);
                    self.values.truncate(state_mark);
                }
                self.expr(body);
                self.clause(ret);
                for c in ops {
                    self.clause(c);
                }
                self.values.truncate(mark);
            }
            ExprKind::Loop(body) => self.expr(body),
            ExprKind::For { pat, iter, body } => {
                // The iterable is evaluated outside the pattern's scope.
                self.expr(iter);
                let mark = self.values.len();
                self.pattern(pat);
                self.expr(body);
                self.values.truncate(mark);
            }
            ExprKind::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            ExprKind::Break(value) | ExprKind::Return(value) => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
        }
        self.span(&mut e.span);
    }
}

#[cfg(test)]
mod tests {
    use super::mangled;

    #[test]
    fn mangling_is_injective_across_separator_characters() {
        let s = |v: &[&str]| v.iter().map(|c| (*c).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            mangled("@demo/geometry", &s(&["util"]), "abs"),
            "demo__geometry__util__abs"
        );
        assert_ne!(
            mangled("@foo-bar/baz", &[], "x"),
            mangled("@foo/bar-baz", &[], "x")
        );
        assert_ne!(
            mangled("@a/b", &s(&["c__d"]), "x"),
            mangled("@a/b", &s(&["c", "d"]), "x")
        );
        assert_ne!(
            mangled("@a/b", &[], "c__x"),
            mangled("@a/b", &s(&["c"]), "x")
        );
        assert_ne!(
            mangled("@a/b_c", &[], "x"),
            mangled("@a/b", &s(&["c"]), "x")
        );
    }
}
