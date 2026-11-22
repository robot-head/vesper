//! The export ABI swap (self-hosting SH4 lane 12, #1286): the default
//! `vesper` and `vesper-hybrid`, whose `export` hook emits the export ABI
//! v2 glue in Vesper (modules/codegen_export), build every library a port
//! crate links (`PORTS`, the `Port::new` calls of crates/*/build.rs) with
//! `build --lib`, and the objects and interface files must be
//! byte-identical. The hybrid's `plan` and `interface_json` hooks plan and
//! write every interface in Vesper (modules/codegen_export), and its stats
//! show the hooks planned and wrote each library and emitted each v2
//! library's glue.
//!
//! `CODEGEN_EXPORT_PORTS=aura,warp` limits the corpus. The default
//! compiler is found as tests/differential.rs finds it.
#[allow(dead_code)]
#[path = "../../../tests/self_host_codegen_export/ports.rs"]
mod ports;

use ports::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

fn hybrid() -> PathBuf {
    absolute(PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid")))
}

fn default_compiler() -> PathBuf {
    for var in ["VESPER_DEFAULT_COMPILER", "VESPER_PORT_COMPILER"] {
        if let Some(path) = std::env::var_os(var) {
            let path = absolute(PathBuf::from(path));
            assert!(path.is_file(), "{var}={} is not a file", path.display());
            return path;
        }
    }
    let hybrid = hybrid();
    let target = hybrid
        .parent()
        .and_then(Path::parent)
        .expect("target directory");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .arg("--target-dir")
        .arg(target)
        .status()
        .expect("run cargo build --bin vesper");
    assert!(status.success(), "cargo build --bin vesper failed");
    target
        .join("debug")
        .join(format!("vesper{}", std::env::consts::EXE_SUFFIX))
}

fn selected(library: &str) -> bool {
    match std::env::var("CODEGEN_EXPORT_PORTS") {
        Ok(list) if !list.is_empty() => list.split(',').any(|l| l.trim() == library),
        _ => true,
    }
}

/// `build --lib` of `input` with `compiler`: the object bytes, the
/// interface file and stderr.
fn build(
    compiler: &Path,
    library: &str,
    input: &Path,
    out: &Path,
    optimize: bool,
) -> (Vec<u8>, Vec<u8>, String) {
    let json = out.with_extension("json");
    let mut c = Command::new(compiler);
    c.arg("build");
    if !optimize {
        c.arg("--no-opt");
    }
    let o = c
        .args(["--lib", "--lib-name", library])
        .arg(input)
        .arg("-o")
        .arg(out)
        .arg("--interface")
        .arg(&json)
        .env("VESPER_HYBRID_STATS", "1")
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{library}: {}", stderr);
    let bytes = std::fs::read(out).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    let interface = std::fs::read(&json).unwrap_or_else(|e| panic!("{}: {e}", json.display()));
    (bytes, interface, stderr)
}

/// The number after `prefix` on the hybrid's stats line that starts with it.
fn stat(stderr: &str, prefix: &str) -> u64 {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix(prefix)?.split(' ').next()?.parse().ok())
        .unwrap_or(0)
}

/// Libraries built with the optimizer as well (small ones; the glue does
/// not depend on it, but the module's flags do).
const OPTIMIZED: &[&str] = &["alloc", "semver", "wheel"];

/// What one library's builds found: objects compared, glues the hook
/// emitted, and the builds whose objects differ.
#[derive(Default)]
struct Outcome {
    builds: u64,
    swapped: u64,
    planned: u64,
    written: u64,
    differ: Vec<String>,
}

fn compare(port: &Port, repo: &Path, scratch: &Path, default: &Path, hybrid: &Path) -> Outcome {
    let library = port.1;
    let input = port_input(port, repo, scratch).unwrap_or_else(|e| panic!("{library}: {e}"));
    let mut out = Outcome::default();
    for optimize in [false, true] {
        if optimize && !OPTIMIZED.contains(&library) {
            continue;
        }
        let a = scratch.join(format!("{library}-{optimize}-rust.o"));
        let b = scratch.join(format!("{library}-{optimize}-hybrid.o"));
        let (rust, rust_json, _) = build(default, library, &input, &a, optimize);
        let (vesper, vesper_json, stderr) = build(hybrid, library, &input, &b, optimize);
        out.builds += 1;
        out.swapped += stat(
            &stderr,
            "vesper-hybrid: codegen emitted the export glue of ",
        );
        out.planned += stat(&stderr, "vesper-hybrid: codegen planned ");
        out.written += stat(&stderr, "vesper-hybrid: codegen wrote ");
        if rust != vesper {
            out.differ.push(format!("{library} (optimize {optimize})"));
        }
        if rust_json != vesper_json {
            out.differ
                .push(format!("{library} interface (optimize {optimize})"));
        }
    }
    out
}

#[test]
fn export_objects_and_interfaces_match_rust() {
    let repo = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-export-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory");
    let ports: Vec<&Port> = PORTS.iter().filter(|p| selected(p.1)).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let total = std::sync::Mutex::new(Outcome::default());
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(port) = ports.get(i) else { break };
                    let o = compare(port, &repo, &scratch, &default, &hybrid);
                    let mut t = total.lock().unwrap();
                    t.builds += o.builds;
                    t.swapped += o.swapped;
                    t.planned += o.planned;
                    t.written += o.written;
                    t.differ.extend(o.differ);
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let mut t = total.into_inner().unwrap();
    t.differ.sort();
    eprintln!(
        "export: {} of {} objects and interfaces differ over {} libraries; the hooks planned {} interfaces, wrote {} interface files and emitted {} glues",
        t.differ.len(),
        t.builds,
        ports.len(),
        t.planned,
        t.written,
        t.swapped
    );
    assert!(t.differ.is_empty(), "objects differ: {:?}", t.differ);
    assert!(t.swapped > 0, "the export hook never ran");
    assert_eq!(t.planned, t.builds, "the plan hook missed a build");
    assert_eq!(
        t.written, t.builds,
        "the interface_json hook missed a build"
    );
}
