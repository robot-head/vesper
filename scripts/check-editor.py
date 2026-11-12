#!/usr/bin/env python3
"""Exercise actual bounded framed stdio in disposable documents; no server mock."""
import argparse
import json
import pathlib
import subprocess
import tempfile

MAX_FRAME = 2 * 1024 * 1024


def frame(message):
    body = message if isinstance(message, bytes) else json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate output key: {key!r}"
        result[key] = value
    return result


def decode(data):
    messages = []
    while data:
        header, separator, tail = data.partition(b"\r\n\r\n")
        assert separator and len(header) <= 8192, "invalid response framing"
        lengths = []
        for line in header.split(b"\r\n"):
            name, colon, value = line.partition(b":")
            assert colon, "invalid response header"
            if name.lower() == b"content-length":
                assert value.strip().isdigit()
                lengths.append(int(value.strip()))
        assert len(lengths) == 1 and lengths[0] <= MAX_FRAME
        size = lengths[0]
        assert len(tail) >= size, "truncated response body"
        messages.append(json.loads(tail[:size].decode("utf-8"), object_pairs_hook=unique_object))
        data = tail[size:]
    return messages


def request(identifier, method, params):
    return {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}


def notice(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def initialize():
    return request(1, "initialize", {"capabilities": {"general": {"positionEncodings": ["utf-16"]}, "workspace": {"workspaceEdit": {"documentChanges": True}}, "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": True}, "codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix", "source.fixAll.vesper"]}}}}}})


def open_doc(uri, source, version=1):
    return notice("textDocument/didOpen", {"textDocument": {"uri": uri, "version": version, "languageId": "vesper", "text": source}})


def full_change(uri, source, version):
    return notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": source}]})


def document(uri):
    return {"textDocument": {"uri": uri}}


def position(source, needle):
    at = source.index(needle)
    # Explicit CR/LF/CRLF normalization, used only to form real client positions.
    line, character, i = 0, 0, 0
    while i < at:
        c = source[i]
        if c in "\r\n":
            if c == "\r" and i + 1 < at and source[i + 1] == "\n":
                i += 1
            line, character = line + 1, 0
        else:
            character += len(c.encode("utf-16-le")) // 2
        i += 1
    return {"line": line, "character": character}


