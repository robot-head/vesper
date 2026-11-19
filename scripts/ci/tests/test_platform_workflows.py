"""Regression tests for required-check naming, dedup ownership and release gates."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[3]


def workflow(name):
    # BaseLoader keeps GitHub's "on" key intact (YAML 1.1 treats it as True).
    return yaml.load((ROOT / ".github/workflows" / name).read_text(), Loader=yaml.BaseLoader)


class PlatformWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.platforms = workflow("platforms.yml")
        self.release = workflow("platform-release.yml")

    def test_required_check_names_and_events_are_preserved(self):
        self.assertTrue({"push", "pull_request", "merge_group", "schedule",
                         "workflow_dispatch", "workflow_call"} <= self.platforms["on"].keys())
        mac = self.platforms["jobs"]["platform"]
        windows = self.platforms["jobs"]["windows"]
        self.assertEqual(mac["strategy"]["matrix"]["include"][0]["name"], "macOS (AArch64)")
        self.assertTrue(windows["name"].startswith("Windows (x86_64 MSVC)"))
        for job in (mac, windows):
            self.assertEqual(job["needs"], "plan")
            self.assertIn("always()", job["if"])
            self.assertIn("!cancelled()", job["if"])
            first = job["steps"][0]
            self.assertEqual(first["run"], 'test "$PLAN_RESULT" = success')
            checkout = next(s for s in job["steps"] if s.get("uses") == "actions/checkout@v4")
            self.assertEqual(checkout["with"]["ref"], "${{ needs.plan.outputs.sha }}")
            self.assertTrue(any(s.get("name") == "Verify exact checkout" for s in job["steps"]))
        self.assertEqual(mac["timeout-minutes"], "60")
        self.assertEqual(windows["timeout-minutes"], "60")
        for job in (mac, windows):
            commands = "\n".join(s.get("run", "") for s in job["steps"])
            self.assertNotIn("--workspace", commands)
            self.assertNotIn("clippy", commands)
            self.assertNotIn("check-lean-models", commands)
            self.assertIn("scripts/ci/test-native-executables.sh", commands)
        mac_commands = "\n".join(s.get("run", "") for s in mac["steps"])
        windows_commands = "\n".join(s.get("run", "") for s in windows["steps"])
        self.assertIn("--test platform", mac_commands)
        self.assertIn("--test windows_platform", windows_commands)
        self.assertIn("--test windows", windows_commands)
        for commands in (mac_commands, windows_commands):
            self.assertIn("--test drivers", commands)
            self.assertIn("--test pool", commands)

    def test_port_harness_runs_once_on_linux(self):
        # macOS and Windows run only the export ABI tier; the ported crates
        # are platform-neutral and run once in the Linux job (#1010).
        jobs = self.platforms["jobs"]
        for job in (jobs["platform"], jobs["windows"]):
            step = next(s for s in job["steps"] if "test-port-harness.sh" in s.get("run", ""))
            self.assertEqual(step["env"]["VESPER_PORT_HARNESS_SCOPE"], "abi")
        ports = jobs["ports"]
        self.assertEqual(ports["runs-on"], "ubuntu-latest")
        self.assertEqual(ports["needs"], "plan")
        self.assertEqual(ports["steps"][0]["run"], 'test "$PLAN_RESULT" = success')
        step = next(s for s in ports["steps"] if "test-port-harness.sh" in s.get("run", ""))
        self.assertNotIn("VESPER_PORT_HARNESS_SCOPE", step.get("env", {}))
        # Seven shards (#1019, #1100), each running its own part of the harness.
        shards = [str(n) for n in range(1, 8)]
        self.assertEqual(ports["strategy"]["matrix"]["shard"], shards)
        self.assertEqual(ports["strategy"]["fail-fast"], "false")
        self.assertEqual(step["env"]["VESPER_PORT_HARNESS_SHARD"], "${{ matrix.shard }}")
        self.assertIn("ports", jobs["complete"]["needs"])
        script = (ROOT / "scripts/test-port-harness.sh").read_text()
        tier_end = script.index('echo "export ABI tier done')
        abi_tier = script[:tier_end]
        for check in ("-p vesper-port-abi", "-p vesper-port-harness", "--test kit",
                      "--test kit_memory", "-p vesper-port-pilot --features vesper-port",
                      "-p vesper-port-build --features divergent"):
            self.assertIn(check, abi_tier)
        self.assertNotIn("vesper-beacon", abi_tier.split("set -eu", 1)[1])
        # Every shard is reached, and later ports land in the last one.
        self.assertIn(f"shards={shards[-1]}\n", script)
        for n in shards[:-1]:
            self.assertIn(f"if part {n}; then", script)
        self.assertIn(f"part {shards[-1]} || exit 0", script)
        tail = script.split(f"part {shards[-1]} || exit 0", 1)[1]
        self.assertNotIn("\nfi\n", tail.replace("\n    fi", ""))

    def test_plan_failure_fails_required_jobs(self):
        for result in ("failure", "cancelled", "skipped", ""):
            command = self.platforms["jobs"]["windows"]["steps"][0]["run"]
            run = subprocess.run(["bash", "-c", command], env={**os.environ, "PLAN_RESULT": result})
            self.assertNotEqual(run.returncode, 0)
        run = subprocess.run(["bash", "-c", command], env={**os.environ, "PLAN_RESULT": "success"})
        self.assertEqual(run.returncode, 0)

    def test_completion_emits_no_success_for_partial_or_skipped_platforms(self):
        run = self.platforms["jobs"]["complete"]["steps"][0]["run"]
        for result in ("failure", "skipped", "cancelled"):
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "outputs"
                for failed in ("WINDOWS_RESULT", "PORTS_RESULT"):
                    env = {**os.environ, "PLAN_RESULT": "success", "MAC_RESULT": "success",
                           "WINDOWS_RESULT": "success", "PORTS_RESULT": "success",
                           "CHECKED_SHA": "a" * 40, "PROFILE": "full",
                           "GITHUB_OUTPUT": str(output), failed: result}
                    completed = subprocess.run(["bash", "-e", "-c", run], env=env)
                    self.assertNotEqual(completed.returncode, 0)
                    self.assertFalse(output.exists())

    def test_dedup_retains_distinct_features_and_moves_stdio_checks(self):
        source = workflow("source-macros.yml")
        matrix = source["jobs"]["native-tier1-source"]["strategy"]["matrix"]["os"]
        self.assertEqual(set(matrix), {"ubuntu-latest"})
        source_steps = source["jobs"]["native-tier1-source"]["steps"]
        commands = "\n".join(s.get("run", "") for s in source_steps)
        self.assertIn("--no-default-features", commands)
        self.assertIn("cargo tree --locked", commands)
        self.assertIn("--test forge_authority", commands)
        self.assertIn("structured-component-foundation", source["jobs"])
        self.assertEqual(workflow("stylus.yml")["jobs"]["native-stylus"]["strategy"]["matrix"]["os"],
                         ["ubuntu-latest"])

    def test_release_is_read_only_full_validation_before_ready(self):
        self.assertEqual(set(self.release["on"]), {"workflow_dispatch"})
        self.assertEqual(self.release["permissions"]["contents"], "read")
        full = self.release["jobs"]["platforms"]
        self.assertEqual(full["needs"], "linux")
        self.assertEqual(full["with"]["profile"], "full")
        self.assertEqual(full["with"]["checkout_ref"], "${{ needs.linux.outputs.sha }}")
        self.assertEqual(set(self.release["jobs"]["ready"]["needs"]), {"linux", "platforms"})
        self.assertNotIn("publish", self.release["jobs"])

    def gate(self, which, *, sha=None, checked=None, profile="full", statuses=None, resolved=None):
        if not shutil.which("node"):
            self.fail("Node is required to execute the actual GitHub release-gate scripts")
        sha = "a" * 40 if sha is None else sha
        if statuses is None:
            statuses = [{"id": 1, "context": "Test all targets", "state": "success"},
                        {"id": 2, "context": "Native and acceptance scripts", "state": "success"}]
        job = self.release["jobs"][which]
        step = next(s for s in job["steps"] if s.get("uses") == "actions/github-script@v7")
        code = step["with"]["script"]
        with tempfile.TemporaryDirectory() as temp:
            config = {
                "code": code, "sha": sha, "checked": sha if checked is None else checked,
                "profile": profile, "statuses": statuses,
                "resolved": sha if resolved is None else resolved, "temp": temp,
            }
            program = """
