"""Regression tests for required-check naming, dedup ownership and release gates."""
import json
import os
from pathlib import Path
import re
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

    @unittest.skipUnless(shutil.which("aspect") and shutil.which("cargo"), "Aspect or Cargo unavailable")
    def test_native_port_selection_rules_and_tracked_harness_plan(self):
        result = subprocess.run(
            ["aspect", "ci", "tests", "port-selection"],
            cwd=ROOT, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("aspect") and shutil.which("cargo"), "Aspect or Cargo unavailable")
    def test_divergent_features_link_the_port_in_the_current_workspace(self):
        result = subprocess.run(["aspect", "ci", "tests", "port-divergent-features"],
                                cwd=ROOT, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_platform_workflow_selects_ports_natively_with_historical_fallback(self):
        plan = self.platforms["jobs"]["plan"]
        steps = {step.get("id"): step for step in plan["steps"]}
        self.assertEqual(steps["ports"]["run"], "aspect ci choose-platform-ports")
        self.assertIn("!= ''", steps["ports"]["if"])
        self.assertIn("== ''", steps["legacy_ports"]["if"])
        self.assertEqual(plan["outputs"]["ports"], "${{ steps.ports.outputs.crates || steps.legacy_ports.outputs.crates }}")

    @unittest.skipUnless(shutil.which("aspect"), "Aspect unavailable")
    def test_native_platform_catalog_and_path_patterns(self):
        result = subprocess.run(["aspect", "ci", "tests", "test-plan"], cwd=ROOT, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_platform_workflow_checks_identity_and_plans_before_test_lanes(self):
        plan = self.platforms["jobs"]["plan"]
        steps = plan["steps"]
        native = next(i for i, step in enumerate(steps) if step.get("run") == "aspect ci verify-platform-dispatch")
        tests = next(i for i, step in enumerate(steps) if step.get("run") == "aspect ci platform-tests")
        report = next(step for step in steps if step.get("id") == "plan")
        self.assertLess(native, tests)
        self.assertEqual(report["run"], "aspect ci plan-platform-workflow")
        self.assertIn("!= ''", report["if"])
        self.assertIn("== ''", next(step["if"] for step in steps if step.get("id") == "legacy_plan"))
        for name in ("sha", "profile", "groups"):
            self.assertEqual(plan["outputs"][name],
                             "${{ steps.plan.outputs." + name + " || steps.legacy_plan.outputs." + name + " }}")

    @unittest.skipUnless(shutil.which("aspect"), "Aspect CLI unavailable")
    def test_all_repository_scripts_have_discoverable_aspect_tasks(self):
        scripts = subprocess.check_output(
            ["git", "ls-files", "*.sh", "*.py"], cwd=ROOT, text=True,
        ).splitlines()
        catalog = json.loads(subprocess.check_output(["aspect", "describe"], cwd=ROOT))
        paths = [
            path
            for command in catalog["commands"]
            if command["summary"].startswith("Run ")
            for path in re.findall(r"(?:^|\s)((?:[\w.-]+/)*[\w.-]+\.(?:sh|py))(?=\s|$)",
                                   command["summary"])
        ]
        self.assertCountEqual(paths, scripts)

    @unittest.skipUnless(shutil.which("aspect"), "Aspect CLI unavailable")
    def test_native_lean_replay_scope_rules_and_bazel_targets(self):
        result = subprocess.run(
            ["aspect", "ci", "tests", "lean-replay-scope"],
            cwd=ROOT, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_required_check_names_and_events_are_preserved(self):
        self.assertTrue({"push", "merge_group", "schedule",
                         "workflow_dispatch", "workflow_call"} <= self.platforms["on"].keys())
        # Pull requests get Platforms from native-checks.yml once they can
        # merge and BuildBuddy is green (#1479), not on every push.
        self.assertNotIn("pull_request", self.platforms["on"])
        self.assertIn("pull_request", self.platforms["on"]["workflow_dispatch"]["inputs"])
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
        self.assertEqual(mac["timeout-minutes"], "120")
        self.assertEqual(windows["timeout-minutes"], "120")
        for job, command in [(mac, "aspect ci verify-platform-checkout"),
                             (windows, "./scripts/windows/verify-platform-checkout.ps1")]:
            check = next(s for s in job["steps"] if s.get("name") == "Verify exact checkout")
            self.assertEqual(check["run"], command)
            self.assertIn("!= ''", check["if"])
            self.assertTrue(all("\n" not in step.get("run", "") for step in job["steps"]))
        for job in (mac, windows):
            commands = "\n".join(s.get("run", "") for s in job["steps"])
            self.assertNotIn("--workspace", commands)
            self.assertNotIn("clippy", commands)
            self.assertNotIn("check-lean-models", commands)
        mac_commands = "\n".join(s.get("run", "") for s in mac["steps"])
        windows_commands = "\n".join(s.get("run", "") for s in windows["steps"])
        self.assertIn("aspect ci test-native-executables", mac_commands)
        native_step = next(s for s in windows["steps"] if s.get("name") == "Validate native COFF executables")
        self.assertEqual(native_step["shell"], "pwsh")
        self.assertIn("./scripts/windows/test-native-executables.ps1", native_step["run"])
        hybrid = workflow("hybrid.yml")["jobs"]["hybrid"]
        hybrid_step = next(s for s in hybrid["steps"]
                           if s.get("name") == "The CLI suites on the hybrid (Windows)")
        self.assertEqual(hybrid_step["shell"], "pwsh")
        self.assertEqual(hybrid_step["run"], "./scripts/windows/test-hybrid-cli.ps1")
        self.assertEqual({row["os"]: row["compiler"] for row in hybrid["strategy"]["matrix"]["include"]},
                         {"ubuntu-latest": "vesper", "macos-latest": "vesper", "windows-latest": "vesper.exe"})
        for variable in ("VESPER_PORT_COMPILER", "VESPER_DEFAULT_COMPILER"):
            self.assertEqual(hybrid["env"][variable], "${{ github.workspace }}/target/debug/${{ matrix.compiler }}")
        self.assertIn("--test platform", mac_commands)
        self.assertIn("--test windows_platform", windows_commands)
        self.assertIn("--test windows", windows_commands)
        for commands in (mac_commands, windows_commands):
            self.assertIn("--test drivers", commands)
            self.assertIn("--test pool", commands)

    def test_buildbuddy_reclaims_disk_and_runs_bazel_via_aspect(self):
        config = yaml.load((ROOT / "buildbuddy.yaml").read_text(), Loader=yaml.BaseLoader)
        launcher = "./.git/vesper-aspect/aspect"
        checksum = (ROOT / ".aspect/launcher-linux-x86_64.sha256").read_text().split()
        self.assertEqual(checksum[1:], [launcher])
        self.assertRegex(checksum[0], r"^[a-f0-9]{64}$")
        lanes = {
            "Test all targets": "--test_tag_filters=-lean_replay,-self_host",
            "Self-host differentials": "--test_tag_filters=self_host,-self_host_codegen,-self_host_part2,-lean_replay",
            "Self-host differentials 2": "--test_tag_filters=self_host_part2,-lean_replay",
            "Self-host codegen differentials": "--test_tag_filters=self_host_codegen,-lean_replay",
            "Native and acceptance scripts": "",
        }
        self.assertCountEqual([action["name"] for action in config["actions"]], lanes.keys())
        for action in config["actions"]:
            with self.subTest(action=action["name"]):
                steps = [step["run"] for step in action["steps"]]
                self.assertTrue(all("\n" not in step for step in steps))
                self.assertEqual(steps[0], "mkdir -p ./.git/vesper-aspect")
                self.assertTrue(steps[1].startswith("curl --fail "))
                self.assertTrue(steps[1].endswith(f" -o {launcher}"))
                self.assertEqual(steps[2:5], [
                    "sha256sum --check .aspect/launcher-linux-x86_64.sha256",
                    f"chmod +x {launcher}", f"{launcher} ci reclaim-disk",
                ])
                lane = lanes[action["name"]]
                if lane:
                    [tests] = [step for step in steps if step.startswith(f"{launcher} ci bazel-tests -- ")]
                    self.assertIn("--config=ci --config=rbe_build --config=ci_tests", tests)
                    self.assertIn("--remote_download_outputs=minimal", tests)
                    self.assertIn(lane, tests.split())
                    self.assertTrue(tests.endswith(" //..."))
                    self.assertLess(steps.index(f"{launcher} ci install-lean"), steps.index(tests))
                else:
                    self.assertIn(f"{launcher} acceptance test-format-proof", steps)

    def test_bazel_setup_selects_native_configuration_by_host(self):
        action = yaml.load((ROOT / ".github/actions/bazel/action.yml").read_text(), Loader=yaml.BaseLoader)
        setup, aspect, unix, windows = action["runs"]["steps"]
        self.assertIn("inputs.buildbuddy-api-key == ''", setup["with"]["disk-cache"])
        self.assertEqual(aspect["uses"], "./.github/actions/aspect")
        self.assertEqual(aspect["if"], "runner.os != 'Windows'")
        self.assertEqual(unix["if"], aspect["if"])
        self.assertEqual(unix["run"], "aspect ci configure-bazel")
        self.assertEqual(windows["if"], "runner.os == 'Windows'")
        self.assertEqual(windows["shell"], "pwsh")
        self.assertEqual(windows["run"], "./scripts/windows/configure-bazel.ps1")
        self.assertEqual(unix["env"], windows["env"])

    def test_bootstrap_workflows_invoke_native_tasks(self):
        stage1 = workflow("bootstrap-stage1.yml")["jobs"]
        stage2 = workflow("bootstrap-stage2.yml")["jobs"]["bootstrap"]
        for job in [*stage1.values(), stage2]:
            self.assertTrue(all("\n" not in step.get("run", "") for step in job["steps"]))
        self.assertIn("aspect ci stage1-cross-backend", [step.get("run") for step in stage1["stage1"]["steps"]])
        self.assertIn("aspect ci install-lean", [step.get("run") for step in stage1["stage0-two-seed"]["steps"]])
        self.assertIn("aspect ci install-wasmtime", [step.get("run") for step in stage2["steps"]])
        verified = next(step for step in stage2["steps"] if step.get("run") == "aspect bootstrap stage2 -- which")
        self.assertEqual(verified["env"]["WASMTIME_BIN"], "${{ runner.temp }}/wasmtime/wasmtime")

    def test_port_harness_runs_once_on_linux(self):
        # macOS and Windows run only the export ABI tier; the ported crates
        # are platform-neutral and run once in the Linux job (#1010).
        jobs = self.platforms["jobs"]
        for job, command in (("platform", "aspect ci test-port-abi"),
                             ("windows", "./scripts/windows/test-port-abi.ps1")):
            step = next(s for s in jobs[job]["steps"] if command in s.get("run", ""))
            self.assertEqual(step["env"]["VESPER_PORT_HARNESS_SCOPE"], "abi")
            if job == "windows":
                self.assertEqual(step["shell"], "pwsh")
        ports = jobs["ports"]
        self.assertEqual(ports["runs-on"], "ubuntu-latest")
        self.assertEqual(ports["needs"], ["plan", "ports-compiler"])
        self.assertEqual(ports["steps"][0]["run"], 'test "$PLAN_RESULT" = success')
        step = next(s for s in ports["steps"] if "aspect acceptance test-port-harness" in s.get("run", ""))
        self.assertNotIn("VESPER_PORT_HARNESS_SCOPE", step.get("env", {}))
        # One optimized compiler, built once and shared by every shard (#1103).
        compiler = jobs["ports-compiler"]
        self.assertEqual(compiler["needs"], "plan")
        build = next(s for s in compiler["steps"] if "cargo build" in s.get("run", ""))
        self.assertIn("--profile port-compiler --bin vesper", build["run"])
        self.assertEqual(step["env"]["VESPER_PORT_COMPILER"], "${{ runner.temp }}/port-compiler/vesper")
        self.assertEqual(step["env"]["VESPER_PORT_HARNESS_CRATES"], "${{ needs.plan.outputs.ports }}")
        self.assertNotIn("cargo build", "\n".join(s.get("run", "") for s in ports["steps"]))
        # Nine shards (#1019, #1100, #1924), each running its own part of the harness.
        shards = [str(n) for n in range(1, 10)]
        self.assertEqual(ports["strategy"]["matrix"]["shard"], "${{ fromJSON(needs.plan.outputs.shards || '[1]') }}")
        plan = self.platforms["jobs"]["plan"]
        self.assertEqual(plan["outputs"]["shards"], "${{ steps.shards.outputs.shards || steps.historical_shards.outputs.shards }}")
        steps = {step.get("id"): step for step in plan["steps"]}
        self.assertEqual(steps["shards"]["run"], "aspect ci port-shard-matrix")
        self.assertIn("!= ''", steps["shards"]["if"])
        self.assertIn("== ''", steps["historical_shards"]["if"])
        self.assertEqual(steps["historical_shards"]["env"]["PORT_HARNESS_ROOT"], "${{ github.workspace }}")
        self.assertEqual(ports["strategy"]["fail-fast"], "false")
        self.assertEqual(step["env"]["VESPER_PORT_HARNESS_SHARD"], "${{ matrix.shard }}")
        self.assertIn("ports", jobs["complete"]["needs"])
        plan = json.loads((ROOT / "scripts/port-abi-plan.json").read_text())
        abi_tier = "\n".join(" ".join(case["args"]) for group in plan for case in group["checks"])
        for check in ("-p vesper-port-abi", "-p vesper-port-harness", "--test kit",
                      "--test kit_memory", "-p vesper-port-pilot --features vesper-port",
                      "-p vesper-port-build --features divergent"):
            self.assertIn(check, abi_tier)
        self.assertNotIn("vesper-beacon", abi_tier)
        # Every configured shard has work, and later ports land in the last one.
        shards_plan = json.loads((ROOT / "scripts/port-harness-plan.json").read_text())
        self.assertEqual(len(shards_plan), len(shards))
        self.assertTrue(all(shards_plan))
        self.assertEqual(shards_plan[-1][-1]["task"], ["crate", "vault", "check"])
        self.assertEqual(shards_plan[6][-1]["task"], ["crate", "diagnostics", "test-port"])
        self.assertEqual(shards_plan[7][-1]["task"], ["crate", "lint", "test-port"])
        self.assertEqual(shards_plan[8][0]["task"], ["crate", "sim-session", "test-port"])

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

    def test_completion_accepts_skipped_shards_only_when_no_port_is_affected(self):
        # The shards skip as jobs when the plan names no port (#1321).
        self.assertIn("needs.plan.outputs.ports != 'none'", self.platforms["jobs"]["ports"]["if"])
        run = self.platforms["jobs"]["complete"]["steps"][0]["run"]
        for ports, expected in (("none", 0), ("vesper-aura", 1), ("", 1)):
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "outputs"
                env = {**os.environ, "PLAN_RESULT": "success", "MAC_RESULT": "success",
                       "WINDOWS_RESULT": "success", "PORTS_RESULT": "skipped", "PORTS": ports,
                       "CHECKED_SHA": "a" * 40, "PROFILE": "pr", "GITHUB_OUTPUT": str(output)}
                completed = subprocess.run(["bash", "-e", "-c", run], env=env)
                self.assertEqual(completed.returncode != 0, bool(expected))
                self.assertEqual(output.exists(), not expected)

    def test_hybrid_and_ports_skip_pull_requests(self):
        # Neither is required; both run on main, nightly and manual runs (#1479).
        hybrid = workflow("hybrid.yml")
        self.assertNotIn("pull_request", hybrid["on"])
        self.assertTrue({"push", "schedule", "workflow_dispatch"} <= hybrid["on"].keys())
        include = hybrid["jobs"]["hybrid"]["strategy"]["matrix"]["include"]
        self.assertEqual([leg["os"] for leg in include],
                         ["ubuntu-latest", "macos-latest", "windows-latest"])
        step = next(s for s in self.platforms["jobs"]["plan"]["steps"] if s.get("id") == "ports")
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "outputs"
            env = {**os.environ, "PULL_REQUEST": "1311", "GITHUB_OUTPUT": str(output),
                   "REQUESTED_PROFILE": "pr", "EVENT": "workflow_dispatch", "BASE_SHA": ""}
            subprocess.run(["bash", "-e", "-c", step["run"]], env=env, check=True, cwd=ROOT,
                           capture_output=True)
            self.assertEqual(output.read_text(), "crates=none\n")

    def test_main_runs_of_pr_skipped_jobs_are_never_cancelled(self):
        # Ports and hybrid run only on main, so a later merge must not cancel
        # the run that would catch a regression (#1479).
        cancel = self.platforms["concurrency"]["cancel-in-progress"]
        self.assertNotIn("'push'", cancel)
        self.assertIn("inputs.pull_request != ''", cancel)
        self.assertIn("'main'", self.platforms["concurrency"]["group"])
        hybrid = workflow("hybrid.yml")["concurrency"]
        self.assertEqual(hybrid["cancel-in-progress"], "false")
        self.assertIn("'main'", hybrid["group"])

    def test_merge_gate_is_the_first_step_of_every_pull_request_job(self):
        # The gate runs inside the jobs it guards, not as a job of its own
        # that queued for a runner ahead of them (#1479).
        self.assertFalse((ROOT / ".github/workflows/merge-gate.yml").exists())
        action = yaml.safe_load((ROOT / ".github/actions/merge-gate/action.yml").read_text())
        self.assertIn("scripts/ci/merge_gate.py", action["runs"]["steps"][0]["run"])
        guarded = []
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            flow = workflow(path.name)
            if "pull_request" not in flow["on"]:
                continue
            self.assertEqual(flow["permissions"].get("actions"), "write", path.name)
            self.assertEqual(flow["permissions"].get("pull-requests"), "read", path.name)
            for name, job in flow["jobs"].items():
                if "github.event_name != 'pull_request'" in job.get("if", ""):
                    continue
                checkout, gate = job["steps"][:2]
                self.assertEqual(checkout["uses"], "actions/checkout@v4", (path.name, name))
                self.assertEqual(checkout["with"]["path"], ".gate")
                self.assertEqual(checkout["with"]["ref"], "${{ github.event.pull_request.head.sha }}")
                self.assertEqual(checkout["with"]["fetch-depth"], "0")
                self.assertEqual(gate["uses"], "./.gate/.github/actions/merge-gate", (path.name, name))
                self.assertEqual(gate["with"]["head-sha"], "${{ github.event.pull_request.head.sha }}")
                for step in (checkout, gate):
                    self.assertEqual(step["if"], "github.event_name == 'pull_request'")
                guarded.append(f"{path.name}:{name}")
        self.assertIn("website.yml:build", guarded)
        self.assertGreaterEqual(len(guarded), 10)

    def test_self_host_differentials_run_as_their_own_buildbuddy_check(self):
        # #1519: on one runner the corpus differentials pushed "Test all
        # targets" past BuildBuddy's one-hour limit.
        config = yaml.load((ROOT / "buildbuddy.yaml").read_text(), Loader=yaml.BaseLoader)
        actions = {a["name"]: a for a in config["actions"]}
        command = lambda name: actions[name]["steps"][-1]["run"]
        everything = command("Test all targets")
        self.assertIn("--test_tag_filters=-lean_replay,-self_host", everything)
        self.assertIn("--build_tag_filters=-lean_replay,-self_host", everything)
        self_host = command("Self-host differentials")
        self.assertIn("--build_tests_only", self_host)
        self.assertIn("--test_tag_filters=self_host,-self_host_codegen,-self_host_part2,-lean_replay", self_host)
        # #1882: the non-codegen differentials neared the hour on one runner.
        part2 = command("Self-host differentials 2")
        self.assertIn("--build_tests_only", part2)
        self.assertIn("--test_tag_filters=self_host_part2,-lean_replay", part2)
        codegen = command("Self-host codegen differentials")
        self.assertIn("--build_tests_only", codegen)
        self.assertIn("--test_tag_filters=self_host_codegen,-lean_replay", codegen)
        for name in ("Self-host differentials", "Self-host differentials 2", "Self-host codegen differentials"):
            self.assertEqual(actions[name]["timeout"], "60m")
            # Per-target times, to balance the split on.
            self.assertIn("--test_summary=short", command(name))
        defs = (ROOT / "tools/bazel/defs.bzl").read_text()
        self.assertIn('startswith("self_host_")', defs)
        self.assertIn('"self_host_codegen"', defs)
        self.assertIn('"self_host_part2"', defs)
        self.assertIn("_SELF_HOST_PART2 = (", defs)
        # Three "Self-host differentials" tests at a time fit the runner (#1519).
        self.assertIn('_SELF_HOST_MEMORY = "resources:memory:6144"', defs)
        self.assertIn("--local_resources=memory=18432", (ROOT / ".bazelrc").read_text())

    def test_native_checks_start_only_after_the_gate_and_buildbuddy(self):
        native = workflow("native-checks.yml")
        self.assertEqual(set(native["on"]), {"status"})
        job = native["jobs"]["start"]
        self.assertIn("github.event.state == 'success'", job["if"])
        self.assertEqual(job["runs-on"], "ubuntu-latest")
        steps = job["steps"]
        find = steps[0]["run"]
        for context in ("Test all targets", "Self-host differentials", "Self-host differentials 2",
                        "Self-host codegen differentials", "Native and acceptance scripts"):
            self.assertIn(context, job["if"])
            self.assertIn(f'"{context}"', find)
        self.assertIn('"$states" != 3', find)
        self.assertIn('"$codegen" != success', find)
        self.assertIn('"$part2" != success', find)
        names = [s.get("uses") or s.get("name") for s in steps]
        gate = names.index("./.gate/.github/actions/merge-gate")
        dispatch = next(i for i, s in enumerate(steps) if "gh workflow run" in s.get("run", ""))
        self.assertLess(gate, dispatch)
        command = steps[dispatch]["run"]
        for flag in ("platforms.yml", "-f profile=pr", '-f checkout_ref="refs/pull/$NUMBER/merge"',
                     '-f pull_request="$NUMBER"', '--ref "$BRANCH"'):
            self.assertIn(flag, command)
        verify = next(s for s in self.platforms["jobs"]["plan"]["steps"]
                      if s.get("name") == "Verify the dispatched pull request commit")
        self.assertIn("inputs.pull_request != ''", verify["if"])

    def test_dedup_retains_distinct_features_and_moves_stdio_checks(self):
        source = workflow("source-macros.yml")
        matrix = source["jobs"]["native-tier1-source"]["strategy"]["matrix"]["os"]
        self.assertEqual(set(matrix), {"ubuntu-latest"})
        source_steps = source["jobs"]["native-tier1-source"]["steps"]
        commands = "\n".join(s.get("run", "") for s in source_steps)
        self.assertIn("--no-default-features", commands)
        self.assertIn("--test forge_authority", commands)
        self.assertIn("structured-component-foundation", source["jobs"])
        stylus = workflow("stylus.yml")["jobs"]["native-stylus"]
        self.assertEqual(stylus["strategy"]["matrix"]["os"], ["ubuntu-latest"])
        for steps in (source_steps, stylus["steps"]):
            runner = next(i for i, step in enumerate(steps) if step.get("uses") == "./.github/actions/aspect")
            graph = next(i for i, step in enumerate(steps) if step.get("run") == "aspect ci check-default-cli-dependencies")
            self.assertLess(runner, graph)
            self.assertTrue(all("python -c" not in step.get("run", "") for step in steps))

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
                        {"id": 2, "context": "Native and acceptance scripts", "state": "success"},
                        {"id": 4, "context": "Self-host differentials", "state": "success"},
                        {"id": 6, "context": "Self-host codegen differentials", "state": "success"},
                        {"id": 8, "context": "Self-host differentials 2", "state": "success"}]
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
                {"id": 2, "context": "Native and acceptance scripts", "state": "success"},
                {"id": 4, "context": "Self-host differentials", "state": "success"},
                {"id": 6, "context": "Self-host codegen differentials", "state": "success"},
                {"id": 8, "context": "Self-host differentials 2", "state": "success"}]
        for which in ("linux", "ready"):
            for state in ("failure", "error", "pending"):
                for context in ("Test all targets", "Self-host differentials", "Self-host codegen differentials",
                                "Self-host differentials 2"):
                    statuses = good + [{"id": 9, "context": context, "state": state}]
                    self.assertFalse(self.gate(which, statuses=statuses)["ok"])
            # A candidate without every self-host check (#1519, #1882) is not ready.
            self.assertFalse(self.gate(which, statuses=good[:4])["ok"])
            self.assertFalse(self.gate(which, statuses=good[:3])["ok"])
            self.assertFalse(self.gate(which, statuses=good[:2])["ok"])
            self.assertFalse(self.gate(which, statuses=good[:1])["ok"])
            self.assertFalse(self.gate(which, statuses=[])["ok"])
            # Input order cannot let an old failure replace a newer success.
            statuses = good + [{"id": 0, "context": "Test all targets", "state": "failure"}]
            self.assertTrue(self.gate(which, statuses=statuses)["ok"])


if __name__ == "__main__":
    unittest.main()
