# Wave five, lane 1: Vesper Vault (§23)

Claim: #121. Branch `claude/project-thread-uriigf`, based on `main` at
`af03122ea74d403f203170926945f9d35786b289`. New crate `crates/vesper-vault`,
new proof file `lean/VaultConstantTime.lean`. No internal dependencies, no
dependency on the root `vesper` package, `vesper-warp-crypto` untouched.

## What shipped

| §23 item | Where | Evidence |
| --- | --- | --- |
| 23.5 constant-time SSA taint verifier over real Cranelift IR | `src/verifier.rs` | `tests/verifier.rs`, 19 tests over `cranelift-frontend` functions that also pass `cranelift_codegen::verify_function` |
| 23.2 secret scalars `SecU8/SecU32/SecU64`, `SecMask64`, `SecVec`, `verify_tag_16_ct` | `src/secret.rs` | `tests/secret.rs`; compile-fail doctests for `==` and `/` |
| 23.1.2 `SecretSlot[K]` (page-aligned, `mlock`, `MADV_DONTDUMP`, closure projections, `shred`) | `src/slot.rs`, gated `src/sys.rs` | `tests/slot.rs`, including an `RLIMIT_MEMLOCK=0` child process |
| 23.3 linear `AeadSealer`/`NonceTicket`, burn, exhaustion, rotation, Doom-principle `open_in_place` | `src/aead.rs` | `tests/aead.rs`, `tests/alloc.rs`, compile-fail doctests |
| 23.1.4 hybrid X25519MLKEM768 | `src/kem.rs` | `tests/kem.rs` with RFC 7748 and the published ML-KEM-768 accumulated vectors |
| 23.1.3 `ConstTime::scope` with AArch64 `PSTATE.DIT` | `src/consttime.rs`, gated `src/sys.rs` | `tests/consttime.rs` (exercised on the macOS AArch64 CI runner) |
| 23.4 Lean proofs | `lean/VaultConstantTime.lean` | `tests/lean.rs` runs `lean` under `VESPER_LEAN` |
| Linear-obligation leak auditing | `src/obligation.rs` | leak tests in `slot.rs`, `aead.rs`, `kem.rs` |

### Public API

- `verify_constant_time(&Function, &CtPolicy) -> Vec<VerifierFinding>`, with
  `VerifierFinding { function, inst, rule, message }` exactly as in the
  wave-five contract (`inst` is `Inst::as_u32`). `CtPolicy` names
  `secret_params`, `secret_pointer_params`, `secret_regions` (Cranelift
  `MemFlags` alias regions), `declassify` callees (default
  `DECLASSIFY_MARKER = "%vesper_ct_declassify"`) and trusted
  `constant_time_callees`.
- `SecU8`, `SecU32`, `SecU64` (`new`, `declassify`, bitwise ops, wrapping
  add/sub/mul, public-amount shifts and rotates, `ct_is_zero`, `ct_eq`,
  `ct_lt`, `ct_select`), `SecMask64` (`declassify_bool`), `SecVec<T, N>`,
  `verify_tag_16_ct`.
- `SecretSlot<K>` (`new`, `from_fn`, `from_bytes`, `with`, `with_mut`,
  `protection`, `shred -> ShredReceipt`), `LockPolicy::{Require, BestEffort}`,
  `SlotError`, `SlotProtection`, `Protection::{Applied, Failed, Unavailable}`.
- `AeadSealer<A>` (`new`, `with_record_limit`, `with_start`, `mint_nonce`,
  `seal_in_place`, `rotate`, `retire`), `NonceTicket<A>` (`epoch`, `seq`,
  `burn`), `AeadOpener<A>` (`open_in_place`, `retire`), `CiphertextFrame`
  (`new`, `discard`), `VerifiedPlaintext`, `FrameHeader`, `AuthError`,
  `CryptoError`, `SealError`, `RotateError`, algorithms `Aes256Gcm` and
  `ChaCha20Poly1305`.
