#!/usr/bin/env python3
"""Actual Tier1 source macro pipeline and explicit normal-artifact execution."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "source_macros"
MAX_OUTPUT = 2 * 1024 * 1024


def unique(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate output key {key!r}"
        result[key] = value
    return result


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def fixture(name):
    return (FIXTURES / (name + ".ves")).read_bytes().decode("utf-8")


def source_range(value, source):
    data = source.encode("utf-8")
    start, end = value["start"], value["end"]
    assert isinstance(start, int) and isinstance(end, int) and 0 <= start <= end <= len(data)
    data[:start].decode("utf-8")
    data[:end].decode("utf-8")
    return data[start:end].decode("utf-8")


def policy(name, effect, emits, ceiling):
    return {"schema": "vesper.forge.policy/1", "macro_emits": [{"name": name, "effects": [effect]}] if emits else [], "caller_ceiling": [effect] if ceiling else []}


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.commands, self.runs, self.serial = 0, 0, 0

    def execute(self, *args, input_data=None, success=True):
        output = subprocess.run([self.binary, *(str(a) for a in args)], input=input_data, capture_output=True, timeout=45, check=False)
        assert output.returncode == (0 if success else 1), f"{args!r}: {output.returncode}\n{output.stdout.decode(errors='replace')}\n{output.stderr.decode(errors='replace')}"
        assert len(output.stdout) <= 8 * MAX_OUTPUT and len(output.stderr) <= MAX_OUTPUT
        self.commands += 1
        return output

    def source(self, source):
        self.serial += 1
        path = self.work / f"original-{self.serial}.ves"
        path.write_bytes(source.encode("utf-8"))
        return path

    def policy(self, value):
        self.serial += 1
        path = self.work / f"policy-{self.serial}.json"
        path.write_bytes((value if isinstance(value, str) else json.dumps(value)).encode("utf-8"))
        return path

    def checked(self, source, args=()):
        path = self.source(source)
        output = self.execute("forge", "check", path, *args)
        result = load(output.stdout)
        assert result["schema"] == "vesper.forge.source/1" and result["status"] == "expanded_checked" and result["scope"] == "tier1_source"
        expanded = result["expanded_source"]
        assert result["source_bytes"] == len(source.encode("utf-8")) and result["expanded_bytes"] == len(expanded.encode("utf-8"))
        for field in ["source_blake3", "expanded_blake3"]:
            assert re.fullmatch("[0-9a-f]{64}", result[field])
        assert result["source_blake3"] != result["expanded_blake3"]
        assert result["macro_definitions"] > 0 and result["invocations"] > 0
        assert len(result["audits"]) == result["invocations"] and result["source_map"]
        for entry in result["source_map"]:
            source_range(entry["original"], source)
            source_range(entry["expanded"], expanded)
            assert entry["kind"] in ["source", "generated"] and (entry["macro"] is None or isinstance(entry["macro"], str))
        for audit in result["audits"]:
            assert audit["scope"] == "expanded_function" and audit["macro"]
            assert "!(" in source_range(audit["invocation"], source)
        assert not {"world", "original_checked", "source_checked", "checked_original"}.intersection(result)
        exact = self.execute("forge", "expand", path, *args).stdout
        assert exact == expanded.encode("utf-8"), "expand added bytes or returned a different candidate"
        assert path.read_bytes() == source.encode("utf-8"), "forge changed original source"
        return result

    def rejected(self, source, args=()):
        path = self.source(source)
        result = load(self.execute("forge", "check", path, *args, success=False).stdout)
        assert result["schema"] == "vesper.forge.source/1" and result["status"] == "rejected" and result["source_map"] == []
        assert result["message"] and result["stage"] in ["parse", "expand", "resolve", "lower", "check", "authority", "ir", "resource", "unsupported", "config", "io"]
        assert self.execute("forge", "expand", path, *args, success=False).stdout == b"", "failed source leaked an accepted artifact"
        assert path.read_bytes() == source.encode("utf-8")
        return result

    def run_expanded(self, source, expected="42"):
        path = self.work / f"expanded-{self.serial}.ves"
        data = source.encode("utf-8")
        path.write_bytes(data)
        assert load(self.execute("check", path, "--json").stdout) == []
        assert self.execute("run", path).stdout.decode("utf-8").strip() == expected
        assert path.read_bytes() == data
        self.runs += 1


def frame(message):
    data = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(data)}\r\n\r\n".encode("ascii") + data


def decode(data):
    messages = []
    while data:
        header, separator, tail = data.partition(b"\r\n\r\n")
        assert separator and len(header) <= 8192
        lengths = []
        for line in header.split(b"\r\n"):
            name, colon, value = line.partition(b":")
            assert colon
            if name.lower() == b"content-length":
                assert value.strip().isdigit()
                lengths.append(int(value.strip()))
        assert len(lengths) == 1 and lengths[0] <= MAX_OUTPUT
        size = lengths[0]
        assert len(tail) >= size
        message = load(tail[:size])
        assert message["jsonrpc"] == "2.0"
        messages.append(message)
        data = tail[size:]
    return messages


def run(binary, work):
    harness = Harness(binary, work)
    happy = ["keep", "addone", "argument_capture", "reference_capture", "initializer", "global_helper", "repeated", "block_argument", "literal_context"]
    for name in happy:
        source = fixture(name)
        result = harness.checked(source)
        assert result["package"] == "@local/source" and result["macro_definitions"] == 1 and result["invocations"] == 1
        if name == "global_helper":
            assert "fn helper() -> i64 { 42 }" in result["expanded_source"]
        harness.run_expanded(result["expanded_source"])

    for source, expected in [(fixture("repeated"), "42"), ("// 🦀😀 CRLF revision\r\n" + fixture("repeated").replace("let n = 21", "let n = 20"), "40")]:
        result = harness.checked(source)
        at = len(source[:source.index("twice!(n)") + len("twice!(")].encode("utf-8"))
        copies = sorted({(entry["expanded"]["start"], entry["expanded"]["end"]) for entry in result["source_map"] if entry["original"] == {"start": at, "end": at + 1} and entry["expanded"]["start"] < entry["expanded"]["end"]})
        assert len(copies) >= 2 and all(a[1] <= b[0] for a, b in zip(copies, copies[1:])), "copied input reused original/display spans as expanded identity"
        harness.run_expanded(result["expanded_source"], expected)

    for name, code in [("linear_duplicate", "E0303"), ("trailing_unit", "E0200")]:
        result = harness.rejected(fixture(name))
        assert result["stage"] == "check"
        candidate = result["expanded_source"]  # explicitly rejected; never executed
        diagnostic = next(d for d in result["diagnostics"] if d["code"] == code)
        primary = source_range(diagnostic["span"], candidate)
        assert primary
        if code == "E0303":
            earlier = next(d for d in diagnostic["related"] if d["message"] == "earlier ownership-taking use of this place")
            assert earlier["span"]["end"] <= diagnostic["span"]["start"]
            assert source_range(earlier["span"], candidate) == primary

    transitive = fixture("transitive_effect")
    assert harness.rejected(transitive)["stage"] == "authority"
    for emits, ceiling in [(False, True), (True, False)]:
        path = harness.policy(policy("leaky", "Log", emits, ceiling))
        assert harness.rejected(transitive, ["--policy", path])["stage"] == "authority"
    path = harness.policy(policy("leaky", "Log", True, True))
    harness.checked(transitive, ["--policy", path])  # no execution of unhandled Log

    harness.checked(fixture("pure_declared_row"))
    for name, macro, argument in [("implicit_overflow", "bump", "ArithOverflow"), ("implicit_runtime_fail", "division", "RuntimeError")]:
        source = fixture(name)
        assert harness.rejected(source)["stage"] == "authority"
        path = harness.policy(policy(macro, "builtin::Fail", False, True))
        assert harness.rejected(source, ["--policy", path])["stage"] == "authority"
        path = harness.policy(policy(macro, "builtin::Fail", True, True))
        assert argument in harness.checked(source, ["--policy", path])["expanded_source"]

    for source in [
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{unknown!(42)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!(keep!(42))}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!()}",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->i64{keep!(1,2)}",
        "macro bad($x:expr)->expr{quote{let $x=42;$x}} fn main()->i64{bad!(name)}",
        "macro bad($x:ty)->expr{quote{42}} fn main()->i64{bad!(i64)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(in n:i64)->i64{keep!(n)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main[T:mod copy]()->i64{keep!(42)}",
        "macro call($x:expr)->expr{quote{$x(2)}} fn main()->i64{call!(let a=1;a)}",
        '#[wasm_macro(component="file:///must-not-be-loaded.wasm")] macro x()->expr{quote{42}} fn main()->i64{x!()}',
        "effect E[T:mod copy]{fn take(in x:T);} macro emit()->expr{quote{E::take(1)}} fn main()eff[E[u64]]{emit!()}",
    ]:
        harness.rejected(source)
    assert harness.rejected("//" + "x" * (128 * 1024) + "\n" + fixture("keep"))["stage"] == "resource"
    assert harness.checked(fixture("keep"), ["--package", "@acceptance/macros"])["package"] == "@acceptance/macros"
    for package in ["@vesper/builtin", "bad package"]:
        assert harness.rejected(fixture("keep"), ["--package", package])["stage"] == "config"
    for value in [
        {"schema": "vesper.forge.policy/1", "approved": True},
        {"schema": "vesper.forge.policy/1", "caller_ceiling": ["Unknown"]},
        '{"schema":"vesper.forge.policy/1","caller_ceiling":[],"caller_ceiling":["builtin::Fail"]}',
        {"schema": "vesper.forge.policy/1", "macro_emits": [{"name": "keep", "effects": []}, {"name": "keep", "effects": []}]},
        {"schema": "vesper.forge.policy/1", "macro_emits": [{"name": "missing", "effects": []}]},
    ]:
        path = harness.policy(value)
        assert harness.rejected(fixture("keep"), ["--policy", path])["stage"] == "config"
    path = harness.policy(" " * 65537)
    assert harness.rejected(fixture("keep"), ["--policy", path])["stage"] in ["config", "resource"]
    original = harness.source(fixture("keep"))
    assert harness.execute("forge", "expand", original, "--json", success=False).stdout == b""
    assert load(harness.execute("check", original, "--json", success=False).stdout)

    uri = "file:///not-implicitly-loaded/source-macros.ves"
    messages = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}},
                {"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "version": 1, "languageId": "vesper", "text": fixture("keep")}}},
                {"jsonrpc": "2.0", "id": 2, "method": "textDocument/hover", "params": {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 5}, "approved": True}},
                {"jsonrpc": "2.0", "id": 900, "method": "shutdown", "params": {}}, {"jsonrpc": "2.0", "method": "exit", "params": {}}]
    replies = decode(harness.execute("lsp", input_data=b"".join(frame(message) for message in messages)).stdout)
    publication = next(m["params"] for m in replies if m.get("method") == "textDocument/publishDiagnostics")
    assert publication["vesper"]["status"] == "parse_error"
    assert next(m for m in replies if m.get("id") == 2)["error"]["code"] == -32003
    print(json.dumps({"status": "passed", "commands": harness.commands, "explicit_normal_source_runs": harness.runs,
                      "scope": "actual Tier1 compiler pipeline; structured WASI foundation is separate"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary
    exe = pathlib.Path(str(binary) + ".exe")
    if exe.is_file():
        binary = exe
    binary = binary.resolve(strict=True)
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-source-macros-", dir=args.work) as directory:
        run(binary, pathlib.Path(directory).resolve())


if __name__ == "__main__":
    main()
