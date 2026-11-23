//! Rust exit gap wave 1 lane 10 (#1508): the standard I/O libraries
//! (`.vsys` `StdEnv` of @vesper-std/env and `StdFs` of @vesper-std/fs) in
//! `@vesper/wasm-backend`, lowered over WASI 0.2
//! (packages/vesper-wasm-backend/src/host.ves). The wasm seed reads its
//! sources and writes its outputs through them.
//!
//! Each file-system program runs twice: natively (the JIT bound to the
//! C-ABI shim crates/vesper-std-host) in one fresh directory, and as a
//! wasm component under Wasmtime with another fresh directory preopened at
//! its own absolute path. Both must return the same value, which folds in
//! every result and error word (the shim reports Linux errnos, so the
//! comparison runs on Linux). Arguments and environment variables are
//! checked against the values the WASI context gives.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/seed_runner.rs"]
mod seed_runner;
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ffi_lowering::{ForeignChecked, Handler, Imports};
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;

fn backend() -> &'static Entry {
    static BACKEND: OnceLock<Entry> = OnceLock::new();
    BACKEND.get_or_init(|| {
        Package::load("packages/vesper-wasm-backend").entry("wasm_backend_compile", true)
    })
}

fn imports() -> Imports {
    let (triple, abi) = vesper::packages::vsys_target(None).expect("host target");
    let mut imports = Imports::new();
    for lib in ["env/env-wasip2", "fs/fs"] {
        let path = format!("packages/vesper-std/{lib}.vsys");
        let text = std::fs::read_to_string(&path).expect("read .vsys");
        let text = text.replace("@TARGET@", triple).replace("@ABI@", abi);
        imports.add(&text, Handler::Direct).expect("import");
    }
    imports
}

fn check(src: &str) -> ForeignChecked {
    match vesper::ffi_lowering::check_source(src, &imports()) {
        Ok(c) => c,
        Err(e) => panic!("{e}: {:?}", e.diagnostics),
    }
}

fn options(optimize: bool) -> WasmOptions {
    WasmOptions {
        optimize,
        effects: EffectLowering::Portable,
        ..WasmOptions::default()
    }
}

/// The port's component for `src`, or its refusal.
fn port(c: &ForeignChecked, o: &WasmOptions) -> Result<Vec<u8>, String> {
    let w = &c.checked.world;
    let program = backend_program(w, o).ok_or("optimize failed")?;
    let input = wasm_input(w, &program, "main", o).ok_or("not encodable")?;
    match outcome(&backend().words(&input)) {
        Outcome::Module { core, wit, .. } => Ok(wrap(core, &wit, o.world)),
        o => Err(format!("{o:?}")),
    }
}