- `HybridClientSecret` (`generate`, `decapsulate`, `abandon`),
  `server_encapsulate`, deterministic `*_for_testing` variants, `SystemRng`
  (`getrandom` as a `rand_core` 0.10 fallible CSPRNG), `KemError`.
- `ConstTime::{scope, probe, dit_flag}`, `HardwareCtMode`.
- `LeakAuditor::{global, this_thread}`, `LeakReport`, `ObligationKind`.

## Verifier rules and analysis

The verifier computes a monotone forward fixpoint over the whole function,
so taint that reaches a block parameter only along a loop back edge is
found (`secret_reaches_branch_only_through_loop_back_edge`). Rules, each
with a rejection test that checks the instruction index:

| rule | rejected |
| --- | --- |
| `secret-branch` | `brif` on a secret |
| `secret-branch-table` | `br_table` on a secret index |
| `secret-load-address` | load address derived from a secret (including the classic S-box lookup indexed by a byte loaded from secret memory) |
| `secret-store-address` | store address derived from a secret |
| `variable-latency-div` | `udiv`, `sdiv`, `urem`, `srem` with a secret numerator or denominator |
| `secret-call-unknown` | a secret, or a pointer into secret memory, passed to a callee the policy does not trust (direct or indirect) |
| `secret-call-target` | `call_indirect` through a secret address |
| `secret-trap` | other trapping instructions (`trapz`, `trapnz`, overflow traps, trapping float conversions) on a secret |
| `unsupported-secret-operand` | memory formats it does not model (dynamic stack slots) receiving a secret |
| `invalid-policy` | policy names a parameter the function lacks (a finding, never a panic) |

