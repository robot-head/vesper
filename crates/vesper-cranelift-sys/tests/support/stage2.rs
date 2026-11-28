//! The fixtures without the Rust compiler (#2672): the committed Stage 2
//! seed builds the package plus a fixture into a component that imports
//! `vesper:cranelift/vcl@0.1.0`, and Wasmtime runs it with those imports
//! served by the Cranelift component (crates/vesper-cranelift-component,
//! this shim built for `wasm32-wasip2`). Published objects are copied out
//! of the Cranelift component, as `vcl_object_publish` does natively.
//!
//! The component comes from `VESPER_CRANELIFT_COMPONENT` (Bazel's
//! `//crates/vesper-cranelift-component:component`) or is built with
//! `cargo build --release --target wasm32-wasip2 -p
//! vesper-cranelift-component`. Without it, or with the seed still a Git LFS
//! pointer, [`stage2`] says so and the test passes, unless
//! `VESPER_STAGE2_FIXTURES` is set (as CI does), when it fails.

use super::gate;
use super::gate::repo;
use super::{host_target, package_source};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wasmtime::component::{Component, Func, Linker, ResourceTable, Val};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// The interface the Cranelift component exports and fixtures import.
const VCL: &str = "vesper:cranelift/vcl@0.1.0";
/// The import that hands an object to the host; served here, not by the
/// component.
const PUBLISH: &str = "vcl-object-publish";
/// Set in CI: a missing component or seed is a failure, not a skip.
const REQUIRE: &str = "VESPER_STAGE2_FIXTURES";
/// The fixture package's name (the manifest is @vesper/cranelift's).
const FIXTURE_PACKAGE: &str = "@vesper-test/cranelift-fixture";
/// The Cranelift package, relative to the repository.
const CRANELIFT_PACKAGE: &str = "packages/vesper-cranelift";
/// The component file the wasm32-wasip2 build writes.
const COMPONENT_FILE: &str = "wasm32-wasip2/release/vesper_cranelift_component.wasm";
/// How a Git LFS pointer file starts.
const LFS_POINTER: &[u8] = b"version https://git-lfs";

/// A fixture's `entry`, as the `main` Stage 2 builds.
pub fn entry_main(entry: &str) -> String {
    format!("pub fn main() -> u64 eff [Ffi[Cranelift]] {{\n    {entry}()\n}}\n")
}

/// Stage 2 and the Cranelift component, ready to build and run fixtures.
pub struct Stage2 {
    seed: gate::Compiler,
    engine: Engine,
    cranelift: Component,
    triple: &'static str,
    published: Mutex<BTreeMap<u64, Vec<u8>>>,
    next: Mutex<u64>,
}

/// The shared runner, or `None` (with a note) when the component or the
/// seed is missing outside CI.
pub fn stage2() -> Option<&'static Stage2> {
    static RUNNER: OnceLock<Option<Stage2>> = OnceLock::new();
    RUNNER.get_or_init(load).as_ref()
}

fn unavailable(why: &str) -> Option<Stage2> {
    assert!(
        std::env::var_os(REQUIRE).is_none(),
        "{why} ({REQUIRE} is set)"
    );
    eprintln!("{why}; the Vesper fixtures did not run");
    None
}

fn load() -> Option<Stage2> {
    let Some((triple, _)) = host_target() else {
        return unavailable("no .vsys target for this host");
    };
    let seed = repo().join(gate::SEED);
    if std::fs::read(&seed).map_or(true, |b| b.starts_with(LFS_POINTER)) {
        return unavailable(&format!(
            "{} is missing or a Git LFS pointer (`git lfs pull`)",
            seed.display()
        ));
    }
    let Some(path) = component_path() else {
        return unavailable("no Cranelift component (rustup target add wasm32-wasip2)");
    };
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();
    let cranelift = Component::from_file(&engine, &path)
        .unwrap_or_else(|e| panic!("load {}: {e:?}", path.display()));
    Some(Stage2 {
        seed: gate::seed(),
        engine,
        cranelift,
        triple,
        published: Mutex::new(BTreeMap::new()),
        next: Mutex::new(1),
    })
}

/// The built component, building it when needed; `None` when this
/// toolchain has no `wasm32-wasip2` target.
pub fn component_path() -> Option<PathBuf> {
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
    Some(target_dir.join(COMPONENT_FILE))
}

/// Writes `@vesper/cranelift` plus `extra` as a package under `dir/pkg`.
fn write_package(dir: &Path, extra: &str) {
    let package = repo().join(CRANELIFT_PACKAGE);
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    let manifest = std::fs::read_to_string(package.join("Loom.toml"))
        .unwrap()
        .replace("\"@vesper/cranelift\"", &format!("\"{FIXTURE_PACKAGE}\""));
    std::fs::write(pkg.join("Loom.toml"), manifest).unwrap();
    std::fs::copy(package.join("cranelift.vsys"), pkg.join("cranelift.vsys")).unwrap();
    std::fs::write(
        pkg.join("src/lib.ves"),
        format!("{}\n{extra}", package_source()),
    )
    .unwrap();
}

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

/// The Cranelift component in a store of its own.
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

/// The fixture's store: WASI and the Cranelift component that serves its
/// imports.
struct Guest {
    wasi: Wasi,
    cranelift: Arc<Mutex<Cranelift>>,
    runner: &'static Stage2,
}
impl WasiView for Guest {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        self.wasi.ctx()
    }
}