const cfg = JSON.parse(process.argv[1]);
const outputs = {};
const requests = [];
const github = {
  rest: {repos: {
    getCommit: async options => { requests.push(options.ref); return {data: {sha: cfg.resolved}}; },
    listCommitStatusesForRef: () => {},
  }},
  paginate: async (method, options) => { requests.push(options.ref); return cfg.statuses; },
};
const summary = {addHeading() {return this;}, addRaw() {return this;}, async write() {}};
const core = {setOutput(name, value) {outputs[name] = value;}, summary};
const context = {repo: {owner: "robot-head", repo: "vesper"}, runId: 123};
const proc = {env: {CANDIDATE_SHA: cfg.sha, CHECKED_SHA: cfg.checked,
                   CHECKED_PROFILE: cfg.profile, RUNNER_TEMP: cfg.temp}};
const fn = new Function("github", "context", "core", "process", "require",
                        "return (async () => {" + cfg.code + "})()");
(async () => {
  try {
    await fn(github, context, core, proc, require);
    console.log(JSON.stringify({ok: true, outputs, requests,
      artifact: require("fs").existsSync(cfg.temp + "/release-validation.json")}));
  } catch (error) {
    console.log(JSON.stringify({ok: false, error: String(error), outputs, requests}));
  }
})();
"""
            result = subprocess.run(["node", "-e", program, json.dumps(config)],
                                    check=True, text=True, capture_output=True)
            return json.loads(result.stdout)

    def test_release_gate_accepts_exact_green_candidate(self):
        result = self.gate("linux")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["outputs"]["sha"], "a" * 40)
        self.assertEqual(set(result["requests"]), {"a" * 40})
        result = self.gate("ready")
        self.assertTrue(result["ok"], result)
        self.assertTrue(result["artifact"])

    def test_release_gate_rejects_moving_refs_and_wrong_commits(self):
        for sha in ("main", "v1.0.0", "", "a" * 39, "A" * 40, "a" * 40 + ";bad"):
            self.assertFalse(self.gate("linux", sha=sha)["ok"])
        self.assertFalse(self.gate("linux", resolved="b" * 40)["ok"])
        self.assertFalse(self.gate("ready", checked="b" * 40)["ok"])
        self.assertFalse(self.gate("ready", profile="pr")["ok"])

    def test_release_gate_uses_latest_linux_states_and_fails_closed(self):
        good = [{"id": 1, "context": "Test all targets", "state": "success"},
                {"id": 2, "context": "Native and acceptance scripts", "state": "success"}]
        for which in ("linux", "ready"):
            for state in ("failure", "error", "pending"):
                statuses = good + [{"id": 3, "context": "Test all targets", "state": state}]
                self.assertFalse(self.gate(which, statuses=statuses)["ok"])
            self.assertFalse(self.gate(which, statuses=good[:1])["ok"])
            self.assertFalse(self.gate(which, statuses=[])["ok"])
            # Input order cannot let an old failure replace a newer success.
            statuses = good + [{"id": 0, "context": "Test all targets", "state": "failure"}]
            self.assertTrue(self.gate(which, statuses=statuses)["ok"])


if __name__ == "__main__":
    unittest.main()
