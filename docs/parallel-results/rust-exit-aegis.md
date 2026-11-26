# Rust-exit SH6 lane 8: Aegis (#1403)

Part 1 (#1433) flipped the parsers and the policy check. Part 2a (below,
after part 1) flips the CBOR decoding and the CCA claim parsers.

## Part 1

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
   `crates/vesper-aegis/seed` is deleted too, and `crates/vesper-hybrid`
   drops a seed dependency it never used. Its last two users needed only
   `claims::single_extend` (one SHA-384/SHA-512 register extend): the
   `vesper-tee` tool and `tests/native_verifiers_cli.rs` now compute it
   with `sha2` instead of linking Aegis. Neither may link the port: Bazel
   builds the port with `//:bin/vesper`, whose runfiles include the tools
   and whose `aliases` name the root crate's dev-dependencies, so either
   link is a dependency cycle (it was the first CI failure of #1433). `IMPLEMENTATION` is always `"vesper"`.
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

## Part 2a: CBOR decoding and CCA claims

Base: `main` at `11cdaefa` (part 1 merged).

1. **CBOR decoder.** `vesper/src/cbor.ves` decodes one item into preorder
   nodes in a word buffer the adapter lends: integers, floats, booleans
   and null as two words, strings as their chunks (offset and length into
   the input), arrays, maps and tags as a count or tag followed by their
   items. It accepts exactly what ciborium 0.2's `Value` deserializer
   accepts, quirks included: nested indefinite strings, the one-byte form
   of simple values (only 20 to 23 are accepted), floats widened as
   `half` and the hardware widen them, bignums (tags 2 and 3 over at most
   16 definite bytes) folded to integers when they fit in 64 bits, and the
   256-level recursion limit. `cca::decode_value`, which the CCA token,
   CoRIM, CoMID and corim-meta parsers share, now uses it; the adapter
   rebuilds the `Value` the Rust structure code still walks.
2. **CCA claims.** `vesper/src/cca.ves` is `CcaRealmClaims::from_cbor`,
   `CcaPlatformClaims::from_cbor` (software components, TBB RoTPK
   entries), and the platform and realm profile checks, over the port's
   nodes. Each claim comes back as chunks of the payload, and every
   `ParseError::Cbor` is a field and reason index into tables in
   `src/port.rs`. The Rust claim parsers are deleted.
3. **Tests.** `tests/port_differential.rs` gains `cca_tokens_agree`,
   `corims_agree` and `cca_claims_agree` (434,086 results: every CCA and
   CoRIM vendor sample, 2,000 or 4,000 mutations of each, the tokens'
   payloads, and 20,000 random claim sets on both sides of every limit,
   encoded by `to_cbor`). Their digests were recorded from the Rust build
   at `11cdaefa`, and the port matches them. The decoder also keeps a live
   differential against ciborium, which coset still links
   (`tests/cbor_differential.rs`): edge
   cases, the vendor samples and the byte strings inside them with 2,000
   mutations each, and 200,000 random inputs. Two divergence checks join
   `port/check.sh`: one level less nesting (`cbor`) and TBB RoTPK index 5
   refused (`cca`).
4. **Benchmark.** Three workloads join `examples/abba.rs`: `cca_claims`
   (both claim parsers on the vendor tokens' payloads), `cca_token`
   (`CcaToken::parse`, whose COSE_Sign1 structure is still Rust) and
   `corim` (`Corim::parse_unsigned`, whose structure is still Rust).
   `bench-abba.sh` builds their Rust arm at `11cdaefa` from this
   checkout's `examples/abba.rs`, which uses only the public API.

Flip gate on the flipped tree (same machine and script as part 1):

```
flip-gate vesper-aegis: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 1/1 pass | differential 0 differences | ABBA vesper/rust snp 1.19x [1.16x, 1.22x] n=30; tdx 1.21x [1.19x, 1.23x] n=30; sgx 1.36x [1.30x, 1.39x] n=30; policy 0.51x [0.50x, 0.56x] n=30; cca_claims 1.30x [1.27x, 1.35x] n=30; cca_token 1.08x [1.03x, 1.12x] n=30; corim 0.80x [0.75x, 0.94x] n=30
```

The CoRIM parse got faster (0.80x) because the decoder hands back chunk
offsets instead of building through serde. The claims parsers are 1.30x,
mostly because the output is written twice (once as nodes, then as claim
words) and because of the `Option` reloads of gap 1. 234 lines of Rust
were deleted, and the adapter added 438. About half of those are the
decoder's live differential and the field and reason tables. The
ciborium `Value` the CoRIM structure code walks goes away with part 2b.

## Part 2b: COSE, CoRIM structure, and CCA appraisal

Base: `main` at `0f5a9eba`.

1. **COSE (`vesper/src/cose.ves`) & extended CBOR scratch (`vesper/src/cbor.ves`).**
   `cose.ves` parses tagged `COSE_Sign1` (tag 18) with full `coset` 0.3.8
   header/alg/crit/`content_type`/`kid`/`iv`/`partial_iv`/`counter_signature`
   validation, builds RFC 8152 §4.4 `Sig_structure` TBS bytes (`cose_sig_structure`),
   and decodes P-384 EC2 `COSE_Key` / raw 97-byte SEC1 RAKs (`cose_key_p384`).
   `cbor.ves` gained `decode_at`, `decode_whole_at`, scratch-backed
   `materialize_bytes` (so embedded indefinite-length byte strings resize a
   caller scratch buffer on retry with zero scratch allocation for definite
   CBOR), and string/integer helpers (`text_eq_s`, `str_nodes_eq`, `is_i64`,
   `i64_bits`, `int_nodes_eq`).
2. **CCA collections (`vesper/src/cca.ves`).** `cca_token_parse` handles Tag 399
   and CMW Tag 907 (`[263, bstr]`) collections, validates both `COSE_Sign1`
   envelopes, decodes each payload once for claims and profiles, and preserves
   exact error precedence. A minimal internal CBOR encoder in `src/cca.rs`
   replaces `coset` in test/simulator encoders (`to_cbor`, `encode_collection`,
   `encode_realm_cose_key`, `build_sign1`).
3. **CoRIM / CoMID & CCA profile appraisal (`vesper/src/corim.ves`, `vesper/src/corim_cca.ves`).**
   `corim_parse` and `corim_meta_parse` parse unsigned CoRIMs (tag 501),
   CoMIDs (tag 506), and `corim-meta` headers without building an intermediate
   `Value` AST. `corim_validate_cca` implements all `CcaCorimStore::add` Arm
   CCA platform and realm profile checks, and `cca_appraise` implements
   `appraise_cca_with_options` with constant-time byte and 64-byte padded
   measurement comparisons.
4. **Crypto & dependencies.** `coset = "0.3"` moved out of `[dependencies]` in
   `crates/vesper-aegis/Cargo.toml` into `[dev-dependencies]` (used by
   `tests/cbor_differential.rs` and `tests/attestation.rs`).
   `ec_public_key_from_pem` moved to `src/chain.rs` (re-exported in
   `src/corim.rs`), keeping SPKI/PKIX parsing and `ring` ECDSA signature
   verification (`EcPublicKey::verify_fixed`, `verify_chain`) in Rust.
   1,527 lines of Rust were deleted across `src/cca.rs` (344 deleted, 693 left)
   and `src/corim.rs` (1,183 deleted, 460 left), with `src/port.rs` expanded
   to 1,696 lines for tables, `PortCborValue`, marshaling, and `AtErr`
   mapping.
5. **Differential & divergence.** `tests/port_differential.rs` now covers 14
   tests: `cca_tokens_agree` projects `CcaToken` across `CoseSign1` (70,035
   cases), plus five new Part 2b suites — `cose_sign1_agrees` (75,050),
   `cose_keys_agree` (22,515), `signed_corims_agree` (9,012),
   `corim_store_agrees` (86), and `cca_appraisal_agrees` (44) — all matching
   recorded digests from `0f5a9eba809fd81ad7050d7d083a0e729ce009d4` with 0
   differences while all 8 earlier digests stay unchanged. Three new
   `DIVERGENT` checks (`cose`, `corim`, `corim_cca`) join `build.rs`,
   `BUILD.bazel`, and `port/check.sh`.
6. **ABBA benchmark** (`examples/abba.rs`, `flip-gate.toml`, `bench-abba.sh`,
   `RUSTFLAGS="-C target-cpu=native"`, `n = 30` epochs against `0f5a9eba`):

```
cca_token:    0.37x [0.35x, 0.40x] n=30 decision=pass
corim:        0.58x [0.54x, 0.61x] n=30 decision=pass
corim_store:  1.12x [1.03x, 1.17x] n=30 decision=pass
appraise_cca: 0.55x [0.52x, 0.56x] n=30 decision=pass
```

## Part 3a: the deprecated Rust-keyed gate is retired

`gate.rs` held two gates. `EnclaveGate` kept its ChaCha20-Poly1305 keys in
the gate itself. It was deprecated in favour of `VaultEnclaveGate` and was
to be kept "until the end of wave six", which ended long ago. This part
deletes it:

- `EnclaveGate` and its `EnclaveError`, `NonceSequence` and `NonceTicket`;
- the `tenant_seal_frame` and `tenant_open_frame` helpers;
- the `nonce || ciphertext || tag` frame (`host::SealedFrame`,
  `CiphertextSlice::from_sealed`);
- the crate's direct `chacha20poly1305` dependency.

`gate.rs` drops from 526 to 251 lines.

The TOCTOU tests (`tests/toctou.rs`) now run on `VaultEnclaveGate`:

- the round trip;
- every error path scrubbing and releasing the grant;
- a rewrite after the snapshot;
- a concurrent hypervisor race, which uses a fresh gate per request so the
  replay window never rejects a frame the race replays.

The allocation test keeps only its Vault-gate case.

`VaultEnclaveGate` stays Rust for now. Its own logic is a few checks
around Rust-owned parts:

- Vault's `AeadOpener` and `AeadSealer`, whose AEAD already runs in Vesper
  over the crypto shim but whose keys live in Rust `SecretSlot`s (lane 3);
- the bounce aperture's grants and ledger (atomics, #1037);
- the caller's `evaluate` callback, and Vesper cannot call back into Rust
  (G5).

Moving only the checks would add a crossing and delete no Rust. The gate
moves when Vault's slots and the aperture are in Vesper.

The manuscript's §33.3 example still spells the egress constructor
`CiphertextSlice::from_sealed`. That is the language-level API, and it is
left for the owner's review.

## Part 4a: measurement hashes and fixed-width ECDSA over the crypto shim

The crate's remaining `ring` calls move to Vesper (`vesper/src/measure.ves`)
over lane 17's crypto shim (`@vesper/crypto`, `vesper-crypto-sys`), so the
library no longer depends on `ring` directly (it stays a dev-dependency for
the test signers, and webpki keeps its own ring backend for X.509 paths):

- `GuestRuntimeClaims::report_data`, `claims::single_extend`;
- `cca::cca_rim_measurement`, `cca::rak_hash`;
- `policy::tls_key_channel_binding`;
- `TdxQuote::expected_qe_report_data`;
- `PinnedCert::der`'s SHA-256 pin check (constant-time compare);
- `EcPublicKey::verify_fixed` (P-256/SHA-256 and P-384/SHA-384, the TDX,
  SGX and CCA COSE signatures);
- AMD's RSASSA-PSS SHA-384 verifier for the VCEK chain.

The Rust functions keep their signatures and lend the output buffer, which
Vesper clears and also uses as the zero padding the formulas hash in, so a
call allocates nothing on the Vesper side.

Differential: `cca_crypto_agrees` (23,860 cases) folds 2,000 random report
data inputs, `single_extend` over three hashes and five pads, RIM and RAK
hashes, fixed-width ECDSA on good and broken keys, messages and signatures,
and 2,000 CBOR mutations each of three committed CCA token fixtures
(`tests/data/cca-crypto/`, P-384 and P-256 platform keys, SHA-256/384/512
realms) through `verify_cca_endorsed`, `cca_vesper_claims` and
`verify_cca_token`. It was recorded from the Rust build at
`fb9909ef037e34ec40ea63899f46780db267966c` and matches with 0 differences.
DIVERGENT `measure` (no extend padding) makes it fail.

ABBA (`measure`, `ecdsa`, against the example built at `fb9909e`):

```
measure: 1.06x [1.01x, 1.09x] n=30 (bootstrap 95% median interval)
ecdsa:   0.99x [0.92x, 1.05x] n=30
```

Both pass the bar (upper bound <= 1.50x, release, `RUSTFLAGS="-C
target-cpu=native"`). The hashes are a few hundred nanoseconds each, so the
crossing shows in `measure`; signature checks are dominated by the curve
arithmetic, which is the same ring code behind the shim.

## What is left

The crate's remaining Rust, by the part that will move it:

| Rust | Lines | Part | Needs |
| --- | --: | --- | --- |
| `cca.rs`, `corim.rs` rest: public types, `verify_sign1` (ECDSA now Vesper, part 4a), `ec_public_key_from_pem`, test encoders, adapters | 1,153 | part 4 | SPKI/PEM parsing in Vesper |
| `gate.rs`: the Vault-keyed gate | 251 | part 3b | Vault `SecretSlot`s in Vesper (lane 3), the aperture (#1037), callbacks (G5) |
| `gpa.rs`, `host.rs`, `linear.rs` | 1,012 | part 3 | atomics in source (#1037) for the leak ledger |
| `intel.rs`, `amd.rs`: collateral, TCB status, CRL floors | 1,655 | part 4 | JSON collateral in Vesper; X.509 and ECDSA behind lane 17's shim |
| `chain.rs`, `roots.rs`: X.509 paths, PKIX PEM keys, pinned roots | 772 | part 4 | lane 17's shim (vetted crypto, never hand-rolled) |
| `claims.rs` rest: claim extraction, register replay (SHA-384) | 258 | parts 2-4 | SHA-2 behind lane 17's shim |
| `snp.rs`, `tdx.rs`, `sgx.rs`: struct definitions, encoders, end-to-end verifiers | 1,400 | parts 2-4 | the verifiers call the crypto above; the structs stay while dependents are Rust |
| `policy.rs` rest, `time.rs`: RA-TLS check (SPKI channel binding, SHA-384) and forwarding | 227 | part 4 | lane 17's shim |
| `port.rs`, `error.rs`, `lib.rs`: adapter, error types | 2,053 | last | dependents in Vesper (`vesper-tools` tee, the root CLI test) |

The census row (`scripts/rust-census.py` STATUS) moves to `partial` with
the parsers and policy flipped once the census script is on `main` (plan
PR #1415).

## Platforms run

Part 2b locally on Windows x86-64 MSVC (Rust 1.100.0): `cargo test --locked -p vesper-aegis`, `port_differential` (14 suites, 0 differences vs `0f5a9eba809fd81ad7050d7d083a0e729ce009d4`), `DIVERGENT` checks (`cbor`, `cca`, `cose`, `corim`, `corim_cca`), ABBA (`bench-abba.sh flip-gate`, `n=30`, `RUSTFLAGS="-C target-cpu=native"`), `cargo +1.100.0 clippy`, `rustfmt --check`, `vesper fmt --check`, and `tools/bazel/sync_build_files.py --check`.
