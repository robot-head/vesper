# Self-hosting SH5 lane 16: LSP transport and memo tables

Claim #1339. Base `bbda89e` (main). Prompt: `docs/agent-prompts/self-host-sh5/16-lsp-core.md`.
Rust stays the default: the language server still runs `crates/vesper-lsp`
and salsa. This lane adds no Rust code to the compiler and deletes none.

## What changed

- **`packages/vesper-lsp`** (`@vesper/lsp`, new, about 1,900 lines): the
  JSON-RPC transport of `crates/vesper-lsp/src/transport.rs` over the
  serde port (`crates/vesper-serde/vesper`).
  - `limits.ves`: `ProtocolLimits` and `ProtocolErrorKind`, with its
    `Display` names.
  - `frame.ves`: `read_frame`. Covers bounded header lines and header
    counts, `Content-Length` (missing, duplicate or invalid), the
    `Content-Type` charset, the body limit and truncation.
  - `envelope.ves`: `parse_envelope` on the serde port's token tape.
    - It does the Rust preflight checks in the same order: depth, nodes,
      string bytes and work, then duplicate keys.
    - It then applies serde_json's number rules: `-0` is a float, so as an
      id it is `InvalidId` and as an error code `InvalidEnvelope`; numbers
      that overflow to infinity are refused; long integers are decoded as
      floats.
  - `encode.ves`: `encode_frame`.
    - The output budget (`JsonBudget`).
    - serde_json's compact writer, write by write. The output limit is
      checked on every write, as Rust's `Output` does, so the
      `OutputLimit`/`WorkLimit` split is the same.
    - Escapes use lowercase `\u00XX`.
    - Floats use ryu's `format64` layout from the serde port's shortest
      digits, including the `e+16` sign.
  - `lib.ves`:
    - The entries `lsp_frames`, `lsp_envelope` and `lsp_encode`.
    - **`lsp_serve`**, the transport half of the stdio loop (`dispatch` in
      `src/lsp.rs`). It frames the client stream and parses each body.
      For a refused body it encodes the server's refusal itself: no id,
      -32700 or -32600, and the message "LSP <kind>". It routes requests,
      notifications, `$/cancelRequest` and client responses, and it stops
      at EOF or on a framing error.
- **`packages/vesper-lsp/documents`** (`@vesper/lsp-documents`, new):
  `DocumentStore` of `crates/vesper-lsp/src/document.rs` over the span
  port.
  - Operations: open, change (ranged and full edits, `rangeLength`, UTF-16
    positions), close and get.
  - It has every limit, every refusal, revisions, and the C0/DEL/C1 URI
    rule.
  - The store is flat: one `String` arena with per-document offsets,
    compacted once the dead bytes pass both 64 KiB and the live bytes.
  - It is its own package because of compiler gap 1.
- **`packages/vesper-memo`** (`@vesper/memo`, new): memo tables with the
  salsa 0.24 invalidation that `src/database.rs` relies on.
  - Inputs: `new` keeps the revision; a setter always starts a new one.
  - Derived memos per (query, input), each holding its verified-at and
    changed-at revisions and its reads in order.
  - `fetch`: hot, else deep-verify the reads in order, else execute.
  - `maybe_changed_after`: verifies, and re-executes if needed.
  - Early cutoff: a value equal to the old one keeps the old changed-at,
    for queries that compare only, never `no_eq` ones.
  - An oracle supplies what each query reads and returns. The differential
    records it from the real queries.
- **`tests/self_host_lsp_core.rs`** (new): the differential, described
  below.
- `BUILD.bazel`: the test's data (anchored globs over `examples/**` and
  the three packages, plus `//crates/vesper-serde:files` and
  `//crates/vesper-span:files`) and the `long` tag.

## Differential

The recorded sessions are produced by the live `vesper lsp` server, run at
test time:

- every stdio scenario of `tests/lsp_cli.rs`: unicode navigation, the
  lifecycle with recovery, five framing errors, legacy symbols and five
  legacy actions;
