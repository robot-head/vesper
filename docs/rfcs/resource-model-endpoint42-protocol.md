# Strict resource-model wire protocol (#480)

`src/bin/resource_oracle42/protocol.rs` implements the fixed version-one array
schema from the endpoint contract. It is self-contained and imports no process
module. Public typed request and response DTOs cover every constructor in the
existing `TypeError`, `ValidationError`, `CheckErrorKind`, `CheckProfile`,
`InputError`, `InvocationError`, `CapacityError`, `ResourceRefusal` and
`InvariantError` declarations, all six stages and all four evaluation statuses.
The codecs preserve declaration-order payloads, optional-slot arrays, checked
information, invocation statistics, chronological events, live residual rows,
tombstoned ledger rows and the independent transition count. Extra payloads,
unknown constructors and execution fields on refusal stages are wire errors.
Internal `serde_json::Value` values never escape as public response payloads.

The byte scanner precedes `serde_json` and permits only the contracted ASCII
strings, canonical unsigned controls, array punctuation and JSON whitespace.
It enforces depth eight, one million tokens, the control maximum and 128-byte
other strings. `Natural` has a private representation and accepts canonical
natural decimal strings through twenty digits, including values above u64.
Transport never rejects a representable natural merely for model range,
handles in invocation inputs, duplicate sites, unknown variables, mismatched
input counts, invalid field modalities, or zero/over-hard model limits.
Those remain inputs to the actual model's admission stages.

All request code-row counts are checked before constructor-stack processing.
Every postfix child is consumed once; final stacks contain exactly one value.
Stacks enforce exact arities, source atom positions and depth 256 without
constructing or inferring a model tree. Separate input counts, per-code row
limits and the aggregate 131072-row quota are fixed. Request encoding checks
owned collection sizes before intermediate JSON allocation and then applies
the same strict decoder. Response encoding likewise bounds collections and
uses the same strict schema decoder. Overflow returns an error without
truncating an encoded response.

Framing reads a four-byte little-endian length before allocating a payload.
Zero/over-cap lengths fail immediately. Only EOF before any header byte is a
normal boundary; every partial header or payload fails. Writers check length
before writing and flush complete frames. IO failures have bounded static
reasons. `CONTROL_CAP` and `BYE_FRAME_RESERVE=512` are public constants;
`BOUNDS` retains the exact twelve contracted entries. The process owner owns
sequence progression, completed-response counters and prepaid session wire
accounting, including the retirement reserve. The codec checks READY identity
and bounds, and checks BYE's internal counter partition and next sequence.

Validation used a standalone temporary crate at
`/tmp/resource-model-endpoint42-protocol-probe`, whose library path points
directly at the owned module. Its manifest pins the existing lockfile versions
`serde=1.0.229` (derive enabled) and `serde_json=1.0.154`, has its own workspace,
and disables debug information and incremental compilation. Every Cargo
command used `CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next`,
`CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0` and `--offline`:

```sh
cargo test --offline --manifest-path /tmp/resource-model-endpoint42-protocol-probe/Cargo.toml --lib
cargo clippy --offline --manifest-path /tmp/resource-model-endpoint42-protocol-probe/Cargo.toml --all-targets -- -D warnings
cargo run --offline --manifest-path /tmp/resource-model-endpoint42-protocol-probe/Cargo.toml
rustfmt --edition 2024 --check src/bin/resource_oracle42/protocol.rs
```

All ten unit tests passed. They cover all diagnostic constructors, request
rows, semantic-negative representability, exact/over-bound row counts and
input counts, depth 256/257, lexical tokens, malformed schemas/operands,
unknown and extra enum payloads, every frame truncation, cap checks before
payload reads, boundary EOF, short/interrupted reads and IO failures. The final
unit run took 0.22s after a 0.98s build. No unit-test assertion failed. The first
strict Clippy run rejected one collapsible nested `if`; the equivalent let
chain fixed it, and the final strict run passed in 0.72s. Rustfmt passed.

The temporary executable also consumed the genuine worker peer's
`/tmp/resource-model-endpoint42-worker-v1/probe-results.json`, decoded and
re-encoded all eighteen actual result payloads and checked their exact JSON
identity, then decoded the actual BYE. These observations cover all six stages,
returned values, u64 boundary literals, invocation errors, live ownership,
mint/consume, tombstone freshness, both branch values, unpack order, zero fuel
and a resource-limited prefix. The probe JSON SHA-256 was
`f38ce1f7eb1b7c6b31c2c67a4e06bd29059972e71cb7ad82db0bc9fcee58335a`.
Mechanical DTO roundtrips are codec tests; no synthetic peer supplies a
successful model observation. The source SHA-256 is
`e37a67133699d6f94b290dcc366c26f762ead0ba42069e7f4cd7b7fbff508397`.

Only the new protocol module and this report are owned by this lane. No root
Cargo invocation, manifest/lock edit, model inference, runtime/kernel/profile
change or Lean compilation occurred. The exclusive Cargo/shared-target lease
was explicitly released after the last standalone verification. Integrated
Session, CLI and production transport acceptance belong to the coordinator.
