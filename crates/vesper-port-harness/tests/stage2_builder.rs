//! The Stage 2 port builder against `vesper build --lib` (bootstrap switch
//! slice 3, lane E, #2666): every library a port crate links
//! (tests/self_host_codegen_export/ports.rs) is built by the Rust compiler
//! and by `vesper-port-builder.wasm` under the pinned wasmtime CLI, as the
//! harness builds it, and the test counts, per output, the libraries whose
//! file is byte-identical, differs, or was declined (with why).
//!
//! An output the builder writes must never differ. The interface and
//! binding of [`MIN_EXPORTS`] libraries must come out identical. The port
//! builder makes @vesper/native-lib's request itself (slice 3g of #2659,
//! #2733); [`MIN_OBJECTS`] is the progress meter for the objects: raise it
//! as more of them come out identical.
//!
//! Inputs: [`stage2::BUILDER_ENV`] (the composed component, from
//! `//:vesper-port-builder.wasm`), the wasmtime CLI ([`stage2::wasmtime`])
//! and [`vesper_port_harness::COMPILER_ENV`] (the Rust compiler). Bazel
//! passes all three. Without the builder or the compiler, the test says so
//! and passes, unless it runs under Bazel. `STAGE2_BUILDER_PORTS` limits it
//! to a comma-separated list of library names.
#[allow(dead_code)]
#[path = "../../../tests/self_host_codegen_export/ports.rs"]
mod ports;

use ports::{PORTS, Port, port_input};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use vesper_port_harness::stage2;

/// Libraries whose interface and binding must be identical.
const MIN_EXPORTS: usize = 52;
/// Libraries whose object must be identical.
const MIN_OBJECTS: usize = 2;
/// The `.vsys` target every library is built for (an explicit triple with
/// no extra CPU features, so objects do not depend on the machine).
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// Why @vesper/export-build declines (`DECLINE_*`), by reason.
const EXPORT_DECLINES: &[&str] = &[
    "",
    "the package graph does not load in Stage 2",
    "a .vsys library does not read or scan",
    "directives Stage 2 cannot follow",
    "a Box type Stage 2 does not declare",
    "fip functions",
    "the planner refused it",
    "syntax or assembly errors",
    "a target without .vsys support",
];
/// Why @vesper/port-builder declines the object (`OBJECT_*`).
const OBJECT_DECLINES: &[&str] = &[
    "",
    "Stage 2's front end made no calls input",
    "native-lib answered with an error",
    "the interface was declined",
    "native-lib ran out of Stage 2's memory",
];

fn repo() -> PathBuf {
    // Cargo runs tests in the crate directory; Bazel in the package's
    // runfiles directory, whose parent holds the other packages.
    for base in [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        PathBuf::from("../.."),
    ] {
        if base.join("packages/vesper-port-builder/Loom.toml").exists() {
            return std::fs::canonicalize(base).unwrap();
        }
    }
    panic!("no repository root above the test")
}

fn selected(library: &str) -> bool {
    match std::env::var("STAGE2_BUILDER_PORTS") {
        Ok(list) if !list.is_empty() => list.split(',').any(|l| l.trim() == library),
        _ => true,
    }
}

fn bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

/// A path from the environment, absolute (Bazel gives runfiles paths).
fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// The three outputs of one build.
struct Outputs {
    object: PathBuf,
    interface: PathBuf,
    binding: PathBuf,
}

impl Outputs {
    fn under(dir: &Path, library: &str) -> Self {
        Self {
            object: dir.join(format!("{library}.o")),
            interface: dir.join(format!("{library}.json")),
            binding: dir.join(format!("{library}_binding.rs")),
        }
    }
}

