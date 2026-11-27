//! Self-hosting SH5 lane 11 (#1334): `@vesper/wit-component` against
//! wit-component 0.261, as src/wasm_backend/component.rs `encode` calls it.
//!
//! The inputs are every core module and WIT world the Rust wasm backend
//! wraps over SH4 lane 15's cases (tests/self_host_wasm_backend.rs; its
//! case list and Wasmtime runner are copied below so this binary does not
//! also run that harness's tests). Lane 15's 640 port-identical
//! compilations are among them: there the port's core module and WIT equal
//! Rust's.
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::wasm_backend::{self, EffectLowering, WasmArtifact, WasmOptions, WasmWorld};

// ------------------------------------------- cases (from lane 15's harness)

/// One program the backend is asked to compile.
pub struct WasmCase {
    pub name: String,
    pub world: World,
}

/// The program literals of tests/wasm_backend.rs (every string literal
/// that defines `main` and checks), then every corpus world with a `main`.
pub fn cases() -> &'static [WasmCase] {
    static C: OnceLock<Vec<WasmCase>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = vec![];
        let src = std::fs::read_to_string(repo().join("tests/wasm_backend.rs")).unwrap();
        let literals = string_literals(&src).into_iter().enumerate();
        let extra = EXTRA
            .iter()
            .enumerate()
            .map(|(i, s)| (1_000_000 + i, s.to_string()));
        for (i, lit) in literals.chain(extra) {
            if !lit.contains("fn ") {
                continue;
            }
            let p = vesper::parser::parse(&lit);
            if !p.diagnostics.is_empty() {
                continue;
            }
            let c = vesper::check::check(&p.module);
            if c.diagnostics.is_empty() {
                out.push(WasmCase {
                    name: format!("tests/wasm_backend.rs#{i}"),
                    world: c.world,
                });
                continue;
            }
            // tests/wasm_backend.rs `hosted_differential` programs: the
            // host capabilities (`Stdout`, `Clock`) are declared in front.
            let hosted = format!("{HOST_PRELUDE}{lit}");
            let p = vesper::parser::parse(&hosted);
            if !p.diagnostics.is_empty() {
                continue;
            }
            let c = vesper::check::check(&p.module);
            if c.diagnostics.is_empty() {
                out.push(WasmCase {
                    name: format!("tests/wasm_backend.rs#{i}+host"),
                    world: c.world,
                });
            }
        }
        for c in corpus() {
            if c.world.functions.contains_key("main") {
                out.push(WasmCase {
                    name: c.name.clone(),
                    world: c.world.clone(),
                });
            }
        }
        out
    })
}

/// Entries the backend's tests reach less often: string parameters (the
/// export's `cabi_realloc`) and Nexus plugins (`run`).
const EXTRA: &[&str] = &[
    "fn main(s: String) -> u64 { 1 }",
    "fn main(a: u64, sink s: String, sink t: String) -> (bool, String) { (t == \"abc\", s) }",
    "fn main(sink s: String) -> String { s }",
    "struct Named { name: String, value: i64 } fn main(sink n: Named) -> i64 { let Named { name, value } = n; value }",
    "fn main(x: u8, y: f64, sink s: String) -> (u8, f64) { (x, y) }",
    "fn main(sink s: String) -> Unit eff [Stdout] { Stdout::write(s); }",
    "fn main(sink s: String) -> u64 eff [Stdout, Clock] { Stdout::write(s); Clock::now_ns() }",
    "fn run(scenario: u32, arg: u64) -> i64 { 7 }",
    "fn run(scenario: u32, arg: u64) -> i64 { if scenario == 0 { 1 } else { -1 } }",
    "struct P { a: u64, b: i64 } fn run(scenario: u32, arg: u64) -> i64 { let p = P { a: arg, b: 2 }; p.b }",
];

/// tests/wasm_backend.rs `HOST_PRELUDE`.
const HOST_PRELUDE: &str =
    "linear effect Stdout { fn write(s: String); } linear effect Clock { fn now_ns() -> u64; } ";

/// The `"..."` and `r#"..."#` literals of a Rust source, unescaped.
fn string_literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'r' && b.get(i + 1) == Some(&b'#') && b.get(i + 2) == Some(&b'"') {
            let start = i + 3;
            let end = src[start..]
                .find("\"#")
                .map(|e| start + e)
                .unwrap_or(b.len());
            out.push(src[start..end].to_string());
            i = end + 2;
            continue;
        }
        if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            i += 3;
            continue;
        }
        if b[i] == b'"' {
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    match b.get(i) {
                        Some(b'n') => s.push('\n'),
                        Some(b't') => s.push('\t'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'"') => s.push('"'),
                        Some(b'\n') => {
                            while i + 1 < b.len() && (b[i + 1] == b' ' || b[i + 1] == b'\n') {
                                i += 1;
                            }
                        }
                        Some(c) => s.push(*c as char),
                        None => {}
                    }
                    i += 1;
                    continue;
                }
                let ch = src[i..].chars().next().unwrap();
                s.push(ch);
                i += ch.len_utf8();
            }
            out.push(s);
            i += 1;
            continue;
        }
        i += 1;
    }
    out
}