- an editing session over each of eight examples plus an inline source.
  Each one has body edits, two changes in one notification, a stale
  version, an out-of-range edit, a version-only change, formatting, a
  revert and a full replace;
- a transport session with eight refused bodies, cancellations and a
  client response.

`cargo test --release --test self_host_lsp_core` (Linux x86-64; the same
tests run unoptimized in CI):

| Test | Compared | Differences |
|---|--:|--:|
| `frames_match_rust`: `read_frame` on both streams of every session, plus the framing cases of crates/vesper-lsp and every truncation and limit edge | 176 streams | 0 |
| `envelopes_match_rust`: `parse_envelope` on every body in both directions, plus the envelope cases | 537 bodies | 0 |
| `responses_match_byte_for_byte`: every frame the server wrote, re-encoded by `@vesper/lsp`, equals the server's bytes; the encoder cases give the same frame or error as Rust | 218 server frames + 37 cases | 0 |
| `stdio_loop_matches_server`: `lsp_serve` against `dispatch`'s transport decisions on every session and framing case; each refusal is a frame the live server wrote | 152 streams, 417 records, 17 refusals | 0 |
| `documents_match_rust`: every result, count, total, revision, version and text after every op of every session and of crates/vesper-lsp/tests/documents.rs, plus revision overflow | 36 scripts, 268 ops | 0 |
| `memo_invalidation_matches_salsa`: salsa's `WillExecute` and `DidValidateMemoizedValue` events, in order, against `memo_replay`'s log, plus every fetched value's fingerprint | 32 databases, 1,056 executions, 237 validations, 1,015 fetches | 0 |

How the memo differential works:

- The test builds its own `#[salsa::db]` with an event callback.
- It runs the actual `src/database.rs` queries on each script the way
  `Session::accepted` does:
  - `set_text` only when the text differs;
  - `set_snapshot` on every accepted change;
  - a new database on close and after 64 accepted changes.
- It fetches `guarded_analysis`, `diagnostics`, `signature_inventory`,
  `preview` and `recovery_diagnostics` in rotation.
- A fingerprint is the index of the first equal value under the type's own
  `PartialEq`: `Arc::ptr_eq` for the recovery snapshot, and blake3
  identities for signatures.

What the counts show:

- The scripts include 35 signature cutoffs, where `signature_snapshot`
  re-executes and `signature_inventory` is only validated.
- Replaying with cutoff turned off changes the log of 13 of the 32
  databases. This shows the comparison detects backdating.
- The scripts are the recorded sessions' edits plus `memo_scripts`: two
  documents with body edits, a signature edit, a parse error and its fix,
  a revert, a version-only change, a close, and 79 changes past the reset.

## Numbers

ABBA best of six, 20 rounds per side, `CHECK_HARNESS_SPEED=1 cargo test
--release --test self_host_lsp_core lsp_core_speed -- --ignored`, on a
4-core Linux cloud host. On these hosts wall time moves by up to 30%
between runs.

- Both sides get their inputs built up front.
- The Vesper side includes the JIT call, decoding the harness input and
  copying the result out.
- "Before" is the Rust implementation, which stays the default. There was
  no Vesper implementation before this lane.

| Layer (per item) | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Transport loop: framing, parsing and refusals, per client message (252) | 2.54 µs | 3.03 µs | 1.19x |
| `parse_envelope`, per body (470) | 3.18 µs | 3.70 µs | 1.16x |
| `read_frame`, per stream (48) | 1.37 µs | 2.43 µs | 1.78x |
| `encode_frame`, per response (218) | 1.51 µs | 3.88 µs | 2.58x |
| Document store, per op (190) | 0.83 µs | 1.96 µs | 2.34x |
| Memo bookkeeping, per fetch (1,015) | n/a | 0.27 µs | n/a |

On the memo numbers:

- The memo bookkeeping has no Rust counterpart outside salsa with its real
  query bodies. Its 0.27 µs per fetch includes decoding the oracle, and it
  is negligible next to the queries themselves.
- Not at parity on encode or the store, so Rust stays the default. Encode
  pays for one `ChunkSeq` push per output byte; Rust writes slices.

