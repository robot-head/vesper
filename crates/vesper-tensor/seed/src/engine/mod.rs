//! The Rust implementations the Vesper port replaced in the package
//! vesper-tensor, frozen for the compiler's closure (bug fixes only until
//! self-hosting Stage 2; SH6 lane 11, #1406).

pub(crate) mod gguf;
pub(crate) mod llama;
pub(crate) mod tokenizer;