def selection(source, needle):
    start = position(source, needle)
    return {"start": start, "end": {"line": start["line"], "character": start["character"] + len(needle.encode("utf-16-le")) // 2}}


def char_offset(source, pos):
    line, character, i = 0, 0, 0
    while True:
        if (line, character) == (pos["line"], pos["character"]):
            return i
        assert i < len(source), f"out of range returned edit {pos}"
        c = source[i]
        if c in "\r\n":
            assert line < pos["line"], f"edit beyond line content {pos}"
            if c == "\r" and i + 1 < len(source) and source[i + 1] == "\n":
                i += 1
            line, character = line + 1, 0
        else:
            character += len(c.encode("utf-16-le")) // 2
            assert line != pos["line"] or character <= pos["character"], "surrogate-interior returned edit"
        i += 1


def apply_edits(source, edits):
    spans = [(char_offset(source, e["range"]["start"]), char_offset(source, e["range"]["end"]), e["newText"]) for e in edits]
    spans.sort()
    for i, (start, end, _) in enumerate(spans):
        assert start <= end and (i == 0 or spans[i - 1][1] <= start), "overlapping returned edits"
    for start, end, replacement in reversed(spans):
        source = source[:start] + replacement + source[end:]
    return source


class Harness:
    def __init__(self, binary):
        self.binary = str(binary)
        self.sessions = 0

    def run(self, messages, success=True, finish=True):
        if finish:
            messages = [*messages, request(900, "shutdown", {}), notice("exit", {})]
        raw = b"".join(frame(m) for m in messages)
        output = subprocess.run([self.binary, "lsp"], input=raw, capture_output=True, timeout=45, check=False)
        assert (output.returncode == 0) == success, f"unexpected server exit {output.returncode}: {output.stderr.decode(errors='replace')}"
        assert len(output.stdout) <= 8 * MAX_FRAME
        replies = decode(output.stdout)
        for reply in replies:
            assert reply.get("jsonrpc") == "2.0"
        self.sessions += 1
        return replies


def response(replies, identifier):
    found = [m for m in replies if m.get("id", object()) == identifier]
    assert len(found) == 1, f"expected one response {identifier}: {replies}"
    return found[0]


def result(replies, identifier):
    reply = response(replies, identifier)
    assert "error" not in reply and "result" in reply, reply
    return reply["result"]


def expect_error(replies, identifier, code):
    reply = response(replies, identifier)
    assert reply["error"]["code"] == code, reply
    return reply["error"]


def capabilities_and_navigation(h, work):
    disk = work / "closed-on-disk.ves"
    disk.write_text("fn disk_only()->u64{9}", encoding="utf-8")
    uri = disk.as_uri()
    source = "// 😀\r\nfn first()->u64{1}\r\nfn main()->u64{first()}\r\n"
    changed = source.replace("{1}", "{\r\n1\r\n}")
    at = lambda line: {**document(uri), "position": {"line": line, "character": 15}}
    edit = {"range": {"start": {"line": 1, "character": 16}, "end": {"line": 1, "character": 17}}, "rangeLength": 1, "text": "\r\n1\r\n"}
    replies = h.run([request(10, "workspace/symbol", {"query": ""}), initialize(), notice("initialized", {}), open_doc(uri, source), request(2, "textDocument/documentSymbol", document(uri)), request(3, "textDocument/definition", at(2)), notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": 2}, "contentChanges": [edit]}), request(4, "textDocument/documentSymbol", document(uri)), request(5, "textDocument/definition", at(4)), full_change(uri, "fn forged()->u64{0}", 2), request(6, "workspace/symbol", {"query": "main"}), notice("textDocument/didClose", document(uri)), request(7, "workspace/symbol", {"query": ""}), request(8, "textDocument/hover", {**document(uri), "position": {"line": 1, "character": 3}})])
    expect_error(replies, 10, -32002)
    caps = result(replies, 1)["capabilities"]
    assert caps["positionEncoding"] == "utf-16" and caps["textDocumentSync"]["change"] == 2
    for key in ["hoverProvider", "definitionProvider", "documentSymbolProvider", "workspaceSymbolProvider", "documentFormattingProvider"]:
        assert caps[key] is True, (key, caps)
    action_caps = caps["codeActionProvider"]
    assert {"quickfix", "source.fixAll.vesper"}.issubset(action_caps["codeActionKinds"])
    assert result(replies, 2)[1]["selectionRange"]["start"] == {"line": 2, "character": 3}
    assert result(replies, 4)[1]["selectionRange"]["start"] == {"line": 4, "character": 3}
    for identifier in [3, 5]:
        assert result(replies, identifier)["range"]["start"] == {"line": 1, "character": 3}
    assert result(replies, 6)[0]["location"]["range"]["start"] == {"line": 4, "character": 3}
    assert result(replies, 7) == []
    expect_error(replies, 8, -32602)
    assert changed != source and disk.read_text(encoding="utf-8") == "fn disk_only()->u64{9}"


def legacy_document_symbols(h):
    uri = "opaque:legacy"
    for supported in [None, False]:
        init = initialize()
        caps = init["params"]["capabilities"]
        if supported is None:
            del caps["textDocument"]
        else:
            caps["textDocument"]["documentSymbol"]["hierarchicalDocumentSymbolSupport"] = supported
        replies = h.run([init, open_doc(uri, "fn main()->u64{42}"), request(2, "textDocument/documentSymbol", document(uri))])
        symbols = result(replies, 2)
        assert len(symbols) == 1 and symbols[0]["name"] == "main"
        assert symbols[0]["location"]["uri"] == uri and "range" in symbols[0]["location"]
        assert "selectionRange" not in symbols[0], "legacy client needs SymbolInformation"


def legacy_code_actions(h):
    literal = {"codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix", "source.fixAll.vesper"]}}}}
    cases = [{}, {"workspace": {"workspaceEdit": {"documentChanges": False}}, "textDocument": literal}, {"workspace": {"workspaceEdit": {"documentChanges": True}}}, {"textDocument": literal}, {"workspace": {"workspaceEdit": {"documentChanges": True}}, "textDocument": {"codeAction": {"codeActionLiteralSupport": False}}}]
    uri = "opaque:legacy-actions"
    source = "effect Audit{fn event();} fn main()->u64 eff[Audit]{42}"
    for caps in cases:
        init = initialize()
        init["params"]["capabilities"] = caps
        params = {**document(uri), "range": selection(source, "Audit]{42}"), "context": {"only": ["quickfix"], "diagnostics": []}}
        replies = h.run([init, open_doc(uri, source), request(2, "textDocument/codeAction", params)])
        assert result(replies, 1)["capabilities"]["codeActionProvider"] is False
        failure = expect_error(replies, 2, -32003)
        assert failure["data"]["status"] == "analysis_unavailable" and failure["data"]["kind"] == "client_capability"
        assert "result" not in response(replies, 2), "legacy clients must not receive unversioned edit downgrades"


