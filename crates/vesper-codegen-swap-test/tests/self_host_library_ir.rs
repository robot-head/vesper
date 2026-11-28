//! Bootstrap switch slice 3 (#2662, part of #2659): the IR pipeline that
//! `vesper build --lib` runs on a library's first lowering
//! (`native_export::library_program`), in Vesper (packages/vesper-library-ir).
//!
//! The package takes the stage to stop after, the optimize flag and
//! @vesper/codegen-calls' calls input of the first lowering (what the
//! `export_plans` hook sees), and returns the program as `put_program`
//! words. At every stage it has ported, the program read back must be
//! `Debug`-equal to Rust's after `library_program` through that stage, and
//! a Rust error must be a refusal.
//!
//! - `library_programs_match_rust`: every library of
//!   tests/common/native_lib_fixtures.rs (the @vesper/native-lib fixtures),
//!   built optimized and unoptimized; the first lowering is recorded by an
//!   `export_plans` hook during `compile_source`.
//! - `port_package_programs_match_rust`: the first lowering of the hybrid's
//!   port libraries (crates/vesper-hybrid/modules), every
//!   `LIBRARY_IR_MODULE_STEP`-th one (default [`DEFAULT_MODULE_STEP`]).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;
#[path = "common/native_lib_fixtures.rs"]
mod native_lib_fixtures;

use std::cell::RefCell;
use std::sync::OnceLock;

use check_harness::{Entry, Package};
use native_lib_fixtures::LIBRARIES;
use vesper::check::World;
use vesper::codegen_swap::{self, CodegenHooks};
use vesper::ir::Program;
use vesper::native::export::{LibraryOptions, LibraryStage, compile_source, library_program};

const PACKAGE: &str = "packages/vesper-library-ir";
/// The port libraries of the hybrid build.
const MODULES: &str = "crates/vesper-hybrid/modules";
/// Port libraries the test cannot check, and why.
const UNCHECKED_MODULES: &[(&str, &str)] = &[(
    "driver",
    "binds Ffi[HybridDriver], which only the hybrid's host provides",
)];
/// Every how-many-th port library the default run covers.
const DEFAULT_MODULE_STEP: usize = 4;
/// The target the fixtures build for (as tests/self_host_native_lib.rs).
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// The calls task flags of a library's first root and of its other roots
/// (packages/vesper-codegen-calls/src/data.ves).
const LIBRARY_TASK: u64 = 4;
const MORE_ROOTS_TASK: u64 = 8;
/// The compared fixture programs the pipeline must change.
const MIN_CHANGED: usize = 8;

/// Libraries for the steps the @vesper/native-lib fixtures do not reach:
/// bit builtins as operators, 128-bit tuple types and memory forwarding.
const PIPELINE_LIBRARIES: &[(&str, &str)] = &[
    (
        "bits",
        "pub fn bits(x: u32, n: u32) -> u32 {
    let mut y: u32 = x;
    y = wrapping_shl_u32(y, 3);
    wrapping_add(count_ones_u32(y), wrapping_shr_u32(x, n))
}
pub fn zeros(x: u64) -> u32 { wrapping_add(leading_zeros_u64(x), trailing_zeros_u64(x)) }
pub fn neg(x: i16) -> i16 { wrapping_neg_i16(x) }",
    ),
    (
        "wide_tuples",
        "pub fn first(a: i128) -> i128 {
    let t: (i128, u64) = (a, 1);
    let (x, y) = t;
    x
}
pub fn pick(c: bool, a: u128) -> u128 {
    let mut t: (u128, u128) = (0, 1);
    if c { t = (a, 2); }
    let (x, y) = t;
    wrapping_add(x, y)
}",
    ),
    (
        "forwarding",
        "pub fn reread(a: u64) -> u64 {
    let mut x: u64 = a;
    x = wrapping_add(x, 1);
    let y = x;
    let z = x;
    wrapping_add(y, z)
}",
    ),
];

/// The package's status words (`Status` of src/lib.ves).
const STATUS_OK: u64 = 0;
const STATUS_FIP_ACTIVE: u64 = 16;
const STATUS_INLINE: u64 = 17;