struct WasiHost {
    ctx: wasmtime_wasi::WasiCtx,
    table: wasmtime::component::ResourceTable,
}
impl wasmtime_wasi::WasiView for WasiHost {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

/// What the WASI context of a run gives the program.
#[derive(Default)]
struct Ctx {
    args: Vec<&'static str>,
    env: Vec<(&'static str, &'static str)>,
    /// (host directory, guest name)
    dirs: Vec<(PathBuf, String)>,
    /// Wasmtime fuel, 200 million by default.
    fuel: Option<u64>,
}

/// Run the nullary `main` returning `u64` under Wasmtime with WASI.
fn run(component: &[u8], ctx: &Ctx) -> Result<u64, String> {
    use wasmtime::component::{Component, Linker, Val};
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    let engine = wasmtime::Engine::new(&c).expect("engine");
    let component = Component::new(&engine, component).map_err(|e| format!("load: {e:?}"))?;
    let mut linker = Linker::<WasiHost>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| format!("{e:?}"))?;
    let mut b = wasmtime_wasi::WasiCtxBuilder::new();
    b.args(&ctx.args);
    for (k, v) in &ctx.env {
        b.env(k, v);
    }
    for (host, guest) in &ctx.dirs {
        b.preopened_dir(host, guest, wasmtime_wasi::FsPerms::ReadWrite)
            .map_err(|e| format!("preopen: {e:?}"))?;
    }
    let mut store = wasmtime::Store::new(
        &engine,
        WasiHost {
            ctx: b.build(),
            table: Default::default(),
        },
    );
    store.set_fuel(ctx.fuel.unwrap_or(200_000_000)).unwrap();
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

/// A fresh empty directory for one run.
fn fresh(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("vesper-seed-io-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// The program over base directory `base` (a `BASE` placeholder in `src`).
fn with_base(src: &str, base: &Path) -> String {
    src.replace("BASE", base.to_str().expect("utf-8 temp path"))
}

/// Helpers every file-system program shares.
const PRELUDE: &str = r#"
fn path(in name: Str) -> ChunkSeq[u8] {
    let mut p = ChunkSeq::new[u8]();
    p.extend(in "BASE/".as_bytes());
    p.extend(in name.as_bytes());
    p
}
fn mix(acc: u64, x: u64) -> u64 { wrapping_add(wrapping_mul(acc, 1000003), x) }
fn zeros(n: u64) -> ChunkSeq[u8] {
    let mut b = ChunkSeq::new[u8]();
    for _i in 0..n { b.push(0); }
    b
}
fn sum(in b: Slice[u8], n: u64) -> u64 {
    let mut t: u64 = 0;
    for i in 0..n {
        match b.try_get(i) {
            Option::Some(x) => { t = wrapping_add(wrapping_mul(t, 31), widen_u64(x)); },
            Option::None => {},
        }
    }
    t
}
// Write `data` to `name` in `mode`: the status, error and a missing-file marker.
fn put(in name: Str, mode: u32, in data: Slice[u8]) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    let p = path(name);
    match StdFs::vstd_fs_open(in p.as_slice(), mode, inout err) {
        Option::Some(f) => {
            let mut f = f;
            let s = StdFs::vstd_fs_write(inout f, in data, inout err);
            StdFs::vstd_fs_close(f);
            mix(widen_u64(s), err)
        },
        Option::None => mix(77, err),
    }
}
// Read `name` through a buffer of `cap` bytes: length, contents and errors.
fn get(in name: Str, cap: u64) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    let p = path(name);
    match StdFs::vstd_fs_open(in p.as_slice(), 0, inout err) {
        Option::Some(f) => {
            let mut f = f;
            let len = StdFs::vstd_fs_file_len(in f);
            let mut buf = zeros(cap);
            let n = StdFs::vstd_fs_read(inout f, inout buf.as_slice_mut(), inout err);
            let again = StdFs::vstd_fs_read(inout f, inout buf.as_slice_mut(), inout err);
            StdFs::vstd_fs_close(f);
            mix(mix(mix(mix(len, n), sum(buf.as_slice(), n)), again), err)
        },
        Option::None => mix(99, err),
    }
}
"#;

const FS_PROGRAMS: &[&str] = &[
    // write, truncate, append, create_new on an existing file, read back
    // through a big and a small buffer, a missing file
    r#"fn main() -> u64 eff [Ffi[StdFs]] {
    let mut acc: u64 = 0;
    acc = mix(acc, put("a.txt", 1, "hello, wasm seed".as_bytes()));
    acc = mix(acc, get("a.txt", 64));
    acc = mix(acc, get("a.txt", 5));
    acc = mix(acc, put("a.txt", 3, " and more".as_bytes()));
    acc = mix(acc, get("a.txt", 64));
    acc = mix(acc, put("a.txt", 1, "short".as_bytes()));
    acc = mix(acc, get("a.txt", 64));
    acc = mix(acc, put("a.txt", 2, "clash".as_bytes()));
    acc = mix(acc, put("b.txt", 2, "new".as_bytes()));
    acc = mix(acc, get("b.txt", 64));
    acc = mix(acc, get("missing.txt", 64));
    acc = mix(acc, put("no/such/dir.txt", 1, "x".as_bytes()));
    acc = mix(acc, put("empty.txt", 1, "".as_bytes()));
    acc = mix(acc, get("empty.txt", 8));
    acc
}"#,
    // directories: create (one level, all levels, existing), metadata,
    // rename, remove, a listing folded in an order-free way
    r#"fn dir(in name: Str, all: bool) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    let p = path(name);
    let s = StdFs::vstd_fs_create_dir(in p.as_slice(), all, inout err);
    mix(widen_u64(s), err)
}
fn meta(in name: Str, follow: bool) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    let p = path(name);
    let mut m = FsMeta { size: 0, mtime_s: 0, mtime_ns: 0, kind: 0 };
    let s = StdFs::vstd_fs_metadata(in p.as_slice(), follow, inout m, inout err);
    let recent: u64 = if m.mtime_s > 1700000000 { 1 } else { 0 };
    mix(mix(mix(mix(widen_u64(s), err), m.size), widen_u64(m.kind)), recent)
}
fn list(in name: Str, cap: u64) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    let p = path(name);
    match StdFs::vstd_fs_read_dir(in p.as_slice(), inout err) {
        Option::Some(d) => {
            let mut d = d;
            let mut total: u64 = 0;
            let mut count: u64 = 0;
            let mut short: u64 = 0;
            let mut buf = zeros(cap);
            loop {
                let mut kind: u32 = 0;
                let n = StdFs::vstd_fs_dir_next(inout d, inout buf.as_slice_mut(), inout kind);
                if n < 0 { break; }
                let len = wrapping_u64(n);
                if len > buf.len() {
                    short = wrapping_add(short, 1);
                    buf = zeros(len);
                    continue;
                }
                count = wrapping_add(count, 1);
                total = wrapping_add(total, wrapping_mul(sum(buf.as_slice(), len), widen_u64(kind)));
            }
            StdFs::vstd_fs_dir_close(d);
            mix(mix(mix(mix(count, total), short), err), 5)
        },
        Option::None => mix(66, err),
    }
}
fn main() -> u64 eff [Ffi[StdFs]] {
    let mut acc: u64 = 0;
    acc = mix(acc, dir("d1", false));
    acc = mix(acc, dir("d1", false));
    acc = mix(acc, dir("d2/x/y", false));
    acc = mix(acc, dir("d2/x/y", true));
    acc = mix(acc, dir("d2/x/y", true));
    acc = mix(acc, put("d1/f.txt", 1, "0123456789".as_bytes()));
    acc = mix(acc, dir("d1/f.txt", true));
    acc = mix(acc, put("d1/a-much-longer-file-name.txt", 1, "z".as_bytes()));
    acc = mix(acc, meta("d1/f.txt", true));
    acc = mix(acc, meta("d1", false));
    acc = mix(acc, meta("d1/none", true));
    acc = mix(acc, list("d1", 64));
    acc = mix(acc, list("d1", 4));
    acc = mix(acc, list("d2", 64));
    acc = mix(acc, list("nowhere", 64));
    let mut err: u64 = 0;
    let from = path("d1/f.txt");
    let to = path("d2/g.txt");
    acc = mix(acc, widen_u64(StdFs::vstd_fs_rename(in from.as_slice(), in to.as_slice(), inout err)));
    acc = mix(acc, err);
    acc = mix(acc, get("d2/g.txt", 64));
    acc = mix(acc, get("d1/f.txt", 64));
    let d1 = path("d1");
    acc = mix(acc, widen_u64(StdFs::vstd_fs_remove_dir(in d1.as_slice(), false, inout err)));
    acc = mix(acc, err);
    let g = path("d2/g.txt");
    acc = mix(acc, widen_u64(StdFs::vstd_fs_remove_file(in g.as_slice(), inout err)));
    acc = mix(acc, err);
    acc = mix(acc, widen_u64(StdFs::vstd_fs_remove_file(in g.as_slice(), inout err)));
    acc = mix(acc, err);
    let y = path("d2/x/y");
    acc = mix(acc, widen_u64(StdFs::vstd_fs_remove_dir(in y.as_slice(), false, inout err)));
    acc = mix(acc, err);
    acc = mix(acc, widen_u64(StdFs::vstd_fs_sync_dir(in d1.as_slice(), inout err)));
    acc = mix(acc, err);
    let d2 = path("d2");
    acc = mix(acc, widen_u64(StdFs::vstd_fs_sync_dir(in d2.as_slice(), inout err)));
    acc = mix(acc, err);
    acc = mix(acc, StdFs::vstd_fs_parent_len(in "x/yz/w.txt".as_bytes()));
    acc = mix(acc, StdFs::vstd_fs_parent_len(in "plain".as_bytes()));
    acc
}"#,
];

