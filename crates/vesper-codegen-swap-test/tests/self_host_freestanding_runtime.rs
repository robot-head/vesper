//! Self-hosting SH4 lane 14 (#1288), part 2: the freestanding runtime
//! object (src/freestanding/runtime.rs) emitted in Vesper by
//! packages/vesper-codegen-freestanding/runtime through @vesper/cranelift
//! and the shim, against Rust's `runtime::build`.
//!
//! For each runtime spec, both sides must give the same `runtime.o` bytes,
//! the same CLIF text of every function (Rust's functions renamed back
//! from Silicon's namespaces), the same trap records and the same Silicon
//! names of main, the trap handler and the exit. The specs are a matrix of
//! targets, UARTs, result kinds, failing entries and trap tables, and the
//! runtime of every image the freestanding tests build (its spec recovered
//! by matching Rust's own build of it).
//!
//! The Vesper entry takes one request (little-endian words and
//! length-prefixed strings) and returns one result buffer; `Request` and
//! `Words` here are its Rust half.
use std::sync::OnceLock;
use std::time::Instant;

use vesper::codegen_swap::{self, CodegenHooks};
use vesper::native::WordFunction;
use vesper::native::freestanding::{
    self as fs, FailEntry, FreestandingImage, FreestandingTarget, ImageOptions, ResultKind, Uart,
    runtime,
};
use vesper::{check, parser};
use vesper_codegen_swap_test::foreign_package;

const PACKAGE: &str = "packages/vesper-codegen-freestanding/runtime";
const CHECKED_SUM: &str = include_str!("../../../tests/fixtures/freestanding/checked_sum.ves");
const ABORT: &str = include_str!("../../../tests/fixtures/freestanding/abort.ves");

/// The run programs of tests/freestanding.rs (`PROGRAMS`), as in
/// tests/self_host_freestanding.rs.
const PROGRAMS: &[(&str, &str)] = &[
    ("scalar", "fn main() -> i64 { 2 + 3 * 4 }"),
    (
        "negative",
        "fn main() -> i64 { 0 - 9223372036854775807 - 1 }",
    ),
    ("bool", "fn main() -> bool { 3 > 2 }"),
    ("unit", "fn main() -> Unit { () }"),
    ("u8", "fn main() -> u8 { 255 }"),
    (
        "inout",
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let mut x = 41; inc(inout x); x }",
    ),
    (
        "handler",
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { wrapping_add(Ask::ask(), Ask::ask()) } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
    ),
    (
        "enum",
        "fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } } fn main() -> i64 { match make(42) { Some(v) => v, None => 0, } }",
    ),
    (
        "division",
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(0) } with { return x => x, raise(e, k) => match e { RuntimeError::DivisionByZero => 42, RuntimeError::IndexOutOfBounds => 0, }, } }",
    ),
    ("checked_sum", CHECKED_SUM),
    ("abort", ABORT),
];

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

/// The package's entry, JIT-compiled once with no hook in effect.
fn entry() -> &'static WordFunction {
    static ENTRY: OnceLock<WordFunction> = OnceLock::new();
    ENTRY.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), PACKAGE);
            let symbols = vesper_cranelift_sys::symbols();
            checked
                .word_function("freestanding_runtime", &symbols, true)
                .unwrap_or_else(|e| panic!("freestanding_runtime: {e}"))
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
    let f = entry();
    let call = vesper_port_abi::HostCall::new().bytes(request);
    // SAFETY: the entry takes one `in Slice[u8]` (passed as the address of
    // its view) and returns a live sequence of the JIT runtime holding
    // bytes, which `HostCall` copies and frees.
    unsafe { call.seq(JIT_RUNTIME, |w| f.call(w).unwrap()) }
}

#[derive(Default)]
struct Request(Vec<u8>);

impl Request {
    fn word(mut self, w: u64) -> Self {
        self.0.extend_from_slice(&w.to_le_bytes());
        self
    }
    fn bytes(mut self, b: &[u8]) -> Self {
        self = self.word(b.len() as u64);
        self.0.extend_from_slice(b);
        self
    }
}

