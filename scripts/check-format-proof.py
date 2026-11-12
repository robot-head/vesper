#!/usr/bin/env python3
"""Exercise explicit actual-Lean formatter document proofs and emitted witnesses."""
import argparse
import json
import os
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures" / "format_proof"
MAX_OUTPUT = 4 * 1024 * 1024


def load(data):
    def unique(pairs):
        result = {}
        for name, value in pairs:
            assert name not in result
            result[name] = value
        return result
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


class Harness:
    def __init__(self, binary, lean, model, work):
        self.binary, self.lean, self.model, self.work = binary, lean, model, work
        self.commands, self.kernel_reports, self.serial = 0, 0, 0

    def execute(self, command, success=True, cwd=None, env=None):
        result = subprocess.run([str(a) for a in command], capture_output=True, timeout=360, check=False, cwd=cwd, env=env)
        assert len(result.stdout) <= MAX_OUTPUT and len(result.stderr) <= MAX_OUTPUT
        assert result.returncode == (0 if success else 1), f"{command!r}\n{result.returncode}\n{result.stdout.decode(errors='replace')}\n{result.stderr.decode(errors='replace')}"
        self.commands += 1
        return result

    def source(self, data):
        self.serial += 1
        path = self.work / f"source-{self.serial}.ves"
        path.write_bytes(data)
        return path

    def verify(self, path, *flags, success=True):
        original = path.read_bytes()
        result = self.execute([self.binary, "verify", path, *flags], success=success)
        assert path.read_bytes() == original
        return result

    def checked(self, source):
        path = self.source(source)
        report = load(self.verify(path, "--format", "--json", "--lean", self.lean, "--timeout", "300").stdout)
        assert report["schema"] == "vesper.format-proof/1" and report["status"] == "kernel_checked" and report["scope"] == "formatter_document_model"
        assert report["input_bytes"] == len(source) and report["output_bytes"] > 0 and report["document_nodes"] > 0
        assert "4.24.0" in report["lean_version"]
        for field in ["source_blake3", "output_blake3"]:
            assert re.fullmatch("[0-9a-f]{64}", report[field])
        audit = report["audit"]
        assert audit["accepted"] and audit["kernel_replayed"] and not audit["cache_eligible"]
        assert audit["forbidden_axioms"] == []
        names = set()
        for theorem in audit["theorems"]:
            names.add(theorem["name"])
            assert theorem["axioms"] == []
        assert {"Vesper.StylusGenerated.comments_preserved", "Vesper.StylusSafety.projection_narrowing_preserves_disjointness"}.issubset(names)
        assert not {"world", "checked_source", "semantic_checked", "rust_renderer_verified", "program_verified"}.intersection(report)
        self.kernel_reports += 1
        return path, report

    def rejected(self, data, flags=("--format", "--json")):
        path = self.source(data)
        result = self.verify(path, *flags, success=False)
        if "--emit-lean" in flags:
            assert result.stdout == b"", "failed witness emission wrote output"
        else:
            report = load(result.stdout)
            assert report["status"] == "rejected" and report["stage"] and report["message"]
            assert not {"audit", "source_blake3", "output_blake3", "lean_version", "document_nodes"}.intersection(report)
        return path

    def replay(self, witness):
        directory = self.work / "independent-kernel"
        directory.mkdir()
        (directory / "StylusAndLintSafety.lean").write_bytes(self.model.read_bytes())
        (directory / "Witness.lean").write_bytes(witness)
        env = {key: value for key, value in os.environ.items() if key not in {"LEAN_PATH", "LEAN_SRC_PATH", "LEAN_PLUGIN_PATH"}}
        env["LEAN_PATH"] = str(directory)
        for module in ["StylusAndLintSafety", "Witness"]:
            self.execute([self.lean, "--trust=0", "-o", module + ".olean", module + ".lean"], cwd=directory, env=env)
        (directory / "Inspect.lean").write_text("import Witness\n#check Vesper.StylusGenerated.comments_preserved\n#print axioms Vesper.StylusGenerated.comments_preserved\n#print axioms Vesper.StylusSafety.projection_narrowing_preserves_disjointness\n", encoding="utf-8")
        result = self.execute([self.lean, "--trust=0", "Inspect.lean"], cwd=directory, env=env)
        assert result.stdout.count(b"does not depend on any axioms") == 2 and b"sorryAx" not in result.stdout
        (directory / "NoInjection.lean").write_text("import Witness\n#check injected\n", encoding="utf-8")
        self.execute([self.lean, "--trust=0", "NoInjection.lean"], success=False, cwd=directory, env=env)


