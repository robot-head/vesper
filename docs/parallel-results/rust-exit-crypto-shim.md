# Rust exit SH6 lane 17: one crypto and TLS shim

Claim #1412. Prompt: `docs/agent-prompts/self-host-sh6/17-crypto-shim.md`.

## Part 1: the shim and `@vesper/crypto`

Base: `main` at `ab965f2a`.

### What changed

- **`crates/vesper-crypto-sys`** (new): the one C-ABI shim over vetted
  cryptography. 33 `vcr_*` calls, each a direct call into one library,
  with no logic beyond length checks:

  | Family | Calls | Library |
  |---|---|---|
  | AEAD (AES-128/256-GCM, ChaCha20-Poly1305) over `aad \|\| msg \|\| tag` in place | `aead_new/free/seal/open` | RustCrypto `aes-gcm`, `chacha20poly1305` |
  | QUIC header protection | `aes_new/free/encrypt_block`, `chacha20_block` | RustCrypto `aes`, `chacha20` (block core, any 32-bit counter) |
  | SHA-256/384/512, running hashes, HMAC, HKDF | `hash_len`, `hash`, `hash_new/clone/update/digest/free`, `hmac`, `hkdf_extract/expand` | RustCrypto `sha2`, `hmac`, `hkdf` |
  | X25519 | `x25519`, `x25519_base` (small-order points refused) | `x25519-dalek` |
  | Ed25519 | `ed25519_public/sign/verify` (strict) | `ed25519-dalek` |
  | ML-KEM-768 | `mlkem768_isa_available/keygen/validate/encaps/decaps` (portable, AVX2, NEON) | `libcrux-ml-kem` |
  | ECDSA P-256/P-384 verification (fixed and ASN.1) | `ecdsa_verify` | `ring` |
| RSA PSS and PKCS #1 v1.5 verification, SHA-256/384/512, 2048-8192 bits (for `vesper-loom-tuf`, lane 16) | `rsa_verify` | `ring` |
  | Randomness, constant-time equality, zeroization | `random`, `ct_eq`, `zeroize` | `getrandom`, `subtle`, `zeroize` |

  The design follows `vesper-cranelift-sys`: three linear handles (`Aead`,
  `Block`, `Hash`) with exactly one finalizer each and live counters for
  leak tests, `(ptr, len)` pairs for `CSlice[u8]`, a status word (0 ok, 1
  length, 2 authentication, 3 library failure, 4 unsupported) with no
  output written on failure, and `catch_unwind` around every call. Same
  dependency versions as the crates it replaces: Cargo.lock gains only
  the new crate, no new package.
- **`packages/vesper-crypto`** (new, `@vesper/crypto`): the `.vsys`
  library `Crypto` (`crypto.vsys`, also the `// @export vsys` block of
  `src/lib.ves`) and typed wrappers that raise `CryptoError` with the
  shim's status. The first protocol logic lives here in Vesper: TLS 1.3
  `HkdfLabel` and `HKDF-Expand-Label` (RFC 8446 §7.1), which Warp's QUIC
  key schedule and the TLS session both need.
- **Tests.** `crates/vesper-crypto-sys/tests/vectors.rs` (16 tests):
  known answers for every call (FIPS 180-4, FIPS 197, the GCM spec, RFC
  4231, 5869, 7748, 8032, 8439, 9001), ML-KEM on every backend this CPU
  runs (identical bytes across backends, implicit rejection, the modulus
  check), ECDSA and RSA against ring signatures, the status and no-write rules,
  leak counts, and `symbols()` against `crypto.vsys`.
  `tests/crypto_shim.rs` (2 tests): a driver package that depends on
  `@vesper/crypto` runs every wrapper in the JIT bound to the shim on the
  same vectors, including RFC 9001 §A.1 through the Vesper
  `HKDF-Expand-Label`, checks that failures raise the right status and
  write nothing, that no handle leaks, and that `crypto.vsys` equals the
  export block.

### Shim size (the crypto share of the irreducible minimum)

