//! Bootstrap switch slice 2a-1 (#2349, part of #1397): a library's whole
//! native object built by @vesper/native-lib (packages/vesper-native-lib)
//! over @vesper/cranelift and the shim, against `vesper build --lib`
//! (`native_export::compile_source`).
//!
//! The Rust build runs with a recording `body` hook: for every function the
//! walk would hand to Vesper it records the request (status and location
//! flags, parameter count, direct callees) and the function's
//! @vesper/codegen-scalar body plan, then declines, so the Rust walk emits
//! the object. A recording `export_plans` hook keeps the calls input of
//! the library's first lowering, from which the driver makes the instances
//! and their declarations itself (slice 2a-2a); the request names only the
//! exports' symbols, read from the lent module. The request built from
//! those records asks @vesper/native-lib for the same object, which must
//! have the same bytes.
//!
//! The Rust side runs with the CLIF peephole pass and the vectorizer off
//! (`VESPER_CLIF_OPT=off`, `VESPER_VECTORIZE=off`): the driver runs no CLIF
//! passes yet (slice 2a-4), so this compares the objects the walk itself
//! makes.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "../../../tests/self_host_codegen_export/words.rs"]
mod export_words;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper::native::export::{
    AbiType, ExportedFunction, LibraryInterface, LibraryOptions, compile_source,
};
use vesper_codegen_swap_test::foreign_package;
use vesper_cranelift_sys::cranelift_codegen::ir::{self, ArgumentExtension, ArgumentPurpose};
use vesper_cranelift_sys::cranelift_module::{Linkage, Module};

const PACKAGE: &str = "packages/vesper-native-lib";
/// The target both sides build for: an explicit triple with no extra CPU
/// features, so the ISA does not depend on the machine.
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// The flags `native::isa_with_flags` sets for an optimized build of a
/// program that needs no NaN canonicalization.
const FLAGS: &str = "enable_llvm_abi_extensions=true;is_pic=true;opt_level=speed";
/// `ObjectBuilder`'s module name in `native_export::object`.
const MODULE_NAME: &str = "vesper";
/// The answer's status word when the object was built.
const BUILT: u64 = 0;
/// The bytes of one request word.
const WORD_BYTES: usize = 8;
/// The calls task flags of a library's first root and of its other roots
/// (packages/vesper-codegen-calls/src/data.ves).
const LIBRARY_TASK: u64 = 4;
const MORE_ROOTS_TASK: u64 = 8;
/// The CLIF passes the walk's objects are compared without.
const PASSES_OFF: [(&str, &str); 2] = [("VESPER_CLIF_OPT", "off"), ("VESPER_VECTORIZE", "off")];

fn repo() -> std::path::PathBuf {
    // Cargo runs tests in the crate directory; Bazel in the runfiles root.
    for base in [
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        std::path::PathBuf::from("."),
    ] {
        if base.join(PACKAGE).join("Loom.toml").exists() {
            return base;
        }
    }
    panic!("cannot find {PACKAGE}");
}

/// The plan entry and the driver, compiled once with no hook in effect.
struct Vesper {
    plan: check_harness::Entry,
    driver: WordFunction,
}

fn vesper() -> &'static Vesper {
    static V: OnceLock<Vesper> = OnceLock::new();
    V.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let package = check_harness::Package::load("packages/vesper-codegen-scalar");
            let checked = foreign_package(&repo(), PACKAGE);
            let symbols = vesper_cranelift_sys::symbols();
            Vesper {
                plan: package.entry("codegen_scalar_body_plan", true),
                driver: checked
                    .word_function("library_object", &symbols, true)
                    .unwrap_or_else(|e| panic!("library_object: {e}")),
            }
        })
    })
}

/// The JIT's sequence runtime, for `HostCall` to free returned sequences.
const JIT_RUNTIME: vesper_port_abi::SeqRuntime = {
    extern "C" fn new(cap: i64, stride: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_new(cap as u64, stride as u64) as i64
    }
    extern "C" fn free(handle: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_free(handle as u64) as i64
    }
    vesper_port_abi::SeqRuntime { new, free }
};

fn call(request: &[u8]) -> Vec<u8> {
    let f = &vesper().driver;
    let call = vesper_port_abi::HostCall::new().bytes(request);
    // SAFETY: the entry takes one `in Slice[u8]` (passed as the address of
    // its view) and returns a live sequence of the JIT runtime holding
    // bytes, which `HostCall` copies and frees.
    unsafe { call.seq(JIT_RUNTIME, |w| f.call(w).unwrap()) }
}