def run(binary, lean, model, work):
    h = Harness(binary, lean, model, work)
    version = h.execute([lean, "--version"]).stdout
    assert b"version 4.24.0," in version
    source = (FIXTURES / "commented.ves").read_bytes()
    path, first = h.checked(source)
    assert first["comment_tokens"] == 3
    formatted = h.source(source)
    h.execute([binary, "fmt", formatted, "--json"])
    assert first["output_bytes"] == len(formatted.read_bytes())
    _, changed = h.checked(source.replace("final 🦀".encode(), "current 🦀".encode()))
    assert first["source_blake3"] != changed["source_blake3"] and first["output_blake3"] != changed["output_blake3"]
    _, macro = h.checked((FIXTURES / "macro.ves").read_bytes())
    assert macro["comment_tokens"] == 2
    hostile = h.source((FIXTURES / "hostile.ves").read_bytes())
    witness = h.verify(hostile, "--format", "--emit-lean", "--lean", lean, "--timeout", "300").stdout
    assert b"comments_preserved" in witness and "Vesper.StylusGenerated" in witness.decode("utf-8").replace("«", "").replace("»", "") and not witness.lstrip().startswith(b"{")
    h.replay(witness)

    for data in [b"fn main(){let x=;", b"fn main(){/* unclosed", b"fn main(){unknown!{raw}}", b"fn main(){\xff}", b" " * (128 * 1024 + 1)]:
        h.rejected(data)
        p = h.rejected(data, ("--format", "--emit-lean"))
        assert p.read_bytes() == data
    for flags in [("--json",), ("--format", "--json", "--emit-lean"), ("--format", "--json", "--timeout", "0"), ("--format", "--json", "--timeout", "301"), ("--format", "--json", "--unknown")]:
        h.rejected(source, flags)
    missing = work / "nonexistent-real-lean"
    h.rejected(source, ("--format", "--json", "--lean", missing))
    failed = h.verify(path, "--format", "--emit-lean", "--lean", missing, success=False)
    assert failed.stdout == b""
    h.execute([binary, "verify", work, "--format", "--json"], success=False)
    assert not any(p.name.startswith(".vesper-") for p in work.iterdir())
    print(json.dumps({"status": "passed", "commands": h.commands, "kernel_reports": h.kernel_reports, "independent_witness_replay": True, "scope": "formatter_document_model; no semantic or Rust-refinement proof"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--lean", default=os.environ.get("VESPER_LEAN", "lean"))
    parser.add_argument("--model", type=pathlib.Path, default=pathlib.Path(os.environ.get("VESPER_FORMAT_PROOF_MODEL", str(ROOT / "lean" / "StylusAndLintSafety.lean"))))
    parser.add_argument("--work", type=pathlib.Path)
    args = parser.parse_args()
    binary = pathlib.Path(str(args.binary) + ".exe") if pathlib.Path(str(args.binary) + ".exe").is_file() else args.binary
    binary = binary.resolve(strict=True)
    prefix = subprocess.run([args.lean, "--print-prefix"], cwd=ROOT, capture_output=True, timeout=45, check=True).stdout.decode().strip()
    lean = pathlib.Path(prefix) / "bin" / ("lean.exe" if os.name == "nt" else "lean")
    assert lean.is_file()
    if args.work:
        args.work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vesper-format-proof-", dir=args.work) as directory:
        run(binary, lean, args.model.resolve(strict=True), pathlib.Path(directory).resolve())


if __name__ == "__main__":
    main()