/// The `vcl` export names, from the checked-in WIT.
fn vcl_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let wit =
            std::fs::read_to_string(repo().join("crates/vesper-cranelift-component/wit/vcl.wit"))
                .unwrap();
        wit.lines()
            .filter_map(|l| l.trim().strip_prefix('%'))
            .filter_map(|r| r.split_once(": func(").map(|(n, _)| n.to_string()))
            .collect()
    })
}

fn u64_of(v: &Val) -> u64 {
    match v {
        Val::U64(n) => *n,
        Val::U32(n) => u64::from(*n),
        other => panic!("not a word: {other:?}"),
    }
}

impl Stage2 {
    /// `@vesper/cranelift` plus `extra` (which defines `main`), built by
    /// the seed into a component.
    pub fn build(&'static self, extra: &str) -> Result<Vec<u8>, String> {
        let dir = gate::fresh("vcl-fixture");
        write_package(&dir, extra);
        let out = dir.join("out.wasm");
        let result = Arc::new(Mutex::new(None));
        let slot = result.clone();
        let build_dir = dir.clone();
        gate::with_large_stack(move || {
            let built = self
                .seed
                .build(&build_dir, &["/out/pkg", "/out/out.wasm"], &out);
            *slot.lock().unwrap() = Some(built);
        });
        let _ = std::fs::remove_dir_all(&dir);
        result.lock().unwrap().take().unwrap()
    }

    /// `entry` of the fixture source `fixture`, built and run.
    pub fn answer(&'static self, fixture: &str, entry: &str) -> u64 {
        let component = self
            .build(&format!("{fixture}\n{}", entry_main(entry)))
            .unwrap_or_else(|e| panic!("{entry}: Stage 2 refused the fixture:\n{e}"));
        self.run(&component)
            .unwrap_or_else(|e| panic!("{entry}: {e}"))
    }

    /// The object a fixture published under `id`.
    pub fn published(&self, id: u64) -> Option<Vec<u8>> {
        self.published.lock().unwrap().remove(&id)
    }

    fn keep(&self, bytes: Vec<u8>) -> u64 {
        let mut n = self.next.lock().unwrap();
        let id = *n;
        *n += 1;
        self.published.lock().unwrap().insert(id, bytes);
        id
    }

    fn wasi(&self) -> Wasi {
        Wasi {
            ctx: WasiCtx::builder()
                .env("VESPER_CRANELIFT_HOST", self.triple)
                .build(),
            table: ResourceTable::new(),
        }
    }

    fn new_cranelift(&self) -> Cranelift {
        let mut linker = Linker::<Wasi>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
        let mut store = Store::new(&self.engine, self.wasi());
        let instance = linker
            .instantiate(&mut store, &self.cranelift)
            .expect("instantiate the Cranelift component");
        let iface = instance
            .get_export_index(&mut store, None, VCL)
            .expect("the vcl interface");
        let mut funcs = HashMap::new();
        for name in vcl_names() {
            let idx = instance
                .get_export_index(&mut store, Some(&iface), name)
                .unwrap_or_else(|| panic!("no export {name}"));
            funcs.insert(name.clone(), instance.get_func(&mut store, idx).unwrap());
        }
        Cranelift { store, funcs }
    }

    /// Runs a fixture's component; its `main` word.
    pub fn run(&'static self, component: &[u8]) -> Result<u64, String> {
        let component =
            Component::new(&self.engine, component).map_err(|e| format!("load: {e:?}"))?;
        let mut linker = Linker::<Guest>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
        {
            let mut vcl = linker.instance(VCL).unwrap();
            for name in vcl_names() {
                if name == PUBLISH {
                    vcl.func_new(name, |cx, _ty, args, results| {
                        let runner = cx.data().runner;
                        let mut cl = cx.data().cranelift.lock().unwrap();
                        let bytes = copy_out(&mut cl, &args[0])?;
                        results[0] = Val::U64(runner.keep(bytes));
                        Ok(())
                    })
                    .unwrap();
                    continue;
                }
                let n = name.clone();
                vcl.func_new(name, move |cx, _ty, args, results| {
                    let mut cl = cx.data().cranelift.lock().unwrap();
                    cl.call(&n, args, results)
                })
                .unwrap();
            }
        }
        let mut store = Store::new(
            &self.engine,
            Guest {
                wasi: self.wasi(),
                cranelift: Arc::new(Mutex::new(self.new_cranelift())),
                runner: self,
            },
        );
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|e| format!("instantiate: {e:?}"))?;
        let iface = instance
            .get_export_index(&mut store, None, gate::ENTRY_INTERFACE)
            .ok_or("no entry interface")?;
        let main = instance
            .get_export_index(&mut store, Some(&iface), "main")
            .ok_or("no main export")?;
        let func = instance
            .get_func(&mut store, main)
            .ok_or("main is not a func")?;
        let mut results = vec![Val::Bool(false)];
        func.call(&mut store, &[], &mut results)
            .map_err(|e| format!("trap: {e:?}"))?;
        match results.first() {
            Some(Val::U64(x)) => Ok(*x),
            other => Err(format!("result {other:?}")),
        }
    }
}

/// Reads object `h` out of the Cranelift component and frees it.
fn copy_out(cl: &mut Cranelift, h: &Val) -> wasmtime::Result<Vec<u8>> {
    let mut len = [Val::U64(0)];
    cl.call("vcl-object-len", std::slice::from_ref(h), &mut len)?;
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
    cl.call("vcl-object-free", std::slice::from_ref(h), &mut [])?;
    Ok(bytes)
}
