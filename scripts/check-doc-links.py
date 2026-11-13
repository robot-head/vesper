#!/usr/bin/env python3
"""Verify actual checked-source intra-doc links and offline HTML fragments."""
import argparse
import collections
import html.parser
import json
import pathlib
import re
import subprocess
import tempfile

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "tests/fixtures/doc_links"
MAX_OUTPUT = 2 * 1024 * 1024


def unique(pairs):
    value = {}
    for key, item in pairs:
        assert key not in value, f"duplicate JSON key {key!r}"
        value[key] = item
    return value


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def fixture(name):
    return (FIXTURES / f"{name}.ves").read_bytes().decode("utf-8")


class Fragments(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = collections.Counter()
        self.hrefs = collections.Counter()
        self.tags = []

    def handle_starttag(self, tag, attrs):
        self.tags.append(tag)
        for key, value in attrs:
            if key == "id":
                self.ids[value] += 1
            if key == "href":
                self.hrefs[value] += 1


def source_span(source, span):
    data = source.encode("utf-8")
    start, end = span["start"], span["end"]
    assert type(start) is int and type(end) is int
    assert 0 <= start < end <= len(data), span
    data[:start].decode("utf-8")
    data[:end].decode("utf-8")
    return data[start:end].decode("utf-8")


def validate_links(report, source, expected):
    assert report["status"] == "valid", report
    document = report["document"]
    assert document["source_bytes"] == len(source.encode("utf-8"))
    links = document["intra_doc_links"]
    assert links["status"] == "resolved" and links["diagnostics"] == [], links
    assert links["source_bytes"] == len(source.encode("utf-8"))
    targets = [(t["path"], t["kind"]) for t in links["targets"]]
    assert len(targets) == len(set(targets)) and targets == sorted(targets)
    assert {t[0] for t in targets}.isdisjoint({"Option", "Result", "Fail", "RuntimeError"})
    refs = links["references"]
    assert [r["target"]["path"] for r in refs] == expected, refs
    previous = -1
    anchors = {}
    for reference in refs:
        assert reference["span"]["start"] > previous
        previous = reference["span"]["start"]
        raw = source_span(source, reference["span"])
        assert raw == f'[{reference["display"]}]', (raw, reference)
        assert reference["display"].strip("`") == reference["target"]["path"]
        key = (reference["target"]["path"], reference["target"]["kind"])
        assert key in targets
        anchor = reference["anchor"]
        assert re.fullmatch(r"[A-Za-z0-9_-]+", anchor)
        assert anchor.endswith(key[0].encode("utf-8").hex())
        assert key not in anchors or anchors[key] == anchor
        anchors[key] = anchor
    assert any("unverified" in value.lower() for value in document["limitations"])
    return links


def validate_html(data, links):
    text = data.decode("utf-8")
    parser = Fragments()
    parser.feed(text)
    assert "script" not in parser.tags and "iframe" not in parser.tags
    assert "unverified" in text.lower()
    expected = collections.Counter("#" + r["anchor"] for r in links["references"])
    assert parser.hrefs == expected, (parser.hrefs, expected)
    for href in parser.hrefs:
        assert href.startswith("#") and parser.ids[href[1:]] == 1, href
    assert all(count == 1 for count in parser.ids.values())
    return text


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work, self.commands = str(binary), work, 0

    def execute(self, *args, status=0):
        output = subprocess.run(
            [self.binary, *(str(arg) for arg in args)],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=45,
            check=False,
        )
        self.commands += 1
        assert output.returncode == status, (args, output.returncode, output.stdout, output.stderr)
        assert len(output.stdout) <= MAX_OUTPUT and len(output.stderr) <= MAX_OUTPUT
        return output

    def write(self, name, source):
        path = self.work / name
        path.write_bytes(source.encode("utf-8"))
        return path

    def accepted(self, name, source, expected):
        path = self.write(name + ".ves", source)
        first = self.execute("doc", path, "--json").stdout
        second = self.execute("doc", path, "--json").stdout
        assert first == second
        links = validate_links(load(first), source, expected)
        a, b = self.execute("doc", path).stdout, self.execute("doc", path).stdout
        assert a == b
        html = validate_html(a, links)
        assert path.read_bytes() == source.encode("utf-8")
        return path, links, html

    def rejected(self, name, source, code, raw=None):
        path = self.write(name + ".ves", source)
        output = self.execute("doc", path, "--json", status=1)
        report = load(output.stdout)
        assert report["status"] == "invalid" and "document" not in report
        assert code in report["error"], report
        if raw is not None:
            data = source.encode("utf-8")
            start = data.index(raw.encode("utf-8"))
            assert f"{start}..{start + len(raw.encode('utf-8'))}" in report["error"], report
        output_path = self.work / (name + ".html")
        failed = self.execute("doc", path, "-o", output_path, status=1)
        assert failed.stdout == b"" and not output_path.exists()
        assert path.read_bytes() == source.encode("utf-8")
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() and pathlib.Path(str(binary) + ".exe").is_file():
        binary = pathlib.Path(str(binary) + ".exe")
    assert binary.is_file(), binary
    with tempfile.TemporaryDirectory(prefix="vesper-doc-links-") as directory:
        harness = Harness(binary, pathlib.Path(directory))
        expected = ["Tx", "close", "Tx::commit", "Ask", "Ask::ask", "Score", "Tx", "Tx::commit", "Tx"]
        local = fixture("local")
        path, links, html = harness.accepted("local", local, expected)
        targets = {(t["path"], t["kind"]) for t in links["targets"]}
        assert targets == {
            ("Tx", "type"), ("Tx::commit", "inherent_method"), ("Ask", "effect"),
            ("Ask::ask", "effect_operation"), ("Score", "trait"),
            ("close", "function"), ("main", "function"),
        }, targets
        assert links["references"][0]["span"]["start"] == local.encode("utf-8").index(b"[Tx]")
        harness.accepted("crlf", local.replace("\n", "\r\n"), expected)
        _, _, escaped = harness.accepted("exclusions", fixture("exclusions"), ["present"])
        assert "<script>" not in escaped and "&lt;script&gt;" in escaped
        assert "example.invalid" in escaped

        for name, code, raw in [
            ("unknown", "unknown", "[missing]"),
            ("ambiguous", "ambiguous", "[Channel]"),
            ("unsupported_self", "unsupported", "[Self::commit]"),
            ("unsupported_package", "unsupported", "[@local/tx::commit]"),
            ("unsupported_generic", "unsupported", "[Box[u64]]"),
        ]:
            harness.rejected(name, fixture(name), code, raw)

        # An unterminated code span/fence cannot suppress links in another group.
        for prefix in ["//! ```\n//! [Hidden]\n\n", "//! ` [Hidden]\n// ordinary boundary\n", "//! ~~~\n//! [Hidden]\nfn boundary() {}\n"]:
            harness.rejected("group-reset-" + str(harness.commands), prefix + "/// [missing]\nfn main() {}\n", "unknown", "[missing]")

        # Rename a real declaration while retaining the original reference.
        edited = local.replace("fn close(sink", "fn finish(sink")
        harness.rejected("changed-declaration", edited, "unknown", "[close]")
        repaired = edited.replace("[close]", "[finish]")
        harness.accepted("repaired", repaired, ["finish" if p == "close" else p for p in expected])

        harness.rejected("invalid-syntax", "/// [missing]\nfn {\n", "source parse failed")
        harness.rejected("invalid-type", "/// [missing]\nfn main() -> bool { 42 }\n", "source check failed")
        harness.rejected("linear-leak", "/// [missing]\nlinear struct Ticket {} fn main() { let ticket = Ticket {}; }\n", "source check failed")
        harness.rejected("reference-limit", "//! " + "[present] " * 1025 + "\nfn present() {}\n", "resource")
        harness.rejected("path-limit", "//! [" + "a" * 1025 + "]\nfn main() {}\n", "resource")
        harness.rejected("source-limit", "//" + "x" * (128 * 1024) + "\nfn main() {}\n", "limit")

        output = harness.work / "existing.html"
        saved = harness.execute("doc", path, "-o", output)
        assert saved.stdout == b"" and output.read_bytes() == html.encode("utf-8")
        harness.execute("doc", path, "-o", output, "--json", status=1)
        assert output.read_bytes() == html.encode("utf-8")
        harness.execute("doc", path, "--approved", status=1)
        assert path.read_bytes() == local.encode("utf-8")
        print(json.dumps({"status": "passed", "commands": harness.commands,
                          "scope": "actual checked local intra-doc references and escaped offline HTML; no proof certificate"}))


if __name__ == "__main__":
    main()
