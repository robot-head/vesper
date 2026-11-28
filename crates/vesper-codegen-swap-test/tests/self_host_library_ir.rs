//! Bootstrap switch slice 3 (#2662, part of #2659): the IR pipeline that
//! `vesper build --lib` runs on a library's first lowering
//! (`native_export::library_program`), in Vesper (packages/vesper-library-ir).
//!
//! The package takes the stage to stop after, the optimize flag and
//! @vesper/codegen-calls' calls input of the first lowering (what the
//! `export_plans` hook sees), and returns the program as `put_program`
//! words. At every stage, the program read back must be `Debug`-equal to
//! Rust's after `library_program` through that stage, and a Rust error must
//! be a refusal; only programs with `@fip` functions (`@fip` reuse) and
//! closure bodies whose `sink` parameters hold a `Box` (lent captures the
//! input does not name) are not ported yet. For an optimized build, the inliner
//! input the package writes for the optimized program must also be
//! tests/common/inline_words.rs `inline_input`'s bytes for Rust's.
//!
//! - `library_programs_match_rust`: every library of
//!   tests/common/native_lib_fixtures.rs (the @vesper/native-lib fixtures),
//!   the pipeline fixtures (`Box` drop elaboration among them) and the port
//!   pilot's `Box` libraries (kit, widen), built optimized and unoptimized;
//!   the first lowering is recorded by an
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
#[path = "../../../tests/common/inline_words.rs"]
mod inline_words;
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
use vesper::ir_inline::Budget;
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
/// The compared fixture programs with elaborated `Box` drops (the `Box`
/// fixtures and the pilot libraries, optimized and not).
const MIN_DROPPED: usize = 8;

/// The port pilot's libraries whose worlds have the `Box` prelude (a boxed
/// tree, boxed trait objects), as the port builder builds them.
const PILOT_LIBRARIES: &[(&str, &str)] = &[
    ("kit", "crates/vesper-port-pilot/vesper/kit.ves"),
    ("widen", "crates/vesper-port-pilot/vesper/widen.ves"),
];

