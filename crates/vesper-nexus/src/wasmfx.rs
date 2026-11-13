//! WasmFX (the WebAssembly stack-switching proposal) probe and one lowered
//! effect handler (§30.1 item 1).
//!
//! Wasmtime 50 implements `cont.new`/`resume`/`suspend` behind
//! `Config::wasm_stack_switching` with the `stack-switching` Cargo feature,
//! on Cranelift for x86_64 Linux and macOS only; `Config` validation rejects
//! it elsewhere. [`probe`] reports which case this host is in, with the exact
//! configuration tried and Wasmtime's error text. Nothing in this crate lowers
//! Vesper source to WasmFX: [`YIELD_SUM_WAT`] is a hand-lowered example of a
//! one-shot `Yield` effect and its handler, showing the shape such a lowering
//! targets.

use wasmtime::{Config, Engine, Instance, Module, Store};

/// The configuration [`probe`] tries, for reports.
pub const PROBE_CONFIG: &str = "wasmtime 50.0.1, Strategy::Cranelift, \
wasm_function_references(true), wasm_exceptions(true), wasm_stack_switching(true)";

/// A one-shot effect lowered to WasmFX.
///
/// Source shape (Vesper):
///
/// ```text
/// effect Yield { fn yield_(v: i32) -> Unit }
/// fn producer() -> i32 eff [Yield] { Yield::yield_(1); Yield::yield_(2); 3 }
/// handle producer() with { yield_(v, k) => { acc += v; resume k } }  // returns acc + 3
/// ```
///
/// Lowering: the effect operation is a control tag `$yield`, `producer`
/// becomes a continuation built with `cont.new`, each `Yield::yield_` is a
/// `suspend $yield`, and the handler is a `resume` loop with an
/// `(on $yield $label)` clause. Each continuation is resumed at most once
/// (one-shot), matching Vesper's lexical one-shot effect discipline.
pub const YIELD_SUM_WAT: &str = r#"
(module
  (type $ft (func (result i32)))
  (type $ct (cont $ft))
  (tag $yield (param i32))
  (func $producer (result i32)
    (suspend $yield (i32.const 1))
    (suspend $yield (i32.const 2))
    (i32.const 3))
  (elem declare func $producer)
  (func (export "handle_sum") (result i32)
    (local $k (ref null $ct))
    (local $acc i32)
    (local.set $k (cont.new $ct (ref.func $producer)))
    (loop $again
      (block $on_yield (result i32 (ref $ct))
        (resume $ct (on $yield $on_yield) (local.get $k))
        ;; producer returned normally: add its result and finish
        (return (i32.add (local.get $acc))))
      ;; suspended: stack is [value, continuation]
      (local.set $k)
      (local.set $acc (i32.add (local.get $acc)))
      (br $again))
    (unreachable))
)
"#;

/// Result of probing WasmFX support on this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WasmFxSupport {
    /// The engine accepted stack switching and the example ran; `sum` is its
    /// result (expected 6).
    Exercised { sum: i32 },
    /// Wasmtime rejected the configuration, module or run on this host.
    Unavailable {
        config: &'static str,
        reason: String,
    },
}

/// Try to run [`YIELD_SUM_WAT`].
pub fn probe() -> WasmFxSupport {
    let unavailable = |stage: &str, e: wasmtime::Error| WasmFxSupport::Unavailable {
        config: PROBE_CONFIG,
        reason: format!("{stage}: {e:?}"),
    };
    let mut c = Config::new();
    c.wasm_function_references(true);
    c.wasm_exceptions(true);
    c.wasm_stack_switching(true);
    let engine = match Engine::new(&c) {
        Ok(e) => e,
        Err(e) => return unavailable("engine", e),
    };
    let module = match Module::new(&engine, YIELD_SUM_WAT) {
        Ok(m) => m,
        Err(e) => return unavailable("compile", e),
    };
    let mut store = Store::new(&engine, ());
    let instance = match Instance::new(&mut store, &module, &[]) {
        Ok(i) => i,
        Err(e) => return unavailable("instantiate", e),
    };
    let f = match instance.get_typed_func::<(), i32>(&mut store, "handle_sum") {
        Ok(f) => f,
        Err(e) => return unavailable("export", e),
    };
    match f.call(&mut store, ()) {
        Ok(sum) => WasmFxSupport::Exercised { sum },
        Err(e) => unavailable("run", e),
    }
}
