//! Rust exit gap wave 1 lane 10 (#1508): the standard I/O libraries
//! (`.vsys` `StdEnv` of @vesper-std/env and `StdFs` of @vesper-std/fs) in
//! `@vesper/wasm-backend`, lowered over WASI 0.2
//! (packages/vesper-wasm-backend/src/host.ves). The wasm seed reads its
//! sources and writes its outputs through them.
//!
//! Each file-system program runs twice: natively (the JIT bound to the
//! reference `vstd_fs_*` host in tests/common/vstd_fs_oracle.rs) in one
//! fresh directory, and as a
//! wasm component under Wasmtime with another fresh directory preopened at
//! its own absolute path. Both must return the same value, which folds in
//! every result and error word (the shim reports Linux errnos, so the
//! comparison runs on Linux). Arguments and environment variables are
//! checked against the values the WASI context gives.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/corpus_shard.rs"]
mod corpus_shard;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/seed_runner.rs"]
mod seed_runner;
#[path = "std_io_support/mod.rs"]
mod std_io_support;
#[path = "common/vstd_fs_oracle.rs"]
mod vstd_fs_oracle;
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ffi_lowering::{ForeignChecked, Handler, Imports};
use vesper::wasm_backend::{EffectLowering, WasmOptions};
use wasm_words::*;

/// Bazel runs this target as three shards (`test_shards` in BUILD.bazel), so
/// each can start beside other tests instead of waiting for one 12 GiB slot
/// at the end of "Test all targets". Shard 0 runs the self-compilation fixed
/// point (the longest test) and the short tests; shards 1 and 2 split the
/// other five stage2 builds. Cargo, without Bazel's shard variables, runs
/// every test.
fn runs_on_shard(owner: usize) -> bool {
    let shard = corpus_shard::Shard::from_environment();
    owner % shard.total == shard.index
}

fn backend() -> &'static Entry {
    static BACKEND: OnceLock<Entry> = OnceLock::new();
    BACKEND.get_or_init(|| {
        Package::load("packages/vesper-wasm-backend").entry("wasm_backend_compile", true)
    })
}

fn backend_diag() -> &'static Entry {
    static BACKEND_DIAG: OnceLock<Entry> = OnceLock::new();
    BACKEND_DIAG.get_or_init(|| {
        Package::load("packages/vesper-wasm-backend").entry("wasm_backend_compile_diag", true)
    })
}

fn imports() -> Imports {
    let (triple, abi) = vesper::packages::vsys_target(None).expect("host target");
    let mut imports = Imports::new();
    for lib in ["env/env-wasip2", "fs/fs-wasip2-wasm32"] {
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

/// Stage 2's own compile limits (`packages/vesper-stage2/src/lib.ves`
/// `empty_facts`): the boot compile of @vesper/stage2 outgrows the CLI's
/// default compilation budget.
const STAGE2_MAX_MEMORY_PAGES: u32 = 65535;
const STAGE2_MAX_FUNCTIONS: usize = 65_536;
const STAGE2_MAX_INSTRUCTIONS: usize = 50_000_000;

fn stage2_boot_options() -> WasmOptions {
    WasmOptions {
        max_memory_pages: STAGE2_MAX_MEMORY_PAGES,
        max_functions: STAGE2_MAX_FUNCTIONS,
        max_instructions: STAGE2_MAX_INSTRUCTIONS,
        ..options(false)
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

fn port_with_syms(c: &ForeignChecked, o: &WasmOptions, syms_out: &Path) -> Result<Vec<u8>, String> {
    let w = &c.checked.world;
    let program = backend_program(w, o).ok_or("optimize failed")?;
    let input = wasm_input(w, &program, "main", o).ok_or("not encodable")?;
    let words = backend_diag().words(&input);
    if words.first() != Some(&0) {
        return Err(format!("{:?}", outcome(&words)));
    }
    let syms_len = *words.get(2).ok_or("missing syms len")? as usize;
    let syms_words = syms_len.div_ceil(8);
    let syms_end = 3 + syms_words;
    let mut syms = Vec::with_capacity(syms_words * 8);
    for w in &words[3..syms_end] {
        syms.extend_from_slice(&w.to_le_bytes());
    }
    syms.truncate(syms_len);
    let _ = std::fs::write(syms_out, &syms);
    let mut stripped = Vec::with_capacity(2 + words.len() - syms_end);
    stripped.push(words[0]);
    stripped.push(words[1]);
    stripped.extend_from_slice(&words[syms_end..]);
    match outcome(&stripped) {
        Outcome::Module { core, wit, .. } => Ok(wrap(core, &wit, o.world)),
        o => Err(format!("{o:?}")),
    }
}

#[derive(Default)]
struct MemLimiter {
    current_bytes: usize,
    peak_bytes: usize,
    grow_calls: usize,
    failed_grow: Option<(usize, usize, Option<usize>)>,
}

impl wasmtime::ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.grow_calls += 1;
        if maximum.is_some_and(|m| desired > m) {
            self.failed_grow = Some((current, desired, maximum));
            return Ok(false);
        }
        self.current_bytes = desired;
        if desired > self.peak_bytes {
            self.peak_bytes = desired;
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(maximum.is_none_or(|m| desired <= m))
    }
}

struct WasiHost {
    ctx: wasmtime_wasi::WasiCtx,
    table: wasmtime::component::ResourceTable,
    limiter: MemLimiter,
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
    /// Optional host path prefix matching the guest output `.wasm` path for `.stage`/`.err.txt` diagnostics.
    diag_base: Option<PathBuf>,
    /// Optional `<func_idx> <name>` map file for symbolizing `<wasm function N>` traps.
    syms_file: Option<PathBuf>,
}

fn sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn remove_stage2_sidecars(base: &Path) {
    for suffix in ["", ".err.txt", ".stage", ".syms"] {
        let _ = std::fs::remove_file(sidecar_path(base, suffix));
    }
}

fn demangle_vesper_sym(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'_' if i > 0 => {
                    out.push_str("::");
                    i += 2;
                    continue;
                }
                b'1' => {
                    out.push('_');
                    i += 2;
                    continue;
                }
                b'2' => {
                    out.push('-');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn load_syms(path: &Path) -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        if let Some((idx, name)) = line.split_once(' ')
            && let Ok(n) = idx.parse::<u32>()
        {
            map.insert(n, demangle_vesper_sym(name));
        }
    }
    map
}

fn symbolize_backtrace(raw: &str, syms: &std::collections::HashMap<u32, String>) -> String {
    if syms.is_empty() {
        return raw.to_string();
    }
    let marker = "<wasm function ";
    let mut out = String::with_capacity(raw.len() + 256);
    let mut rest = raw;
    while let Some(pos) = rest.find(marker) {
        out.push_str(&rest[..pos + marker.len()]);
        let after = &rest[pos + marker.len()..];
        if let Some(end) = after.find('>')
            && let Ok(idx) = after[..end].parse::<u32>()
        {
            out.push_str(&after[..end]);
            if let Some(name) = syms.get(&idx) {
                out.push_str(" (");
                out.push_str(name);
                out.push(')');
            }
            out.push('>');
            rest = &after[end + 1..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn read_stage2_sidecar_notes(base: &Path) -> String {
    let mut notes = String::new();
    if let Ok(stage) = std::fs::read_to_string(sidecar_path(base, ".stage")) {
        let trimmed = stage.trim();
        if !trimmed.is_empty() {
            notes.push_str(&format!(
                "\n  stages:\n    {}",
                trimmed.replace('\n', "\n    ")
            ));
        }
    }
    if let Ok(err) = std::fs::read_to_string(sidecar_path(base, ".err.txt")) {
        let trimmed = err.trim();
        if !trimmed.is_empty() {
            notes.push_str(&format!(
                "\n  stage2_diag:\n    {}",
                trimmed.replace('\n', "\n    ")
            ));
        }
    }
    notes
}

fn format_stage2_error(label: &str, got: u64, base: Option<&Path>) -> String {
    let kind_desc = if got == 10 || got == 11 || got == 12 {
        format!("CLI argument error (code={got})")
    } else {
        let code = got / 1_000_000;
        let val = got % 1_000_000;
        match code {
            1 => format!("load_package_graph failed (code=1, detail={val})"),
            2 => format!("name/package assembly failed with {val} diagnostic(s) (code=2)"),
            3 => format!("wasm-backend WErr failed at site={val} (code=3)"),
            4 => format!("IoError writing core wasm (code=4, detail={val})"),
            _ => format!("unexpected stage2 status code={code} val={val}"),
        }
    };
    let notes = base.map(read_stage2_sidecar_notes).unwrap_or_default();
    format!("{label} returned {got} [{kind_desc}]{notes}")
}

fn wasmtime_cli_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WASMTIME_BIN") {
        let path = PathBuf::from(p);
        assert!(
            path.is_file(),
            "WASMTIME_BIN does not point to a file: {}",
            path.display()
        );
        return Some(path);
    }
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(exe);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    if std::env::var_os("TEST_SRCDIR").is_some() {
        panic!("hermetic @wasmtime//:wasmtime CLI binary `{exe}` missing from Bazel PATH");
    }
    None
}

fn precompile_stage2_wasmtime_cli(cli: &Path, stage2_wasm: &Path, out_cwasm: &Path) {
    let _ = std::fs::remove_file(out_cwasm);
    let comp = std::process::Command::new(cli)
        .arg("compile")
        .arg(stage2_wasm)
        .arg("-o")
        .arg(out_cwasm)
        .output()
        .expect("run wasmtime compile");
    assert!(
        comp.status.success(),
        "wasmtime compile failed (status={}):\nstdout={}\nstderr={}",
        comp.status,
        String::from_utf8_lossy(&comp.stdout),
        String::from_utf8_lossy(&comp.stderr),
    );
}

fn run_stage2_wasmtime_cli(
    cli: &Path,
    stage2_artifact: &Path,
    allow_precompiled: bool,
    workspace_dir: &Path,
    out_dir: &Path,
    pkg_dir: &str,
    out_wasm: &Path,
) {
    let ver = std::process::Command::new(cli)
        .arg("--version")
        .output()
        .expect("run wasmtime --version");
    assert!(
        ver.status.success(),
        "wasmtime --version failed: {}",
        String::from_utf8_lossy(&ver.stderr)
    );
    let ver_str = String::from_utf8_lossy(&ver.stdout);
    assert!(
        ver_str.contains("wasmtime "),
        "unexpected wasmtime --version output: {ver_str}"
    );
    if std::env::var_os("TEST_SRCDIR").is_some() || std::env::var_os("WASMTIME_BIN").is_some() {
        assert!(
            ver_str.contains("50.0.1"),
            "expected pinned wasmtime 50.0.1 CLI, got: {ver_str}"
        );
    }

    remove_stage2_sidecars(out_wasm);
    let out_name = out_wasm
        .file_name()
        .expect("out filename")
        .to_string_lossy()
        .into_owned();
    let mut cmd = std::process::Command::new(cli);
    cmd.arg("run");
    if allow_precompiled {
        cmd.arg("--allow-precompiled");
    }
    let exec = cmd
        .arg("--dir")
        .arg(format!("{}::.", workspace_dir.display()))
        .arg("--dir")
        .arg(format!("{}::/out", out_dir.display()))
        .args(["--env", "VESPER_STAGE2_SYMS=1"])
        .args(["--argv0", "stage2"])
        .arg(stage2_artifact)
        .args([pkg_dir, &format!("/out/{out_name}"), "main"])
        .output()
        .expect("run stage2 under wasmtime CLI");
    let notes = read_stage2_sidecar_notes(out_wasm);
    assert!(
        exec.status.success(),
        "wasmtime CLI stage2 failed (status={}):\nstdout={}\nstderr={}{notes}",
        exec.status,
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr),
    );
    let err_txt = std::fs::read_to_string(sidecar_path(out_wasm, ".err.txt")).unwrap_or_default();
    assert!(
        err_txt.trim().is_empty(),
        "stage2 under wasmtime CLI reported error:{notes}"
    );
}

/// Precompiled Wasmtime component + WASI linker reused across repeated calls.
struct PreparedComponent {
    engine: wasmtime::Engine,
    component: wasmtime::component::Component,
    linker: wasmtime::component::Linker<WasiHost>,
}

impl PreparedComponent {
    fn new(component: &[u8]) -> Result<Self, String> {
        use wasmtime::component::{Component, Linker};
        let mut c = wasmtime::Config::new();
        c.wasm_component_model(true);
        c.consume_fuel(true);
        c.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Enable);
        let engine = wasmtime::Engine::new(&c).expect("engine");
        let component = Component::new(&engine, component).map_err(|e| format!("load: {e:?}"))?;
        let mut linker = Linker::<WasiHost>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            engine,
            component,
            linker,
        })
    }

    fn run(&self, ctx: &Ctx) -> Result<u64, String> {
        use wasmtime::component::Val;
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
            &self.engine,
            WasiHost {
                ctx: b.build(),
                table: Default::default(),
                limiter: MemLimiter::default(),
            },
        );
        store.limiter(|h| &mut h.limiter);
        let fuel_budget = ctx.fuel.unwrap_or(200_000_000);
        store.set_fuel(fuel_budget).unwrap();
        let instance = self
            .linker
            .instantiate(&mut store, &self.component)
            .map_err(|e| format!("instantiate: {e:?}"))?;
        let iface = instance
            .get_export_index(&mut store, None, vesper::wasm_backend::ENTRY_INTERFACE)
            .expect("entry interface");
        let main = instance
            .get_export_index(&mut store, Some(&iface), "main")
            .expect("main export");
        let func = instance.get_func(&mut store, main).expect("main func");
        let mut results = vec![Val::Bool(false)];
        if let Err(e) = func.call(&mut store, &[], &mut results) {
            let fuel_left = store.get_fuel().unwrap_or(0);
            let fuel_used = fuel_budget.saturating_sub(fuel_left);
            let lim = &store.data().limiter;
            let cur_mib = lim.current_bytes / (1024 * 1024);
            let peak_mib = lim.peak_bytes / (1024 * 1024);
            let pages = lim.current_bytes / 65536;
            let max_pages = lim.peak_bytes / 65536;
            let syms = ctx.syms_file.as_deref().map(load_syms).unwrap_or_default();
            let bt = symbolize_backtrace(&format!("{e:?}"), &syms);
            let fail_grow = match lim.failed_grow {
                Some((cur, des, max)) => format!(
                    ", denied_grow=({} MiB -> {} MiB, max={})",
                    cur / (1024 * 1024),
                    des / (1024 * 1024),
                    max.map_or_else(|| "none".into(), |m| format!("{} MiB", m / (1024 * 1024)))
                ),
                None => String::new(),
            };
            let notes = ctx
                .diag_base
                .as_deref()
                .map(read_stage2_sidecar_notes)
                .unwrap_or_default();
            return Err(format!(
                "trap: {bt}\n  [wasm_stats: current={cur_mib} MiB ({pages} pages), peak={peak_mib} MiB ({max_pages} pages), grows={}, fuel_used={fuel_used}/{fuel_budget}{fail_grow}]{notes}",
                lim.grow_calls
            ));
        }
        match results.first() {
            Some(Val::U64(x)) => Ok(*x),
            other => Err(format!("result {other:?}")),
        }
    }
}