/// One function the Rust build declared: the request names the exports'
/// symbols; the driver declares every instance itself.
#[derive(Debug, Clone)]
struct Declared {
    symbol: String,
    linkage: u64,
}

/// One function the walk defined, as the body hook saw it.
#[derive(Debug, Clone)]
struct Defined {
    func: u64,
    /// The instance key (the function's name: a plain library has no
    /// generic instances).
    key: String,
    optimize: bool,
    status: u64,
    locations: u64,
    callees: Vec<u64>,
    plan: Vec<u64>,
}

/// What the recording hook saw during one Rust build.
#[derive(Default)]
struct Record {
    declared: Vec<Declared>,
    defined: Vec<Defined>,
    /// Why the build cannot be asked of the driver, if it cannot.
    refused: Option<String>,
    /// The body plans of the lent program, by function name.
    plans: Option<HashMap<String, Vec<u64>>>,
    /// The calls input of the library's first lowering, which the
    /// `export_plans` hook saw.
    calls: Option<Vec<u8>>,
    /// The library's interface: its 128-bit entries and v2 glue.
    interface: Option<LibraryInterface>,
}

thread_local! {
    static RECORD: RefCell<Record> = RefCell::new(Record::default());
}

fn plans_of(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
) -> HashMap<String, Vec<u64>> {
    let mut bodies = HashMap::new();
    if let Some((input, _)) = ir_words::program_input(world, program) {
        let names: Vec<&String> = program.functions.keys().collect();
        let words = vesper().plan.words(&input);
        let mut rest = &words[..];
        while let [fi, n, tail @ ..] = rest {
            let n = *n as usize;
            bodies.insert(names[*fi as usize].clone(), tail[..n].to_vec());
            rest = &tail[n..];
        }
    }
    bodies
}

/// A parameter or result the shim's `sig_param`/`sig_return` can spell.
fn plain_param(p: &ir::AbiParam) -> bool {
    p.purpose == ArgumentPurpose::Normal && p.extension == ArgumentExtension::None
}

fn declarations(module: &dyn Module) -> Result<Vec<Declared>, String> {
    let mut out = vec![];
    for (id, decl) in module.declarations().get_functions() {
        if id.as_u32() as usize != out.len() {
            return Err(format!("function ids are not dense at {}", id.as_u32()));
        }
        let sig = &decl.signature;
        if !sig.params.iter().chain(&sig.returns).all(plain_param) {
            return Err(format!(
                "{}: a parameter with a purpose or extension",
                decl.linkage_name(id)
            ));
        }
        if !matches!(decl.linkage, Linkage::Local | Linkage::Export) {
            return Err(format!(
                "{}: linkage {:?}",
                decl.linkage_name(id),
                decl.linkage
            ));
        }
        out.push(Declared {
            symbol: decl.linkage_name(id).into_owned(),
            linkage: u64::from(vesper_cranelift_sys::codes::linkage_code(decl.linkage)),
        });
    }
    Ok(out)
}

fn module_has_data(module: &dyn Module) -> bool {
    module.declarations().get_data_objects().next().is_some()
}

/// The recording `body` hook: notes the request and declines.
fn record(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    if lent.op != codegen_swap::body::FUNCTION {
        return Ok(false);
    }
    let function = lent.site.function.to_owned();
    let world = lent.world;
    let program = lent.program;
    let declared = declarations(&*lent.module);
    RECORD.with(|cell| {
        let mut r = cell.borrow_mut();
        if r.refused.is_some() {
            return;
        }
        match declared {
            Ok(d) => r.declared = d,
            Err(e) => {
                r.refused = Some(e);
                return;
            }
        }
        if r.plans.is_none() {
            r.plans = Some(plans_of(world, program));
        }
        let Some(plan) = r.plans.as_ref().and_then(|p| p.get(&function)).cloned() else {
            r.refused = Some(format!("{function}: no body plan"));
            return;
        };
        let args = lent.args;
        let np = args[2] as usize;
        let Some((callees, sret, homes)) = codegen_swap::body::layout(args) else {
            r.refused = Some(format!("{function}: a body request without its callees"));
            return;
        };
        // The module's signatures carry no result pointer, so a struct
        // result stays with the walk.
        if sret != codegen_swap::body::NO_VALUE {
            r.refused = Some(format!("{function}: returns through a result pointer"));
            return;
        }
        let homes = homes.len();
        let callees = callees.to_vec();
        if module_has_data(&*lent.module) {
            r.refused = Some("the module declares data".into());
            return;
        }
        // The walk declares and defines the instances in the same order,
        // so the n-th definition is function n.
        let func = r.defined.len() as u64;
        debug_assert_eq!(args.len(), 6 + np + callees.len() + homes);
        r.defined.push(Defined {
            func,
            key: function,
            optimize: lent.optimize,
            status: args[0],
            locations: args[1],
            callees,
            plan,
        });
    });
    Ok(false)
}

