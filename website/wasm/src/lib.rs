//! WebAssembly build of the source checker and evaluator for the language tour.
#![cfg(target_arch = "wasm32")]

include!("../../../src/compiler_modules.rs");
#[path = "../../../src/browser_tour.rs"]
mod browser_tour;