/// Libraries for the steps the @vesper/native-lib fixtures do not reach:
/// bit builtins as operators, 128-bit tuple types, memory forwarding,
/// inlining, bounds hoisting and `Box` drop elaboration (a world with the
/// `Box` prelude: unpacked, overwritten, projected, returned early, moved
/// on one branch and allocated in a loop).
const PIPELINE_LIBRARIES: &[(&str, &str)] = &[
    (
        "box_tree",
        "pub enum Tree {
    Leaf(u64),
    Node(Box[Tree], Box[Tree]),
}
fn sum(t: Tree) -> u64 {
    match t {
        Tree::Leaf(x) => x,
        Tree::Node(l, r) => match l {
            Box(a) => match r {
                Box(b) => wrapping_add(sum(a), sum(b)),
            },
        },
    }
}
fn depth(in t: Tree) -> u64 {
    match t {
        Tree::Leaf(x) => 1,
        Tree::Node(l, r) => 2,
    }
}
pub fn total(n: u64) -> u64 {
    let t = Tree::Node(Box::new(Tree::Leaf(n)), Box::new(Tree::Leaf(2)));
    let d = depth(in t);
    wrapping_add(sum(t), d)
}",
    ),
    (
        "box_paths",
        "struct Pair {
    a: Box[u64],
    b: Box[u64],
}
fn first(sink p: Pair) -> u64 {
    match p.a {
        Box(x) => x,
    }
}
fn take(sink b: Box[u64]) -> u64 {
    match b {
        Box(x) => x,
    }
}
fn make(n: u64) -> Pair {
    Pair { a: Box::new(n), b: Box::new(1) }
}
fn boxed(n: u64) -> Box[u64] {
    Box::new(n)
}
pub fn branchy(c: bool, n: u64) -> u64 {
    let b = Box::new(n);
    let mut r: u64 = 0;
    if c {
        r = take(b);
    }
    r
}
fn check(sink b: Box[u64]) -> bool {
    take(b) > 2
}
pub fn short(c: bool, n: u64) -> bool {
    let b = Box::new(n);
    c && check(b)
}
pub fn arms(k: u64, n: u64) -> u64 {
    let b = Box::new(n);
    match k {
        0 => take(b),
        _ => 5,
    }
}
pub fn field(n: u64) -> u64 {
    take(make(n).a)
}
pub fn discard(n: u64) -> u64 {
    boxed(n);
    n
}
pub fn spin(n: u64) -> u64 {
    let mut i: u64 = 0;
    let zero: u64 = 0;
    let mut b: Box[u64] = Box::new(zero);
    loop {
        if i == n {
            break;
        }
        b = Box::new(i);
        i = wrapping_add(i, 1);
    }
    take(b)
}
pub fn pick(c: bool, n: u64) -> u64 {
    let b = Box::new(n);
    if c {
        match b {
            Box(x) => x,
        }
    } else {
        7
    }
}
pub fn early(n: u64) -> u64 {
    let b = Box::new(n);
    if n > 3 {
        return 1;
    }
    match b {
        Box(x) => x,
    }
}
pub fn swap(n: u64) -> u64 {
    let mut p = Pair { a: Box::new(n), b: Box::new(2) };
    p.a = Box::new(5);
    first(p)
}
pub fn looped(n: u64) -> u64 {
    let mut i: u64 = 0;
    let mut acc: u64 = 0;
    loop {
        if i == n {
            break;
        }
        let b = Box::new(i);
        let v = match b {
            Box(x) => x,
        };
        acc = wrapping_add(acc, v);
        i = wrapping_add(i, 1);
    }
    acc
}",
    ),
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
    (
        "inlining",
        "fn twice(x: u64) -> u64 { wrapping_mul(x, 2) }
fn pick(c: bool, a: u64, b: u64) -> u64 { if c { a } else { b } }
pub fn quad(x: u64) -> u64 { twice(twice(x)) }
pub fn choose(x: u64) -> u64 { pick(x > 3, twice(x), 7) }",
    ),
    (
        "hoisting",
        "fn bump(inout d: ChunkSeq[u64], n: u64) {
    let mut i = n;
    loop {
        if i == 0 { break; }
        i = wrapping_sub(i, 1);
        let v = match d.try_get(i) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(i, wrapping_add(v, 1));
    }
}
pub fn bumped(n: u64) -> u64 {
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut k: u64 = 0;
    loop {
        if k == n { break; }
        d.push(k);
        k = wrapping_add(k, 1);
    }
    bump(inout d, n);
    match d.try_get(0) { Option::Some(x) => x, Option::None => 0 }
}",
    ),
];

/// The package's status words (`Status` of src/lib.ves).
const STATUS_OK: u64 = 0;
const STATUS_FIP_ACTIVE: u64 = 16;
/// A closure body's `sink` parameter holds a `Box` (src/drops.ves
/// `DropStatus::LentClosure`): the input does not say whether it is lent.
const STATUS_LENT_CLOSURE: u64 = 18;

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

/// `library_inline_input_text`: the inliner's input the package writes
/// for the optimized program.
fn input_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            Package::load(PACKAGE).entry("library_inline_input_text", true)
        })
    })
}

/// The bytes `inline_input_words` packed after the status word.
fn unpack(words: &[u64]) -> Vec<u8> {
    let n = words.first().copied().unwrap_or(0) as usize;
    let mut bytes: Vec<u8> = words.iter().skip(1).flat_map(|w| w.to_le_bytes()).collect();
    bytes.truncate(n);
    bytes
}