#[test]
fn file_system_runs_like_the_native_host() {
    if !cfg!(target_os = "linux") {
        // Error words carry the OS errno, which is Linux's in the shims.
        return;
    }
    for (i, body) in FS_PROGRAMS.iter().enumerate() {
        let src = format!("{PRELUDE}\n{body}");
        let native_dir = fresh(&format!("native{i}"));
        let native = check(&with_base(&src, &native_dir));
        let expected = native
            .jit_with_options("main", &vesper::ffi_lowering::std_host_symbols(), false)
            .unwrap_or_else(|e| panic!("program {i} native: {e}"))
            .to_string();
        for optimize in [false, true] {
            let dir = fresh(&format!("wasm{i}"));
            let c = check(&with_base(&src, &dir));
            let component = port(&c, &options(optimize))
                .unwrap_or_else(|e| panic!("program {i} (optimize {optimize}): {e}"));
            let ctx = Ctx {
                dirs: vec![(dir.clone(), dir.to_str().unwrap().to_owned())],
                ..Ctx::default()
            };
            let got = run(&component, &ctx)
                .unwrap_or_else(|e| panic!("program {i} (optimize {optimize}): {e}"));
            assert_eq!(
                got.to_string(),
                expected,
                "program {i} (optimize {optimize}) differs from the native host"
            );
            // The files the run left behind.
            let left: &[(&str, &[u8])] = match i {
                0 => &[("a.txt", b"short"), ("b.txt", b"new"), ("empty.txt", b"")],
                _ => &[("d1/a-much-longer-file-name.txt", b"z")],
            };
            for (name, bytes) in left {
                assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name}");
            }
            if i == 1 {
                assert!(dir.join("d2/x").is_dir() && !dir.join("d2/x/y").exists());
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
        let _ = std::fs::remove_dir_all(&native_dir);
    }
}

/// Relative paths resolve against the preopen named `.`; paths outside
/// every preopen fail with NotFound (ENOENT).
#[test]
fn relative_paths_use_the_dot_preopen() {
    let src = format!(
        "{}\n{}",
        PRELUDE.replace("BASE/", ""),
        r#"fn main() -> u64 eff [Ffi[StdFs]] {
    let mut acc: u64 = 0;
    acc = mix(acc, put("./rel.txt", 1, "relative".as_bytes()));
    acc = mix(acc, get("rel.txt", 64));
    let mut err: u64 = 0;
    match StdFs::vstd_fs_open(in "/outside/x".as_bytes(), 0, inout err) {
        Option::Some(f) => { StdFs::vstd_fs_close(f); },
        Option::None => {},
    }
    acc = mix(acc, err);
    match StdFs::vstd_fs_open(in "".as_bytes(), 0, inout err) {
        Option::Some(f) => { StdFs::vstd_fs_close(f); },
        Option::None => {},
    }
    mix(acc, err)
}"#
    );
    let c = check(&src);
    let dir = fresh("rel");
    let component = port(&c, &options(false)).expect("port");
    let ctx = Ctx {
        dirs: vec![(dir.clone(), ".".into())],
        ..Ctx::default()
    };
    let got = run(&component, &ctx).expect("run");
    assert_eq!(std::fs::read(dir.join("rel.txt")).unwrap(), b"relative");
    let mix = |a: u64, x: u64| a.wrapping_mul(1000003).wrapping_add(x);
    let sum = |b: &[u8]| {
        b.iter()
            .fold(0u64, |t, &x| t.wrapping_mul(31).wrapping_add(x as u64))
    };
    let put = mix(0, 0);
    let get = mix(mix(mix(mix(8, 8), sum(b"relative")), 0), 0);
    let want = mix(mix(mix(mix(0, put), get), 1 | (2 << 32)), 1 | (2 << 32));
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn too_many_preopens_fail_every_path() {
    // A partial preopen table could resolve a path under the wrong
    // directory, so with more preopens than it holds every path fails
    // with Unsupported (ENOTSUP).
    let src = format!(
        "{}\n{}",
        PRELUDE.replace("BASE/", ""),
        r#"fn main() -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    match StdFs::vstd_fs_open(in "/p16/x.txt".as_bytes(), 1, inout err) {
        Option::Some(f) => { StdFs::vstd_fs_close(f); },
        Option::None => {},
    }
    err
}"#
    );
    let c = check(&src);
    let root = fresh("many");
    let dirs: Vec<_> = (0..17)
        .map(|i| {
            let d = root.join(format!("p{i}"));
            std::fs::create_dir_all(&d).expect("preopen dir");
            (d, format!("/p{i}"))
        })
        .collect();
    let component = port(&c, &options(false)).expect("port");
    let ctx = Ctx {
        dirs,
        ..Ctx::default()
    };
    let got = run(&component, &ctx).expect("run");
    assert_eq!(got, 11 | (95 << 32));
    assert!(!root.join("p16/x.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn env_package_selects_wasi_arguments() {
    let pkg = fresh("wasi-env");
    std::fs::create_dir(pkg.join("src")).unwrap();
    let env = std::fs::canonicalize("packages/vesper-std/env").unwrap();
    std::fs::write(
        pkg.join("Loom.toml"),
        format!(
            r#"[package]
name = "@wasm-seed/env-main"
version = "0.1.0"
edition = "2026"
capability_ceiling = ["Alloc", "Fail", "Ffi[StdEnv]"]

[dependencies]
"@vesper-std/env" = {{ version = "^0.1", path = {env:?} }}
"#
        ),
    )
    .unwrap();
    std::fs::write(
        pkg.join("src/main.ves"),
        "use @vesper-std/env::{arg_count};\nfn main() -> u64 eff [Ffi[StdEnv]] { arg_count() }",
    )
    .unwrap();
    let assembled =
        vesper::packages::load_for_target(&pkg, Some(vesper::wasm_backend::TARGET), false).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let imports = assembled.imports(None).unwrap();
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports)
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let component = port(&checked, &options(true)).unwrap();
    let ctx = Ctx {
        args: vec!["seed", "input"],
        ..Ctx::default()
    };
    assert_eq!(run(&component, &ctx).unwrap(), 2);
    std::fs::remove_dir_all(pkg).unwrap();
}