pub fn configs() -> [WasmOptions; 4] {
    let mk = |optimize, effects| WasmOptions {
        optimize,
        effects,
        ..WasmOptions::default()
    };
    [
        mk(true, EffectLowering::StackSwitching),
        mk(false, EffectLowering::StackSwitching),
        mk(true, EffectLowering::Portable),
        mk(false, EffectLowering::Portable),
    ]
}

fn engine(stack_switching: bool) -> wasmtime::Engine {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    if stack_switching {
        c.wasm_function_references(true);
        c.wasm_exceptions(true);
        c.wasm_stack_switching(true);
    }
    wasmtime::Engine::new(&c).expect("engine")
}

/// Run a nullary command `main` under Wasmtime; its result, or the trap.
pub fn run_main(component: &[u8], stack_switching: bool) -> String {
    use wasmtime::component::{Component, Linker, Val};
    let engine = engine(stack_switching);
    let component = match Component::new(&engine, component) {
        Ok(c) => c,
        Err(e) => return format!("load: {e}"),
    };
    let linker = Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    store.set_fuel(100_000_000).unwrap();
    let instance = match linker.instantiate(&mut store, &component) {
        Ok(i) => i,
        Err(e) => return format!("instantiate: {e}"),
    };
    let iface = instance
        .get_export_index(&mut store, None, wasm_backend::ENTRY_INTERFACE)
        .expect("entry interface");
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .expect("main export");
    let func = instance.get_func(&mut store, main).expect("main func");
    let mut results = vec![Val::Bool(false); func.ty(&store).results().len()];
    match func.call(&mut store, &[], &mut results) {
        Ok(()) => format!("{results:?}"),
        Err(_) => "trap".into(),
    }
}

// ------------------------------------------------------------ differential

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-wit-component"))
}

/// The port's input: the world kind, the WIT's length (u32 LE), the WIT,
/// the core module (packages/vesper-wit-component/src/lib.ves).
pub fn wrap_input(core: &[u8], wit: &str, world: WasmWorld) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + wit.len() + core.len());
    v.push(match world {
        WasmWorld::Command => 0,
        WasmWorld::NexusPlugin => 1,
    });
    v.extend_from_slice(&(wit.len() as u32).to_le_bytes());
    v.extend_from_slice(wit.as_bytes());
    v.extend_from_slice(core);
    v
}

/// The port's answer: `Ok(component)` or `Err(reason)` when it declines.
pub fn answer(words: &[u64]) -> Result<Vec<u8>, u64> {
    match words.first() {
        Some(0) => {
            let len = words[1] as usize;
            let mut bytes = Vec::with_capacity(len + 8);
            for w in &words[2..] {
                bytes.extend_from_slice(&w.to_le_bytes());
            }
            bytes.truncate(len);
            Ok(bytes)
        }
        Some(_) => Err(words.get(1).copied().unwrap_or(0)),
        None => Err(0),
    }
}

/// Every compilation Rust's backend completes, in case order: `main` of
/// each case under the four configurations, then (under the first) every
/// other function as a command entry (parameters, strings among them) and
/// `run` as a Nexus plugin.
fn rust_artifacts() -> &'static [(String, usize, WasmArtifact)] {
    &compiled().0
}

/// The compilations behind `rust_artifacts`, in its order: the case's
/// world, the entry and the options (the flip gate's `build` workload).
type Job = (&'static World, String, WasmOptions);

fn jobs() -> &'static [Job] {
    &compiled().1
}

