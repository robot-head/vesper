//! The lowering differential of the SH4 codegen swap point (self-hosting
//! SH4 lane 1, #1275): one world compiled by the native backend with and
//! without a set of `vesper::codegen_swap` hooks, compared per function
//! (CLIF text, `ctx.func.display`), as object bytes and as JIT runs with
//! the optimizer on and off.
//!
//! Hooks written in Vesper run through the shim's lent handles
//! ([`vesper_hook`]): the hook is a JIT-compiled `fn(call: u64) -> u64`
//! (`ForeignChecked::word_function`) that gets the address of the lent
//! call. Port lanes 6-17 use this harness unchanged: they install their
//! family's hook and read the counts.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::check::World;
use vesper::codegen_swap::{CodegenHooks, Lent};

/// One build: the CLIF text of each emitted function by instance key, and
/// the object bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub clif: BTreeMap<String, String>,
    pub object: Vec<u8>,
}

/// Compiles `entry` of `world` to a host object (`vesper build`'s native
/// path) and splits its CLIF text per function.
pub fn build(world: &World, entry: &str, optimize: bool) -> Result<Build, String> {
    let (object, clif) = vesper::native::object_with_options(world, entry, None, optimize)?;
    Ok(Build {
        clif: split_clif(&clif),
        object,
    })
}

/// The `; <key>` sections of the native backend's CLIF text: a key line
/// is `; ` followed by the instance key, right before a `function` line.
pub fn split_clif(text: &str) -> BTreeMap<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeMap::new();
    let mut key: Option<String> = None;
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(k) = line.strip_prefix("; ")
            && lines.get(i + 1).is_some_and(|n| n.starts_with("function "))
        {
            if let Some(k) = key.take() {
                out.insert(k, std::mem::take(&mut body));
            }
            key = Some(k.to_string());
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if let Some(k) = key {
        out.insert(k, body);
    }
    out
}

/// What one comparison found.
#[derive(Debug, Default, Clone)]
pub struct Diff {
    /// Worlds compiled on both sides.
    pub worlds: usize,
    /// Functions compared.
    pub functions: usize,
    /// `(world, function)` pairs whose CLIF text differs, with the first
    /// differing line of each side.
    pub clif: Vec<(String, String, String)>,
    /// Worlds whose object bytes differ.
    pub objects: Vec<String>,
    /// Worlds whose JIT runs differ: `(world, optimize, rust, hooked)`.
    pub runs: Vec<(String, bool, String, String)>,
    /// Worlds that compiled on one side only: `(world, rust, hooked)`.
    pub failures: Vec<(String, String, String)>,
    /// JIT runs compared.
    pub ran: usize,
}

impl Diff {
    pub fn is_clean(&self) -> bool {
        self.clif.is_empty()
            && self.objects.is_empty()
            && self.runs.is_empty()
            && self.failures.is_empty()
    }
    pub fn add(&mut self, other: Diff) {
        self.worlds += other.worlds;
        self.functions += other.functions;
        self.ran += other.ran;
        self.clif.extend(other.clif);
        self.objects.extend(other.objects);
        self.runs.extend(other.runs);
        self.failures.extend(other.failures);
    }
    /// A short report: counts, then the first few differences.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} worlds, {} functions, {} runs: {} CLIF, {} object, {} run, {} one-sided differences",
            self.worlds,
            self.functions,
            self.ran,
            self.clif.len(),
            self.objects.len(),
            self.runs.len(),
            self.failures.len()
        );
        for (w, f, line) in self.clif.iter().take(5) {
            s.push_str(&format!("\n  CLIF {w} {f}: {line}"));
        }
        for w in self.objects.iter().take(5) {
            s.push_str(&format!("\n  object {w}"));
        }
        for (w, o, r, h) in self.runs.iter().take(5) {
            s.push_str(&format!("\n  run {w} (optimize {o}): {r} vs {h}"));
        }
        for (w, r, h) in self.failures.iter().take(5) {
            s.push_str(&format!("\n  compile {w}: {r} vs {h}"));
        }
        s
    }
}

/// The first line where `a` and `b` differ, both sides.
pub fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}: `{x}` vs `{y}`", i + 1);
        }
    }
    format!("{} vs {} lines", a.lines().count(), b.lines().count())
}