struct Words<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Words<'a> {
    fn word(&mut self) -> u64 {
        let w = u64::from_le_bytes(self.b[self.at..self.at + 8].try_into().unwrap());
        self.at += 8;
        w
    }
    fn bytes(&mut self) -> &'a [u8] {
        let n = self.word() as usize;
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        s
    }
    fn string(&mut self) -> String {
        String::from_utf8(self.bytes().to_vec()).unwrap()
    }
}

/// What both sides produce for one spec.
#[derive(Debug, PartialEq, Eq)]
struct Built {
    object: Vec<u8>,
    clif: String,
    /// `(symbol, offset, TrapCode::as_raw)` in function order.
    traps: Vec<(String, u64, u32)>,
    main: String,
    trap: String,
    exit: String,
}

fn request(spec: &runtime::RuntimeSpec) -> Vec<u8> {
    let (uart, base) = match spec.board.uart {
        Uart::Ns16550 { base } => (0, base),
        Uart::Pl011 { base } => (1, base),
    };
    let (kind, bits) = match spec.result {
        ResultKind::Signed => (0, 0),
        ResultKind::Unsigned(bits) => (1, u64::from(bits)),
        ResultKind::Bool => (2, 0),
        ResultKind::Unit => (3, 0),
    };
    let (fails, instances, record) = match &spec.fail {
        Some(f) => (1, f.instances.len() as u64, u64::from(f.record_bytes)),
        None => (0, 0, 0),
    };
    let mut r = Request::default()
        .word(match spec.target {
            FreestandingTarget::Riscv64Imac => 0,
            FreestandingTarget::Aarch64 => 1,
        })
        .word(uart)
        .word(base)
        .bytes(spec.entry.as_bytes())
        .word(u64::from(spec.entry_func_id))
        .word(kind)
        .word(bits)
        .word(fails)
        .word(instances)
        .word(record)
        .word(spec.trap_messages.len() as u64);
    for m in &spec.trap_messages {
        r = r.bytes(m.as_bytes());
    }
    r = r.word(spec.trap_entries.len() as u64);
    for &e in &spec.trap_entries {
        r = r.word(e as u64);
    }
    r.0
}

fn vesper_runtime(spec: &runtime::RuntimeSpec) -> Result<Built, String> {
    let out = call(&request(spec));
    let mut w = Words { b: &out, at: 0 };
    if w.word() != 0 {
        return Err(w.string());
    }
    let object = w.bytes().to_vec();
    let clif = w.string();
    let mut traps = Vec::new();
    for _ in 0..w.word() {
        let symbol = w.string();
        for _ in 0..w.word() {
            let offset = w.word();
            traps.push((symbol.clone(), offset, w.word() as u32));
        }
    }
    let (main, trap, exit) = (w.string(), w.string(), w.string());
    assert_eq!(w.at, out.len(), "trailing result bytes");
    Ok(Built {
        object,
        clif,
        traps,
        main,
        trap,
        exit,
    })
}

