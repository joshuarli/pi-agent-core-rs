"""No-provider smoke evidence for the pinned upstream coding-profile adapter."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]


class CodingAdapterSmokeTest(unittest.TestCase):
    def test_pinned_profile_imports_without_catalog_hydration_or_host_pi(self) -> None:
        environment = {"PATH": os.environ.get("PATH", ""), "LANG": "C", "LC_ALL": "C"}
        command = [
            "bash",
            str(ROOT / "evals" / "run-upstream-live.sh"),
            "--model",
            "fixture",
            "--task-json",
            "/tmp/quality-no-task.json",
            "--workspace",
            "/tmp",
            "--capabilities-json",
            "/tmp/quality-no-capabilities.json",
            "--result-json",
            "/tmp/quality-no-result.json",
            "--attempt-id",
            "quality-smoke",
            "--baseline-id",
            "upstream",
        ]
        completed = subprocess.run(command, cwd=ROOT, env=environment, text=True, capture_output=True, check=False)
        self.assertEqual(completed.returncode, 2, completed.stderr)
        self.assertIn("OPENROUTER_API_KEY must be supplied", completed.stderr)
        self.assertNotIn("amazon-bedrock.json", completed.stderr)


if __name__ == "__main__":
    unittest.main()
