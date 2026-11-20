"""Regression tests for fail-closed, path-aware native CI selection."""
import copy
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "platform_test_plan.py"
spec = importlib.util.spec_from_file_location("platform_test_plan", SCRIPT)
plan = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plan)


class PolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.policy = plan.load_policy()

    def test_full_profile_has_no_exclusions(self):
        self.assertEqual(plan.deferred_targets("full"), [])

    def test_all_groups_escalated_has_no_exclusions(self):
        self.assertEqual(plan.deferred_targets("pr", self.policy["groups"]), [])

    def test_escalation_restores_only_relevant_groups(self):
        deferred = plan.deferred_targets("pr", ["flux_faults"])
        self.assertNotIn("flux_faults", {r["group"] for r in deferred})
        self.assertIn("proof", {r["group"] for r in deferred})
        self.assertIn("frontend_corpus", {r["group"] for r in deferred})

    def test_partial_suites_preserve_other_cases(self):
        partial = [r for r in plan.deferred_targets("pr") if r["skip_tests"]]
        self.assertEqual(len(partial), 3)
        for record in partial:
            cases = plan.rust_test_cases((plan.REPO_ROOT / record["source"]).read_text())
            self.assertTrue(set(record["skip_tests"]) < cases)
        self.assertNotIn("store_crash", {r["target"] for r in plan.deferred_targets("pr")})
        self.assertNotIn("resource_oracle42", {r["target"] for r in plan.deferred_targets("pr")})

    def test_unknown_profile_or_group_is_error(self):
        for profile, groups in [("quick", []), ("pr", ["typo"]), ("full", ["typo"]), ("pr", "proof")]:
            with self.subTest(profile=profile, groups=groups), self.assertRaises(ValueError):
                plan.deferred_targets(profile, groups)

    def test_paths_select_only_affected_groups(self):
        self.assertEqual(plan.affected_groups(["README.md"]), [])
        self.assertEqual(plan.affected_groups(["docs/design.md", "website/index.html"]), [])
        self.assertEqual(plan.affected_groups(["crates/vesper-flux/src/cluster.rs"]), ["flux_faults"])
        self.assertEqual(plan.affected_groups(["crates/vesper-simpulse/src/lib.rs"]), ["flux_faults"])
        self.assertEqual(plan.affected_groups(["src/native.rs"]), ["frontend_corpus"])
        self.assertEqual(plan.affected_groups(["lean/NewProof.lean"]), ["proof"])
        self.assertEqual(plan.affected_groups(["examples/new.ves"]), ["frontend_corpus"])
        self.assertEqual(plan.affected_groups(["new.ves"]), ["frontend_corpus"])
        self.assertEqual(plan.affected_groups(["tests/format_proof_cli.rs"]), ["proof"])
        self.assertEqual(plan.affected_groups(["Cargo.lock"]), sorted(self.policy["groups"]))
        self.assertEqual(plan.affected_groups([".github/workflows/platforms.yml"]), sorted(self.policy["groups"]))

    def test_proof_producers_and_cli_changes_restore_complete_replay(self):
        for path in [
            "src/audit_commands.rs", "src/replay_commands.rs",
            "crates/vesper-lean-syntax/src/lib.rs", "crates/vesper-fmt/src/lib.rs",
            "src/kernel_snapshot.rs", "src/bin/logos42/process.rs",
            "src/bin/arithmetic_oracle42.rs", "src/bin/arithmetic_oracle42/process.rs",
            "src/lsp/proof_lens.rs", "src/optimize/witness_lean.rs",
            "src/format_commands.rs", "src/stylus_safety.rs",
        ]:
            with self.subTest(path=path):
                self.assertEqual(plan.affected_groups([path]), ["proof"])

    def test_evaluation_and_lowering_changes_restore_complete_corpus(self):
        for path in [
            "src/ffi_lowering.rs", "src/ffi_lowering/native.rs", "src/eval.rs",
            "src/world.rs", "src/resolve.rs", "src/mono.rs", "src/layout.rs",
            "src/types.rs",
            "src/runtime.rs", "src/runtime/resource_trace.rs",
        ]:
            with self.subTest(path=path):
                self.assertEqual(plan.affected_groups([path]), ["frontend_corpus"])

    def invalid_policy(self, change, expected):
        policy = copy.deepcopy(self.policy)
        change(policy)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "policy.json"
            path.write_text(json.dumps(policy))
            with self.assertRaisesRegex(ValueError, expected):
                plan.load_policy(path)

    def test_removed_or_renamed_case_fails_closed(self):
        self.invalid_policy(lambda p: p["deferred_targets"][-1]["skip_tests"].__setitem__(0, "renamed_case"), "stale test cases")

    def test_selector_duplicates_and_missing_source_fail_closed(self):
        self.invalid_policy(lambda p: p["deferred_targets"].append(p["deferred_targets"][0]), "duplicate")
        def rename(p):
            record = p["deferred_targets"][0]
            record.update(target="gone", source="tests/gone.rs", bazel="//:test/gone")
        self.invalid_policy(rename, "source missing")

    def test_helper_function_does_not_count_as_test(self):
        cases = plan.rust_test_cases("fn helper() {}\n#[test]\n#[cfg(unix)]\nfn real_case() {}")
        self.assertEqual(cases, {"real_case"})


class GitPlanTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "--quiet")
        (self.repo / "README.md").write_text("base\n")
        self.git("add", ".")
        self.git("-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", "commit", "--quiet", "-m", "base")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True)

    def commit(self, name):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("changed\n")
        self.git("add", "--", name)
        self.git("-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", "commit", "--quiet", "-m", "change")
        return self.git("rev-parse", "HEAD").strip()

    def test_diff_uses_zero_terminated_names(self):
        head = self.commit("src/native\nodd.rs")
        result = plan.make_plan("pr", self.base, head, repo=self.repo)
        self.assertEqual(result["groups"], ["frontend_corpus"])
        self.assertEqual(result["changed_paths"], ["src/native\nodd.rs"])
        self.assertEqual(result["sha"], head)

    def test_missing_unavailable_or_invalid_base_enables_full_coverage(self):
        for base in [None, "f" * 40, "bad-ref"]:
            with self.subTest(base=base):
                result = plan.make_plan("pr", base, self.base, repo=self.repo)
                self.assertEqual(result["groups"], sorted(plan.load_policy()["groups"]))
                self.assertIn("full coverage", result["reason"])

    def test_unavailable_or_invalid_head_is_error(self):
        for head in ["f" * 40, "HEAD", "--help"]:
            with self.subTest(head=head), self.assertRaises(ValueError):
                plan.make_plan("full", None, head, repo=self.repo)

    def test_available_but_different_head_cannot_label_checked_out_source(self):
        self.commit("README.md")
        with self.assertRaisesRegex(ValueError, "checked-out commit"):
            plan.make_plan("full", None, self.base, repo=self.repo)

    def test_no_changes_does_not_defer_any_unclassified_test(self):
        result = plan.make_plan("pr", self.base, self.base, repo=self.repo)
        self.assertEqual(result["groups"], [])
        self.assertEqual(result["changed_paths"], [])
        self.assertEqual({r["bazel"] for r in plan.deferred_targets("pr")},
                         {r["bazel"] for r in plan.load_policy()["deferred_targets"]})

    def test_failed_diff_selects_full_coverage(self):
        real_run = subprocess.run
        def fail_diff(argv, **kwargs):
            if argv[1] == "diff":
                return subprocess.CompletedProcess(argv, 128, b"", b"no merge base")
            return real_run(argv, **kwargs)
        with mock.patch.object(plan.subprocess, "run", side_effect=fail_diff):
            result = plan.make_plan("pr", self.base, self.base, repo=self.repo)
        self.assertEqual(result["groups"], sorted(plan.load_policy()["groups"]))
        self.assertIn("diff unavailable", result["reason"])

    def test_cli_writes_bound_sha_and_compact_group_output(self):
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=plan.REPO_ROOT, text=True).strip()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "plan.json"
            github = Path(directory) / "github-output"
            with contextlib.redirect_stdout(io.StringIO()):
                result = plan.main(["plan", "--profile", "full", "--head", head,
                                    "--output", str(output), "--github-output", str(github)])
            self.assertEqual(result, 0)
            body = json.loads(output.read_text())
            self.assertEqual(body["sha"], head)
            values = dict(line.split("=", 1) for line in github.read_text().splitlines())
            self.assertEqual(values["profile"], "full")
            self.assertEqual(values["sha"], head)
            self.assertEqual(json.loads(values["groups"]), sorted(plan.load_policy()["groups"]))


if __name__ == "__main__":
    unittest.main()
