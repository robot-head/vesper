//! The Stage 2 fixed-point gate (#1397, #2671). Everything is in `tests/`:
//! the committed seed (`bootstrap/stage2-seed/stage2.wasm`) builds today's
//! `@vesper/stage2` under Wasmtime, which must rebuild itself and every
//! compiler package to the same bytes. The crate has no dependency on the
//! Rust compiler, so the gate outlives it.
