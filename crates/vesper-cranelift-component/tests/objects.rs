//! The Cranelift component builds the objects the native shim builds
//! (#2210).
//!
//! The `vesper-cranelift-sys` fixtures (Vesper code that lowers through
//! `@vesper/cranelift`) run twice under the JIT: once with the native shim
//! bound, once with generated proxies (`proxy.rs`) that forward every call to
//! the shim compiled to `wasm32-wasip2` and run as a component under wasmtime.
//! Every entry must return the same word and publish the same object bytes.
//!
//! The component is built with `cargo build --release --target wasm32-wasip2
//! -p vesper-cranelift-component`, or taken from `VESPER_CRANELIFT_COMPONENT`.
//! Without the `wasm32-wasip2` target the test says so and passes.

#[path = "../../vesper-cranelift-sys/tests/support/mod.rs"]
mod support;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use support::*;
use wasmtime::component::{Component, Func, Linker, Val};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxView, WasiView};

include!(concat!(env!("OUT_DIR"), "/proxy.rs"));

struct State {
    ctx: WasiCtx,
    table: ResourceTable,
}
impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

struct Host {
    store: Store<State>,
    funcs: HashMap<String, Func>,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);

/// Call the component's export `name`.
fn call(name: &str, args: &[Val]) -> Vec<Val> {
    let mut guard = HOST.lock().unwrap_or_else(|e| e.into_inner());
    let host = guard.as_mut().expect("component instantiated");
    let f = *host
        .funcs
        .get(name)
        .unwrap_or_else(|| panic!("no export {name}"));
    let n = f.ty(&host.store).results().len();
    let mut rs = vec![Val::Bool(false); n];
    f.call(&mut host.store, args, &mut rs)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    rs
}

fn result_parts(rs: &[Val]) -> Vec<Val> {
    match rs {
        [Val::Tuple(parts)] => parts.clone(),
        _ => rs.to_vec(),
    }
}

fn val_u64(v: &Val) -> u64 {
    match v {
        Val::U32(n) => u64::from(*n),
        Val::U64(n) => *n,
        Val::S32(n) => *n as u64,
        Val::S64(n) => *n as u64,
        Val::Bool(b) => u64::from(*b),
        other => panic!("not a scalar: {other:?}"),
    }
}

trait Elem: Copy {
    fn to_val(self) -> Val;
    fn from_val(v: &Val) -> Self;
}
impl Elem for u8 {
    fn to_val(self) -> Val {
        Val::U8(self)
    }
    fn from_val(v: &Val) -> u8 {
        match v {
            Val::U8(n) => *n,
            other => panic!("not a u8: {other:?}"),
        }
    }
}
impl Elem for u32 {
    fn to_val(self) -> Val {
        Val::U32(self)
    }
    fn from_val(v: &Val) -> u32 {
        match v {
            Val::U32(n) => *n,
            other => panic!("not a u32: {other:?}"),
        }
    }
}
impl Elem for u64 {
    fn to_val(self) -> Val {
        Val::U64(self)
    }
    fn from_val(v: &Val) -> u64 {
        match v {
            Val::U64(n) => *n,
            other => panic!("not a u64: {other:?}"),
        }
    }
}

fn list_in<T: Elem>(ptr: *const T, len: usize) -> Val {
    if len == 0 || ptr.is_null() {
        return Val::List(Vec::new());
    }
    // SAFETY: the `.vsys` CSlice contract of the caller.
    let s = unsafe { std::slice::from_raw_parts(ptr, len) };
    Val::List(s.iter().map(|&x| x.to_val()).collect())
}

/// A `u32` slice crosses as `list<u64>`.
fn list_in_wide(ptr: *const u32, len: usize) -> Val {
    if len == 0 || ptr.is_null() {
        return Val::List(Vec::new());
    }
    // SAFETY: the `.vsys` CSlice contract of the caller.
    let s = unsafe { std::slice::from_raw_parts(ptr, len) };
    Val::List(s.iter().map(|&x| Val::U64(u64::from(x))).collect())
}

unsafe fn list_back_wide(v: &Val, ptr: *mut u32, len: usize) {
    let Val::List(items) = v else {
        panic!("not a list: {v:?}")
    };
    assert_eq!(items.len(), len, "a handed-back list changed length");
    for (i, x) in items.iter().enumerate() {
        // SAFETY: `i < len`, the caller's buffer.
        unsafe { ptr.add(i).write(u64::from_val(x) as u32) };
    }
}

/// Copy a handed-back list over the caller's buffer.
unsafe fn list_back<T: Elem>(v: &Val, ptr: *mut T, len: usize) {
    let Val::List(items) = v else {
        panic!("not a list: {v:?}")
    };
    assert_eq!(items.len(), len, "a handed-back list changed length");
    for (i, x) in items.iter().enumerate() {
        // SAFETY: `i < len`, the caller's buffer.
        unsafe { ptr.add(i).write(T::from_val(x)) };
    }
}

