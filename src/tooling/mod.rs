//! Pure source-only tooling shared by CLI and editor adapters.
pub mod doc;
pub mod frontend;
#[cfg(not(target_arch = "wasm32"))]
pub mod lint;
#[cfg(not(target_arch = "wasm32"))]
pub mod recovery;
