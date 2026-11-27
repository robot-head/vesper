//! `componentize CORE OUT`: wraps the core module Bazel links for
//! `wasm32-wasip2` into the component (#2210).
//!
//! Bazel links Wasm with plain `rust-lld`, which writes a core module;
//! Cargo links with `wasm-component-ld`, which also runs wit-component over
//! the `component-type` sections. This is that last step. The module
//! imports WASI 0.2 directly, so no preview 1 adapter is needed.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, core, out] = args.as_slice() else {
        eprintln!("usage: componentize CORE OUT");
        return ExitCode::FAILURE;
    };
    let module = match std::fs::read(core) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("componentize: read {core}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let component = wit_component::ComponentEncoder::default()
        .module(&module)
        .and_then(|encoder| encoder.validate(true).encode());
    match component {
        Ok(bytes) => match std::fs::write(out, bytes) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("componentize: write {out}: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("componentize: {core}: {e:?}");
            ExitCode::FAILURE
        }
    }
}
