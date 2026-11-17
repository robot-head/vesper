#!/usr/bin/env python3
"""Regenerates the Tensor reference fixtures in this directory.

These fixtures are the specification side of wave-nine lane 27 (#604): golden
vectors produced by independent implementations, which the Rust crate is
checked against today (tests/reference_fixtures.rs) and which the future
Vesper-source port of the Tensor formats must reproduce. See MANIFEST.md.

Peers (versions recorded in manifest.json):
  * llama.cpp (via QuantFactory/SmolLM-135M-GGUF, Q4_K_S) for Q4_K bytes,
  * gguf-py for Q8_0 / Q4_0 bytes,
  * Hugging Face `tokenizers` and OpenAI `tiktoken` for GPT-2 byte-level BPE,
  * ml_dtypes for FP8 E4M3FN / E5M2 rounding (RNE after saturation).

Usage (offline once the two SmolLM inputs are downloaded, see MANIFEST.md):
  python make_reference.py DOWN11_F32 DOWN11_Q4K
"""
import json
import struct
import sys
from pathlib import Path

import gguf
import ml_dtypes
import numpy as np
import tiktoken
import tokenizers

HERE = Path(__file__).resolve().parent
ROWS = 96  # rows of blk.11.ffn_down kept (of 576)
COLS = 1536
F32 = np.float32


