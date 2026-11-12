# Expansion 37: Tensor host planning and execution

## Scope and document review

Read `docs/rfcs/expansion-37-contracts.md` and the complete refreshed specification
sections 26–29 (`/tmp/vesper-spec-sections/{26,27,28,29}.txt`). This lane adds only
`crates/vesper-tensor/**` and this report. Existing compiler, SIMD, allocation,
FFI, Forge, Ignis, root manifests, lock, workflows and CLI were read-only.

The refreshed §28 calls for symbolic named-axis typing, quantized weight mapping,
fusion, device kernels, and linear paged-KV inference. The implemented slice is
an independent Rust host library: bounded dimension planning and concrete,
shape-checked vector IR with actual scalar FP32/Q4_0 execution. It is deliberately
usable without another new lane or compiler changes.

Important gaps/contradictions identified in the covered source examples:

- §26 describes exact worst-case machine-stack analysis and an MMIO verifier,
  but its Rust example uses saturating frame addition, unchecked call-map access,
  and supplies no MMIO checker. Exactness also needs trustworthy machine-frame
  and indirect-call metadata. The W1C text names `bv_decide` while its sample uses
  `decide`; no proof result should be inferred from this excerpt.
- §27's straight-line abstract-register scaffold is insufficient to guarantee
  Linux verifier acceptance. It omits CFG joins, real packet-pointer provenance,
  actual stack accesses and loop modeling, indexes registers unchecked, and
  saturates packet offset arithmetic. Its named BPF stack constant is not an
  implementation of complete kernel verification.
- §28's shape example multiplies dimensions unchecked, takes modulo a possibly
  zero quant block, and accepts zero query heads (producing a zero group size).
  Syntactic expression equality cannot establish all product equivalences;
  unresolved alignment obligations cannot silently count as discharged. The
  supplied rewrite has no dtype/schema validation or executed numeric evidence;
  its claimed register residency and egglog integration are not demonstrated.
  The displayed Lean declarations refer to definitions not supplied here and
  cannot substitute for validation of actual kernels.
- §29's tape measure can prove strict descent/termination, but binary-search
  zeroing assumes a monotone failure predicate and does not generally find the
  minimum failing tape. Tape shrinking alone cannot establish source linearity;
  it needs execution of an appropriately checked handler. Finite differential
  samples are evidence, not total migration equivalence or a performance result.

## Delivered API and invariants

`shape::Dimensions` is a nonrecursive, backward-reference arena with owner-tagged
handles; foreign arena IDs fail. Callers choose budgets within hard ceilings of
4096 nodes, depth 128 and 128 symbolic factors per normalized expression. Names
are ASCII identifiers of 1–128 bytes. The supported algebra contains named
symbols, nonnegative u64 constants and multiplication. Checked integer
coefficients and sorted symbol multisets establish justified product equalities.
The planner performs no cancellation, nonlinear factor solving or inferred
bindings. Coefficient overflow is conservatively rejected even with unbound
symbols; no zero-factor rescue is assumed.

Unresolved equality, positivity, divisibility, product representability and
expression representability remain explicit `Obligation`s. Canonically equal
symbolic products retain representability obligations because later bindings
can overflow original subexpressions. `CheckedShape::discharge` requires final
bindings and fails closed on unresolved symbols. Proven predicates are retained
privately and rechecked too, so changing previously bound symbols cannot bypass
contraction or alignment checks. MatMul checks contraction,
positive axes, nonzero quant blocks, alignment, and checked products for both
inputs and output. `resolved_matmul` additionally checks the output element
budget. GQA checks both head counts are positive and divisible; RoPE requires
positive even width. The mapping helper checks query index bounds before dividing.
These are host planning checks, not Vesper compile-time typing.

`host::Graph` builds a typed arena against a private immutable `Schema`. The
supported data types are FP32 vectors and row-major Q4_0 matrices; supported ops
are input, RMSNorm, Q4 matvec and residual addition. Builders validate dimensions,
dtypes, epsilon, parent ownership, node count and graph depth before producing a
node handle. Plan compilation validates aggregate logical storage and abstract
work budgets before execution. Hard ceilings are 4096 IR/schema nodes, depth
128, 16,777,216 logical values, 134,217,728 logical bytes and 1,000,000,000 work
units. Defaults are substantially smaller. Work units bound the supported loop
sizes, not CPU instruction counts or time. Logical-byte accounting excludes
Rust container/allocator metadata; it is not an exact RSS guarantee.

