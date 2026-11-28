//! The Stage 2 gate's harness: Stage 2 components run in process under
//! Wasmtime with WASI 0.2 (the pinned CLI also runs them), and the bootstrap
//! chain from the committed seed (`bootstrap/stage2-seed/stage2.wasm`).
//!
//! Nothing here links the Rust compiler: the seed is the only compiler the
//! gate starts from.
#![allow(dead_code)]

pub mod packages;

use std::path::{Path, PathBuf};

/// The interface whose `main` a Stage 2 component exports
/// (packages/vesper-wit-component).
pub const ENTRY_INTERFACE: &str = "vesper:program/entry@0.1.0";
/// The preamble of a WebAssembly component (layer 1, version 0x0d).
pub const COMPONENT_HEADER: &[u8] = b"\0asm\x0d\x00\x01\x00";
/// The committed seed, relative to the repository.
pub const SEED: &str = "bootstrap/stage2-seed/stage2.wasm";
/// How a Git LFS pointer file starts (the seed is stored in LFS).
const LFS_POINTER: &[u8] = b"version https://git-lfs";
/// The package Stage 2 is built from, relative to the repository.
pub const STAGE2_PACKAGE: &str = "packages/vesper-stage2";
/// Wasmtime fuel of a run that sets none.
const DEFAULT_FUEL: u64 = 200_000_000;
/// The stack of the thread a Stage 2 build runs on.
const STAGE2_STACK_BYTES: usize = 16 * 1024 * 1024;
const MIB: usize = 1024 * 1024;
const WASM_PAGE_BYTES: usize = 65536;