// ---------------------------------------------------------------------------
// Published objects, collected the same way on both sides.

static PUBLISHED: Mutex<BTreeMap<u64, Vec<u8>>> = Mutex::new(BTreeMap::new());
static NEXT: Mutex<u64> = Mutex::new(1);

fn keep(bytes: Vec<u8>) -> u64 {
    let mut n = NEXT.lock().unwrap();
    let id = *n;
    *n += 1;
    PUBLISHED.lock().unwrap().insert(id, bytes);
    id
}

/// The host's `.vsys` target triple: what an empty triple means on both
/// sides, so the native side does not detect CPU features the component
/// cannot.
fn host_triple() -> &'static str {
    host_target().expect("a .vsys host").0
}

unsafe extern "C" fn native_module_new(
    t: *const u8,
    tn: usize,
    f: *const u8,
    fnl: usize,
    n: *const u8,
    nn: usize,
) -> *mut vesper_cranelift_sys::Module {
    let host = host_triple();
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

unsafe extern "C" fn component_object_publish(o: *mut u8) -> u64 {
    let h = Val::U64(o as usize as u64);
    let len = val_u64(&call("vcl-object-len", std::slice::from_ref(&h))[0]) as usize;
    let rs = call(
        "vcl-object-read",
        &[h.clone(), Val::U64(0), Val::List(vec![Val::U8(0); len])],
    );
    let parts = result_parts(&rs);
    let Val::List(items) = &parts[1] else {
        panic!("vcl-object-read: {parts:?}")
    };
    let bytes: Vec<u8> = items.iter().map(u8::from_val).collect();
    assert_eq!(val_u64(&parts[0]) as usize, len);
    call("vcl-object-free", &[h]);
    keep(bytes)
}

fn native_symbols() -> Vec<(&'static str, *const u8)> {
    let mut s = vesper_cranelift_sys::symbols();
    for (name, addr) in &mut s {
        if *name == "vcl_module_new" {
            *addr = native_module_new as *const u8;
        } else if *name == "vcl_object_publish" {
            *addr = native_object_publish as *const u8;
        }
    }
    s
}

fn component_symbols() -> Vec<(&'static str, *const u8)> {
    let mut s = proxy_symbols();
    for (name, addr) in &mut s {
        if *name == "vcl_object_publish" {
            *addr = component_object_publish as *const u8;
        }
    }
    s
}

// ---------------------------------------------------------------------------
// The component.

fn repo_root() -> PathBuf {
    repo()
}

/// The built component, building it when needed; `None` when this
/// toolchain has no `wasm32-wasip2` target.
fn component_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VESPER_CRANELIFT_COMPONENT") {
        return Some(PathBuf::from(p));
    }
    // Under Bazel the component is an input, never built by the test.
    let cargo = std::env::var_os("CARGO")?;
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"));
    let out = std::process::Command::new(&cargo)
        .current_dir(repo_root())
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
        panic!("building the component failed:\n{err}");
    }
    Some(target_dir.join("wasm32-wasip2/release/vesper_cranelift_component.wasm"))
}

fn instantiate(path: &Path) {
    let engine = Engine::default();
    let component = Component::from_file(&engine, path).expect("load the component");
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("WASI");
    let ctx = WasiCtx::builder()
        .env("VESPER_CRANELIFT_HOST", host_triple())
        .build();
    let mut store = Store::new(
        &engine,
        State {
            ctx,
            table: ResourceTable::new(),
        },
    );
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("instantiate");
    let iface = instance
        .get_export_index(&mut store, None, "vesper:cranelift/vcl@0.1.0")
        .expect("the vcl interface");
    let wit = vesper_cranelift_component::WIT;
    let mut funcs = HashMap::new();
    for line in wit.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix('%') else {
            continue;
        };
        let Some((name, _)) = rest.split_once(": func(") else {
            continue;
        };
        let idx = instance
            .get_export_index(&mut store, Some(&iface), name)
            .unwrap_or_else(|| panic!("no export {name}"));
        let f = instance.get_func(&mut store, idx).expect("a function");
        funcs.insert(name.to_string(), f);
    }
    *HOST.lock().unwrap() = Some(Host { store, funcs });
}

// ---------------------------------------------------------------------------

struct Outcome {
    word: u64,
    object: Option<Vec<u8>>,
}

fn run(fixture_name: &str, entry: &str, symbols: &[(&str, *const u8)]) -> (Outcome, Duration) {
    let target = host_target().unwrap();
    let checked = checked_with(&fixture(fixture_name), target);
    let start = Instant::now();
    let v = checked
        .jit(entry, symbols)
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
    let took = start.elapsed();
    let word = word(&v);
    let object = PUBLISHED.lock().unwrap().remove(&word);
    (Outcome { word, object }, took)
}

