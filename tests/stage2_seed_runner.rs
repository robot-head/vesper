//! The bootstrap seed's C wasm runner (`bootstrap/seed/runner/vwasm.c`,
//! SH6 lane 2, #1397) against Wasmtime on everything the wasm backend
//! builds.
//!
//! Every corpus world with a parameterless `main` that the wasm backend
//! compiles (portable effect lowering, with and without the IR optimizer)
//! runs twice: in Wasmtime's core-module API and in `vwasm`, both on the
//! artifact's core module with the same WASI 0.2 imports. The two must agree
//! on trap versus return, the flat results (bit patterns), the bytes written
//! to stdout and a digest of the whole linear memory after the call.
//!
//! The runner is compiled from source with the host C compiler (`$CC`, else
//! `cc`; MSVC's `cl.exe` on Windows), as a seed user would build it.
#[path = "common/check_harness.rs"]
mod check_harness;

use std::path::{Path, PathBuf};
use std::process::Command;
use vesper::ast::Type;
use vesper::wasm_backend::{self, EffectLowering, WasmOptions};
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, Val};

const FUEL: u64 = 20_000_000;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-stage2-seed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build the runner with the host C compiler.
fn build_runner(dir: &Path) -> PathBuf {
    let src = check_harness::repo().join("bootstrap/seed/runner/vwasm.c");
    let exe = dir.join(if cfg!(windows) { "vwasm.exe" } else { "vwasm" });
    let status = if cfg!(windows) && std::env::var_os("CC").is_none() {
        let target = if cfg!(target_arch = "aarch64") {
            "aarch64-pc-windows-msvc"
        } else {
            "x86_64-pc-windows-msvc"
        };
        let mut cmd = find_cl(target);
        cmd.arg("/nologo")
            .arg("/O2")
            .arg("/std:c11")
            .arg(format!("/Fo{}\\", dir.display()))
            .arg(format!("/Fe{}", exe.display()))
            .arg(&src)
            .status()
    } else {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        Command::new(cc)
            .args(["-std=c11", "-O2", "-o"])
            .arg(&exe)
            .arg(&src)
            .arg("-lm")
            .status()
    };
    let status = status.expect("run the C compiler for bootstrap/seed/runner/vwasm.c");
    assert!(status.success(), "building vwasm failed: {status}");
    exe
}

#[cfg(windows)]
fn find_cl(target: &str) -> Command {
    find_msvc_tools::find_tool(target, "cl.exe")
        .expect("cl.exe (Visual Studio C compiler)")
        .to_command()
}
#[cfg(not(windows))]
fn find_cl(_: &str) -> Command {
    unreachable!()
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Trap,
    Ok {
        results: Vec<u64>,
        pages: u64,
        memory: u64,
        stdout: Vec<u8>,
    },
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    h
}

fn engine() -> Engine {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.wasm_tail_call(true);
    Engine::new(&config).unwrap()
}

/// Run `export` in Wasmtime; `None` when it runs out of fuel.
fn wasmtime_run(engine: &Engine, core: &[u8], export: &str) -> Option<Outcome> {
    let module = Module::new(engine, core).expect("core module");
    let mut linker = Linker::<Vec<u8>>::new(engine);
    linker
        .func_wrap("wasi:cli/stdout@0.2.0", "get-stdout", || 1i32)
        .unwrap();
    linker
        .func_wrap(
            "wasi:io/streams@0.2.0",
            "[method]output-stream.blocking-write-and-flush",
            |mut c: Caller<'_, Vec<u8>>, _this: i32, ptr: i32, len: i32, ret: i32| {
                let mem = c.get_export("memory").unwrap().into_memory().unwrap();
                let mut buf = vec![0; len as u32 as usize];
                mem.read(&c, ptr as u32 as usize, &mut buf).unwrap();
                c.data_mut().extend_from_slice(&buf);
                mem.write(&mut c, ret as u32 as usize, &[0; 12]).unwrap();
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "wasi:io/streams@0.2.0",
            "[resource-drop]output-stream",
            |_: i32| {},
        )
        .unwrap();
    // Clock values differ between runs; programs that read the clock are
    // filtered out below, so this stub is never compared.
    linker
        .func_wrap("wasi:clocks/monotonic-clock@0.2.0", "now", || 0i64)
        .unwrap();
    let mut store = Store::new(engine, Vec::new());
    store.set_fuel(FUEL).unwrap();
    let instance = linker.instantiate(&mut store, &module).ok()?;
    let func = instance.get_func(&mut store, export).expect("entry export");
    let ty = func.ty(&store);
    let mut results: Vec<Val> = ty.results().map(|_| Val::I32(0)).collect();
    if let Err(e) = func.call(&mut store, &[], &mut results) {
        if matches!(
            e.downcast_ref::<wasmtime::Trap>(),
            Some(wasmtime::Trap::OutOfFuel)
        ) {
            return None;
        }
        return Some(Outcome::Trap);
    }
    let mem = instance.get_memory(&mut store, "memory").unwrap();
    let data = mem.data(&store);
    let results = results
        .iter()
        .map(|v| match v {
            Val::I32(x) => u64::from(*x as u32),
            Val::I64(x) => *x as u64,
            Val::F32(x) => u64::from(*x),
            Val::F64(x) => *x,
            other => panic!("unexpected result {other:?}"),
        })
        .collect();
    Some(Outcome::Ok {
        results,
        pages: (data.len() / 65536) as u64,
        memory: fnv(data),
        stdout: store.into_data(),
    })
}

