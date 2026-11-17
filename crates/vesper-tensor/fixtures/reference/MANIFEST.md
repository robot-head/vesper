# Tensor reference fixtures (spec side of #604)

These files are the acceptance vectors for the Tensor number formats and the
GPT-2 tokenizer. Every expected value was produced by an independent
implementation, never by vesper-tensor. `tests/reference_fixtures.rs` checks
the Rust crate against them today. The future Vesper-source port of the
Tensor formats must reproduce the same bytes and bits before the Rust code is
deleted. `manifest.json` records the peers, their versions, shapes, and the
GPT-2 cases in readable form. `make_reference.py` regenerates everything.

All multi-byte values are little-endian. Matrices are row-major.

| Files | Contents | Produced by |
| --- | --- | --- |
| `q4k-smollm-blk11-ffn_down.bf16` | Rows 0..96 of SmolLM-135M `model.layers.11.mlp.down_proj.weight`, `[96, 1536]`. The source tensor is F32 but every value is BF16-exact, so the upper halves are stored. | HuggingFaceTB/SmolLM-135M (Apache-2.0) |
| `q4k-smollm-blk11-ffn_down.q4k` | The same rows as GGML Q4_K: 576 super-blocks of 144 bytes. | llama.cpp, via `QuantFactory/SmolLM-135M-GGUF` `SmolLM-135M.Q4_K_S.gguf`, tensor `blk.11.ffn_down.weight` |
| `q8-weights.q8_0`, `q8-weights.q4_0` | The same rows as Q8_0 (34-byte blocks) and Q4_0 (18-byte blocks). | gguf-py 0.19.0 `quants.quantize` |
| `q8-activations.f32` | 4 activation vectors of 1536: normal, scaled by 1e-3, every 7th zero, and the first 256 zero. | numpy seed 604 |
| `q8-activations.q8_k` | Each vector as GGML Q8_K: 6 blocks of `f32 d, 256 x i8, 16 x i16 bsums` (292 bytes). | `quantize_row_q8_K_ref`, transliterated in `make_reference.py` |
| `q8-activations.q8_0` | Each vector as Q8_0. | gguf-py 0.19.0 |
| `q8-dots-{q4_k_q8_k,q8_0_q8_0,q4_0_q8_0}.f32` | `[4, 96]` FP32 dot products, activation-major. | `ggml_vec_dot_*_generic` from ggml-cpu/quants.c, transliterated with numpy FP32 scalars |
| `fp8-input.f32` | `[256, 256]` FP32 with per-row magnitudes from 0.01 to 10, an all-zero row 3, an outlier `x[5,17] = 3e4`, and a row 7 of about 1e-30. | numpy seed 28 |
| `fp8-channel-{e4m3,e5m2}.{scales,codes}` | Per-row scale `amax / max_finite` (1 for a zero row) as f32, and the codes `fp8(clip(x / scale))`. | ml_dtypes 0.7.0 (round to nearest even after saturation) |
| `fp8-block128-{e4m3,e5m2}.{scales,codes}` | The same with one scale per 128x128 tile (a `[2, 2]` f32 grid, the DeepSeek-V3 `weight_scale_inv` layout). | ml_dtypes 0.7.0 |
| `mxfp8-{e4m3,e5m2}.{e8m0,codes}` | OCP MX v1.0 MXFP8 over 32-element row blocks: the E8M0 shared exponent `floor(log2(amax)) - emax_elem` (8 for E4M3, 15 for E5M2), clamped to -127..127 and biased by 127 (code 0 for a zero block), then the codes `fp8(clip(x / 2^e))`. Scales and elements are stored separately, so no block layout is implied. | ml_dtypes 0.7.0 |
| `gpt2-tokenizer.json` | The GPT-2 `tokenizer.json` (byte-level BPE, 50,257 tokens, MIT). | openai-community/gpt2 |
| `corpus.txt`, `corpus-whitespace.txt` | 41 cases: each line of `corpus.txt`, the whole file, and the whitespace file. They cover contractions, whitespace runs, digits, code, URLs, Latin with diacritics, Greek, Cyrillic, Hebrew, Arabic, Devanagari, CJK, Hangul, emoji with ZWJ and skin tones, combining marks, CR and CRLF, and `<|endoftext|>` inside text. | hand-written, plus 2000 bytes of `models/stories260K-greedy-200.txt` |
| `gpt2-cases.bin` | The cases as `u32 count`, then per case `u32 len, UTF-8 text, u32 n, n x u32 ids`. | HF `tokenizers` 0.23.3, asserted equal to `tiktoken` 0.14.0 `gpt2` with `allowed_special="all"`. Both decode back to the text. |

`manifest.json` also has `ordinary_ids`, which are tiktoken's `encode_ordinary`
results. There `<|endoftext|>` in text is plain text, not token 50256.

## What a port must show

1. **Q4_K**: `quantize(bf16 rows)` equals `.q4k` byte for byte. This needs
   llama.cpp's `make_qkx2_quants` with FP32 arithmetic in C source order and
   no fused multiply-add. The Rust crate now does this
   (`formats::quantize_q4_k`, all 576 super-blocks here and all 3,456 of the
   full tensor).
2. **Q8_0 / Q4_0 weights**: equal gguf-py's bytes.
3. **Q8 activations**: Q8_K and Q8_0 activation bytes equal, and every dot
   product's FP32 bits equal GGML's *generic* kernels. GGML's AVX2 and NEON
   kernels reorder the FP32 sums, so they are not the reference. A SIMD port
   must keep integer sums exact and the per-block FP32 order shown in the test.
4. **FP8 scaling**: scales and codes equal the files for all three rules and
   both element formats.
5. **GPT-2 BPE**: `encode(text)` equals `ids` for every case in
   `gpt2-cases.bin`, with `tokenizer.json` as the only input. That means the
   GPT-2 regex pre-tokenizer, the byte-to-unicode map, merges by rank, and
   added tokens matched before pre-tokenization.

## Regenerating

```sh
U=https://huggingface.co/HuggingFaceTB/SmolLM-135M/resolve/main/model.safetensors
G=https://huggingface.co/QuantFactory/SmolLM-135M-GGUF/resolve/main/SmolLM-135M.Q4_K_S.gguf
# tensor data offsets: safetensors header is 30368 bytes (+8); GGUF data base 1785120
curl -L -r $((30376+155729664))-$((30376+159268607)) -o down11.f32 $U
curl -L -r $((1785120+37416960))-$((1785120+37416960+497663)) -o down11.q4k $G
python -m venv venv && venv/bin/pip install gguf==0.19.0 tokenizers==0.23.3 tiktoken==0.14.0 ml_dtypes==0.7.0 numpy
venv/bin/python -I make_reference.py down11.f32 down11.q4k   # tiktoken fetches gpt2 BPE once
```