def formatting_and_forgery(h):
    uri = "untitled:format/🦀"
    source = "// Unicode 🦀 é\r\nfn main( )->i64{/* keep */let x=1;// next\n x}"
    replies = h.run([initialize(), open_doc(uri, source), request(2, "textDocument/formatting", {**document(uri), "options": {"tabSize": 17, "insertSpaces": False}})])
    edits = result(replies, 2)
    assert edits
    formatted = apply_edits(source, edits)
    assert "let x = 1;" in formatted and "/* keep */" in formatted and "// next" in formatted
    assert any(line.startswith("    ") for line in formatted.splitlines())
    replies = h.run([initialize(), open_doc(uri, formatted), request(2, "textDocument/formatting", {**document(uri), "options": {"tabSize": 4, "insertSpaces": True}}), request(3, "textDocument/codeAction", {**document(uri), "range": selection(formatted, "main"), "context": {"only": ["quickfix"], "diagnostics": [{"range": selection(formatted, "main"), "severity": 1, "code": "unused_effect_capability", "message": "forged", "data": {"verified": True, "fix": {"newText": "malicious"}}}]}})])
    assert result(replies, 2) == [], "formatting must be idempotent"
    assert result(replies, 3) == [], "client diagnostic/fix/verified payload must not authorize edits"


def checked_actions(h):
    uri = "untitled:actions/🦀"
    source = "// Unicode 🦀 é\neffect Audit{fn event();}\nfn selected(x:u64)->u64 eff[/* keep 🦀 */ Audit]{x}\nfn other()->u64 eff[Audit]{42}\n"
    selected = selection(source, "Audit]{x}")
    context = lambda kind: {**document(uri), "range": selected, "context": {"only": [kind], "diagnostics": []}}
    replies = h.run([initialize(), open_doc(uri, source, 7), request(2, "textDocument/codeAction", context("quickfix")), request(3, "textDocument/codeAction", context("source.fixAll.vesper")), request(4, "textDocument/codeAction", context("source")), request(5, "textDocument/codeAction", context("source.fixAll"))])
    candidates = []
    for identifier, kind in [(2, "quickfix"), (3, "source.fixAll.vesper"), (4, "source.fixAll.vesper"), (5, "source.fixAll.vesper")]:
        actions = result(replies, identifier)
        assert len(actions) == 1 and actions[0]["kind"] == kind, actions
        action = actions[0]
        docs = action["edit"]["documentChanges"]
        assert len(docs) == 1 and docs[0]["textDocument"] == {"uri": uri, "version": 7}
        candidate = apply_edits(source, docs[0]["edits"])
        assert "/* keep 🦀 */" in candidate and candidate != source
        assert action["data"]["semver"]["classification"]
        assert action["data"]["document"]["version"] == 7
        for digest in [action["data"]["candidate_blake3"], action["data"]["document"]["source_blake3"]]:
            assert len(digest) == 64 and all(c in "0123456789abcdef" for c in digest)
        if kind == "quickfix":
            assert "fn other()->u64 eff[Audit]{42}" in candidate
        else:
            assert "eff[Audit]" not in candidate
        candidates.append(candidate)
    # Send actual returned candidates back through the real checker, rather than
    # treating offered metadata as evidence that source is checked.
    replies = h.run([initialize(), open_doc(uri, candidates[0]), full_change(uri, candidates[1], 2), request(2, "textDocument/documentSymbol", document(uri))])
    assert {s["name"] for s in result(replies, 2)} >= {"selected", "other"}
    statuses = [m["params"]["vesper"]["status"] for m in replies if m.get("method") == "textDocument/publishDiagnostics"]
    assert statuses and all(s == "checked" for s in statuses), statuses