def q4k_fixture(f32_path, q4k_path):
    x = np.fromfile(f32_path, dtype="<f4").reshape(576, COLS)[:ROWS]
    bits = x.view("<u4")
    assert np.all(bits & 0xFFFF == 0), "SmolLM weights are BF16-exact"
    (bits >> 16).astype("<u2").tofile(HERE / "q4k-smollm-blk11-ffn_down.bf16")
    q = np.fromfile(q4k_path, dtype=np.uint8)[: ROWS * (COLS // 256) * 144]
    q.tofile(HERE / "q4k-smollm-blk11-ffn_down.q4k")
    return x


# --- GGML generic CPU kernels, transliterated from ggml-cpu/quants.c --------
def nearest_int(v):
    b = np.array([F32(v) + F32(12582912.0)], dtype=F32).view(np.int32)[0]
    return int((b & 0x007FFFFF) - 0x00400000)


def quantize_q8_k(x):
    """quantize_row_q8_K_ref: returns (d, qs[256], bsums[16]) per block."""
    out = []
    for blk in x.reshape(-1, 256):
        amax, mx = F32(0), F32(0)
        for v in blk:
            if abs(v) > amax:
                amax, mx = abs(v), v
        if amax == 0:
            out.append((F32(0), np.zeros(256, np.int8), np.zeros(16, np.int16)))
            continue
        iscale = F32(-127.0) / mx
        qs = np.array([min(127, nearest_int(iscale * v)) for v in blk], np.int8)
        bsums = qs.reshape(16, 16).astype(np.int32).sum(1).astype(np.int16)
        out.append((F32(1) / iscale, qs, bsums))
    return out


def q8_k_bytes(blocks):
    b = bytearray()
    for d, qs, bsums in blocks:
        b += struct.pack("<f", d) + qs.tobytes() + bsums.astype("<i2").tobytes()
    return bytes(b)


def half(b, at):
    return F32(np.frombuffer(b[at : at + 2], dtype="<f2")[0])


def scale_min_k4(j, s):
    if j < 4:
        return s[j] & 63, s[j + 4] & 63
    return (s[j + 4] & 0x0F) | ((s[j - 4] >> 6) << 4), (s[j + 4] >> 4) | ((s[j] >> 6) << 4)


def dot_q4_k_q8_k(row, ys):
    sums = [F32(0)] * 8
    sumf = F32(0)
    for i, (yd, yq, bs) in enumerate(ys):
        blk = row[i * 144 : (i + 1) * 144]
        d, dmin, sc, qs = half(blk, 0), half(blk, 2), blk[4:16], blk[16:]
        a = []
        for j in range(4):
            q = qs[32 * j : 32 * j + 32]
            a += [v & 0xF for v in q] + [v >> 4 for v in q]
        scales, mins = zip(*(scale_min_k4(j, sc) for j in range(8)))
        sumi = sum(int(bs[j]) * mins[j // 2] for j in range(16))
        aux32 = [0] * 8
        for j in range(8):
            for k in range(32):
                aux32[k % 8] += scales[j] * int(yq[32 * j + k]) * a[32 * j + k]
        dd = d * yd
        for l in range(8):
            sums[l] = sums[l] + dd * F32(aux32[l])
        sumf = sumf - (dmin * yd) * F32(sumi)
    for l in range(8):
        sumf = sumf + sums[l]
    return sumf


def dot_q8_0_q8_0(row, y):
    sumf = F32(0)
    for ib in range(len(row) // 34):
        xb, yb = row[ib * 34 : ib * 34 + 34], y[ib * 34 : ib * 34 + 34]
        sumi = int(np.dot(np.frombuffer(xb[2:], np.int8).astype(np.int32),
                          np.frombuffer(yb[2:], np.int8).astype(np.int32)))
        sumf = sumf + F32(sumi) * (half(xb, 0) * half(yb, 0))
    return sumf


def dot_q4_0_q8_0(row, y):
    sumf = F32(0)
    for ib in range(len(row) // 18):
        xb, yb = row[ib * 18 : ib * 18 + 18], y[ib * 34 : ib * 34 + 34]
        yq = np.frombuffer(yb[2:], np.int8).astype(np.int32)
        sumi = sum(((xb[2 + j] & 0xF) - 8) * yq[j] + ((xb[2 + j] >> 4) - 8) * yq[j + 16]
                   for j in range(16))
        sumf = sumf + F32(sumi) * half(xb, 0) * half(yb, 0)
    return sumf


def q8_fixture(w):
    rng = np.random.default_rng(604)
    acts = rng.standard_normal((4, COLS)).astype(F32)
    acts[1] *= F32(1e-3)
    acts[2, ::7] = 0
    acts[3, :256] = 0  # an all-zero Q8_K block
    acts.astype("<f4").tofile(HERE / "q8-activations.f32")
    q4k = (HERE / "q4k-smollm-blk11-ffn_down.q4k").read_bytes()
    q8_0w = gguf.quants.quantize(w, gguf.GGMLQuantizationType.Q8_0).tobytes()
    q4_0w = gguf.quants.quantize(w, gguf.GGMLQuantizationType.Q4_0).tobytes()
    (HERE / "q8-weights.q8_0").write_bytes(q8_0w)
    (HERE / "q8-weights.q4_0").write_bytes(q4_0w)
    q8k_all, q8_0_all, expect = b"", b"", {"q4_k_q8_k": [], "q8_0_q8_0": [], "q4_0_q8_0": []}
    rk, r8, r4 = 6 * 144, 48 * 34, 48 * 18
    for a in acts:
        yk = quantize_q8_k(a)
        y0 = gguf.quants.quantize(a[None, :], gguf.GGMLQuantizationType.Q8_0).tobytes()
        q8k_all += q8_k_bytes(yk)
        q8_0_all += y0
        expect["q4_k_q8_k"].append([float(dot_q4_k_q8_k(q4k[r * rk:(r + 1) * rk], yk)).hex() for r in range(ROWS)])
        expect["q8_0_q8_0"].append([float(dot_q8_0_q8_0(q8_0w[r * r8:(r + 1) * r8], y0)).hex() for r in range(ROWS)])
        expect["q4_0_q8_0"].append([float(dot_q4_0_q8_0(q4_0w[r * r4:(r + 1) * r4], y0)).hex() for r in range(ROWS)])
    (HERE / "q8-activations.q8_k").write_bytes(q8k_all)
    (HERE / "q8-activations.q8_0").write_bytes(q8_0_all)
    for name, rows in expect.items():
        np.array([[float.fromhex(v) for v in r] for r in rows], "<f4").tofile(HERE / f"q8-dots-{name}.f32")
    return expect


# --- FP8 scaling ------------------------------------------------------------
FP8 = {"e4m3": (ml_dtypes.float8_e4m3fn, 448.0, 8), "e5m2": (ml_dtypes.float8_e5m2, 57344.0, 15)}


def encode(v, kind):
    dt, mx, _ = FP8[kind]
    return np.clip(v.astype(F32), -mx, mx).astype(dt).view(np.uint8)


def fp8_fixture():
    rng = np.random.default_rng(28)
    x = (rng.standard_normal((256, 256)) * rng.uniform(0.01, 10, (256, 1))).astype(F32)
    x[3] = 0  # all-zero channel
    x[5, 17] = 3e4  # outlier
    x[7, :] = rng.standard_normal(256).astype(F32) * F32(1e-30)  # tiny channel
    x.astype("<f4").tofile(HERE / "fp8-input.f32")
    meta = {}
    for kind, (dt, mx, emax) in FP8.items():
        # Per channel (row): scale = amax_r / max_finite, 1 for a zero row.
        amax = np.abs(x).max(1)
        s = np.where(amax > 0, amax / F32(mx), F32(1)).astype(F32)
        np.asarray(s, "<f4").tofile(HERE / f"fp8-channel-{kind}.scales")
        encode(x / s[:, None], kind).tofile(HERE / f"fp8-channel-{kind}.codes")
        # Per 128x128 block (DeepSeek-V3 `weight_scale_inv` layout).
        bs = np.zeros((2, 2), F32)
        codes = np.zeros_like(x, np.uint8)
        for bi in range(2):
            for bj in range(2):
                blk = x[bi * 128:(bi + 1) * 128, bj * 128:(bj + 1) * 128]
                a = np.abs(blk).max()
                bs[bi, bj] = a / F32(mx) if a > 0 else F32(1)
                codes[bi * 128:(bi + 1) * 128, bj * 128:(bj + 1) * 128] = encode(blk / bs[bi, bj], kind)
        bs.astype("<f4").tofile(HERE / f"fp8-block128-{kind}.scales")
        codes.tofile(HERE / f"fp8-block128-{kind}.codes")
        # MXFP8 (OCP MX v1.0 §6.3): 32-element blocks along a row, E8M0 shared
        # exponent floor(log2(amax)) - emax_elem clamped to [-127, 127]
        # (code 0, i.e. 2^-127, for an all-zero block); elements are
        # saturating RNE casts of x / 2^shared_exp.
        blocks = x.reshape(-1, 32)
        e8 = np.zeros(len(blocks), np.uint8)
        el = np.zeros(blocks.shape, np.uint8)
        for i, b in enumerate(blocks):
            a = np.abs(b).max()
            e = -127 if a == 0 else int(np.clip(np.floor(np.log2(np.float64(a))) - emax, -127, 127))
            e8[i] = e + 127
            el[i] = encode((b.astype(np.float64) / 2.0 ** e).astype(F32), kind)
        e8.tofile(HERE / f"mxfp8-{kind}.e8m0")
        el.tofile(HERE / f"mxfp8-{kind}.codes")
        meta[kind] = {"max_finite": mx, "emax_elem": emax}
    return meta


def gpt2_fixture():
    tok = tokenizers.Tokenizer.from_file(str(HERE / "gpt2-tokenizer.json"))
    tk = tiktoken.get_encoding("gpt2")
    cases = []
    lines = (HERE / "corpus.txt").read_text("utf-8").split("\n")
    texts = lines + [(HERE / "corpus.txt").read_text("utf-8"),
                     (HERE / "corpus-whitespace.txt").read_bytes().decode("utf-8")]
    for t in texts:
        ids = tok.encode(t).ids
        assert ids == tk.encode(t, allowed_special="all"), t
        assert tok.decode(ids, skip_special_tokens=False) == t and tk.decode(ids) == t, t
        cases.append({"text": t, "ids": ids, "ordinary_ids": tk.encode_ordinary(t)})
    # Binary twin of the cases for consumers without a JSON reader:
    # u32 count, then per case u32 text bytes, UTF-8 text, u32 n, n x u32 ids.
    b = bytearray(struct.pack("<I", len(cases)))
    for c in cases:
        t = c["text"].encode("utf-8")
        b += struct.pack("<I", len(t)) + t + struct.pack("<I", len(c["ids"]))
        b += struct.pack(f"<{len(c['ids'])}I", *c["ids"])
    (HERE / "gpt2-cases.bin").write_bytes(bytes(b))
    return cases


def main():
    w = q4k_fixture(sys.argv[1], sys.argv[2])
    dots = q8_fixture(w)
    fp8 = fp8_fixture()
    gpt2 = gpt2_fixture()
    manifest = {
        "peers": {
            "llama.cpp": "QuantFactory/SmolLM-135M-GGUF SmolLM-135M.Q4_K_S.gguf (blk.11.ffn_down, Q4_K)",
            "source_weights": "HuggingFaceTB/SmolLM-135M model.safetensors model.layers.11.mlp.down_proj.weight (F32, Apache-2.0)",
            "gguf": gguf.__version__ if hasattr(gguf, "__version__") else "0.19.0",
            "tokenizers": tokenizers.__version__,
            "tiktoken": tiktoken.__version__,
            "ml_dtypes": ml_dtypes.__version__,
            "numpy": np.__version__,
        },
        "q4k": {"rows": ROWS, "cols": COLS},
        "q8_dots": {"rows": ROWS, "cols": COLS, "activations": 4, "expected_f32_hex": dots},
        "fp8": {"rows": 256, "cols": 256, "block": 128, "mx_block": 32, "formats": fp8},
        "gpt2": gpt2,
    }
    (HERE / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=1) + "\n", "utf-8")


if __name__ == "__main__":
    main()
