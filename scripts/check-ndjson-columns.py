#!/usr/bin/env python3
"""Check real batch-to-column JSON output with an independent Python oracle."""
import argparse
import json
import pathlib
import struct
import subprocess
import threading
import time

MAX_OUTPUT = 128 * 8193


def unique(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate key {key!r}"
        result[key] = value
    return result


def reject_constant(value):
    raise AssertionError(f"non-JSON constant {value}")


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique, parse_constant=reject_constant)


def same(actual, expected):
    assert type(actual) is type(expected), (actual, expected)
    if isinstance(expected, dict):
        assert set(actual) == set(expected)
        for key in expected:
            same(actual[key], expected[key])
    elif isinstance(expected, list):
        assert len(actual) == len(expected)
        for a, b in zip(actual, expected):
            same(a, b)
    elif isinstance(expected, float):
        assert struct.pack(">d", actual) == struct.pack(">d", expected)
    else:
        assert actual == expected, (actual, expected)


def corpus():
    source, expected = [], []
    state = 0x510E527FADE682D1
    for i in range(96):
        state = (state * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        identifier = [0, (1 << 64) - 1, (1 << 53) + 1, (1 << 32) - 1][i] if i < 4 else state
        topic = ["é🦀", "line\n\0\"\\", "e\u0301", "\u2028\u2029", ""][i % 5] + str(i)
        metadata = [None, True, [i, {"wide": (1 << 64) - 1, "text": "raw🦀"}],
                    {"fraction": 1.25, "negative_zero": -0.0}, "unescaped é and escaped \\ text", -9223372036854775808][i % 6]
        row = {"id": identifier, "topic": topic, "raw_meta": metadata}
        wire = {**row, "ignored": {"nested": ["validated", i, False]}}
        text = json.dumps(wire, ensure_ascii=i % 2 == 1, separators=(",", ":"))
        if i % 3 == 0:
            text = text.replace('"id":', '"\\u0069d":', 1).replace('"topic":', '"to\\u0070ic":', 1).replace('"raw_meta":', '"raw_\\u006deta":', 1)
        source.append(text + ("\r\n" if i % 2 else "\n"))
        expected.append(row)
    payload = "".join(source).encode("utf-8")
    assert len(payload) <= 64 * 1024
    return payload, expected


class Harness:
    def __init__(self, binary):
        self.binary, self.processes = str(binary), 0

    def execute(self, payload=None, failure=None):
        process = subprocess.Popen([self.binary, *( ["--stdin"] if payload is not None else [])],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        captured, write_errors = {}, []

        def write():
            try:
                process.stdin.write(payload or b"")
            except BrokenPipeError as error:
                write_errors.append(error)
            finally:
                process.stdin.close()

        def read(name, stream, limit):
            captured[name] = stream.read(limit + 1)

        threads = [threading.Thread(target=write, daemon=True),
                   threading.Thread(target=read, args=("stdout", process.stdout, MAX_OUTPUT), daemon=True),
                   threading.Thread(target=read, args=("stderr", process.stderr, 65536), daemon=True)]
        for thread in threads:
            thread.start()
        started = time.monotonic()
        try:
            while process.poll() is None:
                if len(captured.get("stdout", b"")) > MAX_OUTPUT or len(captured.get("stderr", b"")) > 65536:
                    raise AssertionError("example output exceeded finite allowance")
                if time.monotonic() - started > 30:
                    raise AssertionError("example process deadline exceeded")
                time.sleep(0.01)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            for thread in threads:
                thread.join(timeout=5)
            process.stdout.close()
            process.stderr.close()
        assert all(not thread.is_alive() for thread in threads)
        stdout, stderr = captured["stdout"], captured["stderr"]
        assert len(stdout) <= MAX_OUTPUT and len(stderr) <= 65536
        if failure is not None:
            assert process.returncode == 1 and not stdout, (process.returncode, stdout, stderr)
            assert failure in stderr.decode("utf-8"), stderr
        else:
            assert process.returncode == 0 and not write_errors and not stderr, (process.returncode, stderr)
        self.processes += 1
        return stdout


def records(output):
    lines = output.split(b"\n")
    assert lines[-1] == b""
    assert len(lines) <= 129
    return [load(line) for line in lines[:-1]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() and pathlib.Path(str(binary) + ".exe").is_file():
        binary = pathlib.Path(str(binary) + ".exe")
    assert binary.is_file(), binary
    h = Harness(binary)
    source, expected = corpus()
    first, second = h.execute(source), h.execute(source)
    assert first == second
    same(records(first), expected)
    same(records(h.execute(source.rstrip(b"\r\n"))), expected)
    same(records(h.execute()), [
        {"id": (1 << 64) - 1, "topic": "é🦀", "raw_meta": {"unknown": [True, None]}},
        {"id": (1 << 53) + 1, "topic": "line\n🦀", "raw_meta": [1, 2, 3]},
        {"id": 0, "topic": "empty metadata", "raw_meta": None},
    ])
    assert h.execute(b"") == b""
    good = b'{"id":1,"topic":"ok","raw_meta":null}\n'
    for tail, reason in [
        (b'{"id":2,"topic":"ok","raw_meta":null,"unknown":[1,]}', "Syntax"),
        (b'{"id":2,"topic":"ok","raw_meta":{"x":1,"\\u0078":2}}', "DuplicateKey"),
        (b'{"id":2,"topic":"\\uD800","raw_meta":null}', "Escape"),
        (b'{"id":2,"topic":"\xff","raw_meta":null}', "Utf8"),
        (b'{"topic":"ok","raw_meta":null}', "MissingField"),
        (b'{"id":18446744073709551616,"topic":"ok","raw_meta":null}', "Number"),
        (b'{"id":2,"topic":false,"raw_meta":null}', "Type"),
        (b'\n', "BlankLine"),
    ]:
        h.execute(good + tail, failure=reason)
    h.execute(good * 129, failure="RecordLimit")
    h.execute(b"x" * (64 * 1024 + 1), failure="exceeds 64 KiB")
    print(json.dumps({"status": "passed", "processes": h.processes,
                      "oracle_rows": len(expected), "encoded_bytes": len(first),
                      "scope": "actual validated NDJSON to caller columns and scalar JSON; independent Unicode/raw metadata/u64 oracle"}))


if __name__ == "__main__":
    main()
