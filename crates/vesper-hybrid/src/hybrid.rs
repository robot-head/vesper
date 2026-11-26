//! The swap-in glue: each swapped module's Vesper binding, wrapped as the
//! module's Rust function table and installed before `main` runs.
//!
//! Installing from a static constructor keeps the included CLI's `main`
//! untouched (the same std start-up, the same CLI thread). The table only
//! stores function pointers, so it needs nothing from std yet.
//!
//! Each swap lives in its own file under `src/swap/` (#1897): its binding,
//! its word encoders, its counters, its tables or hooks, its flip-gate rows
//! (`SWAPS`), `register` (installs it, or fills its fields of the shared
//! [`Registry`]) and `report`. build.rs lists that directory into
//! `$OUT_DIR/swap_registry.rs` ([`SWAP_FILES`]), so a new swap is a new
//! file and edits no list here. This file keeps only what several swaps
//! share. Recipe: docs/parallel-results/self-host-hybrid-build.md.
//!
//! Set `VESPER_HYBRID_STATS=1` to print, at exit, how many calls each
//! swapped module answered from Vesper (the differential harness uses it to
//! show the pilot really ran) and what each checker swap's input and output
//! cost (`stats`; `VESPER_HYBRID_STATS=time` adds the Rust pass), and
//! which swaps were on.
//!
//! Set `VESPER_HYBRID_SWAPS=gated` to install only the swaps whose flip gate
//! passed, or `none` for none (`swaps`).
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

// Which swaps `install` installs: all, the flip-gated ones, or none
// (`VESPER_HYBRID_SWAPS`, SH6 lane 2, #1397).
#[path = "swaps.rs"]
mod swaps;

use swaps::{Swap, on, open, pass};

// The checker swaps' input encoder and result decoders (#1177), shared with
// the root package's tests/hybrid_layout.rs and tests/hybrid_ir_check.rs.
#[path = "../../../tests/common/check_words.rs"]
mod check_words;

// Place IR words both ways (#1240), shared with tests/self_host_place_ir*.rs.
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

/// The swaps' input and output costs (SH4 lane 2, #1276), reported at exit
/// with `VESPER_HYBRID_STATS=1`. `VESPER_HYBRID_STATS=time` also runs the
/// Rust pass on every swapped call, times it and counts the calls whose
/// answers differ, so one hybrid run gives each swap's encode, Vesper and
/// decode time beside the Rust pass it replaces.
mod stats {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Instant;

    /// 0: off, 1: costs, 2: costs and the Rust pass.
    pub fn level() -> u8 {
        static LEVEL: OnceLock<u8> = OnceLock::new();
        *LEVEL.get_or_init(|| match std::env::var("VESPER_HYBRID_STATS").as_deref() {
            Ok("1") => 1,
            Ok("time") => 2,
            _ => 0,
        })
    }

    pub struct Cost {
        pub name: &'static str,
        pub calls: AtomicU64,
        pub scoped: AtomicU64,
        pub reused: AtomicU64,
        /// Calls that crossed into the shared world (SH5 lane 10).
        pub shared: AtomicU64,
        pub encode: AtomicU64,
        pub vesper: AtomicU64,
        pub decode: AtomicU64,
        pub rust: AtomicU64,
        pub differ: AtomicU64,
    }

    impl Cost {
        pub const fn new(name: &'static str) -> Self {
            Self {
                name,
                calls: AtomicU64::new(0),
                scoped: AtomicU64::new(0),
                reused: AtomicU64::new(0),
                shared: AtomicU64::new(0),
                encode: AtomicU64::new(0),
                vesper: AtomicU64::new(0),
                decode: AtomicU64::new(0),
                rust: AtomicU64::new(0),
                differ: AtomicU64::new(0),
            }
        }

        /// Counts one swapped call; at level 2 also times the Rust pass
        /// (`rust`) and counts the call if `same` says its answer differs
        /// from the swap's.
        pub fn call<T>(
            &self,
            scoped: bool,
            rust: impl FnOnce() -> T,
            same: impl FnOnce(&T) -> bool,
        ) {
            if level() == 0 {
                return;
            }
            self.calls.fetch_add(1, Relaxed);
            self.scoped.fetch_add(scoped as u64, Relaxed);
            if level() == 2 {
                let t = Instant::now();
                let answer = std::hint::black_box(rust());
                self.rust.fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
                self.differ.fetch_add(!same(&answer) as u64, Relaxed);
            }
        }

        pub fn report(&self) {
            let ms = |a: &AtomicU64| a.load(Relaxed) as f64 / 1e6;
            eprintln!(
                "vesper-hybrid: cost {}: {} calls ({} in a compilation scope), encode {:.3} ms, \
                 vesper {:.3} ms, decode {:.3} ms, rust {:.3} ms, {} differ, {} reused, \
                 {} shared",
                self.name,
                self.calls.load(Relaxed),
                self.scoped.load(Relaxed),
                ms(&self.encode),
                ms(&self.vesper),
                ms(&self.decode),
                ms(&self.rust),
                self.differ.load(Relaxed),
                self.reused.load(Relaxed),
                self.shared.load(Relaxed),
            );
        }
    }

