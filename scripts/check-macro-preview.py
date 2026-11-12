#!/usr/bin/env python3
"""Exercise actual current-unsaved-source macro previews through framed stdio."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/macro_preview"
MAX_FRAME = 2 * 1024 * 1024


def unique(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate key {key!r}"
        result[key] = value
    return result


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def fixture(name):
    return (FIXTURES / f"{name}.ves").read_bytes().decode("utf-8")


def request(identifier, method, params):
    return {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}


def notice(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def open_document(uri, version, text):
    return notice("textDocument/didOpen", {"textDocument": {"uri": uri, "version": version, "languageId": "vesper", "text": text}})


def change(uri, version, text):
    return notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]})


def preview(identifier, uri, version=None):
    document = {"uri": uri}
    if version is not None:
        document["version"] = version
    return request(identifier, "vesper/macroPreview", {"textDocument": document})


def frame(message):
    body = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body


def decode(data):
    result = []
    while data:
        header, separator, tail = data.partition(b"\r\n\r\n")
        assert separator and len(header) <= 8192
        sizes = []
        for line in header.split(b"\r\n"):
            name, colon, value = line.partition(b":")
            assert colon
            if name.lower() == b"content-length":
                assert value.strip().isdigit()
                sizes.append(int(value.strip()))
        assert len(sizes) == 1 and sizes[0] <= MAX_FRAME and len(tail) >= sizes[0]
        result.append(load(tail[:sizes[0]]))
        assert result[-1]["jsonrpc"] == "2.0"
        data = tail[sizes[0]:]
    return result


def response(replies, identifier):
    found = [r for r in replies if r.get("id") == identifier]
    assert len(found) == 1, (identifier, found)
    return found[0]


def result(replies, identifier):
    value = response(replies, identifier)
    assert "error" not in value, value
    return value["result"]


def error(replies, identifier, code):
    value = response(replies, identifier)
    assert value["error"]["code"] == code and "result" not in value, value


def source_range(value, source):
    data = source.encode("utf-8")
    start, end = value["start"], value["end"]
    assert type(start) is int and type(end) is int and 0 <= start <= end <= len(data)
    data[:start].decode("utf-8")
    data[:end].decode("utf-8")
    return data[start:end].decode("utf-8")


def envelope(value, uri, version):
    assert value["schema"] == "vesper.editor.macro-preview/1"
    assert value["scope"] == "expanded_source_only" and value["tier"] == 1
    assert value["uri"] == uri and value["version"] == version
    assert value["range_unit"] == "utf8_bytes" and re.fullmatch("[0-9a-f]{64}", value["source_blake3"])
    assert not {"world", "checked_source", "approved", "original_checked", "source_checked"}.intersection(value)
    assert len(json.dumps(value, ensure_ascii=False).encode("utf-8")) <= MAX_FRAME


def accepted(value, uri, version, source):
    envelope(value, uri, version)
    assert value["status"] == "expanded_checked" and value["package"] == "@local/source"
    expanded = value["expanded_source"]
    assert re.fullmatch("[0-9a-f]{64}", value["expanded_blake3"])
    assert value["source_bytes"] == len(source.encode("utf-8"))
    assert value["expanded_bytes"] == len(expanded.encode("utf-8"))
    assert value["macro_definitions"] == 1 and value["invocations"] == 1
    assert value["source_map"] and len(value["audits"]) == value["invocations"]
    for entry in value["source_map"]:
        source_range(entry["original"], source)
        source_range(entry["expanded"], expanded)
        assert entry["kind"] in ["source", "generated"]
    for audit in value["audits"]:
        assert audit["scope"] == "expanded_function"
        assert "!(" in source_range(audit["invocation"], source)
    return expanded


def rejected(value, uri, version, stage):
    envelope(value, uri, version)
    assert value["status"] == "rejected" and value["stage"] == stage
    assert value["message"] and value["source_map"] == [] and value["audits"] == []
    assert not {"expanded_blake3", "package", "source_bytes", "expanded_bytes", "macro_definitions", "invocations"}.intersection(value)
    assert value["diagnostic_source"] in ["original", "expanded", "none"]
    if value["diagnostic_source"] == "expanded":
        assert isinstance(value["expanded_source"], str)
        for diagnostic in value["diagnostics"]:
            source_range(diagnostic["span"], value["expanded_source"])


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.commands, self.sessions, self.runs, self.requests = 0, 0, 0, 0
        self.unhandled_refusals = 0

    def execute(self, *args, input_data=None, expected_error=None):
        output = subprocess.run([self.binary, *(str(a) for a in args)], input=input_data, capture_output=True, timeout=60, check=False)
        assert output.returncode == (1 if expected_error else 0), (args, output.returncode, output.stdout.decode(errors="replace"), output.stderr.decode(errors="replace"))
        assert len(output.stdout) <= 16 * MAX_FRAME and len(output.stderr) <= MAX_FRAME
        if expected_error:
            assert output.stdout == b"" and expected_error in output.stderr.decode("utf-8"), output
        self.commands += 1
        return output.stdout

    def session(self, messages):
        all_messages = [request(1, "initialize", {"capabilities": {}}), *messages,
                        request(9999, "shutdown", {}), notice("exit", {})]
        replies = decode(self.execute("lsp", input_data=b"".join(frame(m) for m in all_messages)))
        capability = result(replies, 1)["capabilities"]["experimental"]["vesperMacroPreview"]
        assert capability == {"version": 1, "tier": 1, "scope": "expanded_source_only"}
        assert result(replies, 9999) is None
        self.sessions += 1
        self.requests += sum("id" in m for m in all_messages)
        return replies

    def explicit_run(self, expanded):
        path = self.work / f"expanded-{self.runs}.ves"
        data = expanded.encode("utf-8")
        path.write_bytes(data)
        assert load(self.execute("check", path, "--json")) == []
        assert self.execute("run", path).decode("utf-8").strip() == "42"
        assert path.read_bytes() == data
        self.runs += 1

    def explicit_unhandled_refusal(self, expanded):
        # This authority-drift fixture deliberately preserves a declared Fail row.
        # A checked preview is not an entry-point execution capability.
        path = self.work / "unhandled-entry.ves"
        data = expanded.encode("utf-8")
        path.write_bytes(data)
        assert load(self.execute("check", path, "--json")) == []
        self.execute("run", path, expected_error="entry must handle all effects")
        assert path.read_bytes() == data
        self.unhandled_refusals += 1

    def compare_forge(self, preview_value, source):
        path = self.work / f"comparison-{self.commands}.ves"
        data = source.encode("utf-8")
        path.write_bytes(data)
        actual = load(self.execute("forge", "check", path))
        for field in ["source_blake3", "expanded_blake3", "source_bytes", "expanded_bytes", "expanded_source", "source_map", "audits", "package", "invocations", "macro_definitions"]:
            assert preview_value[field] == actual[field], field
        assert path.read_bytes() == data


def run(binary, work):
    harness = Harness(binary, work)
    # The existing on-disk program deliberately differs from the current unsaved text.
    existing = work / "existing.ves"
    existing.write_bytes(b"fn main()->u64{99}\n")
    uri = existing.as_uri()
    for name in ["argument_capture", "reference_capture", "repeated", "helper_pure"]:
        source = "// 😀 current unsaved buffer\r\n" + fixture(name)
        replies = harness.session([
            open_document(uri, 1, source), preview(2, uri, 1), preview(3, uri),
            request(4, "textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 3}}),
        ])
        value = result(replies, 2)
        expanded = accepted(value, uri, 1, source)
        assert result(replies, 3) == value
        error(replies, 4, -32003)
        publications = [m["params"] for m in replies if m.get("method") == "textDocument/publishDiagnostics"]
        assert len(publications) == 1 and publications[0]["vesper"]["status"] == "parse_error"
        assert publications[0]["uri"] == uri and publications[0]["version"] == 1
        assert existing.read_bytes() == b"fn main()->u64{99}\n", "URI was used for I/O"
        harness.compare_forge(value, source)  # independently invokes actual source compiler.
        if name == "repeated":
            start = len(source[:source.index("twice!(n)") + len("twice!(")].encode("utf-8"))
            copies = sorted({(m["expanded"]["start"], m["expanded"]["end"]) for m in value["source_map"]
                             if m["original"] == {"start": start, "end": start + 1} and m["expanded"]["start"] < m["expanded"]["end"]})
            assert len(copies) >= 2 and all(a[1] <= b[0] for a, b in zip(copies, copies[1:])), "repeated input lost distinct expanded spans"
        if name == "helper_pure":
            harness.explicit_unhandled_refusal(expanded)
        else:
            harness.explicit_run(expanded)  # preview itself never executes this program.

    pure, fallible, linear = fixture("helper_pure"), fixture("helper_fallible"), fixture("linear_duplicate")
    assert pure.split("{")[0] == fallible.split("{")[0], "authority drift must preserve the helper header"
    replies = harness.session([open_document(uri, 1, pure), preview(2, uri, 1), change(uri, 2, fallible), preview(3, uri, 2),
                               change(uri, 3, linear), preview(4, uri, 3), change(uri, 4, pure), preview(5, uri, 4)])
    accepted(result(replies, 2), uri, 1, pure)
    rejected(result(replies, 3), uri, 2, "authority")
    assert result(replies, 3)["source_blake3"] != result(replies, 2)["source_blake3"]
    rejected(result(replies, 4), uri, 3, "check")
    failure = result(replies, 4)
    assert failure["diagnostic_source"] == "expanded"
    duplicate = next(d for d in failure["diagnostics"] if d["code"] == "E0303")
    prior = next(d for d in duplicate["related"] if d["message"] == "earlier ownership-taking use of this place")
    assert prior["span"]["end"] <= duplicate["span"]["start"]
    assert source_range(prior["span"], failure["expanded_source"]) == source_range(duplicate["span"], failure["expanded_source"])
    accepted(result(replies, 5), uri, 4, pure)

    repeated = fixture("repeated")
    messages = [open_document(uri, 1, repeated), preview(2, uri, 1)]
    for version in range(2, 68):
        current = f"// reset revision {version} 😀\r\n{repeated}"
        messages.append(change(uri, version, current))
    messages += [preview(3, uri, 67), change(uri, 68, current), preview(4, uri, 68), preview(5, uri, 67),
                 change(uri, 68, fallible), preview(6, uri, 68), preview(7, "untitled:foreign", 1),
                 notice("textDocument/didClose", {"textDocument": {"uri": uri}}), preview(8, uri, 68), open_document(uri, 1, pure), preview(9, uri, 1)]
    replies = harness.session(messages)
    accepted(result(replies, 3), uri, 67, current)
    accepted(result(replies, 4), uri, 68, current)
    assert result(replies, 3)["source_blake3"] == result(replies, 4)["source_blake3"]
    assert result(replies, 3)["source_blake3"] != result(replies, 2)["source_blake3"]
    error(replies, 5, -32801)
    accepted(result(replies, 6), uri, 68, current)
    error(replies, 7, -32602)
    error(replies, 8, -32602)
    accepted(result(replies, 9), uri, 1, pure)

    invalid = [{}, [], {"textDocument": {}}, {"textDocument": {"uri": uri, "version": None}},
               *({"textDocument": {"uri": uri, "version": v}} for v in [True, "1", 1.5, 2147483648, -2147483649])]
    for field in ["source", "policy", "graph", "world", "approved", "package"]:
        invalid += [{"textDocument": {"uri": uri}, field: True}, {"textDocument": {"uri": uri, field: True}}]
    messages = [open_document(uri, 1, repeated)]
    messages += [request(10 + i, "vesper/macroPreview", value) for i, value in enumerate(invalid)]
    messages.append(request(89, "vesper/macroPreview", None))
    messages.append(preview(90, uri))
    replies = harness.session(messages)
    for i in range(len(invalid)):
        error(replies, 10 + i, -32602)
    # The existing envelope parser rejects null params before request routing.
    malformed = [r for r in replies if r.get("id") is None and "error" in r]
    assert len(malformed) == 1 and malformed[0]["error"]["code"] == -32600
    assert not any(r.get("id") == 89 for r in replies)
    accepted(result(replies, 90), uri, 1, repeated)

    malformed = fixture("malformed")
    oversized = "//" + "x" * (128 * 1024) + "\n" + repeated
    inventory = "macro keep($x:expr)->expr{quote{$x}}\n" + "".join(f"fn helper_{n}()->u64{{{n}}}\n" for n in range(129)) + "fn main()->u64{keep!(42)}\n"
    assert len(inventory.encode("utf-8")) < 128 * 1024
    guest = '#[wasm_macro(component="file:///never-loaded.wasm")] macro x()->expr{quote{42}} fn main()->i64{x!()}'
    replies = harness.session([open_document(uri, 1, malformed), preview(2, uri, 1), change(uri, 2, oversized), preview(3, uri, 1),
                               preview(4, uri, 2), open_document("untitled:oversized-open", 1, oversized), preview(5, "untitled:oversized-open", 1),
                               change(uri, 2, inventory), preview(6, uri, 2), change(uri, 3, guest), preview(7, uri, 3),
                               change(uri, 4, repeated), preview(8, uri, 4)])
    rejected(result(replies, 2), uri, 1, "parse")
    assert result(replies, 2)["diagnostic_source"] == "original"
    assert result(replies, 2) == result(replies, 3), "oversized change replaced current text/version"
    error(replies, 4, -32801)
    error(replies, 5, -32602)
    rejected(result(replies, 6), uri, 2, "resource")
    guest_result = result(replies, 7)
    rejected(guest_result, uri, 3, guest_result["stage"])
    assert guest_result["stage"] in ["parse", "unsupported"]
    accepted(result(replies, 8), uri, 4, repeated)
    assert existing.read_bytes() == b"fn main()->u64{99}\n"
    print(json.dumps({"status": "passed", "commands": harness.commands, "stdio_sessions": harness.sessions,
                      "jsonrpc_requests": harness.requests, "explicit_normal_source_runs": harness.runs,
                      "declared_unhandled_entry_refusals": harness.unhandled_refusals,
                      "scope": "actual pure Tier1 expanded-source preview; ordinary semantics remain original-source"}))


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
    with tempfile.TemporaryDirectory(prefix="vesper-macro-preview-", dir=args.work) as directory:
        run(binary, pathlib.Path(directory).resolve())


if __name__ == "__main__":
    main()