fn vwasm_run(runner: &Path, dir: &Path, core: &[u8], export: &str, tag: usize) -> Outcome {
    let module = dir.join(format!("case{tag}.wasm"));
    let report = dir.join(format!("case{tag}.report"));
    std::fs::write(&module, core).unwrap();
    let _ = std::fs::remove_file(&report);
    let out = Command::new(runner)
        .arg("--invoke")
        .arg(export)
        .arg("--report")
        .arg(&report)
        .arg(&module)
        .output()
        .unwrap();
    let text = std::fs::read_to_string(&report).unwrap_or_else(|_| {
        panic!(
            "vwasm wrote no report (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let mut lines = text.lines();
    match lines.next() {
        Some("trap") => Outcome::Trap,
        Some("ok") => {
            let mut results = vec![];
            let mut pages = 0;
            let mut memory = 0;
            for l in lines {
                if let Some(rest) = l.strip_prefix("memory ") {
                    let (p, h) = rest.split_once(' ').unwrap();
                    pages = p.parse().unwrap();
                    memory = u64::from_str_radix(h, 16).unwrap();
                } else {
                    results.push(u64::from_str_radix(l, 16).unwrap());
                }
            }
            Outcome::Ok {
                results,
                pages,
                memory,
                stdout: out.stdout,
            }
        }
        other => panic!("bad vwasm report {other:?}: {text}"),
    }
}

/// A world's `main` the backend can build: no parameters, no clock.
fn eligible(world: &vesper::check::World) -> bool {
    let Some(f) = world.functions.get("main") else {
        return false;
    };
    f.params.is_empty() && !matches!(f.result, Type::Named(ref n, _) if n == "Never")
}

#[test]
fn vwasm_matches_wasmtime_on_the_corpus() {
    let dir = scratch().join("corpus");
    std::fs::create_dir_all(&dir).unwrap();
    let runner = build_runner(&dir);
    let engine = engine();
    let (mut compared, mut traps, mut fuel_skips, mut refused) = (0usize, 0usize, 0usize, 0usize);
    let mut diffs = vec![];
    let mut tag = 0;
    for case in check_harness::corpus() {
        if !eligible(&case.world) {
            continue;
        }
        for optimize in [false, true] {
            let options = WasmOptions {
                optimize,
                effects: EffectLowering::Portable,
                ..WasmOptions::default()
            };
            let art = match std::panic::catch_unwind(|| {
                wasm_backend::compile(&case.world, "main", &options)
            }) {
                Ok(Ok(art)) => art,
                _ => {
                    refused += 1;
                    continue;
                }
            };
            if art.imports.contains(&wasm_backend::HostCapability::Clock) {
                continue;
            }
            let export = format!("{}#main", wasm_backend::ENTRY_INTERFACE);
            let Some(expected) = wasmtime_run(&engine, &art.core, &export) else {
                fuel_skips += 1;
                continue;
            };
            tag += 1;
            let got = vwasm_run(&runner, &dir, &art.core, &export, tag);
            if expected == Outcome::Trap {
                traps += 1;
            }
            compared += 1;
            if got != expected {
                diffs.push(format!(
                    "{} (optimize={optimize}): wasmtime {expected:?}, vwasm {got:?}",
                    case.name
                ));
            }
        }
    }
    eprintln!(
        "vwasm differential: {compared} runs compared ({traps} traps), {fuel_skips} over the fuel limit, {refused} not built by the backend, {} differ",
        diffs.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
    assert!(compared >= 50, "too few programs compared: {compared}");
}

// ---- the Stage 1 front end, built to wasm, under the C runner ------------

/// The Stage 1 front-end sources, in bootstrap/stage1.rs's order.
const FRONTEND: [&str; 5] = [
    "packages/vesper-compiler/src/lexer.ves",
    "packages/vesper-compiler/src/cursor.ves",
    "packages/vesper-compiler/src/types_items.ves",
    "packages/vesper-compiler/src/expressions.ves",
    "packages/vesper-compiler/src/driver.ves",
];

/// bootstrap/stage1.rs's `source_module`: the input as `source_len()` and
/// `source_byte(i)`, in 64-byte leaves under eight-way nodes.
fn source_module(bytes: &[u8]) -> String {
    const EFFECTS: &str = "eff [Fail[RuntimeError], Fail[ArithOverflow]]";
    fn tree(group: &[(String, usize)]) -> String {
        if group.len() == 1 {
            return format!("{}(i)", group[0].0);
        }
        let mid = group.len() / 2;
        format!(
            "if i < {} {{ {} }} else {{ {} }}",
            group[mid].1,
            tree(&group[..mid]),
            tree(&group[mid..])
        )
    }
    let mut out = String::from("\n");
    let mut nodes = Vec::new();
    for (k, chunk) in bytes.chunks(64).enumerate() {
        let mut values: Vec<_> = chunk.iter().map(u8::to_string).collect();
        values.resize(64, "0".into());
        let name = format!("source_leaf_{k}");
        out.push_str(&format!(
            "fn {name}(i: i64) -> i64 {EFFECTS} {{ ({})[i - {}] }}\n",
            values.join(","),
            k * 64
        ));
        nodes.push((name, k * 64));
    }
    let mut level = 0;
    while nodes.len() > 1 {
        let mut next = Vec::new();
        for (i, group) in nodes.chunks(8).enumerate() {
            let name = format!("source_node_{level}_{i}");
            out.push_str(&format!(
                "fn {name}(i: i64) -> i64 {EFFECTS} {{ {} }}\n",
                tree(group)
            ));
            next.push((name, group[0].1));
        }
        nodes = next;
        level += 1;
    }
    let top = nodes
        .first()
        .map_or_else(|| "-1".into(), |n| format!("{}(i)", n.0));
    let n = bytes.len();
    out.push_str(&format!(
        "fn source_len() -> i64 {{ {n} }}\n\
         fn source_byte(i: i64) -> i64 {{ if i < 0 || i >= {n} {{ -1 }} else {{ \
         handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ {top} }} with {{ \
         return v => v, raise(e, r) => -1, }} }} with {{ return v => v, raise(e, r) => -1, }} }} }}\n"
    ));
    out
}

/// `(name, result)` of every program in bootstrap/stage1.lock.
fn stage1_results() -> Vec<(String, i64)> {
    let lock =
        std::fs::read_to_string(check_harness::repo().join("bootstrap/stage1.lock")).unwrap();
    let mut out = vec![];
    let mut name = None;
    for line in lock.lines() {
        if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_owned());
        } else if let Some(v) = line.strip_prefix("result = ") {
            out.push((name.take().unwrap(), v.parse().unwrap()));
        }
    }
    out
}

/// The Stage 1 front end (packages/vesper-compiler), compiled by the wasm
/// backend and run by the C runner, computes every result that
/// bootstrap/stage1.lock records for the Cranelift build: the CST digest of
/// each bootstrap corpus program and the lexer's byte-class checksum.
#[test]
fn stage1_front_end_runs_under_vwasm() {
    let root = check_harness::repo();
    let dir = scratch().join("stage1");
    std::fs::create_dir_all(&dir).unwrap();
    let runner = build_runner(&dir);
    let frontend: Vec<String> = FRONTEND
        .iter()
        .map(|p| std::fs::read_to_string(root.join(p)).unwrap())
        .collect();
    let frontend = frontend.join("\n");
    let expected = stage1_results();
    assert_eq!(expected.len(), 11, "stage1.lock programs");
    for (tag, (name, result)) in expected.iter().enumerate() {
        let (text, entry) = if name == "lexer-byte-classes" {
            let probe =
                std::fs::read_to_string(root.join("bootstrap/byte-class-probe.ves")).unwrap();
            (
                format!("{frontend}{}{probe}", source_module(&[])),
                "byte_class_probe",
            )
        } else {
            let source = std::fs::read(root.join(format!("bootstrap/corpus/{name}.ves"))).unwrap();
            (
                format!("{frontend}{}", source_module(&source)),
                "parse_main",
            )
        };
        let limits = vesper::parser::ParseLimits {
            max_source_bytes: vesper::lexer::HARD_SOURCE_BYTES,
            max_tokens: vesper::lexer::HARD_TOKENS,
            ..Default::default()
        };
        let parsed = vesper::parser::parse_with_limits(&text, limits).unwrap();
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
        let checked = vesper::check::check(&parsed.module);
        assert!(
            checked.diagnostics.is_empty(),
            "{name}: {:?}",
            checked.diagnostics
        );
        let art = wasm_backend::compile(
            &checked.world,
            entry,
            &WasmOptions {
                effects: EffectLowering::Portable,
                ..WasmOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{name}: the wasm backend refused the front end: {e}"));
        let export = format!("{}#main", wasm_backend::ENTRY_INTERFACE);
        match vwasm_run(&runner, &dir, &art.core, &export, tag) {
            Outcome::Ok { results, .. } => {
                assert_eq!(
                    results,
                    vec![*result as u64],
                    "{name}: vwasm vs stage1.lock"
                );
            }
            Outcome::Trap => panic!("{name}: the front end trapped under vwasm"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
