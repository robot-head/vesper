//! `Ffi[Cranelift]` under Wasm (#2210, part 2): `@vesper/wasm-backend`
//! lowers Cranelift calls to imports of `vesper:cranelift/vcl@0.1.0`, the
//! interface crates/vesper-cranelift-component exports.
//!
//! Each program below is Vesper code that lowers functions through
//! `@vesper/cranelift`. It runs natively (the JIT bound to the native shim)
//! and as a Wasm component compiled by the port, whose Cranelift imports are
//! served by the Cranelift component under Wasmtime. Both must answer the
//! same word and publish byte-identical objects.
//!
//! The Cranelift component is built with `cargo build --release --target
//! wasm32-wasip2 -p vesper-cranelift-component` (or named by
//! `VESPER_CRANELIFT_COMPONENT`); without it the test says so and passes.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;
#[path = "../../../tests/common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use vesper::ffi_lowering::{ForeignChecked, Handler, Imports};
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;
use wasmtime::component::{Component, Func, Linker, ResourceTable, Val};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

fn backend() -> &'static Entry {
    static BACKEND: OnceLock<Entry> = OnceLock::new();
    BACKEND.get_or_init(|| {
        Package::load("packages/vesper-wasm-backend").entry("wasm_backend_compile", true)
    })
}

/// The host's `.vsys` target: what an empty triple means on both sides.
fn host_target() -> (&'static str, &'static str) {
    vesper::packages::vsys_target(None).expect("host target")
}

fn package_source() -> String {
    std::fs::read_to_string(repo().join("packages/vesper-cranelift/src/lib.ves")).unwrap()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        repo()
            .join("crates/vesper-cranelift-sys/tests/fixtures")
            .join(name),
    )
    .unwrap()
}

/// The `@vesper/cranelift` package plus `extra`, checked with its `.vsys`.
fn check(extra: &str) -> ForeignChecked {
    let (triple, abi) = host_target();
    let vsys = std::fs::read_to_string(repo().join("packages/vesper-cranelift/cranelift.vsys"))
        .unwrap()
        .replace("@TARGET@", triple)
        .replace("@ABI@", abi);
    let mut imports = Imports::new();
    imports.add(&vsys, Handler::Direct).unwrap();
    let source = format!("{}\n{extra}", package_source());
    vesper::ffi_lowering::check_source(&source, &imports).unwrap_or_else(|e| panic!("{e}"))
}

fn options() -> WasmOptions {
    WasmOptions {
        optimize: true,
        effects: EffectLowering::Portable,
        ..WasmOptions::default()
    }
}

/// The port's component for `main`, or its refusal.
fn port(c: &ForeignChecked) -> Result<Vec<u8>, String> {
    let o = options();
    let w = &c.checked.world;
    let program = backend_program(w, &o).ok_or("optimize failed")?;
    let input = wasm_input(w, &program, "main", &o).ok_or("not encodable")?;
    match outcome(&backend().words(&input)) {
        Outcome::Module { core, wit, .. } => Ok(wrap(core, &wit, o.world)),
        o => Err(format!("{o:?}")),
    }
}

// ---------------------------------------------------------------------------
// Native side.

static PUBLISHED: Mutex<BTreeMap<u64, Vec<u8>>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u64> = Mutex::new(1 << 40);

fn keep(bytes: Vec<u8>) -> u64 {
    let mut n = NEXT.lock().unwrap();
    let id = *n;
    *n += 1;
    PUBLISHED.lock().unwrap().insert(id, bytes);
    id
}

unsafe extern "C" fn native_module_new(
    t: *const u8,
    tn: usize,
    f: *const u8,
    fnl: usize,
    n: *const u8,
    nn: usize,
) -> *mut vesper_cranelift_sys::Module {
    let host = host_target().0;
    let (t, tn) = if tn == 0 {
        (host.as_ptr(), host.len())
    } else {
        (t, tn)
    };
    // SAFETY: forwarded as received.
    unsafe { vesper_cranelift_sys::vcl_module_new(t, tn, f, fnl, n, nn) }
}

