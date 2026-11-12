#!/usr/bin/env python3
"""Actual syntax-only recovery/stdio acceptance; never authorize recovered semantics."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile

MAX_FRAME = 2 * 1024 * 1024
DAMAGED = "// 😀\r\nlinear struct Tx{}\r\nfn damaged(/*🦀*/sink tx:Tx){tx.}\r\nfn leak(/*😀*/sink other:Tx){}\r\n"
EMPTY_REPAIR = DAMAGED.replace("{tx.}", "{}")
VALID_REPAIR = DAMAGED.replace("{tx.}", "{let Tx{}=tx;}").replace("sink other:Tx){}", "sink other:Tx){let Tx{}=other;}")
LEAK = "linear value '{}' must be consumed exactly once; destructure it or pass it to a sink parameter"


def unique(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate output key {key!r}"
        result[key] = value
    return result


def frame(message):
    data = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(data)}\r\n\r\n".encode("ascii") + data


def decode(data):
    output = []
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
        assert len(lengths) == 1 and lengths[0] <= MAX_FRAME
        size = lengths[0]
        assert len(tail) >= size
        message = json.loads(tail[:size].decode("utf-8"), object_pairs_hook=unique)
        assert message["jsonrpc"] == "2.0"
        output.append(message)
        data = tail[size:]
    return output


def request(identifier, method, params):
    return {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}


def notice(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def initialize():
    return request(1, "initialize", {"capabilities": {"general": {"positionEncodings": ["utf-16"]},
                   "workspace": {"workspaceEdit": {"documentChanges": True}}, "textDocument": {
                       "publishDiagnostics": {"relatedInformation": True, "dataSupport": True},
                       "codeAction": {"codeActionLiteralSupport": {}}}}})


def open_document(uri, source):
    return notice("textDocument/didOpen", {"textDocument": {"uri": uri, "version": 1, "languageId": "vesper", "text": source}})


def change(uri, source, version):
    return notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": source}]})


def status_request(identifier, uri):
    return request(identifier, "vesper/recoveryStatus", {"textDocument": {"uri": uri}})


def response(messages, identifier, code=None):
    found = [m for m in messages if m.get("id") == identifier]
    assert len(found) == 1, f"response {identifier}: {messages!r}"
    if code is not None:
        assert found[0]["error"]["code"] == code, found[0]
        return found[0]["error"]
    assert "error" not in found[0], found[0]
    return found[0]["result"]


def publications(messages, uri):
    return [m["params"] for m in messages if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == uri]


def span(source, text):
    start = source.index(text)
    return {"start": len(source[:start].encode("utf-8")), "end": len(source[:start + len(text)].encode("utf-8"))}


def position(source, byte):
    data = source.encode("utf-8")
    assert 0 <= byte <= len(data) and data[max(0, byte - 1):byte + 1] != b"\r\n"
    prefix = data[:byte].decode("utf-8")
    line, character, i = 0, 0, 0
    while i < len(prefix):
        c = prefix[i]
        if c in "\r\n":
            if c == "\r" and i + 1 < len(prefix) and prefix[i + 1] == "\n":
                i += 1
            line, character = line + 1, 0
        else:
            character += len(c.encode("utf-16-le")) // 2
        i += 1
    return {"line": line, "character": character}


def source_range(source, value):
    return {"start": position(source, value["start"]), "end": position(source, value["end"])}


def raw_span(value, source):
    data = source.encode("utf-8")
    assert isinstance(value["start"], int) and isinstance(value["end"], int)
    assert 0 <= value["start"] <= value["end"] <= len(data)
    data[:value["start"]].decode("utf-8")
    data[:value["end"]].decode("utf-8")
    return data[value["start"]:value["end"]].decode("utf-8")


def syntax_status(messages, identifier, uri, source, version):
    status = response(messages, identifier)
    assert status["scope"] == "syntax_only" and status["uri"] == uri and status["version"] == version
    assert status["byte_length"] == len(source.encode("utf-8"))
    assert re.fullmatch("[0-9a-f]{64}", status["source_blake3"])
    assert not {"source", "world", "types", "checked", "liveness", "proof", "verified"}.intersection(status)
    for declaration in status["declarations"]:
        raw_span(declaration["span"], source)
        raw_span(declaration["header_span"], source)
        assert declaration["span"]["start"] <= declaration["header_span"]["start"] <= declaration["header_span"]["end"] <= declaration["span"]["end"]
        if declaration["body_span"] is not None:
            raw_span(declaration["body_span"], source)
            assert declaration["span"]["start"] <= declaration["body_span"]["start"] <= declaration["body_span"]["end"] <= declaration["span"]["end"]
        assert declaration["status"] in ["complete", "recovered", "opaque"]
    for site in status["sites"]:
        raw_span(site["span"], source)
        assert site["declaration_index"] is None or 0 <= site["declaration_index"] < len(status["declarations"])
        if site["receiver"] is not None:
            raw_span(site["receiver"]["root"], source)
            for projection in site["receiver"]["projections"]:
                raw_span(projection, source)
    return status


def declaration(status, name):
    found = [(i, d) for i, d in enumerate(status["declarations"]) if d["name"] == name]
    assert len(found) == 1, status
    return found[0]


def leak(publication, name):
    found = [d for d in publication["diagnostics"] if d["code"] == "E0302" and d["message"] == LEAK.format(name)]
    assert len(found) == 1, publication
    return found[0]


def independent_leak(publication, source, uri, status):
    assert publication["vesper"]["status"] == "parse_error"
    coverage = publication["vesper"]["recovery"]
    assert coverage["scope"] == "independent_functions"
    _, damaged = declaration(status, "damaged")
    _, complete = declaration(status, "leak")
    assert damaged["status"] == "recovered" and complete["status"] == "complete"
    assert any(f["span"] == complete["span"] for f in coverage["analyzed_functions"])
    assert any(f["span"] == damaged["span"] and f["reason"] == "damaged_function" for f in coverage["unavailable"])
    assert not any(f["span"] == damaged["span"] for f in coverage["analyzed_functions"])
    diagnostic = leak(publication, "other")
    assert diagnostic["range"] == source_range(source, complete["body_span"])
    assert diagnostic["relatedInformation"] == [{"location": {"uri": uri, "range": source_range(source, span(source, "sink other:Tx"))}, "message": "linear binding introduced here"}]
    assert diagnostic["data"]["help"] == "Every owned linear binding must be consumed exactly once on every returning path."
    assert not any(d["code"] == "E0302" and "'tx'" in d["message"] for d in publication["diagnostics"])
    assert any(d["code"] == "E0002" for d in publication["diagnostics"])


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.sessions, self.checks = 0, 0

    def lsp(self, messages):
        messages = [*messages, request(900, "shutdown", {}), notice("exit", {})]
        output = subprocess.run([self.binary, "lsp"], input=b"".join(frame(m) for m in messages), capture_output=True, timeout=45, check=False)
        assert output.returncode == 0, output.stderr.decode(errors="replace")
        assert len(output.stdout) <= 16 * MAX_FRAME and len(output.stderr) <= MAX_FRAME
        self.sessions += 1
        return decode(output.stdout)

    def check(self, name, source, success):
        path = self.work / f"{name}.ves"
        data = source.encode("utf-8")
        path.write_bytes(data)
        output = subprocess.run([self.binary, "check", str(path), "--json"], capture_output=True, timeout=45, check=False)
        assert (output.returncode == 0) == success, output.stderr.decode(errors="replace")
        assert path.read_bytes() == data, "check changed original source"
        self.checks += 1
        return json.loads(output.stdout.decode("utf-8"), object_pairs_hook=unique)


def run(binary, work):
    harness = Harness(binary, work)
    uri = "untitled:recovery/😀"
    at = position(DAMAGED, span(DAMAGED, "tx.")["start"])
    query = {"parameters": [], "result": {"Named": ["u64", []]}, "result_max_mode": "Copy", "effect_ceiling": {"instances": [], "tail": None}}
    messages = harness.lsp([initialize(), open_document(uri, DAMAGED), status_request(2, uri),
        request(3, "textDocument/hover", {"textDocument": {"uri": uri}, "position": at, "verified": True}),
        request(4, "textDocument/definition", {"textDocument": {"uri": uri}, "position": at, "checked": True}),
        request(5, "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": at, "end": at}, "context": {"only": ["quickfix"], "diagnostics": [{"verified": True, "fix": "FORGED_FIX"}]}}),
        request(6, "vesper/signatureSearch", {"uri": uri, "query": query, "verified": True}),
        change(uri, EMPTY_REPAIR, 2), status_request(7, uri), change(uri, VALID_REPAIR, 3), status_request(8, uri),
        request(9, "textDocument/hover", {"textDocument": {"uri": uri}, "position": position(VALID_REPAIR, span(VALID_REPAIR, "=tx")["start"] + 1)})])
    first = syntax_status(messages, 2, uri, DAMAGED, 1)
    assert first["update_kind"] == "opened"
    owner, _ = declaration(first, "damaged")
    dot = span(DAMAGED, "tx.")
    site = next(site for site in first["sites"] if site["kind"] == "missing_member")
    assert site["declaration_index"] == owner and site["span"] == {"start": dot["end"], "end": dot["end"]}
    assert raw_span(site["receiver"]["root"], DAMAGED) == "tx" and site["receiver"]["projections"] == []
    published = publications(messages, uri)
    assert len(published) == 3 and published[0]["version"] == 1
    independent_leak(published[0], DAMAGED, uri, first)
    assert published[0]["vesper"]["source_blake3"] == first["source_blake3"]
    for identifier in range(3, 7):
        assert response(messages, identifier, -32003)["data"]["status"] == "analysis_unavailable"
    syntax_status(messages, 7, uri, EMPTY_REPAIR, 2)
    assert published[1]["version"] == 2 and published[1]["vesper"]["status"] == "semantic_error"
    leak(published[1], "tx")
    leak(published[1], "other")
    final = syntax_status(messages, 8, uri, VALID_REPAIR, 3)
    assert final["sites"] == [] and all(d["status"] == "complete" for d in final["declarations"])
    assert published[2]["version"] == 3 and published[2]["vesper"]["status"] == "checked" and published[2]["diagnostics"] == []
    assert published[2]["vesper"]["source_blake3"] == final["source_blake3"] != first["source_blake3"]
    assert "FORGED_FIX" not in json.dumps(messages)
    assert "Tx" in response(messages, 9)["contents"]["value"]
    assert any(d["code"] == "E0002" for d in harness.check("original-damaged", DAMAGED, False))
    batch = harness.check("empty-repair", EMPTY_REPAIR, False)
    assert {d["message"] for d in batch if d["code"] == "E0302"} == {LEAK.format("tx"), LEAK.format("other")}
    assert harness.check("valid-repair", VALID_REPAIR, True) == []

    source = "fn damaged(){receiver.}\r\nfn good()->u64{1}"
    messages = harness.lsp([initialize(), open_document(uri, source), status_request(2, uri),
        request(3, "textDocument/hover", {"textDocument": {"uri": uri}, "position": position(source, span(source, "{1}")["start"] + 1), "verified": True}),
        request(4, "textDocument/definition", {"textDocument": {"uri": uri}, "position": position(source, span(source, "good")["start"]), "verified": True}),
        request(5, "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "context": {"only": ["quickfix"], "diagnostics": []}, "verified": True}),
        request(6, "vesper/signatureSearch", {"uri": uri, "query": query, "verified": True})])
    status = syntax_status(messages, 2, uri, source, 1)
    _, good = declaration(status, "good")
    publication = publications(messages, uri)[0]
    assert publication["vesper"]["status"] == "parse_error"
    assert any(f["span"] == good["span"] for f in publication["vesper"]["recovery"]["analyzed_functions"])
    assert all(d["code"].startswith("E000") for d in publication["diagnostics"])
    for identifier in range(3, 7):
        response(messages, identifier, -32003)

    uri = "untitled:incremental/🦀"
    source = "//😀\r\nfn value()->u64{1}\r\nfn sibling()->u64{7}\r\n"
    body = source.replace("{1}", "{22}", 1)
    header = body.replace("fn value()", "fn renamed()", 1)
    messages = harness.lsp([initialize(), open_document(uri, source), status_request(2, uri), change(uri, body, 2), status_request(3, uri),
        change(uri, "fn FORGED(){}", 2), status_request(4, uri), notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": 3},
            "contentChanges": [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 3}}, "text": "x"}]}), status_request(5, uri),
        change(uri, header, 3), status_request(6, uri), notice("textDocument/didClose", {"textDocument": {"uri": uri}}), status_request(7, uri),
        open_document(uri, "fn fresh(){}"), status_request(8, uri)])
    assert syntax_status(messages, 2, uri, source, 1)["update_kind"] == "opened"
    fast = syntax_status(messages, 3, uri, body, 2)
    assert fast["update_kind"] == "body_reparse"
    assert raw_span(declaration(fast, "sibling")[1]["span"], body) == "fn sibling()->u64{7}"
    assert fast == response(messages, 4) == response(messages, 5)
    full = syntax_status(messages, 6, uri, header, 3)
    assert full["update_kind"] == "full_reparse"
    declaration(full, "renamed")
    declaration(full, "sibling")
    response(messages, 7, -32602)
    assert syntax_status(messages, 8, uri, "fn fresh(){}", 1)["update_kind"] == "opened"

    original = "// 🦀 é\r\nlinear struct Tx{}\r\nfn good()->u64{1}\r\nfn damaged(sink tx:Tx){tx.}\r\nfn leak(sink other:Tx){}\r\n"
    changed = original.replace("{1}", "{12345}", 1)
    messages = harness.lsp([initialize(), open_document(uri, original), status_request(2, uri), change(uri, changed, 2), status_request(3, uri)])
    before = syntax_status(messages, 2, uri, original, 1)
    after = syntax_status(messages, 3, uri, changed, 2)
    assert after["update_kind"] == "body_reparse"
    old_site = next(s for s in before["sites"] if s["kind"] == "missing_member")
    new_site = next(s for s in after["sites"] if s["kind"] == "missing_member")
    assert new_site["span"]["start"] == old_site["span"]["start"] + 4
    assert raw_span(new_site["receiver"]["root"], changed) == "tx"
    independent_leak(publications(messages, uri)[1], changed, uri, after)

    uri = "untitled:recovery-budget"
    storm = "fn storm(){" + ";" * 20_000 + "}"
    messages = harness.lsp([initialize(), open_document(uri, "fn old(){}"), status_request(2, uri), change(uri, storm, 2), status_request(3, uri),
        change(uri, "fn fresh(){}", 3), status_request(4, uri)])
    syntax_status(messages, 2, uri, "fn old(){}", 1)
    refusal = response(messages, 3, -32003)["data"]
    assert refusal["status"] == "analysis_unavailable" and refusal["kind"] == "recovery" and refusal["version"] == 2
    assert refusal["reason"] and re.fullmatch("[0-9a-f]{64}", refusal["source_blake3"])
    fresh = syntax_status(messages, 4, uri, "fn fresh(){}", 3)
    assert len(fresh["declarations"]) == 1
    declaration(fresh, "fresh")
    published = publications(messages, uri)
    assert len(published) == 3 and published[1]["vesper"]["status"] == "analysis_unavailable"
    assert published[1]["vesper"]["source_blake3"] == refusal["source_blake3"] and published[1]["version"] == 2
    assert published[1]["diagnostics"][0]["severity"] == 2
    assert published[2]["vesper"]["status"] == "checked" and published[2]["diagnostics"] == []
    for damaged_body in ["unknown(}", "\r\n"]:
        source = "// 🦀\r\nlinear struct Tx{}\r\nfn damaged(sink tx:Tx){" + damaged_body + "\r\nfn leak(/*😀*/sink other:Tx){}\r\n"
        uri = "untitled:delimiter-recovery/😀"
        messages = harness.lsp([initialize(), open_document(uri, source), status_request(2, uri)])
        status = syntax_status(messages, 2, uri, source, 1)
        owner, _ = declaration(status, "damaged")
        assert any(site["kind"] == "missing_delimiter" and site["declaration_index"] == owner for site in status["sites"])
        independent_leak(publications(messages, uri)[0], source, uri, status)

    for source, expected, opaque_kind in [
        ("/* fn forged(){} tx. */\r\n// fn forged_again(){}\r\nfn real(){}", "real", None),
        ('fn text()->String{"fn forged(){} tx."}', "text", None),
        ("fn payload(){opaque!(fn forged(){})}\r\nfn real(){}", "real", "opaque_macro"),
        ('fn text()->String{"unterminated fn forged(){}\r\nfn hidden_in_string(){}', "text", "opaque_lexical"),
    ]:
        uri = "untitled:lexical-boundaries/🦀"
        messages = harness.lsp([initialize(), open_document(uri, source), status_request(2, uri)])
        status = syntax_status(messages, 2, uri, source, 1)
        declaration(status, expected)
        assert not any(d["name"] in ["forged", "forged_again", "hidden_in_string"] for d in status["declarations"])
        if opaque_kind:
            assert any(site["kind"] == opaque_kind for site in status["sites"])
            assert any(d["status"] == "opaque" for d in status["declarations"])
        else:
            assert status["sites"] == [] and len(status["declarations"]) == 1

    for source, updated, kind in [
        ("fn f(){0}\r\nfn later(){}", "fn f(){/*0}\r\nfn later(){}", "full_reparse"),
        ("fn id[T:mod copy](sink x:T)->T{x}", "fn id[T:mod copy](sink x:T)->T{x /*body*/}", "full_reparse"),
        ("fn f()->u64{1}", "fn f()->u64{1}", "unchanged"),
    ]:
        uri = "untitled:fallback"
        messages = harness.lsp([initialize(), open_document(uri, source), status_request(2, uri), change(uri, updated, 2), status_request(3, uri)])
        syntax_status(messages, 2, uri, source, 1)
        status = syntax_status(messages, 3, uri, updated, 2)
        assert status["update_kind"] == kind
        if "/*0" in updated:
            assert any(site["kind"] == "opaque_lexical" for site in status["sites"])
            assert not any(d["name"] == "later" for d in status["declarations"])

    uri, other = "untitled:periodic", "untitled:retained"
    messages = [initialize(), open_document(uri, "// 😀\r\nfn value()->u64{0}"), open_document(other, "fn retained()->u64{7}")]
    for version in range(2, 64):
        current = f"// 😀\r\nfn value()->u64{{{version}}}"
        messages.append(change(uri, current, version))
    messages.extend([status_request(2, uri), status_request(3, other), notice("textDocument/didClose", {"textDocument": {"uri": uri}}), status_request(4, other), status_request(5, uri)])
    messages = harness.lsp(messages)
    declaration(syntax_status(messages, 2, uri, current, 63), "value")
    declaration(syntax_status(messages, 3, other, "fn retained()->u64{7}", 1), "retained")
    declaration(syntax_status(messages, 4, other, "fn retained()->u64{7}", 1), "retained")
    response(messages, 5, -32602)
    print(json.dumps({"status": "passed", "stdio_sessions": harness.sessions, "batch_checks": harness.checks,
                      "scope": "actual original-source syntax recovery; no semantic authority or applied fixes"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary
    executable = pathlib.Path(str(binary) + ".exe")
    if executable.is_file():
        binary = executable
    binary = binary.resolve(strict=True)
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-recovery-", dir=args.work) as temporary:
        run(binary, pathlib.Path(temporary).resolve())


if __name__ == "__main__":
    main()