#[test]
fn arguments_and_environment_come_from_wasi() {
    let src = r#"
fn mix(acc: u64, x: u64) -> u64 { wrapping_add(wrapping_mul(acc, 1000003), x) }
fn sum(in b: Slice[u8], n: u64) -> u64 {
    let mut t: u64 = 0;
    for i in 0..n {
        match b.try_get(i) {
            Option::Some(x) => { t = wrapping_add(wrapping_mul(t, 31), widen_u64(x)); },
            Option::None => {},
        }
    }
    t
}
fn zeros(n: u64) -> ChunkSeq[u8] {
    let mut b = ChunkSeq::new[u8]();
    for _i in 0..n { b.push(0); }
    b
}
fn fold(n: i64, in b: ChunkSeq[u8]) -> u64 {
    if n < 0 { 12345 } else {
        let len = wrapping_u64(n);
        if len > b.len() { mix(len, 54321) } else { mix(len, sum(b.as_slice(), len)) }
    }
}
fn main() -> u64 eff [Ffi[StdEnv]] {
    let mut acc = StdEnv::vstd_env_arg_count();
    let mut buf = zeros(16);
    for i in 0..4 {
        let n = StdEnv::vstd_env_arg(i, inout buf.as_slice_mut());
        acc = mix(acc, fold(n, buf));
    }
    let mut small = zeros(2);
    acc = mix(acc, fold(StdEnv::vstd_env_arg(1, inout small.as_slice_mut()), small));
    acc = mix(acc, StdEnv::vstd_env_var_count());
    acc = mix(acc, fold(StdEnv::vstd_env_var(in "HOME".as_bytes(), inout buf.as_slice_mut()), buf));
    acc = mix(acc, fold(StdEnv::vstd_env_var(in "MISSING".as_bytes(), inout buf.as_slice_mut()), buf));
    acc = mix(acc, fold(StdEnv::vstd_env_var(in "A=B".as_bytes(), inout buf.as_slice_mut()), buf));
    acc = mix(acc, fold(StdEnv::vstd_env_var(in "".as_bytes(), inout buf.as_slice_mut()), buf));
    acc = mix(acc, fold(StdEnv::vstd_env_var_at(1, inout buf.as_slice_mut()), buf));
    acc = mix(acc, fold(StdEnv::vstd_env_var_at(7, inout buf.as_slice_mut()), buf));
    let mut err: u64 = 0;
    acc = mix(acc, fold(StdEnv::vstd_env_current_dir(inout buf.as_slice_mut(), inout err), buf));
    acc = mix(acc, err);
    mix(acc, widen_u64(StdEnv::vstd_env_os()))
}"#;
    let c = check(src);
    let ctx = Ctx {
        args: vec!["seed", "build", "src/lib.ves"],
        env: vec![("PATH", "/bin"), ("HOME", "/home/v"), ("LANG", "C")],
        ..Ctx::default()
    };
    // The same fold over the context's values.
    let mix = |a: u64, x: u64| a.wrapping_mul(1000003).wrapping_add(x);
    let sum = |b: &[u8]| {
        b.iter()
            .fold(0u64, |t, &x| t.wrapping_mul(31).wrapping_add(x as u64))
    };
    let fold = |v: Option<&[u8]>, cap: usize| match v {
        None => 12345,
        Some(b) if b.len() > cap => mix(b.len() as u64, 54321),
        Some(b) => mix(b.len() as u64, sum(b)),
    };
    let mut want = ctx.args.len() as u64;
    for i in 0..4 {
        want = mix(want, fold(ctx.args.get(i).map(|a| a.as_bytes()), 16));
    }
    want = mix(want, fold(Some(b"build"), 2));
    want = mix(want, ctx.env.len() as u64);
    want = mix(want, fold(Some(b"/home/v"), 16));
    for _ in 0..3 {
        want = mix(want, 12345);
    }
    want = mix(want, fold(Some(b"HOME=/home/v"), 16));
    want = mix(want, 12345);
    // No initial directory: Unsupported, as the shim's error word.
    want = mix(want, 12345);
    want = mix(want, 11);
    want = mix(want, 0);
    for optimize in [false, true] {
        let component = port(&c, &options(optimize)).expect("port");
        assert_eq!(
            run(&component, &ctx).expect("run"),
            want,
            "optimize {optimize}"
        );
    }
}

/// `main` on the evaluator: its value's display, or "trap".
fn evaluate(world: &vesper::check::World) -> String {
    let mut r = vesper::runtime::Runtime::new(world);
    r.fuel = 10_000_000;
    match r.run("main", vec![]) {
        Ok(v) => v.to_string(),
        Err(_) => "trap".into(),
    }
}

/// Heap programs: `Box`, recursive enums through it, owned fields moved
/// and dropped, `@fip` reuse, and a tail loop that evacuates the heap
/// while boxes are live.
const HEAP_PROGRAMS: &[&str] = &[
    r#"enum List { Nil, Cons(u64, Box[List]) }
fn build(n: u64) -> List { if n == 0 { List::Nil } else { List::Cons(n, Box::new(build(wrapping_sub(n, 1)))) } }
fn total(in l: List) -> u64 { match l { List::Nil => 0, List::Cons(x, rest) => match rest { Box(r) => wrapping_add(x, total(r)) } } }
fn main() -> u64 { let l = build(10); total(l) }"#,
    r#"enum Tree { Leaf(u64), Node(Box[Tree], Box[Tree]) }
fn mk(d: u64) -> Tree { if d == 0 { Tree::Leaf(1) } else { Tree::Node(Box::new(mk(wrapping_sub(d, 1))), Box::new(mk(wrapping_sub(d, 1)))) } }
fn count(sink t: Tree) -> u64 { match t { Tree::Leaf(x) => x, Tree::Node(a, b) => { let Box(l) = a; let Box(r) = b; wrapping_add(count(l), count(r)) } } }
fn main() -> u64 { count(mk(6)) }"#,
    // A tail-recursive loop whose accumulator is a boxed list: each
    // iteration's garbage is reclaimed while the list stays live.
    r#"enum List { Nil, Cons(u64, Box[List]) }
fn push_n(n: u64, sink acc: List) -> List { if n == 0 { acc } else { push_n(wrapping_sub(n, 1), List::Cons(n, Box::new(acc))) } }
fn len(in l: List) -> u64 { match l { List::Nil => 0, List::Cons(_x, rest) => match rest { Box(r) => wrapping_add(1, len(r)) } } }
fn sum(in l: List) -> u64 { match l { List::Nil => 0, List::Cons(x, rest) => match rest { Box(r) => wrapping_add(x, sum(r)) } } }
fn main() -> u64 { let l = push_n(300, List::Nil); wrapping_add(wrapping_mul(len(l), 1000000), sum(l)) }"#,
    // A struct with a boxed owned field, moved out and replaced.
    r#"struct Node { value: u64, next: Option[Box[Node]] }
fn chain(n: u64) -> Node { if n == 0 { Node { value: 0, next: Option::None } } else { Node { value: n, next: Option::Some(Box::new(chain(wrapping_sub(n, 1)))) } } }
fn walk(sink n: Node) -> u64 { let Node { value, next } = n; match next { Option::None => value, Option::Some(b) => { let Box(m) = b; wrapping_add(wrapping_mul(value, 3), walk(m)) } } }
fn main() -> u64 { walk(chain(20)) }"#,
    // `@fip` in-place reuse of an unpacked box.
    r#"enum List { Nil, Cons(u64, Box[List]) }
@fip fn inc_all(sink l: List) -> List { match l { List::Nil => List::Nil, List::Cons(x, rest) => { let Box(r) = rest; List::Cons(wrapping_add(x, 1), Box::new(inc_all(r))) } } }
fn build(n: u64) -> List { if n == 0 { List::Nil } else { List::Cons(n, Box::new(build(wrapping_sub(n, 1)))) } }
fn sum(in l: List) -> u64 { match l { List::Nil => 0, List::Cons(x, rest) => match rest { Box(r) => wrapping_add(x, sum(r)) } } }
fn main() -> u64 { sum(inc_all(build(30))) }"#,
];

#[test]
fn boxes_and_recursive_enums_run_like_the_evaluator() {
    // The evaluator recurses once per list cell.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(heap_programs)
        .expect("thread")
        .join()
        .expect("heap programs");
}

