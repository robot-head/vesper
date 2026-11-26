//! Self-hosting SH1 lane 14 (#1157): the sugar expansion swap. The
//! module's `expand_words` (modules/sugar, linked by build.rs), with its
//! input written by `sugar_words::input` and its words read back by
//! `sugar_words::decode`, must return exactly what
//! `vesper_syntax::sugar_expand::rust_expand_sugar` returns: the expanded
//! source, its effect regions with their import origins, the source map,
//! or the same diagnostic.
//!
//! The corpus is every repository `.ves` file and the source strings of the
//! sugar test suites, each with and without test items, and each program
//! with sugar declarations again with them moved into imports: visible
//! under their names, under aliases, and behind one definition scope that
//! every import shares (the same slice, as src/packages/sugar.rs passes
//! them). Sources with surface macros stay on Rust in the swap (part 6), so
//! they are left out here. Under the `divergent-sugar` feature every source
//! reads back as having no sugar, and the test must see differences.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/sugar.rs"]
mod binding;

#[path = "../src/sugar_words.rs"]
mod sugar_words;

#[path = "../../../tests/common/rust_literals.rs"]
mod rust_literals;

use std::path::{Path, PathBuf};
use vesper_syntax::{parser, sugar_expand};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A source with the caller's imports, as
/// `expand_sugar_with_scopes_for_execution` takes them.
#[derive(Clone, Debug, Default)]
struct Program {
    source: String,
    imports: Vec<(String, String)>,
    /// One shared definition scope for every import (empty: none).
    shared: Vec<(String, usize)>,
    scoped: bool,
    visible: Vec<usize>,
}

impl Program {
    fn plain(source: &str) -> Self {
        Program {
            source: source.to_owned(),
            ..Default::default()
        }
    }
}

/// The first difference between Rust and the module on `p`, if any.
fn compare(p: &Program, include_tests: bool) -> Option<String> {
    let imports: Vec<(&str, &str)> = p
        .imports
        .iter()
        .map(|(a, d)| (a.as_str(), d.as_str()))
        .collect();
    let scopes: Vec<&[(String, usize)]> = if p.scoped {
        vec![p.shared.as_slice(); p.imports.len()]
    } else {
        vec![]
    };
    let rust =
        sugar_expand::rust_expand_sugar(&p.source, &imports, &scopes, &p.visible, include_tests);
    let input = sugar_words::input(&p.source, &imports, &scopes, &p.visible)
        .expect("corpus lengths fit the input");
    let vesper = sugar_words::decode(&binding::expand_words(&input, u64::from(include_tests)));
    (rust != vesper).then(|| format!("rust {rust:?}\nvesper {vesper:?}"))
}

fn repository_sources() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(
                    name.to_str(),
                    Some("target" | ".git" | "node_modules" | "bazel-out")
                ) && !name.to_string_lossy().starts_with("bazel-")
                {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut paths = vec![];
    walk(&root(), &mut paths);
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// Files whose string literals hold sugar declarations and uses.
const SUGAR_FILES: &[&str] = &[
    "crates/vesper-syntax/tests/sugar_expand.rs",
    "crates/vesper-syntax/tests/sugar_surface.rs",
    "crates/vesper-syntax/src/sugar_expand.rs",
    "src/forge_frontend/sugar.rs",
    "src/packages/sugar.rs",
    "tests/forge_cli.rs",
    "tests/package_paths.rs",
    "tests/self_host_front_v2.rs",
    "tests/self_host_sugar.rs",
];

fn suite_sources() -> Vec<String> {
    SUGAR_FILES
        .iter()
        .filter_map(|file| std::fs::read_to_string(root().join(file)).ok())
        .flat_map(|text| rust_literals::rust_string_literals(&text))
        .filter(|s| s.contains("sugar"))
        .collect()
}

/// The top-level sugar declarations of `source`: (start, end, name).
fn declarations(source: &str) -> Vec<(usize, usize, String)> {
    let Ok(parse) = parser::parse_surface_cst(source, Default::default(), true) else {
        return vec![];
    };
    parse
        .syntax()
        .descendants()
        .filter(|n| n.kind() == parser::SUGAR_DECL)
        .filter(|n| {
            !n.ancestors()
                .skip(1)
                .any(|a| a.kind() == parser::SUGAR_DECL)
        })
        .filter_map(|n| {
            let r = n.text_range();
            let (s, e) = (usize::from(r.start()), usize::from(r.end()));
            let text = &source[s..e];
            let at = text.find("sugar")? + "sugar".len();
            let name: String = text[at..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some((s, e, name))
        })
        .collect()
}

/// `source` with its declarations moved into imports, three ways.
fn imported(source: &str, out: &mut Vec<Program>) {
    let decls = declarations(source);
    if decls.is_empty() {
        return;
    }
    let mut rest = source.to_owned();
    for (s, e, _) in decls.iter().rev() {
        rest.replace_range(*s..*e, &" ".repeat(e - s));
    }
    let imports: Vec<(String, String)> = decls
        .iter()
        .map(|(s, e, name)| (name.clone(), source[*s..*e].to_owned()))
        .collect();
    let all: Vec<usize> = (0..imports.len()).collect();
    out.push(Program {
        source: rest.clone(),
        imports: imports.clone(),
        visible: all.clone(),
        ..Default::default()
    });
    let mut renamed = rest.clone();
    let mut aliased = imports.clone();
    for (alias, _) in aliased.iter_mut() {
        let new = format!("{alias}_alias");
        renamed = renamed.replace(alias.as_str(), &new);
        *alias = new;
    }
    out.push(Program {
        source: renamed,
        imports: aliased,
        visible: all,
        ..Default::default()
    });
    out.push(Program {
        source: rest,
        shared: decls
            .iter()
            .enumerate()
            .map(|(i, (_, _, name))| (name.clone(), i))
            .collect(),
        imports,
        scoped: true,
        visible: vec![0],
    });
}

fn corpus() -> Vec<Program> {
    let mut sources = repository_sources();
    sources.extend(suite_sources());
    sources.retain(|s| !s.contains("macro"));
    let mut out: Vec<Program> = sources.iter().map(|s| Program::plain(s)).collect();
    for s in &sources {
        imported(s, &mut out);
    }
    out
}

#[test]
fn expansion_matches_rust() {
    let programs = corpus();
    let mut failures = vec![];
    let mut checked = 0;
    for p in &programs {
        for include_tests in [true, false] {
            checked += 1;
            if let Some(d) = compare(p, include_tests) {
                failures.push(format!("{p:?} tests {include_tests}:\n{d}"));
            }
        }
    }
    eprintln!("{checked} expansions of {} programs", programs.len());
    assert!(checked > 1000, "{checked} expansions");
    if cfg!(feature = "divergent-sugar") {
        assert!(
            !failures.is_empty(),
            "the divergent sugar module agreed with Rust on every expansion"
        );
        eprintln!(
            "divergent sugar module caught on {} expansions",
            failures.len()
        );
        return;
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} expansions differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