#[allow(clippy::type_complexity)]
fn compiled() -> &'static (Vec<(String, usize, WasmArtifact)>, Vec<Job>) {
    static A: OnceLock<(Vec<(String, usize, WasmArtifact)>, Vec<Job>)> = OnceLock::new();
    A.get_or_init(|| {
        let quiet = quiet_panics();
        let compile = |w: &World, entry: &str, o: &WasmOptions| {
            std::panic::catch_unwind(|| wasm_backend::compile(w, entry, o))
                .ok()
                .and_then(Result::ok)
        };
        let mut out = vec![];
        let mut jobs: Vec<Job> = vec![];
        for c in cases().iter().step_by(step()) {
            for (k, o) in configs().iter().enumerate() {
                if let Some(a) = compile(&c.world, "main", o) {
                    out.push((c.name.clone(), k, a));
                    jobs.push((&c.world, "main".into(), o.clone()));
                }
            }
            let mut entries: Vec<&String> = c.world.functions.keys().collect();
            entries.sort();
            for entry in entries {
                if entry == "main" {
                    continue;
                }
                let o = &configs()[0];
                if let Some(a) = compile(&c.world, entry, o) {
                    out.push((format!("{}::{entry}", c.name), 0, a));
                    jobs.push((&c.world, entry.clone(), o.clone()));
                }
                let plugin = WasmOptions {
                    world: WasmWorld::NexusPlugin,
                    ..o.clone()
                };
                if let Some(a) = compile(&c.world, entry, &plugin) {
                    out.push((format!("{}::{entry} (plugin)", c.name), 0, a));
                    jobs.push((&c.world, entry.clone(), plugin));
                }
            }
        }
        drop(quiet);
        (out, jobs)
    })
}

/// Every `n`-th case (`WIT_COMPONENT_STEP`, default 3: compiling every case
/// with the Rust backend takes most of the test's time in a debug build;
/// the report's numbers are from `WIT_COMPONENT_STEP=1`).
fn step() -> usize {
    std::env::var("WIT_COMPONENT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3)
}

#[derive(Default, Debug)]
pub struct Tally {
    pub compared: usize,
    pub identical: usize,
    pub declined: usize,
    pub ran: usize,
    pub diffs: Vec<String>,
}

pub fn differential_run(execute: bool) -> Tally {
    let f = package().entry("wit_component_wrap", true);
    let mut t = Tally::default();
    for (name, k, a) in rust_artifacts() {
        t.compared += 1;
        let tag = format!("{name} [{k}]");
        let input = wrap_input(&a.core, &a.wit, a.world);
        match answer(&f.words(&input)) {
            Ok(c) if c == a.component => {
                t.identical += 1;
                if execute
                    && *k == 0
                    && a.params.is_empty()
                    && !a.stack_switching
                    && a.imports.is_empty()
                {
                    let ours = run_main(&c, false);
                    let theirs = run_main(&a.component, false);
                    if ours != theirs {
                        t.diffs.push(format!("{tag}: run {ours} vs {theirs}"));
                    }
                    t.ran += 1;
                }
            }
            Ok(c) => t.diffs.push(format!(
                "{tag}: component differs ({} vs {} bytes, first at {:?})",
                c.len(),
                a.component.len(),
                c.iter().zip(&a.component).position(|(x, y)| x != y)
            )),
            Err(reason) => {
                t.diffs.push(format!("{tag}: declined ({reason})"));
                t.declined += 1;
            }
        }
    }
    t
}

/// Every Rust component equals the port's, byte for byte: commands with
/// and without host imports, string parameters and Nexus plugins.
#[test]
fn port_matches_wit_component() {
    let t = differential_run(true);
    eprintln!(
        "compared {} identical {} declined {} ran {}",
        t.compared, t.identical, t.declined, t.ran
    );
    for d in t.diffs.iter().take(40) {
        eprintln!("DIFF {d}");
    }
    // scripts/ci/flip-gate/wit-component.toml reads this line.
    println!(
        "flip-gate differential differences={} cases={}",
        t.diffs.len(),
        t.compared
    );
    assert!(t.diffs.is_empty(), "{} differences", t.diffs.len());
    assert!(
        t.identical * step() >= 1000,
        "only {} identical",
        t.identical
    );
}

/// A custom section is inert at run time, but its bytes contribute to the
/// embedded core module's length. Exercise each side of two LEB128 width
/// transitions with an independently validated Rust component as oracle.
#[test]
fn custom_section_size_boundaries_match_rust() {
    use wasm_encoder::{CustomSection, Section};

    let parsed = vesper::parser::parse("fn main() -> i64 { 42 }");
    assert!(parsed.diagnostics.is_empty());
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty());
    let artifact = wasm_backend::compile(&checked.world, "main", &configs()[0]).unwrap();
    let wrap = package().entry("wit_component_wrap", true);

    for size in [126, 127, 128, 129, 16_382, 16_383, 16_384, 16_385] {
        // The one-byte name length and the name "x" occupy two payload bytes.
        let data: Vec<u8> = (0..size - 2).map(|i| (i % 251) as u8).collect();
        let mut core = artifact.core.clone();
        CustomSection {
            name: "x".into(),
            data: data.as_slice().into(),
        }
        .append_to(&mut core);
        let input = wrap_input(&core, &artifact.wit, artifact.world);
        let expected = rust_encode(core, &artifact.wit, artifact.world);
        let actual = answer(&wrap.words(&input))
            .unwrap_or_else(|reason| panic!("section size {size} declined with {reason}"));
        assert!(
            actual == expected,
            "section size {size}, first differing byte {:?}, lengths {} and {}",
            actual.iter().zip(&expected).position(|(a, b)| a != b),
            actual.len(),
            expected.len()
        );
    }
}