fn heap_programs() {
    for (i, src) in HEAP_PROGRAMS.iter().enumerate() {
        let p = vesper::parser::parse(src);
        assert!(p.diagnostics.is_empty(), "program {i}: {:?}", p.diagnostics);
        let c = vesper::check::check(&p.module);
        assert!(c.diagnostics.is_empty(), "program {i}: {:?}", c.diagnostics);
        let expected = evaluate(&c.world);
        for optimize in [false, true] {
            let o = options(optimize);
            let program = backend_program(&c.world, &o).expect("optimize");
            let input = wasm_input(&c.world, &program, "main", &o).expect("encodable");
            let component = match outcome(&backend().words(&input)) {
                Outcome::Module { core, wit, .. } => wrap(core, &wit, o.world),
                other => panic!("program {i} (optimize {optimize}): {other:?}"),
            };
            let got = run(&component, &Ctx::default())
                .unwrap_or_else(|e| panic!("program {i} (optimize {optimize}): {e}"));
            assert_eq!(
                got.to_string(),
                expected,
                "program {i} (optimize {optimize})"
            );
        }
    }
}

/// The driver of `front_end_parses_like_native`: for `BASE/in<i>.ves`,
/// i = 0, 1, ... until one is missing, writes `@vesper/front`'s parse tables
/// as little-endian words to `BASE/out<i>.bin`, and returns the file count.
const FRONT_MAIN: &str = r#"use @vesper/front::{ parse_tables };
use @vesper-std/fs::{ IoError, exists, read_string, write };

fn name(in stem: Str, i: u64, in ext: Str) -> String {
    let mut s = String::new();
    s.push_str("BASE/");
    s.push_str(stem);
    let mut digits = ChunkSeq::new[u8]();
    let mut n = i;
    loop {
        digits.push(wrapping_u8(wrapping_add(48, n % 10)));
        n = n / 10;
        if n == 0 { break; }
    }
    let mut k = digits.len();
    loop {
        if k == 0 { break; }
        k = wrapping_sub(k, 1);
        match digits.try_get(k) {
            Option::Some(d) => { let b = s.push_char(widen_u32(d)); },
            Option::None => {},
        }
    }
    s.push_str(ext);
    s
}

fn one(i: u64) -> u64 eff [Ffi[StdFs], Fail[IoError]] {
    let src = read_string(name("input", i, ".ves"));
    let t = parse_tables(src);
    let mut out = ChunkSeq::new[u8]();
    for j in 0..t.len() {
        match t.try_get(j) {
            Option::Some(w) => {
                out.push(wrapping_u8(w & 255));
                out.push(wrapping_u8((w >> 8) & 255));
                out.push(wrapping_u8((w >> 16) & 255));
                out.push(wrapping_u8((w >> 24) & 255));
                out.push(wrapping_u8((w >> 32) & 255));
                out.push(wrapping_u8((w >> 40) & 255));
                out.push(wrapping_u8((w >> 48) & 255));
                out.push(wrapping_u8((w >> 56) & 255));
            },
            Option::None => {},
        }
    }
    write(name("output", i, ".bin"), out.as_slice());
    t.len()
}

// A tail call, not a loop: the wasm heap is a bump heap that is reclaimed
// only at self tail calls, so each file's garbage goes before the next.
fn all(i: u64) -> u64 eff [Ffi[StdFs], Fail[IoError]] {
    if !exists(name("input", i, ".ves")) { return i; }
    let n = one(i);
    all(wrapping_add(i, 1))
}

fn main() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        all(0)
    } with {
        return n => n,
        raise(e, k) => wrapping_add(18446744073709551000, widen_u64(e.kind)),
    }
}
"#;

/// The corpus `front_end_parses_like_native` parses.
fn front_corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("corpus dir")
            .map(|e| e.expect("entry").path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "ves") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    for d in ["examples", "packages/vesper-front/src"] {
        walk(Path::new(d), &mut out);
    }
    out
}

/// Lane 10 step 3 (#1508): a compiler package built to wasm. A package over
/// `@vesper/front` and `@vesper-std/fs` reads each corpus file, parses it
/// and writes the parse tables. It runs natively (JIT plus the std host
/// shim) and as a wasm component under Wasmtime, and every output file
/// must be byte-identical.
#[test]
fn front_end_parses_like_native() {
    if !cfg!(target_os = "linux") {
        // The native run binds crates/vesper-std-host through the JIT.
        return;
    }
    let corpus = front_corpus();
    assert!(corpus.len() > 10, "corpus: {corpus:?}");
    let base = fresh("front");
    for (i, p) in corpus.iter().enumerate() {
        std::fs::copy(p, base.join(format!("input{i}.ves"))).expect("copy input");
    }
    let pkg = fresh("front-pkg");
    std::fs::create_dir_all(pkg.join("src")).expect("package dir");
    let dep = |p: &str| {
        std::fs::canonicalize(p)
            .expect("dependency")
            .display()
            .to_string()
    };
    std::fs::write(
        pkg.join("Loom.toml"),
        format!(
            "[package]\nname = \"@wasm-seed/front-main\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\"]\n\n\
             [dependencies]\n\"@vesper/front\" = {{ version = \"^0.1\", path = {:?} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {:?} }}\n",
            dep("packages/vesper-front"),
            dep("packages/vesper-std/fs"),
        ),
    )
    .expect("Loom.toml");
    std::fs::write(pkg.join("src/main.ves"), with_base(FRONT_MAIN, &base)).expect("main.ves");
    let assembled = vesper::packages::load(&pkg).expect("load");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let c = vesper::ffi_lowering::check_module(&assembled.module, &imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let n = corpus.len().to_string();

    let native = c
        .jit_with_options("main", &vesper::ffi_lowering::std_host_symbols(), true)
        .expect("native run")
        .to_string();
    assert_eq!(native, n, "native run");
    let expected: Vec<Vec<u8>> = (0..corpus.len())
        .map(|i| {
            let p = base.join(format!("output{i}.bin"));
            let b = std::fs::read(&p).expect("native output");
            std::fs::remove_file(&p).expect("clear output");
            b
        })
        .collect();

    let component = port(&c, &options(true)).expect("port");
    let ctx = Ctx {
        dirs: vec![(base.clone(), base.to_str().unwrap().to_owned())],
        fuel: Some(u64::MAX),
        ..Ctx::default()
    };
    let got = run(&component, &ctx).expect("wasm run");
    assert_eq!(got.to_string(), n, "wasm run");
    for (i, want) in expected.iter().enumerate() {
        let have = std::fs::read(base.join(format!("output{i}.bin"))).expect("wasm output");
        assert!(
            have == *want,
            "{}: the wasm parse tables differ from native ({} vs {} bytes)",
            corpus[i].display(),
            have.len(),
            want.len()
        );
    }
    // Give the C runner a single source and an explicit destination. All the
    // other inputs still exist on the host, but must be invisible to the guest.
    let runner = seed_runner::build_runner(&pkg);
    let module = pkg.join("front.wasm");
    std::fs::write(&module, &component).expect("write parser component");
    let input = base.join("input0.ves");
    let output = base.join("output0.bin");
    std::fs::remove_file(&output).expect("clear Wasmtime output");
    let run = std::process::Command::new(runner)
        .arg("--input")
        .arg(&input)
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg(&output)
        .args(["--invoke", "vesper:program/entry@0.1.0#main", "--print"])
        .arg(module)
        .output()
        .expect("run parser in C seed runner");
    assert!(
        run.status.success(),
        "C seed runner: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1");
    assert_eq!(
        std::fs::read(output).expect("C runner parse tables"),
        expected[0],
        "C runner parse tables for {}",
        corpus[0].display()
    );
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&pkg);
}