/// What one comparison found.
#[derive(Default)]
struct Tally {
    /// Stages whose programs are `Debug`-equal.
    same: usize,
    /// Stages the package refused where Rust failed too.
    both_failed: usize,
    /// Stages the package does not run yet (`@fip` worlds).
    not_ported: usize,
    /// Optimized programs whose inliner input is Rust's byte for byte.
    same_inputs: usize,
    /// Identical stages where the inliner or bounds hoisting changed the
    /// program.
    inlined: usize,
    hoisted: usize,
    /// Identical `Drops` stages with `Box` drops elaborated.
    dropped: usize,
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
        self.same_inputs += other.same_inputs;
        self.inlined += other.inlined;
        self.hoisted += other.hoisted;
        self.dropped += other.dropped;
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
    let mut before = lowered.clone();
    for (stage, word) in STAGES {
        let mut rust = program.clone();
        let expected = library_program(world, &mut rust, optimize, stage);
        let mut input = Vec::with_capacity(calls.len() + 16);
        input.extend(word.to_le_bytes());
        input.extend(u64::from(optimize).to_le_bytes());
        input.extend(&calls);
        let words = entry().words(&input);
        let what = format!("{label} (optimize {optimize}, {stage:?})");
        if optimize && stage == LibraryStage::Optimize && expected.is_ok() {
            compare_inline_input(&what, world, &rust, &input, &mut t);
        }
        match (words.first().copied(), expected) {
            (Some(STATUS_OK), Ok(())) => {
                let ours = ir_words::decode_program(&words[1..], &pool);
                let theirs = format!("{rust:?}");
                if format!("{ours:?}") == theirs {
                    t.same += 1;
                    t.functions += rust.functions.len();
                    t.changed += usize::from(theirs != lowered);
                    let stepped = usize::from(theirs != before);
                    match stage {
                        LibraryStage::Inline => t.inlined += stepped,
                        LibraryStage::Hoist => t.hoisted += stepped,
                        LibraryStage::Drops => t.dropped += usize::from(has_box_drops(&rust)),
                        _ => {}
                    }
                    before = theirs;
                } else {
                    let d = first_difference(&ir_words::dump(&rust), &ir_words::dump(&ours));
                    t.failures.push(format!("{what}: programs differ at {d}"));
                }
            }
            (Some(STATUS_FIP_ACTIVE | STATUS_LENT_CLOSURE), _) => t.not_ported += 1,
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

/// Whether a function of `p` has elaborated `Box` drops.
fn has_box_drops(p: &Program) -> bool {
    p.functions.values().any(|f| {
        !f.box_drops_before_inst.is_empty()
            || !f.box_drops_after_inst.is_empty()
            || !f.box_drops_before_term.is_empty()
    })
}

/// The inliner's input the package writes for the optimized program must
/// be `inline_input`'s bytes for Rust's.
fn compare_inline_input(what: &str, world: &World, rust: &Program, input: &[u8], t: &mut Tally) {
    let words = input_entry().words(input);
    if words.first().copied() != Some(STATUS_OK) {
        t.failures.push(format!(
            "{what}: no inliner input, status {:?}",
            words.first()
        ));
        return;
    }
    let ours = unpack(&words[1..]);
    let theirs = inline_words::inline_input(world, rust, &Budget::default()).bytes;
    if ours == theirs {
        t.same_inputs += 1;
        return;
    }
    let at = ours
        .iter()
        .zip(&theirs)
        .position(|(a, b)| a != b)
        .unwrap_or(ours.len().min(theirs.len()));
    t.failures.push(format!(
        "{what}: the inliner input differs at byte {at} of {} (Rust {})",
        ours.len(),
        theirs.len()
    ));
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
    let mut libraries: Vec<(&str, String)> = LIBRARIES
        .iter()
        .chain(PIPELINE_LIBRARIES)
        .map(|(l, s)| (*l, s.to_string()))
        .collect();
    for (library, path) in PILOT_LIBRARIES {
        let source = std::fs::read_to_string(check_harness::repo().join(path))
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        libraries.push((library, source));
    }
    for (library, source) in &libraries {
        let (library, source) = (*library, source.as_str());
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
        "library-ir: {} stages identical ({} functions, {} changed by the pipeline), {} inliner inputs identical, {} inlined, {} hoisted, {} with Box drops, {} refused by both, {} not ported, {} failures",
        total.same,
        total.functions,
        total.changed,
        total.same_inputs,
        total.inlined,
        total.hoisted,
        total.dropped,
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
    assert!(
        total.dropped >= MIN_DROPPED,
        "only {} compared programs have Box drops",
        total.dropped
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
        "library-ir: {covered} port libraries, {} stages identical ({} functions), {} inliner inputs identical, {} inlined, {} hoisted, {} refused by both, {} not ported, {} failures",
        total.same,
        total.functions,
        total.same_inputs,
        total.inlined,
        total.hoisted,
        total.both_failed,
        total.not_ported,
        total.failures.len()
    );
    assert!(total.failures.is_empty(), "{}", total.failures.join("\n"));
    assert!(total.same > 0, "no stage was compared");
}