/// WIT outside the backend's grammar is declined, never misread.
#[test]
fn declines_outside_the_subset() {
    let f = package().entry("wit_component_wrap", true);
    let core = b"\0asm\x01\0\0\0";
    let good = "package vesper:program@0.1.0;\n\ninterface entry {\n  main: func() -> s64;\n}\n\nworld program {\n  export wasi:cli/run@0.2.0;\n  export entry;\n}\n";
    assert!(answer(&f.words(&wrap_input(core, good, WasmWorld::Command))).is_ok());
    for (wit, world) in [
        (good, WasmWorld::NexusPlugin),
        (
            &good.replace("vesper:program", "other:program") as &str,
            WasmWorld::Command,
        ),
        (&good.replace("s64", "list<u8>"), WasmWorld::Command),
        (&good.replace("s64", "%missing"), WasmWorld::Command),
        (
            &good.replace(
                "export entry;",
                "export entry;\n  import wasi:http/types@0.2.0;",
            ),
            WasmWorld::Command,
        ),
        (
            "package vesper:program@0.1.0;\n\nworld plugin {\n  export run: func(%s: string) -> s64;\n}\n",
            WasmWorld::NexusPlugin,
        ),
        (&good.replace("}\n\nworld", "\n\nworld"), WasmWorld::Command),
        (&good[..good.len() - 3], WasmWorld::Command),
    ] {
        let r = answer(&f.words(&wrap_input(core, wit, world)));
        assert!(r.is_err(), "accepted:\n{wit}");
    }
}

/// ABBA timing over the identical cases: Rust's `encode` (the five WIT
/// packages parsed, the world embedded, wit-component with validation)
/// against the port from its input. `CHECK_HARNESS_SPEED=1 --ignored`, in
/// a release build.
#[test]
#[ignore]
fn timing() {
    if !speed() {
        return;
    }
    let f = package().entry("wit_component_wrap", true);
    let work: Vec<_> = rust_artifacts()
        .iter()
        .map(|(_, _, a)| (a, wrap_input(&a.core, &a.wit, a.world)))
        .collect();
    let reps = 5;
    let mut rust = || {
        time(|| {
            for (a, _) in &work {
                for _ in 0..reps {
                    std::hint::black_box(rust_encode(a.core.clone(), &a.wit, a.world));
                }
            }
        })
    };
    let mut port = || {
        time(|| {
            for (_, input) in &work {
                for _ in 0..reps {
                    std::hint::black_box(f.words(input));
                }
            }
        })
    };
    eprintln!("{} components, {} reps", work.len(), reps);
    abba("wit-component wrap", &mut rust, &mut port);
}

/// The port as the `component` hook of `vesper::codegen_swap`.
fn port_component(core: &[u8], wit: &str, world: WasmWorld) -> Option<Vec<u8>> {
    static F: OnceLock<Entry> = OnceLock::new();
    let f = F.get_or_init(|| package().entry("wit_component_wrap", true));
    answer(&f.words(&wrap_input(core, wit, world))).ok()
}

