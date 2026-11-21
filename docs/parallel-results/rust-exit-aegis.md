# Rust-exit SH6 lane 8: Aegis (#1403), part 1

The quote, report and claims parsers, the collateral timestamp parser and
the relying-party policy check of `crates/vesper-aegis` passed the flip
gate, so the Vesper port (`@vesper/aegis`, `crates/vesper-aegis/vesper/`)
is now their only implementation and their Rust is deleted. The rest of the
crate (certificate chains, collateral, CCA/CoRIM, the gate, GPA and host
values) is still Rust; "What is left" below lists it and the parts that
will move it.

Base: `main` at `ab965f2a`. Continues port wave P3a lane 8 (#1043,
[report](port-p3-aegis.md)).

## What changed

1. **Speed (port rewrites only, no compiler edits).**
   - *Zero-copy boundary.* Each parser used to fill two Vesper sequences
     (`inout ChunkSeq[u64]`, `inout ChunkSeq[u8]`), which the binding
     copies in from and back out to Rust `Vec`s on every call: two
     allocations, two frees and a copy of every byte array, about a quarter
     of the port's instructions. A parser now writes into a word buffer
     the adapter lends (`inout Slice[u64]`, a stack array in
     `src/port.rs`): `w[0]` counts the words, and every byte field is the
     *offset* of its bytes in the input, which the adapter slices from the
     input it passed. Only the reversed SNP signature, which the parser
     computes, goes to a second lent buffer (`inout Slice[u8]`, 96 bytes).
   - *Reserved fields.* `all_zero` (`find_not_in`) is vectorized now, so
     `reader::zero` uses it directly instead of the word-at-a-time
     `try_read_u64_le` fold the P3a lane needed (SNP 35,063 to 29,875
     instructions per pass on its own).
   - *SNP fixed offsets.* A report of at least 1,184 bytes cannot be
     truncated, so `snp::parse_whole` reads every field at its layout
     offset with no reader window; shorter input takes the field-by-field
     `parse_short`. Both run the same checks in the same order, so the
     first failure is the same.
   - *Policy.* `ct_eq` folds eight bytes per step instead of one (still
     constant time: every word is visited, no early exit).