/// Rust's CLIF names a reference `u<ns>:<index>` as Silicon sees it
/// (`runtime::build`'s `rename`); the module's own names are `u0:<FuncId>`.
/// Map `u0:<id>` tokens of the module's text to Silicon's.
fn silicon_names(text: &str, entry_func_id: u32) -> String {
    // Declaration order of `runtime::build`: putc, puts, put_udec,
    // put_hex, exit (4), entry (5), main, trap.
    const EXIT: u32 = 4;
    const ENTRY: u32 = 5;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("u0:") {
        out.push_str(&rest[..i]);
        let digits: String = rest[i + 3..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let preceded = out
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        match digits.parse::<u32>() {
            Ok(n) if !preceded => {
                let renamed = if n == ENTRY {
                    format!("u0:{entry_func_id}")
                } else if n == EXIT {
                    format!("u{}:0", runtime::STUB_NAMESPACE)
                } else {
                    format!("u{}:{n}", runtime::RUNTIME_NAMESPACE)
                };
                out.push_str(&renamed);
            }
            _ => {
                out.push_str("u0:");
                out.push_str(&digits);
            }
        }
        rest = &rest[i + 3 + digits.len()..];
    }
    out.push_str(rest);
    out
}

fn rust_runtime(spec: &runtime::RuntimeSpec) -> Result<Built, String> {
    let r = runtime::build(spec).map_err(|e| e.to_string())?;
    Ok(Built {
        object: r.object,
        clif: r
            .functions
            .iter()
            .map(|(_, f)| f.display().to_string())
            .collect(),
        traps: r
            .traps
            .iter()
            .map(|(s, o, c)| (s.clone(), *o, u32::from(c.as_raw().get())))
            .collect(),
        main: r.main,
        trap: r.trap,
        exit: r.exit,
    })
}

fn compare(label: &str, spec: &runtime::RuntimeSpec) {
    let rust = rust_runtime(spec).unwrap_or_else(|e| panic!("{label}: Rust: {e}"));
    let mut vesper = vesper_runtime(spec).unwrap_or_else(|e| panic!("{label}: Vesper: {e}"));
    vesper.clif = silicon_names(&vesper.clif, spec.entry_func_id);
    if rust.clif != vesper.clif {
        let first = rust
            .clif
            .lines()
            .zip(vesper.clif.lines())
            .find(|(a, b)| a != b);
        panic!("{label}: CLIF differs at {first:?}");
    }
    assert_eq!(rust.traps, vesper.traps, "{label}: trap records");
    assert_eq!(
        (&rust.main, &rust.trap, &rust.exit),
        (&vesper.main, &vesper.trap, &vesper.exit),
        "{label}: names"
    );
    assert!(
        rust.object == vesper.object,
        "{label}: runtime.o differs ({} vs {} bytes)",
        rust.object.len(),
        vesper.object.len()
    );
}

fn spec(
    target: FreestandingTarget,
    uart: Uart,
    result: ResultKind,
    fail: Option<FailEntry>,
    messages: &[&str],
    entries: &[usize],
) -> runtime::RuntimeSpec {
    let mut board = target.qemu_virt();
    board.uart = uart;
    runtime::RuntimeSpec {
        target,
        isa: target.isa().unwrap(),
        board,
        entry: "main".into(),
        entry_func_id: 3,
        result,
        fail,
        trap_messages: messages.iter().map(|m| (*m).to_string()).collect(),
        trap_entries: entries.to_vec(),
    }
}

fn matrix() -> Vec<(String, runtime::RuntimeSpec)> {
    let uarts = [
        Uart::Ns16550 { base: 0x1000_0000 },
        Uart::Pl011 { base: 0x0900_0000 },
    ];
    let kinds = [
        ResultKind::Signed,
        ResultKind::Unsigned(8),
        ResultKind::Unsigned(32),
        ResultKind::Unsigned(64),
        ResultKind::Bool,
        ResultKind::Unit,
    ];
    let fails = [
        None,
        Some(FailEntry {
            instances: vec!["Fail[RuntimeError]".into()],
            record_bytes: 16,
        }),
        Some(FailEntry {
            instances: vec!["Fail[A]".into(), "Fail[B]".into(), "Fail[C]".into()],
            record_bytes: 40,
        }),
    ];
    // No table; and a table with repeated, shared and runtime messages
    // (one message equal to a runtime string, which `Gen::string` shares).
    let tables: [(&[&str], &[usize]); 2] = [
        (&[], &[]),
        (
            &[
                "integer overflow",
                "index out of bounds",
                "true",
                "unhandled Fail",
            ],
            &[0, 1, 1, 0, 2, 3],
        ),
    ];
    let mut out = Vec::new();
    for target in FreestandingTarget::ALL {
        for uart in uarts {
            for kind in kinds {
                for fail in &fails {
                    for (t, (messages, entries)) in tables.iter().enumerate() {
                        let label = format!(
                            "{target} {uart:?} {kind:?} fail={:?} table={t}",
                            fail.as_ref().map(|f| f.instances.len())
                        );
                        out.push((
                            label,
                            spec(target, uart, kind, fail.clone(), messages, entries),
                        ));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn runtime_objects_are_byte_identical() {
    let specs = matrix();
    for (label, spec) in &specs {
        compare(label, spec);
    }
    eprintln!(
        "{} runtime specs: objects, CLIF and traps equal",
        specs.len()
    );
}

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

/// The spec `build_image` gave `runtime::build` for `image`: messages and
/// table entries from its trap sites (user sites first, deduplicated, then
/// one `unhandled` message per runtime abort site), the result kind and
/// failure record found by matching Rust's own build against the image's
/// `runtime.o`.
fn image_spec(image: &FreestandingImage) -> runtime::RuntimeSpec {
    let mut messages: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    let mut instances = Vec::new();
    for site in &image.traps {
        if site.function == runtime::MAIN_SYMBOL {
            messages.push(site.message.clone());
            entries.push(messages.len() - 1);
            let name = site.message.trim_start_matches("unhandled ").to_string();
            instances.push(name);
        } else {
            let i = match messages.iter().position(|m| *m == site.message) {
                Some(i) => i,
                None => {
                    messages.push(site.message.clone());
                    messages.len() - 1
                }
            };
            entries.push(i);
        }
    }
    let object = &image
        .objects
        .iter()
        .find(|(n, _)| n == "runtime.o")
        .unwrap()
        .1;
    // The last abort site is the unknown tag's ("unhandled Fail").
    instances.pop();
    let fails: Vec<Option<FailEntry>> = if image
        .traps
        .iter()
        .any(|s| s.function == runtime::MAIN_SYMBOL)
    {
        (1..=16)
            .map(|w| {
                Some(FailEntry {
                    instances: instances.clone(),
                    record_bytes: w * 8,
                })
            })
            .collect()
    } else {
        vec![None]
    };
    let kinds = [
        ResultKind::Signed,
        ResultKind::Unsigned(8),
        ResultKind::Unsigned(32),
        ResultKind::Unsigned(64),
        ResultKind::Bool,
        ResultKind::Unit,
    ];
    for kind in kinds {
        for fail in &fails {
            let s = runtime::RuntimeSpec {
                target: image.target,
                isa: image.target.isa().unwrap(),
                board: image.board.clone(),
                entry: image.entry_function.clone(),
                entry_func_id: 0,
                result: kind,
                fail: fail.clone(),
                trap_messages: messages.clone(),
                trap_entries: entries.clone(),
            };
            if runtime::build(&s).is_ok_and(|r| r.object == *object) {
                return s;
            }
        }
    }
    panic!("no runtime spec reproduces the image's runtime.o");
}

#[test]
fn image_runtimes_are_byte_identical() {
    let mut n = 0;
    for target in FreestandingTarget::ALL {
        for (name, source) in PROGRAMS {
            let image = fs::build_image(&world(source), "main", &ImageOptions::new(target))
                .unwrap_or_else(|e| panic!("{name} on {target}: {e}"));
            let spec = image_spec(&image);
            compare(&format!("{name} on {target}"), &spec);
            n += 1;
        }
    }
    assert_eq!(n, 22);
}

#[test]
fn malformed_request_is_an_error() {
    let out = call(&[1, 2, 3]);
    let mut w = Words { b: &out, at: 0 };
    assert_eq!(w.word(), 1);
    assert_eq!(w.string(), "malformed runtime request");
}

/// Rust `runtime::build` against the Vesper emission over the matrix,
/// `RUNTIME_ROUNDS` rounds (default 5), ABBA. `cargo test --release --
/// --ignored runtime_cost --nocapture`.
#[test]
#[ignore]
fn runtime_cost() {
    let rounds: usize = std::env::var("RUNTIME_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let specs = matrix();
    let requests: Vec<Vec<u8>> = specs.iter().map(|(_, s)| request(s)).collect();
    let rust = || {
        let t = Instant::now();
        for _ in 0..rounds {
            for (_, s) in &specs {
                std::hint::black_box(runtime::build(s).unwrap());
            }
        }
        t.elapsed()
    };
    let vesper = || {
        let t = Instant::now();
        for _ in 0..rounds {
            for r in &requests {
                std::hint::black_box(call(r));
            }
        }
        t.elapsed()
    };
    let _ = vesper();
    let (a1, b1, b2, a2) = (rust(), vesper(), vesper(), rust());
    let (r, v) = (a1 + a2, b1 + b2);
    eprintln!(
        "runtime build, {} specs x {rounds} x 2: Rust {:?}, Vesper {:?} ({:.2}x)",
        specs.len(),
        r,
        v,
        v.as_secs_f64() / r.as_secs_f64()
    );
}
