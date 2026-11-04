"""Required CI aggregation for upstream and lightweight fork workflows."""

import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

CHECKER = Path(__file__).with_name("check_ci_results.py")


class CheckCiResultsTests(unittest.TestCase):
    def check(
        self, results: dict[str, str], required: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["NEEDS"] = json.dumps(
            {name: {"result": status} for name, status in results.items()}
        )
        env.pop("REQUIRED_CI_DEPENDENCIES", None)
        if required is not None:
            env["REQUIRED_CI_DEPENDENCIES"] = required
        return subprocess.run(
            [sys.executable, str(CHECKER)],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_fork_succeeds_with_upstream_jobs_skipped(self) -> None:
        result = self.check(
            {"fork-smoke": "success", "rust-ci": "skipped", "sdk": "skipped"},
            required="fork-smoke",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_failed_fork_smoke_blocks_merge(self) -> None:
        result = self.check(
            {"fork-smoke": "failure", "rust-ci": "skipped"},
            required="fork-smoke",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fork-smoke: failure", result.stdout)

    def test_missing_fork_smoke_blocks_merge(self) -> None:
        result = self.check({"rust-ci": "skipped"}, required="fork-smoke")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fork-smoke: missing", result.stdout)

    def test_upstream_requires_original_job_family(self) -> None:
        upstream = (
            "blob-size-policy,cargo-deny,codespell,repo-checks,rust-ci,sdk"
        )
        results = {name: "success" for name in upstream.split(",")}
        results["fork-smoke"] = "skipped"
        self.assertEqual(self.check(results, required=upstream).returncode, 0)

        results["rust-ci"] = "skipped"
        failure = self.check(results, required=upstream)
        self.assertNotEqual(failure.returncode, 0)
        self.assertIn("rust-ci: skipped", failure.stdout)

    def test_unconfigured_postmerge_policy_still_requires_all_jobs(self) -> None:
        result = self.check({"fork-smoke": "skipped", "codespell": "success"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fork-smoke: skipped", result.stdout)

    def test_invalid_job_selection_fails_closed(self) -> None:
        for required in ("", "fork-smoke,", "fork-smoke,fork-smoke"):
            with self.subTest(required=required):
                result = self.check(
                    {"fork-smoke": "success"}, required=required
                )
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
