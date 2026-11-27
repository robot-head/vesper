//! Bootstrap switch slice 2a-1 (#2349, part of #1397): a library's whole
//! native object built by @vesper/native-lib (packages/vesper-native-lib)
//! over @vesper/cranelift and the shim, against `vesper build --lib`
//! (`native_export::compile_source`).
//!
//! The Rust build runs with a recording `body` hook: for every function the
//! walk would hand to Vesper it records the request (status and location
//! flags, parameter count, direct callees) and the function's
//! @vesper/codegen-scalar body plan, then declines, so the Rust walk emits
//! the object. The module's declarations (symbol, linkage, signature)
//! are read from the lent module. The request built from those records
//! asks @vesper/native-lib for the same object, which must have the same
//! bytes.
//!
//! The Rust side runs with the CLIF peephole pass and the vectorizer off
//! (`VESPER_CLIF_OPT=off`, `VESPER_VECTORIZE=off`): Stage 2 has no Vesper
//! port of `native_clif_opt` yet (slice 2a-4), so slice 2a-1 compares the
//! objects the walk itself makes.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper::native::export::{LibraryOptions, compile_source};
use vesper_codegen_swap_test::foreign_package;
use vesper_cranelift_sys::cranelift_codegen::ir::{self, ArgumentExtension, ArgumentPurpose};
use vesper_cranelift_sys::cranelift_codegen::isa::CallConv;
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

/// One declared function, as the request names it.
#[derive(Debug, Clone)]
struct Declared {
    symbol: String,
    linkage: u64,
    call_conv: u64,
    params: Vec<u64>,
    returns: Vec<u64>,
}

/// One function the walk defined, as the body hook saw it.
#[derive(Debug, Clone)]
struct Defined {
    func: u64,
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

fn type_code(t: ir::Type) -> u64 {
    u64::from(vesper_cranelift_sys::codes::ty_code(t))
}

/// The shim's calling convention code (`codes::CALLCONV`).
fn call_conv_code(cc: CallConv) -> Option<u64> {
    Some(match cc {
        CallConv::SystemV => 1,
        CallConv::WindowsFastcall => 2,
        CallConv::AppleAarch64 => 3,
        CallConv::Tail => 4,
        CallConv::Fast => 5,
        CallConv::Cold => 6,
        _ => return None,
    })
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
            call_conv: call_conv_code(sig.call_conv)
                .ok_or_else(|| format!("calling convention {:?}", sig.call_conv))?,
            params: sig.params.iter().map(|p| type_code(p.value_type)).collect(),
            returns: sig
                .returns
                .iter()
                .map(|p| type_code(p.value_type))
                .collect(),
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
            optimize: lent.optimize,
            status: args[0],
            locations: args[1],
            callees,
            plan,
        });
    });
    Ok(false)
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
    q.word(r.declared.len() as u64);
    for d in &r.declared {
        q.bytes(d.symbol.as_bytes());
        q.word(d.linkage);
        q.word(d.call_conv);
        q.word(d.params.len() as u64);
        d.params.iter().for_each(|&t| q.word(t));
        q.word(d.returns.len() as u64);
        d.returns.iter().for_each(|&t| q.word(t));
    }
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
    q.0
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
        ..CodegenHooks::default()
    };
    let options = LibraryOptions {
        library,
        target: Some(TRIPLE),
        optimize: true,
    };
    let built = codegen_swap::with_hooks(hooks, || compile_source(library, source, &options))
        .map_err(|e| format!("{e:?}"))?;
    let record = RECORD.with(|r| std::mem::take(&mut *r.borrow_mut()));
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
/// floats, narrow integers, and direct calls between exports and locals.
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
        } else {
            failures.push(format!(
                "{library}: objects differ ({} vs {} bytes)\nVesper CLIF:\n{clif}",
                object.len(),
                rust.len()
            ));
        }
    }
    eprintln!(
        "native-lib: {same} of {} library objects identical",
        LIBRARIES.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A request that does not read is an error answer, not a crash.
#[test]
fn malformed_requests_are_refused() {
    let refused = |request: &[u8]| {
        let answer = call(request);
        read_word(&answer, &mut 0) != BUILT
    };
    assert!(refused(&[1, 2, 3]), "a request shorter than its first word");
    // A whole header and an empty declaration list, then no definition
    // count.
    let mut q = Request::default();
    q.bytes(TRIPLE.as_bytes());
    q.bytes(FLAGS.as_bytes());
    q.bytes(MODULE_NAME.as_bytes());
    q.word(0);
    q.word(0);
    assert!(refused(&q.0), "a request without its definition count");
    q.word(0);
    assert!(!refused(&q.0), "an empty library is an empty object");
}
