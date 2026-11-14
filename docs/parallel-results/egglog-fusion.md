# Wave eight lane 11: egglog operator fusion for Tensor graphs and Resonance DSP chains (§28, §32, §7 Stage 4)

Claim issue #460. Design coverage: §28.1 item 4 and §28.5 (Stage 4 operator
fusion of Tensor dataflow graphs: epilogues, elementwise chains, the fused
RMSNorm + quantized matvec), §32.1 item 4 (fusing gain/pan/biquad chains in
registers), §7 Stage 4 (egglog equality saturation with a cost model and
extraction).

- Base: `1094e64` (origin/main after plan PR #477; a descendant of the
  required `ea1ed29`).
- Branch: `claude/project-thread-qhu89k`.
- Paths touched: new `crates/vesper-fusion/**`, new `lean/FusionRules.lean`,
  this report, and the root registration patch in its own commit (root
  `Cargo.toml` members and default-members before `experiments/native-pulse`,
  the regenerated `Cargo.lock` entry, and `crates/vesper-fusion/BUILD.bazel`
  from `tools/bazel/sync_build_files.py` plus the Lean test data).
- Dependency: `egglog = "=3.0.0"` (MIT), `default-features = false`. It was
  already in `Cargo.lock` through the root package's `egglog = "3"`, so the
  lockfile gains only the `vesper-fusion` entry. egglog 3.0.0 declares no
  `rust-version`; it builds and passes Clippy on Rust 1.97 and 1.100.
  `vesper-tensor` and `vesper-resonance` are used only through APIs already
  on main (`quant::Matrix`, `ops::rms_norm`, `ops::silu`,
  `pdc::compile_schedule`).

## What was built

### Engine (`src/engine.rs`)

A rule set is plain egglog text: sort and constructor declarations whose
`:cost` annotations are the cost model, then named `rewrite`s. The input
graph is emitted as `let` bindings (shared subgraphs stay shared), run for at
most 24 iterations, and the root is extracted with egglog's tree-cost
extractor. The extracted `TermDag` is converted iteratively (no recursion on
untrusted depth) into a hash-consed arena, capped at 65,536 nodes; the
printable form is budgeted so a shared DAG cannot blow up when printed.

### Tensor rules (`src/tensor.rs`)

`TensorGraph` builds a checked DAG: named FP32 or `i32` inputs, declared
weight matrices (FP32, Q8_0, Q4_0), `matvec`, `add`, `mul`, `scale_f32`,
`scale_i32`, `activation` (ReLU, SiLU) and `rms_norm`. Handles carry an owner
tag, so a node from another graph is `InvalidNode`. `evaluate` is the
unfused reference: every node writes a whole vector, products go through
`vesper_tensor::quant::Matrix::matvec`, RMSNorm through
`vesper_tensor::ops::rms_norm`, and a quantized matrix is first dequantized
into a full FP32 matrix. `fuse` saturates and extracts; `FusedTensor::evaluate`
runs the term, where `Fuse` is one pass computing its lane body per output
element and `Dot` is a row dot product inside that pass.

| Rule | Rewrite | Numerics |
| --- | --- | --- |
| `ew-act`, `ew-add`, `ew-mul`, `ew-scale` | elementwise node ⇒ one-op `Fuse` kernel | bit-identical |
| `ew-inline` | `(Lane (Fuse e)) ⇒ e` (chains collapse) | bit-identical |
| `matvec-epilogue` | `(Lane (MatVec m x)) ⇒ (Dot m x)` (matmul + bias + activation is one pass) | bit-identical |
| `dequant-into-matmul` | `(Deq q) ⇒ (QView q)` (dequantize each row in the consuming kernel) | bit-identical |
| `rmsnorm-prologue` | `(MatVec m (RmsNorm x w e)) ⇒ (RmsMatVec m x w e)` | reassociates; `rmsnorm_tolerance` |

Cost model: a node that writes a whole intermediate vector costs 16, a
materialized dequantized matrix 64, work inside a fused lane 1 or 2, and the
fused RMSNorm matvec 14.

### Resonance rules (`src/dsp.rs`)

