#!/usr/bin/env python3
"""Exercise actual tuple-aware macro compilation, hygiene and unsaved previews."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/source_macro_tuples"
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
    assert value["macro_definitions"] >= 1 and value["invocations"] >= 1
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

    def execute(self, *args, input_data=None, expected_error=None, expected_status=0):
        output = subprocess.run([self.binary, *(str(a) for a in args)], input=input_data, capture_output=True, timeout=60, check=False)
        assert output.returncode == (1 if expected_error else expected_status), (args, output.returncode, output.stdout.decode(errors="replace"), output.stderr.decode(errors="replace"))
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


    def forge(self, source, success=True):
        path = self.work / f"parameter-source-{self.commands}.ves"
        original = source.encode("utf-8")
        path.write_bytes(original)
        value = load(self.execute("forge", "check", path, expected_status=0 if success else 1))
        assert value["schema"] == "vesper.forge.source/1"
        assert value["status"] == ("expanded_checked" if success else "rejected"), value
        expanded = self.execute("forge", "expand", path, expected_status=0 if success else 1)
        if success:
            assert expanded == value["expanded_source"].encode("utf-8")
            assert value["source_bytes"] == len(original)
            assert value["expanded_bytes"] == len(expanded)
            assert value["source_map"]
            for entry in value["source_map"]:
                source_range(entry["original"], source)
                source_range(entry["expanded"], value["expanded_source"])
        else:
            assert expanded == b"" and value["source_map"] == []
            assert not {"world", "expanded_blake3", "invocations", "package"}.intersection(value)
        assert path.read_bytes() == original
        return value


def tokens(source):
    # Independent lexical inventory for these closed fixtures, including exact
    # ordered comment/string payloads. No nested comments occur in this corpus.
    return re.findall(r'/\*.*?\*/|//[^\r\n]*|"(?:\\.|[^"\\])*"|[A-Za-z_][A-Za-z_0-9]*|[0-9]+|[^\s]', source, re.S)

def check_repeated_maps(value, source):
    expanded = value["expanded_source"]
    assert "/* tuple , ) λ 🦀 */" in expanded
    original = source.encode("utf-8")
    header_start = original.index(b"value:", original.index(b"fn forward"))
    header = [m for m in value["source_map"] if m["original"] == {"start": header_start, "end": header_start + 5}]
    assert len(header) == 1 and header[0]["kind"] == "source"
    name = source_range(header[0]["expanded"], expanded)
    assert name != "value" and name.isidentifier()
    argument = original.index(b"twice!(value)") + len(b"twice!(")
    entries = [m for m in value["source_map"] if m["original"] == {"start": argument, "end": argument + 5}]
    assert len(entries) == 2 and entries[0]["expanded"] != entries[1]["expanded"]
    assert all(source_range(m["expanded"], expanded) == name for m in entries)
    a, b = [m["expanded"] for m in entries]
    assert a["end"] <= b["start"] or b["end"] <= a["start"]


def run(binary, work):
    h = Harness(binary, work)
    existing = work / "existing.ves"
    existing.write_bytes(b"fn main()->i64{99}\n")
    uri = existing.as_uri()
    names = ["unit", "grouped", "singleton", "multiple", "nested", "unit_slot", "singleton_slot", "nested_slot",
             "input_slot", "parameter", "argument_capture", "reference_capture", "global_capture", "initializer",
             "element_scope", "evaluation_order", "punctuation", "repeated", "linear_consume", "arity_nine"]
    for name in names:
        source = "// current unsaved tuples 😀\r\n" + fixture(name)
        checked = h.forge(source)
        h.explicit_run(checked["expanded_source"])
        replies = h.session([
            open_document(uri, 1, source), preview(2, uri, 1),
            change(uri, 2, source), preview(3, uri, 2),
            request(4, "textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 3}}),
        ])
        value = result(replies, 2)
        accepted(value, uri, 1, source)
        for field in ["source_blake3", "expanded_blake3", "expanded_source", "source_map", "audits", "invocations"]:
            assert value[field] == checked[field], (name, field)
        current = result(replies, 3)
        assert current["version"] == 2
        assert {k: v for k, v in current.items() if k != "version"} == {k: v for k, v in value.items() if k != "version"}
        error(replies, 4, -32003)
        publications = [m["params"] for m in replies if m.get("method") == "textDocument/publishDiagnostics"]
        assert len(publications) == 2 and all(p["vesper"]["status"] == "parse_error" and p["diagnostics"] for p in publications)
        assert existing.read_bytes() == b"fn main()->i64{99}\n"
        if name == "repeated":
            check_repeated_maps(checked, source)

    for name, code in [("linear_duplicate", "E0303"), ("linear_leak", "E0302"), ("field_mismatch", "E0200")]:
        value = h.forge(fixture(name), success=False)
        assert value["stage"] == "check" and any(d["code"] == code for d in value["diagnostics"])
    for expression, result_type in [("(42,)", "i64"), ("(42)", "(i64,)")]:
        source = "macro make()->expr{quote{" + expression + "}} fn forward()->" + result_type + "{make!()} fn main()->i64{42}"
        value = h.forge(source, success=False)
        assert value["stage"] == "check" and any(d["code"] == "E0200" for d in value["diagnostics"])
    for body in ["(,)", "(1,,2)", "(1 2)", "(1,2]", "((1,),,)"]:
        h.forge("macro make()->expr{quote{" + body + "}} fn main(){make!()}", success=False)
    for source in [
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!((42,),)}",
        "fn score(in n:(i64,))->i64{let(x,)=n;x} macro make()->expr{quote{(42,)}} fn main()->i64{score(make!(),)}",
    ]:
        h.forge(source, success=False)
    nested = "(" * 160 + "42" + ",)" * 160
    failure = h.forge("macro make()->expr{quote{" + nested + "}} fn main(){make!()}", success=False)
    assert failure["stage"] == "resource"
    values = ",".join(["0"] * 7000)
    h.forge("macro make()->expr{quote{(" + values + ")}} fn main(){make!()}", success=False)

    pure, caller = fixture("generated_pure"), fixture("caller_effect")
    h.forge(pure)
    for old, new in [("wrapping_add(left, 1)", "left + 1"), ("wrapping_add(right, 1)", "right + 1")]:
        changed = pure.replace(old, new)
        assert changed != pure
        assert h.forge(changed, success=False)["stage"] == "authority"
    caller_report = h.forge(caller)
    # Source slot effects retain their real declared Fail row. A compiler artifact
    # is not permission to execute an unhandled entry point.
    h.explicit_unhandled_refusal(caller_report["expanded_source"])
    changed = pure.replace("wrapping_add(right, 1)", "right + 1")
    large = "macro make()->expr{quote{(" + ",".join(["0"] * 600) + ")}} fn main(){make!()}"
    replies = h.session([
        open_document(uri, 10, pure), preview(2, uri, 10),
        change(uri, 11, changed), preview(3, uri, 11),
        change(uri, 12, caller), preview(4, uri, 12), preview(5, uri, 11),
        change(uri, 13, fixture("linear_duplicate")), preview(6, uri, 13),
        change(uri, 14, large), preview(7, uri, 14),
    ])
    accepted(result(replies, 2), uri, 10, pure)
    rejected(result(replies, 3), uri, 11, "authority")
    accepted(result(replies, 4), uri, 12, caller)
    error(replies, 5, -32801)
    rejected(result(replies, 6), uri, 13, "check")
    refused = result(replies, 7)
    assert refused["status"] == "rejected" and refused["stage"] in ["lower", "resource"]
    assert refused["source_map"] == [] and refused["audits"] == []
    assert "expanded_blake3" not in refused

    for name in ["repeated", "punctuation", "nested"]:
        path = work / f"format-{name}.ves"
        original = fixture(name).replace("fn main", "fn   main").replace(" -> ", "->")
        path.write_bytes(original.encode("utf-8"))
        changed = load(h.execute("fmt", path, "--json"))
        assert changed["changed"] is True
        formatted = path.read_bytes().decode("utf-8")
        assert tokens(formatted) == tokens(original)
        stable = load(h.execute("fmt", path, "--check", "--json"))
        assert stable["changed"] is False and path.read_bytes() == formatted.encode("utf-8")
        h.explicit_run(h.forge(formatted)["expanded_source"])
    print(json.dumps({"status": "passed", "commands": h.commands, "stdio_sessions": h.sessions,
                      "jsonrpc_requests": h.requests, "explicit_expanded_source_runs": h.runs,
                      "unhandled_entry_refusals": h.unhandled_refusals,
                      "scope": "actual pure Expr tuple compiler; original editor semantics unchanged"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = pathlib.Path(str(args.binary) + ".exe") if pathlib.Path(str(args.binary) + ".exe").is_file() else args.binary
    binary = binary.resolve(strict=True)
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-source-macro-tuples-", dir=args.work) as directory:
        run(binary, pathlib.Path(directory).resolve())


if __name__ == "__main__":
    main()
