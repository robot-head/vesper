# Port wave P3a lane 8: Aegis quote and report parsing (#1043)

The structure parsers of `crates/vesper-aegis` (AMD SEV-SNP reports, Intel
TDX quotes v4 and v5, Intel SGX quotes v3), the 128-byte claims wire
struct, the collateral timestamp parser and the relying-party policy check
run in Vesper in the port-backed package `vesper-aegis`, from the Loom
package `@vesper/aegis` in `crates/vesper-aegis/vesper/`. Every test file
passes against both builds. Signatures, certificate chains, X.509, COSE,
AEAD and the CoRIM/CCA CBOR code stay Rust. The Rust build stays the
default for every dependent (the owner's standing rule: no switch and no
deletion until the port matches Rust in function and performance).

Baseline: `main` at `3c075a4`.

## Packages (seed-copy rule)

Aegis is linked by the compiler (`src/tee_commands.rs` uses
`vesper_aegis::claims`), so it follows Beacon's seed-copy rule:

- `crates/vesper-aegis/seed` is the package `vesper-aegis-seed`, the Rust
  build of `../src` under the library name `vesper_aegis`, with no build
  script. The root depends on it by rename (`vesper-aegis = { path =
  "crates/vesper-aegis/seed", package = "vesper-aegis-seed" }`). No other
  crate depends on Aegis, so every dependent links Rust.
- `crates/vesper-aegis` is the port-backed package: `build.rs` builds the
  Loom package with `vesper_port_harness::Port`, links it, checks
  `port/binding.rs` and sets `cfg(vesper_port)`. Each ported Rust function
  is `#[cfg(not(vesper_port))]`, followed by a `#[cfg(vesper_port)]`
  function with the same signature that forwards to the port. The Rust
  bodies are unchanged.
- The test files read `tests/data/vendor` through `support::crate_dir()`
  (one level up when built by the seed, whose manifest directory is
  `seed/`). That is the only change to the existing tests.

## What was ported

| Rust | Vesper module | Notes |
| --- | --- | --- |
| `reader.rs`, `ParseError` constructors in `error.rs` | `reader.ves` | Bounded windows over one input slice; truncation, reserved, trailing and tail rules. |
| `SnpReport::parse`, `SnpEtcb::parse` (`snp.rs`) | `snp.ves` | Versions 2-6, Venice ETCBs, signature byte reversal. |
| `TdxQuote::parse`, `parse_padded`, `parse_prefix`, the header, TD 1.0/1.5/1.5-ex bodies and `QeReportBody` (`tdx.rs`) | `tdx.ves` | Body descriptor, signature data, nested certification data. |
| `SgxQuote::parse`, `parse_padded`, `parse_prefix`, `SgxReportBody` (`sgx.rs`) | `sgx.ves` | Shares the QE report parser with TDX. |
| `AegisAttestationClaimsWire::decode` (`claims.rs`) | `claims.ves` | |
| `verify_attestation_claims` (`policy.rs`) | `claims.ves` | Same check order; constant-time byte comparison (XOR/OR fold, no early exit). |
| `iso8601_utc_to_unix` (`time.rs`) | `time.ves` | |

**Boundary.** A parser fills two sequences, `w` (every integer field, in
reading order) and `d` (every byte array, then variable-length fields
whose lengths are in `w`, in reading order). `src/port.rs` reads them back
with a cursor (`Fields`), so the Rust side holds no offsets or sizes of
its own. A failure is one `AErr { kind, format, field, a, b }`: the
`ParseError` variant, the `Format`, an index into the field-name table
`FIELDS` (129 names) and the variant's numbers. The policy check takes the
claims and the policy as word and byte slices (layout documented at
`check_policy`) and fails with `PErr { kind, a, b, c }`, the
`AegisVerifyError` variant and its payload.

## What stays Rust, and why

