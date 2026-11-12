#!/usr/bin/env python3
"""Black-box causal CLI/stdio acceptance. No compiler mock or automatic fixes."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile

MAX_FRAME = 2 * 1024 * 1024
MOVE = "// 🦀\r\nlinear struct Tx{}\r\nfn bad(/* 😀 */sink tx:Tx){\r\n/*🦀*/let Tx{}=tx;\r\n/*😀*/let Tx{}=tx;\r\n}\r\n"
ALIAS = "// 🦀\r\ncopy struct Pair{left:i64,right:i64}\r\nfn both(inout a:Pair,in b:Pair){}\r\nfn bad(/*😀*/inout p:Pair){/*🦀*/both(inout p,/*😀*/in p);}\r\n"
ABORT = "// 🦀\r\nlinear struct Tx{}\r\neffect Ask{fn ask();}\r\nfn bad(/*😀*/sink tx:Tx)eff[Ask]{/*🦀*/Ask::ask();let Tx{}=tx;}\r\n"
IR = "// 🦀\r\nlinear struct Tx {} fn consume(sink tx:Tx,number:i64){let Tx{}=tx;}\r\nfn bad()eff[Fail[i64]]{/*😀*/consume(Tx {},/*🦀*/Fail::raise(1));}\r\n"
SHADOW = "// 🦀\r\nlinear struct Tx{} fn bad(sink tx:Tx){let Tx{}=tx;let tx=Tx{};/*😀*/let Tx{}=tx;/*🦀*/let Tx{}=tx;}"


def span(source, needle, occurrence=0, skip=0, size=None):
    start = -1
    for _ in range(occurrence + 1):
        start = source.index(needle, start + 1)
    start = len(source[:start].encode("utf-8")) + skip
    return {"start": start, "end": start + (len(needle.encode("utf-8")) if size is None else size)}


def initializer(source, occurrence):
    return span(source, "=tx", occurrence, skip=1, size=2)


def fixtures():
    return [
        {"name": "move", "source": MOVE, "code": "E0303", "message": "use of moved place 'tx'",
         "primary": initializer(MOVE, 1),
         "related": [span(MOVE, "sink tx:Tx"), initializer(MOVE, 0)],
         "labels": ["binding introduced here", "earlier ownership-taking use of this place"],
         "help": "A moved non-copy place cannot be used again, including through an overlapping projection."},
        {"name": "alias", "source": ALIAS, "code": "E0301", "message": "overlapping arguments 'p' and 'p' violate alias XOR mutability",
         "primary": span(ALIAS, "in p", skip=3, size=1),
         "related": [span(ALIAS, "both(inout p", skip=len("both(inout "), size=1)],
         "labels": ["earlier overlapping argument evaluated here"],
         "help": "Overlapping argument places are allowed only when both arguments use shared in passing."},
        {"name": "abort", "source": ABORT, "code": "E0402", "message": "potentially abortable effect 'Ask' may discard live linear value 'tx'",
         "primary": span(ABORT, "Ask::ask()"), "related": [span(ABORT, "sink tx:Tx")],
         "labels": ["live linear binding introduced here"],
         "help": "An abortable checkpoint may discard a continuation that still owns this linear binding."},
    ]


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        assert key not in result, f"duplicate response key {key!r}"
        result[key] = value
    return result


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique_object)


def frame(message):
    body = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body


def decode(data):
    output = []
    while data:
        header, separator, tail = data.partition(b"\r\n\r\n")
        assert separator and len(header) <= 8192, "bad response header"
        lengths = []
        for line in header.split(b"\r\n"):
            name, colon, value = line.partition(b":")
            assert colon
            if name.lower() == b"content-length":
                assert value.strip().isdigit()
                lengths.append(int(value.strip()))
        assert len(lengths) == 1 and lengths[0] <= MAX_FRAME
        size = lengths[0]
        assert len(tail) >= size, "truncated response"
        message = load(tail[:size])
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
                   "workspace": {"workspaceEdit": {"documentChanges": True}},
                   "textDocument": {"publishDiagnostics": {"relatedInformation": True, "dataSupport": True}, "codeAction": {"codeActionLiteralSupport": {}}}}})


def open_document(uri, source):
    return notice("textDocument/didOpen", {"textDocument": {"uri": uri, "version": 1, "languageId": "vesper", "text": source}})


def change(uri, source, version):
    return notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": source}]})


def position(source, byte_offset):
    encoded = source.encode("utf-8")
    assert 0 <= byte_offset <= len(encoded)
    assert encoded[max(0, byte_offset - 1):byte_offset + 1] != b"\r\n", "CRLF-interior cause"
    prefix = encoded[:byte_offset].decode("utf-8")  # rejects UTF-8-interior causes
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


def source_range(source, byte_span):
    return {"start": position(source, byte_span["start"]), "end": position(source, byte_span["end"])}


def select(diagnostics, code, message):
    found = [d for d in diagnostics if d["code"] == code and d["message"] == message]
    assert len(found) == 1, f"missing/duplicate compiler cause {code}: {diagnostics!r}"
    return found[0]


def assert_core(diagnostic, case):
    assert diagnostic["span"] == case["primary"]
    assert [label["span"] for label in diagnostic["related"]] == case["related"]
    assert [label["message"] for label in diagnostic["related"]] == case["labels"]
    assert diagnostic["help"] == case["help"]


def assert_lsp(publication, source, core, uri, version):
    assert publication["uri"] == uri and publication["version"] == version
    assert re.fullmatch("[0-9a-f]{64}", publication["vesper"]["source_blake3"])
    diagnostic = select(publication["diagnostics"], core["code"], core["message"])
    assert diagnostic["range"] == source_range(source, core["span"])
    assert diagnostic["severity"] == 1 and diagnostic["source"] == "vesper"
    if core.get("related"):
        expected = [{"location": {"uri": uri, "range": source_range(source, label["span"])},
                     "message": label["message"]} for label in core["related"]]
        assert diagnostic["relatedInformation"] == expected, "nonstandard/stale causal location"
    else:
        assert "relatedInformation" not in diagnostic
    if "help" in core:
        assert diagnostic["data"]["help"] == core["help"]
    else:
        assert "help" not in diagnostic.get("data", {})
    assert "edit" not in diagnostic and "fixes" not in diagnostic


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.cli_checks, self.sessions = 0, 0

    def check(self, name, source, rich=False):
        path = self.work / f"{name}.ves"
        data = source.encode("utf-8")
        path.write_bytes(data)
        command = [self.binary, "check", str(path)]
        if not rich:
            command.append("--json")
        result = subprocess.run(command, capture_output=True, timeout=45, check=False)
        assert result.returncode != 0, f"expected actual compiler rejection: {name}"
        assert len(result.stdout) < MAX_FRAME and len(result.stderr) < MAX_FRAME
        assert path.read_bytes() == data, "check modified source"
        self.cli_checks += 1
        return result.stderr.decode("utf-8") if rich else load(result.stdout)

    def lsp(self, messages):
        messages = [*messages, request(900, "shutdown", {}), notice("exit", {})]
        result = subprocess.run([self.binary, "lsp"], input=b"".join(frame(message) for message in messages), capture_output=True, timeout=45, check=False)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        assert len(result.stdout) <= 8 * MAX_FRAME and len(result.stderr) <= MAX_FRAME
        self.sessions += 1
        return decode(result.stdout)


def publications(messages, uri):
    return [m["params"] for m in messages if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == uri]


def run(binary, work):
    harness = Harness(binary, work)
    for case in fixtures():
        source, name = case["source"], case["name"]
        diagnostic = select(harness.check(name, source), case["code"], case["message"])
        assert_core(diagnostic, case)
        terminal = harness.check(name + "-terminal", source, rich=True)
        for text in [case["code"], case["message"], *case["labels"], case["help"]]:
            assert text in terminal, f"terminal lost causal text: {text!r}"
        assert "\x1b[" not in terminal
        assert "😀" in terminal and "🦀" in terminal
        edited = "// longer 😀🦀 revision\r\n" + source
        after = select(harness.check(name + "-edited", edited), case["code"], case["message"])
        # This is an actual existing file URI. Buffer edits/recovery must not
        # apply edits or write that file; the server only owns in-memory text.
        path = work / f"{name}.ves"
        uri = path.as_uri()
        messages = harness.lsp([initialize(), open_document(uri, source), change(uri, edited, 2), change(uri, "fn main(){}", 3)])
        emitted = publications(messages, uri)
        assert len(emitted) == 3
        assert_lsp(emitted[0], source, diagnostic, uri, 1)
        assert_lsp(emitted[1], edited, after, uri, 2)
        assert emitted[0]["vesper"]["source_blake3"] != emitted[1]["vesper"]["source_blake3"]
        assert emitted[0]["diagnostics"][0]["range"] != emitted[1]["diagnostics"][0]["range"]
        assert emitted[2]["version"] == 3 and emitted[2]["diagnostics"] == []
        assert emitted[2]["vesper"]["status"] == "checked"
        assert path.read_bytes() == source.encode("utf-8"), "stdio wrote/applied a fix to disk"

    shadow = select(harness.check("shadow", SHADOW), "E0303", "use of moved place 'tx'")
    expected = [span(SHADOW, "let tx=Tx{}", skip=4, size=2), initializer(SHADOW, 1)]
    assert [label["span"] for label in shadow["related"]] == expected
    assert shadow["span"] == initializer(SHADOW, 2)
    uri = "untitled:shadow/😀"
    assert_lsp(publications(harness.lsp([initialize(), open_document(uri, SHADOW)]), uri)[0], SHADOW, shadow, uri, 1)

    ir = select(harness.check("ir", IR), "E0410", "abortable effect may abandon linear temporary %0")
    assert ir["span"] == span(IR, "Fail::raise(1)")
    constructor = IR.rindex("Tx {}")
    origin = {"start": len(IR[:constructor].encode("utf-8")), "end": len(IR[:constructor + len("Tx {}")].encode("utf-8"))}
    assert ir["related"] == [{"span": origin, "message": "this IR temporary is defined here"}]
    assert ir["help"]
    terminal = harness.check("ir-terminal", IR, rich=True)
    assert "this IR temporary is defined here" in terminal and ir["help"] in terminal
    uri = "untitled:ir/🦀"
    assert_lsp(publications(harness.lsp([initialize(), open_document(uri, IR)]), uri)[0], IR, ir, uri, 1)

    for name, source in [("empty", "// 🦀\r\nfn main(){missing}"), ("eof", "// 🦀\r\nfn main(){\r\n"), ("separator", "/*\u2028*/fn main(){missing}")]:
        diagnostics = harness.check(name, source)
        for diagnostic in diagnostics:
            assert set(diagnostic) == {"code", "message", "span"}, "empty metadata changed legacy JSON"
        if name == "eof":
            diagnostic = select(diagnostics, "E0002", "expected RBrace, found Eof")
            assert diagnostic["span"] == {"start": len(source.encode("utf-8")), "end": len(source.encode("utf-8"))}
        if name == "separator":
            primary = span(source, "missing")
            terminal = harness.check(name + "-terminal", source, rich=True)
            assert "diagnostic source rendering unavailable:" in terminal
            assert f"E0201: unknown name 'missing' (UTF-8 bytes {primary['start']}..{primary['end']})" in terminal
        uri = f"untitled:{name}/🦀"
        publication = publications(harness.lsp([initialize(), open_document(uri, source)]), uri)[0]
        for diagnostic in diagnostics:
            assert_lsp(publication, source, diagnostic, uri, 1)

    uri = "file:///never-read-or-written/forged.ves"
    forged = {"code": "E0303", "message": "FORGED_CAUSE", "relatedInformation": [
        {"location": {"uri": "file:///secret", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}, "message": "FORGED_CAUSE"}],
        "data": {"help": "FORGED_HELP", "replacement": "FORGED_FIX", "verified": True}}
    messages = harness.lsp([initialize(), open_document(uri, "fn main(){}"), request(2, "textDocument/codeAction", {
        "textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "context": {"diagnostics": [forged]}})])
    assert publications(messages, uri)[0]["diagnostics"] == []
    assert next(m for m in messages if m.get("id") == 2)["result"] == []
    encoded = json.dumps(messages)
    assert "FORGED_" not in encoded and "file:///secret" not in encoded
    source = "// 🦀\r\nlinear struct Tx{} fn bad(sink tx:Tx){" + "let Tx{}=tx;" * 70 + "}"
    uri = "untitled:causal-budget/😀"
    publication = publications(harness.lsp([initialize(), open_document(uri, source)]), uri)[0]
    assert publication["version"] == 1 and publication["vesper"]["status"] == "analysis_unavailable"
    assert re.fullmatch("[0-9a-f]{64}", publication["vesper"]["source_blake3"])
    diagnostics = publication["diagnostics"]
    assert len(diagnostics) == 1 and diagnostics[0]["severity"] == 2
    assert diagnostics[0]["code"] == "VESPER_ANALYSIS_UNAVAILABLE"
    assert diagnostics[0]["range"] == source_range(source, {"start": 0, "end": 0})
    assert "relatedInformation" not in diagnostics[0]
    case = fixtures()[0]
    core = select(harness.check("capability-move", MOVE), case["code"], case["message"])
    assert_core(core, case)
    for related, data in [(None, None), (False, False), (True, False), (False, True), (True, True)]:
        init = initialize()
        text_document = init["params"]["capabilities"]["textDocument"]
        text_document.pop("publishDiagnostics")
        flags = {}
        if related is not None:
            flags["relatedInformation"] = related
        if data is not None:
            flags["dataSupport"] = data
        if flags:
            text_document["publishDiagnostics"] = flags
        uri = "untitled:publication-capabilities/🦀"
        publication = publications(harness.lsp([init, open_document(uri, MOVE)]), uri)[0]
        assert publication["version"] == 1
        diagnostic = select(publication["diagnostics"], core["code"], core["message"])
        assert diagnostic["severity"] == 1 and diagnostic["range"] == source_range(MOVE, core["span"])
        assert ("relatedInformation" in diagnostic) == (related is True)
        assert ("help" in diagnostic.get("data", {})) == (data is True)
        if related is True:
            assert diagnostic["relatedInformation"] == [
                {"location": {"uri": uri, "range": source_range(MOVE, label["span"])}, "message": label["message"]}
                for label in core["related"]]
        if data is True:
            assert diagnostic["data"]["help"] == core["help"]
    legacy = initialize()
    legacy["params"]["capabilities"]["textDocument"].pop("publishDiagnostics")
    uri = "untitled:legacy-unavailable"
    publication = publications(harness.lsp([legacy, open_document(uri, source)]), uri)[0]
    assert publication["vesper"]["status"] == "analysis_unavailable"
    assert publication["diagnostics"][0]["data"]["status"] == "analysis_unavailable"
    assert publication["diagnostics"][0]["data"]["kind"] == "resource"
    print(json.dumps({"status": "passed", "cli_checks": harness.cli_checks, "stdio_sessions": harness.sessions,
                      "scope": "real ownership, alias, resumption and IR origins; no applied fixes"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary
    # Native Windows Python needs .exe even when MSYS resolves the bare alias.
    executable = pathlib.Path(str(binary) + ".exe")
    if executable.is_file():
        binary = executable
    binary = binary.resolve(strict=True)
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-diagnostics-", dir=args.work) as temporary:
        run(binary, pathlib.Path(temporary).resolve())


if __name__ == "__main__":
    main()