/// The stages, in order, with their words (`Stage` of src/lib.ves).
const STAGES: [(LibraryStage, u64); 5] = [
    (LibraryStage::Widths, 0),
    (LibraryStage::Optimize, 1),
    (LibraryStage::Inline, 2),
    (LibraryStage::Drops, 3),
    (LibraryStage::Hoist, 4),
];

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            Package::load(PACKAGE).entry("library_ir_text", true)
        })
    })
}

/// What one comparison found.
#[derive(Default)]
struct Tally {
    /// Stages whose programs are `Debug`-equal.
    same: usize,
    /// Stages the package refused where Rust failed too.
    both_failed: usize,
    /// Stages the package does not run yet (the inliner, `@fip` worlds).
    not_ported: usize,
    /// Functions in the compared programs.
    functions: usize,
    /// Identical stages whose program differs from the first lowering.
    changed: usize,
    failures: Vec<String>,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        self.same += other.same;
        self.both_failed += other.both_failed;
        self.not_ported += other.not_ported;
        self.functions += other.functions;
        self.changed += other.changed;
        self.failures.extend(other.failures);
    }
}

/// The first line where two dumps differ.
fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  rust:   {x}\n  vesper: {y}", i + 1);
        }
    }
    format!(
        "one dump is a prefix of the other ({} vs {} lines)",
        a.lines().count(),
        b.lines().count()
    )
}

/// Runs both pipelines on the first lowering `program` of `world` at every
/// stage.
fn compare(
    label: &str,
    world: &World,
    program: &Program,
    tasks: &[(String, u64)],
    optimize: bool,
) -> Tally {
    let mut t = Tally::default();
    let Some((calls, pool)) = codegen_calls_words::calls_input_pool(world, program, tasks, true)
    else {
        t.failures
            .push(format!("{label}: the program has no calls input"));
        return t;
    };
    let lowered = format!("{program:?}");
    for (stage, word) in STAGES {
        let mut rust = program.clone();
        let expected = library_program(world, &mut rust, optimize, stage);
        let mut input = Vec::with_capacity(calls.len() + 16);
        input.extend(word.to_le_bytes());
        input.extend(u64::from(optimize).to_le_bytes());
        input.extend(&calls);
        let words = entry().words(&input);
        let what = format!("{label} (optimize {optimize}, {stage:?})");
        match (words.first().copied(), expected) {
            (Some(STATUS_OK), Ok(())) => {
                let ours = ir_words::decode_program(&words[1..], &pool);
                let theirs = format!("{rust:?}");
                if format!("{ours:?}") == theirs {
                    t.same += 1;
                    t.functions += rust.functions.len();
                    t.changed += usize::from(theirs != lowered);
                } else {
                    let d = first_difference(&ir_words::dump(&rust), &ir_words::dump(&ours));
                    t.failures.push(format!("{what}: programs differ at {d}"));
                }
            }
            (Some(STATUS_FIP_ACTIVE | STATUS_INLINE), _) => t.not_ported += 1,
            (Some(status), Err(_)) if status != STATUS_OK => t.both_failed += 1,
            (status, Err(e)) => t.failures.push(format!(
                "{what}: Rust failed ({e}), Vesper status {status:?}"
            )),
            (status, Ok(())) => t
                .failures
                .push(format!("{what}: Rust succeeded, Vesper status {status:?}")),
        }
    }
    t
}

thread_local! {
    /// What the recording hook compared during one build.
    static SEEN: RefCell<Option<Tally>> = const { RefCell::new(None) };
    /// The library being built, for the hook's labels.
    static LIBRARY: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The calls tasks of a library build over `roots` (as
/// tests/self_host_native_lib.rs `library_tasks`).
fn library_tasks(roots: &[&str], optimize: bool) -> Vec<(String, u64)> {
    roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let place = if i == 0 {
                LIBRARY_TASK
            } else {
                MORE_ROOTS_TASK
            };
            (r.to_string(), u64::from(optimize) | place)
        })
        .collect()
}

/// The recording `export_plans` hook: compares the pipelines on the first
/// lowering and declines, so the Rust build goes on as usual.
fn compare_first_lowering(
    world: &World,
    program: &Program,
    roots: &[&str],
    optimize: bool,
) -> Option<vesper::native::EntryPlans> {
    let label = LIBRARY.with(|l| l.borrow().clone());
    let tally = compare(
        &label,
        world,
        program,
        &library_tasks(roots, optimize),
        optimize,
    );
    SEEN.with(|s| s.borrow_mut().get_or_insert_with(Tally::default).add(tally));
    None
}