`DspGraph` builds stereo chains: named sources, `gain`, `pan`
(constant-power, position in `[-1, 1]`), `biquad` (normalized TDF-II, checked
finite and inside the stability triangle, so both poles are strictly inside
the unit circle; the check also applies when the public fields are filled
directly) and `mix`. `evaluate` is one whole-block pass per stage;
`FusedDsp::evaluate` turns each chain of unary stages above a source or mix
into one loop that carries each sample through every stage.
`DspGraph::schedule` and `FusedDsp::schedule` lower both forms to
`vesper_resonance::pdc::compile_schedule`.

| Rule | Rewrite | Numerics |
| --- | --- | --- |
| `gain-stereo`, `pan-stereo` | gain or pan ⇒ per-channel `Stereo` stage | bit-identical |
| `biquad-cascade`, `cascade-merge`, `cascade-append-nil`, `cascade-append-cons` | sections join one per-sample `Cascade` | bit-identical |
| `stereo-compose` | two gain stages ⇒ one with product gains | reassociates; `gain_tolerance` |
| `mix-sends` | `(Mix (Stereo a b s) (Stereo c d s))` ⇒ one stage with summed gains | reassociates; `gain_tolerance` |
| `gain-into-biquad`, `gain-before-biquad` | a uniform gain scales `b0, b1, b2` of the first section | zero initial state; `filter_tolerance` |

### Lean (`lean/FusionRules.lean`)

Model `vesper.fusion.rules-model/1`, 19 theorems, one per rule
(`vesper_fusion::RULE_LEMMAS` maps each rule name to its lemma) plus
`cascade_fused_eq_seq`. Lean core only (`Rat`, `grind`), no `sorry`,
`admit`, `native_decide` or axioms; the axiom report lists only `propext`,
`Classical.choice` and `Quot.sound`. It compiles in about 1.5 s.

- Rules that keep each lane's operation sequence are proved over an arbitrary
  carrier with arbitrary operations and no algebraic laws (tensor terms and
  lane expressions as a mutual inductive with a lane-wise semantics; biquad
  sections as an arbitrary state machine). They therefore also describe IEEE
  evaluation order, which is why the Rust tests demand bit equality for them.
  `cascade_fused_eq_seq` is the loop interchange: one per-sample loop over all
  sections equals the sequence of whole-block section passes.
- `rmsnorm_prologue_refines`, `stereo_compose_refines` and `mix_sends_refines`
  are proved over `Rat`. `gain_into_biquad_refines` and
  `gain_before_biquad_refines` are proved over `Rat` for TDF-II sections from
  zero state, via `biquad_scaled_input` (a gain on the input scales output
  and state), `biquad_scaled_coef` (scaled feed-forward coefficients do the
  same) and `gain_commutes_cascade`.

What is proved is about these models. It is not a proof about the Rust code,
about egglog's saturation, congruence closure or extraction, or about `f32`.
The dequantization lemma is definitional (a consumer that dequantizes in place
reads the same `scale * code` entries as one that reads a materialized
matrix). The semantics are compositional, but no lemma quantifies over
sequences of rewrites at arbitrary positions.

## Acceptance

Fused and unfused graphs are compared on deterministic random inputs
(xorshift seeds in the tests):

- Matmul + bias + ReLU/SiLU (FP32), the same with Q8_0 and Q4_0 weights,
  a six-node elementwise chain with a shared (diamond) operand, a SwiGLU feed
  forward with a residual over Q8_0 weights, a 300-node chain, and checked
  `i32` chains: **bit-identical**, and integer overflow is `Error::Overflow` on
  both sides. Kernel counts: 3 → 1 (matmul epilogue), 6 → 1 (chain), 6 → 2
  (SwiGLU: the gated hidden vector, then `W2` with the residual epilogue).
  No fused term materializes a dequantized matrix.
- RMSNorm prologue (FP32 and Q4_0, 96 columns, 40 rows): every row within
  `rmsnorm_tolerance` = `4 (n + 4) u · inv · Σ|W||w||x|`, `u = 2^-24`.
  Both sides are within `(n + 3) u` of the exact sum (recursive summation),
  with slack for the rounded `inv`. Observed worst error / tolerance: 0.013
  (FP32) and 0.009 (Q4_0).
- Biquad cascades (4 random stable sections, 1024 samples, 20 seeds):
  **bit-identical**, 4 passes → 1.