/// The flip gate's ABBA workloads (scripts/ci/flip-gate/wit-component.toml;
/// `CHECK_HARNESS_SPEED=1`, release build). Each epoch runs Rust, Vesper,
/// Vesper, Rust and prints one pair per epoch in the gate's
/// `flip-gate pairs` format (`WIT_COMPONENT_GATE_EPOCHS`, default 12;
/// `WIT_COMPONENT_GATE_WORKLOAD` runs only one):
/// - `wrap`: every component from its core module and WIT, Rust's
///   `component::encode` against the port.
/// - `build`: `wasm_backend::compile` of every input (the whole wasm
///   build, backend and wrap) without and with the port as the
///   `component` hook.
#[test]
#[ignore]
fn flip_gate() {
    if !speed() {
        return;
    }
    let epochs: usize = std::env::var("WIT_COMPONENT_GATE_EPOCHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let only = std::env::var("WIT_COMPONENT_GATE_WORKLOAD").ok();
    let wants = |w: &str| only.as_deref().is_none_or(|o| o == w);
    let f = package().entry("wit_component_wrap", true);
    let work: Vec<_> = rust_artifacts()
        .iter()
        .map(|(_, _, a)| (a, wrap_input(&a.core, &a.wit, a.world)))
        .collect();
    let rust = || {
        time(|| {
            for (a, _) in &work {
                std::hint::black_box(rust_encode(a.core.clone(), &a.wit, a.world));
            }
        })
        .as_nanos()
    };
    let port = || {
        time(|| {
            for (_, input) in &work {
                std::hint::black_box(f.words(input));
            }
        })
        .as_nanos()
    };
    let hooks = vesper::codegen_swap::CodegenHooks {
        component: Some(port_component),
        ..Default::default()
    };
    let build = |vesper: bool| {
        let run = || {
            time(|| {
                for (w, entry, o) in jobs() {
                    std::hint::black_box(wasm_backend::compile(w, entry, o).ok());
                }
            })
            .as_nanos()
        };
        if vesper {
            vesper::codegen_swap::with_hooks(hooks, run)
        } else {
            run()
        }
    };
    let (mut wrap_pairs, mut build_pairs) = (String::new(), String::new());
    // Warm-up, untimed; the hooked build must give Rust's components.
    if wants("wrap") {
        rust();
        port();
    }
    if wants("build") {
        build(false);
        build(true);
        let same = vesper::codegen_swap::with_hooks(hooks, || {
            jobs()
                .iter()
                .zip(rust_artifacts())
                .all(|((w, e, o), (_, _, a))| {
                    wasm_backend::compile(w, e, o).is_ok_and(|b| b.component == a.component)
                })
        });
        assert!(same, "the hooked build's components differ from Rust's");
    }
    for _ in 0..epochs {
        if wants("wrap") {
            let (r1, v1, v2, r2) = (rust(), port(), port(), rust());
            wrap_pairs += &format!(" {}:{}", r1 + r2, v1 + v2);
        }
        if wants("build") {
            let (r1, v1, v2, r2) = (build(false), build(true), build(true), build(false));
            build_pairs += &format!(" {}:{}", r1 + r2, v1 + v2);
        }
    }
    eprintln!("{} components", work.len());
    if wants("wrap") {
        println!("flip-gate pairs wrap{wrap_pairs}");
    }
    if wants("build") {
        println!("flip-gate pairs build{build_pairs}");
    }
}

#[test]
fn std_env_and_std_fs_match_rust() {
    use wasm_encoder::{
        CodeSection, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
        Instruction as I, MemorySection, MemoryType, Module, TypeSection, ValType,
    };

    const CLOCKS_ALL: &str = "package wasi:clocks@0.2.0;\n\ninterface monotonic-clock {\n  type instant = u64;\n  now: func() -> instant;\n}\n\ninterface wall-clock {\n  record datetime {\n    seconds: u64,\n    nanoseconds: u32,\n  }\n}\n";
    const CLI_ALL: &str = "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n\ninterface stderr {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stderr: func() -> output-stream;\n}\n\ninterface environment {\n  get-environment: func() -> list<tuple<string, string>>;\n  get-arguments: func() -> list<string>;\n  initial-cwd: func() -> option<string>;\n}\n";
    const FS_ALL: &str = r#"package wasi:filesystem@0.2.0;

interface types {
  use wasi:clocks/wall-clock@0.2.0.{datetime};
  type filesize = u64;
  enum descriptor-type {
    unknown,
    block-device,
    character-device,
    directory,
    fifo,
    symbolic-link,
    regular-file,
    socket,
  }
  flags descriptor-flags {
    read,
    write,
    file-integrity-sync,
    data-integrity-sync,
    requested-write-sync,
    mutate-directory,
  }
  flags path-flags {
    symlink-follow,
  }
  flags open-flags {
    create,
    directory,
    exclusive,
    truncate,
  }
  type link-count = u64;
  record descriptor-stat {
    %type: descriptor-type,
    link-count: link-count,
    size: filesize,
    data-access-timestamp: option<datetime>,
    data-modification-timestamp: option<datetime>,
    status-change-timestamp: option<datetime>,
  }
  record directory-entry {
    %type: descriptor-type,
    name: string,
  }
  enum error-code {
    access,
    would-block,
    already,
    bad-descriptor,
    busy,
    deadlock,
    quota,
    exist,
    file-too-large,
    illegal-byte-sequence,
    in-progress,
    interrupted,
    invalid,
    io,
    is-directory,
    loop,
    too-many-links,
    message-size,
    name-too-long,
    no-device,
    no-entry,
    no-lock,
    insufficient-memory,
    insufficient-space,
    not-directory,
    not-empty,
    not-recoverable,
    unsupported,
    no-tty,
    no-such-device,
    overflow,
    not-permitted,
    pipe,
    read-only,
    invalid-seek,
    text-file-busy,
    cross-device,
  }
  resource descriptor {
    read: func(length: filesize, offset: filesize) -> result<tuple<list<u8>, bool>, error-code>;
    write: func(buffer: list<u8>, offset: filesize) -> result<filesize, error-code>;
    read-directory: func() -> result<directory-entry-stream, error-code>;
    sync: func() -> result<_, error-code>;
    create-directory-at: func(path: string) -> result<_, error-code>;
    stat: func() -> result<descriptor-stat, error-code>;
    stat-at: func(path-flags: path-flags, path: string) -> result<descriptor-stat, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    remove-directory-at: func(path: string) -> result<_, error-code>;
    rename-at: func(old-path: string, new-descriptor: borrow<descriptor>, new-path: string) -> result<_, error-code>;
    unlink-file-at: func(path: string) -> result<_, error-code>;
  }
  resource directory-entry-stream {
    read-directory-entry: func() -> result<option<directory-entry>, error-code>;
  }
}

interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

    fn make_core(
        imps: &[(&str, &str, Vec<ValType>, Vec<ValType>)],
        with_str_param: bool,
    ) -> Vec<u8> {
        let mut types = TypeSection::new();
        let mut imports = ImportSection::new();
        for (idx, (m, f, params, results)) in imps.iter().enumerate() {
            types.ty().function(params.clone(), results.clone());
            imports.import(m, f, wasm_encoder::EntityType::Function(idx as u32));
        }
        let ti_main = imps.len() as u32;
        if with_str_param {
            types
                .ty()
                .function([ValType::I32, ValType::I32], [ValType::I64]);
        } else {
            types.ty().function([], [ValType::I64]);
        }
        let ti_run = ti_main + 1;
        types.ty().function([], [ValType::I32]);
        let ti_realloc = ti_main + 2;
        types.ty().function(
            [ValType::I32, ValType::I32, ValType::I32, ValType::I32],
            [ValType::I32],
        );

        let mut funcs = FunctionSection::new();
        funcs.function(ti_main);
        funcs.function(ti_run);
        funcs.function(ti_realloc);

        let mut mem = MemorySection::new();
        mem.memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        let mut exports = ExportSection::new();
        let base = imps.len() as u32;
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("vesper:program/entry@0.1.0#main", ExportKind::Func, base);
        exports.export("wasi:cli/run@0.2.0#run", ExportKind::Func, base + 1);
        exports.export("cabi_realloc", ExportKind::Func, base + 2);

        let mut code = CodeSection::new();
        let mut f0 = Function::new([]);
        f0.instruction(&I::I64Const(0));
        f0.instruction(&I::End);
        code.function(&f0);
        let mut f1 = Function::new([]);
        f1.instruction(&I::I32Const(0));
        f1.instruction(&I::End);
        code.function(&f1);
        let mut f2 = Function::new([]);
        f2.instruction(&I::I32Const(0));
        f2.instruction(&I::End);
        code.function(&f2);

        let mut m = Module::new();
        m.section(&types);
        m.section(&imports);
        m.section(&funcs);
        m.section(&mem);
        m.section(&exports);
        m.section(&code);
        m.finish()
    }

    fn encode_rust(core_in: &[u8], wit: &str) -> Vec<u8> {
        let mut core = core_in.to_vec();
        let mut resolve = wit_parser::Resolve::default();
        for (name, source) in [
            ("wasi-io.wit", HOST_WIT[0].1),
            ("wasi-cli.wit", CLI_ALL),
            ("wasi-clocks.wit", CLOCKS_ALL),
            ("wasi-filesystem.wit", FS_ALL),
            ("vesper-beacon.wit", HOST_WIT[3].1),
        ] {
            resolve.push_source(name, source).unwrap();
        }
        let pkg = resolve.push_source("vesper-program.wit", wit).unwrap();
        let id = resolve.select_world(&[pkg], Some("program")).unwrap();
        wit_component::embed_component_metadata(
            &mut core,
            &resolve,
            id,
            wit_component::StringEncoding::UTF8,
            false,
        )
        .unwrap();
        wit_component::ComponentEncoder::default()
            .validate(true)
            .module(&core)
            .unwrap()
            .encode()
            .unwrap()
    }

    let env_imps = vec![
        (
            "wasi:cli/environment@0.2.0",
            "get-arguments",
            vec![ValType::I32],
            vec![],
        ),
        (
            "wasi:cli/environment@0.2.0",
            "get-environment",
            vec![ValType::I32],
            vec![],
        ),
        (
            "wasi:cli/environment@0.2.0",
            "initial-cwd",
            vec![ValType::I32],
            vec![],
        ),
    ];
    let fs_imps = vec![
        (
            "wasi:filesystem/preopens@0.2.0",
            "get-directories",
            vec![ValType::I32],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.open-at",
            vec![ValType::I32; 7],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.read",
            vec![ValType::I32, ValType::I64, ValType::I64, ValType::I32],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.write",
            vec![
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I64,
                ValType::I32,
            ],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.stat",
            vec![ValType::I32; 2],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.stat-at",
            vec![ValType::I32; 5],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.sync",
            vec![ValType::I32; 2],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.create-directory-at",
            vec![ValType::I32; 4],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.remove-directory-at",
            vec![ValType::I32; 4],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.unlink-file-at",
            vec![ValType::I32; 4],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.rename-at",
            vec![ValType::I32; 7],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]descriptor.read-directory",
            vec![ValType::I32; 2],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[method]directory-entry-stream.read-directory-entry",
            vec![ValType::I32; 2],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[resource-drop]descriptor",
            vec![ValType::I32],
            vec![],
        ),
        (
            "wasi:filesystem/types@0.2.0",
            "[resource-drop]directory-entry-stream",
            vec![ValType::I32],
            vec![],
        ),
    ];
    let mut both = env_imps.clone();
    both.extend(fs_imps.clone());
    let mut all_five = both.clone();
    all_five.extend([
        (
            "wasi:cli/stdout@0.2.0",
            "get-stdout",
            vec![],
            vec![ValType::I32],
        ),
        (
            "wasi:io/streams@0.2.0",
            "[method]output-stream.blocking-write-and-flush",
            vec![ValType::I32; 4],
            vec![],
        ),
        (
            "wasi:io/streams@0.2.0",
            "[resource-drop]output-stream",
            vec![ValType::I32],
            vec![],
        ),
        (
            "wasi:clocks/monotonic-clock@0.2.0",
            "now",
            vec![],
            vec![ValType::I64],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "current-ctx",
            vec![ValType::I32],
            vec![],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "start-span",
            vec![ValType::I32; 4],
            vec![],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "start-child",
            vec![
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I32,
            ],
            vec![],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "emit-ok",
            vec![
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
            ],
            vec![],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "emit-err",
            vec![
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I32,
            ],
            vec![],
        ),
        (
            "vesper:beacon/trace@0.1.0",
            "emit-attr",
            vec![
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I64,
            ],
            vec![],
        ),
    ]);

    let wrap = package().entry("wit_component_wrap", true);
    let cases = [
        (
            "StdEnv",
            &env_imps[..],
            "  import wasi:cli/environment@0.2.0;\n",
            false,
        ),
        (
            "StdEnv+StrParam",
            &env_imps[..],
            "  import wasi:cli/environment@0.2.0;\n",
            true,
        ),
        (
            "StdFs",
            &fs_imps[..],
            "  import wasi:filesystem/types@0.2.0;\n  import wasi:filesystem/preopens@0.2.0;\n",
            false,
        ),
        (
            "StdEnv+StdFs",
            &both[..],
            "  import wasi:cli/environment@0.2.0;\n  import wasi:filesystem/types@0.2.0;\n  import wasi:filesystem/preopens@0.2.0;\n",
            false,
        ),
        (
            "StdEnv+StdFs+StrParam",
            &both[..],
            "  import wasi:cli/environment@0.2.0;\n  import wasi:filesystem/types@0.2.0;\n  import wasi:filesystem/preopens@0.2.0;\n",
            true,
        ),
        (
            "AllFive",
            &all_five[..],
            "  import wasi:cli/stdout@0.2.0;\n  import wasi:clocks/monotonic-clock@0.2.0;\n  import vesper:beacon/trace@0.1.0;\n  import wasi:cli/environment@0.2.0;\n  import wasi:filesystem/types@0.2.0;\n  import wasi:filesystem/preopens@0.2.0;\n",
            false,
        ),
        (
            "AllFiveReversed",
            &all_five[..],
            "  import wasi:filesystem/preopens@0.2.0;\n  import wasi:filesystem/types@0.2.0;\n  import wasi:cli/environment@0.2.0;\n  import vesper:beacon/trace@0.1.0;\n  import wasi:clocks/monotonic-clock@0.2.0;\n  import wasi:cli/stdout@0.2.0;\n",
            false,
        ),
    ];

    let mut errs = vec![];
    for (label, imps, wit_imports, with_str) in cases {
        let core = make_core(imps, with_str);
        let main_sig = if with_str {
            "main: func(s: string) -> s64;"
        } else {
            "main: func() -> s64;"
        };
        let wit = format!(
            "package vesper:program@0.1.0;\n\ninterface entry {{\n  {main_sig}\n}}\n\nworld program {{\n{wit_imports}  export wasi:cli/run@0.2.0;\n  export entry;\n}}\n"
        );
        let expected = encode_rust(&core, &wit);
        match answer(&wrap.words(&wrap_input(&core, &wit, WasmWorld::Command))) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => {
                let pos = actual.iter().zip(&expected).position(|(a, b)| a != b);
                let slice = |b: &[u8]| {
                    let s = pos.unwrap_or(0).saturating_sub(8);
                    let e = (pos.unwrap_or(0) + 24).min(b.len());
                    b[s..e]
                        .iter()
                        .map(|x| format!("{x:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                errs.push(format!(
                    "{label}: differs (actual {} vs expected {}, first byte {pos:?}\n  act: {}\n  exp: {})",
                    actual.len(),
                    expected.len(),
                    slice(&actual),
                    slice(&expected)
                ));
            }
            Err(reason) => errs.push(format!("{label}: declined ({reason})")),
        }
    }
    assert!(errs.is_empty(), "diffs:\n{}", errs.join("\n"));
}

