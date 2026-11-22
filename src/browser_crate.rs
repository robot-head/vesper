//! WebAssembly build of the source checker and evaluator for the language tour.
#![cfg(target_arch = "wasm32")]

include!("compiler_modules.rs");
mod browser_tour;
