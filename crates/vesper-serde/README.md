# vesper-serde

A bounded scalar Rust host implementation of the JSON core in design §20.
It has no dependencies and forbids unsafe code. It does not provide source
macros, source Region typing, linear-resource deserialization, SIMD kernels,
float conversion, archives, NDJSON/AoSoA ingestion, or throughput guarantees.

```rust
use vesper_serde::{IndexedJson, Limits};

let json = IndexedJson::parse(br#"{"id":42,"topic":"ready"}"#, Limits::default())?;
let id = json.root().field("id", 1024)?.unwrap().as_u64()?;
assert_eq!(id, 42);
# Ok::<(), vesper_serde::Error>(())
```

`IndexedJson::parse` validates all syntax, Unicode, UTF-8, numbers and nested
unknown fields before publishing any `Node`. Numbers follow JSON grammar and
can be arbitrarily large within the input/work bounds; `as_u64`/`as_i64`
separately check representability. No floating-point parsing is advertised.
Duplicate object keys are rejected by decoded Unicode scalar equality in all
objects. Thus `"id"` and `"\u0069d"` conflict. Unicode normalization and case
folding are not applied. Members in separate objects may reuse keys.

The token tape stores subtree ends, allowing a `Cursor` to skip a previously
validated subtree in constant time. Array cursors stop at their own array end.
Object fields support schema-directed lookup and decoded key comparisons.
Unescaped strings, including non-ASCII UTF-8, borrow immutable input. Escaped
strings decode into explicit caller scratch after a capacity check. Failed
scratch reads preserve every scratch byte and the cursor position. Rust
lifetimes enforce these host borrows; they do not implement Vesper Region modes.

Parsing requests one token-vector allocation, bounded by input length, token
count and `max_index_bytes`. Limits count logical vector payload capacity, not
allocator bookkeeping or process RSS. Strings are never heap-allocated by the
codec. Container recursion has a hard ceiling of 128 in addition to the caller
limit. All input bytes count toward work before UTF-8 validation; consumed bytes
and decoded duplicate-key comparisons add work. A comparison step costs 24
units to cover the bounded escape decoding of two Unicode scalars. Wide objects
use quadratic duplicate checks, stopped by this work allowance. Work units are
a deterministic budget, not CPU instructions or elapsed time. `work_used()` and
`index_capacity_bytes()` expose accounting on success.

`Value`/`encode_into` support null, bool, exact u64/i64, UTF-8 strings, arrays,
objects and validated raw `Node`s. Preflight checks duplicate keys, all limits,
checked sizes and output capacity before any write; errors preserve every output
byte. Raw embeddings also count nested depth/tokens/decoded-string bytes against
the destination limits. Encoding allocates no dynamic storage and uses only
borrowed inputs. Bytes beyond the successful encoded length remain untouched.
There are no consuming operations or partial linear-resource construction.

`SwarFieldMatcher` safely compiles at most seven printable ASCII bytes, rejecting
quote/backslash/control/non-ASCII keys. `matches_window` checks lexical bytes
including the closing quote without over-reading short windows; it does not
validate JSON. `matches_key` operates on validated keys and includes the decoded
fallback for escaped-equivalent spellings. `JsonSizeBound` offers checked integer,
boolean and conservative string ceilings. `Dynamic { static_ceiling }` bounds
only the statically bounded contribution after excluding dynamic payloads; it
provides neither an upper nor a lower bound on total encoded size. The scalar escape-mask reference
carries backslash parity across 64-byte blocks; it is not used as a SIMD proof.

Run the self-checking example with:

```sh
cargo run -p vesper-serde --example bounded_json
```