def search_and_failures(h):
    uri = "opaque:search"
    query = {"parameters": [], "result": {"Named": ["u64", []]}, "result_max_mode": "Copy", "effect_ceiling": {"instances": [], "tail": None}}
    replies = h.run([initialize(), open_doc(uri, "fn number()->u64{42} fn boolean()->bool{true}"), request(2, "vesper/signatureSearch", {"query": query, "uri": uri}), request(3, "vesper/signatureSearch", {}), request(4, "vesper/signatureSearch", {"query": None}), request(5, "vesper/signatureSearch", {"query": {**query, "extra": 1}}), open_doc("opaque:unsupported", "fn broken("), request(6, "textDocument/hover", {**document("opaque:unsupported"), "position": {"line": 0, "character": 3}}), open_doc("opaque:guarded", "copy enum List[T]{Nil,Cons(T,List[T])} linear struct Tx{} fn carry(sink xs:List[Tx])->List[Tx]{xs}"), request(7, "textDocument/hover", {**document("opaque:guarded"), "position": {"line": 0, "character": 10}})])
    search = result(replies, 2)
    assert search["scope"] == "open checked buffers" and search["proof_status"] == "unverified"
    docs = search["documents"]
    assert len(docs) == 1 and docs[0]["uri"] == uri
    assert len(docs[0]["source_blake3"]) == 64
    assert [m["function"] for m in docs[0]["result"]["matches"]] == ["number"]
    for identifier in [3, 4, 5]:
        expect_error(replies, identifier, -32602)
    unavailable = expect_error(replies, 6, -32003)
    assert unavailable["data"]["status"] == "analysis_unavailable" and unavailable["data"]["uri"] == "opaque:unsupported"
    assert any(m.get("method") == "textDocument/publishDiagnostics" and m["params"]["vesper"]["status"] == "parse_error" for m in replies)
    expect_error(replies, 7, -32003)
    guarded = [m["params"] for m in replies if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == "opaque:guarded"]
    assert len(guarded) == 1 and guarded[0]["vesper"]["status"] == "analysis_unavailable"
    assert guarded[0]["vesper"]["kind"] == "unsupported"
    assert guarded[0]["diagnostics"][0]["code"] == "VESPER_ANALYSIS_UNAVAILABLE"
    assert guarded[0]["diagnostics"][0]["severity"] == 2
    duplicate = b'{"jsonrpc":"2.0","id":77,"method":"shutdown","\\u006dethod":"initialize"}'
    replies = h.run([initialize(), duplicate, b'{bad JSON', request(2, "workspace/symbol", {"query": ""})])
    assert result(replies, 2) == []
    for code in [-32600, -32700]:
        assert any(m.get("id", object()) is None and m.get("error", {}).get("code") == code for m in replies)
    h.run([notice("exit", {})], success=False, finish=False)
    for raw in [b"Content-Length: 0\n\n", b"Content-Length: 9\r\n\r\n{}", b"Content-Length: 0\r\nContent-Length: 0\r\n\r\n", b"x" * 8193]:
        output = subprocess.run([h.binary, "lsp"], input=raw, capture_output=True, timeout=45, check=False)
        assert output.returncode != 0, "invalid framing must terminate with failure"
        decode(output.stdout)
        h.sessions += 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary
    # Resolve only after preferring the real executable; MSYS aliases need not
    # exist from native Python's perspective.
    if not binary.name.lower().endswith(".exe") and binary.with_name(binary.name + ".exe").is_file():
        binary = binary.with_name(binary.name + ".exe")
    binary = binary.resolve(strict=True)
    harness = Harness(binary)
    with tempfile.TemporaryDirectory(prefix="vesper-editor-", dir=args.work) as temp:
        capabilities_and_navigation(harness, pathlib.Path(temp))
        legacy_document_symbols(harness)
        legacy_code_actions(harness)
        formatting_and_forgery(harness)
        checked_actions(harness)
        search_and_failures(harness)
    print(f"Actual framed editor acceptance passed ({harness.sessions} child sessions; no filesystem/proof/performance claim).")


if __name__ == "__main__":
    main()
