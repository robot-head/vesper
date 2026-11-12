#!/usr/bin/env python3
"""Check real CLI output against disposable fixtures; no performance threshold.

The shell driver builds the executable. This helper can also check an explicitly
provided existing binary without touching Cargo, useful during integration.
"""
from __future__ import annotations

import argparse
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import subprocess


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


class LocalHtml(HTMLParser):
    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        require(tag not in {"script", "iframe", "img", "link", "object", "embed"},
                f"unexpected active/remote asset tag: {tag}")
        for name, _ in attrs:
            require(name.lower() not in {"src", "href", "srcset"},
                    f"unexpected external asset attribute: {name}")
            require(not name.lower().startswith("on"), f"unexpected event attribute: {name}")


def check(binary: Path, root: Path, work: Path) -> None:
    require(binary.is_file(), f"missing built executable: {binary}")
    require(work.is_dir(), f"missing disposable directory: {work}")
    fixtures = {
        name: root / relative for name, relative in {
            "lint": "crates/vesper-lint/examples/lint_demo.ves",
            "api": "crates/vesper-doc/examples/api.ves",
            "query": "crates/vesper-doc/examples/query.json",
        }.items()
    }
    original = {name: path.read_bytes() for name, path in fixtures.items()}

    def run(*args: object, success: bool = True) -> subprocess.CompletedProcess[str]:
        command = [str(binary), *(str(arg) for arg in args)]
        result = subprocess.run(command, cwd=root, text=True, encoding="utf-8",
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=60, check=False)
        require((result.returncode == 0) == success,
                f"unexpected exit {result.returncode}: {command!r}\n"
                f"stdout={result.stdout}\nstderr={result.stderr}")
        return result

    def report(*args: object, success: bool = True) -> dict:
        # json.loads rejects extra stdout documents, including on expected errors.
        result = run(*args, "--json", success=success)
        data = json.loads(result.stdout)
        require(isinstance(data, dict), f"expected JSON object: {result.stdout}")
        return data

    def source(name: str, text: str) -> Path:
        path = work / name
        path.write_bytes(text.encode("utf-8"))
        return path

    try:
        fmt = source("format.ves", "// Disposable formatter acceptance\n"
                     "fn main( )->i64{let answer=42;answer}\n")
        before = fmt.read_bytes()
        dirty = report("fmt", fmt, "--check", success=False)
        require(dirty["status"] == "needs_format" and dirty["changed"],
                "formatter check did not detect actual formatting")
        require(fmt.read_bytes() == before, "fmt --check mutated input")
        formatted = report("fmt", fmt)
        require(formatted["status"] == "formatted" and formatted["changed"],
                "formatter did not perform its requested write")
        after = fmt.read_bytes()
        require(after != before and b"// Disposable formatter acceptance" in after,
                "formatting must preserve the comment and change whitespace")
        unchanged = report("fmt", fmt, "--check")
        require(unchanged["status"] == "unchanged" and not unchanged["changed"],
                "formatted source is not idempotent")
        require(fmt.read_bytes() == after, "second fmt check mutated input")
        require(json.loads(run("check", fmt, "--json").stdout) == [],
                "formatted source failed actual checking")
        require(run("run", fmt).stdout.strip() == "42", "formatted source did not execute as 42")

        lint = work / "lint-demo.ves"
        lint.write_bytes(original["lint"])
        initial = report("lint", lint)
        require(any(d["code"] == "L0102" for d in initial["diagnostics"]),
                "actual lint demo has no unused-effect evidence")
        fixed = report("lint", lint, "--fix", "--semver-safe")
        require(fixed["status"] == "checked" and fixed["changed"]
                and fixed["applied_edits"] > 0, "lint fix did not apply validated edits")
        require(fixed["semver"]["classification"] in {"patch", "minor"}
                and fixed["semver"]["required_adapters"] == [],
                "lint --semver-safe did not produce compatible interface evidence")
        require(lint.read_bytes() != original["lint"], "lint demo copy was not actually fixed")
        require(json.loads(run("check", lint, "--json").stdout) == [],
                "fixed lint source failed Core checking")
        require(run("run", lint).stdout.strip() == "42", "fixed lint demo did not execute as 42")
        final = report("lint", lint)
        require(not any(d["code"] == "L0102" for d in final["diagnostics"]),
                "unused-effect finding survived successful fix")

        api, query = work / "api.ves", work / "query.json"
        api.write_bytes(original["api"])
        query.write_bytes(original["query"])
        doc = report("doc", api, "--query", query)
        require(doc["status"] == "valid" and doc["schema_version"] == 1,
                "checked documentation did not validate")
        require([hit["function"] for hit in doc["search"]["matches"]] == ["close"],
                "full structural query must match only close")
        require(doc["document"]["types"], "actual native type documentation is empty")
        html = run("doc", api).stdout
        require("&lt;b&gt;native&lt;/b&gt;" in html and "<b>native</b>" not in html,
                "source documentation was not HTML-escaped")
        require("unverified" in html and "Content-Security-Policy" in html,
                "HTML omitted its proof scope or local-content policy")
        LocalHtml().feed(html)
        require(run("doc", api).stdout == html, "static documentation is nondeterministic")
        require(api.read_bytes() == original["api"] and query.read_bytes() == original["query"],
                "documentation/search mutated its copied source/query")

        positive = source("positive-docs.ves", """//! ```vesper,entry=first
//! fn first() -> i64 { helper(41) }
//! ```
/// ```vesper,entry=second
/// fn second() -> i64 { helper(8) }
/// ```
fn helper(x: i64) -> i64 { wrapping_add(x, 1) }
""")
        passed = report("test", "--doc", positive)
        body = passed["report"]
        require(passed["passed"] and body["passed"] and body["base_diagnostics"] == [],
                "positive documentation failed real parser/checker/execution")
        require([example["result"] for example in body["examples"]] == ["42", "9"],
                "positive entries did not return their executed values")
        require(all(example["passed"] for example in body["examples"]),
                "a positive example was silently omitted")
        require(body["batch"] == {
            "module_count": 1, "compilation_count": 1, "defined_functions": 3,
            "entry_count": 2, "runs": 4, "max_entry_work": body["batch"]["max_entry_work"],
        } and body["batch"]["max_entry_work"] > 0,
                "positive doctests did not share one actual helper/module")
        negative_text = """/// ```vesper,compile_fail(E0302)
/// fn bad() { let tx = Tx {}; }
/// ```
linear struct Tx {}
"""
        negative = source("negative-docs.ves", negative_text)
        passed = report("test", "--doc", negative)
        body = passed["report"]
        require(passed["passed"] and body["base_diagnostics"] == [] and body["batch"] is None,
                "negative example was not an isolated compile-fail check")
        require(len(body["examples"]) == 1, "negative example count differs")
        example = body["examples"][0]
        require(example["passed"] and example["expected_code"] == "E0302"
                and example["diagnostics"]
                and all(d["code"] == "E0302" for d in example["diagnostics"]),
                "negative example matched unrelated diagnostics")
        wrong = source("wrong-negative-docs.ves", negative_text.replace("compile_fail(E0302)",
                                                                        "compile_fail(E0301)"))
        failed = report("test", "--doc", wrong, success=False)
        require(not failed["passed"] and not failed["report"]["examples"][0]["passed"],
                "wrong requested diagnostic falsely passed")

        bench = source("benchmark.ves", "fn helper() -> u64 { 42 }\n"
                       "fn main() -> u64 { helper() }\n")
        bench_before = bench.read_bytes()
        measured = report("bench", bench, "--samples", 6, "--warmup", 0, "--iterations", 8)
        require(measured["schema"] == "vesper.bench/1" and measured["status"] == "measured",
                "benchmark without a threshold must remain measurement-only")
        require(measured["provider"] == "std::time::Instant" and measured["unit"] == "nanoseconds",
                "benchmark misreported provider or wall-clock units")
        require(measured["body_calls"] == 192 and len(measured["pairs"]) == 6,
                "benchmark did not actually execute six ABBA epochs")
        require(measured["config"] == {"epochs": 6, "warmup_epochs": 0,
                                       "operations_per_window": 8}, "benchmark config differs")
        for arm in ("candidate", "baseline"):
            jit = measured[arm]["jit"]
            require(jit["modules"] == jit["compilations"] == 1 and jit["entries"] == 1
                    and jit["defined_functions"] == 2 and jit["runs"] == 96,
                    f"{arm} did not compile once and execute 96 real calls")
            require(re.fullmatch(r"[0-9a-f]{64}", measured[arm]["source_blake3"]) is not None,
                    f"{arm} source identity missing")
        require(measured["candidate"]["source_blake3"] == measured["baseline"]["source_blake3"],
                "default baseline was not compiled from the same source snapshot")
        for pair in measured["pairs"]:
            require(int(pair["baseline_total_ns"]) > 0 and int(pair["candidate_total_ns"]) > 0
                    and pair["operations_per_arm"] == 16,
                    "nonpositive or mismatched benchmark evidence")
        require(measured["analysis"]["gate_decision"] is None,
                "measurement-only acceptance accidentally applied a speed threshold")
        require(measured["analysis"]["interval"] is not None,
                "six valid epochs should have a supported median interval")
        require(measured["evidence"] == {"hardware_pmu": "unavailable",
                                        "zero_allocation": "unavailable",
                                        "vectorization": "unavailable"},
                "unsupported hardware/static evidence was promoted")
        require(bench.read_bytes() == bench_before, "benchmark rewrote source")
        print("Source tooling acceptance: fmt/check, lint/fix/run=42, typed doc search/HTML, "
              "shared JIT doctests/exact E0302, and 192 measured callback calls passed")
    finally:
        for name, path in fixtures.items():
            require(path.read_bytes() == original[name], f"tracked {name} fixture was mutated")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    args = parser.parse_args()
    check(args.binary.resolve(), args.root.resolve(), args.work.resolve())


if __name__ == "__main__":
    main()
