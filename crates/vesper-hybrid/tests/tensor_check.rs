//! The tensor type checker swap (SH2 lane 14, #1186) against Rust, as
//! `src/swap/tensor_types.rs` runs it: the module encoded
//! (`tensor_words::encode_tensor_module`), `tensor_diags` of the linked
//! Vesper module (`modules/tensor_types`), the diagnostics decoded, and
//! Rust's `analyze` for an active module the Vesper side accepts. Rust is
//! `tensor_types::check` with no table installed. The flip gate
//! (scripts/ci/flip-gate/tensor-check.json, #2207) runs `flip_gate`.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/tensor_types.rs"]
mod tensor_types_binding;

#[allow(dead_code)]
#[path = "../../../tests/common/tensor_words.rs"]
mod tensor_words;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vesper::ast::{Diagnostic, Module};
use vesper::tensor_types;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Every `.ves` file of the repository, sorted.
fn repository_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if !matches!(&*name, "target" | ".git" | "node_modules" | "bazel-out")
                    && !name.starts_with("bazel-")
                {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "ves") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    walk(&repository(), &mut out);
    out.sort();
    out
}

/// Every repository module, parsed, with its path.
fn modules() -> Vec<(PathBuf, Module)> {
    repository_sources()
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            Some((p, vesper::parser::parse(&text).module))
        })
        .collect()
}

/// What `tensor_types::check` returns, reduced to what the checker uses
/// from a rejected module: `None` (not a tensor module) or the
/// diagnostics (empty when accepted).
type Answer = Option<Vec<Diagnostic>>;

fn rust(m: &Module) -> Answer {
    tensor_types::check(m).map(|c| c.diagnostics)
}

/// `check` with the swap's table, as `src/swap/tensor_types.rs` fills it.
fn swap(m: &Module) -> Answer {
    let (names, bytes) = tensor_words::encode_tensor_module(m);
    let words = tensor_types_binding::tensor_diags(&names, &bytes);
    match tensor_words::decode_tensor_diags(&words) {
        None => None,
        Some(diags) if !diags.is_empty() => Some(diags),
        Some(_) => {
            if !tensor_types::active(m) {
                return None;
            }
            Some(tensor_types::analyze(m).checked.diagnostics)
        }
    }
}

fn show(a: &Answer) -> String {
    match a {
        None => "inactive".into(),
        Some(d) => d
            .iter()
            .map(|d| format!("{} {:?} {}", d.code, d.span, d.message))
            .collect::<Vec<_>>()
            .join("; "),
    }
}

#[test]
fn tensor_swap_matches_rust_on_the_repository_sources() {
    let all = modules();
    let mut differ = vec![];
    let mut active = 0;
    for (p, m) in &all {
        let (r, v) = (rust(m), swap(m));
        active += usize::from(r.is_some());
        if show(&r) != show(&v) {
            differ.push(format!(
                "{}:\n  rust   {}\n  vesper {}",
                p.display(),
                show(&r),
                show(&v)
            ));
        }
    }
    eprintln!("{} modules, {active} tensor modules", all.len());
    println!(
        "flip-gate differential differences={} cases={}",
        differ.len(),
        all.len()
    );
    assert!(active > 0, "no tensor module in the repository");
    assert!(
        differ.is_empty(),
        "{} of {} modules differ:\n{}",
        differ.len(),
        all.len(),
        differ.join("\n")
    );
}

fn time(f: impl FnOnce()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

/// ABBA rounds (`FLIP_GATE_EPOCHS`, default 12) of `check` over every
/// repository module, the checker's workload: `all` for every module,
/// `tensor` for the tensor modules alone.
#[test]
#[ignore = "timing; FLIP_GATE=1"]
fn flip_gate() {
    if std::env::var_os("FLIP_GATE").is_none() {
        return;
    }
    let epochs: usize = std::env::var("FLIP_GATE_EPOCHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let all: Vec<Module> = modules().into_iter().map(|(_, m)| m).collect();
    let tensor: Vec<Module> = all
        .iter()
        .filter(|m| tensor_types::active(m))
        .cloned()
        .collect();
    eprintln!("{} modules, {} tensor modules", all.len(), tensor.len());
    for (workload, set) in [("all", &all), ("tensor", &tensor)] {
        let r = || {
            time(|| {
                for m in set.iter() {
                    std::hint::black_box(rust(m));
                }
            })
        };
        let v = || {
            time(|| {
                for m in set.iter() {
                    std::hint::black_box(swap(m));
                }
            })
        };
        r();
        v();
        let mut pairs = String::new();
        for _ in 0..epochs {
            let (r1, v1, v2, r2) = (r(), v(), v(), r());
            pairs += &format!(" {}:{}", (r1 + r2).as_nanos(), (v1 + v2).as_nanos());
        }
        println!("flip-gate pairs {workload}{pairs}");
    }
}