/// The calls tasks of a library build over `roots` (flags as the
/// `export_plans` hook of crates/vesper-hybrid/src/swap/codegen_walk.rs
/// sets them: optimize, then library on the first root and more roots on
/// the rest).
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

/// The recording `export_plans` hook: notes the calls input of the
/// library task and declines, so the Rust plans are made.
fn record_calls(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    roots: &[&str],
    optimize: bool,
) -> Option<vesper::native::EntryPlans> {
    let input = codegen_calls_words::calls_input_flags_wide(
        world,
        program,
        &library_tasks(roots, optimize),
    );
    RECORD.with(|cell| {
        let mut r = cell.borrow_mut();
        match input {
            Some(i) => r.calls = Some(i),
            None => r.refused = Some("the program has no calls input".into()),
        }
    });
    None
}

#[derive(Default)]
struct Request(Vec<u8>);

impl Request {
    fn word(&mut self, w: u64) {
        self.0.extend_from_slice(&w.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        self.0.extend_from_slice(b);
    }
}

fn request(r: &Record) -> Vec<u8> {
    let mut q = Request::default();
    q.bytes(TRIPLE.as_bytes());
    q.bytes(FLAGS.as_bytes());
    q.bytes(MODULE_NAME.as_bytes());
    q.word(1);
    let wide: Vec<&ExportedFunction> = r
        .interface
        .as_ref()
        .map(|i| i.functions.iter().filter(|f| needs_wide_entry(f)).collect())
        .unwrap_or_default();
    // The exports declared under their own symbols, then the 128-bit ones,
    // which the driver declares locally behind their entries.
    let exports: Vec<(&str, &str)> = r
        .defined
        .iter()
        .filter_map(|d| {
            let declared = &r.declared[d.func as usize];
            (declared.linkage
                == u64::from(vesper_cranelift_sys::codes::linkage_code(Linkage::Export)))
            .then_some((d.key.as_str(), declared.symbol.as_str()))
        })
        .chain(wide.iter().map(|f| (f.name.as_str(), f.symbol.as_str())))
        .collect();
    q.word(exports.len() as u64);
    for (key, symbol) in exports {
        q.bytes(key.as_bytes());
        q.bytes(symbol.as_bytes());
    }
    q.bytes(r.calls.as_deref().unwrap_or_default());
    q.word(r.defined.len() as u64);
    for d in &r.defined {
        q.word(d.func);
        q.word(u64::from(d.optimize));
        q.word(d.status);
        q.word(d.locations);
        q.word(d.callees.len() as u64);
        d.callees.iter().for_each(|&c| q.word(c));
        q.word(d.plan.len() as u64);
        d.plan.iter().for_each(|&w| q.word(w));
    }
    q.word(wide.len() as u64);
    for f in wide {
        let inner = r
            .defined
            .iter()
            .find(|d| d.key == f.name)
            .map_or(u64::MAX, |d| d.func);
        q.bytes(f.symbol.as_bytes());
        q.word(inner);
        q.word(u64::from(f.result_pointer));
        q.word(u64::from(wide_by_pointer(&f.result)));
        q.word(u64::from(f.fail_record_words.is_some()));
        q.word(f.params.len() as u64);
        for p in &f.params {
            q.word(u64::from(p.pointer));
            q.word(u64::from(p.pass() != "inout" && wide_by_pointer(&p.ty)));
        }
    }
    match r.interface.as_ref().filter(|i| i.is_v2()) {
        Some(i) => {
            q.word(1);
            codegen_swap::seq::runtime_args(false)
                .iter()
                .for_each(|&w| q.word(w));
            q.bytes(&export_words::binding_input(i, &Default::default()));
        }
        None => q.word(0),
    }
    q.0
}

/// Whether `ty` is `i128` or `u128`, which the export ABI passes through a
/// pointer to its two words (`native_export::wide_by_pointer`).
fn wide_by_pointer(ty: &AbiType) -> bool {
    matches!(ty, AbiType::Scalar { name } if name == "i128" || name == "u128")
}

/// Whether `f`'s export symbol is a 128-bit entry around a local function
/// (`native_export::needs_wide_entry`).
fn needs_wide_entry(f: &ExportedFunction) -> bool {
    wide_by_pointer(&f.result)
        || f.params
            .iter()
            .any(|p| p.pass() != "inout" && wide_by_pointer(&p.ty))
}

fn read_word(b: &[u8], at: &mut usize) -> u64 {
    let w = u64::from_le_bytes(b[*at..*at + 8].try_into().unwrap());
    *at += 8;
    w
}

fn read_bytes<'a>(b: &'a [u8], at: &mut usize) -> &'a [u8] {
    let n = read_word(b, at) as usize;
    let s = &b[*at..*at + n];
    *at += n;
    s
}

