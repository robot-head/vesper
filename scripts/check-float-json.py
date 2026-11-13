#!/usr/bin/env python3
"""Compare actual finite_float_json output with Python's independent binary64 JSON parser."""
import argparse
import json
import math
import pathlib
import struct
import subprocess
import sys
import threading
import time

EDGES = [
    0x0000000000000000, 0x8000000000000000,
    0x0000000000000001, 0x8000000000000001,
    0x000FFFFFFFFFFFFF, 0x800FFFFFFFFFFFFF,
    0x0010000000000000, 0x8010000000000000,
    0x0010000000000001, 0x8010000000000001,
    0x7FEFFFFFFFFFFFFF, 0xFFEFFFFFFFFFFFFF,
    0x7FEFFFFFFFFFFFFE, 0xFFEFFFFFFFFFFFFE,
    0x3FF0000000000000, 0xBFF0000000000000,
    0x3FEFFFFFFFFFFFFF, 0x3FF0000000000001,
    0x3CB0000000000000, 0x3FB999999999999A,
    0xBFB999999999999A, 0x4340000000000000,
    0x4340000000000001, 0xC340000000000001,
]
MAX_RECORDS = len(EDGES) + 4096
MAX_OUTPUT = MAX_RECORDS * 1024


def corpus():
    result = list(EDGES)
    state = 0x6A09E667F3BCC909
    for _ in range(4096):
        state = (state * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        if state & 0x7FF0000000000000 != 0x7FF0000000000000:
            result.append(state)
    return result


def unique(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate JSON key {key!r}"
        result[key] = value
    return result


def reject_constant(text):
    raise AssertionError(f"non-JSON constant {text}")


def execute(binary):
    process = subprocess.Popen([str(binary)], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    captured = {}

    def read(name, stream, limit):
        captured[name] = stream.read(limit + 1)

    readers = [threading.Thread(target=read, args=("stdout", process.stdout, MAX_OUTPUT), daemon=True),
               threading.Thread(target=read, args=("stderr", process.stderr, 65536), daemon=True)]
    for reader in readers:
        reader.start()
    started = time.monotonic()
    try:
        while process.poll() is None:
            if len(captured.get("stdout", b"")) > MAX_OUTPUT or len(captured.get("stderr", b"")) > 65536:
                raise AssertionError("example output exceeded its finite allowance")
            if time.monotonic() - started > 30:
                raise AssertionError("example exceeded its process deadline")
            time.sleep(0.01)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        for reader in readers:
            reader.join(timeout=5)
        process.stdout.close()
        process.stderr.close()
    assert all(not reader.is_alive() for reader in readers)
    output, stderr = captured["stdout"], captured["stderr"]
    assert len(output) <= MAX_OUTPUT and len(stderr) <= 65536
    assert process.returncode == 0, (process.returncode, stderr)
    assert not stderr, stderr
    return output


def validate(output):
    expected = corpus()
    lines = output.splitlines()
    assert len(lines) == len(expected) <= MAX_RECORDS
    negative_zero = False
    for line, bits in zip(lines, expected):
        assert len(line) <= 1024
        # Both hooks are necessary: a valid JSON integer spelling '-0' must
        # retain its binary64 sign instead of passing through Python's int(0).
        record = json.loads(line.decode("utf-8"), parse_int=float, parse_float=float,
                            parse_constant=reject_constant, object_pairs_hook=unique)
        assert set(record) == {"bits", "value"}
        assert record["bits"] == f"{bits:016x}"
        value = record["value"]
        assert type(value) is float and math.isfinite(value), record
        assert struct.pack(">d", value).hex() == record["bits"], record
        negative_zero |= bits == 0x8000000000000000
    assert negative_zero
    return len(expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() and pathlib.Path(str(binary) + ".exe").is_file():
        binary = pathlib.Path(str(binary) + ".exe")
    assert binary.is_file(), binary
    assert struct.calcsize("d") == 8 and sys.float_info.mant_dig == 53
    first, second = execute(binary), execute(binary)
    assert first == second, "actual encoded corpus must be deterministic"
    count = validate(first)
    print(json.dumps({"status": "passed", "processes": 2, "finite_values_per_process": count,
                      "output_bytes_per_process": len(first),
                      "scope": "actual finite binary64 JSON interoperability including negative zero; no throughput or proof claim"}))


if __name__ == "__main__":
    main()
