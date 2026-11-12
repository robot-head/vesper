"""Generate independent Arrow IPC fixtures; Python 3.12 + pyarrow==20.0.0.
Run with a user-local venv, e.g. /tmp/vesper-ipc-reference-venv/bin/python.
No repository Rust code participates in producing the bytes or expected values.
"""
import hashlib
import json
from pathlib import Path
import pyarrow as pa
import pyarrow.ipc as ipc

ROOT = Path(__file__).resolve().parent
SCHEMA = pa.schema([pa.field("nullable", pa.int64(), nullable=True), pa.field("required", pa.int64(), nullable=False)])
BATCHES = [
    [[1, None, -2, 0, 2**40, -(2**40), None, 7, 5], list(range(10, 100, 10))],
    [[], []],
    [[None] * 4, [-9, 0, 9, 12]],
]
manifest = {"producer": "Apache Arrow pyarrow", "version": pa.__version__, "fixtures": {}}
def write(name, schema, batches, **options):
    target = ROOT / name
    with pa.OSFile(str(target), "wb") as sink:
        with ipc.new_stream(sink, schema, options=ipc.IpcWriteOptions(**options)) as writer:
            for values in batches:
                writer.write_batch(pa.record_batch([pa.array(v, type=f.type) for v, f in zip(values, schema)], schema=schema))
    wire = target.read_bytes()
    with ipc.open_stream(wire) as reader:
        actual = [[col.to_pylist() for col in batch.columns] for batch in reader]
    manifest["fixtures"][name] = {"bytes": len(wire), "sha256": hashlib.sha256(wire).hexdigest(), "values": actual, "sums": [[sum(x for x in col if x is not None) if any(x is not None for x in col) and all(x is None or isinstance(x, int) for x in col) else None for col in b] for b in actual]}
write("modern-v5.arrow", SCHEMA, BATCHES)
write("legacy-v4.arrow", SCHEMA, BATCHES, use_legacy_format=True, metadata_version=ipc.MetadataVersion.V4)
write("schema-only.arrow", SCHEMA, [])
write("sum-overflow.arrow", pa.schema([pa.field("x", pa.int64())]), [[[2**63 - 1, 1]]])
write("int32.arrow", pa.schema([pa.field("x", pa.int32())]), [[[1, None, -2]]])
write("unsigned.arrow", pa.schema([pa.field("x", pa.uint64())]), [[[1, None, 2]]])
write("custom-metadata.arrow", SCHEMA.with_metadata({b"key": b"value"}), [])
write("empty-schema.arrow", pa.schema([]), [[]])
write("compressed.arrow", SCHEMA, BATCHES[:1], compression="lz4")
write("nested.arrow", pa.schema([pa.field("list", pa.list_(pa.int64()))]), [[[[1, 2], None, []]]])
# Dictionary construction uses Arrow's real dictionary encoder and writer.
dictionary_schema = pa.schema([pa.field("dictionary", pa.dictionary(pa.int8(), pa.int64()))])
with pa.OSFile(str(ROOT / "dictionary.arrow"), "wb") as sink:
    with ipc.new_stream(sink, dictionary_schema) as writer:
        writer.write_batch(pa.record_batch([pa.array([1, 2, 1, None], type=pa.int64()).dictionary_encode().cast(dictionary_schema[0].type)], schema=dictionary_schema))
wire = (ROOT / "dictionary.arrow").read_bytes()
manifest["fixtures"]["dictionary.arrow"] = {"bytes": len(wire), "sha256": hashlib.sha256(wire).hexdigest(), "unsupported": "dictionary"}
(ROOT / "expected.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
print(json.dumps({name: item["bytes"] for name, item in manifest["fixtures"].items()}, sort_keys=True))
