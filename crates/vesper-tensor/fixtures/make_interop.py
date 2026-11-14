"""Regenerates the interop fixtures in this directory with independent writers.

Run once, offline (not part of the test suite):

    python3 -m venv /tmp/ref && /tmp/ref/bin/pip install gguf==0.19.0 \
        safetensors==0.8.0 numpy==2.5.4 ml_dtypes==0.6.0
    /tmp/ref/bin/python3 -I crates/vesper-tensor/fixtures/make_interop.py

Outputs (all written by the reference libraries, never by vesper-tensor):

* interop.gguf: tensors in every GGML type vesper-tensor computes with,
  quantized by gguf-py (llama.cpp's Python package) where it has a quantizer
  (Q8_0, Q4_0, MXFP4, BF16, F16); Q4_K bytes are pseudo-random valid blocks
  because gguf-py only dequantizes Q4_K.
* interop-expected.safetensors (safetensors 0.8.0 + ml_dtypes): gguf-py's
  dequantization of every interop.gguf tensor, the FP32 sources of the
  quantized tensors, and FP8 E4M3/E5M2 tables (all 256 codes decoded by
  ml_dtypes, plus ml_dtypes round-to-nearest-even encodings of sample values).
* stories260K-q8_0.gguf: the bundled llama2.c stories260K checkpoint and
  tok512 vocabulary converted by gguf-py's GGUFWriter, matrices whose width is
  a multiple of 32 as gguf-py Q8_0 and the rest (w2, norms, embedding) FP32.
"""
import os
import struct

import gguf
import ml_dtypes
import numpy as np
from gguf.quants import dequantize, quantize
from safetensors.numpy import save_file

HERE = os.path.dirname(os.path.abspath(__file__))
MODELS = os.path.join(HERE, "..", "models")
Q = gguf.GGMLQuantizationType
rng = np.random.default_rng(20261114)


def interop():
    w = gguf.GGUFWriter(os.path.join(HERE, "interop.gguf"), arch="interop")
    w.add_string("interop.string", "héllo ▁world")
    w.add_uint32("interop.u32", 4_000_000_000)
    w.add_int32("interop.i32", -7)
    w.add_float32("interop.f32", 0.15625)
    w.add_bool("interop.bool", True)
    w.add_uint64("interop.u64", 1 << 40)
    w.add_array("interop.strings", ["a", "▁b", "<0x0A>"])
    w.add_array("interop.ints", [1, 2, 3])
    expected = {}
    tensors = {}
    tensors["f32"] = (rng.standard_normal((3, 5)).astype(np.float32), Q.F32)
    tensors["vec"] = (rng.standard_normal(7).astype(np.float32), Q.F32)
    tensors["f16"] = (rng.standard_normal((2, 8)).astype(np.float32), Q.F16)
    tensors["bf16"] = (rng.standard_normal((2, 8)).astype(np.float32), Q.BF16)
    tensors["q8_0"] = (rng.standard_normal((2, 64)).astype(np.float32) * 3, Q.Q8_0)
    tensors["q4_0"] = (rng.standard_normal((2, 64)).astype(np.float32), Q.Q4_0)
    mx = rng.standard_normal((3, 64)).astype(np.float32) * 5
    mx[2, :32] = 0.0  # an all-zero block (e = 0)
    tensors["mxfp4"] = (mx, Q.MXFP4)
    for name, (data, qtype) in tensors.items():
        if qtype == Q.F32:
            w.add_tensor(name, data)
            expected[f"dequant.{name}"] = data
        elif qtype == Q.F16:
            w.add_tensor(name, data.astype(np.float16))
            expected[f"dequant.{name}"] = data.astype(np.float16).astype(np.float32)
        else:
            q = quantize(data, qtype)
            w.add_tensor(name, q, raw_dtype=qtype)
            expected[f"source.{name}"] = data
            expected[f"bytes.{name}"] = q.reshape(-1)
            expected[f"dequant.{name}"] = dequantize(q, qtype).astype(np.float32)
    # Q4_K: valid random blocks (finite binary16 d and dmin).
    blocks = []
    for _ in range(2):
        d = np.float16(abs(rng.standard_normal()) * 0.01).tobytes()
        dmin = np.float16(abs(rng.standard_normal()) * 0.01).tobytes()
        blocks.append(d + dmin + rng.integers(0, 256, 12 + 128, dtype=np.uint8).tobytes())
    q4k = np.frombuffer(b"".join(blocks), dtype=np.uint8).reshape(2, 144)
    w.add_tensor("q4_k", q4k, raw_dtype=Q.Q4_K)
    expected["dequant.q4_k"] = dequantize(q4k, Q.Q4_K).astype(np.float32)
    w.write_header_to_file()
    w.write_kv_data_to_file()
    w.write_tensors_to_file()
    w.close()

    codes = np.arange(256, dtype=np.uint8)
    samples = np.concatenate(
        [
            rng.standard_normal(2000).astype(np.float32) * 50,
            rng.standard_normal(500).astype(np.float32) * 0.01,
            np.float32([0.0, -0.0, 1.0, 448.0, 0.001953125, 0.0009765625, 0.0029296875, 57344.0]),
        ]
    )
    for name, dt in (("e4m3", ml_dtypes.float8_e4m3fn), ("e5m2", ml_dtypes.float8_e5m2)):
        expected[f"fp8.{name}.decoded"] = codes.view(dt).astype(np.float32)
        limit = 448.0 if name == "e4m3" else 57344.0
        inside = samples[np.abs(samples) <= limit]
        expected[f"fp8.{name}.inputs"] = inside
        expected[f"fp8.{name}.encoded"] = inside.astype(dt).view(np.uint8)
        expected[f"fp8.{name}.as_dtype"] = inside[:16].astype(dt)
    save_file(expected, os.path.join(HERE, "interop-expected.safetensors"))


