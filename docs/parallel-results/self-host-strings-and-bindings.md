# Self-hosting SH1 lane 4: strings and export-binding costs

Claim: #1147. Plan: `docs/agent-prompts/self-host-sh1/04-strings-and-bindings.md`.
Base: e29e63d (origin/main). PR: #1160.

The SH0 pilots and the hybrid build found four costs at the string and
binding level that every swapped compiler module would pay. This lane
re-checked each on current `main`, fixed the ones that still held and
measured the toolchain pilots and one P3 port before and after.

Rust stays the default everywhere. Nothing Rust is deleted.

## Reproduced on main

| Gap | Source | On main | Scope |
| --- | --- | --- | --- |
| `String::push_str` copies byte by byte | pilots gap 4 | still holds: the native lowering was a byte loop (`seq_copy`) | fixed |
| byte `extend` copies byte by byte | pilots gap 4 | already passes: `SeqOp::Extend` calls `memmove` (byte loops lane, #942/#947) | dropped |
| bindings re-validate returned `String`s | pilots gap 5 | still holds: `vesper_port_abi::str_view` ran `from_utf8` on every returned or viewed string | fixed |
| aggregate results allocate a `Vec` per call | hybrid gap 1 | holds for export ABI v1 only (`port-pilot`, `port-build` bindings); the v2/v3 generator already uses `[0u64; N]` | fixed (v1) |
| no host API for `Str` in / sequence out | front end v2 gap 7 | still holds | fixed |

Repros committed before the fix (both fail on `main`):

- `tests/string_copy.rs` `push_str_lowers_to_a_block_copy`: the emitted CLIF
  of `fn append(inout s: String, in t: Str) { s.push_str(in t); }` calls
  `memmove`, with the optimizer on and off.
- `tests/binding_stack_words.rs` `v1_aggregate_words_are_stack_arrays`: the
  v1 binding of an `Option` result, a struct argument and a `Fail` record has
  no `vec!`.

## What changed

- **`push_str`** (`src/strings/native.rs`, `src/seq/native_bytes.rs`): one
  reserve, then `seq_copy_run`: a run of 16 bytes or more is one `memmove`
  where the C library is available (the JIT and hosted executables); a
  shorter run, most literals, keeps the byte loop, which costs less than a
  call. Freestanding images keep the byte loop. Source and destination never
  overlap (the checker refuses `s.push_str(in s...)`), and `memmove` would
  be correct if they did.
- **v1 bindings** (`src/native_export/binding.rs`): the result, pointer
  arguments and the failure record are `[0u64; N]` stack arrays. Regenerated
  `crates/vesper-port-pilot/port/binding.rs` and
  `crates/vesper-port-build/port/binding.rs` with `VESPER_PORT_BLESS=1` (no
  hand edits). No v2/v3 binding text changed, so no other port binding moved.
- **UTF-8** (`crates/vesper-port-abi/src/seq.rs`): `str_view`, and through
  it `string_take` and `with_string_mut`, take the bytes with
  `from_utf8_unchecked`. Vesper strings are UTF-8 by construction (§13:
  bytes enter only as a whole encoded char, another string or a literal), so
  this is the caller's existing safety contract; a `debug_assert!` still
  checks. The copy into a Rust `String` stays: Vesper's buffer comes from the
  library's `malloc`, which Rust's allocator cannot adopt.
- **`vesper_port_abi::HostCall`** (`crates/vesper-port-abi/src/host.rs`): a
  builder of `word`, `str` and `bytes` arguments laid out as the export ABI
  passes them (a `Str` as the address of `{len, 0, data}`, a `Slice[u8]` as
  the address of the view `{header, 0, len}`), with `with_words`, `seq::<T>`
  (a `ChunkSeq[T]` handle result, copied and freed) and `string` (a `String`
  through a three-word result pointer). `tests/self_host_front_v2.rs` uses it
  in place of its `str_words`/`take_words` plumbing; the root crate gains
  `vesper-port-abi` as a dev-dependency (root patch, own commit).

No manuscript change: no language rule, builtin or std API changed.

### Tried and reverted

Copying packed scalar sequences in `seq_copy` with one block copy instead of
the per-element loop raised span index and span edits Ir by 7.5%: glibc's
`memcpy` uses `rep movsb` for these sizes, which callgrind counts per byte,
and the per-element loop was already vectorized by rustc. Wall clock did not
move (ABBA index 0.28 both ways). Reverted.

## Stage 1 guard

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed
(`git diff --name-only origin/main`). The Stage 1 corpus uses no strings, so
no Cranelift artifact changes either. The `bootstrap-stage1` workflow ran on
the branch; its result is on the PR.

## Numbers

Linux x86_64 cloud container, release builds, the crates' own
`scripts/bench.sh` with `CALLGRIND=1`: instructions for five batches (runs of
6 minus 1 batches). Ir is exact; wall clock on these hosts moves by up to 30%
between runs.

| Workload | Vesper Ir before | Vesper Ir after | Change | Rust Ir | Ir ratio after |
| --- | --- | --- | --- | --- | --- |
| span lookup | 51.30M | 51.30M | 0 | 34.76M | 1.48 |
| span index | 2.68M | 2.68M | 0 | 10.31M | 0.26 |
| span edits (64 insertions) | 3.50M | 3.50M | 0 | 0.92M | 3.80 |
| lean term (depth-15 term) | 419.3M | 380.1M | **-9.3%** | 1,401M | 0.27 |
| lean module (400 definitions) | 331.9M | 300.3M | **-9.5%** | 997M | 0.30 |
| lean free | 293.5M | 294.0M | +0.2% | 96.0M | 3.06 |
| prism-h3 decode | 165.8M | 165.7M | -0.1% | 171.5M | 0.97 |
| prism-h3 encode | 75.5M | 74.7M | -1.0% | 60.0M | 1.24 |
| prism-h3 frames | 61.0M | 61.5M | +0.8% | 57.9M | 1.06 |
| prism-h3 messages | 101.3M | 100.1M | -1.2% | 56.4M | 1.78 |

(The prism-h3 harness rebuilds both arms per run; its Rust arm moved by up to
6% between the two runs with identical Rust code, so the ±1% Vesper moves
there are noise. Span rows were re-measured after the revert above.)

- **Lean print** is where the lane shows: the printer pushes every
  identifier, keyword and literal into one pre-sized `String`, and the
  returned text is no longer validated again in Rust.
- **Span edits** does not move because the committed port already works
  around gap 4: `edits_apply` returns a `ChunkSeq[u8]` built with `extend`
  (already `memmove`), and Rust validates it with `String::from_utf8` in
  `src/lib.rs`. Moving it back to a `String` result is a port change for
  the span lane, not this one; the lowering it needs is now in place.
- No P3 port uses `push_str` or returns strings (checked: pact, aegis,
  bench, loom-audit, ebpf, silicon, warp, prism-h3, chorus, flux, kinetic,
  resonance). prism-h3 was measured as the P3 port whose bindings take the
  most sequences; it is flat.

ABBA (branch, median Vesper/Rust): span lookup 1.49, index 0.28, edits
3.20; lean term 0.32, module 0.36, free 1.91; prism-h3 decode 0.94, encode
1.39, frames 0.99, messages 2.38.

## Compiler gaps

1. **No inline short copy.** A `push_str` of a run under 16 bytes is still a
   byte loop with a bounds-free load and store per byte. A run whose length
   is a constant (every literal) could be a fixed sequence of wide loads and
   stores, but the length reaches the lowering as a load from the 24-byte
   string value, so the constant is not visible. Needs the literal's length
   carried into `StrOp::PushStr` (Place IR, Stage 1 guarded) or a fold of
   the store/load pair.

   ```vesper
   fn sep(inout s: String) { s.push_str(in ", "); }   // 2-byte loop, not one 16-bit store
   ```

2. **Returned strings and sequences are copied once.** `string_take` and
   `seq_take` copy Vesper's buffer into a Rust `String`/`Vec` and free it,
   because the buffer comes from the library's `malloc`. A binding that
   returned a borrowed view with a drop guard (`VesperString`, derefs to
   `&str`) would skip the copy; that changes the binding's public types, so it
   is left for a port that needs it.

3. **`WordFunction` cannot return a `String`.** The JIT host entry takes and
   returns one word each, so a JIT entry returning `String` is refused
   (`a word function returns one word`). `HostCall::string` works for
   export-ABI objects (result pointer) only; JIT callers return a
   `ChunkSeq[u8]`.

   ```vesper
   pub fn name(in s: Str) -> String { s.concat(in "!") }   // WordFunction::compile refuses
   ```

## What is left

- Span edits back to a `String` result, now that `push_str` is a block copy
  (span port follow-up).
- Gaps 1-3 above.
