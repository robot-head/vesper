#!/usr/bin/env python3
"""Maintainer-only refresh from an assembled candidate (requires Python blake3)."""
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import sys
from blake3 import blake3

TOOL = Path(__file__).resolve().parent
ROOT = TOOL.parent.parent

def write(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def main():
    candidate = Path(sys.argv[1]).resolve(strict=True)
    spec = importlib.util.spec_from_file_location("logos_setup", TOOL / "setup.py")
    setup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(setup)
    cases = json.loads((TOOL / "controls/cases.json").read_text())
    known = {
        "schema": 1, "role": "review_evidence_not_success_receipt",
        "upstream_base": setup.UPSTREAM, "exporter_commit": setup.EXPORTER,
        "lean_toolchain": setup.RELEASE,
        "controls": [{"id": c["id"], "source": "tools/logos42/controls/cases.json",
                      "expected_accept": c["expected_accept"],
                      "expected_reason": c["expected_reason"], "kind": "raw_export"} for c in cases],
        "fixes": [{"id": "lean435-primitive-reflection",
                   "disposition": "Regenerate the checked primitive reference terms with Lean 4.35's structure-based Decidable, and use its decide projection. Primitive recognizers and declaration checking remain enabled; no trusted-prefix or parse-only path is exposed.",
                   "patch": "tools/logos42/patches/lean435-primitives.patch"}],
        "source_sha256": {"tools/logos42/" + p: hashlib.sha256((TOOL / p).read_bytes()).hexdigest()
                          for p in setup.ADAPTERS},
        "executions": [],
        "unexecuted": ["whole-checker formal verification", "exporter/compiler refinement proof"],
    }
    write(TOOL / "known-fixes.json", known)
    bundle = json.loads((TOOL / "resource-source-manifest.json").read_text())
    for f in bundle["files"]:
        data = (ROOT / f["path"]).read_bytes()
        f.update(bytes=len(data), blake3=list(blake3(data).digest()))
    write(TOOL / "resource-source-manifest.json", bundle)
    files = []
    for p in setup.source_paths(candidate):
        data = (candidate / p).read_bytes()
        files.append(dict(path=p, bytes=len(data), blake3=list(blake3(data).digest())))
    (TOOL / "source-manifest.json").write_text('{\n  "schema": 1,\n  "files": [\n' +
        ',\n'.join('    ' + json.dumps(f, separators=(',', ':')) for f in files) + '\n  ]\n}\n')
    h = blake3(b"vesper.logos42.sources\0" + struct.pack("<HI", 1, len(files)))
    for f in files:
        p = f["path"].encode()
        h.update(struct.pack("<I", len(p)) + p + struct.pack("<Q", f["bytes"]) + bytes(f["blake3"]))
    pins = {
        "schema": 3, "profile": setup.PROFILE, "upstream_commit": setup.UPSTREAM,
        "exporter_commit": setup.EXPORTER, "lean_release": setup.RELEASE,
        "lean_commit": setup.LEAN_COMMIT,
        "patches": [{"path": p, "blake3": list(blake3((ROOT / p).read_bytes()).digest())} for p in setup.PATCHES],
        "source_overlays": [{"source": "tools/logos42/" + p, "destination": p,
                            "blake3": list(blake3((TOOL / p).read_bytes()).digest())} for p in setup.ADAPTERS],
        "known_fixes_blake3": list(blake3((TOOL / "known-fixes.json").read_bytes()).digest()),
        "source_manifest_blake3": list(h.digest()),
        "resource_bundle_blake3": list(blake3((TOOL / "resource-source-manifest.json").read_bytes()).digest()),
        "build_argv": ["{lake}", "build", "logos42"], "cargo_build_argv": setup.CARGO_RECIPE,
        "checker_relative_path": ".lake/build/bin/logos42",
        "kernel_relative_path": ".lake/build/rust/release/logos-kernel",
    }
    write(TOOL / "pins.json", pins)
    print(f"Refreshed {len(files)} source records and {len(cases)} control expectations.")

if __name__ == "__main__":
    main()