def stories():
    raw = open(os.path.join(MODELS, "stories260K.bin"), "rb").read()
    dim, hidden, layers, heads, kv_heads, vocab, seq_len = struct.unpack("<7i", raw[:28])
    shared = vocab > 0
    vocab = abs(vocab)
    hd = dim // heads
    kv_dim = kv_heads * hd
    f = np.frombuffer(raw, dtype=np.float32, offset=28)
    at = 0

    def take(*shape):
        nonlocal at
        n = int(np.prod(shape))
        out = f[at:at + n].reshape(shape)
        at += n
        return out

    emb = take(vocab, dim)
    rms_att = take(layers, dim)
    wq = take(layers, dim, dim)
    wk = take(layers, kv_dim, dim)
    wv = take(layers, kv_dim, dim)
    wo = take(layers, dim, dim)
    rms_ffn = take(layers, dim)
    w1 = take(layers, hidden, dim)
    w2 = take(layers, dim, hidden)
    w3 = take(layers, hidden, dim)
    rms_final = take(dim)
    take(seq_len, hd)  # legacy RoPE tables
    assert shared and at == len(f)

    tok = open(os.path.join(MODELS, "tok512.bin"), "rb").read()
    (max_len,) = struct.unpack("<i", tok[:4])
    pos = 4
    pieces, scores = [], []
    for _ in range(vocab):
        score, n = struct.unpack("<fi", tok[pos:pos + 8])
        pos += 8
        pieces.append(tok[pos:pos + n].decode("utf-8").replace(" ", "▁"))
        pos += n
        scores.append(score)

    w = gguf.GGUFWriter(os.path.join(HERE, "stories260K-q8_0.gguf"), arch="llama")
    w.add_context_length(seq_len)
    w.add_embedding_length(dim)
    w.add_feed_forward_length(hidden)
    w.add_block_count(layers)
    w.add_head_count(heads)
    w.add_head_count_kv(kv_heads)
    w.add_rope_dimension_count(hd)
    w.add_layer_norm_rms_eps(1e-5)
    w.add_rope_freq_base(10000.0)
    w.add_tokenizer_model("llama")
    w.add_token_list(pieces)
    w.add_token_scores(scores)
    w.add_bos_token_id(1)
    w.add_eos_token_id(2)

    def mat(name, m):
        if m.shape[1] % 32 == 0:
            w.add_tensor(name, quantize(np.ascontiguousarray(m), Q.Q8_0), raw_dtype=Q.Q8_0)
        else:
            w.add_tensor(name, np.ascontiguousarray(m))

    w.add_tensor("token_embd.weight", np.ascontiguousarray(emb))
    for i in range(layers):
        w.add_tensor(f"blk.{i}.attn_norm.weight", np.ascontiguousarray(rms_att[i]))
        mat(f"blk.{i}.attn_q.weight", wq[i])
        mat(f"blk.{i}.attn_k.weight", wk[i])
        mat(f"blk.{i}.attn_v.weight", wv[i])
        mat(f"blk.{i}.attn_output.weight", wo[i])
        w.add_tensor(f"blk.{i}.ffn_norm.weight", np.ascontiguousarray(rms_ffn[i]))
        mat(f"blk.{i}.ffn_gate.weight", w1[i])
        mat(f"blk.{i}.ffn_down.weight", w2[i])
        mat(f"blk.{i}.ffn_up.weight", w3[i])
    w.add_tensor("output_norm.weight", np.ascontiguousarray(rms_final))
    w.write_header_to_file()
    w.write_kv_data_to_file()
    w.write_tensors_to_file()
    w.close()


interop()
stories()
