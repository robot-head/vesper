#!/usr/bin/env python3
"""Independent Logos replay of every Lean module under lean/.

Compiles each module (and the archived Lean4Lean helper sources it imports)
with the pinned Lean, partitions the modules into batches whose joint import
closures fit one environment, exports every eligible declaration of each
closure with the lean4export library pinned in the sealed Logos root, and
checks the export with that root's `logos-kernel` bridge, which also requires
the exact declaration name/kind inventory in Logos' checked environment.

Unsafe and partial declarations are reported as explicit skips. This is an
independent kernel replay, not an axiom audit and not a proof of Logos, the
exporter, the Lean compiler or the host.
"""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

TOOL = Path(__file__).resolve().parent
REPOSITORY = TOOL.parent.parent
MODELS = REPOSITORY / "lean"
ARCHIVED = REPOSITORY / "tools" / "lean4lean-foundations42" / "upstream"
EXTRACTION_FIXTURES = REPOSITORY / "tests" / "fixtures" / "lean_extract"
LAKEFILE = """name = "vesperLogosSweep"
defaultTargets = ["logos-sweep"]

[[lean_lib]]
name = "Export"

[[lean_exe]]
name = "logos-sweep"
root = "Sweep"
supportInterpreter = true
"""
IMPORT = re.compile(r"^\s*(?:public\s+)?import\s+(.+)$")


def log(message):
    print(f"logos-sweep: {message}", file=sys.stderr, flush=True)


def run(argv, **kwargs):
    started = time.monotonic()
    result = subprocess.run(argv, capture_output=True, text=True, **kwargs)
    elapsed = time.monotonic() - started
    if result.returncode != 0:
        detail = (result.stderr or result.stdout)[-4000:]
        raise SystemExit(f"logos-sweep: {Path(argv[0]).name} failed ({result.returncode}):\n{detail}")
    return result, elapsed


def modules_under(root):
    found = {}
    for path in sorted(root.rglob("*.lean")):
        name = ".".join(path.relative_to(root).with_suffix("").parts)
        imports = []
        for line in path.read_text(encoding="utf-8").splitlines():
            match = IMPORT.match(line)
            if match:
                imports += match.group(1).split()
        found[name] = (path, imports)
    return found


def topological(modules):
    order, done, active = [], set(), set()

    def visit(name):
        if name in done or name not in modules:
            return
        if name in active:
            raise SystemExit(f"logos-sweep: import cycle through {name}")
        active.add(name)
        for dependency in modules[name][1]:
            visit(dependency)
        active.discard(name)
        done.add(name)
        order.append(name)

    for name in modules:
        visit(name)
    return order


def extract_fixtures(vesper, destination):
    """Write each §11 extraction fixture's extracted module plus its proofs.

    This is what tests/lean_extract.rs compiles; here every resulting
    declaration (extracted definitions, fuel companions, contracts and the
    fixture's theorems) is replayed by Logos as well.
    """
    package = destination / "VesperExtracted"
    package.mkdir(parents=True)
    for source in sorted(EXTRACTION_FIXTURES.glob("*.ves")):
        proofs = source.with_name(source.stem + ".proofs.lean")
        if not proofs.is_file():
            continue
        emitted, _ = run([vesper, "emit", str(source), "--emit", "lean"])
        name = "".join(part.capitalize() for part in source.stem.split("_"))
        text = emitted.stdout + "\n" + proofs.read_text(encoding="utf-8")
        (package / f"{name}.lean").write_text(text, encoding="utf-8")
    return modules_under(destination)