unsafe extern "C" fn native_object_publish(o: *mut vesper_cranelift_sys::Object) -> u64 {
    // SAFETY: a live object handle, consumed here.
    unsafe {
        let len = vesper_cranelift_sys::vcl_object_len(o) as usize;
        let mut bytes = vec![0u8; len];
        vesper_cranelift_sys::vcl_object_read(o, 0, bytes.as_mut_ptr(), len);
        vesper_cranelift_sys::vcl_object_free(o);
        keep(bytes)
    }
}

fn native(c: &ForeignChecked) -> u64 {
    let mut symbols = vesper_cranelift_sys::symbols();
    for (name, addr) in &mut symbols {
        if *name == "vcl_module_new" {
            *addr = native_module_new as *const u8;
        } else if *name == "vcl_object_publish" {
            *addr = native_object_publish as *const u8;
        }
    }
    match c.jit("main", &symbols).unwrap() {
        vesper::runtime::Value::Int(n) => n as u64,
        vesper::runtime::Value::UInt(n, _) => n,
        other => panic!("not a word: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Wasm side: the program's component, with its `vesper:cranelift/vcl`
// imports served by the Cranelift component in a store of its own.

const VCL: &str = "vesper:cranelift/vcl@0.1.0";

struct Wasi {
    ctx: WasiCtx,
    table: ResourceTable,
}
impl WasiView for Wasi {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

struct Cranelift {
    store: Store<Wasi>,
    funcs: HashMap<String, Func>,
}

impl Cranelift {
    fn call(&mut self, name: &str, args: &[Val], results: &mut [Val]) -> wasmtime::Result<()> {
        let f = self.funcs[name];
        f.call(&mut self.store, args, results)
    }
}

struct Guest {
    wasi: Wasi,
    cranelift: Arc<Mutex<Cranelift>>,
}
impl WasiView for Guest {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        self.wasi.ctx()
    }
}

fn wasi() -> Wasi {
    Wasi {
        ctx: WasiCtx::builder()
            .env("VESPER_CRANELIFT_HOST", host_target().0)
            .build(),
        table: ResourceTable::new(),
    }
}

fn component_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VESPER_CRANELIFT_COMPONENT") {
        return Some(PathBuf::from(p));
    }
    // Under Bazel the component is an input, never built by the test.
    let cargo = std::env::var_os("CARGO")?;
    // A relative CARGO_TARGET_DIR is relative to the build's directory.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(|d| repo().join(d))
        .unwrap_or_else(|| repo().join("target"));
    let out = std::process::Command::new(cargo)
        .current_dir(repo())
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "-p",
            "vesper-cranelift-component",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("wasm32-wasip2") && err.contains("target may not be installed") {
            return None;
        }
        panic!("building the Cranelift component failed:\n{err}");
    }
    Some(target_dir.join("wasm32-wasip2/release/vesper_cranelift_component.wasm"))
}

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut c = wasmtime::Config::new();
        c.wasm_component_model(true);
        Engine::new(&c).unwrap()
    })
}

fn cranelift_component(path: &PathBuf) -> &'static Component {
    static C: OnceLock<Component> = OnceLock::new();
    C.get_or_init(|| Component::from_file(engine(), path).expect("load the Cranelift component"))
}

fn vcl_names() -> Vec<String> {
    let wit = std::fs::read_to_string(repo().join("crates/vesper-cranelift-component/wit/vcl.wit"))
        .unwrap();
    wit.lines()
        .filter_map(|l| l.trim().strip_prefix('%'))
        .filter_map(|r| r.split_once(": func(").map(|(n, _)| n.to_string()))
        .collect()
}

fn new_cranelift(path: &PathBuf) -> Cranelift {
    let mut linker = Linker::<Wasi>::new(engine());
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
    let mut store = Store::new(engine(), wasi());
    let instance = linker
        .instantiate(&mut store, cranelift_component(path))
        .expect("instantiate the Cranelift component");
    let iface = instance
        .get_export_index(&mut store, None, VCL)
        .expect("the vcl interface");
    let mut funcs = HashMap::new();
    for name in vcl_names() {
        let idx = instance
            .get_export_index(&mut store, Some(&iface), &name)
            .unwrap_or_else(|| panic!("no export {name}"));
        funcs.insert(name.clone(), instance.get_func(&mut store, idx).unwrap());
    }
    Cranelift { store, funcs }
}