    /// A stopwatch that only reads the clock when stats are on.
    #[derive(Clone, Copy)]
    pub struct Timer(Option<Instant>);

    impl Timer {
        pub fn start() -> Self {
            Self((level() > 0).then(Instant::now))
        }

        /// Adds the time since the last lap to `into`.
        pub fn lap(self, into: &AtomicU64) -> Self {
            match self.0 {
                Some(t) => {
                    let now = Instant::now();
                    into.fetch_add((now - t).as_nanos() as u64, Relaxed);
                    Self(Some(now))
                }
                None => self,
            }
        }
    }
}

/// One request of a `lend_as` codegen hook (`numeric`, `seq`, `control`,
/// `ffi`, `drops`), counted in `calls` or `declined`.
fn lend_codegen_op(
    lent: &mut vesper::codegen_swap::Lent<'_, '_>,
    family: &str,
    entry: fn(u64) -> u64,
    calls: &AtomicU64,
    declined: &AtomicU64,
) -> Result<bool, String> {
    if lent.op == vesper::codegen_swap::INSTRUCTION {
        return Ok(false);
    }
    let kind = if lent.jit {
        vesper_cranelift_sys::ModuleKind::Jit
    } else {
        vesper_cranelift_sys::ModuleKind::Object
    };
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind,
        lent.builder,
        &mut *lent.module,
        lent.op,
        lent.args,
        entry,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen ({family} op {}): {e}", lent.op));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen ({family} op {}) kept a lent handle",
            lent.op
        ));
    }
    if answer == 1 {
        calls.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        declined.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The tables several swap files fill in, installed once after every
/// file's `register`: the hooks of `vesper::codegen_swap` and the native IR
/// passes of `vesper::ir_swap` (`None` runs the Rust pass).
#[derive(Default)]
struct Registry {
    codegen: vesper::codegen_swap::CodegenHooks,
    ir: vesper::ir_swap::IrPasses,
}

/// One file of `src/swap/`, as build.rs lists it.
struct SwapFile {
    /// The file's name, without `.rs`.
    #[allow(dead_code)]
    name: &'static str,
    /// Its flip-gate rows (`swaps::on` reads every file's).
    swaps: &'static [Swap],
    /// Installs its swaps, or fills its fields of the [`Registry`].
    register: fn(&mut Registry),
    /// Its `VESPER_HYBRID_STATS` lines.
    report: fn(),
}

// `mod <file>` for every file of src/swap/ and `SWAP_FILES`, in file-name
// order (build.rs `swap_registry`).
include!(concat!(env!("OUT_DIR"), "/swap_registry.rs"));

/// The Vesper driver's own parse (#1893) reads back through the parse swap.
pub(crate) use parse::{driver_parse, driver_parse_begin};

/// The Vesper driver's `run` (#2053) evaluates through the run swap.
pub(crate) use run::driver_run;

// The Vesper driver's wasm and eBPF builds (#1895) take those swaps' input
// words.
pub(crate) use ebpf_backend::{DRIVER_EBPF_FALLBACKS, DRIVER_EBPF_INPUTS, ebpf_build_input};
pub(crate) use wasm_backend::{DRIVER_WASM_FALLBACKS, DRIVER_WASM_INPUTS, wasm_build_input};

/// Stops the process when a slot was already taken: nothing runs before
/// the constructor, so the slots are empty.
fn must<T, E>(installed: Result<T, E>) {
    if installed.is_err() {
        std::process::abort();
    }
}

/// Whether the opt-out `var` is `0` (`VESPER_HYBRID_SEQ=0` and the rest).
fn env_off(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| v == "0")
}

extern "C" fn install() {
    // Read the set first: an unknown value stops here, before any swap.
    swaps::set();
    // Swaps keep one input per compilation (SH4 lane 2, #1276).
    // `VESPER_HYBRID_SCOPES=0` encodes per call instead (the before side of
    // the report's measurement).
    if !env_off("VESPER_HYBRID_SCOPES") {
        vesper::swap_input::enable();
    }
    let mut registry = Registry::default();
    for file in SWAP_FILES {
        (file.register)(&mut registry);
    }
    must(vesper::codegen_swap::install(registry.codegen));
    must(vesper::ir_swap::install(registry.ir));
    if std::env::var_os("VESPER_HYBRID_STATS").is_some_and(|v| v == "1" || v == "time") {
        unsafe extern "C" {
            fn atexit(f: extern "C" fn()) -> std::ffi::c_int;
        }
        // SAFETY: `report` is a plain `extern "C" fn` that only formats
        // and writes to stderr.
        unsafe { atexit(report) };
    }
}

extern "C" fn report() {
    for file in SWAP_FILES {
        (file.report)();
    }
    eprintln!(
        "vesper-hybrid: the driver ran {} passes from Vesper",
        crate::driver::PASSES.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: the command layers answered {} commands from Vesper",
        crate::commands::ANSWERS.load(Relaxed)
    );
    swaps::report();
}

// Run `install` before `main` (the platform's static-constructor section).
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_vendor = "apple",
    windows
)))]
compile_error!(
    "vesper-hybrid installs its modules from a static constructor; add this target's section"
);

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
static INSTALL: extern "C" fn() = install;
