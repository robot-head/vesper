# Independent Arrow IPC fixtures

Originally generated and decoded on 2026-11-12 using **Apache Arrow pyarrow
20.0.0**. The Aspect task assembles the original messages from
`wire-metadata.json`, independently encodes the values and validity bitmaps,
and uses the `lz4` command for compressed frames. It reproduces each stream
and `expected.json` byte for byte without calling Vesper. The manifest records
the decoded values, mathematical sums, byte lengths, and SHA-256 digests.
The sum-overflow fixture's mathematical sum is
9223372036854775808; a checked i64 projection cannot represent it.

With `lz4` on PATH, regenerate from the repository root:

```sh
aspect crate grammar generate
aspect ci tests arrow-fixtures
```

The positive streams cover V5 continuation framing, V4 legacy framing, two
signed-i64 fields with distinct nullability, positive/negative/large values,
9-row bitmap tails, empty batches, all-null columns, schema-only and zero-field
streams. Negative reference streams contain real Arrow compression (LZ4),
dictionaries, nested lists, signed i32, unsigned i64, and schema custom metadata.
They are valid Arrow IPC outside this reader's deliberately restricted profile.

Format references at the same upstream release:

- https://github.com/apache/arrow/blob/apache-arrow-20.0.0/format/Message.fbs
- https://github.com/apache/arrow/blob/apache-arrow-20.0.0/format/Schema.fbs
- https://arrow.apache.org/docs/format/Columnar.html#ipc-streaming-format

The tests additionally mutate independent fixtures to exercise invalid framing,
uoffsets, signed vtable offsets, scalar/object overlap, vector counts, UTF-8,
union tags, endianness, nodes, descriptors, bitmap counts and nullability. Such
mutations are hostile test vectors, not independently produced valid streams.