fn u64_of(v: &Val) -> u64 {
    match v {
        Val::U64(n) => *n,
        Val::U32(n) => u64::from(*n),
        other => panic!("not a word: {other:?}"),
    }
}

/// Run the program's component; its `main` word.
fn wasm(component: &[u8], cranelift_path: &PathBuf) -> Result<u64, String> {
    let component = Component::new(engine(), component).map_err(|e| format!("load: {e:?}"))?;
    let mut linker = Linker::<Guest>::new(engine());
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
    {
        let mut vcl = linker.instance(VCL).unwrap();
        for name in vcl_names() {
            let n = name.clone();
            if name == "vcl-object-publish" {
                // Copy the object out of the Cranelift component, as the
                // native side does.
                vcl.func_new(&name, move |cx, _ty, args, results| {
                    let mut cl = cx.data().cranelift.lock().unwrap();
                    let h = args[0].clone();
                    let mut len = [Val::U64(0)];
                    cl.call("vcl-object-len", std::slice::from_ref(&h), &mut len)?;
                    let len = u64_of(&len[0]) as usize;
                    let mut read = [Val::Bool(false)];
                    cl.call(
                        "vcl-object-read",
                        &[h.clone(), Val::U64(0), Val::List(vec![Val::U8(0); len])],
                        &mut read,
                    )?;
                    let Val::Tuple(parts) = &read[0] else {
                        panic!("vcl-object-read: {read:?}")
                    };
                    let Val::List(items) = &parts[1] else {
                        panic!("vcl-object-read: {parts:?}")
                    };
                    let bytes = items
                        .iter()
                        .map(|v| match v {
                            Val::U8(b) => *b,
                            other => panic!("{other:?}"),
                        })
                        .collect();
                    cl.call("vcl-object-free", &[h], &mut [])?;
                    results[0] = Val::U64(keep(bytes));
                    Ok(())
                })
                .unwrap();
                continue;
            }
            vcl.func_new(&name, move |cx, _ty, args, results| {
                let mut cl = cx.data().cranelift.lock().unwrap();
                cl.call(&n, args, results)
            })
            .unwrap();
        }
    }
    let mut store = Store::new(
        engine(),
        Guest {
            wasi: wasi(),
            cranelift: Arc::new(Mutex::new(new_cranelift(cranelift_path))),
        },
    );
    let instance = linker
        .instantiate(&mut store, &component)
        .map_err(|e| format!("instantiate: {e:?}"))?;
    let iface = instance
        .get_export_index(&mut store, None, vesper::wasm_backend::ENTRY_INTERFACE)
        .expect("entry interface");
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .expect("main export");
    let func = instance.get_func(&mut store, main).expect("main func");
    let mut results = vec![Val::Bool(false)];
    func.call(&mut store, &[], &mut results)
        .map_err(|e| format!("trap: {e:?}"))?;
    match results.first() {
        Some(Val::U64(x)) => Ok(*x),
        other => Err(format!("result {other:?}")),
    }
}

// ---------------------------------------------------------------------------

/// (fixture, entry, publishes an object).
const PROGRAMS: &[(&str, &str, bool)] = &[
    ("checked.ves", "checked_x64_linux", true),
    ("checked.ves", "checked_aarch64_linux", true),
    ("checked.ves", "checked_aarch64_macos", true),
    ("checked.ves", "checked_x64_windows", true),
    ("checked.ves", "checked_none", true),
    ("checked.ves", "checked_speed", true),
    ("pilot.ves", "pilot", true),
    ("sh5_extras.ves", "extras", true),
    ("pilot.ves", "invalid_function", false),
    ("pilot.ves", "poisoned_builder", false),
    ("pilot.ves", "invalid_argument", false),
    ("pilot.ves", "bad_triple", false),
    ("pilot.ves", "vectors_and_atomics", false),
    ("pilot.ves", "churn", false),
];