def compile_modules(lean, olean, env, jobs, extracted_tree=None, extracted=None):
    """Compile every model from source; never reuse caller-supplied objects."""
    archived = modules_under(ARCHIVED)
    models = modules_under(MODELS)
    trees = [(ARCHIVED, archived), (MODELS, models)]
    if extracted:
        trees.append((extracted_tree, extracted))
    names = [n for _, modules in trees for n in modules]
    if len(names) != len(set(names)):
        raise SystemExit("logos-sweep: a module name occurs in two source trees")
    sources = {name: (tree, entry[0], entry[1])
               for tree, modules in trees
               for name, entry in modules.items()}
    for name in topological({n: (p, i) for n, (_, p, i) in sources.items()}):
        pass  # rejects cycles before anything runs

    def build(name):
        tree, path, _ = sources[name]
        target = olean / path.relative_to(tree).with_suffix(".olean")
        target.parent.mkdir(parents=True, exist_ok=True)
        # `lean -o` requires the source under the working directory.
        run([lean, "-o", str(target), str(path.relative_to(tree))], cwd=tree, env=env)

    waiting = {name: {d for d in imports if d in sources}
               for name, (_, _, imports) in sources.items()}
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        running = {}
        while waiting or running:
            for name in [n for n, deps in waiting.items() if not deps]:
                del waiting[name]
                running[pool.submit(build, name)] = name
            done, _ = concurrent.futures.wait(running, return_when=concurrent.futures.FIRST_COMPLETED)
            for future in done:
                name = running.pop(future)
                future.result()
                for deps in waiting.values():
                    deps.discard(name)
    return topological(models) + topological(extracted or {})


def build_sweeper(root, work, lake, env):
    package = work / "sweeper"
    package.mkdir()
    shutil.copyfile(root / "Export.lean", package / "Export.lean")
    shutil.copyfile(root / "lean-toolchain", package / "lean-toolchain")
    shutil.copyfile(TOOL / "Sweep.lean", package / "Sweep.lean")
    (package / "lakefile.toml").write_text(LAKEFILE, encoding="utf-8")
    run([lake, "build"], cwd=package, env=env)
    return package / ".lake" / "build" / "bin" / "logos-sweep"


MAX_DECLINES_PER_BATCH = 8
DECLINED = re.compile(r"^Logos declined: .*?declaration '([^']+)'")


def load_expected_declines():
    """Declarations Logos is known to decline (resource limits), with reasons.

    A decline is not acceptance: such a declaration and every declaration that
    uses it are checked by Lean's kernel only, and the report says so.
    """
    entries = json.loads((TOOL / "expected-declines.json").read_text(encoding="utf-8"))
    expected = {}
    for entry in entries:
        if set(entry) != {"declaration", "reason"} or not entry["reason"].strip():
            raise SystemExit(f"logos-sweep: malformed expected decline {entry}")
        if entry["declaration"] in expected:
            raise SystemExit(f"logos-sweep: duplicate expected decline {entry['declaration']}")
        expected[entry["declaration"]] = entry["reason"]
    return expected


