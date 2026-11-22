# vesper-tensor

Bounded host tensor planning, the paged K/V cache, the decoder kernels and
greedy CPU inference for small Llama-architecture models (manuscript §28).
The crate documentation (`src/lib.rs`) describes the modules.

**Frozen for the Vesper port** (port wave P1 lane 8, claim
[#905](https://github.com/robot-head/vesper/issues/905)): the Rust code
takes bug fixes only. Two packages build it:

- `vesper-tensor-seed` (`seed/`) is the Rust build. The compiler links it,
  and so does every other dependent until the port reaches performance
  parity.
- `vesper-tensor` is the port-backed build: the element and block formats,
  the quantizers, the row decoder of `Matrix`, sampling and the FP32 kernels
  forward to the Loom package `@vesper/tensor` in `vesper/` (`src/port.rs`).

**Vesper-only modules** (Rust-exit wave SH6 lane 11,
[#1406](https://github.com/robot-head/vesper/issues/1406)): the GGUF header
reader, the BPE prompt encoder, the llama2.c checkpoint and tokenizer
readers (`Config::validate`, `Model::from_llama2c`,
`Tokenizer::from_llama2c`) and the safetensors JSON header reader run
within 1.5x of the Rust build, so `src/` has no Rust implementation of
them: the package `vesper-tensor` runs only the port, and their frozen Rust
lives in `seed/src/engine/`, which only the seed builds (the compiler links
the seed until self-hosting Stage 2 retires it).

`port/test-port.sh` runs every test against both packages and checks that a
deliberately divergent port fails, so the seed's Rust engine is the
differential for the Vesper-only modules. The lane reports are
`docs/parallel-results/port-p1-tensor-formats.md` and
`docs/parallel-results/rust-exit-serde-tensor-alloc.md`.