/// Entries that publish an object (their word is the object's id) and
/// entries whose word is the answer.
const PUBLISHING: &[(&str, &str)] = &[
    ("checked.ves", "checked_none"),
    ("checked.ves", "checked_speed"),
    ("checked.ves", "checked_x64_linux"),
    ("checked.ves", "checked_aarch64_linux"),
    ("checked.ves", "checked_aarch64_macos"),
    ("checked.ves", "checked_x64_windows"),
    ("pilot.ves", "pilot"),
    ("sh5_extras.ves", "extras"),
];
const ANSWERING: &[(&str, &str)] = &[
    ("pilot.ves", "invalid_function"),
    ("pilot.ves", "poisoned_builder"),
    ("pilot.ves", "invalid_argument"),
    ("pilot.ves", "bad_triple"),
    ("pilot.ves", "vectors_and_atomics"),
    ("pilot.ves", "churn"),
    ("sh5_extras.ves", "typed_words"),
];

#[test]
fn the_component_builds_the_objects_the_native_shim_builds() {
    if host_target().is_none() {
        eprintln!("no .vsys target for this host; comparison unavailable");
        return;
    }
    let Some(path) = component_path() else {
        eprintln!(
            "no wasm32-wasip2 target (rustup target add wasm32-wasip2); component comparison unavailable"
        );
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    instantiate(&path);
    let native = native_symbols();
    let component = component_symbols();
    let mut diffs = Vec::new();
    let (mut t_native, mut t_component) = (Duration::ZERO, Duration::ZERO);
    for (file, entry) in PUBLISHING {
        let (n, tn) = run(file, entry, &native);
        let (c, tc) = run(file, entry, &component);
        t_native += tn;
        t_component += tc;
        let (Some(no), Some(co)) = (&n.object, &c.object) else {
            diffs.push(format!(
                "{entry}: native published {:?}, component {:?}",
                n.object.as_ref().map(Vec::len),
                c.object.as_ref().map(Vec::len)
            ));
            continue;
        };
        assert!(no.len() > 100, "{entry}: {} bytes", no.len());
        if no != co {
            diffs.push(format!(
                "{entry}: objects differ ({} vs {} bytes)",
                no.len(),
                co.len()
            ));
        }
    }
    for (file, entry) in ANSWERING {
        let (n, _) = run(file, entry, &native);
        let (c, _) = run(file, entry, &component);
        if n.word != c.word {
            diffs.push(format!(
                "{entry}: native answered {}, component {}",
                n.word, c.word
            ));
        }
    }
    eprintln!(
        "{} objects + {} answers: native {:?}, component {:?} ({:.2}x)",
        PUBLISHING.len(),
        ANSWERING.len(),
        t_native,
        t_component,
        t_component.as_secs_f64() / t_native.as_secs_f64().max(1e-9)
    );
    assert!(
        diffs.is_empty(),
        "{} differ:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

/// ABBA timing of the publishing entries (each run: JIT the fixture entry
/// and build its object), native shim against component, with
/// `VESPER_CRANELIFT_ABBA=<rounds>`. Run it with `--release`.
#[test]
fn abba_timing() {
    let Some(rounds) = std::env::var("VESPER_CRANELIFT_ABBA")
        .ok()
        .and_then(|r| r.parse::<usize>().ok())
    else {
        return;
    };
    let Some(path) = component_path() else {
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    instantiate(&path);
    let native = native_symbols();
    let component = component_symbols();
    let target = host_target().unwrap();
    let mut prepared = Vec::new();
    for (file, entry) in PUBLISHING {
        prepared.push((checked_with(&fixture(file), target), *entry));
    }
    let time = |symbols: &[(&str, *const u8)]| {
        let start = Instant::now();
        for (checked, entry) in &prepared {
            let v = checked.jit(entry, symbols).unwrap();
            PUBLISHED.lock().unwrap().remove(&word(&v));
        }
        start.elapsed().as_secs_f64()
    };
    time(&native);
    time(&component);
    let mut ratios = Vec::new();
    for _ in 0..rounds {
        let a1 = time(&native);
        let b1 = time(&component);
        let b2 = time(&component);
        let a2 = time(&native);
        ratios.push((b1 + b2) / (a1 + a2));
    }
    let n = ratios.len() as f64;
    let mean = ratios.iter().sum::<f64>() / n;
    let sd = (ratios.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
    let upper = mean + 1.96 * sd / n.sqrt();
    eprintln!(
        "ABBA component/native over {rounds} rounds: mean {mean:.3}x, 95% CI upper {upper:.3}x"
    );
}

#[test]
fn the_checked_in_wit_is_the_generated_one() {
    let path = repo().join("crates/vesper-cranelift-component/wit/vcl.wit");
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vesper_cranelift_component::WIT).unwrap();
        return;
    }
    assert!(
        on_disk == vesper_cranelift_component::WIT,
        "{} is stale: rerun with VESPER_BLESS=1",
        path.display()
    );
}