def replay_batch(work, sweeper, kernel, env, index, batch, exclusions, control=False):
    exported = work / f"batch{index}.ndjson"
    manifest = work / f"batch{index}.json"
    summary_path = work / f"batch{index}.summary.json"
    exclude = work / f"batch{index}.exclude.json"
    exclude.write_text(json.dumps(exclusions), encoding="utf-8")
    log(f"batch {index + 1}: {len(batch)} module(s), {len(exclusions)} withheld root(s)")
    _, export_seconds = run(
        [str(sweeper), str(exported), str(manifest), str(summary_path), str(exclude), *batch],
        env=env)
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    export_bytes = exported.stat().st_size
    started = time.monotonic()
    checked = subprocess.run([str(kernel), "--check-manifest", str(exported), str(manifest)],
                             capture_output=True, text=True)
    check_seconds = time.monotonic() - started
    if control and checked.returncode == 0:
        # Coverage control on real data: drop one declaration from the expected
        # inventory; the bridge must refuse the now-unassociated declaration.
        entries = json.loads(manifest.read_text(encoding="utf-8"))
        manifest.write_text(json.dumps(entries[1:]), encoding="utf-8")
        refused = subprocess.run([str(kernel), "--check-manifest", str(exported), str(manifest)],
                                 capture_output=True, text=True)
        if refused.returncode == 0 or "unassociated declaration" not in refused.stderr:
            raise SystemExit("logos-sweep: coverage control was not refused: "
                             f"{refused.returncode} {refused.stderr[:400]}")
        log("coverage control refused as expected")
    exported.unlink()
    manifest.unlink()
    outcome = {"summary": summary, "export_bytes": export_bytes,
               "export_seconds": round(export_seconds, 1),
               "check_seconds": round(check_seconds, 1)}
    if checked.returncode == 0:
        return {**outcome, "verdict": "accepted", "logos": json.loads(checked.stdout)}
    detail = checked.stderr.strip()[:4000]
    match = DECLINED.match(detail)
    verdict = "declined" if detail.startswith("Logos declined:") else "rejected"
    return {**outcome, "verdict": verdict, "detail": detail,
            "declaration": match.group(1) if match else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", required=True, type=Path,
                        help="sealed Logos root published by tools/logos42/setup.py")
    parser.add_argument("--driver", required=True, type=Path,
                        help="the logos42 driver, used to verify the root's sources")
    parser.add_argument("--lean", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--vesper", type=Path,
                        help="vesper binary; also replay the §11 extraction fixtures")
    parser.add_argument("--work", type=Path, help="scratch directory (default: temporary)")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1,
                        help="parallel Lean compilations")
    parser.add_argument("--batch-jobs", type=int, default=max(1, (os.cpu_count() or 1) // 2),
                        help="batches exported and checked at once (each is one process pair)")
    args = parser.parse_args()

    root = args.root.resolve()
    lean = args.lean.resolve()
    lake = lean.parent / ("lake.exe" if os.name == "nt" else "lake")
    kernel = root / ".lake" / "build" / "rust" / "release" / (
        "logos-kernel.exe" if os.name == "nt" else "logos-kernel")
    for required in (root / "setup-receipt.json", root / "Export.lean", kernel, lean, lake):
        if not required.is_file():
            raise SystemExit(f"logos-sweep: missing {required}")
    # Bind the exporter source we compile to the pinned, sealed root.
    verified, _ = run([str(args.driver.resolve()), "--verify-setup-source", str(root)])
    source_manifest = json.loads(verified.stdout)["source_manifest_blake3"]

    env = dict(os.environ)
    env["PATH"] = str(lean.parent) + os.pathsep + env.get("PATH", "")
    env.pop("LEAN_PATH", None)
    version, _ = run([str(lean), "--version"], env=env)
    if "version 4.35.0" not in version.stdout:
        raise SystemExit(f"logos-sweep: expected Lean 4.35.0, got {version.stdout.strip()}")

    temporary = None
    if args.work is None:
        temporary = tempfile.TemporaryDirectory(prefix="logos-sweep-")
        work = Path(temporary.name)
    else:
        work = args.work.resolve()
        work.mkdir(parents=True, exist_ok=False)
    try:
        olean = work / "olean"
        started = time.monotonic()
        log("compiling lean/ models")
        extracted_tree = work / "extracted"
        extracted = (extract_fixtures(str(args.vesper.resolve()), extracted_tree)
                     if args.vesper else None)
        order = compile_modules(str(lean), olean, {**env, "LEAN_PATH": str(olean)}, args.jobs,
                                extracted_tree, extracted)
        compile_seconds = time.monotonic() - started
        log("building the sweeper")
        sweeper = build_sweeper(root, work, str(lake), env)
        sweep_env = {**env, "LEAN_PATH": str(olean)}
        plan_path = work / "plan.json"
        run([str(sweeper), "--plan", str(plan_path), *order], env=sweep_env)
        batches = json.loads(plan_path.read_text(encoding="utf-8"))
        planned = [m for batch in batches for m in batch]
        if sorted(planned) != sorted(order) or len(set(planned)) != len(planned):
            raise SystemExit("logos-sweep: plan does not cover every module exactly once")

        rows, results, declined = {}, [], {}
        control_batch = min(range(len(batches)), key=lambda i: (len(batches[i]), i))
        def replay_with_declines(index):
            batch, exclusions, attempt = batches[index], [], 0
            for attempt in range(MAX_DECLINES_PER_BATCH + 1):
                outcome = replay_batch(work, sweeper, kernel, sweep_env, index, batch, exclusions,
                                       control=index == control_batch)
                if outcome["verdict"] == "accepted":
                    return outcome, exclusions, attempt
                if outcome["verdict"] == "rejected" or outcome["declaration"] is None:
                    raise SystemExit(f"logos-sweep: batch {index} ({batch}): {outcome['detail']}")
                name = outcome["declaration"]
                if name in exclusions:
                    raise SystemExit(f"logos-sweep: {name} declined again after exclusion")
                log(f"Logos declined {name}: {outcome['detail']}")
                declined[name] = outcome["detail"]
                exclusions.append(name)
            raise SystemExit(f"logos-sweep: batch {index} exceeded {MAX_DECLINES_PER_BATCH} declines")

        # Largest closures first, so the long batch overlaps the small ones.
        by_size = sorted(range(len(batches)), key=lambda i: -len(batches[i]))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.batch_jobs) as pool:
            finished = dict(zip(by_size, pool.map(replay_with_declines, by_size)))
        for index, batch in enumerate(batches):
            outcome, exclusions, attempt = finished[index]
            summary, verdict = outcome["summary"], outcome["logos"]
            if verdict.get("checked") != summary["exported"]:
                raise SystemExit(f"logos-sweep: batch {index} coverage {verdict.get('checked')} "
                                 f"!= exported {summary['exported']}")
            for row in summary["roots"]:
                rows[row["module"]] = {**row, "batch": index}
            results.append({
                "batch": index,
                "roots": batch,
                "closure_modules": summary["modules_in_closure"],
                "constants": summary["constants"],
                "logos_checked": verdict["checked"],
                "skipped_unsafe_or_partial": summary["skipped_unsafe_or_partial"],
                "withheld_after_decline": summary["withheld"],
                "declined": exclusions,
                "attempts": attempt + 1,
                "export_bytes": outcome["export_bytes"],
                "export_seconds": outcome["export_seconds"],
                "check_seconds": outcome["check_seconds"],
                "validated_primitives": verdict["validated_primitives"],
            })
        problems = []
        expected = load_expected_declines()
        for name, detail in sorted(declined.items()):
            if name not in expected:
                problems.append(f"unexpected Logos decline of {name}: {detail}")
        for name in sorted(set(expected) - set(declined)):
            problems.append(f"{name} is listed in expected-declines.json but Logos now accepts it; "
                            "remove the entry")
        for module in order:
            row = rows.get(module)
            if row is None:
                problems.append(f"{module}: missing from every batch summary")
            elif row["checked"] == 0 and (row["withheld"] or row["skipped_unsafe_or_partial"]):
                problems.append(f"{module}: none of its own declarations was checked by Logos")
        report = {
            "schema": 1,
            "profile": "vesper.logos-sweep/1",
            "lean": version.stdout.strip(),
            "logos_root_source_manifest_blake3": source_manifest,
            "status": "complete" if not problems else "failed",
            "problems": problems,
            "declined": [{"declaration": n, "detail": d, "reason": expected.get(n)}
                         for n, d in sorted(declined.items())],
            "modules": len(order),
            "extracted_fixture_modules": sorted(extracted or {}),
            "compile_seconds": round(compile_seconds, 1),
            "total_seconds": round(time.monotonic() - started, 1),
            "batches": results,
            "per_module": [rows[m] for m in sorted(order)],
            "coverage_control": f"batch {control_batch}: refused",
            "axiom_policy_audit": "not_run",
            "implementation_proof_audit": "not_run",
        }
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8")
        log(f"{len(order)} modules in {len(batches)} batches, "
            f"{sum(r['logos_checked'] for r in results)} declarations checked by Logos, "
            f"{len(declined)} declined")
        if problems:
            raise SystemExit("logos-sweep: FAILED\n" + "\n".join(problems))
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    main()