- Gain → pan → gain: within `gain_tolerance` (`2 k u M` per sample), 3 → 1.
  Parallel sends: within `gain_tolerance`, 3 → 1.
- Gain folded into a two-section cascade, gain before and after the cascade,
  4096 samples, pole radius at most 0.99: within `filter_tolerance`
  (`1e-4 · max(1, peak)`). This is an engineering bound for that regime, not
  a derived one: rounding differences enter the recursive state and are
  amplified by the filter's noise gain.
- Two channel strips (two biquads, trim, pan each), a mix and a bus gain:
  10 passes → 4; the Resonance PDC schedule goes from 12 ops to 6 with the
  same buffer count (3), since its linear-scan reuse already recycles a
  chain's buffers.
- Typed errors (never panics): duplicate or invalid names (including a quote
  that would break the egglog program), zero lengths, mismatched types and
  lengths, SiLU over `i32`, non-finite parameters and inputs, invalid
  epsilon, quantized matrices without whole 32-element blocks, matrices that
  differ from their declaration, foreign handles, missing inputs, unstable
  biquads, pan positions outside `[-1, 1]`, mismatched mix lengths.

Measured (debug build, this cloud host): fusing the 300-node chain takes
about 0.7 s, mostly saturation. No kernel performance is claimed; pass
counts and egglog costs are what the tests check.

## Checks

```sh
cargo fmt --all -- --check                                           # clean
cargo test -p vesper-fusion --locked                                 # 17 passed
VESPER_LEAN=.../lean-4.24.0-linux/bin/lean cargo test -p vesper-fusion --test lean_model   # 3 passed, Lean compiled
cargo clippy -p vesper-fusion --all-targets --locked -- -D warnings          # clean (1.97)
cargo +1.100.0 clippy -p vesper-fusion --all-targets --locked -- -D warnings # clean (1.100)
lean lean/FusionRules.lean                                           # 19 axiom lines, standard axioms only
python3 tools/bazel/sync_build_files.py                              # BUILD.bazel created, then edited for //:lean
```

The workspace-wide `cargo test --workspace --locked` and Clippy run, and
`bazel test //...`, are left to CI on the PR; see the PR for their results.

## Decisions

- Host library only. It does not edit `src/optimize*`, `src/ir_opt.rs` or the
  rewrite-witness files (Codex owns them), and nothing reads Vesper source.
  Rust ownership here is not evidence that Vesper's source typing rules are
  implemented.
- Fusion is expressed in the e-graph as a `Fuse` kernel whose body is a lane
  expression, so epilogue fusion, chain fusion and dequantize-into-matmul
  compose instead of needing one rule per pattern (matmul + bias + activation
  is `matvec-epilogue` plus the elementwise rules).
- Gains, pan gains and section coefficients stay symbolic in the e-graph
  (`GMul`, `GAdd`, `CScale` over parameter indices) and are evaluated in
  `f32` when the term is lowered, so egglog never does float arithmetic.
- Exact rules use the same scalar functions in both evaluators; the only
  differences between the two are which pass computes a value and where it
  is stored. Reassociating rules are tested against tolerances and proved
  only over `Rat`.
- `dsp::Biquad` keeps public fields for inspection, but every graph entry
  point re-validates it.

## Remaining gaps

- Extraction is egglog's tree cost, which does not price sharing: a fused
  subexpression with two consumers is computed once per consumer (correct,
  but possibly more work). A DAG-aware extractor is future work.
- The fused evaluators are scalar reference executors, not SIMD or GPU code;
  §28's cooperative-matrix lowering and §32's TiledSeq slab colouring of the
  fused buffers are not done here.
- No RoPE, softmax or attention rules, and no integer-dot path for quantized
  activations (the integer path is checked `i32` elementwise arithmetic).
- `gain-*-biquad` folding needs zero initial state; a stateful streaming
  executor that keeps section state across blocks would need the scaled-state
  form (`biquad_scaled_input` covers it in the model, the Rust code does not
  expose it).
- `RmsMatVec` rounds differently from the unfused graph, so for inputs near
  `f32` overflow one side can report `NonFinite` where the other does not.
- The Lean lemmas are per rule at the root of a term; egglog's congruence
  closure and the extraction are trusted, not verified.