Accepted: bitwise ops, add/sub/mul, shifts and rotates, `select` and
`bitselect` on secret masks, secret values stored to public addresses, and
branches on the result of the declassify marker. Memory is tracked per value
as "points into a secret buffer / a stack slot / untracked memory", and
flow-insensitively per region: a secret stored to a stack slot or to
untracked memory makes later loads from it secret, and any pointer that may
reach such memory counts as exposing a secret when passed to a callee. Any
callee (trusted or not) that receives a secret is assumed to write secrets
through every pointer argument, because callees have no memory-effect
summaries (review findings on PR #150).

## Measurements and environment

Measured in a Linux x86_64 cloud container (4 vCPU, Rust 1.97.0, running as
root) and by CI on GitHub's Linux x86_64, macOS AArch64 and Windows x86_64
runners.

- Zero heap allocations over 1000 warmed-up seal+open round trips plus 100
  tamper/zeroize failures of 1200-byte payloads, for both AES-256-GCM and
  ChaCha20-Poly1305 (`tests/alloc.rs`, counting global allocator, one test
  per binary). Slot creation uses `mmap`/`VirtualAlloc`, not the heap, and
  is outside the hot path.
- `mlock` and `MADV_DONTDUMP` were applied in the container; slots were
  page-aligned (4096-byte pages). The container runs as root, which ignores
  `RLIMIT_MEMLOCK`, so there the `RLIMIT_MEMLOCK=0` child test observed a
  successful lock and asserted it was running with euid 0. On an
  unprivileged runner the same test requires `SlotError::MlockFailed`.
- DIT: the container is x86_64, where `ConstTime::scope` reports DOITM as
  unavailable (it needs a kernel write to `IA32_UARCH_MISC_CTL`). The DIT
  path compiles and assembles for `aarch64-apple-darwin` and
  `aarch64-unknown-linux-gnu`; whether CI's Apple-silicon runner reports
  FEAT_DIT, and the observed set/restore, is in that job's
  `scope_reports_and_restores_hardware_mode` output.
- ML-KEM-768 matched the published accumulated digest for 100 iterations in
  every test run and for 10000 iterations in a release run
  (`VAULT_MLKEM_ACCUMULATED=10000`). Since #486 this runs through Vault's
  own entry points once per available backend (libcrux portable and AVX2 on
  the x86_64 container; portable and NEON on the AArch64 macOS runner), and
  separately through RustCrypto `ml-kem` as an independent reference.

No timing numbers are claimed.

## Lean

`lean/VaultConstantTime.lean` compiles with Lean 4.24.0 and contains no
`sorry`, `admit`, new axioms or `native_decide`.

1. `VaultSpec.verify_tag_16_ct_sound`: for all 128-bit `expected` and
   `computed`, the modelled branchless comparison (xor halves, or, then
   `((x ||| -x) >>> 63) == 0`) equals `decide (expected = computed)`. Proved
   with `bv_decide`, which relies on Lean's standard `Lean.ofReduceBool`
   trust in the compiled SAT-certificate checker.
2. `VaultCt.trace_independent`: in a small register machine whose
   `LeakageTrace` records the pc of every step, every memory address, both
   operands of every divide and every branch outcome, a program accepted by
   `WellTyped` (the verifier's rules: no secret branch, address or divide
   operand; taint through ALU and select) yields the same trace from any two
   states that agree on public registers and public memory.
   `tagKernel_ct` instantiates it for the tag comparison written on that
   machine. It uses only `propext` and `Quot.sound`.

These are theorems about the Lean models. They are not theorems about the
Rust code, the Cranelift IR, or machine code, and the machine model excludes
declassification.

## Decisions and corrections to the design text

- §23.5 sketches a single linear pass over an abstracted instruction list.
  That pass misses taint arriving through loop back edges, so the verifier
  iterates to a fixpoint over real Cranelift IR; the sketched enum is not
  used.
- §23.3 rejects at `next_seq == 0xFFFF_FFFF_FFFF_FFFE`. The sealer rejects at
  `next_seq >= limit` with `limit <= 0xFFFF_FFFF_FFFF_FFFE`, which also covers
  counters resumed above the bound, and applies AES-GCM's lower RFC 8446
  §5.5 record bound (2^24.5 records). ChaCha20-Poly1305 uses the §23.3 bound.
- The 28-byte `EncryptedFrameWireHeader` (4 + 8 + 16) is internally
  consistent and kept. The 96-bit nonce is `key_epoch (BE) || nonce_seq (BE)`;
  both fields are bound through the nonce, so tampering with either fails
  authentication.
- Tickets carry their sealer's identity; a ticket presented to another
  sealer is handed back (`ForeignTicket`). Failed seals return the ticket
  unless the cipher saw its nonce.
- The §23.4 `chacha20_seal_constant_time_trace` statement (proved by `simp`
  over an undefined `LeakageTrace`) is not meaningful as written. It is
  replaced by a general non-interference theorem with an explicit trace
  semantics, instantiated for the tag kernel. ChaCha20 itself is not
  modelled.
- RustCrypto's AEADs check the tag before decrypting, so on failure the
  buffer holds ciphertext rather than tentative plaintext. `open_in_place`
  zeroizes the whole frame anyway, as §23.3 requires, and uses the crates'
  `subtle`-based tag check rather than `verify_tag_16_ct`.
- The expansion-37 review said no cryptography was claimed for §23; this lane
  implements it with mature crates (RustCrypto `aes-gcm`, `chacha20poly1305`,
  `ml-kem` 0.3.4; `x25519-dalek` 3.0.0). No primitive is hand-rolled.
  #486 later moved ML-KEM-768 to `libcrux-ml-kem` (below).

## Vector NTT for ML-KEM-768 (#486)

The design's `eff [Isa[P]]` polynomial NTT was left out of wave eight
because it would have meant hand-rolling a primitive. The owner chose to
build it on vetted libraries, so Vault now delegates ML-KEM-768 to Cryspen
`libcrux-ml-kem` 0.0.11 (no `std`, `alloc` or `rand`), whose portable, AVX2
and NEON backends are formally verified with hax and F* (panic freedom,
functional correctness against FIPS 203, secret independence) and which
ships in NSS. Vault implements none of the arithmetic.

- `KemIsa` (`Portable`, `Avx2`, `Neon`) is the ISA witness. `detect()` picks
  the fastest backend the CPU supports (`is_x86_feature_detected!("avx2")`,
  `is_aarch64_feature_detected!("neon")`); `HybridClientSecret::generate_on`,
  `from_seeds_for_testing_on`, `server_encapsulate_on` and
  `server_encapsulate_for_testing_on` pin one, and the client secret keeps
  its backend for decapsulation (`HybridClientSecret::isa`). A backend the
  CPU lacks, or one not compiled for the architecture, is refused with
  `KemError::IsaUnavailable` rather than run, and caller seeds are still
  wiped.
- The `simd256` feature is enabled only on `x86_64` and `simd128` only on
  `aarch64` (each compiles against that architecture's intrinsics). Vault
  still has no `unsafe`; the intrinsics calls are inside libcrux.
- The expanded 2400-byte decapsulation key is wiped on drop (`WipedKey`);
  libcrux types do not zeroize themselves.
- Server-side FIPS 203 encapsulation-key validation is now an explicit
  `validate_public_key` call on the selected backend, since libcrux's
  `encapsulate` does not validate.
- Tests: the accumulated vectors on every available backend, byte-for-byte
  agreement of every backend with RustCrypto `ml-kem` (now a
  dev-dependency), hybrid exchanges across every client/server backend pair,
  and the refusal path. The crate checks and passes clippy for
  `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and
  `x86_64-pc-windows-msvc`.
- No timing numbers are claimed.

## Root patch

One commit adds `crates/vesper-vault` to `members` and `default-members`
(before `experiments/native-pulse`), adds `[workspace.dependencies]` entries
for `cranelift-codegen`, `cranelift-frontend`, `getrandom`, `libc`, `ml-kem`,
`rand_core`, `region`, `shake` and `x25519-dalek`, and regenerates
`Cargo.lock` (new packages: `ml-kem` 0.3.4, `kem` 0.3.0, `module-lattice`
0.2.4, `hybrid-array` 0.4.15, `sha3` 0.12.0, `shake` 0.1.0, `keccak` 0.2.2,
`sponge-cursor` 0.1.0, `digest` 0.11.3, `crypto-common` 0.2.2, `ctutils`
0.4.8, `cmov` 0.5.4, `x25519-dalek` 3.0.0, `curve25519-dalek` 5.0.0 and
`fiat-crypto` 0.3.0). The root `vesper` package keeps its own direct
Cranelift entries.

## Remaining gaps

- Linear ownership here is Rust host-library discipline with counted leaks;
  it is not the Vesper source typing that makes nonce reuse or a missed
  `shred` a compile-time error. The `eff [ConstTime]` source effect and the
  Stage 5 hook in `src/native.rs` belong to integration B (#136).
- The verifier says nothing about machine code: whether `select` lowers to
  `cmov`/`csel`, register allocation, or callees the policy trusts. It treats
  multiplication and variable shifts as constant time. Pointers reloaded
  from memory lose their provenance.
- No register scrubbing after `shred` (no portable way in Rust). The AEAD key
  schedule is rebuilt on the stack per call and zeroized on drop by the
  RustCrypto types; those stack frames are not in locked memory.
- Secret scalars rely on `core::hint::black_box`, not a guarantee from LLVM.
- The ML-KEM vector kernels are libcrux's Rust intrinsics, selected at run
  time; they are not lowered from Vesper source `Vector` code, and the
  source `eff [Isa[P]]` effect does not yet type-check calls into them.
  Stack copies made inside libcrux (seed, intermediate polynomials) are not
  wiped by Vault. The draft-ietf-tls-ecdhe-mlkem text publishes no hybrid test
  vectors; the components are checked against RFC 7748 and the C2SP/Go
  accumulated ML-KEM-768 vectors, and the hybrid against round-trip,
  combiner-layout and tamper tests.
- x86_64 DOITM is always unavailable from user space. `MADV_DONTDUMP` is
  unavailable outside Linux and Android.