fn rust_build(compiler: &Path, port: &Port, input: &Path, out: &Outputs) -> Result<(), String> {
    let (_, library, _, rust_types, _) = *port;
    let mut command = Command::new(compiler);
    command
        .args(["build", "--lib", "--lib-name", library, "--target", TRIPLE])
        .arg(input)
        .arg("-o")
        .arg(&out.object)
        .arg("--interface")
        .arg(&out.interface)
        .arg("--rust-binding")
        .arg(&out.binding);
    if let Some(types) = rust_types {
        command.args(["--rust-types", types]);
    }
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Identical, differing and declined counts of one output.
#[derive(Default)]
struct Tally {
    same: usize,
    differ: Vec<&'static str>,
    declined: BTreeMap<&'static str, Vec<&'static str>>,
}

impl Tally {
    fn compare(&mut self, library: &'static str, rust: &Path, stage2: &Path) {
        if std::fs::read(rust).ok() == std::fs::read(stage2).ok() {
            self.same += 1;
        } else {
            self.differ.push(library);
        }
    }

    fn decline(&mut self, library: &'static str, why: &'static str) {
        self.declined.entry(why).or_default().push(library);
    }

    fn report(&self, what: &str, total: usize) {
        eprintln!(
            "stage2 port builder {what}: {} identical, {} differ, {} declined of {total}",
            self.same,
            self.differ.len(),
            self.declined.values().map(Vec::len).sum::<usize>()
        );
        for (why, libraries) in &self.declined {
            eprintln!("  {} declined: {why}: {libraries:?}", libraries.len());
        }
        if !self.differ.is_empty() {
            eprintln!("  differ: {:?}", self.differ);
        }
    }
}

fn reason(table: &[&'static str], code: u64) -> &'static str {
    usize::try_from(code)
        .ok()
        .and_then(|i| table.get(i))
        .copied()
        .unwrap_or("an unknown reason")
}

#[test]
fn stage2_port_builder_matches_rust() {
    let builder = env_path(stage2::BUILDER_ENV);
    let compiler = env_path(vesper_port_harness::COMPILER_ENV);
    let (Some(builder), Some(compiler)) = (builder, compiler) else {
        assert!(
            !bazel(),
            "Bazel must pass {} and {}",
            stage2::BUILDER_ENV,
            vesper_port_harness::COMPILER_ENV
        );
        eprintln!(
            "{} or {} is not set; the Stage 2 port builder differential did not run",
            stage2::BUILDER_ENV,
            vesper_port_harness::COMPILER_ENV
        );
        return;
    };
    let wasmtime = stage2::wasmtime();
    let wasmtime = std::fs::canonicalize(&wasmtime).unwrap_or(wasmtime);
    let repo = repo();
    let scratch =
        std::env::temp_dir().join(format!("vesper-stage2-builder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let (mut interface, mut binding, mut object) =
        (Tally::default(), Tally::default(), Tally::default());
    let ports: Vec<&Port> = PORTS.iter().filter(|p| selected(p.1)).collect();
    for port in &ports {
        let (_, library, _, rust_types, _) = **port;
        let dir = scratch.join(library);
        std::fs::create_dir_all(dir.join("input")).unwrap();
        let input = port_input(port, &repo, &dir.join("input"))
            .unwrap_or_else(|e| panic!("{library}: {e}"));
        let rust = Outputs::under(&dir.join("rust"), library);
        let ours = Outputs::under(&dir.join("stage2"), library);
        for d in [dir.join("rust"), dir.join("stage2")] {
            std::fs::create_dir_all(d).unwrap();
        }
        rust_build(&compiler, port, &input, &rust)
            .unwrap_or_else(|e| panic!("{library}: the Rust compiler failed: {e}"));
        let request = stage2::Request {
            builder: &builder,
            wasmtime: &wasmtime,
            package: &input,
            library,
            target: TRIPLE,
            object: &ours.object,
            interface: &ours.interface,
            binding: &ours.binding,
            rust_types,
            scratch: &dir.join("scratch"),
        };
        let outcome = stage2::build(&request)
            .unwrap_or_else(|e| panic!("{library}: the port builder did not run: {e}"));
        if outcome.export == 0 {
            interface.compare(library, &rust.interface, &ours.interface);
            binding.compare(library, &rust.binding, &ours.binding);
        } else {
            let why = reason(EXPORT_DECLINES, outcome.export);
            interface.decline(library, why);
            binding.decline(library, why);
        }
        if outcome.object == 0 {
            object.compare(library, &rust.object, &ours.object);
        } else {
            object.decline(library, reason(OBJECT_DECLINES, outcome.object));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    interface.report("interfaces", ports.len());
    binding.report("bindings", ports.len());
    object.report("objects", ports.len());
    assert!(
        interface.differ.is_empty() && binding.differ.is_empty() && object.differ.is_empty(),
        "the port builder wrote outputs that differ from the Rust compiler's"
    );
    if ports.len() == PORTS.len() {
        let exports = interface.same.min(binding.same);
        assert!(
            exports >= MIN_EXPORTS,
            "only {exports} interfaces and bindings identical"
        );
        assert!(
            object.same >= MIN_OBJECTS,
            "only {} objects identical",
            object.same
        );
    }
}