#[test]
fn cranelift_calls_lower_to_component_imports() {
    let Some(path) = component_path() else {
        eprintln!(
            "no wasm32-wasip2 target (rustup target add wasm32-wasip2); Cranelift import comparison unavailable"
        );
        return;
    };
    let mut diffs = Vec::new();
    let mut refused = Vec::new();
    for (file, entry, publishes) in PROGRAMS {
        let src = format!(
            "{}\npub fn main() -> u64 eff [Ffi[Cranelift]] {{\n    {entry}()\n}}\n",
            fixture(file)
        );
        let c = check(&src);
        let component = match port(&c) {
            Ok(bytes) => bytes,
            Err(e) => {
                refused.push(format!("{entry}: {e}"));
                continue;
            }
        };
        let n = native(&c);
        let w = match wasm(&component, &path) {
            Ok(w) => w,
            Err(e) => {
                diffs.push(format!("{entry}: {e}"));
                continue;
            }
        };
        if *publishes {
            let mut p = PUBLISHED.lock().unwrap();
            match (p.remove(&n), p.remove(&w)) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => diffs.push(format!(
                    "{entry}: objects differ ({} vs {} bytes)",
                    a.len(),
                    b.len()
                )),
                (a, b) => diffs.push(format!(
                    "{entry}: native published {:?}, Wasm {:?}",
                    a.map(|x| x.len()),
                    b.map(|x| x.len())
                )),
            }
        } else if n != w {
            diffs.push(format!("{entry}: native answered {n}, Wasm {w}"));
        }
    }
    assert!(
        diffs.is_empty() && refused.is_empty(),
        "{} differ, {} refused by the port:\n{}\n{}",
        diffs.len(),
        refused.len(),
        diffs.join("\n"),
        refused.join("\n")
    );
}

/// The pinned CLIs compose and run the same programs (`WAC_BIN` and
/// `WASMTIME_BIN`, as `aspect ci install-wac` and `install-wasmtime` lay
/// them out; without them the test says so and passes): `wac plug` links
/// the Cranelift component into each program's component, and the wasmtime
/// CLI runs the result with no other host (its `wasi:cli/run` succeeds).
#[test]
fn composed_programs_run_under_the_wasmtime_cli() {
    let (Some(wac), Some(wasmtime), Some(cranelift)) = (
        std::env::var_os("WAC_BIN"),
        std::env::var_os("WASMTIME_BIN"),
        component_path(),
    ) else {
        eprintln!(
            "WAC_BIN, WASMTIME_BIN or the Cranelift component missing; composition unavailable"
        );
        return;
    };
    let dir = std::env::temp_dir().join(format!("vesper-vcl-compose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for (file, entry, publishes) in PROGRAMS {
        if !publishes {
            continue;
        }
        let src = format!(
            "{}\npub fn main() -> u64 eff [Ffi[Cranelift]] {{\n    {entry}()\n}}\n",
            fixture(file)
        );
        let guest = dir.join(format!("{entry}.wasm"));
        let composed = dir.join(format!("{entry}.composed.wasm"));
        std::fs::write(&guest, port(&check(&src)).unwrap()).unwrap();
        let plug = std::process::Command::new(&wac)
            .arg("plug")
            .arg(&guest)
            .arg("--plug")
            .arg(&cranelift)
            .arg("-o")
            .arg(&composed)
            .output()
            .unwrap();
        if !plug.status.success() {
            failures.push(format!(
                "{entry}: wac plug failed: {}",
                String::from_utf8_lossy(&plug.stderr)
            ));
            continue;
        }
        let run = std::process::Command::new(&wasmtime)
            .arg("run")
            .args([
                "--env",
                &format!("VESPER_CRANELIFT_HOST={}", host_target().0),
            ])
            .arg(&composed)
            .output()
            .unwrap();
        if !run.status.success() {
            failures.push(format!(
                "{entry}: the wasmtime CLI failed ({}): {}",
                run.status,
                String::from_utf8_lossy(&run.stderr)
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