/// src/wasm_backend/component.rs `encode` (the module is crate-private).
fn rust_encode(mut core: Vec<u8>, wit: &str, world: WasmWorld) -> Vec<u8> {
    let mut resolve = wit_parser::Resolve::default();
    for (name, source) in HOST_WIT {
        resolve.push_source(name, source).unwrap();
    }
    let pkg = resolve.push_source("vesper-program.wit", wit).unwrap();
    let name = match world {
        WasmWorld::Command => "program",
        WasmWorld::NexusPlugin => "plugin",
    };
    let id = resolve.select_world(&[pkg], Some(name)).unwrap();
    wit_component::embed_component_metadata(
        &mut core,
        &resolve,
        id,
        wit_component::StringEncoding::UTF8,
        false,
    )
    .unwrap();
    wit_component::ComponentEncoder::default()
        .validate(true)
        .module(&core)
        .unwrap()
        .encode()
        .unwrap()
}

/// src/wasm_backend/host.rs `IO_WIT`, `CLI_WIT`, `CLOCKS_WIT`, `BEACON_WIT` (crate-private).
const HOST_WIT: [(&str, &str); 4] = [
    (
        "wasi-io.wit",
        "package wasi:io@0.2.0;\n\ninterface error {\n  resource error;\n}\n\ninterface streams {\n  use error.{error};\n  variant stream-error {\n    last-operation-failed(error),\n    closed,\n  }\n  resource output-stream {\n    blocking-write-and-flush: func(contents: list<u8>) -> result<_, stream-error>;\n  }\n}\n",
    ),
    (
        "wasi-cli.wit",
        "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n",
    ),
    (
        "wasi-clocks.wit",
        "package wasi:clocks@0.2.0;\n\ninterface monotonic-clock {\n  type instant = u64;\n  now: func() -> instant;\n}\n",
    ),
    (
        "vesper-beacon.wit",
        "package vesper:beacon@0.1.0;

interface trace {
  record span-context {
    trace-id-hi: u64,
    trace-id-lo: u64,
    span-id: u64,
    %flags: u8,
  }

  record active-span {
    ctx: span-context,
    parent-span-id: u64,
    start-mono-ns: u64,
    schema-id: u32,
  }

  variant telemetry-attr {
    no-attr,
    count(u64),
    bytes(u64),
    redacted,
  }

  current-ctx: func() -> span-context;
  start-span: func(schema-id: u32, name: string) -> active-span;
  start-child: func(parent: span-context, schema-id: u32, name: string) -> active-span;
  emit-ok: func(span: active-span, attr-u64: u64);
  emit-err: func(span: active-span, err-code: u32, msg: string);
  emit-attr: func(span: active-span, attr: telemetry-attr);
}
",
    ),
];

/// Write every `component::encode` input over the cases (`WIT_DUMP=<dir>`):
/// `<n>.core`, `<n>.wit`, `<n>.component` and `<n>.meta`. Part 1's
/// coverage driver reads these.
#[test]
#[ignore]
fn dump_wrap_inputs() {
    let Some(dir) = std::env::var_os("WIT_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (n, (name, k, a)) in rust_artifacts().iter().enumerate() {
        let w = match a.world {
            WasmWorld::Command => "program",
            WasmWorld::NexusPlugin => "plugin",
        };
        std::fs::write(dir.join(format!("{n}.core")), &a.core).unwrap();
        std::fs::write(dir.join(format!("{n}.wit")), &a.wit).unwrap();
        std::fs::write(dir.join(format!("{n}.component")), &a.component).unwrap();
        std::fs::write(
            dir.join(format!("{n}.meta")),
            format!(
                "{w}\n{name} [{k}]\nstack_switching={}\nimports={}\nhandlers={}\n",
                a.stack_switching,
                a.imports.len(),
                a.handlers.len()
            ),
        )
        .unwrap();
    }
    eprintln!("dumped {}", rust_artifacts().len());
}