#[test]
fn seed_runner_grants_only_the_named_files_and_modes() {
    let src = r#"
fn opens(in name: Str, mode: u32) -> u64 eff [Ffi[StdFs]] {
    let mut err: u64 = 0;
    match StdFs::vstd_fs_open(in name.as_bytes(), mode, inout err) {
        Option::Some(f) => { StdFs::vstd_fs_close(f); 1 },
        Option::None => 0,
    }
}
fn main() -> u64 eff [Ffi[StdFs]] {
    if opens("/src/in", 0) != 1 { return 1; }
    if opens("/src/out", 1) != 1 { return 2; }
    if opens("/src/in", 1) != 0 { return 3; }
    if opens("/src/out", 0) != 0 { return 4; }
    if opens("/src/../src/in", 0) != 0 { return 5; }
    if opens("/src/unlisted", 0) != 0 { return 6; }
    0
}
"#;
    let dir = fresh("seed-grants");
    let input = dir.join("input");
    let output = dir.join("output");
    std::fs::write(&input, b"immutable").unwrap();
    std::fs::write(dir.join("unlisted"), b"hidden").unwrap();
    let module = dir.join("grants.wasm");
    std::fs::write(&module, port(&check(src), &options(true)).expect("port")).unwrap();
    let run = std::process::Command::new(seed_runner::build_runner(&dir))
        .args(["--input", "/src/in"])
        .arg(&input)
        .args(["--output", "/src/out"])
        .arg(&output)
        .args(["--invoke", "vesper:program/entry@0.1.0#main", "--print"])
        .arg(module)
        .output()
        .expect("run C seed runner");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "0");
    assert_eq!(std::fs::read(input).unwrap(), b"immutable");
    assert_eq!(std::fs::read(output).unwrap(), b"");
    std::fs::remove_dir_all(dir).unwrap();
}

const WASM_BACKEND_MAIN: &str = r#"use @vesper/wasm-backend::{ wasm_backend_compile_bytes };
use @vesper-std/fs::{ IoError, read, write };

fn compile_file() eff [Ffi[StdFs], Fail[IoError]] {
    let input = read("/src/compiler-input.bin");
    let words = wasm_backend_compile_bytes(in input.as_slice());
    let mut output = ChunkSeq::new[u8]();
    for i in 0..words.len() {
        match words.try_get(i) {
            Option::Some(w) => {
                let mut remaining = w;
                for byte in 0..8 {
                    output.push(wrapping_u8(remaining));
                    remaining = remaining >> 8;
                }
            },
            Option::None => {},
        }
    }
    write("/src/compiler-output.bin", output.as_slice());
}

fn main() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in { compile_file() } with {
        return value => 0,
        raise(e, k) => wrapping_add(1, widen_u64(e.kind)),
    }
}
"#;