2. **Flip.** The `#[cfg(not(vesper_port))]` Rust bodies of the ported
   items and `src/reader.rs` are deleted; the forwarding functions are
   unconditional. Aegis is outside the compiler's closure (`cargo tree -p
   vesper -e normal,build` has no Aegis; `vesper-tee` moved to
   `crates/vesper-tools` in #1133), so the seed package
   `crates/vesper-aegis/seed` is deleted too: `vesper-tools` and the root
   dev-dependency (`tests/native_verifiers_cli.rs`) link the port-backed
   package, and `crates/vesper-hybrid` drops a seed dependency it never
   used. `IMPLEMENTATION` is always `"vesper"`.
3. **Tests move, never vanish.** Every test (100, in 17 integration files and one unit
   test) runs against the port, unchanged except `tests/port_differential.rs`:
   it compared the port with the seed in the same process, and now folds
   the `Debug` text of every result into a SHA-256 digest per test and
   compares it with `tests/data/port_differential.sha256`, recorded from
   the Rust build at `a8570f28`, the last commit with the Rust parsers (the
   Aura pattern of #977, with recorded results instead of a rebuilt
   baseline). Same inputs as before: the vendor samples, 4,000 mutations of
   each, 87,519 timestamps and 200,000 claims/policy pairs (434,559
   results in all). `AEGIS_DIFFERENTIAL_RECORD=1` prints the digests. The
   divergence checks (`port/check.sh`, one wrong line per ported module)
   still fail their named tests. Two edits moved with the code they break:
   reader now skips every reserved-field check (`zero` no longer has the
   word fold the old edit changed), and snp raises the version ceiling to 7
   (the version check is in both SNP paths now).
4. **Benchmark.** `examples/abba.rs` no longer links a Rust build;
   `bench-abba.sh` builds the example at `a8570f28` in a worktree under the
   target directory and runs it as the Rust arm (`window rust`, timed
   inside the child). `flip-gate <workload>` prints the gate's pairs line.
   `flip-gate.toml` configures `scripts/ci/flip_gate.py` (SH6 lane 1,
   #1418).

## Flip gate

Linux x86-64 cloud container (4 vCPU Intel Xeon), Rust 1.97 `--release`,
port built by the in-tree compiler; lane 1's `scripts/ci/flip_gate.py`
from #1418 (not merged yet), 30 epochs per workload, median ratio and
the exact 95% median interval:

| Module | Differential | Tests | Vesper/Rust time | Verdict |
| --- | --- | --- | --- | --- |
| SNP report parse | 0 differences (20,009 results) | pass | 1.13x [1.08x, 1.22x] | pass |
| TDX quote parse | 0 differences (72,018) | pass | 1.26x [1.20x, 1.36x] | pass |
| SGX quote parse | 0 differences (24,006) | pass | 1.34x [1.23x, 1.45x] | pass |
| Claims wire decode | 0 differences (12,007) | pass | (no workload; one call per quote) | pass |
| Timestamps | 0 differences (87,519) | pass | (no workload) | pass |
| Policy check | 0 differences (200,000) | pass | 0.54x [0.51x, 0.55x] | pass |

The gate's line, run on the flipped tree (differential against the
recorded Rust results, ABBA against the `a8570f28` build; the table's
numbers are this run). An earlier run before the flip, with both builds in
one binary, gave snp 1.24x [1.20x, 1.26x], tdx 1.26x [1.24x, 1.28x], sgx
1.39x [1.38x, 1.41x] and policy 0.50x [0.49x, 0.51x]:

```
flip-gate vesper-aegis: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 1/1 pass | differential 0 differences | ABBA vesper/rust snp 1.13x [1.08x, 1.22x] n=30; tdx 1.26x [1.20x, 1.36x] n=30; sgx 1.34x [1.23x, 1.45x] n=30; policy 0.54x [0.51x, 0.55x] n=30
```

Before this lane, on `main`: snp 1.96x, tdx 2.03x, sgx 2.10x, policy 1.33x
(medians, 9 epochs). Instructions per pass (callgrind), port then Rust:

| Workload | `main` | This PR | Rust |
| --- | --: | --: | --: |
| snp | 35,063 | 20,473 | 17,242 |
| tdx | 42,780 | 23,540 | 18,764 |
| sgx | 23,200 | 14,024 | 10,483 |
| policy | 105,062 | 36,714 | 44,242 |

## Rust deleted

1,274 lines of Rust and manifests removed, 286 added (the adapter's
offset reader, the digest differential and the subprocess benchmark):
`src/reader.rs` (84), the Rust parser and policy bodies in `snp.rs`,
`tdx.rs`, `sgx.rs`, `claims.rs`, `policy.rs` and `time.rs` (811), and the
seed package (194). 8,858 non-blank lines of Rust remain in
`crates/vesper-aegis/src`.

## Compiler gaps

Each with a minimal repro; none is fixed here (port lane: no compiler
edits).

1. **`Option` results go through the stack.** `fn f(in b: Slice[u8]) ->
   u64 { match b.try_read_u32_le(0) { Option::Some(v) => widen_u64(v),
   Option::None => 0 } }` stores the tag and payload to a stack slot,
   reloads both and compares the tag (about 12 instructions per read, most
   of what the SNP parse still spends). Keeping a scalar `Option` in
   registers (or folding `try_read_*` behind a dominating length check)
   would remove most of the remaining gap.
2. **`inout` copy structs and slice headers are reloaded after every
   call.** The reader window `Rd` (`inout r: Rd`) and the input's header
   are loaded from memory again after each `take`, even when nothing they
   alias was written. The SNP fixed-offset path is the workaround.
3. **`inout ChunkSeq` at the export boundary copies.** `with_seq_mut`
   (crates/vesper-port-abi) copies a Rust `Vec` into a fresh Vesper
   sequence and back on every call. Lending a Rust buffer as `inout
   Slice[T]` avoids it; a sequence the binding lends in place would let a
   port return variable-length output without the offset scheme.
4. **Resolved since P3a:** `find_not_in` is vectorized (the P3a report's
   gap 1), so reserved-field checks need no hand-written word fold.
   `extend_reversed` exists now (gap 2), but the SNP signature is written
   with `try_write_u64_be` into the lent buffer, which needs no sequence.

## What is left

The crate's remaining Rust, by the part that will move it:

| Rust | Lines | Part | Needs |
| --- | --: | --- | --- |
| `cca.rs`, `corim.rs`: CBOR token and CoRIM structure, appraisal | 2,484 | part 2 | a CBOR reader in Vesper; COSE signatures go behind lane 17's shim |
| `gate.rs`: frame and ring logic | 497 | part 3 | ChaCha20-Poly1305 behind lane 17's shim; the evaluator callbacks stay as Rust adapters while callers are Rust |
| `gpa.rs`, `host.rs`, `linear.rs` | 1,012 | part 3 | atomics in source (#1037) for the leak ledger |
| `intel.rs`, `amd.rs`: collateral, TCB status, CRL floors | 1,655 | part 4 | JSON collateral in Vesper; X.509 and ECDSA behind lane 17's shim |
| `chain.rs`, `roots.rs`: X.509 paths, pinned roots | 696 | part 4 | lane 17's shim (vetted crypto, never hand-rolled) |
| `claims.rs` rest: claim extraction, register replay (SHA-384) | 258 | parts 2-4 | SHA-2 behind lane 17's shim |
| `snp.rs`, `tdx.rs`, `sgx.rs`: struct definitions, encoders, end-to-end verifiers | 1,400 | parts 2-4 | the verifiers call the crypto above; the structs stay while dependents are Rust |
| `policy.rs` rest, `time.rs`: RA-TLS check (SPKI channel binding, SHA-384) and forwarding | 227 | part 4 | lane 17's shim |
| `port.rs`, `error.rs`, `lib.rs`: adapter, error types | 629 | last | dependents in Vesper (`vesper-tools` tee, the root CLI test) |

The census row (`scripts/rust-census.py` STATUS) moves to `partial` with
the parsers and policy flipped once the census script is on `main` (plan
PR #1415).

## Platforms run

Locally: Linux x86-64 (Cargo, Rust 1.97, `cargo test --release -p
vesper-aegis`, `cargo test --release -p vesper --test
native_verifiers_cli`, `cargo build -p vesper-tools`, the flip gate).
CI: the four required checks.
