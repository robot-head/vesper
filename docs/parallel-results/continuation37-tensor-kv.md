# §28 bounded CPU KV-cache lifecycle (#49)

Implemented issue [#49](https://github.com/robot-head/vesper/issues/49) on
`continuation37/tensor-kv`, starting from `5e699f8`. Read the issue and refreshed
specification §28.2–28.4. Exclusive changes are in `crates/vesper-tensor/**` and
this report; the existing dimension checker and FP32/Q4 kernels are preserved.
Root manifests, Cargo.lock, CLI, workflows, other crates and external Warp work
were read-only.

## Working storage and ownership protocol

The new `vesper_tensor::kv` module stores actual BF16 K/V bytes, rather than just
counting page leases. `KvPool::new` checks nonzero heads/width, checked element,
byte and page products, sequence/token limits and payload/table budgets. Each
physical page has sixteen token slots, each slot containing `[head, width]`
little-endian BF16 elements. Keys occupy the first page plane and values the
second. BF16 bits are opaque: signed zeros, NaNs and infinities are stored and
read without arithmetic, conversion or a numerical inference claim.

The pool has a private process-local identity; sequence handles carry a private
slot/generation key and are neither Clone nor Copy. Page mappings carry private
page-generation leases, and every used lease must match its current page owner.
`begin` assigns a fresh sequence generation. `append` requires the exact next
position and complete correctly shaped K/V steps. `read` and `read_head` expose
only borrowed bytes from valid existing tokens; Rust borrowing prevents a live
read from surviving a mutation of its pool. No raw page IDs or mutable backing
bytes are public.

`checkpoint` captures a sequence-local speculative boundary and epoch.
`rollback` truncates to that boundary, zeros both K/V tails of a retained page,
and zeroes/frees all completely removed pages. A successful rollback increments
the epoch and invalidates all old checkpoints from that epoch, including nested
ones. Callers obtain a fresh checkpoint before another speculative round.
Checkpoints from another sequence, an earlier sequence incarnation, or another
pool fail. Appended tokens are readable immediately; explicitly choosing rollback
is the authorization to remove the suffix. No separate commit operation is
needed to retain that suffix.

`release` is the explicit cancellation/completion path. It consumes the sequence
only on success, zeroes its pages and makes the sequence slot reusable. A failed
release returns `ReleaseFailure { error, sequence }`, so a foreign-pool error
cannot lose the original owner's handle. Append/rollback take a mutable borrow
of the handle and preserve it on every error. Dropping a handle does not reclaim
its pages; this is an explicit host protocol, not a Rust or Vesper linearity
proof, scope cleanup guarantee or automatic cancellation runtime.

All append and rollback errors are detected before mutation. Shape, position,
OOM, stale generation/checkpoint and foreign-owner failures preserve all existing
tokens and page allocations. Sequence, page and checkpoint counters use checked
increments and never wrap. Exhausted free sequence/page generations are skipped
while reusable slots remain; when none remain, `GenerationExhausted` is returned.
Release still works for a valid owner at an exhausted epoch/generation. The
`free_pages` statistic counts unowned physical pages, including generation-retired
ones; it is not a promise that all free pages can be allocated again.

## Explicit bounds and limitations

Hard maxima are 4096 physical pages, 1024 sequence slots, 65,536 tokens per
sequence, 64 MiB of payload backing bytes, and 65,536 preallocated page-reference
entries across sequence slots. The configured per-sequence token limit must fit
in the physical page capacity. All K/V products must fit usize and the configured
payload budget. The budget counts payload bytes, not allocator overhead or exact
RSS; slot/page/reference metadata is bounded separately.

Payload storage and per-sequence reference capacities are allocated during pool
creation using fallible reserves. Begin, append, read, checkpoint, rollback and
release do not grow their storage. A read is O(1); mutation validation/scanning
is bounded by the configured page count, and clearing is bounded by total
payload capacity. Allocation failure during construction yields no partial pool.
No runtime allocator pressure is introduced by append, including page boundaries.

Page reuse starts from zeroed bytes; logical reads additionally require a token
position below the live sequence length. This prevents stale suffixes or another
sequence's data from becoming readable through the public API. Ordinary byte
clearing is not a cryptographic memory-erasure or constant-time guarantee.
No GPU/VRAM, paged attention kernel, RoPE, full inference, source typing/effects,
Lean extraction, cross-ISA floating-point determinism or performance claim is
made. Existing FP32/Q4 shape/kernel behavior remains covered by its prior tests.

## Validation and example

Use a private build target with one job, debug info disabled and incremental off:

```sh
export CARGO_TARGET_DIR=/tmp/vesper-continuation37-tensor-kv-target
export CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
cargo fmt -p vesper-tensor -- --check
cargo test --locked -p vesper-tensor --all-targets
cargo clippy --locked -p vesper-tensor --all-targets -- -D warnings
cargo run --locked -p vesper-tensor --example kv_pages
```

Twenty-five tests pass: eight new KV tests plus all seventeen existing Tensor
checks. Public-API tests compare actual token/head payloads across positions
15/16/17/31/32/33, exercise rollback to 0/1/15/16/17/31/32 after 49 tokens,
reappend different data, independent sequences, explicit cancellation/release,
reuse, full-pool OOM, token/sequence limits, shape/length/position errors and
returned foreign owners. Internal tests inspect actual zeroed byte tails and
released pages, inject exhausted sequence/page/checkpoint counters, and verify
page-generation mismatches and stale handles fail before any byte mutation.
Strict all-target Clippy and formatting checks pass.

`cargo run -p vesper-tensor --example kv_pages` is a self-checking acceptance
example: append 33 tokens, roll back across pages to 15, reuse freed pages for an
independent sequence, verify both sequences' payloads, release both, and assert
that all three pages return to the pool. Assertion or API failures exit nonzero.
It prints:

```text
BF16 KV payloads survived page-boundary rollback and independent reuse; all 3 pages reclaimed
```

No new dependencies or coordinator-owned manifest/lock changes are needed.