/// The object `vesper build --lib` makes of `source` (passes off), with
/// the recorded request; or why the driver cannot be asked for it.
fn rust_build(library: &str, source: &str) -> Result<(Vec<u8>, Record), String> {
    RECORD.with(|r| *r.borrow_mut() = Record::default());
    let hooks = CodegenHooks {
        body: Some(record),
        export_plans: Some(record_calls),
        ..CodegenHooks::default()
    };
    let options = LibraryOptions {
        library,
        target: Some(TRIPLE),
        optimize: true,
    };
    let built = codegen_swap::with_hooks(hooks, || compile_source(library, source, &options))
        .map_err(|e| format!("{e:?}"))?;
    let mut record = RECORD.with(|r| std::mem::take(&mut *r.borrow_mut()));
    record.interface = Some(built.interface);
    if let Some(why) = &record.refused {
        return Err(why.clone());
    }
    if record.defined.len() != record.declared.len() {
        return Err(format!(
            "{} of {} functions went through the body hook",
            record.defined.len(),
            record.declared.len()
        ));
    }
    Ok((built.object, record))
}

/// Libraries of plain scalar functions: arithmetic, branches, loops,
/// floats, narrow integers, direct calls between exports and locals, and
/// `fail_only` functions with their failure records (#2425).
const LIBRARIES: &[(&str, &str)] = &[
    (
        "arith",
        "pub fn add3(a: i64, b: i64, c: i64) -> i64 { wrapping_add(wrapping_add(a, b), c) }
pub fn mix(a: u32, b: u32) -> u32 { (a ^ b) | (a & 7) }",
    ),
    (
        "branches",
        "pub fn sign(x: i16) -> i16 { if x < 0 { -1 } else if x == 0 { 0 } else { 1 } }
pub fn pick(c: bool, x: u8) -> u8 { let mut y: u8 = x; if c { y = x & 15; } y }",
    ),
    (
        "loops",
        "pub fn gcd(a: u64, b: u64) -> u64 {
    let mut x: u64 = a;
    let mut y: u64 = b;
    loop {
        if y == 0 { break; }
        let t: u64 = y;
        y = x % y;
        x = t;
    }
    x
}",
    ),
    (
        "floats",
        "pub fn lerp(a: f64, b: f64, t: f64) -> f64 { a + (b - a) * t }
pub fn half(x: f32) -> f32 { x * 0.5 }",
    ),
    (
        "calls",
        "fn square(x: i64) -> i64 { wrapping_mul(x, x) }
pub fn sum_squares(a: i64, b: i64) -> i64 { wrapping_add(square(a), square(b)) }
pub fn quad(x: i64) -> i64 { square(square(x)) }",
    ),
    (
        "fails",
        "fn step(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow]] { a * b + 1 }
pub fn total(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { step(a, b) / b }",
    ),
    // 128-bit parameters and results (#1277): each export symbol is a
    // `define_wide_entry` adapter around a local function.
    (
        "wide",
        "pub fn xor128(a: u128, b: u128) -> u128 { a ^ b }
pub fn negate(a: i128) -> i128 { wrapping_sub(0, a) }
pub fn is_zero(a: u128) -> bool { a == 0 }
pub fn mixed(x: u64, a: i128, y: f64) -> f64 { if a < 0 { y } else { y + 1.0 } }
pub fn scaled(a: u128, b: u64) -> u64 eff [Fail[ArithOverflow]] { b * 2 }",
    ),
    // (Checked 128-bit arithmetic has no body plan yet, so a failing
    // function with a wide result stays out.)
    // v2 libraries: the runtime entry points next to the functions, and
    // the drop glue of a handle type.
    (
        "v2",
        "// @export abi 3
pub fn add(a: u64, b: u64) -> u64 { wrapping_add(a, b) }",
    ),
    (
        "v2_wide",
        "// @export abi 3
pub fn twice(a: i128) -> i128 { wrapping_add(a, a) }
pub fn keep(x: u32) -> u32 { x }",
    ),
    (
        "v2_handle",
        "// @export abi 3
pub struct Buf { data: ChunkSeq[u8], n: u64 }
pub fn count(inout b: Buf) -> u64 { 7 }",
    ),
];

