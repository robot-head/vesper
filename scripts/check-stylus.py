#!/usr/bin/env python3
"""Exercise actual macro/partial-buffer formatting and current UTF-16 stdio edits."""
import argparse
import json
import pathlib
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "stylus"
MAX_OUTPUT = 4 * 1024 * 1024


def load(data):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            assert key not in value, f"duplicate JSON key {key!r}"
            value[key] = item
        return value
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def frame(message):
    data = json.dumps(message, ensure_ascii=False).encode("utf-8")
    return f"Content-Length: {len(data)}\r\n\r\n".encode("ascii") + data


def decode(data):
    messages = []
    while data:
        header, separator, tail = data.partition(b"\r\n\r\n")
        assert separator and len(header) <= 8192
        sizes = [line.partition(b":")[2].strip() for line in header.split(b"\r\n") if line.partition(b":")[0].lower() == b"content-length"]
        assert len(sizes) == 1 and sizes[0].isdigit()
        size = int(sizes[0])
        assert size <= MAX_OUTPUT and len(tail) >= size
        message = load(tail[:size])
        assert message["jsonrpc"] == "2.0"
        messages.append(message)
        data = tail[size:]
    return messages


def request(number, method, params):
    return {"jsonrpc": "2.0", "id": number, "method": method, "params": params}