## Stage 1 guard

This lane edits no file under `src/`, `bootstrap/`, `lean/`,
`packages/vesper-compiler/` or any crate. The verified backend and its
inputs are untouched. Evidence: the `bootstrap-stage1` workflow on the PR
branch (linked from the PR) compares a fresh `bootstrap/run.sh stage1
--reproduce --outcome` with `bootstrap/stage1.lock`.

## Compiler gaps

1. **Enum variant names are world-global and shadow builtins.**
   - Once any package in a world declares a variant named `String`, as
     `@vesper/serde`'s `Kind::String` does, `String::new()` is refused in
     every module of that world, even modules that do not import it. Other
     variants also clash across packages (E0113), so `Perr` and
     `StoreError` here prefix their variants.
   - Workaround: the document store is a separate package.
   - Repro, package `a`: `pub copy enum Kind { Null, String }`. Package `b`
     depends on `a`:
     ```vesper
     pub fn empty() -> u64 {
         let s = String::new();
         s.len()
     }
     ```
     `vesper check b` reports E0111: "variant 'String' belongs to enum
     @repro/a::Kind, which is not declared or imported in this module".
2. **A variant constructor in a `handle` body or clause leaves the other
   type parameter as `_`.**
   - `vesper check` accepts the program, but `jit`/`build` fail with
     "native layout is not yet defined for _". A type annotation on the
     binding does not help.
   - Workaround: typed constructor functions (`env_ok`, `count_err`, ...)
     at every handle site.
   - Repro (`vesper jit repro.ves`):
     ```vesper
     copy enum E { Bad }
     fn half(x: u64) -> u64 eff [Fail[E]] {
         if x % 2 == 1 { Fail::raise(E::Bad) } else { x / 2 }
     }
     fn try_half(x: u64) -> Result[u64, E] {
         handle Fail[E] in {
             Result::Ok(half(x))
         } with {
             return v => v,
             raise(e, resume) => Result::Err(e),
         }
     }
     fn main() -> u64 {
         match try_half(4) { Result::Ok(v) => v, Result::Err(e) => 99 }
     }
     ```
     The same program with `fn ok(v: u64) -> Result[u64, E] { Result::Ok(v) }`
     in the body runs and returns 2.
3. **`ChunkSeq` has no `insert` or `remove`.**
   - ryu-style float text has to be built from digit slices instead of
     inserting the point.
   - Workaround: build each layout from pieces.
   - Repro: `let mut s: ChunkSeq[u8] = ChunkSeq::new(); s.insert(0, 1);`
     reports an unknown method.
4. **A view of a view is refused.** `s.pool.as_bytes()[a..b]` gives
   E1531/E1521. Binding the view first, `let p = s.pool.as_bytes();`, then
   `p[a..b]`, works.
5. **Shifts and `*` always carry `Fail`, and there is no `wrapping_neg`.**
   Byte packing needs `wrapping_shl_u64` with a `u32` count, and negation
   needs `wrapping_sub(0, v)`. These are minor ergonomics gaps, not
   correctness gaps.

## What is left

- **The hybrid module `lsp_core`** is not wired. Swapping in the Vesper
  transport needs either an export ABI binding of `@vesper/lsp` for
  `crates/vesper-lsp` (the seed pattern) or the hybrid compiler's module
  table. It also needs encode and the store at speed parity first.
- **The rest of the stdio loop**: the pending-request table (`begin`,
  `cancel`, `finish`, `MAX_PENDING`), read-ahead credits, the threads, and
  `send`'s `-32004` fallback when a feature result exceeds the output
  limits. `lsp_serve` routes cancellations, but the pending table that
  decides whether they answer stays in Rust.
- **Memo tables in a server**: `@vesper/memo` takes its reads and values
  from an oracle. A Vesper server would call the query bodies in their
  place, and those are the checker ports of earlier waves.
- **Speed**: encode (2.6x) needs slice writes into the output buffer; the
  store (2.3x) needs cheaper arena edits. Lane 17 (LSP features) builds on
  this transport.