/// The repository root: the nearest ancestor that holds `packages/` and the
/// seed. Under Bazel `CARGO_MANIFEST_DIR` is `.` (tests start in their
/// package directory in the runfiles tree), so the search starts from its
/// absolute form.
pub fn repo() -> PathBuf {
    let dir = std::path::absolute(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    dir.ancestors()
        .find(|d| d.join("packages").is_dir() && d.join(SEED).is_file())
        .map_or_else(|| dir.clone(), Path::to_path_buf)
}

#[derive(Default)]
pub struct MemLimiter {
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

pub struct WasiHost {
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
pub struct Ctx {
    pub args: Vec<&'static str>,
    pub env: Vec<(&'static str, &'static str)>,
    /// (host directory, guest name)
    pub dirs: Vec<(PathBuf, String)>,
    /// Wasmtime fuel, 200 million by default.
    pub fuel: Option<u64>,
    /// Optional host path prefix matching the guest output `.wasm` path for `.stage`/`.err.txt` diagnostics.
    pub diag_base: Option<PathBuf>,
    /// Optional `<func_idx> <name>` map file for symbolizing `<wasm function N>` traps.
    pub syms_file: Option<PathBuf>,
}

pub fn sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn remove_stage2_sidecars(base: &Path) {
    for suffix in ["", ".err.txt", ".stage", ".syms"] {
        let _ = std::fs::remove_file(sidecar_path(base, suffix));
    }
}

pub fn demangle_vesper_sym(raw: &str) -> String {
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

pub fn load_syms(path: &Path) -> std::collections::HashMap<u32, String> {
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

pub fn symbolize_backtrace(raw: &str, syms: &std::collections::HashMap<u32, String>) -> String {
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

pub fn read_stage2_sidecar_notes(base: &Path) -> String {
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

/// Stage 2's exit statuses (packages/vesper-stage2/src/lib.ves): the CLI
/// argument errors, and `kind * STATUS_KIND_SCALE + detail` otherwise.
const STATUS_ARGUMENT_ERRORS: std::ops::RangeInclusive<u64> = 10..=12;
const STATUS_KIND_SCALE: u64 = 1_000_000;
const STATUS_LOAD: u64 = 1;
const STATUS_ASSEMBLY: u64 = 2;
const STATUS_BACKEND: u64 = 3;
const STATUS_WRITE: u64 = 4;

pub fn format_stage2_error(label: &str, got: u64, base: Option<&Path>) -> String {
    let kind_desc = if STATUS_ARGUMENT_ERRORS.contains(&got) {
        format!("CLI argument error (code={got})")
    } else {
        let code = got / STATUS_KIND_SCALE;
        let val = got % STATUS_KIND_SCALE;
        match code {
            STATUS_LOAD => format!("load_package_graph failed (code={code}, detail={val})"),
            STATUS_ASSEMBLY => {
                format!("name/package assembly failed with {val} diagnostic(s) (code={code})")
            }
            STATUS_BACKEND => format!("wasm-backend WErr failed at site={val} (code={code})"),
            STATUS_WRITE => format!("IoError writing core wasm (code={code}, detail={val})"),
            _ => format!("unexpected stage2 status code={code} val={val}"),
        }
    };
    let notes = base.map(read_stage2_sidecar_notes).unwrap_or_default();
    format!("{label} returned {got} [{kind_desc}]{notes}")
}

pub fn wasmtime_cli_bin() -> Option<PathBuf> {
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

pub fn precompile_stage2_wasmtime_cli(cli: &Path, stage2_wasm: &Path, out_cwasm: &Path) {
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

pub fn run_stage2_wasmtime_cli(
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
pub struct PreparedComponent {
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
        let fuel_budget = ctx.fuel.unwrap_or(DEFAULT_FUEL);
        store.set_fuel(fuel_budget).unwrap();
        let instance = self
            .linker
            .instantiate(&mut store, &self.component)
            .map_err(|e| format!("instantiate: {e:?}"))?;
        let iface = instance
            .get_export_index(&mut store, None, ENTRY_INTERFACE)
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
            let cur_mib = lim.current_bytes / MIB;
            let peak_mib = lim.peak_bytes / MIB;
            let pages = lim.current_bytes / WASM_PAGE_BYTES;
            let max_pages = lim.peak_bytes / WASM_PAGE_BYTES;
            let syms = ctx.syms_file.as_deref().map(load_syms).unwrap_or_default();
            let bt = symbolize_backtrace(&format!("{e:?}"), &syms);
            let fail_grow = match lim.failed_grow {
                Some((cur, des, max)) => format!(
                    ", denied_grow=({} MiB -> {} MiB, max={})",
                    cur / MIB,
                    des / MIB,
                    max.map_or_else(|| "none".into(), |m| format!("{} MiB", m / MIB))
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
pub fn run(component: &[u8], ctx: &Ctx) -> Result<u64, String> {
    PreparedComponent::new(component)?.run(ctx)
}

/// A fresh empty directory for one run.
pub fn fresh(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!(
        "vesper-stage2-gate-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Runs `f` on a thread with Stage 2's stack, propagating its panic.
pub fn with_large_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(STAGE2_STACK_BYTES)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));
}

/// One Stage 2 compiler of the bootstrap chain, prepared to run.
pub struct Compiler {
    pub label: String,
    pub bytes: Vec<u8>,
    pub prepared: PreparedComponent,
    /// Its `<function index> <name>` map, for symbolizing traps (the seed has
    /// none).
    pub syms: Option<PathBuf>,
}

impl Compiler {
    pub fn new(label: &str, bytes: Vec<u8>, syms: Option<PathBuf>) -> Compiler {
        let prepared =
            PreparedComponent::new(&bytes).unwrap_or_else(|e| panic!("prepare {label}: {e}"));
        Compiler {
            label: label.to_string(),
            bytes,
            prepared,
            syms,
        }
    }

    /// Runs Stage 2 with `args` (package, output and nothing else; the entry
    /// is `main`) with the repository at `.` and `/repo` and `dir` at
    /// `/out`; `out` is the host path of the output. Returns the component
    /// it wrote.
    pub fn build(&self, dir: &Path, args: &[&'static str], out: &Path) -> Result<Vec<u8>, String> {
        remove_stage2_sidecars(out);
        let mut argv = vec!["stage2"];
        argv.extend_from_slice(args);
        argv.push("main");
        let code = self
            .prepared
            .run(&Ctx {
                args: argv,
                env: vec![("VESPER_STAGE2_SYMS", "1")],
                dirs: vec![
                    (repo(), ".".into()),
                    (repo(), "/repo".into()),
                    (dir.to_path_buf(), "/out".into()),
                ],
                fuel: Some(u64::MAX),
                diag_base: Some(out.to_path_buf()),
                syms_file: self.syms.clone(),
            })
            .map_err(|e| format!("{} trapped: {e}", self.label))?;
        if code != 0 {
            return Err(format_stage2_error(&self.label, code, Some(out)));
        }
        let bytes = std::fs::read(out).unwrap_or_default();
        if !bytes.starts_with(COMPONENT_HEADER) {
            return Err(format!(
                "{} wrote no component ({} bytes)",
                self.label,
                bytes.len()
            ));
        }
        Ok(bytes)
    }

    /// `@vesper/stage2` built by this compiler into `<dir>/<name>`, as the
    /// next compiler of the chain.
    pub fn next(&self, dir: &Path, name: &'static str, label: &str) -> Compiler {
        let out = dir.join(name);
        let guest: &'static str = Box::leak(format!("/out/{name}").into_boxed_str());
        let bytes = self
            .build(dir, &[STAGE2_PACKAGE, guest], &out)
            .unwrap_or_else(|e| panic!("{} building @vesper/stage2:\n{e}", self.label));
        Compiler::new(label, bytes, Some(sidecar_path(&out, ".syms")))
    }
}

/// The bootstrap chain: the committed seed builds stage A from today's
/// sources. A runs today's Stage 2 but was compiled by the seed's backend,
/// so its bytes equal the seed only while the seed is current. When they
/// differ, A builds B (today's Stage 2 compiled by itself) and B builds C,
/// and B == C is the fixed point. A stale seed is fine as long as it still
/// builds today's sources; `aspect bootstrap stage2 -- refresh-seed`
/// replaces it.
pub struct Chain {
    pub a: Compiler,
    /// `None` when the seed is current (A == seed, so A rebuilds itself).
    pub b: Option<Compiler>,
}

impl Chain {
    /// Stage 2 built by itself: B, or A when the seed is current.
    pub fn last(&self) -> &Compiler {
        self.b.as_ref().unwrap_or(&self.a)
    }
}

pub fn seed() -> Compiler {
    let path = repo().join(SEED);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(LFS_POINTER),
        "{} is a Git LFS pointer: run `git lfs pull` (or `aspect ci fetch-stage2-seed`)",
        path.display()
    );
    assert!(
        bytes.starts_with(COMPONENT_HEADER),
        "{} is not a component",
        path.display()
    );
    Compiler::new("seed", bytes, None)
}

/// Stage A: today's Stage 2 built by the committed seed.
pub fn stage_a(dir: &Path) -> Compiler {
    seed().next(dir, "stage2_a.wasm", "stage A (built by the seed)")
}

/// Builds the chain in `dir`; with a stale seed, checks B == C.
pub fn bootstrap(dir: &Path) -> Chain {
    let seed = seed();
    let a = seed.next(dir, "stage2_a.wasm", "stage A (built by the seed)");
    if a.bytes == seed.bytes {
        return Chain { a, b: None };
    }
    eprintln!(
        "note: the committed seed is stale ({} bytes; today's Stage 2 built by it is {} bytes); refresh it with `aspect bootstrap stage2 -- refresh-seed`",
        seed.bytes.len(),
        a.bytes.len()
    );
    let b = a.next(dir, "stage2_b.wasm", "stage B (built by A)");
    let c = b.next(dir, "stage2_c.wasm", "stage C (built by B)");
    assert!(
        b.bytes == c.bytes,
        "Stage 2 fixed point mismatch: stage B ({} bytes) != stage C ({} bytes)",
        b.bytes.len(),
        c.bytes.len()
    );
    Chain { a, b: Some(b) }
}