/// Compile the production Vesper backend to wasm. Run its small output and
/// compare the core it emits for its own Place IR with the native port's core.
#[test]
fn wasm_backend_emits_runnable_core_under_seed_runner() {
    let dir = fresh("backend-self-build");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let backend_path = repo().join("packages/vesper-wasm-backend");
    let fs = repo().join("packages/vesper-std/fs");
    std::fs::write(
        dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@wasm-seed/backend-main\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\"]\n\
             [dependencies]\n\"@vesper/wasm-backend\" = {{ version = \"^0.1\", path = {backend_path:?} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {fs:?} }}\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/main.ves"), WASM_BACKEND_MAIN).unwrap();
    let assembled = vesper::packages::load(&dir).expect("load backend package");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let c = vesper::ffi_lowering::check_module(&assembled.module, &imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let o = WasmOptions {
        // Self-compiling the backend needs several hundred MiB of guest heap.
        max_memory_pages: 16_384,
        ..options(false)
    };
    let backend_world = &c.checked.world;
    let program = backend_program(backend_world, &o).expect("lower the wasm backend");
    let own_request =
        wasm_input(backend_world, &program, "main", &o).expect("encode the wasm backend");
    let own_expected = backend().words(&own_request);
    let Outcome::Module {
        core: own_core,
        wit,
        ..
    } = outcome(&own_expected)
    else {
        panic!("the native port did not compile the wasm backend");
    };
    let component = wrap(own_core.clone(), &wit, o.world);

    let target = vesper::parser::parse("fn main() -> u64 { 42 }");
    assert!(target.diagnostics.is_empty(), "{:?}", target.diagnostics);
    let target = vesper::check::check(&target.module);
    assert!(target.diagnostics.is_empty(), "{:?}", target.diagnostics);
    let program = backend_program(&target.world, &o).expect("lower target");
    let request = wasm_input(&target.world, &program, "main", &o).expect("encode target");
    let expected = backend().words(&request);
    assert!(matches!(outcome(&expected), Outcome::Module { .. }));

    let module = dir.join("backend.wasm");
    let input = dir.join("compiler-input.bin");
    let output = dir.join("compiler-output.bin");
    std::fs::write(&module, component).unwrap();
    let runner = seed_runner::build_runner(&dir);
    let invoke = |cmd: &mut std::process::Command, module: &Path, fuel: u64| {
        let run = cmd
            .args(["--invoke", "vesper:program/entry@0.1.0#main", "--print"])
            .arg("--fuel")
            .arg(fuel.to_string())
            .arg(module)
            .output()
            .expect("run C seed runner");
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        String::from_utf8(run.stdout).unwrap().trim().to_owned()
    };
    let compile = |request: &[u8], fuel| {
        std::fs::write(&input, request).unwrap();
        assert_eq!(
            invoke(
                std::process::Command::new(&runner)
                    .args(["--input", "/src/compiler-input.bin"])
                    .arg(&input)
                    .args(["--output", "/src/compiler-output.bin"])
                    .arg(&output),
                &module,
                fuel
            ),
            "0"
        );
        let words = std::fs::read(&output)
            .unwrap()
            .chunks(8)
            .map(|word| u64::from_le_bytes(word.try_into().expect("complete output word")))
            .collect::<Vec<_>>();
        std::fs::remove_file(&output).unwrap();
        words
    };
    let words = compile(&request, 200_000_000);
    assert_eq!(words, expected);
    let Outcome::Module { core, .. } = outcome(&words) else {
        panic!("the guest backend did not emit a module")
    };
    let target = dir.join("target.wasm");
    std::fs::write(&target, core).unwrap();
    assert_eq!(
        invoke(
            &mut std::process::Command::new(&runner),
            &target,
            200_000_000
        ),
        "42"
    );

    let from_guest = compile(&own_request, 10_000_000_000);
    let Outcome::Module {
        core: guest_core, ..
    } = outcome(&from_guest)
    else {
        panic!(
            "the guest backend did not compile itself: {:?}",
            &from_guest[..from_guest.len().min(4)]
        );
    };
    assert!(
        guest_core == own_core,
        "backend fixed point differs (guest {} bytes, native {} bytes)",
        guest_core.len(),
        own_core.len()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_collects_package_under_wasi() {
    let dir = fresh("stage2-collect");
    let sample_dir = dir.join("sample");
    let runner_dir = dir.join("runner");
    std::fs::create_dir_all(sample_dir.join("src")).unwrap();
    std::fs::create_dir_all(runner_dir.join("src")).unwrap();

    std::fs::write(
        sample_dir.join("Loom.toml"),
        "[package]\nname = \"@vesper-test/sample\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        sample_dir.join("src/lib.ves"),
        "mod sub;\n\
         use self::sub::helper;\n\
         struct Pair { a: u64, b: u64 }\n\
         enum Flag { Off, On(u64) }\n\
         effect Ping { fn ping(x: u64) -> u64; }\n\
         pub fn main() -> u64 { helper(21) }\n",
    )
    .unwrap();
    std::fs::write(
        sample_dir.join("src/sub.ves"),
        "pub fn helper(x: u64) -> u64 { wrapping_mul(x, 2) }\n",
    )
    .unwrap();

    let stage2_path = repo()
        .join("packages/vesper-stage2")
        .to_string_lossy()
        .replace('\\', "/");
    std::fs::write(
        runner_dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@vesper-test/stage2-run\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\"@vesper/stage2\" = {{ version = \"^0.1\", path = \"{stage2_path}\" }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        runner_dir.join("src/lib.ves"),
        "use @vesper/stage2::stage2_collect_dir;\n\
         pub fn main() -> u64 eff [Ffi[StdFs]] {\n\
             let (mods, fns, adts, diags) = stage2_collect_dir(in \".\");\n\
             wrapping_add(\n\
                 wrapping_add(wrapping_mul(mods, 1000000), wrapping_mul(fns, 10000)),\n\
                 wrapping_add(wrapping_mul(adts, 100), diags)\n\
             )\n\
         }\n",
    )
    .unwrap();

    let assembled = vesper::packages::load_for_target(&runner_dir, Some("wasm32-wasip2"), false).expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let mut o = options(false);
    o.max_memory_pages = 65535;
    let component = port(&checked, &o).expect("compile @vesper/stage2 to wasm");
    let got = run(
        &component,
        &Ctx {
            dirs: vec![(sample_dir, ".".into())],
            ..Ctx::default()
        },
    )
    .expect("run @vesper/stage2 under WASI");
    // 3 modules (prelude + `lib.ves` + `sub.ves`), 19 functions (17 prelude + `main` + `helper`),
    // 4 ADTs (2 prelude + `Pair` + `Flag`), 0 diagnostic bytes => 3190400.
    assert_eq!(got, 3190400);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_compiles_package_to_runnable_wasm() {
    let dir = fresh("stage2-compile");
    let sample_dir = dir.join("sample");
    let runner_dir = dir.join("runner");
    std::fs::create_dir_all(sample_dir.join("src")).unwrap();
    std::fs::create_dir_all(runner_dir.join("src")).unwrap();

    std::fs::write(
        sample_dir.join("Loom.toml"),
        "[package]\nname = \"@vesper-test/sample\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        sample_dir.join("src/lib.ves"),
        "mod sub;\n\
         use self::sub::helper;\n\
         struct Pair { a: u64, b: u64 }\n\
         enum Flag { Off, On(u64) }\n\
         pub fn main() -> u64 {\n\
             let p = Pair { a: 10, b: 11 };\n\
             let f = Flag::On(p.a);\n\
             let v = match f {\n\
                 Flag::Off => 0,\n\
                 Flag::On(x) => wrapping_add(x, p.b),\n\
             };\n\
             helper(v)\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        sample_dir.join("src/sub.ves"),
        "pub fn helper(x: u64) -> u64 { wrapping_mul(x, 2) }\n",
    )
    .unwrap();

    let stage2_path = repo()
        .join("packages/vesper-stage2")
        .to_string_lossy()
        .replace('\\', "/");
    std::fs::write(
        runner_dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@vesper-test/stage2-compile-run\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\"@vesper/stage2\" = {{ version = \"^0.1\", path = \"{stage2_path}\" }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        runner_dir.join("src/lib.ves"),
        "use @vesper/stage2::stage2_compile_dir;\n\
         pub fn main() -> u64 eff [Ffi[StdFs]] {\n\
             let (code, val) = stage2_compile_dir(in \".\", in \"out.wasm\", in \"main\");\n\
             wrapping_add(wrapping_mul(code, 1000000), val)\n\
         }\n",
    )
    .unwrap();

    let assembled = vesper::packages::load_for_target(&runner_dir, Some("wasm32-wasip2"), false).expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let mut o = options(false);
    o.max_memory_pages = 65535;
    let component = port(&checked, &o).expect("compile @vesper/stage2 to wasm");
    let got = run(
        &component,
        &Ctx {
            dirs: vec![(sample_dir.clone(), ".".into())],
            ..Ctx::default()
        },
    )
    .expect("run @vesper/stage2 compile under WASI");
    assert!(got > 0 && got < 1_000_000, "stage2_compile_dir returned {got}");

    let out_core = std::fs::read(sample_dir.join("out.wasm")).expect("read out.wasm");
    let runner_bin = seed_runner::build_runner(&dir);
    let out_file = dir.join("out.wasm");
    std::fs::write(&out_file, &out_core).unwrap();
    let exec = std::process::Command::new(&runner_bin)
        .args([
            "--invoke",
            &format!("{}#main", vesper::wasm_backend::ENTRY_INTERFACE),
            "--print",
        ])
        .arg(&out_file)
        .output()
        .expect("run vwasm on stage2 output");
    assert!(
        exec.status.success(),
        "vwasm failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let stdout = String::from_utf8_lossy(&exec.stdout);
    assert_eq!(stdout.trim(), "42", "stage2-compiled wasm returned {stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_compiles_vesper_ast() {
    let dir = fresh("stage2-ast");
    let runner_dir = dir.join("runner");
    std::fs::create_dir_all(runner_dir.join("src")).unwrap();

    let stage2_path = repo()
        .join("packages/vesper-stage2")
        .to_string_lossy()
        .replace('\\', "/");
    std::fs::write(
        runner_dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@vesper-test/stage2-ast-run\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\"@vesper/stage2\" = {{ version = \"^0.1\", path = \"{stage2_path}\" }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        runner_dir.join("src/lib.ves"),
        "use @vesper/stage2::stage2_compile_dir;\n\
         pub fn main() -> u64 eff [Ffi[StdFs]] {\n\
             let (code, val) = stage2_compile_dir(in \"packages/vesper-stage2\", in \"target/tmp_stage2_ast_out.wasm\", in \"main\");\n\
             wrapping_add(wrapping_mul(code, 1000000), val)\n\
         }\n",
    )
    .unwrap();

    let assembled = vesper::packages::load_for_target(&runner_dir, Some("wasm32-wasip2"), false).expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let mut o = options(false);
    o.max_memory_pages = 65535;
    let component = port(&checked, &o).expect("compile @vesper/stage2 to wasm");
    let got = run(
        &component,
        &Ctx {
            dirs: vec![(repo(), ".".into())],
            fuel: Some(u64::MAX),
            ..Ctx::default()
        },
    )
    .expect("run @vesper/stage2 compile under WASI");
    assert!(got > 1_000_000 && got < 10_000_000, "stage2_compile_dir returned {got}");
    let out_bytes = std::fs::read(repo().join("target/tmp_stage2_ast_out.wasm")).expect("read out");
    assert_eq!(out_bytes.len(), got as usize);
    assert!(out_bytes.starts_with(b"\0asm"), "invalid wasm header");
    let _ = std::fs::remove_file(repo().join("target/tmp_stage2_ast_out.wasm"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_self_compilation_fixed_point() {
    let dir = fresh("stage2-fixed-point");
    let stage2_pkg = repo().join("packages/vesper-stage2");
    let assembled = vesper::packages::load_for_target(&stage2_pkg, Some("wasm32-wasip2"), false)
        .expect("load @vesper/stage2");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let mut o = options(false);
    o.max_memory_pages = 65535;
    let boot_component = port(&checked, &o).expect("compile @vesper/stage2 boot component");

    let stage2_rel = "target/tmp_stage2_fp.wasm";
    let stage3_rel = "target/tmp_stage3_fp.wasm";
    let stage2_path = repo().join(stage2_rel);
    let stage3_path = repo().join(stage3_rel);
    let _ = std::fs::remove_file(&stage2_path);
    let _ = std::fs::remove_file(&stage3_path);

    let got = run(
        &boot_component,
        &Ctx {
            args: vec!["stage2", "packages/vesper-stage2", stage2_rel, "main"],
            dirs: vec![(repo(), ".".into())],
            fuel: Some(u64::MAX),
            ..Ctx::default()
        },
    )
    .expect("run @vesper/stage2 boot under WASI");
    assert_eq!(got, 0, "stage2 boot returned {got}");
    let stage2_bytes = std::fs::read(&stage2_path).expect("read stage2.wasm");
    assert!(stage2_bytes.starts_with(b"\0asm"), "invalid stage2.wasm");

    // Wrap stage2.wasm (core module emitted by @vesper/stage2) into a WASI component and run it to produce stage3.wasm.
    let program = backend_program(&checked.checked.world, &o).expect("backend_program");
    let input = wasm_input(&checked.checked.world, &program, "main", &o).expect("wasm_input");
    let wit = match outcome(&backend().words(&input)) {
        Outcome::Module { wit, .. } => wit,
        other => panic!("expected module wit, got {other:?}"),
    };
    let stage2_component = wrap(stage2_bytes.clone(), &wit, o.world);
    let got3 = run(
        &stage2_component,
        &Ctx {
            args: vec!["stage2", "packages/vesper-stage2", stage3_rel, "main"],
            dirs: vec![(repo(), ".".into())],
            fuel: Some(u64::MAX),
            ..Ctx::default()
        },
    )
    .expect("run @vesper/stage2 self-built component under WASI");
    assert_eq!(got3, 0, "stage2 self-built returned {got3}");

    let stage3_bytes = std::fs::read(&stage3_path).expect("read stage3.wasm");
    assert_eq!(
        stage2_bytes, stage3_bytes,
        "Stage 2 fixed point mismatch: stage2.wasm ({} bytes) != stage3.wasm ({} bytes)",
        stage2_bytes.len(),
        stage3_bytes.len()
    );

    let _ = std::fs::remove_file(&stage2_path);
    let _ = std::fs::remove_file(&stage3_path);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_compiles_package_under_seed_runner() {
    let dir = fresh("stage2-seed-runner");
    let sample_dir = dir.join("sample");
    std::fs::create_dir_all(sample_dir.join("src")).unwrap();
    std::fs::write(
        sample_dir.join("Loom.toml"),
        "[package]\nname = \"@vesper-test/sample\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        sample_dir.join("src/lib.ves"),
        "pub fn main() -> u64 { 42 }\n",
    )
    .unwrap();

    let stage2_pkg = repo().join("packages/vesper-stage2");
    let assembled = vesper::packages::load_for_target(&stage2_pkg, Some("wasm32-wasip2"), false)
        .expect("load @vesper/stage2");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let mut o = options(false);
    o.max_memory_pages = 65535;
    let boot_component = port(&checked, &o).expect("compile @vesper/stage2 boot component");
    let stage2_wasm = dir.join("stage2_boot.wasm");
    std::fs::write(&stage2_wasm, &boot_component).unwrap();

    let runner_bin = seed_runner::build_runner(&dir);
    let out_wasm = dir.join("out.wasm");
    let exec = std::process::Command::new(&runner_bin)
        .args([
            "--arg",
            "stage2",
            "--arg",
            "/sample",
            "--arg",
            "/out.wasm",
            "--arg",
            "main",
            "--cwd",
            "/",
            "--stack",
            "65536",
            "--input",
            "/sample/Loom.toml",
        ])
        .arg(sample_dir.join("Loom.toml"))
        .args(["--input", "/sample/src/lib.ves"])
        .arg(sample_dir.join("src/lib.ves"))
        .args(["--output", "/out.wasm"])
        .arg(&out_wasm)
        .args([
            "--invoke",
            &format!("{}#main", vesper::wasm_backend::ENTRY_INTERFACE),
            "--print",
        ])
        .arg(&stage2_wasm)
        .output()
        .expect("run vwasm on stage2");
    assert!(
        exec.status.success(),
        "vwasm stage2 failed: stdout={} stderr={}",
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&exec.stdout).trim(), "0");

    let run_out = std::process::Command::new(&runner_bin)
        .args([
            "--invoke",
            &format!("{}#main", vesper::wasm_backend::ENTRY_INTERFACE),
            "--print",
        ])
        .arg(&out_wasm)
        .output()
        .expect("run vwasm on stage2 output");
    assert!(
        run_out.status.success(),
        "vwasm out.wasm failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_out.stdout).trim(), "42");
    std::fs::remove_dir_all(dir).unwrap();
}
