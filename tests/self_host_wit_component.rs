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
        for (i, lit) in string_literals(&src).into_iter().enumerate() {
            if !lit.contains("fn main") {
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

/// Every compilation Rust's backend completes, in case order.
fn rust_artifacts() -> &'static [(String, usize, WasmArtifact)] {
    static A: OnceLock<Vec<(String, usize, WasmArtifact)>> = OnceLock::new();
    A.get_or_init(|| {
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut out = vec![];
        for c in cases().iter().step_by(step()) {
            for (k, o) in configs().iter().enumerate() {
                if let Ok(Ok(a)) =
                    std::panic::catch_unwind(|| wasm_backend::compile(&c.world, "main", o))
                {
                    out.push((c.name.clone(), k, a));
                }
            }
        }
        std::panic::set_hook(quiet);
        out
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
    pub declined_imports: usize,
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
                // Host imports (the WIT `import`s) are the one shape the
                // port leaves to Rust (wit.ves `E_IMPORT`).
                if a.wit.contains("\n  import ") && reason == 16 + 4 {
                    t.declined_imports += 1;
                } else {
                    t.diffs.push(format!("{tag}: declined ({reason})"));
                }
                t.declined += 1;
            }
        }
    }
    t
}

/// Every Rust component equals the port's, byte for byte, except the worlds
/// with host imports, which the port declines.
#[test]
fn port_matches_wit_component() {
    let t = differential_run(true);
    eprintln!(
        "compared {} identical {} declined {} (host imports {}) ran {}",
        t.compared, t.identical, t.declined, t.declined_imports, t.ran
    );
    for d in t.diffs.iter().take(40) {
        eprintln!("DIFF {d}");
    }
    assert!(t.diffs.is_empty(), "{} differences", t.diffs.len());
    assert!(
        t.identical * step() >= 1000,
        "only {} identical",
        t.identical
    );
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
                "export entry;\n  import wasi:cli/stdout@0.2.0;",
            ),
            WasmWorld::Command,
        ),
        (
            &good.replace("func()", "func(%s: string)"),
            WasmWorld::Command,
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
        .filter(|(_, _, a)| a.imports.is_empty() && !a.wit.contains("\n  import "))
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