/// Compares `world` built with no hook against the same build with
/// `hooks` installed on this thread, for `optimize`. With `run`, also
/// JIT-runs `entry` both ways and compares the printed result.
pub fn compare(
    name: &str,
    world: &World,
    entry: &str,
    hooks: CodegenHooks,
    optimize: bool,
    run: bool,
) -> Diff {
    let mut d = Diff::default();
    let rust = build(world, entry, optimize);
    let hooked = vesper::codegen_swap::with_hooks(hooks, || build(world, entry, optimize));
    match (rust, hooked) {
        (Ok(r), Ok(h)) => {
            d.worlds = 1;
            d.functions = r.clif.len();
            for (key, text) in &r.clif {
                match h.clif.get(key) {
                    Some(t) if t == text => {}
                    Some(t) => d
                        .clif
                        .push((name.into(), key.clone(), first_difference(text, t))),
                    None => d.clif.push((name.into(), key.clone(), "missing".into())),
                }
            }
            for key in h.clif.keys().filter(|k| !r.clif.contains_key(*k)) {
                d.clif.push((name.into(), key.clone(), "extra".into()));
            }
            if r.object != h.object {
                d.objects.push(name.into());
            }
        }
        (Err(_), Err(_)) => return d,
        (r, h) => {
            let show = |x: Result<Build, String>| match x {
                Ok(_) => "ok".to_string(),
                Err(e) => e,
            };
            d.failures.push((name.into(), show(r), show(h)));
            return d;
        }
    }
    if run {
        let rust = vesper::native::jit_with_options(world, entry, optimize).map(|v| v.to_string());
        let hooked = vesper::codegen_swap::with_hooks(hooks, || {
            vesper::native::jit_with_options(world, entry, optimize).map(|v| v.to_string())
        });
        d.ran = 1;
        if rust != hooked {
            let show = |x: Result<String, String>| x.unwrap_or_else(|e| format!("error: {e}"));
            d.runs
                .push((name.into(), optimize, show(rust), show(hooked)));
        }
    }
    d
}

// ------------------------------------------------------------ Vesper hooks

/// Requests a Vesper hook answered, and requests it declined (all hooks of
/// the process, so a test can show its hook really ran).
pub static HANDLED: AtomicU64 = AtomicU64::new(0);
pub static DECLINED: AtomicU64 = AtomicU64::new(0);

/// Runs one request through the Vesper hook `f` (a `fn(call: u64) -> u64`
/// word function) over the shim's lent handles.
pub fn vesper_hook(
    lent: &mut Lent<'_, '_>,
    f: &vesper::native::WordFunction,
) -> Result<bool, String> {
    let (answer, crossing) = vesper_cranelift_sys::lend(
        lent.builder,
        &mut *lent.module,
        lent.op,
        lent.args,
        // SAFETY: `f` takes one word, the lent call's address, which is
        // live for the whole call.
        |call| unsafe { f.call(&[call]) },
    );
    let answer = answer?;
    if let Some(e) = crossing.error {
        return Err(format!("codegen hook (op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!("codegen hook (op {}) kept a lent handle", lent.op));
    }
    if answer == 1 {
        HANDLED.fetch_add(1, Ordering::Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        DECLINED.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }
}

/// The host's `.vsys` target and ABI names (`packages::vsys_target`'s).
pub fn host_target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

/// Loads the Loom package at `dir` (repository-relative), with the `.vsys`
/// libraries its graph carries, checked for the host.
pub fn foreign_package(repo: &std::path::Path, dir: &str) -> vesper::ffi_lowering::ForeignChecked {
    let p = vesper::packages::load(&repo.join(dir)).unwrap_or_else(|e| panic!("{dir}: {e:?}"));
    assert!(p.ok(), "{dir}: {:?} {:?}", p.diagnostics, p.manifest_errors);
    let imports = p
        .imports(host_target())
        .unwrap_or_else(|e| panic!("{dir}: {e:?}"));
    vesper::ffi_lowering::check_module(&p.module, &imports).unwrap_or_else(|e| panic!("{dir}: {e}"))
}

/// JIT-compiles hook `entry` of `checked` with the shim's symbols bound.
pub fn hook_function(
    checked: &vesper::ffi_lowering::ForeignChecked,
    entry: &str,
) -> vesper::native::WordFunction {
    let symbols = vesper_cranelift_sys::symbols();
    checked
        .word_function(entry, &symbols, true)
        .unwrap_or_else(|e| panic!("{entry}: {e}"))
}