| File or item | Why | Unblocked by |
| --- | --- | --- |
| `chain.rs`, `roots.rs`, `intel.rs`, `amd.rs` | ECDSA, webpki paths, X.509 and CRL parsing, pinned roots: vetted crypto (`ring`, `webpki`, `x509-parser`). | Stays Rust (survey class). |
| `ring` digests in `tdx.rs`, `claims.rs`, `policy.rs` | SHA-256/384 (QE report data, register replay, channel binding). | Stays Rust. |
| `gate.rs` | ChaCha20-Poly1305 AEAD, plus `&mut dyn FnMut` evaluators/observers (10 of the 11 `dyn` sites). | Part 2: port the frame/ring logic, return request bytes, consume the reply (`dyn` stays in the Rust adapter). |
| `chain.rs` `CHAIN_ALGS` (`&dyn SignatureVerificationAlgorithm`, the 11th site) | A static table of webpki algorithms. | Stays Rust. |
| `cca.rs`, `corim.rs` | CBOR/COSE token and CoRIM structure (2.4k lines). | Part 2 (CBOR reader in Vesper; COSE signatures stay Rust). |
| `claims.rs` other items, `gpa.rs`, `host.rs` | Not parsing; claim extraction depends on CCA. | Part 2. |
| `linear.rs` | It keeps a global `AtomicU64` for issuer identities and an `Arc<Ledger>` with atomic leak counters, updated on `Obligation::drop`, which `gpa.rs` checks. | Stays Rust until the source-atomics lane (#1037) lands. A `gpa.rs` port takes issuer identities as values. |
| Encoders (`encode`, `signed_bytes`) | Serialize trusted structs; no untrusted input. | Part 2 if wanted. |
| `sgx.rs` TCB info JSON (`serde_json`) | JSON collateral. | Serde port lane. |

## Tests

- In-crate unit tests: one (`time::tests::known_dates`); it calls the
  public function, so it runs against both builds unchanged.
- Integration tests: 15 files, 93 tests, all pass against both packages
  (`cargo test -p vesper-aegis -p vesper-aegis-seed`), including
  `parser_fuzz.rs`, the vendor samples and `zero_alloc.rs` (the gate is
  Rust, so the zero-allocation property is unchanged).
- New: `tests/port_differential.rs` runs every ported parser on the vendor
  samples and 4,000 mutations of each through the port and the seed and
  requires the same `Ok`/`Err` value (Debug-equal); 200,000 random
  claims/policy pairs give the same decision and reach every rejection.
  `tests/port_memory.rs` checks that the port frees every sequence on
  success and on every error path.

## Divergence checks

One wrong line per ported module (`DIVERGENT` in `build.rs`, picked by
`VESPER_AEGIS_DIVERGE`; Bazel builds the same objects,
`:aegis_vesper_divergent_*`). `port/check.sh`, run by
`scripts/test-port-harness.sh` (Linux shard 3), asserts that each fails the
named test while an unrelated test in the same file passes:

| Module | Edit | Fails |
| --- | --- | --- |
| reader | a reserved word whose first byte is 1 reads as zero | `parser_fuzz` round trips (TDX, SNP), plus 8 vendor-sample tests |
| snp | report version 7 accepted | `snp_report_versions_and_reserved_fields`, `v6_versions_and_mutations` |
| tdx | TD 1.5 "ex" body size 884 instead of 885 | **the vendor sample** `real_quote_v5_td15_ex_verifies_end_to_end` (dcap-qvl 0.7 `tdx_quote_td15ex`), plus `tdx_quotes_agree` |
| sgx | PCK chain must be certification type 6 | every real SGX quote test in `sgx_quote_v3` |
| wire | CCA architecture tag refused | `claims_wire_agrees` |
| policy | TCB SVN equal to the floor is stale | `policy` (all three), `attestation` (four), `policy_decisions_agree` |
| time | a fraction-less timestamp must end in `Y` | `known_dates`, `timestamps_agree`, the SGX/TDX collateral tests |

## Performance

`examples/abba.rs` links both builds into one binary (the port-backed
package plus the seed as `vesper_aegis_rust`) and runs 9 epochs of ABBA
windows per workload on the vendor samples: `snp` parses four AMD reports,
`tdx` four TDX quotes (v4 and v5, including TD 1.5 "ex"), `sgx` two SGX
quotes, `policy` four passing `verify_attestation_claims` calls (SNP claims,
three architectures, 16 measurements). Host: 4 vCPU Intel Xeon @ 2.10 GHz
cloud container, Linux x86-64, Rust 1.97 `--release`, port built by the
in-tree compiler (optimized object). Instructions are callgrind per pass,
with the 0-pass setup subtracted.

| Workload | Port/Rust wall, median (min-max) | Rust instr./pass | Port instr./pass | Instr. ratio |
| --- | --- | --: | --: | --: |
| snp | **2.10x** (1.64-2.42) | 17,242 | 35,063 | 2.03x |
| tdx | **2.33x** (2.29-2.96) | 18,764 | 42,780 | 2.28x |
| sgx | **2.47x** (2.39-2.48) | 10,483 | 23,200 | 2.21x |
| policy | **1.37x** (1.32-1.66) | 44,242 | 105,062 | 2.37x |

Not at parity, so Rust stays the default and nothing is deleted. Before two
port-side changes (below) the parsers were 4.3x / 3.6x / 3.8x wall (93,199 /
67,439 / 40,392 instructions per pass): writing the 96 reversed signature
bytes as six byte-swapped words instead of 96 `push` calls, reserving the
output sequences, and checking reserved fields eight bytes at a time
instead of with `find_not_in` halved them.

## Compiler gaps

Each with a repro; none is fixed here (no compiler edits in a port lane).

1. **`find_not_in` is slow on this host.** `fn z(in s: Slice[u8]) -> bool
   { match s.find_not_in(1, 0, false) { Option::Some(i) => false,
   Option::None => true } }` over the SNP report's ~600 reserved bytes cost
   more than the rest of the parse: replacing it with a `try_read_u64_le`
   OR-fold took SNP from 69,076 to 35,063 instructions per pass. The byte
   classes lane noted it needs SSSE3 on x86; the fallback should at least
   be word-at-a-time, and a dedicated "all bytes zero" builtin would serve
   every reserved-field check.
2. **Byte-at-a-time `push` grows and checks per element.** `for i in 0..48
   { d.push(..) }` (reversing a signature component) was most of the
   first 25% saved; a reversed copy (`extend_reversed`) or `bswap` builtin
   would remove the workaround (`extend` then six `try_write_u64_be`).
3. **Boundary copies.** Each parse allocates two Vesper sequences, copies
   them back into Rust `Vec`s (`with_seq_mut`), and the adapter copies the
   arrays a third time into the structs: about a quarter of the remaining
   port instructions are malloc/free/memcpy. Returning `[u8; N]` fields
   in a v3 mirror (fixed-size byte arrays in copy structs) would let the
   port write the structs in place.
4. **An imported function named like a scalar type shadows the type.**
   `use self::reader::{u64}` makes every `u64` in the importing module mean
   the function: `expected ChunkSeq[@vesper/aegis::reader::u64], found
   ChunkSeq[u64]` (E0200). The reader functions are `rd_u16/rd_u32/rd_u64`
   instead.
5. **`?` works on `Result` only.** `num(in s, 0, 4)?` in an
   `Option`-returning function is E0200 "'?' requires Result[T, E]"; the
   timestamp parser uses a sentinel instead.
6. **Slice views cannot be method receivers.** `b[a..c].find_not_in(..)`
   is E1521; a helper taking `in s: Slice[u8]` is needed.
7. **Shifts by a guarded amount still raise.** `(x >> wrapping_mul(i, 8))`
   with `i < 8` checked needs a `Fail[ArithOverflow]` handler (known
   guard-range totality gap).

Std candidates (private helpers in this package): `all_zero`,
constant-time `ct_eq`, the bounded reader window (`Rd`, `take`, `rd_u*`).

## Platforms run

Locally: Linux x86-64 (Cargo, Rust 1.97, and `cargo +1.100.0 clippy
--all-targets -D warnings`). CI: the four required checks.
