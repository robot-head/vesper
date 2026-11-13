//! Package-root and source discovery for path-less `vesper lint` and
//! `vesper test --doc`.
//!
//! The package root is the nearest ancestor (including the start directory)
//! holding a regular `Loom.toml`, else the start directory itself. Sources are
//! the `.ves` files below that root, walked in sorted byte order without
//! following symlinks. Hidden entries, `target/`, `bazel-*` output trees and
//! nested packages (subdirectories with their own `Loom.toml`) are skipped.
//! Every walk is bounded; exceeding a bound is an error, never a silent cut.
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Walk bounds. Limits may be lowered for tests, never raised above defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLimits {
    pub max_files: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_ancestors: usize,
}
impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_files: 1024,
            max_entries: 65_536,
            max_depth: 64,
            max_ancestors: 256,
        }
    }
}
impl SourceLimits {
    fn validate(self) -> Result<Self, String> {
        let hard = Self::default();
        for (value, cap) in [
            (self.max_files, hard.max_files),
            (self.max_entries, hard.max_entries),
            (self.max_depth, hard.max_depth),
            (self.max_ancestors, hard.max_ancestors),
        ] {
            if value == 0 || value > cap {
                return Err("invalid source discovery limits".into());
            }
        }
        Ok(self)
    }
}

/// The discovered package and its sources, relative paths in sorted order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageSources {
    /// Absolute package root.
    pub root: PathBuf,
    /// The `Loom.toml` that selected the root, if any.
    pub manifest: Option<PathBuf>,
    /// Absolute `.ves` paths, sorted by their root-relative path.
    pub files: Vec<PathBuf>,
}
impl PackageSources {
    /// A root-relative display path using `/` separators.
    pub fn display(&self, file: &Path) -> String {
        file.strip_prefix(&self.root)
            .unwrap_or(file)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}

fn is_manifest(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join("Loom.toml")).is_ok_and(|m| m.is_file())
}

/// Nearest ancestor of `start` (inclusive) with a regular `Loom.toml`, else
/// `start`. `start` must be an existing directory.
pub fn package_root(
    start: &Path,
    limits: SourceLimits,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let limits = limits.validate()?;
    // Canonical so `..` components cannot make ancestors skip a manifest.
    let start = fs::canonicalize(start).map_err(|e| format!("{}: {e}", start.display()))?;
    if !fs::metadata(&start)
        .map_err(|e| format!("{}: {e}", start.display()))?
        .is_dir()
    {
        return Err(format!("{} is not a directory", start.display()));
    }
    for (count, dir) in start.ancestors().enumerate() {
        if count >= limits.max_ancestors {
            break;
        }
        if is_manifest(dir) {
            return Ok((dir.to_path_buf(), Some(dir.join("Loom.toml"))));
        }
    }
    Ok((start, None))
}

fn skipped(name: &str) -> bool {
    name.starts_with('.') || name == "target" || name.starts_with("bazel-")
}

/// Discover the package containing `start` and every `.ves` source in it.
/// Finding no sources is an error: a vacuous pass would hide a wrong root.
pub fn discover(start: &Path, limits: SourceLimits) -> Result<PackageSources, String> {
    let limits = limits.validate()?;
    let (root, manifest) = package_root(start, limits)?;
    let mut files = Vec::new();
    let mut entries = 0usize;
    // Depth-first in sorted order; the explicit stack keeps recursion bounded.
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(format!(
                "source discovery exceeds {} directory levels under {}",
                limits.max_depth,
                root.display()
            ));
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            entries += 1;
            if entries > limits.max_entries {
                return Err(format!(
                    "source discovery exceeds {} directory entries under {}",
                    limits.max_entries,
                    root.display()
                ));
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(format!(
                    "non-UTF-8 path under {}; rename it or pass explicit files",
                    dir.display()
                ));
            };
            if skipped(name) {
                continue;
            }
            // symlink_metadata: never follow links out of the package.
            let kind = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("{}: {e}", entry.path().display()))?
                .file_type();
            children.push((name.to_string(), kind));
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let mut subdirectories = Vec::new();
        for (name, kind) in children {
            let path = dir.join(&name);
            if kind.is_dir() {
                if !is_manifest(&path) {
                    subdirectories.push(path);
                }
            } else if kind.is_file() && name.ends_with(".ves") {
                files.push(path);
                if files.len() > limits.max_files {
                    return Err(format!(
                        "package {} has more than {} .ves sources",
                        root.display(),
                        limits.max_files
                    ));
                }
            }
        }
        for path in subdirectories.into_iter().rev() {
            stack.push((path, depth + 1));
        }
    }
    files.sort_by(|a, b| {
        a.strip_prefix(&root)
            .unwrap_or(a)
            .cmp(b.strip_prefix(&root).unwrap_or(b))
    });
    if files.is_empty() {
        return Err(format!(
            "no .ves sources found under package root {}",
            root.display()
        ));
    }
    Ok(PackageSources {
        root,
        manifest,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-sources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn nearest_manifest_selects_root_and_skips_outputs_and_nested_packages() {
        let dir = scratch();
        fs::write(dir.join("Loom.toml"), "[package]\n").unwrap();
        for path in [
            "src/b.ves",
            "src/a.ves",
            "src/deep/c.ves",
            "target/out.ves",
            ".hidden/h.ves",
            "bazel-out/x.ves",
            "nested/Loom.toml",
            "nested/n.ves",
            "notes.txt",
        ] {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let found = discover(&dir.join("src/deep"), SourceLimits::default()).unwrap();
        assert_eq!(found.root, dir);
        assert_eq!(found.manifest, Some(dir.join("Loom.toml")));
        let names: Vec<_> = found.files.iter().map(|f| found.display(f)).collect();
        assert_eq!(names, ["src/a.ves", "src/b.ves", "src/deep/c.ves"]);
        // The nested package is its own root.
        let nested = discover(&dir.join("nested"), SourceLimits::default()).unwrap();
        assert_eq!(nested.root, dir.join("nested"));
        assert_eq!(nested.files.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_manifest_uses_start_and_empty_or_oversized_trees_are_errors() {
        let dir = scratch();
        let err = discover(&dir, SourceLimits::default()).unwrap_err();
        assert!(err.contains("no .ves sources"), "{err}");
        for name in ["a.ves", "b.ves", "c.ves"] {
            fs::write(dir.join(name), "").unwrap();
        }
        let found = discover(&dir, SourceLimits::default()).unwrap();
        assert_eq!(found.manifest, None);
        assert_eq!(found.files.len(), 3);
        let limits = SourceLimits {
            max_files: 2,
            ..Default::default()
        };
        assert!(discover(&dir, limits).unwrap_err().contains("more than 2"));
        assert!(
            discover(
                &dir,
                SourceLimits {
                    max_files: 0,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(discover(&dir.join("a.ves"), SourceLimits::default()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let dir = scratch();
        let outside = scratch();
        fs::write(outside.join("escape.ves"), "").unwrap();
        fs::write(dir.join("real.ves"), "").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("escape.ves"), dir.join("l.ves")).unwrap();
        let found = discover(&dir, SourceLimits::default()).unwrap();
        assert_eq!(found.files, [dir.join("real.ves")]);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