/// Run the nullary `main` returning `u64` under Wasmtime with WASI.
fn run(component: &[u8], ctx: &Ctx) -> Result<u64, String> {
    PreparedComponent::new(component)?.run(ctx)
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
    if !runs_on_shard(0) {
        return;
    }
    if !cfg!(target_os = "linux") {
        // Error words carry the OS errno, which is Linux's in the shims.
        return;
    }
    for (i, body) in FS_PROGRAMS.iter().enumerate() {
        let src = format!("{PRELUDE}\n{body}");
        let native_dir = fresh(&format!("native{i}"));
        let native = check(&with_base(&src, &native_dir));
        let expected = native
            .jit_with_options("main", &vstd_fs_oracle::symbols(), false)
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
    if !runs_on_shard(0) {
        return;
    }
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
    if !runs_on_shard(0) {
        return;
    }
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
fn preopen_name_overflow_fails_every_path() {
    if !runs_on_shard(0) {
        return;
    }
    // Only 2 preopens (< 16), whose combined guest names exceed the 1,024-byte
    // table budget, with the longer prefix second: resolves must fail with
    // Unsupported (ENOTSUP) rather than routing to `/a`.
    let long_guest = format!("/a/{}", "x".repeat(1024));
    let target_guest = format!("{long_guest}/leaf.txt");
    let prelude = PRELUDE.replace("BASE/", "");
    let src = format!(
        r#"{prelude}
fn main() -> u64 eff [Ffi[StdFs]] {{
    let mut err: u64 = 0;
    match StdFs::vstd_fs_open(in {target_guest:?}.as_bytes(), 1, inout err) {{
        Option::Some(f) => {{ StdFs::vstd_fs_close(f); }},
        Option::None => {{}},
    }}
    err
}}"#
    );
    let c = check(&src);
    let root = fresh("preopen-names");
    let short_dir = root.join("short");
    let long_dir = root.join("long");
    std::fs::create_dir_all(&short_dir).expect("short dir");
    std::fs::create_dir_all(&long_dir).expect("long dir");
    let component = port(&c, &options(false)).expect("port");
    let ctx = Ctx {
        dirs: vec![(short_dir, "/a".into()), (long_dir, long_guest)],
        ..Ctx::default()
    };
    let got = run(&component, &ctx).expect("run");
    assert_eq!(got, 11 | (95 << 32));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_dir_invokes_descriptor_sync_and_propagates_errors() {
    if !runs_on_shard(0) {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use wasmtime::component::{Component, Linker, Resource, Val};
    use wasmtime_wasi::filesystem::WasiFilesystemView;
    use wasmtime_wasi::p2::bindings::sync::filesystem::types::{
        Descriptor, DescriptorType, ErrorCode, Host, HostDescriptor,
    };

    let pkg = fresh("sync-dir-observe");
    std::fs::create_dir(pkg.join("src")).unwrap();
    let base = pkg.join("work");
    std::fs::create_dir(&base).unwrap();
    let fs_dep = std::fs::canonicalize("packages/vesper-std/fs").unwrap();
    std::fs::write(
        pkg.join("Loom.toml"),
        format!(
            r#"[package]
name = "@wasm-seed/sync-observe"
version = "0.1.0"
edition = "2026"
capability_ceiling = ["Alloc", "Fail", "Ffi[StdFs]"]

[dependencies]
"@vesper-std/fs" = {{ version = "^0.1", path = {fs_dep:?} }}
"#
        ),
    )
    .unwrap();
    std::fs::write(
        pkg.join("src/main.ves"),
        r#"use @vesper-std/fs::{ IoError, replace_path };

fn run_replace() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        replace_path(in "/work/out.bin".as_bytes(), in "ok".as_bytes());
        0
    } with {
        return v => v,
        raise(e, _k) => wrapping_add(1_000_000, widen_u64(e.kind)),
    }
}

pub fn main() -> u64 eff [Ffi[StdFs]] {
    let r = run_replace();
    if r != 0 {
        return r;
    }
    let mut err: u64 = 0;
    let s = StdFs::vstd_fs_sync_dir(in "/work".as_bytes(), inout err);
    if s != 0 {
        return wrapping_add(2_000_000, err);
    }
    0
}
"#,
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
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let component_bytes = port(&checked, &options(false)).expect("port");

    let run_with_sync_hook = |fail_on_call: Option<usize>| -> (u64, usize, Vec<DescriptorType>) {
        let mut c = wasmtime::Config::new();
        c.wasm_component_model(true);
        let engine = wasmtime::Engine::new(&c).expect("engine");
        let component = Component::new(&engine, &component_bytes).expect("component");
        let mut linker = Linker::<WasiHost>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi");
        linker.allow_shadowing(true);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_hook = Arc::clone(&calls);
        let kinds = Arc::new(Mutex::new(Vec::new()));
        let kinds_hook = Arc::clone(&kinds);
        linker
            .instance("wasi:filesystem/types@0.2.12")
            .expect("filesystem types instance")
            .func_wrap(
                "[method]descriptor.sync",
                move |mut caller: wasmtime::StoreContextMut<'_, WasiHost>,
                      (desc,): (Resource<Descriptor>,)|
                      -> wasmtime::Result<(Result<(), ErrorCode>,)> {
                    let n = calls_hook.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut fs_view = WasiFilesystemView::filesystem(caller.data_mut());
                    let kind = HostDescriptor::stat(&mut fs_view, Resource::new_borrow(desc.rep()))
                        .expect("stat descriptor before sync")
                        .type_;
                    kinds_hook.lock().unwrap().push(kind);
                    if fail_on_call == Some(n) {
                        return Ok((Err(ErrorCode::Io),));
                    }
                    match HostDescriptor::sync(&mut fs_view, desc) {
                        Ok(()) => Ok((Ok(()),)),
                        Err(err) => {
                            let code = Host::convert_error_code(&mut fs_view, err)?;
                            // Windows FlushFileBuffers denies directory handles with ERROR_ACCESS_DENIED.
                            if cfg!(windows)
                                && kind == DescriptorType::Directory
                                && code == ErrorCode::Access
                            {
                                return Ok((Ok(()),));
                            }
                            Ok((Err(code),))
                        }
                    }
                },
            )
            .expect("shadow descriptor.sync");

        let mut b = wasmtime_wasi::WasiCtxBuilder::new();
        b.preopened_dir(&base, "/work", wasmtime_wasi::FsPerms::ReadWrite)
            .expect("preopen");
        let mut store = wasmtime::Store::new(
            &engine,
            WasiHost {
                ctx: b.build(),
                table: Default::default(),
                limiter: MemLimiter::default(),
            },
        );
        let instance = linker
            .instantiate(&mut store, &component)
            .expect("instantiate");
        let iface = instance
            .get_export_index(&mut store, None, vesper::wasm_backend::ENTRY_INTERFACE)
            .expect("entry iface");
        let main = instance
            .get_export_index(&mut store, Some(&iface), "main")
            .expect("main");
        let func = instance.get_func(&mut store, main).expect("func");
        let mut results = vec![Val::Bool(false)];
        func.call(&mut store, &[], &mut results).expect("call");
        let got = match results.first() {
            Some(Val::U64(x)) => *x,
            other => panic!("unexpected result: {other:?}"),
        };
        let recorded = kinds.lock().unwrap().clone();
        (got, calls.load(Ordering::SeqCst), recorded)
    };

    // Pass 1: replace_path syncs the temp file (call 1) and parent dir (call 2),
    // then direct vstd_fs_sync_dir syncs `/work` (call 3).
    let (ok_got, ok_calls, ok_kinds) = run_with_sync_hook(None);
    assert_eq!(ok_got, 0);
    assert_eq!(ok_calls, 3);
    assert_eq!(
        ok_kinds,
        vec![
            DescriptorType::RegularFile,
            DescriptorType::Directory,
            DescriptorType::Directory,
        ]
    );
    assert_eq!(std::fs::read(base.join("out.bin")).unwrap(), b"ok");

    // Pass 2: inject EIO on call 2 (replace_path's directory sync after file sync + rename)
    // and verify replace_path surfaces IoError instead of acknowledging success.
    let (dir_fail_got, dir_fail_calls, dir_fail_kinds) = run_with_sync_hook(Some(2));
    assert_eq!(dir_fail_calls, 2);
    assert_eq!(
        dir_fail_kinds,
        vec![DescriptorType::RegularFile, DescriptorType::Directory]
    );
    assert!(
        (1_000_000..2_000_000).contains(&dir_fail_got),
        "expected Fail[IoError] from replace_path dir sync failure, got {dir_fail_got}"
    );

    // Pass 3: let replace_path succeed (calls 1..=2) and fail call 3 (direct
    // StdFs::vstd_fs_sync_dir) with ErrorCode::Io; verify non-zero status and EIO errno (5).
    let (raw_fail_got, raw_fail_calls, raw_fail_kinds) = run_with_sync_hook(Some(3));
    assert_eq!(raw_fail_calls, 3);
    assert_eq!(
        raw_fail_kinds,
        vec![
            DescriptorType::RegularFile,
            DescriptorType::Directory,
            DescriptorType::Directory,
        ]
    );
    assert!(
        raw_fail_got >= 2_000_000,
        "expected vstd_fs_sync_dir failure, got {raw_fail_got}"
    );
    let err_word = raw_fail_got - 2_000_000;
    assert_eq!(
        err_word >> 32,
        5,
        "expected Linux EIO (5), got err_word={err_word:#x}"
    );

    let _ = std::fs::remove_dir_all(&pkg);
}

#[test]
fn env_package_selects_wasi_arguments() {
    if !runs_on_shard(0) {
        return;
    }
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
    if !runs_on_shard(0) {
        return;
    }
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
    if !runs_on_shard(0) {
        return;
    }
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
    if !runs_on_shard(0) {
        return;
    }
    if !cfg!(target_os = "linux") {
        // The native run binds the C library through the JIT.
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
    // Natively the package's `@vesper-std/fs` is the C library; in the
    // component it is its WASI implementation (src-wasip2).
    let native_pkg = vesper::packages::load(&pkg).expect("load");
    assert!(
        native_pkg.ok(),
        "{:?} {:?}",
        native_pkg.diagnostics,
        native_pkg.manifest_errors
    );
    let native_imports = native_pkg.imports(None).unwrap();
    let native_checked = vesper::ffi_lowering::check_module(&native_pkg.module, &native_imports)
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let assembled =
        vesper::packages::load_for_target(&pkg, Some(vesper::wasm_backend::TARGET), false)
            .expect("load");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let c = vesper::ffi_lowering::check_module(&assembled.module, &imports())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics));
    let n = corpus.len().to_string();

    let native = native_checked
        .jit_with_options("main", &std_io_support::symbols(), true)
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

    // The guest heap is never freed between files, and the corpus includes
    // the front end's own sources (parse.ves alone is ~4.7k lines since the
    // surface profile), so the default 64 MiB is too tight.
    let o = WasmOptions {
        max_memory_pages: 4096,
        ..options(true)
    };
    let component = port(&c, &o).expect("port");
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
    if !runs_on_shard(0) {
        return;
    }
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
    if !runs_on_shard(0) {
        return;
    }
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
    let assembled =
        vesper::packages::load_for_target(&dir, Some(vesper::wasm_backend::TARGET), false)
            .expect("load backend package");
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

/// How many stage2 tests may hold a compiler build at once. Each one compiles
/// `@vesper/stage2` in-process and runs it with up to 4 GiB of linear memory;
/// all six at once ran the 22GB "Test all targets" VM out of memory (#1870).
/// Under Bazel's three shards each shard builds one at a time, so each stays
/// under about 5 GiB; BUILD.bazel reserves memory per shard to match. Cargo
/// runs every test in one process, two builds at a time (about 9 GiB).
fn stage2_slots() -> usize {
    if corpus_shard::Shard::from_environment().total > 1 {
        1
    } else {
        2
    }
}

/// Holds one of the `stage2_slots()` until dropped, also when the test panics.
struct Stage2Slot;

static STAGE2_IN_USE: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

impl Stage2Slot {
    fn acquire() -> Self {
        let (lock, freed) = &STAGE2_IN_USE;
        let mut in_use = lock.lock().unwrap_or_else(|e| e.into_inner());
        while *in_use >= stage2_slots() {
            in_use = freed.wait(in_use).unwrap_or_else(|e| e.into_inner());
        }
        *in_use += 1;
        Stage2Slot
    }
}

impl Drop for Stage2Slot {
    fn drop(&mut self) {
        let (lock, freed) = &STAGE2_IN_USE;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        freed.notify_one();
    }
}

fn with_large_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let _slot = Stage2Slot::acquire();
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn stage2_collects_package_under_wasi() {
    if !runs_on_shard(2) {
        return;
    }
    with_large_stack(stage2_collects_package_under_wasi_impl);
}

fn stage2_collects_package_under_wasi_impl() {
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

    let assembled =
        vesper::packages::load_for_target(&runner_dir, Some(vesper::wasm_backend::TARGET), false)
            .expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let o = stage2_boot_options();
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
    if !runs_on_shard(2) {
        return;
    }
    with_large_stack(stage2_compiles_package_to_runnable_wasm_impl);
}

fn stage2_compiles_package_to_runnable_wasm_impl() {
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

    let assembled =
        vesper::packages::load_for_target(&runner_dir, Some("wasm32-wasip2"), false).expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let o = stage2_boot_options();
    let component = port(&checked, &o).expect("compile @vesper/stage2 to wasm");
    let out_diag = sample_dir.join("out.wasm");
    remove_stage2_sidecars(&out_diag);
    let got = run(
        &component,
        &Ctx {
            dirs: vec![(sample_dir.clone(), ".".into())],
            diag_base: Some(out_diag.clone()),
            ..Ctx::default()
        },
    )
    .unwrap_or_else(|e| panic!("run @vesper/stage2 compile under WASI:\n{e}"));
    assert!(
        got > 0 && got < 1_000_000,
        "{}",
        format_stage2_error("stage2_compile_dir", got, Some(&out_diag))
    );

    let out_comp = std::fs::read(sample_dir.join("out.wasm")).expect("read out.wasm");
    assert!(
        out_comp.starts_with(b"\0asm\x0d\x00\x01\x00"),
        "expected WASI component header from @vesper/stage2"
    );
    let wasmtime_got = run(&out_comp, &Ctx::default())
        .unwrap_or_else(|e| panic!("run stage2-emitted component under Wasmtime:\n{e}"));
    assert_eq!(wasmtime_got, 42);
    let runner_bin = seed_runner::build_runner(&dir);
    let out_file = dir.join("out.wasm");
    std::fs::write(&out_file, &out_comp).unwrap();
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
    assert_eq!(
        stdout.trim(),
        "42",
        "stage2-compiled wasm returned {stdout}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_compiles_vesper_ast() {
    if !runs_on_shard(1) {
        return;
    }
    with_large_stack(stage2_compiles_vesper_ast_impl);
}

fn stage2_compiles_vesper_ast_impl() {
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
             let (code, val) = stage2_compile_dir(in \"packages/vesper-stage2\", in \"/out/tmp_stage2_ast_out.wasm\", in \"main\");\n\
             wrapping_add(wrapping_mul(code, 1000000), val)\n\
         }\n",
    )
    .unwrap();

    let assembled =
        vesper::packages::load_for_target(&runner_dir, Some("wasm32-wasip2"), false).expect("load");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let o = stage2_boot_options();
    let component = port(&checked, &o).expect("compile @vesper/stage2 to wasm");
    let ast_out_path = dir.join("tmp_stage2_ast_out.wasm");
    remove_stage2_sidecars(&ast_out_path);
    let got = run(
        &component,
        &Ctx {
            dirs: vec![(repo(), ".".into()), (dir.clone(), "/out".into())],
            fuel: Some(u64::MAX),
            diag_base: Some(ast_out_path.clone()),
            ..Ctx::default()
        },
    )
    .unwrap_or_else(|e| panic!("run @vesper/stage2 compile under WASI:\n{e}"));
    let err_note = read_stage2_sidecar_notes(&ast_out_path);
    assert!(
        err_note.is_empty() && got > 1_000_000 && got < 10_000_000,
        "{}",
        format_stage2_error("stage2_compile_dir", got, Some(&ast_out_path))
    );
    let out_bytes = std::fs::read(&ast_out_path).expect("read out");
    assert_eq!(out_bytes.len(), got as usize);
    assert!(
        out_bytes.starts_with(b"\0asm\x0d\x00\x01\x00"),
        "invalid wasm component header"
    );
    remove_stage2_sidecars(&ast_out_path);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stage2_self_compilation_fixed_point() {
    if !runs_on_shard(0) {
        return;
    }
    with_large_stack(stage2_self_compilation_fixed_point_impl);
}

fn stage2_self_compilation_fixed_point_impl() {
    let dir = fresh("stage2-fixed-point");
    let stage2_pkg = repo().join("packages/vesper-stage2");
    let assembled = vesper::packages::load_for_target(&stage2_pkg, Some("wasm32-wasip2"), false)
        .expect("load @vesper/stage2");
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
    let o = stage2_boot_options();
    let stage2_guest = "/out/tmp_stage2_fp.wasm";
    let stage3_guest = "/out/tmp_stage3_fp.wasm";
    let stage2_path = dir.join("tmp_stage2_fp.wasm");
    let stage3_path = dir.join("tmp_stage3_fp.wasm");
    let boot_syms_path = sidecar_path(&stage2_path, ".boot.syms");
    remove_stage2_sidecars(&stage2_path);
    remove_stage2_sidecars(&stage3_path);
    let _ = std::fs::remove_file(&boot_syms_path);
    let boot_component = port_with_syms(&checked, &o, &boot_syms_path)
        .expect("compile @vesper/stage2 boot component");
    let boot = PreparedComponent::new(&boot_component).expect("prepare @vesper/stage2 boot");

    let got = boot
        .run(&Ctx {
            args: vec!["stage2", "packages/vesper-stage2", stage2_guest, "main"],
            env: vec![("VESPER_STAGE2_SYMS", "1")],
            dirs: vec![(repo(), ".".into()), (dir.clone(), "/out".into())],
            fuel: Some(u64::MAX),
            diag_base: Some(stage2_path.clone()),
            syms_file: Some(boot_syms_path.clone()),
        })
        .unwrap_or_else(|e| panic!("run @vesper/stage2 boot under WASI:\n{e}"));
    assert_eq!(
        got,
        0,
        "{}",
        format_stage2_error("stage2 boot", got, Some(&stage2_path))
    );
    let stage2_bytes = std::fs::read(&stage2_path).expect("read stage2.wasm");
    assert!(
        stage2_bytes.starts_with(b"\0asm\x0d\x00\x01\x00"),
        "expected WASI component header from @vesper/stage2"
    );

    // Stage 2 directly emits a runnable WASI component via @vesper/wit-component:
    // instantiate stage2.wasm directly with zero Rust compiler or wrapping glue.
    let stage2_syms = sidecar_path(&stage2_path, ".syms");
    let got3 = run(
        &stage2_bytes,
        &Ctx {
            args: vec!["stage2", "packages/vesper-stage2", stage3_guest, "main"],
            env: vec![("VESPER_STAGE2_SYMS", "1")],
            dirs: vec![(repo(), ".".into()), (dir.clone(), "/out".into())],
            fuel: Some(u64::MAX),
            diag_base: Some(stage3_path.clone()),
            syms_file: Some(stage2_syms),
        },
    )
    .unwrap_or_else(|e| panic!("run @vesper/stage2 self-built component under WASI:\n{e}"));
    assert_eq!(
        got3,
        0,
        "{}",
        format_stage2_error("stage2 self-built", got3, Some(&stage3_path))
    );

    let stage3_bytes = std::fs::read(&stage3_path).expect("read stage3.wasm");
    assert_eq!(
        stage2_bytes,
        stage3_bytes,
        "Stage 2 fixed point mismatch: stage2.wasm ({} bytes) != stage3.wasm ({} bytes)",
        stage2_bytes.len(),
        stage3_bytes.len()
    );

    let cli = wasmtime_cli_bin();
    let stage2_cwasm_path = dir.join("tmp_stage2_fp.cwasm");
    if let Some(cli) = &cli {
        let stage3_cli_path = dir.join("tmp_stage3_cli_fp.wasm");
        run_stage2_wasmtime_cli(
            cli,
            &stage2_path,
            false,
            &repo(),
            &dir,
            "packages/vesper-stage2",
            &stage3_cli_path,
        );
        let stage3_cli_bytes = std::fs::read(&stage3_cli_path).expect("read CLI stage3.wasm");
        assert_eq!(
            stage2_bytes,
            stage3_cli_bytes,
            "Stage 2 wasmtime CLI fixed point mismatch: stage2.wasm ({} bytes) != stage3_cli.wasm ({} bytes)",
            stage2_bytes.len(),
            stage3_cli_bytes.len()
        );
        remove_stage2_sidecars(&stage3_cli_path);

        let stage3_cwasm_path = dir.join("tmp_stage3_cwasm_fp.wasm");
        precompile_stage2_wasmtime_cli(cli, &stage2_path, &stage2_cwasm_path);
        run_stage2_wasmtime_cli(
            cli,
            &stage2_cwasm_path,
            true,
            &repo(),
            &dir,
            "packages/vesper-stage2",
            &stage3_cwasm_path,
        );
        let stage3_cwasm_bytes =
            std::fs::read(&stage3_cwasm_path).expect("read precompiled CLI stage3.wasm");
        assert_eq!(
            stage2_bytes,
            stage3_cwasm_bytes,
            "Stage 2 precompiled wasmtime CLI fixed point mismatch: stage2.wasm ({} bytes) != stage3_cwasm.wasm ({} bytes)",
            stage2_bytes.len(),
            stage3_cwasm_bytes.len()
        );
        remove_stage2_sidecars(&stage3_cwasm_path);
    }

    let stage2 = PreparedComponent::new(&stage2_bytes).expect("prepare stage2.wasm");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    check_stage2_native_object(&stage2, &dir);
    check_compiler_packages_fixed_point(
        &dir,
        &boot,
        &boot_syms_path,
        &stage2,
        &sidecar_path(&stage2_path, ".syms"),
        cli.as_deref().map(|c| (c, stage2_cwasm_path.as_path())),
    );
    let _ = std::fs::remove_file(&stage2_cwasm_path);

    remove_stage2_sidecars(&stage2_path);
    remove_stage2_sidecars(&stage3_path);
    let _ = std::fs::remove_file(&boot_syms_path);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn check_stage2_native_object(stage2: &PreparedComponent, dir: &Path) {
    use object::{Object, ObjectSection, ObjectSymbol};

    let package = dir.join("native-scalar");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(
        package.join("Loom.toml"),
        "[package]\nname = \"@vesper-test/native-scalar\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        package.join("src/lib.ves"),
        "pub fn mix(x: u64, y: u64) -> u64 { wrapping_add(wrapping_mul(x, 5), y ^ 11) }\n\
         pub fn choose(x: u64, y: u64) -> u64 {\n\
             wrapping_add(\n\
                 if x < y { if x == 0 { y } else { wrapping_mul(x, 3) } } else { y ^ 8 },\n\
                 if y >= 2 && x != 0 { x } else { y }\n\
             )\n\
         }\n\
         fn twist(x: u64, y: u64) -> u64 { wrapping_mul(x ^ 5, wrapping_add(y, 1)) }\n\
         fn factorial(x: u64) -> u64 {\n\
             if x <= 1 { 1 } else { wrapping_mul(x, factorial(wrapping_sub(x, 1))) }\n\
         }\n\
         pub fn calls(x: u64, y: u64) -> u64 {\n\
             wrapping_add(twist(y, x), wrapping_add(twist(x, y), factorial(x & 7)))\n\
         }\n\
         fn stack_sum(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {\n\
             wrapping_add(twist(a, b), wrapping_add(twist(c, d), wrapping_add(twist(e, f), twist(g, h))))\n\
         }\n\
         pub fn stack_args(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {\n\
             wrapping_add(stack_sum(h, g, f, e, d, c, b, a), if g < h { h } else { g })\n\
         }\n\
         fn offset(a: i64, b: i64) -> i64 {\n\
             if a < b { wrapping_add(a, -7) } else { wrapping_sub(a, -11) }\n\
         }\n\
         pub fn select_signed(a: i64, b: u64) -> i64 {\n\
             let right = wrapping_i64(b);\n\
             let a = if !(a > right) { offset(a, right) }\n\
                 else { wrapping_i64(wrapping_u64(a) ^ b) };\n\
             let _ = wrapping_add(a, 1);\n\
             a\n\
         }\n\
         pub fn mutate(a: i64) -> i64 { let mut b = a; b = 7; b }\n\
         fn unsupported(x: u64) -> u64 { match x { 0 => 7, _ => 9 } }\n\
         pub fn decline(x: u64) -> u64 { unsupported(x) }\n",
    )
    .unwrap();
    let object = dir.join("native-scalar.o");
    let compile = |entry| {
        stage2
            .run(&Ctx {
                args: vec!["stage2", "native-scalar", "/out/native-scalar.o", entry],
                env: vec![("VESPER_STAGE2_X86_64_ELF", "1")],
                dirs: vec![(dir.into(), ".".into()), (dir.into(), "/out".into())],
                fuel: Some(u64::MAX),
                diag_base: Some(object.clone()),
                ..Ctx::default()
            })
            .expect("Stage 2 native object request")
    };
    assert_eq!(
        compile("mix"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let leaf = dir.join("native-mix.o");
    std::fs::rename(&object, &leaf).unwrap();
    assert_eq!(
        compile("choose"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let branch = dir.join("native-choose.o");
    std::fs::rename(&object, &branch).unwrap();
    assert_eq!(
        compile("calls"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bytes = std::fs::read(&object).unwrap();
    let file = object::File::parse(bytes.as_slice()).unwrap();
    assert_eq!(file.format(), object::BinaryFormat::Elf);
    assert!(file.symbol_by_name("calls").unwrap().is_global());
    for name in ["twist", "factorial"] {
        let symbol = file.symbol_by_name(name).unwrap();
        assert!(symbol.is_definition() && !symbol.is_global());
    }
    assert_eq!(
        file.section_by_name(".text").unwrap().relocations().count(),
        4
    );
    let calls = dir.join("native-calls.o");
    std::fs::rename(&object, &calls).unwrap();
    assert_eq!(
        compile("stack_args"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let stack = dir.join("native-stack-args.o");
    std::fs::rename(&object, &stack).unwrap();
    assert_eq!(
        compile("select_signed"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bytes = std::fs::read(&object).unwrap();
    assert_ne!(compile("decline"), 0);
    assert!(read_stage2_sidecar_notes(&object).contains("unsupported x86-64 ELF entry"));
    assert_eq!(std::fs::read(&object).unwrap(), bytes);
    assert_ne!(compile("mutate"), 0);
    assert_eq!(std::fs::read(&object).unwrap(), bytes);

    let caller = dir.join("native-scalar-caller.c");
    let binary = dir.join("native-scalar-caller");
    std::fs::write(
        &caller,
        "#include <stdint.h>\n\
         extern uint64_t mix(uint64_t, uint64_t);\n\
         extern uint64_t choose(uint64_t, uint64_t);\n\
         extern uint64_t calls(uint64_t, uint64_t);\n\
         extern uint64_t stack_args(uint64_t, uint64_t, uint64_t, uint64_t,\n\
                                    uint64_t, uint64_t, uint64_t, uint64_t);\n\
         extern int64_t select_signed(int64_t, uint64_t);\n\
         int main(void) {\n\
             if (mix(10, 4) != 65 || mix(UINT64_MAX, UINT64_MAX) != UINT64_MAX - 16) return 1;\n\
             if (choose(0, 4) != 8 || choose(2, 4) != 8 || choose(4, 1) != 10\n\
                 || choose(UINT64_MAX, 2) != 9 || choose(2, UINT64_MAX) != 8) return 2;\n\
             if (calls(0, 1) != 15 || calls(6, 2) != 778\n\
                 || calls(UINT64_MAX, UINT64_MAX) != 5040) return 3;\n\
             if (stack_args(1, 2, 3, 4, 5, 6, 7, 8) != 148\n\
                 || stack_args(8, 7, 6, 5, 4, 3, 2, 1) != 62) return 4;\n\
             return select_signed(-5, 3) != -12 || select_signed(5, UINT64_MAX) != -6\n\
                 || select_signed(INT64_MIN, 0) != INT64_MAX - 6;\n\
         }\n",
    )
    .unwrap();
    let link = std::process::Command::new("cc")
        .args([&caller, &leaf, &branch, &calls, &stack, &object])
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("link native object");
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        std::process::Command::new(binary)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn stage2_compiles_package_under_seed_runner() {
    if !runs_on_shard(2) {
        return;
    }
    with_large_stack(stage2_compiles_package_under_seed_runner_impl);
}

fn stage2_compiles_package_under_seed_runner_impl() {
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
    let o = stage2_boot_options();
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

    if let Some(cli) = wasmtime_cli_bin() {
        let cli_out = dir.join("cli_out.wasm");
        run_stage2_wasmtime_cli(&cli, &stage2_wasm, false, &dir, &dir, "sample", &cli_out);
        let cli_bytes = std::fs::read(&cli_out).expect("read cli_out.wasm");
        assert_eq!(cli_bytes, std::fs::read(&out_wasm).unwrap());
        assert_eq!(
            run(&cli_bytes, &Ctx::default())
                .unwrap_or_else(|e| panic!("run wasmtime-CLI compiled sample:\n{e}")),
            42
        );
        remove_stage2_sidecars(&cli_out);
    }

    std::fs::remove_dir_all(dir).unwrap();
}

enum CompilerExecCheck {
    CompileOnly,
    Exact(u64),
    NonZero,
}

/// Every compiler package Stage 2 builds: (stem, package, path, probe
/// expression, execution check). Each probe is a one-line wrapper package
/// whose `main` calls the expression, so Stage 2 compiles the package and
/// every package it depends on.
const COMPILER_PACKAGES: &[(&str, &str, &str, &str, CompilerExecCheck)] = &[
    (
        "front",
        "@vesper/front",
        "packages/vesper-front",
        "parse_tables(in \"fn main() -> u64 { 1 }\").len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "sugar",
        "@vesper/sugar",
        "packages/vesper-sugar",
        "match_rule_words(in \"a $x:expra 1\", 12884901897, 100000).len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "backend-checks",
        "@vesper/backend-checks",
        "packages/vesper-backend-checks",
        "backend_verify_count(in \"\")",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "bounds-hoist",
        "@vesper/bounds-hoist",
        "packages/vesper-bounds-hoist",
        "decide_text(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-dyn",
        "@vesper/check-dyn",
        "packages/vesper-check-dyn",
        "check_dyn_count(in \"\", 0, 0)",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-effects",
        "@vesper/check-effects",
        "packages/vesper-check-effects",
        "check_effects(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-fip",
        "@vesper/check-fip",
        "packages/vesper-check-fip",
        "check_fip_count(in \"\")",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-loops-atomics",
        "@vesper/check-loops-atomics",
        "packages/vesper-check-loops-atomics",
        "check_loops_control(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-pact",
        "@vesper/check-pact",
        "packages/vesper-check-pact",
        "check_pact_mono_plan(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-regions-seqs",
        "@vesper/check-regions-seqs",
        "packages/vesper-check-regions-seqs",
        "regions_seqs_types(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-tensor-types",
        "@vesper/check-tensor-types",
        "packages/vesper-check-tensor-types",
        "check_tensor_types(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-aggregates",
        "@vesper/codegen-aggregates",
        "packages/vesper-codegen-aggregates",
        "aggregate_decisions(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-calls",
        "@vesper/codegen-calls",
        "packages/vesper-codegen-calls",
        "codegen_calls_text(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-control",
        "@vesper/codegen-control",
        "packages/vesper-codegen-control",
        "control_session_plans(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-export",
        "@vesper/codegen-export",
        "packages/vesper-codegen-export",
        "export_plan(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-freestanding",
        "@vesper/codegen-freestanding",
        "packages/vesper-codegen-freestanding",
        "freestanding_constants()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-scalar",
        "@vesper/codegen-scalar",
        "packages/vesper-codegen-scalar",
        "codegen_scalar_decisions(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-seq",
        "@vesper/codegen-seq",
        "packages/vesper-codegen-seq",
        "codegen_seq_decisions(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "codegen-walk",
        "@vesper/codegen-walk",
        "packages/vesper-codegen-walk",
        "native_walk_decisions(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "driver",
        "@vesper/driver",
        "packages/vesper-driver",
        "widen_u64(cmd_check())",
        CompilerExecCheck::Exact(1),
    ),
    (
        "ebpf-backend",
        "@vesper/ebpf-backend",
        "packages/vesper-ebpf-backend",
        "ebpf_backend_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "egraph",
        "@vesper/egraph",
        "packages/vesper-egraph",
        "egraph_math(in \"\").len()",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ffi-lowering",
        "@vesper/ffi-lowering",
        "packages/vesper-ffi-lowering",
        "{ let _ = s_new(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ir-check",
        "@vesper/ir-check",
        "packages/vesper-ir-check",
        "{ let _ = paths_new(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "ir-inline",
        "@vesper/ir-inline",
        "packages/vesper-ir-inline",
        "inline_program_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "ir-opt",
        "@vesper/ir-opt",
        "packages/vesper-ir-opt",
        "ir_opt_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "ir-opt-policy",
        "@vesper/ir-opt-policy",
        "packages/vesper-ir-opt-policy",
        "ir_opt_policy_entry(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "optimize",
        "@vesper/optimize",
        "packages/vesper-optimize",
        "optimize_verify(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "doc",
        "@vesper/doc",
        "packages/vesper-doc",
        "format()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "lsp",
        "@vesper/lsp",
        "packages/vesper-lsp",
        "lsp_frames(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "lsp-features",
        "@vesper/lsp-features",
        "packages/vesper-lsp-features",
        "lsp_document_symbols(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "lsp-proof-run",
        "@vesper/lsp-proof-run",
        "packages/vesper-lsp-proof-run",
        "job_obligations(in \"\".as_bytes())",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "memo",
        "@vesper/memo",
        "packages/vesper-memo",
        "memo_replay(in \"\").len()",
        CompilerExecCheck::Exact(1),
    ),
    (
        "test-runner",
        "@vesper/test-runner",
        "packages/vesper-test-runner",
        "case_rows_words(in \"\".as_bytes()).len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "lean-extract",
        "@vesper/lean-extract",
        "packages/vesper-lean-extract",
        "extract_head_words(in \"\")",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-collect",
        "@vesper/check-collect",
        "packages/vesper-check-collect",
        "collect_text(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "preludes",
        "@vesper/preludes",
        "packages/vesper-preludes",
        "strings_source().as_bytes().len()",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-data",
        "@vesper/check-data",
        "packages/vesper-check-data",
        "check_data_sizes(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "ast",
        "@vesper/ast",
        "packages/vesper-ast",
        "ast_count(in \"fn main() -> u64 { 1 }\")",
        CompilerExecCheck::NonZero,
    ),
    (
        "check-layout",
        "@vesper/check-layout",
        "packages/vesper-check-layout",
        "check_layout_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-core-typing",
        "@vesper/check-core-typing",
        "packages/vesper-check-core-typing",
        "core_typing_exprs(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "check-names-packages",
        "@vesper/check-names-packages",
        "packages/vesper-check-names-packages",
        "module_count(in \"\")",
        CompilerExecCheck::Exact(0),
    ),
    (
        "place-ir",
        "@vesper/place-ir",
        "packages/vesper-place-ir",
        "place_ir_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "place-ir-lower",
        "@vesper/place-ir-lower",
        "packages/vesper-place-ir-lower",
        "ir_lower_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "vsys",
        "@vesper/vsys",
        "packages/vesper-vsys",
        "run_vsys_check(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "clif",
        "@vesper/clif",
        "packages/vesper-clif",
        "inc(1)",
        CompilerExecCheck::Exact(2),
    ),
    (
        "cranelift",
        "@vesper/cranelift",
        "packages/vesper-cranelift",
        "{ let _ = ty_i8(); 0 }",
        CompilerExecCheck::Exact(0),
    ),
    (
        "crypto",
        "@vesper/crypto",
        "packages/vesper-crypto",
        "zeros(4).len()",
        CompilerExecCheck::Exact(4),
    ),
    (
        "native-cold-outline",
        "@vesper/native-cold-outline",
        "packages/vesper-native-cold-outline",
        "outline_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "native-vectorize",
        "@vesper/native-vectorize",
        "packages/vesper-native-vectorize",
        "vectorize_words(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "verified-codegen",
        "@vesper/verified-codegen",
        "packages/vesper-verified-codegen",
        "select_text(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "wasm-backend",
        "@vesper/wasm-backend",
        "packages/vesper-wasm-backend",
        "wasm_backend_compile(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
    (
        "wit-component",
        "@vesper/wit-component",
        "packages/vesper-wit-component",
        "wit_component_wrap(in \"\").len()",
        CompilerExecCheck::CompileOnly,
    ),
];

/// Compiler packages over `Ffi[Cranelift]` (native Cranelift glue over
/// vesper-cranelift-sys): (stem, package, path, probe `src/lib.ves`). Stage 2
/// declares the `Ffi$Cranelift` operations from cranelift.vsys
/// (packages/vesper-stage2/src/vcl_decls.ves), and the Wasm backend lowers
/// them to imports of `vesper:cranelift/vcl@0.1.0` (#2210). The fixed point
/// only builds them: running one needs the Cranelift component composed in
/// (crates/vesper-cranelift-component/tests/wasm_imports.rs).
const STAGE2_CRANELIFT_PACKAGES: &[(&str, &str, &str, &str)] = &[
    (
        "codegen-base",
        "@vesper/codegen-base",
        "packages/vesper-codegen-base",
        "use @vesper/codegen-base::word_of;\nuse @vesper/codegen-base::arg_value;\npub fn main() -> u64 eff [Ffi[Cranelift]] { word_of(arg_value(0, 0)) }\n",
    ),
    (
        "codegen-control-emit",
        "@vesper/codegen-control-emit",
        "packages/vesper-codegen-control-emit",
        "use @vesper/codegen-control-emit::control_hook;\npub fn main() -> u64 eff [Ffi[Cranelift]] { control_hook(0) }\n",
    ),
    (
        "codegen-numeric",
        "@vesper/codegen-numeric",
        "packages/vesper-codegen-numeric",
        "use @vesper/codegen-numeric::numeric_hook;\npub fn main() -> u64 eff [Ffi[Cranelift]] { numeric_hook(0) }\n",
    ),
];

/// Compiler packages Stage 2 cannot build yet: (stem, package, path, probe
/// `src/lib.ves`, reason). `stage2_self_compilation_fixed_point` checks that
/// each one still fails, so a gap that closes moves into [`COMPILER_PACKAGES`].
const STAGE2_PACKAGE_GAPS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "driver-commands",
        "@vesper/driver-commands",
        "packages/vesper-driver-commands",
        "use @vesper/driver-commands::run;\nuse @vesper/driver::args_new;\npub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv], Ffi[StdProcess], Ffi[HybridCommands]] { let a = args_new(); wrapping_u64(run(in a)) }\n",
        "its printing and help go through the HybridCommands import, which only the hybrid compiler's Rust implements (crates/vesper-hybrid/src/commands.rs): the Wasm entry cannot carry Ffi[HybridCommands] (WErr Entry). Formatting calls @vesper/fmt (crates/vesper-fmt/vesper), which hashes with C BLAKE3 (@vesper-std/hash-sys). @vesper-std/process builds for WASI (src-wasip2, #1928)",
    ),
    (
        "run",
        "@vesper/run",
        "packages/vesper-run",
        "use @vesper/run::run_words;\npub fn main() -> u64 { run_words(in \"\".as_bytes()).len() }\n",
        "the `vesper run` evaluator (#2053) divides under local Fail handlers (packages/vesper-run/src/arith.ves), which the Wasm backend does not lower yet (WErr Unsupported GenericEffectInstance)",
    ),
];

/// Directories under `packages/` that are in none of [`COMPILER_PACKAGES`],
/// [`STAGE2_CRANELIFT_PACKAGES`] and [`STAGE2_PACKAGE_GAPS`], with where they
/// are covered instead.
const STAGE2_PACKAGES_COVERED_ELSEWHERE: &[(&str, &str)] = &[
    (
        "vesper-compiler",
        "Stage 1 front end, checked as one unit; bootstrap/stage1.lock (tests/stage2_seed_runner.rs)",
    ),
    (
        "vesper-export-build",
        "`vesper build --lib`'s interface and binding from Stage 2's front end (#2270) as a native executable beside the Rust CLI (tests/self_host_codegen_export.rs); it is @vesper/stage2 plus the export packages, built under Stage 2 as one program in bootstrap switch slice 3",
    ),
    (
        "vesper-lsp-format",
        "the formatting action of `vesper lsp` (#1828 part 11a) in the JIT beside Rust's (tests/self_host_lsp_features.rs); it calls @vesper/fmt, which hashes with C BLAKE3 (@vesper-std/hash-sys), which a Wasm entry does not have",
    ),
    (
        "vesper-proof-audit",
        "`vesper proof-audit` as a native executable beside Rust's (tests/self_host_proof_audit.rs); it hashes with C BLAKE3 (@vesper-std/hash-sys) and runs Lean, neither of which a Wasm entry has",
    ),
    (
        "vesper-stage2",
        "Stage 2 itself: stage2.wasm == stage3.wasm",
    ),
    (
        "vesper-verify",
        "`vesper verify` pieces as a native executable beside Rust's (tests/self_host_verify.rs); it depends on @vesper/proof-audit, which hashes with C BLAKE3 (@vesper-std/hash-sys), which a Wasm entry does not have",
    ),
    (
        "vesper-std",
        "standard library packages, built as dependencies of the probes",
    ),
];

#[test]
fn stage2_fixed_point_lists_every_package() {
    if !runs_on_shard(0) {
        return;
    }
    let mut listed: Vec<String> = COMPILER_PACKAGES
        .iter()
        .map(|(_, _, rel, _, _)| *rel)
        .chain(STAGE2_PACKAGE_GAPS.iter().map(|(_, _, rel, _, _)| *rel))
        .chain(STAGE2_CRANELIFT_PACKAGES.iter().map(|(_, _, rel, _)| *rel))
        .map(|rel| rel.trim_start_matches("packages/").to_string())
        .chain(
            STAGE2_PACKAGES_COVERED_ELSEWHERE
                .iter()
                .map(|(d, _)| d.to_string()),
        )
        .collect();
    let n = listed.len();
    listed.sort();
    listed.dedup();
    assert_eq!(n, listed.len(), "a package is listed twice");
    let mut on_disk: Vec<String> = std::fs::read_dir(repo().join("packages"))
        .expect("read packages/")
        .filter_map(|e| {
            let e = e.ok()?;
            (e.file_type().ok()?.is_dir() && e.path().join("Loom.toml").is_file()
                || e.file_name() == "vesper-std")
                .then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    on_disk.sort();
    assert_eq!(
        listed, on_disk,
        "every directory under packages/ must be in COMPILER_PACKAGES, STAGE2_CRANELIFT_PACKAGES, STAGE2_PACKAGE_GAPS or STAGE2_PACKAGES_COVERED_ELSEWHERE"
    );
}

/// The probe wrapper (Loom.toml, src/lib.ves) for one compiler package. Its
/// dependency path is the package's guest path under `/repo`.
fn compiler_package_probe(
    stem: &str,
    pkg_name: &str,
    pkg_rel: &str,
    expr: &str,
) -> (String, String) {
    let dep_guest = format!("/repo/{pkg_rel}");
    let manifest = format!(
        "[package]\nname = \"@vesper-test/probe-{stem}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"{pkg_name}\" = {{ version = \"^0.1\", path = \"{dep_guest}\" }}\n"
    );
    let fn_name = expr
        .split(&['(', ' '][..])
        .find(|s| !s.is_empty() && *s != "{" && *s != "let" && *s != "_" && *s != "=")
        .unwrap_or("");
    let use_line = if expr.starts_with("widen_u64(") {
        format!("use {pkg_name}::cmd_check;")
    } else if expr.starts_with('{') {
        let inner_fn = expr.split("= ").nth(1).unwrap().split('(').next().unwrap();
        format!("use {pkg_name}::{inner_fn};")
    } else {
        format!("use {pkg_name}::{fn_name};")
    };
    let lib_src = format!("{use_line}\npub fn main() -> u64 {{ {expr} }}\n");
    (manifest, lib_src)
}

/// Runs Stage 2 under the wasmtime CLI with the repository mapped at both
/// `.` and `/repo`, so a probe wrapper under `/out` resolves its compiler
/// package dependency the same way it does in process.
fn run_stage2_probe_wasmtime_cli(cli: &Path, stage2_cwasm: &Path, out_dir: &Path, out_wasm: &Path) {
    remove_stage2_sidecars(out_wasm);
    let out_name = out_wasm
        .file_name()
        .expect("out filename")
        .to_string_lossy()
        .into_owned();
    let exec = std::process::Command::new(cli)
        .args(["run", "--allow-precompiled"])
        .arg("--dir")
        .arg(format!("{}::.", repo().display()))
        .arg("--dir")
        .arg(format!("{}::/repo", repo().display()))
        .arg("--dir")
        .arg(format!("{}::/out", out_dir.display()))
        .args(["--env", "VESPER_STAGE2_SYMS=1"])
        .args(["--argv0", "stage2"])
        .arg(stage2_cwasm)
        .args(["/out/wrap", &format!("/out/{out_name}"), "main"])
        .output()
        .expect("run stage2 under wasmtime CLI");
    let notes = read_stage2_sidecar_notes(out_wasm);
    assert!(
        exec.status.success(),
        "wasmtime CLI stage2 failed (status={}):\nstdout={}\nstderr={}{notes}",
        exec.status,
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr),
    );
}

/// Builds the probe for one compiler package with `compiler` into
/// `<dir>/out.wasm` and returns the component bytes.
fn build_compiler_package_probe(
    compiler: &PreparedComponent,
    syms: &Path,
    dir: &Path,
    label: &str,
) -> Result<Vec<u8>, String> {
    let out_wasm = dir.join("out.wasm");
    remove_stage2_sidecars(&out_wasm);
    let code = compiler
        .run(&Ctx {
            args: vec!["stage2", "/out/wrap", "/out/out.wasm", "main"],
            env: vec![("VESPER_STAGE2_SYMS", "1")],
            dirs: vec![
                (repo(), ".".into()),
                (repo(), "/repo".into()),
                (dir.to_path_buf(), "/out".into()),
            ],
            fuel: Some(u64::MAX),
            diag_base: Some(out_wasm.clone()),
            syms_file: Some(syms.to_path_buf()),
        })
        .map_err(|e| format!("{label} trapped: {e}"))?;
    if code != 0 {
        return Err(format_stage2_error(label, code, Some(&out_wasm)));
    }
    let bytes = std::fs::read(&out_wasm).unwrap_or_default();
    if !bytes.starts_with(b"\0asm\x0d\x00\x01\x00") {
        return Err(format!(
            "{label} wrote no component ({} bytes)",
            bytes.len()
        ));
    }
    Ok(bytes)
}

/// The manifest of a probe given as a whole `src/lib.ves`: the package
/// itself, and each other compiler package the probe `use`s to reach the
/// code it builds.
fn source_probe_manifest(stem: &str, pkg_name: &str, pkg_rel: &str, lib_src: &str) -> String {
    let (mut manifest, _) = compiler_package_probe(stem, pkg_name, pkg_rel, "");
    for other in lib_src
        .split("use @vesper/")
        .skip(1)
        .filter_map(|rest| rest.split("::").next())
        .filter(|other| format!("@vesper/{other}") != *pkg_name)
    {
        manifest.push_str(&format!(
            "\"@vesper/{other}\" = {{ version = \"^0.1\", path = \"/repo/packages/vesper-{other}\" }}\n"
        ));
    }
    manifest
}

fn write_probe(wrap_dir: &Path, manifest: &str, lib_src: &str) {
    let _ = std::fs::remove_dir_all(wrap_dir);
    std::fs::create_dir_all(wrap_dir.join("src")).unwrap();
    std::fs::write(wrap_dir.join("Loom.toml"), manifest).unwrap();
    std::fs::write(wrap_dir.join("src/lib.ves"), lib_src).unwrap();
}

/// The Stage 2 fixed point over every compiler package (#1928, part of
/// #1397).
///
/// `stage2.wasm == stage3.wasm` shows that `@vesper/stage2` rebuilds itself.
/// This extends the fixed point to the packages the compiler is made of:
/// each one in [`COMPILER_PACKAGES`] is built by the boot component (Stage 2
/// compiled by the Rust wasm backend) and by `stage2.wasm` (Stage 2 compiled
/// by itself), and the two components must be byte-identical. The two
/// compilers come from different backends, so equal bytes mean neither
/// backend miscompiled Stage 2 in a way these packages exercise. With the
/// pinned wasmtime CLI (always present under Bazel), the precompiled
/// `stage2.cwasm` rebuilds every package again and must match too.
///
/// Each package in [`STAGE2_CRANELIFT_PACKAGES`] is built the same way. Each
/// package in [`STAGE2_PACKAGE_GAPS`] must still fail to build, so a gap that
/// closes is moved into [`COMPILER_PACKAGES`] instead of going unnoticed.
fn check_compiler_packages_fixed_point(
    dir: &Path,
    boot: &PreparedComponent,
    boot_syms: &Path,
    stage2: &PreparedComponent,
    stage2_syms: &Path,
    cli: Option<(&Path, &Path)>,
) {
    let wrap_dir = dir.join("wrap");
    let out_wasm = dir.join("out.wasm");
    let mut failures = Vec::new();
    for (stem, pkg_name, pkg_rel, expr, _) in COMPILER_PACKAGES.iter() {
        let (manifest, lib_src) = compiler_package_probe(stem, pkg_name, pkg_rel, expr);
        write_probe(&wrap_dir, &manifest, &lib_src);
        let by_boot = match build_compiler_package_probe(boot, boot_syms, dir, "boot stage2") {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{pkg_name}: {e}"));
                continue;
            }
        };
        match build_compiler_package_probe(stage2, stage2_syms, dir, "stage2.wasm") {
            Ok(b) if b == by_boot => {}
            Ok(b) => failures.push(format!(
                "{pkg_name}: boot stage2 built {} bytes, stage2.wasm built {} different bytes",
                by_boot.len(),
                b.len()
            )),
            Err(e) => failures.push(format!("{pkg_name}: {e}")),
        }
        if let Some((cli, stage2_cwasm)) = cli {
            run_stage2_probe_wasmtime_cli(cli, stage2_cwasm, dir, &out_wasm);
            let by_cli = std::fs::read(&out_wasm).unwrap_or_default();
            if by_cli != by_boot {
                failures.push(format!(
                    "{pkg_name}: boot stage2 built {} bytes, stage2.cwasm under the wasmtime CLI built {} different bytes",
                    by_boot.len(),
                    by_cli.len()
                ));
            }
        }
    }
    for (stem, pkg_name, pkg_rel, lib_src) in STAGE2_CRANELIFT_PACKAGES.iter() {
        write_probe(
            &wrap_dir,
            &source_probe_manifest(stem, pkg_name, pkg_rel, lib_src),
            lib_src,
        );
        let by_boot = match build_compiler_package_probe(boot, boot_syms, dir, "boot stage2") {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{pkg_name}: {e}"));
                continue;
            }
        };
        match build_compiler_package_probe(stage2, stage2_syms, dir, "stage2.wasm") {
            Ok(b) if b == by_boot => {}
            Ok(b) => failures.push(format!(
                "{pkg_name}: boot stage2 built {} bytes, stage2.wasm built {} different bytes",
                by_boot.len(),
                b.len()
            )),
            Err(e) => failures.push(format!("{pkg_name}: {e}")),
        }
        if let Some((cli, stage2_cwasm)) = cli {
            run_stage2_probe_wasmtime_cli(cli, stage2_cwasm, dir, &out_wasm);
            let by_cli = std::fs::read(&out_wasm).unwrap_or_default();
            if by_cli != by_boot {
                failures.push(format!(
                    "{pkg_name}: boot stage2 built {} bytes, stage2.cwasm under the wasmtime CLI built {} different bytes",
                    by_boot.len(),
                    by_cli.len()
                ));
            }
        }
    }
    for (stem, pkg_name, pkg_rel, lib_src, why) in STAGE2_PACKAGE_GAPS.iter() {
        write_probe(
            &wrap_dir,
            &source_probe_manifest(stem, pkg_name, pkg_rel, lib_src),
            lib_src,
        );
        match build_compiler_package_probe(stage2, stage2_syms, dir, "stage2.wasm") {
            Ok(_) => failures.push(format!(
                "{pkg_name}: Stage 2 now builds it (listed as a gap: {why}); move it from STAGE2_PACKAGE_GAPS into COMPILER_PACKAGES"
            )),
            // Every gap is in the Wasm backend: loading, assembly and the
            // checks must pass.
            Err(e) if !e.contains("[wasm-backend WErr") => failures.push(format!(
                "{pkg_name}: a gap fails before the Wasm backend (listed as: {why}): {e}"
            )),
            Err(_) => {}
        }
    }
    // @vesper-std/process on WASI (src-wasip2): a start is refused with
    // Unsupported (IoKind 11) and `id` is 0, built the same by both Stage 2
    // compilers.
    let proc_manifest = "[package]\nname = \"@vesper-test/probe-process-wasi\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper-std/process\" = { version = \"^0.1\", path = \"/repo/packages/vesper-std/process\" }\n";
    let proc_src = "use @vesper-std/process::{command, arg, spawn, release, id};\n\
        pub fn main() -> u64 eff [Ffi[StdProcess]] {\n\
            let mut c = command(in \"cc\");\n\
            arg(inout c, in \"--version\");\n\
            let r = match spawn(c) {\n\
                Result::Ok(ch) => {\n\
                    release(ch);\n\
                    1000\n\
                },\n\
                Result::Err(e) => widen_u64(e.kind),\n\
            };\n\
            wrapping_add(r, id())\n\
        }\n";
    write_probe(&wrap_dir, proc_manifest, proc_src);
    match (
        build_compiler_package_probe(boot, boot_syms, dir, "boot stage2"),
        build_compiler_package_probe(stage2, stage2_syms, dir, "stage2.wasm"),
    ) {
        (Ok(a), Ok(b)) => {
            if a != b {
                failures.push(format!(
                    "@vesper-std/process (WASI probe): boot stage2 built {} bytes, stage2.wasm built {} different bytes",
                    a.len(),
                    b.len()
                ));
            }
            match run(&b, &Ctx::default()) {
                Ok(11) => {}
                other => failures.push(format!(
                    "@vesper-std/process (WASI probe): expected Unsupported (11), got {other:?}"
                )),
            }
        }
        (a, b) => failures.push(format!(
            "@vesper-std/process (WASI probe): {:?} / {:?}",
            a.err(),
            b.err()
        )),
    }
    remove_stage2_sidecars(&out_wasm);
    let _ = std::fs::remove_dir_all(&wrap_dir);
    assert!(
        failures.is_empty(),
        "Stage 2 package fixed point broken for {} of {} packages:\n{}",
        failures.len(),
        COMPILER_PACKAGES.len() + STAGE2_CRANELIFT_PACKAGES.len() + STAGE2_PACKAGE_GAPS.len(),
        failures.join("\n")
    );
}

#[test]
fn stage2_compiles_compiler_packages() {
    if !runs_on_shard(1) {
        return;
    }
    with_large_stack(|| {
        let dir = fresh("stage2-compiler-pkgs");
        let stage2_pkg = repo().join("packages/vesper-stage2");
        let assembled =
            vesper::packages::load_for_target(&stage2_pkg, Some("wasm32-wasip2"), false)
                .expect("load @vesper/stage2");
        assert!(assembled.ok(), "{:?}", assembled.diagnostics);
        let checked =
            vesper::ffi_lowering::check_module(&assembled.module, &imports()).expect("check");
        let o = stage2_boot_options();
        let stage2_comp_path = dir.join("stage2_boot.wasm");
        let syms_path = sidecar_path(&stage2_comp_path, ".boot.syms");
        let boot_component = port_with_syms(&checked, &o, &syms_path).expect("boot");
        let boot_prepared =
            PreparedComponent::new(&boot_component).expect("prepare @vesper/stage2 boot");

        let specs = COMPILER_PACKAGES;

        let wrap_dir = dir.join("wrap");
        let host_wasm = dir.join("out.wasm");
        let compile_wrap = |manifest: &str, lib_src: &str| {
            let _ = std::fs::remove_dir_all(&wrap_dir);
            std::fs::create_dir_all(wrap_dir.join("src")).unwrap();
            std::fs::write(wrap_dir.join("Loom.toml"), manifest).unwrap();
            std::fs::write(wrap_dir.join("src/lib.ves"), lib_src).unwrap();
            let _ = std::fs::remove_file(&host_wasm);
            remove_stage2_sidecars(&host_wasm);
            boot_prepared.run(&Ctx {
                args: vec!["stage2", "/out/wrap", "/out/out.wasm", "main"],
                env: vec![("VESPER_STAGE2_SYMS", "1")],
                dirs: vec![
                    (repo(), ".".into()),
                    (repo(), "/repo".into()),
                    (dir.clone(), "/out".into()),
                ],
                fuel: Some(u64::MAX),
                diag_base: Some(host_wasm.clone()),
                syms_file: Some(syms_path.clone()),
            })
        };

        for (stem, pkg_name, pkg_rel, expr, exec_check) in specs.iter() {
            let (manifest, lib_src) = compiler_package_probe(stem, pkg_name, pkg_rel, expr);
            let exit_code = compile_wrap(&manifest, &lib_src)
                .unwrap_or_else(|e| panic!("stage2 trap on {stem}: {e}"));
            let err_txt =
                std::fs::read_to_string(sidecar_path(&host_wasm, ".err.txt")).unwrap_or_default();
            assert_eq!(
                exit_code,
                0,
                "stage2 failed on {stem}: {}",
                read_stage2_sidecar_notes(&host_wasm)
            );
            assert!(
                err_txt.trim().is_empty(),
                "unexpected stage2 error diagnostic on {stem}: {err_txt}"
            );
            let bytes = std::fs::read(&host_wasm).unwrap_or_default();
            assert!(
                bytes.starts_with(b"\0asm\x0d\x00\x01\x00"),
                "expected wasm component header for {stem}, got {} bytes",
                bytes.len()
            );
            match exec_check {
                CompilerExecCheck::CompileOnly => {}
                CompilerExecCheck::Exact(expected) => {
                    let got = run(&bytes, &Ctx::default())
                        .unwrap_or_else(|e| panic!("run {stem} component: {e}"));
                    assert_eq!(got, *expected, "unexpected return value running {stem}");
                }
                CompilerExecCheck::NonZero => {
                    let got = run(&bytes, &Ctx::default())
                        .unwrap_or_else(|e| panic!("run {stem} component: {e}"));
                    assert!(
                        got > 0,
                        "expected positive return running {stem}, got {got}"
                    );
                }
            }
            remove_stage2_sidecars(&host_wasm);
        }

        // Verify Stage 2 lowering & Wasmtime execution for fixed-array writes,
        // on-demand `Result` ADTs, integer intrinsics/conversions, and `usize`/`isize`.
        let smoke_manifest = "[package]\nname = \"@vesper-test/stage2-lowering-smoke\"\nversion = \"0.1.0\"\nedition = \"2026\"\n";
        let smoke_src = "\
fn res_val(flag: bool) -> u64 {
    let r: Result[u64, u64] = if flag {
        Result::Ok(10)
    } else {
        Result::Err(3)
    };
    match r {
        Result::Ok(x) => x,
        Result::Err(e) => e,
    }
}

pub fn main() -> u64 {
    let mut arr: [u32; 4] = [1, 2, 3, 4];
    let idx: u64 = 2;
    arr[1] = 20;
    arr[idx] += 10;
    let (lo, hi) = mul_wide(3, 5);
    let sh = wrapping_shl_u8(1, 3);
    let zero64: i64 = 0;
    let w32: i32 = wrapping_i32(wrapping_sub(zero64, 5));
    let neg_adj: u64 = wrapping_u64(wrapping_add(w32, 7));
    let u: usize = 4;
    let i: isize = 5;
    let us_sum: u64 = wrapping_add(wrapping_u64(u), wrapping_u64(i));
    let arr_sum: u64 = widen_u64(wrapping_add(arr[1], arr[2]));
    wrapping_add(
        wrapping_add(wrapping_add(arr_sum, neg_adj), wrapping_add(widen_u64(sh), us_sum)),
        wrapping_add(wrapping_add(res_val(true), res_val(false)), wrapping_add(lo, hi))
    )
}
";
        let exit_code = compile_wrap(smoke_manifest, smoke_src).expect("compile lowering smoke");
        let note = read_stage2_sidecar_notes(&host_wasm);
        assert_eq!(exit_code, 0, "lowering smoke failed: {note}");
        let smoke_bytes = std::fs::read(&host_wasm).unwrap_or_default();
        assert!(smoke_bytes.starts_with(b"\0asm\x0d\x00\x01\x00"));
        let smoke_got = run(&smoke_bytes, &Ctx::default()).expect("run lowering smoke");
        assert_eq!(smoke_got, 80);
        remove_stage2_sidecars(&host_wasm);

        // Verify WErr formatting surfaces symbol context via `werr_syms`.
        let err_manifest = "[package]\nname = \"@vesper-test/stage2-werr-syms\"\nversion = \"0.1.0\"\nedition = \"2026\"\n";
        let err_src = "pub fn main() -> u64 { unknown_host_fn() }\n";
        let err_code = compile_wrap(err_manifest, err_src).expect("compile werr syms negative");
        assert_ne!(err_code, 0, "expected non-zero exit for unresolved call");
        let err_note = read_stage2_sidecar_notes(&host_wasm);
        assert!(
            err_note.contains("ExternalFunction")
                && err_note.contains("sym=")
                && err_note.contains("unknown_host_fn"),
            "expected werr_syms symbol details in diagnostic note, got:\n{err_note}"
        );
        remove_stage2_sidecars(&host_wasm);

        let _ = std::fs::remove_file(&syms_path);
        std::fs::remove_dir_all(dir).unwrap();
    });
}