| | Lines | Non-comment, non-blank |
|---|--:|--:|
| `crates/vesper-crypto-sys/src/lib.rs` | 1,137 | 888 |

That is inside the plan's 1-2k estimate. TLS 1.3 (rustls) and X.509 path
validation (webpki) are not in the shim yet; they come with the
`vesper-warp-tls` and Aegis parts and will add a few hundred lines.

### Flip-gate verdicts

None yet: part 1 deletes no Rust. The four crates flip in the parts below.

### Note for the owner: a C library instead of the Rust libraries

The Rust libraries could all be replaced by one C library, which would
remove this shim from the irreducible minimum (Vesper would import the C
API through `.vsys` directly, as the C-API lanes do). The tree already
builds one: `rustls` is configured with its `aws_lc_rs` provider, so
**aws-lc** (AWS's BoringSSL fork, C) is compiled and linked today. Its
libcrypto covers every family above (AES-GCM, ChaCha20-Poly1305, SHA-2,
HMAC, HKDF, X25519, Ed25519, ECDSA, ML-KEM-768 and FIPS-validated
builds), and its libssl has TLS 1.3 with the QUIC API
(`SSL_set_quic_method`) and X.509 path validation. The costs: a C build
of aws-lc on every platform (CMake, plus NASM on Windows for the
assembly), libcrux's formally verified ML-KEM replaced by aws-lc's, and
the libssl state machine in place of rustls.

**Owner decision (2026-11-21):** keep the small Rust shim ("crypto is
hard"); any replacement of the vetted libraries happens last, with an
expert. This lane does not pursue the C route.

### Compiler gaps

- A package that names its `.vsys` in `Loom.toml` (`vsys = [...]`) cannot
  itself be built with `vesper build --lib`: the library's own
  `// @export vsys` block and the manifest entry are both imported
  ("library Crypto is imported twice"). `@vesper/cranelift` has the same
  behaviour. Repro: `vesper build --lib --lib-name c --target
  x86_64-unknown-linux-gnu packages/vesper-crypto -o c.o`. Not blocking:
  the package is only ever a dependency.

## Part 2: BLAKE3 in C (PR #1462)

The official BLAKE3 1.8.7 C implementation (`packages/vesper-std/hash/sys/c/`)
replaces the Rust hash-sys shim (`shim.rs`, deleted), built by the new
`crates/vesper-blake3-sys` (`cc`, one archive of per-ISA objects, every
internal symbol renamed so it never meets the `blake3` crate's assembly).
`tests/c_matches_crate.rs` checks it against the crate. This is the shared C
package and C build support lanes 12 and 16 asked for (#1421).

## Part 3: vesper-warp-crypto on Vesper

### What changed

- **`crates/vesper-warp-crypto/vesper/`** (`@vesper/warp-crypto`, depends on
  `@vesper/crypto`): every RFC 9001 rule of the crate. The §5.1/§5.2 key
  schedule and §6.1 "quic ku", the §5.3 nonce, the bounds of every offset,
  the §5.4 masks and their application, the sealer's ticket rules and §6.6
  confidentiality limit, the opener's integrity limit, the §6 key-update
  state machine (key choice by phase bit and packet number, promotion,
  retirement, the peer-initiated response) and the §5.8 Retry tag. Export
  ABI 4: the AEAD and AES keys are `vesper-crypto-sys` values the Rust
  structs hold inline and lend to each call (`Aead::new`/`Block::new`, new
  safe constructors in the shim, which its C calls now use too). The packet
  path allocates nothing: each call borrows a 64-byte stack scratch buffer
  for the nonce, the sample and the HKDF labels (`tests/zero_alloc.rs`
  still passes, key updates included).
- **The Rust crate** keeps the public API unchanged (vesper-warp, warp-tls,
  vault and chorus build and pass as they are) and only what the ABI cannot
  carry: the key values, the linear `NonceTicket` with its drop accounting
  and sealer ids, the packet-number reconstruction closure (`open_packet`
  is split around it), the swaps of keys on an update, and error
  conversion. Its RustCrypto dependencies are gone (dev-only for the
  reference). Non-comment Rust lines: 1,122 before, 939 now; the façade
  goes when its Rust callers are ported.
- **`crates/vesper-port-harness`**: a divergent (edited) build now copies the
  `.vsys` files a package manifest names, so ports over `@vesper/crypto`
  can run divergence checks.

### Flip-gate verdict (hand-run recipe; `scripts/ci/flip_gate.py` is not on main yet)

| Workload | Differential | Every test | ABBA (Vesper/Rust, 95% interval) | Verdict |
|---|---|---|---|---|
| `one_rtt`: 512 round trips of 1,200-byte 1-RTT packets per suite, a client key update every 64 | 0 differences | all crate tests, plus vesper-warp, warp-tls, vault and chorus | **1.04x** (1.028x..1.058x) | flipped |
| `initial`: 64 fresh Initial key schedules with one packet each way | 0 differences | | **1.07x** (1.069x..1.085x) | flipped |
| `header`: header-protection removal and re-application alone, 64-byte packet | 0 differences | | 1.89x (1.875x..1.914x) | over the bar (see below) |

- **Differential**: `src/differential.rs` keeps the deleted Rust modules as
  `src/reference/` (test-only) and runs both on seeded cases: secrets of
  every length, connection IDs up to 24 bytes, packet numbers at the 2^62
  edge, offsets and payload lengths near `usize::MAX`, tampered bytes,
  wrong and declined packet-number reconstructions, foreign and stale
  tickets, small confidentiality and integrity limits, reordered packets
  across key phases and retirement windows, and Retry packets cut short.
  Each case compares results, errors, buffers and state (audits, failure
  counters, generations), and the tests assert that every error path
  occurred.
- **Divergence checks**: six edits (`schedule`, `nonce`, `header`,
  `sealer`, `update`, `retry`) in `build.rs` and `BUILD.bazel`;
  `port/check.sh` asserts that each one fails its differential test while
  another passes.
- **ABBA**: `examples/abba.rs`, 12 epochs of A/B/B/A on a 4 vCPU cloud
  container, release builds. The Rust arm is this branch's base with the
  same example file.
- **The header micro-benchmark** is 50 ns against 95 ns per removal and
  re-application. Callgrind puts it in codegen, not the shim: the Vesper
  `hp_remove` runs about 180 instructions where the Rust one runs about 90
  (`Option` results materialized on the stack, slice headers reloaded on
  every access). That is the codegen-size thread's work (#1426). On every
  workload that seals or opens a packet the AEAD dominates and the port is
  within 1.07x.

### Compiler gaps

- A `handle Fail[E] in { Result::Ok(f()) } with { raise(e, _) =>
  Result::Err(e) }` whose result is then matched, with `Fail::raise(e)` in
  the `Err` arm, fails in codegen: "native layout is not yet defined for
  `_`". Worked around with a `bool` helper.
- Header-protection code is about 2x Rust's instruction count (above).

### What is left

1. `vesper-warp-crypto`: done (part 3), except the header micro-benchmark,
   which waits on codegen.
2. `vesper-loom-trust`: the attestation quorum and slopsquat checks to
   Vesper over `ed25519_verify` (compiler-linked: seed pattern until lane 3
   or the retirement).
3. `vesper-vault`: AEAD slots, the hybrid X25519 + ML-KEM-768 KEM and the
   secret slots to Vesper (compiler-linked: seed pattern). Its
   constant-time verifier over Cranelift IR is compiler code, not crypto,
   and goes with lane 3's verifier move.
4. `vesper-warp-tls`: a TLS 1.3 session behind the shim (rustls's QUIC
   API as linear handles) and the session layer (transport parameters,
   reassembly, configuration) in Vesper.
5. The shared BLAKE3 C package: done (part 2, PR #1462).
6. The census row in `scripts/rust-census.py` once plan PR #1415 lands.
