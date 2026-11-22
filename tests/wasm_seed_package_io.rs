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
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ffi_lowering::{ForeignChecked, Handler, Imports};
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-wasm-backend"))
}

fn imports() -> Imports {
    let (triple, abi) = vesper::packages::vsys_target(None).expect("host target");
    let mut imports = Imports::new();
    for lib in ["env", "fs"] {
        let path = format!("packages/vesper-std/{lib}/{lib}.vsys");
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
    match outcome(&package().entry("wasm_backend_compile", true).words(&input)) {
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
    store.set_fuel(200_000_000).unwrap();
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
    let want = mix(mix(mix(0, put), get), 1 | (2 << 32));
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
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
            let component =
                match outcome(&package().entry("wasm_backend_compile", true).words(&input)) {
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