def notice(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def offset(source, position):
    lines = source.splitlines(keepends=True)
    line, character = position["line"], position["character"]
    assert isinstance(line, int) and isinstance(character, int) and 0 <= line <= len(lines)
    if line == len(lines):
        assert character == 0 and (not source or source.endswith(("\n", "\r")))
        return len(source)
    text = lines[line].removesuffix("\n").removesuffix("\r")
    units = 0
    for index, char in enumerate(text):
        if units == character:
            return sum(map(len, lines[:line])) + index
        units += len(char.encode("utf-16-le")) // 2
    assert units == character, "edit falls inside a UTF-16 surrogate or outside line"
    return sum(map(len, lines[:line])) + len(text)


def apply(source, edits):
    spans = [(offset(source, e["range"]["start"]), offset(source, e["range"]["end"]), e["newText"]) for e in edits]
    spans.sort()
    assert all(a <= b for a, b, _ in spans)
    assert all(a[1] <= b[0] for a, b in zip(spans, spans[1:]))
    result = source
    for start, end, text in reversed(spans):
        result = result[:start] + text + result[end:]
    return result


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work, self.commands, self.sessions, self.serial = str(binary), work, 0, 0, 0

    def execute(self, *args, success=True, input_data=None):
        result = subprocess.run([self.binary, *(str(a) for a in args)], input=input_data, capture_output=True, timeout=45, check=False)
        assert len(result.stdout) <= MAX_OUTPUT and len(result.stderr) <= MAX_OUTPUT
        assert result.returncode == (0 if success else 1), f"{args!r}\n{result.returncode}\n{result.stdout.decode(errors='replace')}\n{result.stderr.decode(errors='replace')}"
        self.commands += 1
        return result

    def source(self, data):
        self.serial += 1
        path = self.work / f"source-{self.serial}.ves"
        path.write_bytes(data)
        return path

    def formatted(self, data, recovery=False):
        path = self.source(data)
        flags = ["--recovery"] if recovery else []
        mode = "recovery" if recovery else "source"
        check = load(self.execute("fmt", path, *flags, "--check", "--json", success=False).stdout)
        assert check["schema_version"] == 1 and check["mode"] == mode and check["changed"] and check["check"]
        assert path.read_bytes() == data
        value = load(self.execute("fmt", path, *flags, "--json").stdout)
        assert value["schema_version"] == 1 and value["mode"] == mode and value["changed"] and not value["check"]
        if recovery:
            assert value["edits"] > 0
        after = path.read_bytes()
        assert after != data and value["input_bytes"] == len(data) and value["output_bytes"] == len(after)
        stable = load(self.execute("fmt", path, *flags, "--check", "--json").stdout)
        assert not stable["changed"] and stable["status"] == "unchanged"
        if recovery:
            assert stable["edits"] == 0
        assert path.read_bytes() == after
        return path, after

    def session(self, sources):
        uri = "untitled:stylus/😀"
        messages = [request(1, "initialize", {"capabilities": {"general": {"positionEncodings": ["utf-16"]}}})]
        for version, source in enumerate(sources, 1):
            params = {"textDocument": {"uri": uri, "version": version}}
            if version == 1:
                params["textDocument"].update(languageId="vesper", text=source)
                messages.append(notice("textDocument/didOpen", params))
            else:
                params["contentChanges"] = [{"text": source}]
                messages.append(notice("textDocument/didChange", params))
            messages.append(request(10 + version, "textDocument/formatting", {"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": True}}))
        messages += [request(800, "textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}), request(900, "shutdown", {}), notice("exit", {})]
        replies = decode(self.execute("lsp", input_data=b"".join(map(frame, messages))).stdout)
        self.sessions += 1
        results = []
        for version in range(1, len(sources) + 1):
            reply = next(m for m in replies if m.get("id") == 10 + version)
            assert "error" not in reply, reply
            assert isinstance(reply["result"], list)
            results.append(reply["result"])
        publications = [m["params"] for m in replies if m.get("method") == "textDocument/publishDiagnostics"]
        assert len(publications) == len(sources)
        for version, publication in enumerate(publications, 1):
            assert publication["uri"] == uri and publication["version"] == version
            assert publication["vesper"]["status"] == "parse_error" and publication["diagnostics"]
        assert next(m for m in replies if m.get("id") == 800)["error"]["code"] == -32003
        return results


def run(binary, work):
    h = Harness(binary, work)
    macro = (FIXTURES / "macro.ves").read_bytes()
    path, after = h.formatted(macro)
    for payload in [b"// fn fake(){} stays a comment", b"/* quote {$fake} */", '"fn fake(){ keep!(0) } 😀"'.encode()]:
        assert payload in after
    expanded = h.execute("forge", "expand", path).stdout
    artifact = h.source(expanded)
    assert load(h.execute("check", artifact, "--json").stdout) == []
    assert h.execute("run", artifact).stdout.strip() == b"42"
    assert path.read_bytes() == after

    recovery = (FIXTURES / "recovery.ves").read_bytes()
    path = h.source(recovery)
    h.execute("fmt", path, "--json", success=False)
    assert path.read_bytes() == recovery
    path, after = h.formatted(recovery, True)
    assert after.startswith(recovery.split(b"fn main")[0]) and after.endswith(b"\n\n")
    assert load(h.execute("check", path, "--json", success=False).stdout)

    opaque = (FIXTURES / "opaque.ves").read_bytes()
    _, after = h.formatted(opaque, True)
    assert after.startswith(opaque.split(b"fn literal")[0])
    assert b'"fn string_fake( )->i64{42}"' in after
    only = b"fn opaque(){unknown!{ fn fake( )->i64{42} }}"
    path = h.source(only)
    result = load(h.execute("fmt", path, "--recovery", "--check", "--json").stdout)
    assert not result["changed"] and result["edits"] == 0 and path.read_bytes() == only

    for data in [b"fn main(){/* unclosed", b"fn main(){\xff}", b" " * (256 * 1024 + 1), b"macro m($x:ty)->expr{quote{$x}} fn main(){m!(i64)}", b"macro m($x:expr)->expr{quote{$x}} fn main(){m!(m!(42))}"]:
        path = h.source(data)
        h.execute("fmt", path, "--json", success=False)
        assert path.read_bytes() == data
        if data.startswith((b"fn main(){", b" ")):
            h.execute("fmt", path, "--recovery", "--json", success=False)
            assert path.read_bytes() == data
    path = h.source(b"fn main(){42}")
    h.execute("fmt", path, "--bad", success=False)
    # A directory is walked; --check reports its noncanonical files without writing.
    h.execute("fmt", work, "--recovery", "--check", success=False)
    assert path.read_bytes() == b"fn main(){42}"

    for source, code in [(b"linear struct Tx{} fn leak(sink tx:Tx){}", "E0302"), (b"linear struct Tx{} fn make()->Tx{Tx{}} fn consume(sink tx:Tx){let Tx{}=tx;} fn main(){let tx=make();consume(tx);consume(tx)}", "E0303")]:
        path, _ = h.formatted(source)
        diagnostics = load(h.execute("check", path, "--json", success=False).stdout)
        assert any(item["code"] == code for item in diagnostics)

    first = "// 😀\r\nfn damaged(){let x=;}\r\nfn main( )->i64{42}\r\n"
    current = "// 😀 updated\r\n\r\nfn damaged(){let x=;}\r\n// 🦀\r\nfn main( )->i64{41}\r\n"
    edits = h.session([first, current])
    for source, result in zip([first, current], edits):
        assert result
        candidate = apply(source, result)
        assert candidate != source and candidate.startswith(source.split("fn main")[0])
        assert source[source.index("fn damaged"):source.index("fn damaged") + len("fn damaged(){let x=;}")] in candidate
        assert h.session([candidate]) == [[]]
    assert not any(p.name.startswith(".vesper-fmt-") for p in work.iterdir())
    print(json.dumps({"status": "passed", "commands": h.commands, "stdio_sessions": h.sessions, "explicit_expanded_source_runs": 1, "scope": "syntax formatting; damaged source remains semantically unavailable"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = pathlib.Path(str(args.binary) + ".exe") if pathlib.Path(str(args.binary) + ".exe").is_file() else args.binary
    binary = binary.resolve(strict=True)
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-stylus-", dir=args.work) as directory:
        run(binary, pathlib.Path(directory).resolve())


if __name__ == "__main__":
    main()