Every binding, including unused schema entries, is checked against the complete
planned schema before kernels execute: names/count, dtype, concrete shape,
exact FP32/weight byte length and finiteness. Schema mismatch, missing/extra
binding, malformed quantized block or nonfinite input/scale fails closed.
Empty vectors/matrices and partial Q4 blocks are unsupported and rejected.
Any nonfinite arithmetic intermediate returns `Error::NonFinite` with no output
published. The immutable plan can run new values with the same schema.

Q4_0 is exactly 18 bytes per 32 values: little-endian binary16 scale and 16
packed bytes, low nibbles for columns 0–15 and high nibbles for 16–31, biased by 8.
Rows contain complete blocks; no unsafe cast or padding assumption is used.
Binary16 decoding covers signed zero, subnormals, finite normals, infinities and
NaN payloads exactly. Kernels reject infinity/NaN scales; negative scales are
supported. This is not GGUF/Safetensors parsing or Q4_K/MXFP4 support.

## Actual floating-point and fusion semantics

All arithmetic is ordered FP32 without `mul_add`. RMSNorm squares and accumulates
in ascending index order, divides by the exactly representable vector length
(at most 2^24), adds epsilon, takes sqrt and reciprocal, then rounds `(x*inv)*w`.
Matvec dequantizes `scale*(nibble-8)`, rounds each product, then adds to its FP32
accumulator in ascending column order. Residual is one FP32 addition per element.
No reassociation or tolerance-based equivalence claim is used.

The optional fusion pass replaces Q4MatVec(RMSNorm(x)) with an executing fused
node. It computes the same reduction once and recomputes each identically rounded
normalized element per output row. This removes the intermediate vector when
no other reachable consumer needs it. Shared DAG values remain correct; fusion
may increase work, and its larger work estimate is budgeted. It does not claim
speedup, register residency, e-graph saturation, SIMD or device execution.

## Validation and runnable example

All checks used a private `/tmp/vesper-expansion37-tensor-target`, one build job,
zero development/test debug information and disabled incremental compilation.
The target occupied about 8 MiB before the optional release check.

Commands (run from repository root):

```sh
cargo fmt --manifest-path crates/vesper-tensor/Cargo.toml --check
cargo test --manifest-path crates/vesper-tensor/Cargo.toml --all-targets
cargo test --release --manifest-path crates/vesper-tensor/Cargo.toml --all-targets
cargo clippy --manifest-path crates/vesper-tensor/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-tensor/Cargo.toml --example cpu_fusion
```

Seventeen integration tests passed in debug and optimized builds, including all 65,536 binary16 encodings,
independent materialized reference versus actual fused/unfused bit patterns,
quantized K=32/64/96 and output tails 1/3/17, nonquantized RMS tails, negative and
subnormal scales, nibble boundary ordering, signed zeros, a concrete
nonreassociation counterexample, shared/nested fusion, schema/dtype/length
mismatches, finite-input arithmetic overflow, explicit symbolic ambiguity,
zero/odd/GQA boundaries, input/output dimension overflow, foreign handles and
expression/IR/work/storage limits. Clippy is warning-free for all targets.

`cpu_fusion` executes RMSNorm→Q4_0 matvec→residual twice and exits nonzero on a
bit mismatch or failure to eliminate the expected intermediate. Observed output:

```text
FP32/Q4_0 fused execution matched all 3 output bit patterns: [1.8651333, -2.4325666, 0.25000006]
unfused=PlanStats { nodes: 5, fused_nodes: 0, values: 236, bytes: 614, work: 841 }
fused=PlanStats { nodes: 4, fused_nodes: 1, values: 204, bytes: 486, work: 905 }
```

No timings were collected. These host tests are not evidence for full LLM
inference, gradient computation, Lean-extracted correctness, constant-time
execution, source linearity, a KV protocol, GPU/AMX/SME2/SIMD lowering, zero heap
allocation or production compiler integration. KV lifecycle work was left out
so the supported planning/kernel slice could be complete and tested.

## Exact coordinator registration proposal

1. Remove the leaf `[workspace]` stanza (and optional leaf dev/test profiles)
   from `crates/vesper-tensor/Cargo.toml` when registering in the root workspace.
2. Add `"crates/vesper-tensor"` to root workspace `members` and `default-members`.
   The package is dependency-free and requires Rust 1.88 or newer.
3. Regenerate only the coordinator-owned root Cargo.lock; the new package has no
   dependencies. The temporary leaf lockfile is not part of this commit.
4. The stable self-checking example command after registration is
   `cargo run -p vesper-tensor --example cpu_fusion` (also suitable for focused CI).
   No root crate dependency or CLI changes are required to run it.
