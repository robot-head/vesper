# Independent Arrow IPC fixtures

Generated on 2026-11-12 by `generate.py` using **Apache Arrow pyarrow 20.0.0**
(CPython 3.12, official manylinux wheel), then decoded again with PyArrow to
record actual arrays and mathematical sums in `expected.json`. No Vesper code
produces these bytes or the expectations. `expected.json` includes every file's
byte length and SHA-256 digest. The sum-overflow fixture's mathematical sum is
9223372036854775808; a checked i64 projection cannot represent it.

Reproduce in a user-local environment (no system package installation):

```sh
python3 -m venv /tmp/vesper-ipc-reference-venv
/tmp/vesper-ipc-reference-venv/bin/pip install pyarrow==20.0.0
/tmp/vesper-ipc-reference-venv/bin/python crates/vesper-grammar/tests/fixtures/ipc/generate.py
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
