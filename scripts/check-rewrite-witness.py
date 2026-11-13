#!/usr/bin/env python3
"""Exercise the actual default witness-gated AST optimizer and --no-opt CLI."""
import argparse
import json
import pathlib
import subprocess
import tempfile


def unique(pairs):
    value = {}
    for key, item in pairs:
        assert key not in value, f"duplicate JSON key {key!r}"
        value[key] = item
    return value


def load(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def body(module, name):
    found = [item["Function"]["body"] for item in module["items"]
             if "Function" in item and item["Function"]["name"] == name]
    assert len(found) == 1
    return found[0]


def tail(expression):
    while isinstance(expression["kind"], dict) and "Block" in expression["kind"]:
        expression = expression["kind"]["Block"]["tail"]
        assert expression is not None
    return expression


def nodes(value):
    if isinstance(value, dict):
        if "kind" in value and "span" in value:
            yield value
        for child in value.values():
            yield from nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from nodes(child)


def intrinsic_calls(expression):
    result = []
    for value in nodes(expression):
        if not isinstance(value["kind"], dict) or "Call" not in value["kind"]:
            continue
        call = value["kind"]["Call"]
        name = call["callee"]["kind"].get("Name") if isinstance(call["callee"]["kind"], dict) else None
        if name in {"wrapping_add", "wrapping_sub", "wrapping_mul"}:
            result.append((name, call))
    return result


class Harness:
    def __init__(self, binary, work):
        self.binary, self.work = str(binary), work
        self.commands, self.programs = 0, 0

    def source(self, text):
        path = self.work / f"program-{self.programs}.ves"
        path.write_bytes(text.encode("utf-8"))
        self.programs += 1
        return path

    def execute(self, mode, path, *options, status=0):
        original = path.read_bytes()
        output = subprocess.run([self.binary, mode, str(path), *options],
                                stdin=subprocess.DEVNULL, capture_output=True,
                                timeout=60, check=False)
        self.commands += 1
        assert output.returncode == status, (mode, options, output.returncode, output.stdout, output.stderr)
        assert len(output.stdout) <= 4 * 1024 * 1024 and len(output.stderr) <= 1024 * 1024
        assert path.read_bytes() == original
        return output

    def hir(self, path, no_opt=False):
        return self.execute("emit", path, "--emit", "hir", *(["--no-opt"] if no_opt else [])).stdout

    def agree(self, source, expected):
        path = self.source(source)
        # check is baseline validity, and returns before the optimizer gate.
        assert load(self.execute("check", path, "--json").stdout) == []
        for mode in ["run", "jit"]:
            for options in [[], ["--no-opt"]]:
                assert self.execute(mode, path, *options).stdout.decode("utf-8").strip() == expected, source
        return path


def guarded(kind, expression):
    return f"fn main() -> {kind} {{ handle Fail[ArithOverflow] in {{ {expression} }} with {{ return v => v, raise(e, k) => 42, }} }}"


def run(binary, work):
    h = Harness(binary, work)
    folds = [("2 + 3", 5), ("9 - 4", 5), ("2 * 3", 6),
             ("wrapping_add(2, 3)", 5), ("wrapping_sub(9, 4)", 5), ("wrapping_mul(2, 3)", 6)]
    for kind in ["u8", "u32", "u64", "i64"]:
        for expression, expected in folds:
            path = h.agree(f"fn main() -> {kind} {{ {expression} }}", str(expected))
            original, encoded = h.hir(path, True), h.hir(path)
            before, after = load(original), load(encoded)
            assert tail(body(before, "main"))["kind"] != tail(body(after, "main"))["kind"]
            assert tail(body(after, "main"))["kind"] == {"Literal": {"Int": expected}}
            assert h.hir(path) == encoded

    for kind, maximum in [("u8", "255"), ("u32", "4294967295"), ("u64", "18446744073709551615"), ("i64", "9223372036854775807")]:
        source = f"fn f(x: {kind}) -> {kind} {{ (x + 0) * 0 }} fn main() -> {kind} {{ f(42) }}"
        path = h.agree(source, "0")
        assert tail(body(load(h.hir(path)), "f"))["kind"] == {"Literal": {"Int": 0}}
        source = f"fn f(x: {kind}) -> {kind} eff [Fail[ArithOverflow]] {{ (x + 1) * 0 }} " + guarded(kind, f"f({maximum})")
        path = h.agree(source, "42")
        assert tail(body(load(h.hir(path)), "f"))["kind"] != {"Literal": {"Int": 0}}

    for kind, expression, expected in [
        ("u64", "wrapping_mul(18446744073709551615, 18446744073709551615)", 1),
        ("u64", "9223372036854775808 * 1", 9223372036854775808),
        ("u64", "wrapping_add(18446744073709551615, 0)", 18446744073709551615),
        ("u64", "wrapping_mul(9223372036854775808, 2)", 0),
        ("i64", "wrapping_mul(-9223372036854775808, -1)", -9223372036854775808),
    ]:
        path = h.agree(f"fn main() -> {kind} {{ {expression} }}", str(expected))
        assert tail(body(load(h.hir(path)), "main"))["kind"] == {"Literal": {"Int": expected}}
    h.agree(guarded("u64", "18446744073709551615 * 18446744073709551615"), "42")

    source = "// original source é🦀\nfn f(x: i64, y: i64) -> i64 { wrapping_add(wrapping_add(x, 0), y) } fn main() -> i64 { f(40, 2) }"
    path = h.agree(source, "42")
    before, after = load(h.hir(path, True)), load(h.hir(path))
    original = [(name, call["callee"]["span"]) for name, call in intrinsic_calls(body(before, "f"))]
    calls = intrinsic_calls(body(after, "f"))
    assert len(original) == 2 and len(calls) == 1
    for name, call in calls:
        span = call["callee"]["span"]
        assert (name, span) in original and span["start"] < span["end"]
        assert source.encode("utf-8")[span["start"]:span["end"]].decode("utf-8") == name
        assert call["types"] == [] and len(call["args"]) == 2
        assert all(arg["pass"] == "In" and arg["explicit"] is False for arg in call["args"])

    for name, implementation, a, b in [("wrapping_add", "wrapping_mul(a, b)", 6, 7),
                                        ("wrapping_sub", "wrapping_add(a, b)", 40, 2),
                                        ("wrapping_mul", "wrapping_add(a, b)", 40, 2)]:
        source = f"fn main() -> i64 {{ let {name} = |a: i64, b: i64| {implementation}; {name}({a} + 0, {b} + 0) }}"
        path = h.agree(source, "42")
        before, after = load(h.hir(path, True)), load(h.hir(path))
        original = tail(body(before, "main"))["kind"]["Call"]
        call = tail(body(after, "main"))["kind"]["Call"]
        assert original["callee"] == call["callee"]
        assert [argument["expr"]["kind"] for argument in call["args"]] == [{"Literal": {"Int": a}}, {"Literal": {"Int": b}}]

    source = "fn f(x: i64) -> i64 { (x + 0) / 2 } fn main() -> i64 { f(84) }"
    path = h.agree(source, "42")
    division = tail(body(load(h.hir(path)), "f"))["kind"]["Binary"]
    assert division[0] == "/" and division[1]["kind"] == {"Name": "x"}
    source = "fn f(x: i64) -> i64 eff [Fail[RuntimeError]] { x / 1 } fn main() -> i64 { handle Fail[RuntimeError] in { f(20 + 22) } with { return v => v + 0, raise(e, k) => 42 + 0, } }"
    path = h.agree(source, "42")
    handler = tail(body(load(h.hir(path)), "main"))["kind"]["Handle"]
    assert tail(handler["ret"]["body"])["kind"] == {"Name": "v"}
    assert tail(handler["ops"][0]["body"])["kind"] == {"Literal": {"Int": 42}}
    assert tail(handler["body"])["kind"]["Call"]["args"][0]["expr"]["kind"] == {"Literal": {"Int": 42}}
    h.agree("fn next(inout n: i64) -> i64 { n = wrapping_add(n, 1); n } fn main() -> i64 { let n = 0; let a = wrapping_mul(next(inout n), 0); let b = next(inout n); wrapping_add(wrapping_mul(n, 100), wrapping_add(wrapping_mul(a, 10), b)) }", "202")
    for arguments, expected in [("overflow(), division()", "10"), ("division(), overflow()", "20")]:
        source = "fn overflow() -> i64 eff [Fail[ArithOverflow]] { 9223372036854775807 + 1 } fn division() -> i64 eff [Fail[RuntimeError]] { 1 / 0 } " + f"fn main() -> i64 {{ handle Fail[RuntimeError] in {{ handle Fail[ArithOverflow] in {{ wrapping_mul({arguments}) }} with {{ return v => v, raise(e, k) => 10, }} }} with {{ return v => v, raise(e, k) => 20, }} }}"
        h.agree(source, expected)

    source = "fn f(x: i64) -> i64 { x" + " + 0" * 70 + " } fn main() -> i64 { f(42) }"
    path = h.agree(source, "42")
    original, rewritten = h.hir(path, True), h.hir(path)
    assert original == rewritten == h.hir(path), "resource refusal must keep the complete island unchanged"

    path = h.source("fn main() -> u8 { 255 + 1 }")
    original = h.execute("run", path, "--json", "--no-opt", status=1)
    rewritten = h.execute("run", path, "--json", status=1)
    assert original.stdout == rewritten.stdout and original.stderr == rewritten.stderr
    assert any(diagnostic["code"] == "E0401" for diagnostic in load(rewritten.stdout))
    return h


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() and pathlib.Path(str(binary) + ".exe").is_file():
        binary = pathlib.Path(str(binary) + ".exe")
    assert binary.is_file(), binary
    with tempfile.TemporaryDirectory(prefix="vesper-rewrite-witness-") as directory:
        harness = run(binary, pathlib.Path(directory))
        print(json.dumps({"status": "passed", "commands": harness.commands, "programs": harness.programs,
                          "scope": "actual default witness-gated AST optimizer/evaluator/JIT; no proof certificate or performance claim"}))


if __name__ == "__main__":
    main()
