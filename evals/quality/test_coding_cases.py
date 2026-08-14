"""Provider-free contract checks for the small ecological coding suite."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import call, patch

from . import coding_cases
from .coding_cases import CodingCaseError, cache_bare_repository, load_cases
from .coding_runner import _adapter_task, _profile_capabilities


class CodingCasesTest(unittest.TestCase):
    def test_exact_three_requested_cases_and_pins(self) -> None:
        cases = load_cases()
        self.assertEqual(
            [case["id"] for case in cases],
            ["express-3936-medium", "express-4205-hard", "express-4744-easy"],
        )
        for case in cases:
            self.assertEqual(case["setup"]["network"], False)
            self.assertEqual(case["setup"]["tools"], ["read", "bash", "edit", "write"])
            self.assertEqual(case["validators"]["full"]["audit_command"], "npm install && npm test")
            self.assertEqual(case["validators"]["fast"]["evidence"]["baseline"], "fails")
            self.assertEqual(case["validators"]["fast"]["evidence"]["known_correct"], "passes")

    def test_scoring_requires_a_prepopulated_bare_cache(self) -> None:
        case = load_cases()[0]
        with tempfile.TemporaryDirectory(prefix="pi-agent-quality-cache-") as temporary:
            with self.assertRaises(CodingCaseError):
                cache_bare_repository(case["baseline"]["repository"], case["baseline"]["commit"], Path(temporary))

    def test_cache_publishes_a_private_ref_for_each_verified_commit(self) -> None:
        case = load_cases()[0]
        repository = case["baseline"]["repository"]
        commit = case["baseline"]["commit"]
        with tempfile.TemporaryDirectory(prefix="pi-agent-quality-cache-") as temporary:
            root = Path(temporary)
            key = coding_cases.hashlib.sha256(repository.encode()).hexdigest()[:32]
            bare = root / "bare" / f"{key}.git"
            bare.mkdir(parents=True)
            with patch.object(coding_cases, "_git") as git:
                cache_bare_repository(repository, commit, root, populate=True)
            self.assertIn(
                call(
                    "--git-dir",
                    str(bare.resolve()),
                    "update-ref",
                    f"refs/heads/pi-agent-quality/{commit}",
                    commit,
                ),
                git.call_args_list,
            )

    def test_adapter_task_uses_the_pinned_default_profile_tool_contract(self) -> None:
        capabilities = _profile_capabilities()
        task = _adapter_task(load_cases()[0], capabilities)
        self.assertEqual(task["capabilities"], capabilities)
        self.assertEqual([tool["name"] for tool in capabilities], ["read", "bash", "edit", "write"])


if __name__ == "__main__":
    unittest.main()