#[test]
fn library_programs_match_rust() {
    entry();
    let mut total = Tally::default();
    for (library, source) in LIBRARIES.iter().chain(PIPELINE_LIBRARIES) {
        for optimize in [true, false] {
            LIBRARY.with(|l| *l.borrow_mut() = library.to_string());
            SEEN.with(|s| *s.borrow_mut() = None);
            let hooks = CodegenHooks {
                export_plans: Some(compare_first_lowering),
                ..CodegenHooks::default()
            };
            let options = LibraryOptions {
                library,
                target: Some(TRIPLE),
                optimize,
            };
            let built =
                codegen_swap::with_hooks(hooks, || compile_source(library, source, &options));
            if let Err(e) = built {
                total
                    .failures
                    .push(format!("{library} (optimize {optimize}): {e:?}"));
            }
            match SEEN.with(|s| s.borrow_mut().take()) {
                Some(t) => total.add(t),
                None => total.failures.push(format!(
                    "{library} (optimize {optimize}): the export_plans hook did not run"
                )),
            }
        }
    }
    eprintln!(
        "library-ir: {} stages identical ({} functions, {} changed by the pipeline), {} refused by both, {} not ported, {} failures",
        total.same,
        total.functions,
        total.changed,
        total.both_failed,
        total.not_ported,
        total.failures.len()
    );
    assert!(total.failures.is_empty(), "{}", total.failures.join("\n"));
    assert!(
        total.changed >= MIN_CHANGED,
        "the pipeline changed only {} compared programs",
        total.changed
    );
}

/// Every `LIBRARY_IR_MODULE_STEP`-th port library.
fn module_step() -> usize {
    std::env::var("LIBRARY_IR_MODULE_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MODULE_STEP)
}

/// The checked world of the package at `dir`, with the host's `.vsys`
/// imports when it binds foreign libraries (the driver module does).
fn checked_world(dir: &str) -> Result<World, String> {
    let p =
        vesper::packages::load(&check_harness::repo().join(dir)).map_err(|e| format!("{e:?}"))?;
    if !p.ok() {
        return Err(format!("{:?} {:?}", p.diagnostics, p.manifest_errors));
    }
    let checked = if p.vsys.is_empty() {
        vesper::check::check(&p.module)
    } else {
        let imports = p
            .imports(vesper_codegen_swap_test::host_target())
            .map_err(|e| format!("{e:?}"))?;
        vesper::ffi_lowering::check_module(&p.module, &imports)
            .map_err(|e| e.to_string())?
            .checked
    };
    if !checked.diagnostics.is_empty() {
        return Err(format!("{} diagnostics", checked.diagnostics.len()));
    }
    Ok(checked.world)
}

#[test]
fn port_package_programs_match_rust() {
    entry();
    let mut dirs: Vec<String> = std::fs::read_dir(check_harness::repo().join(MODULES))
        .expect("read the port libraries")
        .filter_map(|e| {
            let e = e.ok()?;
            e.path()
                .join("Loom.toml")
                .is_file()
                .then(|| format!("{MODULES}/{}", e.file_name().to_string_lossy()))
        })
        .collect();
    dirs.sort();
    let mut total = Tally::default();
    let mut covered = 0;
    for dir in dirs.iter().step_by(module_step()) {
        if UNCHECKED_MODULES
            .iter()
            .any(|(m, _)| dir.ends_with(&format!("/{m}")))
        {
            continue;
        }
        let world = match checked_world(dir) {
            Ok(w) => w,
            Err(e) => {
                total.failures.push(format!("{dir}: {e}"));
                continue;
            }
        };
        let program = vesper::ir_swap::lower(&world);
        for optimize in [true, false] {
            total.add(compare(dir, &world, &program, &[], optimize));
        }
        covered += 1;
    }
    eprintln!(
        "library-ir: {covered} port libraries, {} stages identical ({} functions), {} refused by both, {} not ported, {} failures",
        total.same,
        total.functions,
        total.both_failed,
        total.not_ported,
        total.failures.len()
    );
    assert!(total.failures.is_empty(), "{}", total.failures.join("\n"));
    assert!(total.same > 0, "no stage was compared");
}