fn with_passes_off<R>(f: impl FnOnce() -> R) -> R {
    let saved: Vec<_> = PASSES_OFF
        .iter()
        .map(|(k, v)| {
            let old = std::env::var_os(k);
            // SAFETY: the test's only thread that reads these variables is
            // this one, inside `f`.
            unsafe { std::env::set_var(k, v) };
            (*k, old)
        })
        .collect();
    let r = f();
    for (k, old) in saved {
        // SAFETY: as above.
        unsafe {
            match old {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
    r
}

/// Byte-identical objects for every library of plain functions.
#[test]
fn vesper_library_objects_match_vesper_build_lib() {
    let mut same = 0;
    // What the identical objects covered past the walk.
    let mut wide_entries = 0;
    let mut v2_libraries = 0;
    let mut drop_glue = 0;
    let mut failures = vec![];
    for (library, source) in LIBRARIES {
        let (rust, record) = match with_passes_off(|| rust_build(library, source)) {
            Ok(built) => built,
            Err(why) => {
                failures.push(format!("{library}: not a plain library: {why}"));
                continue;
            }
        };
        let answer = call(&request(&record));
        let mut at = 0;
        if read_word(&answer, &mut at) != BUILT {
            let why = String::from_utf8_lossy(read_bytes(&answer, &mut at));
            failures.push(format!("{library}: the driver failed: {why}"));
            continue;
        }
        let object = read_bytes(&answer, &mut at).to_vec();
        let clif = String::from_utf8_lossy(read_bytes(&answer, &mut at)).into_owned();
        if object == rust {
            same += 1;
            if let Some(i) = &record.interface {
                wide_entries += i.functions.iter().filter(|f| needs_wide_entry(f)).count();
                if i.is_v2() {
                    v2_libraries += 1;
                    drop_glue += i
                        .types
                        .values()
                        .filter_map(|t| t.drop_symbol.as_ref())
                        .count();
                }
            }
        } else {
            failures.push(format!(
                "{library}: objects differ ({} vs {} bytes)\nVesper CLIF:\n{clif}",
                object.len(),
                rust.len()
            ));
        }
    }
    eprintln!(
        "native-lib: {same} of {} library objects identical \
         ({wide_entries} 128-bit entries, {v2_libraries} v2 libraries, \
         {drop_glue} drop functions)",
        LIBRARIES.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        wide_entries > 0 && v2_libraries > 0 && drop_glue > 0,
        "the libraries no longer cover 128-bit entries, v2 glue and drop glue"
    );
}

/// A request that does not read is an error answer, not a crash.
#[test]
fn malformed_requests_are_refused() {
    let refused = |request: &[u8]| {
        let answer = call(request);
        read_word(&answer, &mut 0) != BUILT
    };
    assert!(refused(&[1, 2, 3]), "a request shorter than its first word");
    // A whole header and no exports, then no calls input.
    let mut q = Request::default();
    q.bytes(TRIPLE.as_bytes());
    q.bytes(FLAGS.as_bytes());
    q.bytes(MODULE_NAME.as_bytes());
    q.word(0);
    q.word(0);
    assert!(refused(&q.0), "a request without its calls input");
    q.bytes(&[]);
    q.word(0);
    assert!(refused(&q.0), "a calls input without a library task");
    // A whole library's request cut short in its trailing sections: the
    // v2 word, then the 128-bit entry count.
    let (_, record) = with_passes_off(|| rust_build(LIBRARIES[0].0, LIBRARIES[0].1))
        .expect("the first library builds");
    let whole = request(&record);
    assert!(!refused(&whole), "the whole request builds");
    for (cut, what) in [(1, "the v2 word"), (2, "the 128-bit entry count")] {
        let short = &whole[..whole.len() - cut * WORD_BYTES];
        assert!(refused(short), "a request without {what}");
    }
}
