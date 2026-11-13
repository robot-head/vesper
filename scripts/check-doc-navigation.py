#!/usr/bin/env python3
"""Exercise actual current-source intra-doc definitions through framed stdio."""
import argparse
import json
import pathlib
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/doc_navigation"
MAX_FRAME = 2 * 1024 * 1024


def unique(pairs):
    value = {}
    for key, item in pairs:
        assert key not in value, f"duplicate key {key!r}"
        value[key] = item
    return value


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def fixture(name):
    return (FIXTURES / f"{name}.ves").read_bytes().decode("utf-8")


def request(identifier, method, params):
    return {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}


def notice(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def document(uri):
    return {"textDocument": {"uri": uri}}


def opened(uri, version, source):
    return notice("textDocument/didOpen", {"textDocument": {"uri": uri, "version": version, "languageId": "vesper", "text": source}})


def changed(uri, version, source):
    return notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": source}]})


def position(source, offset):
    prefix = source[:offset]
    return {"line": prefix.count("\n"), "character": len(prefix.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2}


def definition(identifier, uri, source, needle, delta=0):
    return request(identifier, "textDocument/definition", {**document(uri), "position": position(source, source.index(needle) + delta)})


def location(uri, source, marker, skip, name):
    start = source.index(marker) + skip
    assert source[start:start + len(name)] == name
    return {"uri": uri, "range": {"start": position(source, start), "end": position(source, start + len(name))}}


def frame(message):
    data = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(data)}\r\n\r\n".encode("ascii") + data


def decode(data):
    replies = []
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
        assert len(sizes) == 1 and 0 < sizes[0] <= MAX_FRAME and len(tail) >= sizes[0]
        value = load(tail[:sizes[0]])
        assert value["jsonrpc"] == "2.0"
        replies.append(value)
        data = tail[sizes[0]:]
    return replies


def response(replies, identifier):
    found = [r for r in replies if r.get("id") == identifier]
    assert len(found) == 1, (identifier, found)
    return found[0]


def result(replies, identifier):
    value = response(replies, identifier)
    assert "error" not in value, value
    return value["result"]


def error(replies, identifier, code, kind=None):
    value = response(replies, identifier)
    assert value["error"]["code"] == code and "result" not in value, value
    if kind is not None:
        assert value["error"]["data"]["kind"] == kind, value
    return value["error"]


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.sessions, self.requests = 0, 0
        self.disk = work / "unchanged-🦀.ves"
        self.disk_bytes = b"fn on_disk() -> i64 { 99 } fn main() -> i64 { on_disk() }\n"
        self.disk.write_bytes(self.disk_bytes)
        self.uri = self.disk.as_uri()

    def session(self, messages):
        messages = [request(1, "initialize", {"capabilities": {}}), *messages,
                    request(9999, "shutdown", {}), notice("exit", {})]
        output = subprocess.run([self.binary, "lsp"], input=b"".join(frame(m) for m in messages), capture_output=True, timeout=45, check=False)
        assert output.returncode == 0, (output.stdout, output.stderr)
        assert len(output.stdout) <= 8 * MAX_FRAME and len(output.stderr) <= MAX_FRAME
        replies = decode(output.stdout)
        assert result(replies, 1)["capabilities"]["definitionProvider"] is True
        assert result(replies, 9999) is None
        assert self.disk.read_bytes() == self.disk_bytes
        self.sessions += 1
        self.requests += sum("id" in m for m in messages)
        return replies


def run(binary, work):
    h = Harness(binary, work)
    uri = h.uri
    targets = [
        ("[close]", "\nfn close", 4, "close"),
        ("[`Tx::commit`]", "fn commit", 3, "commit"),
        ("[Ask::ask]", "linear effect Ask { fn ask", 23, "ask"),
        ("[Score]", "trait Score", 6, "Score"),
        ("[i64::score]", "impl i64 { fn score", 14, "score"),
        ("[Point::score]", "impl Point { fn score", 16, "score"),
        ("[Tx]", "linear struct Tx", 14, "Tx"),
        ("[Ask]", "linear effect Ask", 14, "Ask"),
        ("[ask]", "\nfn ask", 4, "ask"),
    ]
    for source in [fixture("local"), fixture("local").replace("\n", "\r\n")]:
        messages, expected = [opened(uri, 1, source)], {}
        for identifier, (reference, marker, skip, name) in enumerate(targets, 10):
            messages.append(definition(identifier, uri, source, reference))
            expected[identifier] = location(uri, source, marker, skip, name)
        # Brackets and backticks belong to the half-open hit range, the end does not.
        messages.extend([definition(30, uri, source, "[`Tx::commit`]", 1),
                         definition(31, uri, source, "[`Tx::commit`]", len("[`Tx::commit`]")),
                         definition(32, uri, source, "; close", 2),
                         definition(35, uri, source, "[`Tx::commit`]", len("[`Tx::commit`]") - 1),
                         request(33, "textDocument/hover", {**document(uri), "position": position(source, source.index("[close]"))})])
        midpoint = position(source, source.index("😀"))
        midpoint["character"] += 1
        messages.append(request(34, "textDocument/definition", {**document(uri), "position": midpoint}))
        replies = h.session(messages)
        for identifier, expected_location in expected.items():
            assert result(replies, identifier) == expected_location, (identifier, result(replies, identifier), expected_location)
        assert result(replies, 30) == result(replies, 35) == expected[11]
        assert result(replies, 31) is None and result(replies, 33) is None
        assert result(replies, 32) == location(uri, source, "let close", 4, "close")
        error(replies, 34, -32602)

    source = fixture("exclusions")
    excluded = ["NoInline", "NoDouble", "NoEscape", "NoImage", "NoUrl", "NoReference", "id", "T-CallProj", "NoFence", "NoTilde", "NoFourSlash", "NoOrdinary", "NoString"]
    replies = h.session([opened(uri, 1, source), definition(10, uri, source, "[present]"),
                         *(definition(i, uri, source, "[" + name, 1) for i, name in enumerate(excluded, 20))])
    assert result(replies, 10) == location(uri, source, "fn present", 3, "present")
    assert all(result(replies, i) is None for i in range(20, 20 + len(excluded)))

    source = fixture("collisions")
    rejected = ["[Channel]", "[missing]", "[Self::commit]", "[@local/type::method]", "[Box[i64]]", "[i64]"]
    replies = h.session([opened(uri, 1, source), *(definition(i, uri, source, raw) for i, raw in enumerate(rejected, 10)),
                         definition(30, uri, source, "[Channel::read]"), definition(31, uri, source, "[current]")])
    assert all(result(replies, i) is None for i in range(10, 10 + len(rejected)))
    assert result(replies, 30) == location(uri, source, "fn read", 3, "read")
    assert result(replies, 31) == location(uri, source, "fn current", 3, "current")

    original = "//! 😀 [current]\r\nfn current() -> i64 { 42 }\r\nfn main() -> i64 { current() }\r\n"
    renamed = original.replace("current", "renamed")
    broken_link = renamed.replace("[renamed]", "[current]")
    final = original.replace("current", "final_name")
    invalid = position(original, original.index("😀"))
    invalid["character"] += 1
    replies = h.session([
        opened(uri, 1, original), definition(10, uri, original, "[current]"),
        changed(uri, 2, broken_link), definition(11, uri, broken_link, "[current]"),
        changed(uri, 3, renamed), definition(12, uri, renamed, "[renamed]"),
        changed(uri, 2, original), definition(13, uri, renamed, "[renamed]"),
        notice("textDocument/didChange", {"textDocument": {"uri": uri, "version": 4}, "contentChanges": [{"range": {"start": invalid, "end": invalid}, "text": "x"}]}),
        definition(14, uri, renamed, "[renamed]"), changed(uri, 4, final), definition(15, uri, final, "[final_name]"),
        changed(uri, 5, "x" * (128 * 1024 + 1)), definition(16, uri, final, "[final_name]"),
        notice("textDocument/didClose", document(uri)), definition(17, uri, final, "[final_name]"),
        opened(uri, 1, original), definition(18, uri, original, "[current]"),
    ])
    assert result(replies, 10) == result(replies, 18) == location(uri, original, "fn current", 3, "current")
    assert result(replies, 11) is None
    assert result(replies, 12) == result(replies, 13) == result(replies, 14) == location(uri, renamed, "fn renamed", 3, "renamed")
    assert result(replies, 15) == result(replies, 16) == location(uri, final, "fn final_name", 3, "final_name")
    error(replies, 17, -32602)

    # Link resource refusal must not degrade an ordinary lexical definition.
    source = "//! " + "[present]; " * 1025 + "\nfn present() -> i64 { 42 } fn main() -> i64 { present() }\n"
    replies = h.session([opened(uri, 1, source), definition(10, uri, source, "[present]"), definition(11, uri, source, "{ present()", 2)])
    error(replies, 10, -32003, "resource")
    assert result(replies, 11) == location(uri, source, "fn present", 3, "present")

    macro = "//! [score]\nfn score(in value: ()) -> i64 { 42 }\nmacro make() -> expr { quote { () } }\nfn main() -> i64 { score(make!()) }\n"
    replies = h.session([opened(uri, 1, macro), definition(10, uri, macro, "[score]"),
                         request(11, "vesper/macroPreview", {"textDocument": {"uri": uri, "version": 1}}),
                         definition(12, uri, macro, "[score]")])
    error(replies, 10, -32003, "parse")
    assert result(replies, 11)["status"] == "expanded_checked"
    error(replies, 12, -32003, "parse")
    for source, kind in [("//! [main]\nfn main() -> bool { 42 }\n", "semantic"), ("//! [main]\nfn broken(\n", "parse")]:
        replies = h.session([opened(uri, 1, source), definition(10, uri, source, "[main]")])
        error(replies, 10, -32003, kind)
    return h


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() and pathlib.Path(str(binary) + ".exe").is_file():
        binary = pathlib.Path(str(binary) + ".exe")
    assert binary.is_file(), binary
    with tempfile.TemporaryDirectory(prefix="vesper-doc-navigation-") as directory:
        h = run(binary, pathlib.Path(directory))
        print(json.dumps({"status": "passed", "stdio_sessions": h.sessions, "jsonrpc_requests": h.requests,
                          "scope": "actual current-source intra-doc definition locations; no semantic overlay or proof"}))


if __name__ == "__main__":
    main()
