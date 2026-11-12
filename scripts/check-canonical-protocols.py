#!/usr/bin/env python3
"""Exercise actual binary codecs, explicit lint policy and measured/unavailable PMU.

The shell builds the binary. All command inputs here are disposable copies; no
tracked fixture is passed to a mutating command. There is no speed threshold or
requirement for hardware permission. PMU unavailability must be typed, nonzero
and free of fabricated samples/counters.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

PROTO_PROFILE = "vesper.protobuf.explicit-presence/1"
PROTO_SCHEMA = ('{"profile":"vesper.protobuf.explicit-presence/1","root":0,"messages":'
                '[{"fields":[{"number":1,"kind":"u64","repeated":false}]}]}')
# Fixed byte-vector regressions, not merely hash-shaped strings. Schema is the
# exact ASCII byte sequence above (no newline); input is exactly 08 2a.
PROTO_INPUT_BLAKE3 = "d1a1ec12a16f4c9020a0e8e9500e8ed4aafce737dcb56baa3f805e0b96039785"
PROTO_SCHEMA_BLAKE3 = "187b1a3ce443d2d3fef0525c3708fcf54e16e9b63ba43a84ce6b5b7c9b7d3b7b"
PMU_PROVIDER = "linux.perf_event_open.group_read"
CLOCK_PROVIDER = "std::time::Instant"
UNAVAILABLE_REASONS = {
    "UnsupportedPlatform", "PermissionDenied", "OsError", "InvalidRead",
    "InvalidMetadata", "WrongProcessOrThread", "InvalidState",
    "ForeignOrStaleToken", "Exhausted", "CounterRegression",
    "NotFullyScheduled", "NoEvents",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def decimal(value: object, label: str, *, positive: bool = False) -> int:
    require(isinstance(value, str) and re.fullmatch(r"[0-9]+", value) is not None,
            f"{label} must be a decimal string, got {value!r}")
    number = int(value)
    require(number > 0 if positive else number >= 0, f"invalid {label}: {value}")
    return number


class Runner:
    def __init__(self, binary: Path, root: Path, work: Path) -> None:
        self.binary, self.root, self.work = binary, root, work

    def file(self, name: str, data: bytes | str) -> Path:
        path = self.work / name
        path.write_bytes(data.encode("utf-8") if isinstance(data, str) else data)
        return path

    def run(self, *args: object, success: bool | None = True) -> subprocess.CompletedProcess[str]:
        command = [str(self.binary), *(str(arg) for arg in args)]
        result = subprocess.run(command, cwd=self.root, text=True, encoding="utf-8",
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=60, check=False)
        if success is not None:
            require((result.returncode == 0) == success,
                    f"unexpected exit {result.returncode}: {command!r}\n"
                    f"stdout={result.stdout}\nstderr={result.stderr}")
        return result

    def report(self, *args: object, success: bool | None = True) -> tuple[dict, int]:
        result = self.run(*args, "--json", success=success)
        data = json.loads(result.stdout)
        require(isinstance(data, dict), f"expected one JSON object: {result.stdout}")
        return data, result.returncode

    def grammar(self, kind: str, path: Path, *options: object,
                success: bool = True) -> dict:
        before = path.read_bytes()
        data, _ = self.report("grammar", "check", kind, path, *options, success=success)
        require(path.read_bytes() == before, "grammar command mutated its input")
        require(data["schema"] == "vesper.grammar/1"
                and data["status"] == ("valid" if success else "invalid"),
                f"wrong grammar status: {data}")
        if success:
            require(data["format"] == kind and data["input_bytes"] == len(before)
                    and data["consumed_bytes"] == len(before), "grammar did not consume actual bytes")
            require(re.fullmatch(r"[0-9a-f]{64}", data["input_blake3"]) is not None,
                    "grammar input identity is missing")
        else:
            require(isinstance(data.get("error"), str) and bool(data["error"]),
                    "invalid grammar report omitted its error")
        return data


def check_protocols(run: Runner, originals: dict[str, bytes]) -> None:
    schema = run.file("protobuf-schema.json", PROTO_SCHEMA)
    encoded = run.file("message.pb", bytes.fromhex("082a"))
    data = run.grammar("protobuf", encoded, "--schema", schema)
    result = data["result"]
    require(data["input_blake3"] == PROTO_INPUT_BLAKE3
            and result["schema_blake3"] == PROTO_SCHEMA_BLAKE3,
            "protobuf actual input/schema snapshot hash differs")
    require(result["profile"] == PROTO_PROFILE and result["roundtrip_bytes_equal"] is True,
            "protobuf did not execute canonical byte roundtrip")
    require(result["projection"]["fields"] == [
        {"number": 1, "value": {"kind": "u64", "value": "42"}}],
        "protobuf did not decode the actual u64 value42")
    require(result["projection"]["field_occurrences"] == 1
            and not result["projection_truncated"], "unexpected protobuf projection")
    for raw in (b"\x08\xaa\x00", b"\x08\x01\x08\x02", b"\x10\x01", b"\x08\x80"):
        encoded.write_bytes(raw)
        run.grammar("protobuf", encoded, "--schema", schema, success=False)
    encoded.write_bytes(b"\x08\x2a")
    for bad_schema in (
        PROTO_SCHEMA.replace('"root":0', '"root":0,"\\u0072oot":0'),
        PROTO_SCHEMA.replace('"root":0', '"root":0,"unknown":{}'),
        PROTO_SCHEMA.replace('"kind":"u64"', '"kind":"u64","message":null'),
    ):
        schema.write_bytes(bad_schema.encode("ascii"))
        run.grammar("protobuf", encoded, "--schema", schema, success=False)
        require(schema.read_bytes() == bad_schema.encode("ascii"), "schema validator wrote its input")
    schema.write_bytes(PROTO_SCHEMA.encode("ascii"))

    # OpenSSL3.5.7-generated fixture recorded in the leaf DER test/example:
    # Booleantrue, Integer-129, BitsA0, octets00ff, Null, rsaEncryptionOID.
    der_bytes = bytes.fromhex("301c0101ff0202ff7f030200a0040200ff050006092a864886f70d010101")
    der = run.file("openssl-profile.der", der_bytes)
    result = run.grammar("der", der)["result"]
    children = result["projection"]["value"]["children"]
    require(result["profile"] == "vesper.der.universal-subset/1"
            and result["roundtrip_bytes_equal"] is True and result["nodes"] == 7,
            "DER did not validate/reencode the independent fixture")
    require(result["projection"]["kind"] == "Sequence" and len(children) == 6
            and children[0]["value"]["value"] is True
            and children[1]["value"]["value_i64"] == "-129"
            and children[2]["value"]["unused_bits"] == 0
            and children[2]["value"]["payload"]["hex_prefix"] == "a0"
            and children[3]["value"]["hex_prefix"] == "00ff"
            and children[4]["kind"] == "Null"
            and children[5]["value"]["hex_prefix"] == "2a864886f70d010101",
            "DER actual logical values differ")
    der.write_bytes(bytes.fromhex("3106020101020102"))
    result = run.grammar("der", der, "--set-of")["result"]
    require(result["root_set_of"] is True and result["roundtrip_bytes_equal"] is True
            and result["projection"]["kind"] == "SetOf"
            and [c["value"]["value_i64"] for c in result["projection"]["value"]["children"]]
            == ["1", "2"], "typed SET OF canonical order differs")
    run.grammar("der", der, success=False)  # Tag17 alone does not mean SET OF.
    der.write_bytes(bytes.fromhex("3106020102020101"))
    run.grammar("der", der, "--set-of", success=False)
    for raw in (bytes.fromhex(s) for s in ("04810100", "010101", "0202ffff", "30800000", "05000500", "a000")):
        der.write_bytes(raw)
        run.grammar("der", der, success=False)

    expected = json.loads(originals["ipc-expected.json"])["fixtures"]
    for name, reference in expected.items():
        require(name in originals and len(originals[name]) == reference["bytes"]
                and hashlib.sha256(originals[name]).hexdigest() == reference["sha256"],
                f"committed PyArrow reference bytes differ: {name}")
    for name, version in (("modern-v5.arrow", "V5"), ("legacy-v4.arrow", "V4")):
        raw = originals[name]
        require(hashlib.sha256(raw).hexdigest() == expected[name]["sha256"],
                f"independent Arrow fixture identity changed: {name}")
        path = run.file(f"ipc-{name}", raw)
        result = run.grammar("arrow-ipc", path)["result"]
        require(result["profile"] == "arrow.ipc.signed-i64/1"
                and result["canonical_serialization_claimed"] is False
                and result["metadata_version"] == version
                and result["metadata_version_discriminant"] == (4 if version == "V5" else 3)
                and result["batch_count"] == 3 and result["total_rows"] == 13,
                "Arrow version/framing/structural scope differs")
        require([b["rows"] for b in result["batches"]] == [9, 0, 4]
                and [f["name"]["preview"] for f in result["fields"]] == ["nullable", "required"]
                and [f["nullable"] for f in result["fields"]] == [True, False],
                "Arrow schema or real batch lengths differ")
        columns = result["batches"][0]["columns"]
        require(columns[0]["nulls"] == 2 and columns[0]["sum"] == "11"
                and columns[1]["nulls"] == 0 and columns[1]["sum"] == "450"
                and columns[0]["values"] == ["1", None, "-2", "0", "1099511627776",
                                              "-1099511627776", None, "7"]
                and columns[0]["values_truncated"] is True,
                "Arrow actual bitmaps/values/sums differ")
        require(result["batches"][2]["columns"][0]["nulls"] == 4
                and result["batches"][2]["columns"][0]["sum"] is None
                and result["batches"][2]["columns"][1]["sum"] == "12",
                "all-null Arrow evidence was fabricated as a number")
    overflow = run.file("ipc-sum-overflow.arrow", originals["sum-overflow.arrow"])
    column = run.grammar("arrow-ipc", overflow)["result"]["batches"][0]["columns"][0]
    require(column["values"] == ["9223372036854775807", "1"]
            and column["sum"] is None and "Overflow" in column["sum_unavailable_reason"],
            "valid Arrow input's overflowing sum was promoted to numeric evidence")
    for name, reason in {
        "compressed.arrow": "UnsupportedCompression", "dictionary.arrow": "UnsupportedDictionary",
        "nested.arrow": "UnsupportedNested", "int32.arrow": "UnsupportedType",
        "unsigned.arrow": "UnsupportedType", "custom-metadata.arrow": "UnsupportedMetadata",
    }.items():
        path = run.file(f"unsupported-{name}", originals[name])
        require(reason in run.grammar("arrow-ipc", path, success=False)["error"],
                f"unsupported actual Arrow fixture did not fail explicitly: {name}")


def check_policy(run: Runner, originals: dict[str, bytes]) -> None:
    source = run.file("lint-demo.ves", originals["lint-demo.ves"])
    config = run.file("Loom.lint.toml", originals["Loom.lint.toml"])
    before = source.read_bytes()
    initial, _ = run.report("lint", source, "--config", config, success=False)
    require(initial["status"] == "denied" and initial["outcome"]["remaining_denied"] == 2
            and source.read_bytes() == before, "explicit deny did not fail read-only")
    unused = [d for d in initial["diagnostics"] if d["code"] == "L0102"]
    require(len(unused) == 2 and all(d["severity"] == "deny" for d in unused),
            "two actual unused effects were not denied")
    policy = initial["policy"]
    require(policy["provenance"]["kind"] == "explicit"
            and Path(policy["provenance"]["path"]).resolve() == config.resolve()
            and policy["provenance"]["bytes"] == len(originals["Loom.lint.toml"])
            and re.fullmatch(r"[0-9a-f]{64}", policy["provenance"]["blake3"]) is not None,
            "policy snapshot provenance is missing")
    require({r["lint"] for r in policy["unavailable"]} == {
        "demote_alloc_to_region", "overly_restrictive_mode", "missed_fip_opportunity"}
        and all(r["available"] is False for r in policy["unavailable"])
        and policy["plugin_packs_supported"] is False,
        "future analyses were fabricated as available")
    fixed, _ = run.report("lint", source, "--config", config, "--fix", "--semver-safe")
    require(fixed["status"] == "checked" and fixed["changed"] is True
            and fixed["applied_edits"] >= 2 and fixed["outcome"]["remaining_denied"] == 0
            and source.read_bytes() != before, "validated fix did not clear denied unused effects")
    require(fixed["semver"]["classification"] in {"patch", "minor"}
            and fixed["semver"]["required_adapters"] == [], "actual SemVer gate was not compatible")
    require(json.loads(run.run("check", source, "--json").stdout) == []
            and run.run("run", source).stdout.strip() == "42", "fixed source did not check/run as42")
    final, _ = run.report("lint", source, "--config", config)
    require(not any(d["code"] == "L0102" for d in final["diagnostics"])
            and final["outcome"]["remaining_denied"] == 0, "denied findings survived fix")
    require(config.read_bytes() == originals["Loom.lint.toml"], "lint mutated policy config")
    source.write_bytes(before)
    allow = run.file("allow.toml", '[lints.vesper]\nunused_effect_capability="allow"\n'
                     'overbroad_inout_projection="allow"\n')
    allowed, _ = run.report("lint", source, "--config", allow, "--fix", "--semver-safe")
    require(allowed["diagnostics"] == [] and not allowed["changed"]
            and allowed["outcome"]["suppressed"] == 3 and source.read_bytes() == before,
            "allow policy still applied a suppressed fix")
    for bad in ('[lints.vesper]\nunused_effect_capability="future"\n',
                '[lints.vesper]\ndemote_alloc_to_region="deny"\n',
                '[lints.plugins]\nexample={path="plugin.wasm"}\n'):
        invalid = run.file("invalid.toml", bad)
        data, _ = run.report("lint", source, "--config", invalid, "--fix", success=False)
        require(data["status"] == "invalid" and source.read_bytes() == before
                and invalid.read_bytes() == bad.encode(), "invalid policy changed owned source/config")


def check_pairs(pairs: object, unit: str, count: int = 6) -> None:
    require(isinstance(pairs, list) and len(pairs) == count, f"wrong {unit} pair count")
    for pair in pairs:
        require(set(pair) == {f"baseline_total_{unit}", f"candidate_total_{unit}",
                              "operations_per_arm", "regression_basis_points"},
                f"{unit} pair contains mislabeled or extra measurement fields")
        decimal(pair[f"baseline_total_{unit}"], f"baseline {unit}", positive=True)
        decimal(pair[f"candidate_total_{unit}"], f"candidate {unit}", positive=True)
        require(pair["operations_per_arm"] == 16, "pair did not represent both ABBA windows")
        require(re.fullmatch(r"-?[0-9]+", pair["regression_basis_points"]["floor"]) is not None
                and re.fullmatch(r"-?[0-9]+", pair["regression_basis_points"]["ceil"]) is not None,
                "ratio bounds lost their exact string representation")


def check_pmu(run: Runner) -> None:
    source = run.file("finite-pmu.ves", "fn helper()->u64{42}\nfn main()->u64{helper()}\n")
    before = source.read_bytes()
    for flags, metric in ((["--pmu"], "time"), (["--metric", "cycles"], "cycles"),
                          (["--pmu", "--metric", "instructions"], "instructions")):
        data, code = run.report("bench", source, "--samples", 6, "--warmup", 0,
                                "--iterations", 8, *flags, success=None)
        require(source.read_bytes() == before, "PMU benchmark rewrote source")
        require(data["schema"] == "vesper.bench/1" and data["metric"] == metric,
                "requested metric was not preserved")
        pmu = data["pmu"]
        require(pmu["requested"] is True and pmu["provider"] == PMU_PROVIDER,
                "PMU provider/request identity missing")
        if data["status"] == "inconclusive":
            require(code != 0 and pmu["status"] == "unavailable"
                    and data["provider_requested"] == PMU_PROVIDER
                    and data["evidence"]["hardware_pmu"] == "unavailable",
                    "unavailable PMU falsely succeeded or claimed hardware evidence")
            failure = pmu.get("unavailable") or data.get("observer_error", {}).get("unavailable")
            require(isinstance(failure, dict) and failure["reason"] in UNAVAILABLE_REASONS
                    and isinstance(failure["operation"], str) and bool(failure["operation"])
                    and (failure["errno"] is None or type(failure["errno"]) is int),
                    "unavailability lost reason/operation/errno")
            if "observer_error" in data:
                require(data["observer_cleanup_errors"] == 0,
                        "observer cleanup loss was mislabeled as mere unavailability")
            if not sys.platform.startswith("linux"):
                require(failure["reason"] == "UnsupportedPlatform", "non-Linux PMU did not stay unavailable")
            require(all(k not in data for k in ("pairs", "analysis", "clock", "body_calls"))
                    and all(k not in pmu for k in ("windows", "cycles_pairs", "instructions_pairs")),
                    "unavailable PMU exposed samples/counters or a wall-clock fallback")
            require(data["unit"] == {"time": "nanoseconds", "cycles": "cycles",
                                     "instructions": "instructions"}[metric],
                    "unavailable report silently switched units")
            continue
        require(code == 0 and data["status"] == "measured" and pmu["status"] == "measured",
                f"unexpected PMU/run failure: {data}")
        unit = {"time": "ns", "cycles": "cycles", "instructions": "instructions"}[metric]
        require(data["provider"] == (CLOCK_PROVIDER if metric == "time" else PMU_PROVIDER)
                and data["unit"] == ("nanoseconds" if metric == "time" else metric),
                "selected metric provider/unit do not match measured evidence")
        require(data["body_calls"] == 192 and data["config"] == {
            "epochs": 6, "warmup_epochs": 0, "operations_per_window": 8}, "wrong actual callback count")
        for arm in ("candidate", "baseline"):
            jit = data[arm]["jit"]
            require(jit["modules"] == jit["compilations"] == 1
                    and jit["entries"] == 1 and jit["defined_functions"] == 2 and jit["runs"] == 96,
                    "PMU kernel was not prepared once and actually invoked96times perarm")
        check_pairs(data["pairs"], unit)
        analysis = data["analysis"]
        require(analysis["gate_decision"] is None and analysis["threshold_bp"] is None
                and analysis["interval"] is not None, "measurement-only run incorrectly applied a speed gate")
        nested_pairs = [analysis["median_lower"], analysis["median_upper"],
                        analysis["interval"]["lower"], analysis["interval"]["upper"]]
        check_pairs(nested_pairs, unit, 4)
        require(all(pair in data["pairs"] for pair in nested_pairs),
                "analysis endpoints are not actual selected-metric samples")
        require(data["candidate"]["source_blake3"] == data["baseline"]["source_blake3"]
                and re.fullmatch(r"[0-9a-f]{64}", data["candidate"]["source_blake3"]) is not None,
                "same-source baseline/candidate identities differ")
        clock = data["clock"]
        require(clock["provider"] == CLOCK_PROVIDER and clock["unit"] == "nanoseconds",
                "nested clock was mislabeled as a hardware counter")
        decimal(clock["elapsed_run_ns"], "clock elapsed", positive=True)
        check_pairs(clock["pairs"], "ns")
        require(pmu["cycles_unit"] == "cycles" and pmu["instructions_unit"] == "instructions",
                "raw hardware units are missing")
        for counter in ("cycles", "instructions"):
            check_pairs(pmu[f"{counter}_pairs"], counter)
        require(data["pairs"] == (clock["pairs"] if metric == "time" else pmu[f"{metric}_pairs"]),
                "top-level pairs did not select the requested provider evidence")
        windows = pmu["windows"]
        require(len(windows) == 24 and isinstance(pmu["scope"], str) and bool(pmu["scope"]),
                "observed windows/measurement boundary scope missing")
        for index, window in enumerate(windows):
            invocation, completion = window["invocation"], window["completion"]
            require(invocation["arm"] == ("baseline" if index % 4 in (0, 3) else "candidate")
                    and invocation["epoch"] == index // 4 and invocation["window"] == index % 4
                    and invocation["warmup"] is False and invocation["operation"] == 0,
                    "PMU windows were not actual A/B/B/A order")
            require(completion["invocation"] == invocation
                    and completion["planned_operations"] == completion["attempted_operations"]
                    == completion["successful_operations"] == 8 and completion["body"] == "Completed"
                    and completion["clock"]["status"] == "measured", "observer callback invariants differ")
            decimal(completion["clock"]["elapsed_ns"], "observed elapsed", positive=True)
            decimal(window["cycles"], "cycles", positive=True)
            decimal(window["instructions"], "instructions", positive=True)
            require(decimal(window["enabled_ns"], "enabled", positive=True)
                    == decimal(window["running_ns"], "running", positive=True),
                    "multiplexed/unscheduled counters were accepted")
        # Independently check each paired total against its actual two observed
        # windows; a relabeled or synthetic selected pair must not pass.
        for epoch in range(6):
            group = windows[epoch * 4:epoch * 4 + 4]
            for counter in ("ns", "cycles", "instructions"):
                totals = [int(w["completion"]["clock"]["elapsed_ns"] if counter == "ns"
                              else w[counter]) for w in group]
                pair = (clock["pairs"] if counter == "ns" else pmu[f"{counter}_pairs"])[epoch]
                require(int(pair[f"baseline_total_{counter}"]) == totals[0] + totals[3]
                        and int(pair[f"candidate_total_{counter}"]) == totals[1] + totals[2],
                        "paired evidence does not aggregate actual ABBA windows")
    print("PMU CLI acceptance: selected metric/provider checked; unavailable stays nonzero "
          "without samples, or actual positive counters retain separate clock evidence")


def check(binary: Path, root: Path, work: Path) -> None:
    require(binary.is_file(), f"missing executable: {binary}")
    require(work.is_dir() and not any(work.iterdir()) and not work.is_relative_to(root),
            "work must be an empty disposable directory outside the repository")
    fixture_dir = root / "crates/vesper-grammar/tests/fixtures/ipc"
    tracked = {path.name: path for path in fixture_dir.glob("*.arrow")}
    tracked.update({"ipc-expected.json": fixture_dir / "expected.json",
                    "lint-demo.ves": root / "crates/vesper-lint/examples/lint_demo.ves",
                    "Loom.lint.toml": root / "crates/vesper-lint/examples/Loom.lint.toml"})
    originals = {name: path.read_bytes() for name, path in tracked.items()}
    run = Runner(binary, root, work)
    try:
        check_protocols(run, originals)
        check_policy(run, originals)
        check_pmu(run)
        print("Canonical protocol acceptance: protobuf42/hash/roundtrip, strict DER/typedSET OF, "
              "real Arrow V4/V5, explicit lint deny/fix/run42 and PMU evidence scope passed")
    finally:
        for name, path in tracked.items():
            require(path.read_bytes() == originals[name], f"tracked fixture changed: {path}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    args = parser.parse_args()
    check(args.binary.resolve(), args.root.resolve(), args.work.resolve())


if __name__ == "__main__":
    main()
